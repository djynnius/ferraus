//! Pure-Rust pYIN pitch tracker (Mauch & Dixon, "pYIN: a fundamental frequency estimator
//! using probabilistic threshold distributions", ICASSP 2014).
//!
//! 1. For every frame the YIN difference function is computed with an FFT-based
//!    autocorrelation, then normalised by its cumulative mean (CMNDF).
//! 2. Instead of one threshold, a Beta-distributed prior over 100 thresholds (0.01..1.00)
//!    is used: each threshold picks the first CMNDF trough below it, so each trough
//!    collects the prior mass of the thresholds that chose it. The result is a small set of
//!    pitch candidates with probabilities; their sum is the frame's voicing probability.
//! 3. A hidden Markov model over pitch bins (`bin_cents` apart between `fmin` and `fmax`)
//!    times {voiced, unvoiced} with a triangular pitch-transition band and a sticky voicing
//!    switch is decoded with Viterbi. The output frequency of a voiced frame is the exact
//!    (parabolically interpolated) candidate nearest the decoded bin.
//!
//! ```
//! use ferraus_pitch::pyin::{track, PyinOptions};
//! let sr = 16000;
//! let x: Vec<f32> = (0..sr).map(|i| (2.0 * std::f32::consts::PI * 220.0 * i as f32 / sr as f32).sin() * 0.5).collect();
//! let frames = track(&x, sr, &PyinOptions::default());
//! let mid = &frames[frames.len() / 2];
//! assert!(mid.voiced && (mid.freq_hz - 220.0).abs() < 1.0);
//! ```

use crate::util::Fft;
use serde::Serialize;

/// pYIN settings. Defaults: 60 Hz..1.2 kHz, 10 ms hop, 20-cent HMM bins.
#[derive(Clone, Debug)]
pub struct PyinOptions {
    /// Lowest pitch considered (Hz).
    pub fmin: f64,
    /// Highest pitch considered (Hz).
    pub fmax: f64,
    /// Hop between frame centres (seconds). Frame `k` is centred on `k * hop`.
    pub hop_s: f64,
    /// HMM pitch-bin resolution (cents).
    pub bin_cents: f64,
    /// Mean of the Beta prior over YIN thresholds (pYIN uses 0.10, 0.15 or 0.20).
    pub beta_mean: f64,
    /// Probability of staying voiced (or unvoiced) from one frame to the next.
    pub voicing_stay: f64,
    /// Fastest pitch movement the HMM allows, in octaves per second.
    pub max_octaves_per_s: f64,
    /// Frames quieter than this (dBFS RMS) get no pitch candidates.
    pub silence_db: f64,
    /// Weight given to YIN candidate probabilities against the unvoiced hypothesis.
    pub yin_trust: f64,
}

impl Default for PyinOptions {
    fn default() -> Self {
        PyinOptions {
            fmin: 60.0,
            fmax: 1200.0,
            hop_s: 0.010,
            bin_cents: 20.0,
            beta_mean: 0.15,
            voicing_stay: 0.99,
            max_octaves_per_s: 35.92,
            silence_db: -60.0,
            yin_trust: 0.5,
        }
    }
}

/// One analysis frame of a pitch track.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct PitchFrame {
    /// Frame centre (seconds).
    pub time_s: f64,
    /// Fundamental frequency in Hz; 0 when unvoiced.
    pub freq_hz: f64,
    /// Voicing probability (sum of the YIN candidate probabilities, 0..1).
    pub confidence: f64,
    /// Decoded voicing decision.
    pub voiced: bool,
    /// RMS of the frame (linear, full scale = 1).
    pub rms: f64,
}

impl PitchFrame {
    /// Fractional MIDI note number (`None` when unvoiced).
    pub fn midi(&self) -> Option<f64> {
        (self.voiced && self.freq_hz > 0.0)
            .then(|| ferraus_core::units::hz_to_midi(self.freq_hz))
    }
}

/// A YIN pitch candidate for one frame.
#[derive(Clone, Copy, Debug)]
pub struct Candidate {
    pub freq_hz: f64,
    pub prob: f64,
}

/// Hop in samples for a sample rate and hop duration (at least 1).
pub fn hop_samples(sr: u32, hop_s: f64) -> usize {
    ((hop_s * sr as f64).round() as usize).max(1)
}

