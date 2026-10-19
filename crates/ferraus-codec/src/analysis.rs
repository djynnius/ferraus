//! Measurements used by `info`: EBU R128 loudness (via `ebur128`) and 1/3-octave averages.

use ferraus_core::{AudioBuffer, Error, Result};
use rustfft::num_complex::Complex;
use serde_json::{json, Value};

/// Incremental EBU R128 meter (integrated loudness, LRA, true peak, sample peak).
pub struct Loudness {
    meter: ebur128::EbuR128,
    channels: usize,
    sample_peak: f32,
}

fn db(x: f64) -> Value {
    if x.is_finite() {
        json!((x * 100.0).round() / 100.0)
    } else {
        Value::Null
    }
}

impl Loudness {
    pub fn new(channels: usize, sample_rate: u32) -> Result<Loudness> {
        let mode = ebur128::Mode::I | ebur128::Mode::LRA | ebur128::Mode::TRUE_PEAK | ebur128::Mode::SAMPLE_PEAK;
        let meter = ebur128::EbuR128::new(channels.max(1) as u32, sample_rate, mode)
            .map_err(|e| Error::processing(format!("loudness meter: {e:?}")))?;
        Ok(Loudness { meter, channels: channels.max(1), sample_peak: 0.0 })
    }

    pub fn add(&mut self, buf: &AudioBuffer) -> Result<()> {
        let b;
        let buf = if buf.channels() != self.channels {
            b = buf.with_channels(self.channels);
            &b
        } else {
            buf
        };
        for c in &buf.data {
            for s in c {
                self.sample_peak = self.sample_peak.max(s.abs());
            }
        }
        let planes: Vec<&[f32]> = buf.data.iter().map(|c| c.as_slice()).collect();
        self.meter.add_frames_planar_f32(&planes).map_err(|e| Error::processing(format!("loudness meter: {e:?}")))
    }

    pub fn result(&self) -> Value {
        let i = self.meter.loudness_global().unwrap_or(f64::NEG_INFINITY);
        let lra = self.meter.loudness_range().unwrap_or(0.0);
        let mut tp = 0.0f64;
        for c in 0..self.channels {
            tp = tp.max(self.meter.true_peak(c as u32).unwrap_or(0.0));
        }
        json!({
            "integrated_lufs": db(i),
            "loudness_range_lu": db(lra),
            "true_peak_dbtp": db(20.0 * tp.max(1e-12).log10()),
            "sample_peak_dbfs": db(20.0 * (self.sample_peak as f64).max(1e-12).log10()),
        })
    }
}

/// ISO 266 nominal 1/3-octave centre frequencies from 25 Hz to 20 kHz.
pub const THIRD_OCTAVE: [f64; 30] = [
    25.0, 31.5, 40.0, 50.0, 63.0, 80.0, 100.0, 125.0, 160.0, 200.0, 250.0, 315.0, 400.0, 500.0, 630.0, 800.0, 1000.0,
    1250.0, 1600.0, 2000.0, 2500.0, 3150.0, 4000.0, 5000.0, 6300.0, 8000.0, 10000.0, 12500.0, 16000.0, 20000.0,
];

/// Average power spectrum (Welch, Hann window) accumulated over chunks of a mono mix.
pub struct Spectrum {
    n: usize,
    sr: u32,
    fft: std::sync::Arc<dyn rustfft::Fft<f32>>,
    window: Vec<f32>,
    acc: Vec<f64>,
    frames: usize,
    pending: Vec<f32>,
}

