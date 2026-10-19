//! Tempo and beat tracking.
//!
//! 1. Onset strength: spectral flux on a log-mel spectrogram (10 ms hop), local mean
//!    removed, normalised.
//! 2. Tempo: autocorrelation of the onset envelope scored on a comb of metrical levels
//!    (τ, 2τ, 3τ) and weighted by a log-normal perceptual prior around 120 BPM.
//! 3. Beats: dynamic-programming beat tracking (Ellis 2007), then the BPM is refined by a
//!    least-squares fit through the beat times.
//! 4. Meter and downbeats: per-beat accents (low-frequency onset energy and overall onset
//!    strength) are folded at 3 and 4 beats per bar; subdivision energy at thirds versus
//!    halves of the beat separates 6/8 from 4/4.

use crate::util::{self, mel_filterbank, next_pow2};
use ferraus_core::AudioBuffer;
use serde::Serialize;

/// Options for [`detect_tempo`].
#[derive(Clone, Debug)]
pub struct TempoOptions {
    /// Lowest tempo considered (BPM).
    pub min_bpm: f64,
    /// Highest tempo considered (BPM).
    pub max_bpm: f64,
    /// Centre of the perceptual tempo prior (BPM).
    pub prior_bpm: f64,
    /// Width of the prior in octaves.
    pub prior_octaves: f64,
    /// How strongly beats keep to the tempo (Ellis' α).
    pub tightness: f64,
}

impl Default for TempoOptions {
    fn default() -> Self {
        TempoOptions {
            min_bpm: 60.0,
            max_bpm: 200.0,
            prior_bpm: 120.0,
            prior_octaves: 1.0,
            tightness: 100.0,
        }
    }
}

/// A tempo candidate and its normalised score.
#[derive(Clone, Debug, Serialize)]
pub struct TempoCandidate {
    pub bpm: f64,
    pub score: f64,
}

/// Result of [`detect_tempo`].
#[derive(Clone, Debug, Serialize)]
pub struct TempoResult {
    /// Tempo in beats per minute.
    pub bpm: f64,
    /// 0..1: how clearly beats line up with onsets and how regular they are.
    pub confidence: f64,
    /// Beat times in seconds.
    pub beats: Vec<f64>,
    /// Downbeat (bar start) times in seconds; empty when the bar phase is not confident.
    pub downbeats: Vec<f64>,
    /// 0..1 confidence of the downbeat phase.
    pub downbeat_confidence: f64,
    /// `4/4`, `3/4`, or `6/8` (best guess).
    pub time_signature: String,
    /// 0..1 confidence of the time signature guess.
    pub time_signature_confidence: f64,
    /// Other plausible tempi (metrical levels), best first.
    pub alternatives: Vec<TempoCandidate>,
}

impl TempoResult {
    fn empty() -> TempoResult {
        TempoResult {
            bpm: 0.0,
            confidence: 0.0,
            beats: Vec::new(),
            downbeats: Vec::new(),
            downbeat_confidence: 0.0,
            time_signature: "4/4".into(),
            time_signature_confidence: 0.0,
            alternatives: Vec::new(),
        }
    }
}

/// Onset strength envelope and a low-frequency onset band, both at `frame_rate` Hz.
/// Frame `i` is centred at time `i / frame_rate` seconds.
#[derive(Clone, Debug)]
pub struct OnsetEnvelope {
    pub frame_rate: f64,
    /// Normalised onset strength (unit standard deviation, local mean removed, ≥ 0).
    pub values: Vec<f32>,
    /// Onset strength of the bands below ~200 Hz (kick-drum accents), same normalisation.
    pub low: Vec<f32>,
    /// Level of the bands below ~200 Hz per frame in dB (for accent and downbeat cues).
    pub low_level_db: Vec<f32>,
}