/// Track the pitch of a mono signal. Returns one frame per hop (frame `k` centred on
/// `k * hop`), covering the whole input.
pub fn track(samples: &[f32], sr: u32, opts: &PyinOptions) -> Vec<PitchFrame> {
    let (cands, rms, hop) = candidates(samples, sr, opts);
    if cands.is_empty() {
        return Vec::new();
    }
    let path = viterbi(&cands, sr, hop, opts);
    let n_bins = n_bins(opts);
    cands
        .iter()
        .zip(&rms)
        .zip(&path)
        .enumerate()
        .map(|(k, ((c, r), state))| {
            let conf: f64 = c.iter().map(|c| c.prob).sum::<f64>().min(1.0);
            let time_s = (k * hop) as f64 / sr as f64;
            match state {
                Some(b) => {
                    let centre = bin_freq(*b, opts);
                    // nearest exact candidate to the decoded bin
                    let best = c
                        .iter()
                        .filter(|c| (bin_of(c.freq_hz, opts, n_bins) as i64 - *b as i64).abs() <= 2)
                        .min_by(|x, y| {
                            let dx = (x.freq_hz / centre).log2().abs();
                            let dy = (y.freq_hz / centre).log2().abs();
                            dx.partial_cmp(&dy).unwrap_or(std::cmp::Ordering::Equal)
                        });
                    let f = best.map(|c| c.freq_hz).unwrap_or(centre);
                    PitchFrame {
                        time_s,
                        freq_hz: f,
                        confidence: conf,
                        voiced: true,
                        rms: *r,
                    }
                }
                None => PitchFrame {
                    time_s,
                    freq_hz: 0.0,
                    confidence: conf,
                    voiced: false,
                    rms: *r,
                },
            }
        })
        .collect()
}

fn n_bins(o: &PyinOptions) -> usize {
    ((1200.0 * (o.fmax / o.fmin).log2() / o.bin_cents).floor() as usize + 1).max(2)
}

fn bin_freq(b: usize, o: &PyinOptions) -> f64 {
    o.fmin * 2f64.powf(b as f64 * o.bin_cents / 1200.0)
}

fn bin_of(f: f64, o: &PyinOptions, n: usize) -> usize {
    let b = (1200.0 * (f.max(1e-9) / o.fmin).log2() / o.bin_cents).round();
    b.clamp(0.0, (n - 1) as f64) as usize
}

/// Beta(α, β) prior over thresholds 0.01..1.00 with the requested mean (α = 2).
fn threshold_prior(mean: f64) -> Vec<f64> {
    let a = 2.0;
    let b = a * (1.0 - mean) / mean;
    let mut p: Vec<f64> = (0..100)
        .map(|i| {
            let t = (i as f64 + 0.5) / 100.0;
            t.powf(a - 1.0) * (1.0 - t).powf(b - 1.0)
        })
        .collect();
    let s: f64 = p.iter().sum();
    p.iter_mut().for_each(|v| *v /= s);
    p
}

