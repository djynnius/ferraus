//! STFT / iSTFT matching `torch.stft(center=True, pad_mode="reflect", window=hann(periodic))`
//! and `torch.istft(center=True)`, used by the MDX adapter (spectrogram models whose STFT
//! lives outside the graph), by ensembles (magnitude averaging), and by the mask-confidence
//! quality estimate.

use realfft::num_complex::Complex32;
use realfft::RealFftPlanner;

/// Complex spectrogram `[frame][bin]` of one channel.
#[derive(Clone, Debug, PartialEq)]
pub struct Spec {
    pub frames: Vec<Vec<Complex32>>,
    pub n_fft: usize,
    pub hop: usize,
    /// Signal length before padding (for the inverse).
    pub len: usize,
}

pub fn hann(n: usize) -> Vec<f32> {
    (0..n).map(|i| (0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / n as f64).cos()) as f32).collect()
}

fn reflect(x: &[f32], i: isize) -> f32 {
    let n = x.len() as isize;
    if n == 1 {
        return x[0];
    }
    let period = 2 * (n - 1);
    let mut j = i.rem_euclid(period);
    if j >= n {
        j = period - j;
    }
    x[j as usize]
}

/// Forward STFT (unnormalized, like torch's default).
pub fn stft(x: &[f32], n_fft: usize, hop: usize) -> Spec {
    let win = hann(n_fft);
    let pad = (n_fft / 2) as isize;
    let padded_len = x.len() + n_fft;
    let n_frames = if x.is_empty() { 0 } else { 1 + (padded_len - n_fft) / hop };
    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(n_fft);
    let mut input = fft.make_input_vec();
    let mut out = fft.make_output_vec();
    let mut frames = Vec::with_capacity(n_frames);
    for f in 0..n_frames {
        let start = (f * hop) as isize - pad;
        for (i, v) in input.iter_mut().enumerate() {
            *v = reflect(x, start + i as isize) * win[i];
        }
        fft.process(&mut input, &mut out).expect("fft");
        frames.push(out.clone());
    }
    Spec { frames, n_fft, hop, len: x.len() }
}

/// Inverse STFT with window-squared normalization; returns `len` samples.
pub fn istft(s: &Spec, len: usize) -> Vec<f32> {
    let n_fft = s.n_fft;
    let hop = s.hop;
    let win = hann(n_fft);
    let pad = n_fft / 2;
    let total = (s.frames.len().saturating_sub(1)) * hop + n_fft;
    let mut y = vec![0f32; total.max(len + 2 * pad)];
    let mut wsum = vec![0f32; y.len()];
    let mut planner = RealFftPlanner::<f32>::new();
    let ifft = planner.plan_fft_inverse(n_fft);
    let mut buf = ifft.make_input_vec();
    let mut out = ifft.make_output_vec();
    for (f, frame) in s.frames.iter().enumerate() {
        buf.copy_from_slice(frame);
        // the DC and Nyquist bins of a real signal have no imaginary part
        buf[0].im = 0.0;
        if let Some(last) = buf.last_mut() {
            last.im = 0.0;
        }
        ifft.process(&mut buf, &mut out).expect("ifft");
        let off = f * hop;
        for i in 0..n_fft {
            y[off + i] += out[i] / n_fft as f32 * win[i];
            wsum[off + i] += win[i] * win[i];
        }
    }
    (0..len)
        .map(|i| {
            let w = wsum[i + pad];
            if w > 1e-8 {
                y[i + pad] / w
            } else {
                0.0
            }
        })
        .collect()
}

/// Magnitude-domain combination of several estimates of the same stem (one channel).
///
/// * `avg`: magnitude = weighted mean of magnitudes, phase of the weighted complex mean.
/// * `max` / `min`: per bin, the estimate with the largest / smallest magnitude.
pub fn combine(estimates: &[&[f32]], weights: &[f64], mode: &str, n_fft: usize, hop: usize) -> Vec<f32> {
    let len = estimates[0].len();
    let specs: Vec<Spec> = estimates.iter().map(|e| stft(e, n_fft, hop)).collect();
    let wsum: f64 = weights.iter().sum::<f64>().max(1e-12);
    let mut out = specs[0].clone();
    for f in 0..out.frames.len() {
        for b in 0..out.frames[f].len() {
            let vals: Vec<Complex32> = specs.iter().map(|s| s.frames[f][b]).collect();
            out.frames[f][b] = match mode {
                "max" => *vals.iter().max_by(|a, b| a.norm_sqr().total_cmp(&b.norm_sqr())).unwrap(),
                "min" => *vals.iter().min_by(|a, b| a.norm_sqr().total_cmp(&b.norm_sqr())).unwrap(),
                _ => {
                    let mut c = Complex32::new(0.0, 0.0);
                    let mut mag = 0f64;
                    for (v, w) in vals.iter().zip(weights) {
                        c += v * (*w as f32);
                        mag += v.norm() as f64 * w;
                    }
                    let mag = (mag / wsum) as f32;
                    let n = c.norm();
                    if n > 1e-20 {
                        c * (mag / n)
                    } else {
                        Complex32::new(0.0, 0.0)
                    }
                }
            };
        }
    }
    istft(&out, len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perfect_reconstruction() {
        let x: Vec<f32> =
            (0..5000).map(|i| ((i as f32) * 0.05).sin() * 0.7 + ((i * 13 % 17) as f32 - 8.0) * 0.01).collect();
        for (n, h) in [(2048usize, 512usize), (1024, 256), (7680, 1024)] {
            let s = stft(&x, n, h);
            let y = istft(&s, x.len());
            let err = x.iter().zip(&y).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
            assert!(err < 1e-4, "n_fft {n}: {err}");
        }
    }

    #[test]
    fn frame_count_matches_torch() {
        // torch: 1 + len // hop frames with center=True
        let s = stft(&vec![0.0; 261_120], 7680, 1024);
        assert_eq!(s.frames.len(), 256);
        assert_eq!(s.frames[0].len(), 3841);
    }

    #[test]
    fn combine_single_is_identity_and_modes_order() {
        let a: Vec<f32> = (0..4096).map(|i| ((i as f32) * 0.03).sin()).collect();
        let b: Vec<f32> = a.iter().map(|v| v * 0.5).collect();
        let y = combine(&[&a], &[1.0], "avg", 1024, 256);
        let err = a.iter().zip(&y).map(|(p, q)| (p - q).abs()).fold(0f32, f32::max);
        assert!(err < 1e-4);
        let mx = combine(&[&a, &b], &[1.0, 1.0], "max", 1024, 256);
        let mn = combine(&[&a, &b], &[1.0, 1.0], "min", 1024, 256);
        let av = combine(&[&a, &b], &[1.0, 1.0], "avg", 1024, 256);
        let rms = |v: &[f32]| (v.iter().map(|x| x * x).sum::<f32>() / v.len() as f32).sqrt();
        assert!((rms(&mx) - rms(&a)).abs() < 1e-3);
        assert!((rms(&mn) - rms(&b)).abs() < 1e-3);
        assert!((rms(&av) - 0.75 * rms(&a)).abs() < 1e-3);
    }
}