/// Compute the onset strength envelope of a buffer (mono mix).
pub fn onset_envelope(buf: &AudioBuffer) -> OnsetEnvelope {
    let sr = buf.sample_rate as f64;
    let x = buf.mono_mix();
    let hop = ((sr / 100.0).round() as usize).max(1);
    let frame_rate = sr / hop as f64;
    if x.len() < hop * 4 {
        return OnsetEnvelope {
            frame_rate,
            values: vec![0.0; x.len() / hop + 1],
            low: vec![0.0; x.len() / hop + 1],
            low_level_db: vec![-80.0; x.len() / hop + 1],
        };
    }
    let n_fft = next_pow2((sr * 0.023) as usize).max(256);
    let n_mels = 40;
    let bank = mel_filterbank(sr, n_fft, n_mels, 30.0, 8000.0_f64.min(sr / 2.0));
    let low_bands = (0..n_mels)
        .filter(|m| {
            let c = bank[*m]
                .iter()
                .map(|(k, _)| *k)
                .max()
                .unwrap_or(0) as f64
                * sr
                / n_fft as f64;
            c < 200.0
        })
        .count()
        .max(1);
    let frames = util::stft_mag(&x, n_fft, hop);
    // log-mel in dB, floored 80 dB below the loudest value
    let mut mel: Vec<Vec<f32>> = frames
        .iter()
        .map(|f| {
            let p: Vec<f32> = f.iter().map(|m| m * m).collect();
            util::apply_bank(&bank, &p)
                .into_iter()
                .map(|e| 10.0 * (e.max(1e-12)).log10())
                .collect()
        })
        .collect();
    let top = mel
        .iter()
        .flat_map(|f| f.iter())
        .fold(f32::NEG_INFINITY, |m, v| m.max(*v));
    for f in mel.iter_mut() {
        for v in f.iter_mut() {
            *v = v.max(top - 80.0);
        }
    }
    let n = mel.len();
    let low_level_db: Vec<f32> = mel
        .iter()
        .map(|f| {
            let e: f32 = f[..low_bands].iter().map(|db| 10f32.powf(db / 10.0)).sum();
            10.0 * e.max(1e-12).log10() - top
        })
        .collect();
    let mut flux = vec![0.0f32; n];
    let mut low = vec![0.0f32; n];
    for t in 1..n {
        let mut s = 0.0;
        let mut sl = 0.0;
        for m in 0..n_mels {
            let d = (mel[t][m] - mel[t - 1][m]).max(0.0);
            s += d;
            if m < low_bands {
                sl += d;
            }
        }
        flux[t] = s / n_mels as f32;
        low[t] = sl / low_bands as f32;
    }
    let values = normalise_onset(&flux, frame_rate);
    let low = normalise_onset(&low, frame_rate);
    OnsetEnvelope {
        frame_rate,
        values,
        low,
        low_level_db,
    }
}

/// Subtract a 1 s moving average, half-wave rectify, and scale to unit std.
fn normalise_onset(x: &[f32], frame_rate: f64) -> Vec<f32> {
    let n = x.len();
    let w = (frame_rate * 0.5).round() as usize;
    let mut cs = vec![0.0f64; n + 1];
    for i in 0..n {
        cs[i + 1] = cs[i] + x[i] as f64;
    }
    let mut out: Vec<f32> = (0..n)
        .map(|i| {
            let a = i.saturating_sub(w);
            let b = (i + w + 1).min(n);
            let m = (cs[b] - cs[a]) / (b - a) as f64;
            (x[i] as f64 - m).max(0.0) as f32
        })
        .collect();
    let v: Vec<f64> = out.iter().map(|v| *v as f64).collect();
    let sd = util::std_dev(&v);
    if sd > 1e-9 {
        for o in out.iter_mut() {
            *o = (*o as f64 / sd) as f32;
        }
    }
    out
}

/// Normalised autocorrelation of the onset envelope for lags `0..=max_lag`.
fn autocorr(x: &[f32], max_lag: usize) -> Vec<f64> {
    let n = x.len();
    let m = x.iter().map(|v| *v as f64).sum::<f64>() / n.max(1) as f64;
    let y: Vec<f64> = x.iter().map(|v| *v as f64 - m).collect();
    let mut ac = vec![0.0f64; max_lag + 1];
    for (lag, a) in ac.iter_mut().enumerate() {
        if lag >= n {
            break;
        }
        let s: f64 = y[..n - lag].iter().zip(&y[lag..]).map(|(a, b)| a * b).sum();
        *a = s / (n - lag) as f64;
    }
    let a0 = ac[0];
    if a0 > 0.0 {
        for a in ac.iter_mut() {
            *a /= a0;
        }
    }
    ac
}