impl Spectrum {
    pub fn new(sample_rate: u32) -> Spectrum {
        let n = 8192;
        let mut planner = rustfft::FftPlanner::new();
        let window =
            (0..n).map(|i| (0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / n as f64).cos()) as f32).collect();
        Spectrum {
            n,
            sr: sample_rate,
            fft: planner.plan_fft_forward(n),
            window,
            acc: vec![0.0; n / 2 + 1],
            frames: 0,
            pending: Vec::new(),
        }
    }

    pub fn add(&mut self, buf: &AudioBuffer) {
        self.pending.extend(buf.mono_mix());
        let hop = self.n / 2;
        let mut start = 0;
        let mut scratch = vec![Complex::new(0.0f32, 0.0); self.n];
        while self.pending.len() - start >= self.n {
            for (i, s) in scratch.iter_mut().enumerate() {
                *s = Complex::new(self.pending[start + i] * self.window[i], 0.0);
            }
            self.fft.process(&mut scratch);
            for (a, c) in self.acc.iter_mut().zip(&scratch) {
                *a += c.norm_sqr() as f64;
            }
            self.frames += 1;
            start += hop;
        }
        self.pending.drain(..start);
    }

    /// Band levels in dB relative to a full-scale sine (a 0 dBFS sine inside a band reads ~0 dB).
    pub fn result(&mut self) -> Value {
        if self.frames == 0 && !self.pending.is_empty() {
            let pad = self.n - self.pending.len().min(self.n);
            self.pending.extend(std::iter::repeat_n(0.0, pad));
            let b = AudioBuffer::from_mono(self.sr, Vec::new());
            self.add(&b);
        }
        let frames = self.frames.max(1) as f64;
        let wsum: f64 = self.window.iter().map(|w| *w as f64).sum();
        // a full-scale sine gives |X|^2 = (A * wsum / 2)^2 summed over ~1.5 bins (Hann)
        let ref_power = (wsum / 2.0).powi(2) * 1.5;
        let bin_hz = self.sr as f64 / self.n as f64;
        let nyq = self.sr as f64 / 2.0;
        let bands: Vec<Value> = THIRD_OCTAVE
            .iter()
            .filter(|&&fc| fc * 2f64.powf(1.0 / 6.0) <= nyq)
            .map(|&fc| {
                let lo = fc / 2f64.powf(1.0 / 6.0);
                let hi = fc * 2f64.powf(1.0 / 6.0);
                let a = (lo / bin_hz).ceil() as usize;
                let b = ((hi / bin_hz).floor() as usize).min(self.acc.len() - 1);
                let p: f64 = if b >= a { self.acc[a..=b].iter().sum::<f64>() / frames } else { 0.0 };
                let d = 10.0 * (p / ref_power).max(1e-20).log10();
                json!({"center_hz": fc, "db": db(d.max(-200.0))})
            })
            .collect();
        json!({"bands": bands, "fft_size": self.n, "frames": self.frames})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferraus_core::testing::sine;

    #[test]
    fn loudness_of_sine() {
        // BS.1770: a 1 kHz sine in one channel reads 3.01 dB below its peak level, so -20 dBFS peak is about -23 LUFS
        let s = sine(48000, 1000.0, 5.0, 0.1);
        let mut l = Loudness::new(1, 48000).unwrap();
        l.add(&s).unwrap();
        let r = l.result();
        let i = r["integrated_lufs"].as_f64().unwrap();
        assert!((i - (-23.0)).abs() < 0.5, "{i}");
        let tp = r["true_peak_dbtp"].as_f64().unwrap();
        assert!((tp - (-20.0)).abs() < 0.3, "{tp}");
    }

    #[test]
    fn spectrum_peak_band() {
        let s = sine(48000, 1000.0, 2.0, 1.0);
        let mut sp = Spectrum::new(48000);
        sp.add(&s);
        let r = sp.result();
        let bands = r["bands"].as_array().unwrap();
        let best = bands
            .iter()
            .max_by(|a, b| a["db"].as_f64().unwrap().partial_cmp(&b["db"].as_f64().unwrap()).unwrap())
            .unwrap();
        assert_eq!(best["center_hz"].as_f64().unwrap(), 1000.0);
        assert!(best["db"].as_f64().unwrap().abs() < 2.0, "{best}");
    }
}
