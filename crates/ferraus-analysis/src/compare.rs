//! Null-test comparison (`compare a b`) and perceptual similarity (`analyze similarity`).

use crate::util::{self, amp_db, pow_db};
use ferraus_core::resample::{resample, Quality};
use ferraus_core::AudioBuffer;
use serde::Serialize;

/// Options for [`compare`].
#[derive(Clone, Debug)]
pub struct CompareOptions {
    /// Largest offset searched when aligning `b` to `a` (seconds).
    pub max_offset_s: f64,
    /// Skip alignment (compare sample-for-sample).
    pub no_align: bool,
}

impl Default for CompareOptions {
    fn default() -> Self {
        CompareOptions { max_offset_s: 1.0, no_align: false }
    }
}

/// Null-test metrics of `b` against reference `a`.
#[derive(Clone, Debug, Serialize)]
pub struct CompareReport {
    /// Samples by which `b` lags `a` (negative: `b` leads). Found by FFT cross-correlation.
    pub offset_samples: i64,
    pub offset_s: f64,
    /// Normalised cross-correlation at the chosen offset (−1..1).
    pub alignment_correlation: f64,
    /// `b` is polarity-inverted relative to `a`.
    pub polarity_inverted: bool,
    /// Frames compared after alignment.
    pub compared_frames: usize,
    /// Duration of `b` minus duration of `a` (seconds).
    pub length_difference_s: f64,
    /// RMS of `a − b` over the aligned region (dBFS, floored at −240).
    pub rms_diff_db: f64,
    /// `rms_diff_db` relative to the RMS of `a` (how deep the null is).
    pub null_depth_db: f64,
    /// Peak of `a − b` (dBFS).
    pub diff_peak_db: f64,
    /// Peak of `b` minus peak of `a` (dB).
    pub peak_delta_db: f64,
    /// RMS of `b` minus RMS of `a` (dB).
    pub rms_delta_db: f64,
    /// Integrated loudness of `b` minus `a` (LU); null when either is unmeasurable.
    pub loudness_delta_lu: f64,
    /// Least-squares gain that best maps `a` onto `b` (dB).
    pub gain_db: f64,
    /// Scale-invariant SDR of `b` against `a` (dB, capped at 200).
    pub si_sdr_db: f64,
    /// Same length, zero offset, and bit-identical samples.
    pub identical: bool,
    /// `identical`, `transparent` (null below −90 dB), `gain_only`, `similar`, or `different`.
    pub verdict: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// Bring `b` to `a`'s sample rate and channel count, noting what changed.
fn conform(a: &AudioBuffer, b: &AudioBuffer, notes: &mut Vec<String>) -> AudioBuffer {
    let mut b2 = b.clone();
    if b2.sample_rate != a.sample_rate {
        notes.push(format!("resampled b from {} Hz to {} Hz", b2.sample_rate, a.sample_rate));
        b2 = resample(&b2, a.sample_rate, Quality::High);
    }
    if b2.channels() != a.channels() {
        notes.push(format!("converted b from {} to {} channel(s)", b2.channels(), a.channels()));
        b2 = b2.with_channels(a.channels());
    }
    b2
}

/// Offset (samples) by which `b` lags `a`, from FFT cross-correlation of the mono mixes
/// over at most the first 30 s: returns `(lag, normalised_correlation)`.
pub fn find_offset(a: &AudioBuffer, b: &AudioBuffer, max_offset_s: f64) -> (i64, f64) {
    let sr = a.sample_rate as f64;
    let max_lag = (max_offset_s.max(0.0) * sr).round() as usize;
    let span = (30.0 * sr) as usize + max_lag;
    let ma = a.mono_mix();
    let mb = b.mono_mix();
    let la = ma.len().min(span);
    let lb = mb.len().min(span);
    let (lag, _, norm) = util::xcorr_peak(&ma[..la], &mb[..lb], max_lag);
    (lag as i64, norm)
}

fn si_sdr(reference: &[f64], estimate: &[f64]) -> f64 {
    let dot: f64 = reference.iter().zip(estimate).map(|(r, e)| r * e).sum();
    let rr: f64 = reference.iter().map(|r| r * r).sum();
    if rr <= 1e-30 {
        return if estimate.iter().all(|e| *e == 0.0) { 200.0 } else { -200.0 };
    }
    let alpha = dot / rr;
    let (mut st, mut en) = (0.0f64, 0.0f64);
    for (r, e) in reference.iter().zip(estimate) {
        let t = alpha * r;
        st += t * t;
        en += (e - t).powi(2);
    }
    if en <= 0.0 {
        return 200.0;
    }
    if st <= 0.0 {
        return -200.0;
    }
    (10.0 * (st / en).log10()).clamp(-200.0, 200.0)
}

/// Compare `b` against reference `a`: align by cross-correlation, then measure the null.
pub fn compare(a: &AudioBuffer, b: &AudioBuffer, opts: &CompareOptions) -> CompareReport {
    let mut notes = Vec::new();
    let b = conform(a, b, &mut notes);
    let (lag, corr) = if opts.no_align { (0, 0.0) } else { find_offset(a, &b, opts.max_offset_s) };
    // aligned region: a[i] vs b[i + lag]
    let (a0, b0) = if lag >= 0 { (0usize, lag as usize) } else { ((-lag) as usize, 0usize) };
    let len = a.frames().saturating_sub(a0).min(b.frames().saturating_sub(b0));
    let ch = a.channels();
    let mut ra = Vec::with_capacity(len * ch);
    let mut rb = Vec::with_capacity(len * ch);
    for c in 0..ch {
        ra.extend(a.data[c][a0..a0 + len].iter().map(|v| *v as f64));
        rb.extend(b.data[c][b0..b0 + len].iter().map(|v| *v as f64));
    }
    let n = ra.len().max(1) as f64;
    let (mut sd, mut pd, mut sa, mut sb, mut ab) = (0.0f64, 0.0f64, 0.0f64, 0.0f64, 0.0f64);
    for (x, y) in ra.iter().zip(&rb) {
        let d = x - y;
        sd += d * d;
        pd = pd.max(d.abs());
        sa += x * x;
        sb += y * y;
        ab += x * y;
    }
    let rms_diff_db = pow_db(sd / n);
    let rms_a_db = pow_db(sa / n);
    let la = crate::loudness::integrated_lufs(a);
    let lb = crate::loudness::integrated_lufs(&b);
    let gain = if sa > 0.0 { ab / sa } else { 0.0 };
    let si = si_sdr(&ra, &rb);
    let identical = lag == 0 && a.frames() == b.frames() && pd == 0.0 && notes.is_empty();
    let null_depth = if sa > 0.0 { rms_diff_db - rms_a_db } else { rms_diff_db };
    let gain_db = amp_db(gain.abs());
    let verdict = if identical {
        "identical"
    } else if null_depth <= -90.0 {
        "transparent"
    } else if si >= 60.0 {
        "gain_only"
    } else if si >= 20.0 {
        "similar"
    } else {
        "different"
    };
    CompareReport {
        offset_samples: lag,
        offset_s: lag as f64 / a.sample_rate as f64,
        alignment_correlation: corr,
        polarity_inverted: gain < 0.0,
        compared_frames: len,
        length_difference_s: b.duration_s() - a.duration_s(),
        rms_diff_db,
        null_depth_db: null_depth,
        diff_peak_db: amp_db(pd),
        peak_delta_db: amp_db(b.peak() as f64) - amp_db(a.peak() as f64),
        rms_delta_db: pow_db(sb / n) - rms_a_db,
        loudness_delta_lu: if la.is_finite() && lb.is_finite() { lb - la } else { f64::NAN },
        gain_db,
        si_sdr_db: si,
        identical,
        verdict: verdict.into(),
        notes,
    }
}

// ---------------------------------------------------------------------------------
// Similarity
// ---------------------------------------------------------------------------------

/// Result of [`similarity`].
#[derive(Clone, Debug, Serialize)]
pub struct SimilarityReport {
    /// Overall similarity 0..1 (1 = indistinguishable by these measures).
    pub score: f64,
    /// Cosine similarity of the mean-removed log third-octave spectra (clamped to 0..1).
    pub spectral_similarity: f64,
    /// RMS difference of the level-matched third-octave spectra (dB).
    pub spectral_distance_db: f64,
    /// Integrated loudness of `b` minus `a` (LU).
    pub loudness_difference_lu: f64,
    /// Euclidean distance between mean MFCC vectors (c1..c12, dB scale).
    pub mfcc_distance: f64,
    pub mfcc_similarity: f64,
    /// `same` (score ≥ 0.9), `similar` (≥ 0.7), or `different`.
    pub verdict: String,
}

/// Mel-frequency cepstral coefficients per frame (`n_coeffs` including c0), computed from
/// 40 mel bands of a 46 ms STFT with 50% overlap; band energies in dB.
pub fn mfcc(buf: &AudioBuffer, n_coeffs: usize) -> Vec<Vec<f32>> {
    let sr = buf.sample_rate as f64;
    let x = buf.mono_mix();
    let n_fft = util::next_pow2((sr * 0.046) as usize).max(256);
    let hop = n_fft / 2;
    let bank = util::mel_filterbank(sr, n_fft, 40, 30.0, 16000.0f64.min(sr / 2.0));
    let frames = util::stft_mag(&x, n_fft, hop);
    let energies: Vec<f32> = frames.iter().map(|f| f.iter().map(|m| m * m).sum()).collect();
    let emax = energies.iter().copied().fold(0.0f32, f32::max);
    frames
        .iter()
        .zip(&energies)
        .filter(|(_, e)| emax > 0.0 && **e > emax * 1e-6)
        .map(|(f, _)| {
            let p: Vec<f32> = f.iter().map(|m| m * m).collect();
            let mel: Vec<f32> = util::apply_bank(&bank, &p).into_iter().map(|e| 10.0 * (e + 1e-10).log10()).collect();
            util::dct2(&mel, n_coeffs)
        })
        .collect()
}

fn mean_mfcc(buf: &AudioBuffer) -> Option<Vec<f64>> {
    let m = mfcc(buf, 13);
    if m.is_empty() {
        return None;
    }
    let mut acc = vec![0.0f64; 12];
    for f in &m {
        for (a, v) in acc.iter_mut().zip(&f[1..]) {
            *a += *v as f64;
        }
    }
    Some(acc.into_iter().map(|v| v / m.len() as f64).collect())
}

/// Spectral, cepstral, and loudness similarity between two buffers.
pub fn similarity(a: &AudioBuffer, b: &AudioBuffer) -> SimilarityReport {
    let sa = crate::spectrum::third_octave_levels(a);
    let sb = crate::spectrum::third_octave_levels(b);
    let pairs: Vec<(f64, f64)> = sa
        .iter()
        .filter_map(|x| sb.iter().find(|y| y.center_hz == x.center_hz).map(|y| (x.center_hz, x.db, y.db)))
        .filter(|(c, x, y)| *c >= 31.0 && *c <= 16500.0 && (*x > -120.0 || *y > -120.0))
        .map(|(_, x, y)| (x.max(-120.0), y.max(-120.0)))
        .collect();
    let (spec_sim, spec_dist) = if pairs.len() >= 3 {
        let xa: Vec<f64> = pairs.iter().map(|p| p.0).collect();
        let xb: Vec<f64> = pairs.iter().map(|p| p.1).collect();
        let ma = util::mean(&xa);
        let mb = util::mean(&xb);
        let (mut dot, mut na, mut nb, mut dd) = (0.0, 0.0, 0.0, 0.0);
        for (x, y) in xa.iter().zip(&xb) {
            let (p, q) = (x - ma, y - mb);
            dot += p * q;
            na += p * p;
            nb += q * q;
            dd += (p - q).powi(2);
        }
        let cos = if na > 1e-12 && nb > 1e-12 {
            dot / (na * nb).sqrt()
        } else if na <= 1e-12 && nb <= 1e-12 {
            1.0
        } else {
            0.0
        };
        (cos.clamp(0.0, 1.0), (dd / xa.len() as f64).sqrt())
    } else {
        (if pairs.is_empty() { 1.0 } else { 0.0 }, 0.0)
    };
    let la = crate::loudness::integrated_lufs(a);
    let lb = crate::loudness::integrated_lufs(b);
    let ldiff = if la.is_finite() && lb.is_finite() {
        lb - la
    } else if la.is_finite() != lb.is_finite() {
        f64::INFINITY
    } else {
        0.0
    };
    let (mdist, msim) = match (mean_mfcc(a), mean_mfcc(b)) {
        (Some(x), Some(y)) => {
            let d = x.iter().zip(&y).map(|(p, q)| (p - q).powi(2)).sum::<f64>().sqrt();
            (d, (-d / 25.0).exp())
        }
        (None, None) => (0.0, 1.0),
        _ => (f64::INFINITY, 0.0),
    };
    let lsim = if ldiff.is_finite() { (-ldiff.abs() / 6.0).exp() } else { 0.0 };
    let score = (0.45 * spec_sim + 0.4 * msim + 0.15 * lsim).clamp(0.0, 1.0);
    let verdict = if score >= 0.9 {
        "same"
    } else if score >= 0.7 {
        "similar"
    } else {
        "different"
    };
    SimilarityReport {
        score,
        spectral_similarity: spec_sim,
        spectral_distance_db: spec_dist,
        loudness_difference_lu: ldiff,
        mfcc_distance: mdist,
        mfcc_similarity: msim,
        verdict: verdict.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferraus_core::testing::{add, chirp, noise, sine, stereo};

    fn music_like() -> AudioBuffer {
        let a = add(&sine(48000, 220.0, 2.0, 0.3), &noise(48000, 2.0, 0.05, 7));
        stereo(add(&a, &chirp(48000, 300.0, 3000.0, 2.0, 0.2)))
    }

    #[test]
    fn identical_files() {
        let a = music_like();
        let r = compare(&a, &a, &CompareOptions::default());
        assert!(r.identical);
        assert_eq!(r.offset_samples, 0);
        assert_eq!(r.rms_diff_db, -240.0);
        assert!(r.si_sdr_db >= 150.0, "{}", r.si_sdr_db);
        assert_eq!(r.verdict, "identical");
    }

    #[test]
    fn delayed_and_scaled() {
        let a = music_like();
        let mut b = AudioBuffer::new(48000, 2, 100);
        b.append(&a);
        b.scale(0.5);
        let r = compare(&a, &b, &CompareOptions::default());
        assert_eq!(r.offset_samples, 100);
        assert!(r.si_sdr_db > 100.0, "{}", r.si_sdr_db);
        assert!((r.gain_db + 6.02).abs() < 0.01, "{}", r.gain_db);
        assert!(!r.polarity_inverted);
        assert_eq!(r.verdict, "gain_only");
        // b leading a
        let r = compare(&b, &a, &CompareOptions::default());
        assert_eq!(r.offset_samples, -100);
        // polarity
        let mut inv = a.clone();
        inv.scale(-1.0);
        let r = compare(&a, &inv, &CompareOptions::default());
        assert!(r.polarity_inverted);
        assert_eq!(r.offset_samples, 0);
    }

    #[test]
    fn different_signals() {
        let a = music_like();
        let b = stereo(noise(48000, 2.0, 0.3, 99));
        let r = compare(&a, &b, &CompareOptions::default());
        assert_eq!(r.verdict, "different");
        assert!(r.si_sdr_db < 0.0);
    }

    #[test]
    fn similarity_scores() {
        let a = music_like();
        let mut a2 = a.clone();
        a2.scale(0.9);
        let s = similarity(&a, &a2);
        assert!(s.score > 0.9, "{s:?}");
        assert_eq!(s.verdict, "same");
        let b = stereo(sine(48000, 5000.0, 2.0, 0.1));
        let s = similarity(&a, &b);
        assert!(s.score < 0.6, "{s:?}");
        assert_eq!(s.verdict, "different");
    }
}