/// Stage 1 and 2: YIN candidates per frame, plus frame RMS and the hop in samples.
pub fn candidates(
    samples: &[f32],
    sr: u32,
    opts: &PyinOptions,
) -> (Vec<Vec<Candidate>>, Vec<f64>, usize) {
    let hop = hop_samples(sr, opts.hop_s);
    if samples.is_empty() || sr == 0 {
        return (Vec::new(), Vec::new(), hop);
    }
    let srf = sr as f64;
    let min_lag = ((srf / opts.fmax).floor() as usize).saturating_sub(1).max(2);
    let max_lag = (srf / opts.fmin).ceil() as usize + 1;
    let w = max_lag.max((0.020 * srf) as usize); // integration window
    let span = w + max_lag;
    let nfft = (span + 1).next_power_of_two();
    let pad = span / 2;
    let n_frames = samples.len() / hop + 1;

    // padded signal and prefix sums of squares
    let mut xp = vec![0.0f32; pad + samples.len() + span + nfft];
    xp[pad..pad + samples.len()].copy_from_slice(samples);
    let mut psq = vec![0.0f64; xp.len() + 1];
    for (i, v) in xp.iter().enumerate() {
        psq[i + 1] = psq[i] + (*v as f64) * (*v as f64);
    }
    let prior = threshold_prior(opts.beta_mean);
    let silence = 10f64.powf(opts.silence_db / 20.0);

    let mut fft_a = Fft::new(nfft);
    let mut spec_b = vec![realfft::num_complex::Complex::new(0.0f32, 0.0); nfft / 2 + 1];
    let mut buf_b = vec![0.0f32; nfft];
    let mut d = vec![0.0f64; max_lag + 1];
    let mut cm = vec![1.0f64; max_lag + 1];

    let mut all = Vec::with_capacity(n_frames);
    let mut rms_v = Vec::with_capacity(n_frames);
    for k in 0..n_frames {
        let s = k * hop; // frame start in padded coordinates (centre = s + pad)
        // RMS over the integration window centred on the frame centre
        let c = s + pad;
        let r0 = c.saturating_sub(w / 2);
        let r1 = (r0 + w).min(xp.len());
        let rms = ((psq[r1] - psq[r0]) / (r1 - r0).max(1) as f64).max(0.0).sqrt();
        rms_v.push(rms);
        if rms < silence {
            all.push(Vec::new());
            continue;
        }
        // cross term r(τ) = Σ_{j<w} x[s+j] x[s+j+τ] via FFT
        fft_a.time[..w].copy_from_slice(&xp[s..s + w]);
        fft_a.time[w..].iter_mut().for_each(|v| *v = 0.0);
        fft_a.forward();
        buf_b[..span].copy_from_slice(&xp[s..s + span]);
        buf_b[span..].iter_mut().for_each(|v| *v = 0.0);
        fft_a.forward_into(&mut buf_b, &mut spec_b);
        for (a, b) in fft_a.freq.iter_mut().zip(&spec_b) {
            *a = a.conj() * b;
        }
        fft_a.inverse();
        let scale = 1.0 / nfft as f64;
        let e0 = psq[s + w] - psq[s];
        for (tau, dv) in d.iter_mut().enumerate() {
            let et = psq[s + tau + w] - psq[s + tau];
            let r = fft_a.time[tau] as f64 * scale;
            *dv = (e0 + et - 2.0 * r).max(0.0);
        }
        // cumulative mean normalised difference
        cm[0] = 1.0;
        let mut run = 0.0;
        for tau in 1..=max_lag {
            run += d[tau];
            cm[tau] = if run > 1e-12 {
                d[tau] * tau as f64 / run
            } else {
                1.0
            };
        }
        // troughs in lag order; each takes the prior mass of thresholds above its value
        // that no earlier trough took
        let mut cands = Vec::new();
        let mut running_min = f64::INFINITY; // thresholds above this are already taken
        let mut taken_to = 100usize; // index of first taken threshold
        for tau in min_lag.max(1)..max_lag {
            if !(cm[tau] < cm[tau - 1] && cm[tau] <= cm[tau + 1]) {
                continue;
            }
            let v = cm[tau];
            if v >= running_min || v >= 1.0 {
                continue;
            }
            // thresholds t_i = (i+1)/100 with t_i > v
            let first = ((v * 100.0).floor() as usize).min(100);
            let mass: f64 = if first < taken_to {
                prior[first..taken_to].iter().sum()
            } else {
                0.0
            };
            running_min = v;
            taken_to = taken_to.min(first);
            if mass <= 0.0 {
                continue;
            }
            // parabolic interpolation of the trough
            let (a, b, cc) = (cm[tau - 1], cm[tau], cm[tau + 1]);
            let den = a - 2.0 * b + cc;
            let off = if den.abs() > 1e-12 {
                (0.5 * (a - cc) / den).clamp(-0.5, 0.5)
            } else {
                0.0
            };
            let f = srf / (tau as f64 + off);
            if f >= opts.fmin * 0.97 && f <= opts.fmax * 1.03 {
                cands.push(Candidate {
                    freq_hz: f,
                    prob: mass,
                });
            }
            if taken_to == 0 {
                break;
            }
        }
        all.push(cands);
    }
    (all, rms_v, hop)
}