fn interp(v: &[f64], pos: f64) -> f64 {
    if pos < 0.0 {
        return 0.0;
    }
    let i = pos.floor() as usize;
    if i + 1 >= v.len() {
        return 0.0;
    }
    let f = pos - i as f64;
    v[i] * (1.0 - f) + v[i + 1] * f
}

/// Detect tempo, beats, meter, and downbeats.
pub fn detect_tempo(buf: &AudioBuffer, opts: &TempoOptions) -> TempoResult {
    let env = onset_envelope(buf);
    detect_tempo_from_onsets(&env, opts)
}

/// Tempo analysis from a precomputed onset envelope.
pub fn detect_tempo_from_onsets(env: &OnsetEnvelope, opts: &TempoOptions) -> TempoResult {
    let fr = env.frame_rate;
    let o = &env.values;
    let n = o.len();
    let (min_bpm, max_bpm) = (
        opts.min_bpm.max(20.0).min(opts.max_bpm),
        opts.max_bpm.max(opts.min_bpm),
    );
    if n < (fr * 1.5) as usize || o.iter().all(|v| *v <= 0.0) {
        return TempoResult::empty();
    }
    // lags up to three bar-level periods of the slowest tempo considered by the comb
    let max_lag = ((60.0 * fr / (min_bpm / 3.0)).ceil() as usize).min(n - 1);
    let ac = autocorr(o, max_lag);
    let prior = |bpm: f64| -> f64 {
        let z = (bpm / opts.prior_bpm).log2() / opts.prior_octaves;
        (-0.5 * z * z).exp()
    };
    let tps = |bpm: f64| -> f64 {
        let lag = 60.0 * fr / bpm;
        // take the best of the interpolated value and its neighbours (period jitter)
        let v = interp(&ac, lag)
            .max(interp(&ac, lag - 0.5))
            .max(interp(&ac, lag + 0.5));
        prior(bpm) * v.max(0.0)
    };
    let score = |bpm: f64| -> f64 { tps(bpm) + 0.5 * tps(bpm / 2.0) + 0.25 * tps(bpm / 3.0) };
    let step = 0.25;
    let mut grid = Vec::new();
    let mut b = min_bpm;
    while b <= max_bpm + 1e-9 {
        grid.push((b, score(b)));
        b += step;
    }
    let best_score = grid.iter().map(|g| g.1).fold(0.0, f64::max);
    if best_score <= 0.0 {
        return TempoResult::empty();
    }
    // local maxima as candidates
    let mut peaks: Vec<(f64, f64)> = (0..grid.len())
        .filter(|&i| {
            let l = if i > 0 { grid[i - 1].1 } else { f64::NEG_INFINITY };
            let r = if i + 1 < grid.len() { grid[i + 1].1 } else { f64::NEG_INFINITY };
            grid[i].1 >= l && grid[i].1 > r
        })
        .map(|i| grid[i])
        .collect();
    peaks.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let coarse_bpm = peaks.first().map(|p| p.0).unwrap_or(120.0);

    let period = 60.0 * fr / coarse_bpm;
    let beats_f = track_beats(o, period, opts.tightness);
    if beats_f.len() < 2 {
        return TempoResult::empty();
    }
    // timing offset: the flux peak sits slightly before the perceptual onset
    let bias = 0.0;
    let beats: Vec<f64> = beats_f.iter().map(|b| *b as f64 / fr + bias).collect();

    // refine tempo by least squares through beat times
    let bpm = refine_bpm(&beats).unwrap_or(coarse_bpm);

    // confidence: onset salience at beats, periodicity, and interval regularity
    let lm: Vec<f64> = (0..n).map(|i| local_max(o, i, 1) as f64).collect();
    let mean_all = util::mean(&lm);
    let at_beats = beats_f.iter().map(|&b| lm[b]).sum::<f64>() / beats_f.len() as f64;
    let salience = if mean_all > 0.0 {
        ((at_beats / mean_all - 1.0) / 2.0).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let lag = 60.0 * fr / bpm;
    let periodicity = (interp(&ac, lag)
        .max(interp(&ac, lag - 0.5))
        .max(interp(&ac, lag + 0.5))
        / 0.5)
        .clamp(0.0, 1.0);
    let ibis: Vec<f64> = beats.windows(2).map(|w| w[1] - w[0]).collect();
    let mean_ibi = util::mean(&ibis);
    let regularity = if mean_ibi > 0.0 {
        (-(util::std_dev(&ibis) / mean_ibi) / 0.1).exp()
    } else {
        0.0
    };
    let confidence = ((salience * periodicity).sqrt() * regularity).clamp(0.0, 1.0);

    let alternatives: Vec<TempoCandidate> = peaks
        .iter()
        .filter(|p| (p.0 - coarse_bpm).abs() > 3.0)
        .take(3)
        .map(|p| TempoCandidate {
            bpm: p.0,
            score: p.1 / best_score,
        })
        .collect();

    let meter = analyse_meter(env, &beats_f, 60.0 * fr / bpm);
    let downbeats = if meter.downbeat_confidence >= 0.35 && confidence > 0.3 {
        beats
            .iter()
            .enumerate()
            .filter(|(i, _)| i % meter.beats_per_bar == meter.phase)
            .map(|(_, t)| *t)
            .collect()
    } else {
        Vec::new()
    };
    TempoResult {
        bpm,
        confidence,
        beats,
        downbeats,
        downbeat_confidence: meter.downbeat_confidence,
        time_signature: meter.signature,
        time_signature_confidence: meter.confidence,
        alternatives,
    }
}

fn local_max(x: &[f32], i: usize, r: usize) -> f32 {
    let a = i.saturating_sub(r);
    let b = (i + r + 1).min(x.len());
    x[a..b].iter().fold(0.0f32, |m, v| m.max(*v))
}

/// Least-squares tempo through beat times (robust to a few missing beats by using the
/// median interval to index beats).
fn refine_bpm(beats: &[f64]) -> Option<f64> {
    if beats.len() < 4 {
        return None;
    }
    let ibis: Vec<f64> = beats.windows(2).map(|w| w[1] - w[0]).collect();
    let med = util::median(&ibis);
    if med <= 0.0 {
        return None;
    }
    // beat indices accounting for gaps
    let mut idx = vec![0.0f64];
    for w in ibis.iter() {
        let k = (w / med).round().max(1.0);
        idx.push(idx.last().unwrap() + k);
    }
    let n = beats.len() as f64;
    let mx = idx.iter().sum::<f64>() / n;
    let my = beats.iter().sum::<f64>() / n;
    let (mut sxy, mut sxx) = (0.0, 0.0);
    for (x, y) in idx.iter().zip(beats) {
        sxy += (x - mx) * (y - my);
        sxx += (x - mx) * (x - mx);
    }
    if sxx <= 0.0 {
        return None;
    }
    let slope = sxy / sxx;
    if slope <= 0.0 {
        None
    } else {
        Some(60.0 / slope)
    }
}

/// Ellis (2007) dynamic-programming beat tracker. Returns beat frame indices.
fn track_beats(onset: &[f32], period: f64, tightness: f64) -> Vec<usize> {
    let n = onset.len();
    if period < 1.0 || n < 2 {
        return Vec::new();
    }
    // local score: onset smoothed by a Gaussian of std period/32
    let sigma = (period / 32.0).max(0.5);
    let half = (sigma * 4.0).ceil() as isize;
    let kernel: Vec<f64> = (-half..=half)
        .map(|k| (-0.5 * (k as f64 / sigma).powi(2)).exp())
        .collect();
    let local: Vec<f64> = (0..n as isize)
        .map(|i| {
            let mut s = 0.0;
            for (j, w) in kernel.iter().enumerate() {
                let idx = i + j as isize - half;
                if idx >= 0 && (idx as usize) < n {
                    s += onset[idx as usize] as f64 * w;
                }
            }
            s
        })
        .collect();
    let lo = (period / 2.0).round() as usize;
    let hi = (2.0 * period).round() as usize;
    let mut cum = vec![0.0f64; n];
    let mut back = vec![usize::MAX; n];
    // precompute transition costs for each distance
    let cost: Vec<f64> = (0..=hi)
        .map(|d| {
            if d == 0 {
                f64::NEG_INFINITY
            } else {
                -tightness * ((d as f64 / period).ln()).powi(2)
            }
        })
        .collect();
    for t in 0..n {
        let mut best = f64::NEG_INFINITY;
        let mut arg = usize::MAX;
        if t >= lo {
            let start = t.saturating_sub(hi);
            for tau in start..=t - lo.max(1) {
                let v = cum[tau] + cost[t - tau];
                if v > best {
                    best = v;
                    arg = tau;
                }
            }
        }
        // chain onto the best predecessor when that beats starting fresh
        if arg != usize::MAX && best > 0.0 {
            cum[t] = local[t] + best;
            back[t] = arg;
        } else {
            cum[t] = local[t];
        }
    }
    // last beat: the latest local maximum of the cumulative score above half the median
    let maxima: Vec<usize> = (1..n.saturating_sub(1))
        .filter(|&i| cum[i] > cum[i - 1] && cum[i] >= cum[i + 1])
        .collect();
    if maxima.is_empty() {
        return Vec::new();
    }
    let vals: Vec<f64> = maxima.iter().map(|&i| cum[i]).collect();
    let med = util::median(&vals);
    // search only the final two periods for the end point
    let tail_start = n.saturating_sub((2.0 * period) as usize + 1);
    let mut last = *maxima
        .iter()
        .rev()
        .find(|&&i| cum[i] >= 0.5 * med)
        .unwrap_or(maxima.last().unwrap());
    let mut best_tail = f64::NEG_INFINITY;
    for &i in maxima.iter().filter(|&&i| i >= tail_start) {
        if cum[i] > best_tail {
            best_tail = cum[i];
            last = i;
        }
    }
    let mut beats = vec![last];
    let mut cur = last;
    while back[cur] != usize::MAX {
        cur = back[cur];
        beats.push(cur);
    }
    beats.reverse();
    trim_weak_beats(&local, beats)
}

/// Drop leading and trailing beats whose local onset score is far below typical.
fn trim_weak_beats(local: &[f64], mut beats: Vec<usize>) -> Vec<usize> {
    if beats.len() < 3 {
        return beats;
    }
    let vals: Vec<f64> = beats.iter().map(|&b| local[b]).collect();
    let rms = (vals.iter().map(|v| v * v).sum::<f64>() / vals.len() as f64).sqrt();
    let thr = 0.5 * rms;
    while beats.len() > 2 && local[beats[0]] < thr {
        beats.remove(0);
    }
    while beats.len() > 2 && local[*beats.last().unwrap()] < thr {
        beats.pop();
    }
    beats
}

struct Meter {
    signature: String,
    beats_per_bar: usize,
    phase: usize,
    confidence: f64,
    downbeat_confidence: f64,
}

fn analyse_meter(env: &OnsetEnvelope, beats: &[usize], period: f64) -> Meter {
    let default = Meter {
        signature: "4/4".into(),
        beats_per_bar: 4,
        phase: 0,
        confidence: 0.0,
        downbeat_confidence: 0.0,
    };
    if beats.len() < 8 {
        return default;
    }
    let r = ((period / 8.0).round() as usize).max(1);
    // accents: low-band onset dominates (kick drums), plus overall onset
    let zscore = |v: Vec<f64>| -> Vec<f64> {
        let m = util::mean(&v);
        let sd = util::std_dev(&v).max(1e-9);
        v.into_iter().map(|x| (x - m) / sd).collect()
    };
    let level = zscore(
        beats
            .iter()
            .map(|&b| {
                let a = b.saturating_sub(1);
                let e = (b + r + 1).min(env.low_level_db.len());
                env.low_level_db[a..e].iter().fold(-200.0f32, |m, v| m.max(*v)) as f64
            })
            .collect(),
    );
    let strength = zscore(beats.iter().map(|&b| local_max(&env.values, b, r) as f64).collect());
    let acc: Vec<f64> = level
        .iter()
        .zip(&strength)
        .map(|(l, s)| 0.7 * l + 0.3 * s)
        .collect();
    let sd = util::std_dev(&acc).max(1e-9);
    let fold = |m: usize| -> Vec<f64> {
        (0..m)
            .map(|phi| {
                let (mut on, mut non, mut off, mut noff) = (0.0, 0usize, 0.0, 0usize);
                for (i, a) in acc.iter().enumerate() {
                    if i % m == phi {
                        on += a;
                        non += 1;
                    } else {
                        off += a;
                        noff += 1;
                    }
                }
                if non == 0 || noff == 0 {
                    0.0
                } else {
                    (on / non as f64 - off / noff as f64) / sd
                }
            })
            .collect()
    };
    let c3 = fold(3);
    let c4 = fold(4);
    let best = |c: &[f64]| -> (usize, f64, f64) {
        let mut idx: Vec<usize> = (0..c.len()).collect();
        idx.sort_by(|a, b| c[*b].partial_cmp(&c[*a]).unwrap_or(std::cmp::Ordering::Equal));
        (idx[0], c[idx[0]], c[idx[1]])
    };
    let (p3, s3, s3b) = best(&c3);
    let (p4, s4, s4b) = best(&c4);
    // subdivision: thirds versus halves of the beat
    let o = &env.values;
    let (mut tern, mut bin, mut cnt) = (0.0f64, 0.0f64, 0usize);
    for w in beats.windows(2) {
        let (a, b) = (w[0] as f64, w[1] as f64);
        let d = b - a;
        if d < 3.0 {
            continue;
        }
        let at = |pos: f64| local_max(o, pos.round() as usize, 1) as f64;
        tern += 0.5 * (at(a + d / 3.0) + at(a + 2.0 * d / 3.0));
        bin += at(a + d / 2.0);
        cnt += 1;
    }
    let ternary = cnt > 0 && tern > 1.5 * bin && tern / cnt as f64 > 0.3;
    let (m, phase, s, sb, other) = if s3 > s4 * 1.15 {
        (3usize, p3, s3, s3b, s4)
    } else {
        (4usize, p4, s4, s4b, s3)
    };
    let confidence = if s > 0.0 {
        ((s - other.max(0.0)) / s).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let downbeat_confidence = if s > 0.0 {
        ((s - sb.max(0.0)) / s).clamp(0.0, 1.0) * (s / 1.5).min(1.0)
    } else {
        0.0
    };
    let signature = if ternary && m == 4 {
        "6/8".to_string()
    } else if m == 3 {
        "3/4".to_string()
    } else {
        "4/4".to_string()
    };
    Meter {
        signature,
        beats_per_bar: m,
        phase,
        confidence,
        downbeat_confidence,
    }
}

/// Onsets per second: peaks of the onset envelope above an adaptive threshold.
pub fn transient_density(buf: &AudioBuffer) -> f64 {
    let env = onset_envelope(buf);
    let o = &env.values;
    if o.len() < 3 || buf.duration_s() <= 0.0 {
        return 0.0;
    }
    let mut count = 0usize;
    let min_gap = (env.frame_rate * 0.05) as usize;
    let mut last: Option<usize> = None;
    for i in 1..o.len() - 1 {
        if o[i] > 1.5 && o[i] >= o[i - 1] && o[i] > o[i + 1] {
            if let Some(l) = last {
                if i - l < min_gap {
                    continue;
                }
            }
            count += 1;
            last = Some(i);
        }
    }
    count as f64 / buf.duration_s()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use ferraus_core::testing;
    use std::f64::consts::PI;

    /// Simple deterministic RNG for fixtures.
    pub(crate) struct Rng(u64);
    impl Rng {
        pub(crate) fn new(seed: u64) -> Rng {
            Rng(seed.wrapping_mul(0x9E3779B97F4A7C15) | 1)
        }
        pub(crate) fn next(&mut self) -> f64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 11) as f64 / (1u64 << 53) as f64
        }
        pub(crate) fn sym(&mut self) -> f64 {
            self.next() * 2.0 - 1.0
        }
    }

    fn add_kick(x: &mut [f32], sr: f64, t: f64, amp: f64) {
        let s = (t * sr) as usize;
        let len = (0.25 * sr) as usize;
        let mut ph = 0.0;
        for i in 0..len {
            if s + i >= x.len() {
                break;
            }
            let tt = i as f64 / sr;
            let f = 50.0 + 100.0 * (-tt / 0.03).exp();
            ph += 2.0 * PI * f / sr;
            x[s + i] += (amp * (-tt / 0.12).exp() * ph.sin()) as f32;
        }
    }

    fn add_noise_hit(x: &mut [f32], sr: f64, t: f64, amp: f64, decay: f64, hp: bool, rng: &mut Rng) {
        let s = (t * sr) as usize;
        let len = (decay * 6.0 * sr) as usize;
        let mut prev = 0.0;
        for i in 0..len {
            if s + i >= x.len() {
                break;
            }
            let tt = i as f64 / sr;
            let n = rng.sym();
            let v = if hp { n - prev } else { n };
            prev = n;
            x[s + i] += (amp * (-tt / decay).exp() * v) as f32;
        }
    }

    /// A drum pattern: kick on 1 and 3 (accented on 1), snare on 2 and 4, swung hi-hat
    /// eighths, timing jitter, and background noise. Returns the buffer and true beats.
    pub(crate) fn drum_track(bpm: f64, secs: f64, swing: f64, jitter_s: f64, seed: u64) -> (AudioBuffer, Vec<f64>) {
        let sr = 22050.0;
        let n = (secs * sr) as usize;
        let mut x = vec![0.0f32; n];
        let mut rng = Rng::new(seed);
        let beat = 60.0 / bpm;
        let start = 0.25;
        let mut truth = Vec::new();
        let mut i = 0usize;
        loop {
            let t0 = start + i as f64 * beat;
            if t0 > secs - 0.3 {
                break;
            }
            truth.push(t0);
            let j = |r: &mut Rng| r.sym() * jitter_s;
            match i % 4 {
                0 => add_kick(&mut x, sr, t0 + j(&mut rng), 0.9),
                2 => add_kick(&mut x, sr, t0 + j(&mut rng), 0.6),
                _ => add_noise_hit(&mut x, sr, t0 + j(&mut rng), 0.5, 0.05, false, &mut rng),
            }
            add_noise_hit(&mut x, sr, t0 + j(&mut rng), 0.15, 0.015, true, &mut rng);
            let off = t0 + beat * (0.5 + swing) + j(&mut rng);
            add_noise_hit(&mut x, sr, off, 0.1, 0.015, true, &mut rng);
            i += 1;
        }
        for v in x.iter_mut() {
            *v += (rng.sym() * 0.02) as f32;
        }
        (AudioBuffer::from_mono(sr as u32, x), truth)
    }

    /// Fraction of reference beats with a detected beat within `tol` seconds, allowing
    /// the detection to be at half or double the reference metrical level.
    pub(crate) fn beat_hits(truth: &[f64], det: &[f64], tol: f64) -> f64 {
        let score = |r: &[f64], d: &[f64]| -> f64 {
            let hits = r
                .iter()
                .filter(|t| d.iter().any(|b| (*b - **t).abs() <= tol))
                .count();
            hits as f64 / r.len().max(1) as f64
        };
        let straight = score(truth, det);
        let half_a: Vec<f64> = truth.iter().step_by(2).copied().collect();
        let half_b: Vec<f64> = truth.iter().skip(1).step_by(2).copied().collect();
        let dbl_a: Vec<f64> = det.iter().step_by(2).copied().collect();
        let dbl_b: Vec<f64> = det.iter().skip(1).step_by(2).copied().collect();
        let rd = det.len() as f64 / truth.len().max(1) as f64;
        if rd < 0.7 {
            score(&half_a, det).max(score(&half_b, det))
        } else if rd > 1.4 {
            straight.max(score(truth, &dbl_a)).max(score(truth, &dbl_b))
        } else {
            straight
        }
    }

    fn tempo_ok(want: f64, got: f64) -> bool {
        [1.0, 2.0, 0.5, 1.5, 2.0 / 3.0, 3.0, 1.0 / 3.0]
            .iter()
            .any(|m| (got - want * m).abs() <= 2.0)
    }

    #[test]
    fn tempo_on_synthetic_drum_tracks() {
        let tempi = [85.0, 100.0, 120.0, 128.0, 140.0, 174.0, 92.0, 110.0, 160.0, 70.0];
        let mut ok = 0;
        let mut exact = 0;
        let mut total_hits = 0.0;
        let mut total_beats = 0.0;
        for (k, bpm) in tempi.iter().enumerate() {
            let swing = if k % 3 == 1 { 0.12 } else { 0.0 };
            let (buf, truth) = drum_track(*bpm, 10.0, swing, 0.008, k as u64 + 1);
            let r = detect_tempo(&buf, &TempoOptions::default());
            let good = tempo_ok(*bpm, r.bpm);
            if good {
                ok += 1;
            }
            if (r.bpm - bpm).abs() <= 2.0 {
                exact += 1;
            }
            let h = beat_hits(&truth, &r.beats, 0.07);
            eprintln!(
                "bpm {bpm}: got {:.2} conf {:.2} beats {} hits {:.2} sig {} db {:?}",
                r.bpm,
                r.confidence,
                r.beats.len(),
                h,
                r.time_signature,
                r.downbeats.first()
            );
            total_hits += h * truth.len() as f64;
            total_beats += truth.len() as f64;
        }
        assert!(ok as f64 >= 0.9 * tempi.len() as f64, "tempo ok {ok}/{}", tempi.len());
        assert!(exact >= 7, "exact tempo {exact}/{}", tempi.len());
        assert!(total_hits / total_beats >= 0.85, "beat accuracy {:.3}", total_hits / total_beats);
    }

    #[test]
    fn click_track_and_range() {
        let b = testing::clicks(22050, 0.5, 8.0, 0.9);
        let r = detect_tempo(&b, &TempoOptions::default());
        assert!((r.bpm - 120.0).abs() < 1.0, "{}", r.bpm);
        assert!(r.confidence > 0.5, "{}", r.confidence);
        // restricting the range forces the double-time level
        let r = detect_tempo(
            &b,
            &TempoOptions {
                min_bpm: 180.0,
                max_bpm: 260.0,
                ..Default::default()
            },
        );
        assert!((r.bpm - 240.0).abs() < 2.0 || (r.bpm - 120.0).abs() < 1.0, "{}", r.bpm);
        // other sample rates
        let b = testing::add(
            &testing::clicks(48000, 0.6, 9.0, 0.8),
            &testing::noise(48000, 9.0, 0.01, 5),
        );
        let r = detect_tempo(&b, &TempoOptions::default());
        assert!((r.bpm - 100.0).abs() < 1.0, "{}", r.bpm);
        assert!(r.beats.iter().any(|b| (b - 1.2).abs() < 0.03), "{:?}", &r.beats[..3]);
    }

    #[test]
    fn silence_and_noise() {
        let s = AudioBuffer::new(22050, 1, 22050 * 3);
        let r = detect_tempo(&s, &TempoOptions::default());
        assert_eq!(r.bpm, 0.0);
        assert!(r.beats.is_empty());
        let short = testing::sine(22050, 440.0, 0.2, 0.5);
        let r = detect_tempo(&short, &TempoOptions::default());
        assert!(r.beats.is_empty());
        let nz = testing::noise(22050, 5.0, 0.3, 3);
        let r = detect_tempo(&nz, &TempoOptions::default());
        assert!(r.confidence < 0.5, "noise conf {}", r.confidence);
    }

    #[test]
    fn three_four_accent() {
        // accented first beat of three
        let sr = 22050.0;
        let bpm = 120.0;
        let secs = 12.0;
        let mut x = vec![0.0f32; (secs * sr) as usize];
        let mut rng = Rng::new(9);
        let mut truth_db = Vec::new();
        for i in 0..23 {
            let t = 0.3 + i as f64 * 60.0 / bpm;
            if i % 3 == 0 {
                add_kick(&mut x, sr, t, 0.9);
                truth_db.push(t);
            }
            add_noise_hit(&mut x, sr, t, 0.3, 0.03, true, &mut rng);
        }
        let r = detect_tempo(&AudioBuffer::from_mono(sr as u32, x), &TempoOptions::default());
        assert_eq!(r.time_signature, "3/4", "{r:?}");
        assert!(!r.downbeats.is_empty());
        assert!(beat_hits(&truth_db, &r.downbeats, 0.07) > 0.8);
    }

    #[test]
    fn density_counts_onsets() {
        let b = testing::clicks(22050, 0.25, 6.0, 0.9);
        let d = transient_density(&b);
        assert!((d - 4.0).abs() < 0.6, "{d}");
    }
}
