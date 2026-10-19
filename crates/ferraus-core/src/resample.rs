//! Windowed-sinc sample-rate conversion (Kaiser window), used for output encoding
//! overrides and by model pipelines that need a fixed rate. `ferraus-dsp`'s `resample`
//! command uses this same engine with a quality setting.

use crate::buffer::AudioBuffer;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quality {
    Fast,
    Medium,
    High,
}

impl Quality {
    pub fn parse(s: &str) -> Quality {
        match s {
            "fast" => Quality::Fast,
            "medium" => Quality::Medium,
            _ => Quality::High,
        }
    }
    fn taps(self) -> usize {
        match self {
            Quality::Fast => 12,
            Quality::Medium => 32,
            Quality::High => 64,
        }
    }
    fn beta(self) -> f64 {
        match self {
            Quality::Fast => 6.0,
            Quality::Medium => 8.0,
            Quality::High => 9.0,
        }
    }
}

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let y = x * x / 4.0;
    for k in 1..50 {
        term *= y / (k as f64 * k as f64);
        sum += term;
        if term < sum * 1e-16 {
            break;
        }
    }
    sum
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

/// Resample one channel from `from` Hz to `to` Hz.
pub fn resample_channel(input: &[f32], from: u32, to: u32, q: Quality) -> Vec<f32> {
    if from == to || input.is_empty() {
        return input.to_vec();
    }
    let g = gcd(from as u64, to as u64);
    let up = (to as u64 / g) as usize; // L
    let down = (from as u64 / g) as usize; // M
    let half = q.taps(); // zero crossings on each side, at the lower rate
    let cutoff = (to as f64 / from as f64).min(1.0) * 0.97;
    let beta = q.beta();
    let i0b = bessel_i0(beta);
    let out_len = (input.len() as u64 * to as u64).div_ceil(from as u64) as usize;
    // Polyphase filter bank: for each phase p in 0..up, taps at input offsets.
    let width = (half as f64 / cutoff).ceil() as isize;
    let ntaps = (2 * width + 1) as usize;
    let mut bank = vec![vec![0f32; ntaps]; up];
    for (p, row) in bank.iter_mut().enumerate() {
        let frac = p as f64 / up as f64;
        let mut sum = 0.0;
        for (k, t) in row.iter_mut().enumerate() {
            let x = (k as isize - width) as f64 - frac;
            let sinc = if x.abs() < 1e-12 {
                1.0
            } else {
                (std::f64::consts::PI * x * cutoff).sin() / (std::f64::consts::PI * x * cutoff)
            };
            let r = x / (width as f64 + 1.0);
            let w = if r.abs() >= 1.0 { 0.0 } else { bessel_i0(beta * (1.0 - r * r).sqrt()) / i0b };
            let v = sinc * w * cutoff;
            *t = v as f32;
            sum += v;
        }
        // normalize DC gain to exactly 1
        if sum.abs() > 1e-12 {
            for t in row.iter_mut() {
                *t = (*t as f64 / sum) as f32;
            }
        }
    }
    let n = input.len() as isize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let pos = i * down; // position on the upsampled grid
        let base = (pos / up) as isize;
        let phase = pos % up;
        let row = &bank[phase];
        let mut acc = 0f64;
        for (k, t) in row.iter().enumerate() {
            let idx = base + k as isize - width;
            if idx >= 0 && idx < n {
                acc += input[idx as usize] as f64 * *t as f64;
            }
        }
        out.push(acc as f32);
    }
    out
}

pub fn resample(buf: &AudioBuffer, to: u32, q: Quality) -> AudioBuffer {
    if buf.sample_rate == to {
        return buf.clone();
    }
    use rayon::prelude::*;
    let data: Vec<Vec<f32>> = buf.data.par_iter().map(|c| resample_channel(c, buf.sample_rate, to, q)).collect();
    AudioBuffer { sample_rate: to, data }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(sr: u32, f: f64, secs: f64) -> Vec<f32> {
        (0..(sr as f64 * secs) as usize)
            .map(|i| (2.0 * std::f64::consts::PI * f * i as f64 / sr as f64).sin() as f32)
            .collect()
    }

    #[test]
    fn preserves_sine() {
        let x = sine(44100, 1000.0, 0.5);
        let y = resample_channel(&x, 44100, 48000, Quality::High);
        assert_eq!(y.len(), 24000);
        let reference = sine(48000, 1000.0, 0.5);
        // compare the middle (edges have filter transients)
        let err: f64 = (2000..22000).map(|i| (y[i] - reference[i]).abs() as f64).fold(0.0, f64::max);
        assert!(err < 1e-3, "max err {err}");
    }

    #[test]
    fn identity_rate() {
        let x = sine(48000, 440.0, 0.1);
        assert_eq!(resample_channel(&x, 48000, 48000, Quality::High), x);
    }

    #[test]
    fn downsample_rejects_alias() {
        // 20 kHz at 48k → 16k must be removed (above new Nyquist 8 kHz)
        let x = sine(48000, 20000.0, 0.5);
        let y = resample_channel(&x, 48000, 16000, Quality::High);
        let rms: f64 = (y[1000..7000].iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / 6000.0).sqrt();
        assert!(rms < 1e-3, "alias rms {rms}");
    }
}