/// Stage 3: Viterbi decoding. Returns the voiced pitch bin per frame (`None` = unvoiced).
fn viterbi(
    cands: &[Vec<Candidate>],
    sr: u32,
    hop: usize,
    opts: &PyinOptions,
) -> Vec<Option<usize>> {
    let n = n_bins(opts);
    let t_len = cands.len();
    let hop_s = hop as f64 / sr as f64;
    let jump =
        ((opts.max_octaves_per_s * 1200.0 / opts.bin_cents * hop_s).round() as usize).max(1);
    // triangular transition weights, normalised over the band
    let norm = ((jump + 1) * (jump + 1)) as f64;
    let logw: Vec<f64> = (0..=jump)
        .map(|dlt| (((jump + 1 - dlt) as f64) / norm).ln())
        .collect();
    let ls = opts.voicing_stay.ln();
    let lsw = (1.0 - opts.voicing_stay).max(1e-12).ln();
    let floor = 1e-9f64;

    let obs = |t: usize, ov: &mut [f64], ou: &mut [f64]| {
        ov.iter_mut().for_each(|v| *v = 0.0);
        let mut total = 0.0;
        for c in &cands[t] {
            let b = bin_of(c.freq_hz, opts, n);
            ov[b] += opts.yin_trust * c.prob;
            total += c.prob;
        }
        let unv = ((1.0 - opts.yin_trust * total.min(1.0)) / n as f64).max(floor);
        for v in ov.iter_mut() {
            *v = v.max(floor).ln();
        }
        ou.iter_mut().for_each(|v| *v = unv.ln());
    };

    let mut ov = vec![0.0; n];
    let mut ou = vec![0.0; n];
    obs(0, &mut ov, &mut ou);
    let init = (0.5 / n as f64).ln();
    let mut dv: Vec<f64> = ov.iter().map(|o| init + o).collect();
    let mut du: Vec<f64> = ou.iter().map(|o| init + o).collect();
    let mut back: Vec<u16> = vec![0; t_len * 2 * n];
    let mut mv = vec![0.0; n];
    let mut mu = vec![0.0; n];
    let mut sv = vec![0u16; n]; // source state for mv (index into 0..2n)
    let mut su = vec![0u16; n];
    let mut nv = vec![0.0; n];
    let mut nu = vec![0.0; n];
    for t in 1..t_len {
        obs(t, &mut ov, &mut ou);
        for i in 0..n {
            // into voiced targets
            let a = dv[i] + ls;
            let b = du[i] + lsw;
            if a >= b {
                mv[i] = a;
                sv[i] = i as u16;
            } else {
                mv[i] = b;
                sv[i] = (i + n) as u16;
            }
            // into unvoiced targets
            let a = dv[i] + lsw;
            let b = du[i] + ls;
            if a >= b {
                mu[i] = a;
                su[i] = i as u16;
            } else {
                mu[i] = b;
                su[i] = (i + n) as u16;
            }
        }
        let row = &mut back[t * 2 * n..(t + 1) * 2 * n];
        for j in 0..n {
            let lo = j.saturating_sub(jump);
            let hi = (j + jump).min(n - 1);
            let mut bv = f64::NEG_INFINITY;
            let mut bvi = 0u16;
            let mut bu = f64::NEG_INFINITY;
            let mut bui = 0u16;
            for i in lo..=hi {
                let w = logw[i.abs_diff(j)];
                let x = mv[i] + w;
                if x > bv {
                    bv = x;
                    bvi = sv[i];
                }
                let y = mu[i] + w;
                if y > bu {
                    bu = y;
                    bui = su[i];
                }
            }
            nv[j] = bv + ov[j];
            nu[j] = bu + ou[j];
            row[j] = bvi;
            row[j + n] = bui;
        }
        // renormalise to keep numbers small
        let m = nv
            .iter()
            .chain(nu.iter())
            .cloned()
            .fold(f64::NEG_INFINITY, f64::max);
        for j in 0..n {
            dv[j] = nv[j] - m;
            du[j] = nu[j] - m;
        }
    }
    // backtrack
    let mut state = 0usize;
    let mut best = f64::NEG_INFINITY;
    for j in 0..n {
        if dv[j] > best {
            best = dv[j];
            state = j;
        }
        if du[j] > best {
            best = du[j];
            state = j + n;
        }
    }
    let mut path = vec![None; t_len];
    for t in (0..t_len).rev() {
        path[t] = (state < n).then_some(state);
        if t > 0 {
            state = back[t * 2 * n + state] as usize;
        }
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::*;

    fn accuracy(frames: &[PitchFrame], truth: impl Fn(f64) -> f64, skip_s: f64) -> (f64, f64) {
        let end = frames.last().map(|f| f.time_s).unwrap_or(0.0);
        let mut n = 0;
        let mut ok = 0;
        let mut octave = 0;
        for f in frames {
            if f.time_s < skip_s || f.time_s > end - skip_s {
                continue;
            }
            n += 1;
            if f.voiced {
                let c = crate::util::cents(truth(f.time_s), f.freq_hz).abs();
                if c <= 50.0 {
                    ok += 1;
                } else if (c - 1200.0).abs() < 100.0 || (c - 1902.0).abs() < 100.0 {
                    octave += 1;
                }
            }
        }
        (ok as f64 / n as f64, octave as f64 / n as f64)
    }

    #[test]
    fn sine_and_vocal_accuracy_80_to_1000_hz() {
        let sr = 16000;
        let mut worst: f64 = 1.0;
        let mut worst_oct: f64 = 0.0;
        for &f0 in &[80.0, 110.0, 165.0, 220.0, 330.0, 440.0, 660.0, 1000.0] {
            for kind in 0..2 {
                let x = if kind == 0 {
                    sine_wave(sr, f0, 0.5, 0.5)
                } else {
                    vocal(sr, &[(f0, 0.5)], Vowel::A, 0.0, 0.0)
                };
                let fr = track(&x, sr, &PyinOptions::default());
                let (acc, oct) = accuracy(&fr, |_| f0, 0.06);
                worst = worst.min(acc);
                worst_oct = worst_oct.max(oct);
                assert!(acc >= 0.97, "f0 {f0} kind {kind}: accuracy {acc}");
            }
        }
        eprintln!("pyin worst raw pitch accuracy {worst:.3}, worst octave-error rate {worst_oct:.3}");
        assert!(worst_oct < 0.02);
    }

    #[test]
    fn exact_frequency_on_steady_vocal() {
        let sr = 22050;
        let x = vocal(sr, &[(196.0, 0.6)], Vowel::A, 0.0, 0.0);
        let fr = track(&x, sr, &PyinOptions::default());
        let mids: Vec<f64> = fr[10..fr.len() - 10].iter().map(|f| f.freq_hz).collect();
        let m = crate::util::median(&mids).unwrap();
        assert!(crate::util::cents(196.0, m).abs() < 2.0, "median {m}");
    }

    #[test]
    fn glide_is_tracked() {
        let sr = 16000;
        let x = ferraus_core::testing::chirp(sr, 150.0, 600.0, 1.0, 0.5).data.remove(0);
        let fr = track(&x, sr, &PyinOptions::default());
        let (acc, _) = accuracy(&fr, |t| 150.0 + 450.0 * t, 0.06);
        assert!(acc > 0.95, "glide accuracy {acc}");
    }

    #[test]
    fn silence_and_noise_are_unvoiced() {
        let sr = 16000;
        let mut x = vec![0.0f32; sr as usize / 2];
        x.extend(ferraus_core::testing::noise(sr, 0.5, 0.3, 7).data.remove(0));
        let fr = track(&x, sr, &PyinOptions::default());
        let voiced = fr.iter().filter(|f| f.voiced).count();
        assert!(voiced as f64 / fr.len() as f64 <= 0.05, "voiced {voiced}/{}", fr.len());
        // silence then tone then silence: voicing follows the tone
        let mut y = vec![0.0f32; 4000];
        y.extend(sine_wave(sr, 300.0, 0.4, 0.4));
        y.extend(vec![0.0f32; 4000]);
        let fr = track(&y, sr, &PyinOptions::default());
        for f in &fr {
            let inside = f.time_s > 0.27 && f.time_s < 0.63;
            let outside = f.time_s < 0.22 || f.time_s > 0.68;
            if inside {
                assert!(f.voiced, "{f:?}");
            }
            if outside {
                assert!(!f.voiced, "{f:?}");
            }
        }
    }

    #[test]
    fn empty_and_tiny_inputs() {
        assert!(track(&[], 16000, &PyinOptions::default()).is_empty());
        let fr = track(&[0.1; 10], 16000, &PyinOptions::default());
        assert_eq!(fr.len(), 1);
        assert!(!fr[0].voiced);
    }
}
