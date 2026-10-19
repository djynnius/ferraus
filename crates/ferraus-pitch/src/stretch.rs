//! Hybrid time stretching: phase vocoder for tonal material, WSOLA for transients and
//! percussive material, chosen per segment by a transient detector.
//!
//! 1. Transients are detected with a log-spectral-flux onset function (~23 ms frames,
//!    ~3 ms hop, adaptive threshold) and refined to the sample where short-term energy
//!    jumps (a click lands on its exact sample).
//! 2. A [`TimeMap`] anchors every transient: a slope-1 window around it is copied verbatim
//!    and the onset lands exactly at `factor × onset`; the material between transients is
//!    stretched uniformly to fit.
//! 3. In `auto` mode each segment between transients is classified by spectral flatness:
//!    tonal segments are rendered by the phase vocoder, noisy/percussive segments and the
//!    protected transient windows by WSOLA, with 10 ms cross-fades between them.
//!
//! Modes: `tonal` = phase vocoder everywhere (no anchoring), `percussive` = WSOLA with
//! transient anchoring, `speech` = the `auto` hybrid with shorter windows. `fast` quality
//! uses WSOLA everywhere (anchored), or the short vocoder for `tonal`.

use crate::timemap::TimeMap;
use crate::util::{hann, Fft};
use crate::vocoder::{self, PvParams};
use crate::wsola::{self, WsolaParams};
use serde::Serialize;

/// How `stretch` chooses its engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Auto,
    Tonal,
    Percussive,
    Speech,
}

impl Mode {
    pub fn parse(s: &str) -> Mode {
        match s {
            "tonal" => Mode::Tonal,
            "percussive" => Mode::Percussive,
            "speech" => Mode::Speech,
            _ => Mode::Auto,
        }
    }
}

/// `high` or `fast`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Quality {
    High,
    Fast,
}

/// What the stretcher did (for metrics).
#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub factor: f64,
    pub transients: usize,
    pub anchored: usize,
    pub tonal_segments: usize,
    pub percussive_segments: usize,
    pub engine: &'static str,
}

/// Detected onsets (sample positions) plus per-frame flatness and RMS from the same STFT.
pub struct OnsetAnalysis {
    pub onsets: Vec<usize>,
    pub hop: usize,
    pub flatness: Vec<f64>,
    pub rms: Vec<f64>,
    pub flux: Vec<f64>,
}

/// Transient detector: log spectral flux with an adaptive threshold, refined to samples.
pub fn analyze_onsets(x: &[f32], sr: u32) -> OnsetAnalysis {
    let n = ((0.023 * sr as f64) as usize).next_power_of_two().max(64);
    let hop = (n / 8).max(1);
    let half = n / 2;
    let win = hann(n);
    let mut fft = Fft::new(n);
    let nb = n / 2 + 1;
    let frames = x.len() / hop + 1;
    let mut prev = vec![0.0f64; nb];
    let mut flux = Vec::with_capacity(frames);
    let mut flat = Vec::with_capacity(frames);
    let mut rms = Vec::with_capacity(frames);
    for t in 0..frames {
        let c = (t * hop) as isize;
        let mut e = 0.0f64;
        for i in 0..n {
            let idx = c - half as isize + i as isize;
            let v = if idx >= 0 && (idx as usize) < x.len() {
                x[idx as usize]
            } else {
                0.0
            };
            e += (v as f64) * (v as f64);
            fft.time[i] = v * win[i];
        }
        rms.push((e / n as f64).sqrt());
        fft.forward();
        let mut d = 0.0;
        let (mut lsum, mut psum) = (0.0f64, 0.0f64);
        for k in 0..nb {
            let m = fft.freq[k].norm() as f64;
            let l = (1.0 + m).ln();
            d += (l - prev[k]).max(0.0); // frame -1 is silence
            prev[k] = l;
            let pw = m * m + 1e-12;
            lsum += pw.ln();
            psum += pw;
        }
        flux.push(d / nb as f64);
        let gm = (lsum / nb as f64).exp();
        let am = psum / nb as f64;
        flat.push(if am > 1e-10 { (gm / am).min(1.0) } else { 1.0 });
    }
    // adaptive threshold + peak picking
    let w = ((0.05 * sr as f64) / hop as f64).round().max(1.0) as usize;
    let pk = ((0.01 * sr as f64) / hop as f64).round().max(1.0) as usize;
    let min_gap = (0.03 * sr as f64) as usize;
    let mut onsets: Vec<usize> = Vec::new();
    for t in 0..flux.len() {
        let d = flux[t];
        if d < 0.02 {
            continue;
        }
        let lo = t.saturating_sub(w);
        let hi = (t + w + 1).min(flux.len());
        let mean = flux[lo..hi].iter().sum::<f64>() / (hi - lo) as f64;
        if d < 1.5 * mean + 0.01 {
            continue;
        }
        let a = t.saturating_sub(pk);
        let b = (t + pk + 1).min(flux.len());
        if flux[a..b].iter().any(|&v| v > d) || flux[a..t].contains(&d) {
            continue;
        }
        // refine: sample where short energy after jumps most over energy before
        let centre = t * hop;
        let s0 = centre.saturating_sub(half + hop);
        let s1 = (centre + half).min(x.len());
        if let Some(p) = refine_onset(x, s0, s1, sr) {
            if onsets.last().map(|&l| p >= l + min_gap).unwrap_or(true) {
                onsets.push(p);
            }
        }
    }
    OnsetAnalysis {
        onsets,
        hop,
        flatness: flat,
        rms,
        flux,
    }
}

fn refine_onset(x: &[f32], s0: usize, s1: usize, sr: u32) -> Option<usize> {
    if s1 <= s0 + 2 {
        return None;
    }
    let after = ((0.0007 * sr as f64) as usize).max(4);
    let before = ((0.003 * sr as f64) as usize).max(8);
    let mut pre = vec![0.0f64; x.len() + 1];
    // prefix sums only over the needed region (+ margins)
    let a = s0.saturating_sub(before);
    let b = (s1 + after).min(x.len());
    for i in a..b {
        pre[i + 1] = pre[i] + (x[i] as f64) * (x[i] as f64);
    }
    let sum = |i: usize, j: usize| -> f64 {
        let (i, j) = (i.max(a), j.min(b));
        if j <= i {
            0.0
        } else {
            pre[j] - pre[i]
        }
    };
    let mut best = (0.0f64, None);
    for n in s0..s1 {
        let ea = sum(n, n + after) / after as f64;
        let eb = sum(n.saturating_sub(before), n) / before as f64;
        let score = ea / (eb + 1e-9);
        if score >= best.0 * (1.0 - 1e-9) && ea > 1e-8 {
            best = (score, Some(n));
        }
    }
    best.1
}

/// Stretch planar channels by `factor` (> 0). `factor == 1.0` returns the input unchanged.
pub fn stretch(
    channels: &[Vec<f32>],
    sr: u32,
    factor: f64,
    mode: Mode,
    quality: Quality,
) -> (Vec<Vec<f32>>, Report) {
    let mut report = Report {
        factor,
        transients: 0,
        anchored: 0,
        tonal_segments: 0,
        percussive_segments: 0,
        engine: "identity",
    };
    let len = channels.first().map(|c| c.len()).unwrap_or(0);
    if factor == 1.0 || len == 0 {
        return (channels.to_vec(), report);
    }
    let mono = crate::util::mono(channels);
    let wp = if mode == Mode::Speech {
        WsolaParams::speech(sr)
    } else {
        WsolaParams::music(sr)
    };
    let pv = match (mode, quality) {
        (Mode::Speech, _) => PvParams::speech(sr),
        (_, Quality::Fast) => PvParams::fast(sr),
        _ => PvParams::high(sr),
    };
    let pv = PvParams {
        formant_preserve: false,
        ..pv
    };

    if mode == Mode::Tonal {
        let map = TimeMap::uniform(len, factor);
        report.engine = "phase_vocoder";
        report.tonal_segments = 1;
        let out = channels
            .iter()
            .map(|c| vocoder::render(c, &map, &|_| 1.0, &|_| false, &pv))
            .collect();
        return (out, report);
    }

    let oa = analyze_onsets(&mono, sr);
    report.transients = oa.onsets.len();
    let use_pv = quality == Quality::High && matches!(mode, Mode::Auto | Mode::Speech);
    let radius = if use_pv {
        wp.protect_radius().max(pv.n_fft / 2 + 16)
    } else {
        wp.protect_radius()
    };
    let map = TimeMap::anchored(len, factor, &oa.onsets, radius, radius);
    report.anchored = map.protected().len();

    if !use_pv {
        report.engine = "wsola";
        report.percussive_segments = map.protected().len() + 1;
        let out = channels.iter().map(|c| wsola::render(c, &map, &wp)).collect();
        return (out, report);
    }

    // classify segments between protected windows (in output coordinates)
    let out_len = map.out_len;
    let mut bounds: Vec<(f64, f64, bool)> = Vec::new(); // (start, end, protected)
    let mut cur = 0.0;
    for &(a, b) in map.protected() {
        bounds.push((cur, a, false));
        bounds.push((a, b, true));
        cur = b;
    }
    bounds.push((cur, out_len as f64, false));
    let mut mask = vec![0.0f32; out_len]; // 1 = vocoder, 0 = WSOLA
    for &(a, b, prot) in &bounds {
        if prot || b <= a {
            continue;
        }
        // input range of the segment → analysis frames
        let ia = map.in_at(a) as usize / oa.hop;
        let ib = (map.in_at(b) as usize / oa.hop).min(oa.flatness.len());
        let (mut fw, mut ew) = (0.0, 0.0);
        let mut peak = 0.0f64;
        for k in ia..ib.max(ia) {
            let e = oa.rms[k] * oa.rms[k];
            fw += oa.flatness[k] * e;
            ew += e;
            peak = peak.max(oa.rms[k]);
        }
        let flat = if ew > 0.0 { fw / ew } else { 1.0 };
        let tonal = peak > 1e-3 && flat < 0.3;
        if tonal {
            report.tonal_segments += 1;
            for m in mask[a as usize..(b as usize).min(out_len)].iter_mut() {
                *m = 1.0;
            }
        } else {
            report.percussive_segments += 1;
        }
    }
    // 10 ms cross-fades: smooth the mask with a ramp
    let ramp = ((0.01 * sr as f64) as usize).max(1);
    let mask = smooth_mask(&mask, ramp);
    let any_pv = mask.iter().any(|&m| m > 0.0);
    let any_ws = mask.iter().any(|&m| m < 1.0);
    report.engine = match (any_pv, any_ws) {
        (true, true) => "hybrid",
        (true, false) => "phase_vocoder",
        _ => "wsola",
    };
    let out = channels
        .iter()
        .map(|c| {
            let p = if any_pv {
                Some(vocoder::render(c, &map, &|_| 1.0, &|_| false, &pv))
            } else {
                None
            };
            let w = if any_ws {
                Some(wsola::render(c, &map, &wp))
            } else {
                None
            };
            match (p, w) {
                (Some(p), Some(w)) => p
                    .iter()
                    .zip(&w)
                    .zip(&mask)
                    .map(|((a, b), m)| a * m + b * (1.0 - m))
                    .collect(),
                (Some(p), None) => p,
                (None, Some(w)) => w,
                (None, None) => vec![0.0; out_len],
            }
        })
        .collect();
    (out, report)
}

/// Linear ramps of `ramp` samples at every mask transition (centred on the edge).
fn smooth_mask(mask: &[f32], ramp: usize) -> Vec<f32> {
    let n = mask.len();
    let mut pre = vec![0.0f64; n + 1];
    for (i, v) in mask.iter().enumerate() {
        pre[i + 1] = pre[i] + *v as f64;
    }
    let h = ramp / 2;
    (0..n)
        .map(|i| {
            let a = i.saturating_sub(h);
            let b = (i + ramp - h).min(n);
            ((pre[b] - pre[a]) / (b - a).max(1) as f64) as f32
        })
        .collect()
}

/// Rough tempo estimate (BPM, confidence 0..1) from the onset-strength autocorrelation,
/// searched over 60..200 BPM with a log-normal preference around 120 BPM.
pub fn estimate_tempo(x: &[f32], sr: u32) -> Option<(f64, f64)> {
    let oa = analyze_onsets(x, sr);
    // decimate the flux to ~10 ms
    let step = ((0.01 * sr as f64) / oa.hop as f64).round().max(1.0) as usize;
    let env: Vec<f64> = oa
        .flux
        .chunks(step)
        .map(|c| c.iter().cloned().fold(0.0, f64::max))
        .collect();
    let hop_s = (oa.hop * step) as f64 / sr as f64;
    if env.len() < (4.0 / hop_s) as usize {
        return None;
    }
    let mean = env.iter().sum::<f64>() / env.len() as f64;
    let e: Vec<f64> = env.iter().map(|v| v - mean).collect();
    let r0: f64 = e.iter().map(|v| v * v).sum();
    if r0 <= 1e-12 {
        return None;
    }
    let lag_min = (60.0 / 200.0 / hop_s).floor() as usize;
    let lag_max = (60.0 / 60.0 / hop_s).ceil() as usize;
    let ac = |l: usize| -> f64 { e.iter().zip(&e[l..]).map(|(a, b)| a * b).sum::<f64>() / r0 };
    let mut best = (f64::NEG_INFINITY, 0usize, 0.0);
    for l in lag_min.max(1)..=lag_max.min(e.len() - 1) {
        let r = ac(l);
        let bpm = 60.0 / (l as f64 * hop_s);
        let w = (-0.5 * ((bpm / 120.0).log2() / 1.0).powi(2)).exp();
        if r * w > best.0 {
            best = (r * w, l, r);
        }
    }
    if best.1 == 0 {
        return None;
    }
    let l = best.1;
    let (a, b, c) = (ac(l - 1), ac(l), ac(l + 1));
    let den = a - 2.0 * b + c;
    let off = if den.abs() > 1e-12 {
        (0.5 * (a - c) / den).clamp(-0.5, 0.5)
    } else {
        0.0
    };
    let bpm = 60.0 / ((l as f64 + off) * hop_s);
    Some((bpm, best.2.clamp(0.0, 1.0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn click_positions(y: &[f32], thr: f32) -> Vec<usize> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < y.len() {
            if y[i].abs() > thr {
                // local peak within 2 ms
                let end = (i + 100).min(y.len());
                let p = (i..end)
                    .max_by(|a, b| y[*a].abs().partial_cmp(&y[*b].abs()).unwrap())
                    .unwrap();
                out.push(p);
                i = end;
            } else {
                i += 1;
            }
        }
        out
    }

    #[test]
    fn factor_one_is_bit_exact() {
        let x = ferraus_core::testing::noise(8000, 0.2, 0.5, 1).data;
        let (y, r) = stretch(&x, 8000, 1.0, Mode::Auto, Quality::High);
        assert_eq!(y, x);
        assert_eq!(r.engine, "identity");
    }

    #[test]
    fn click_track_factor_two_keeps_click_times() {
        let sr = 44100u32;
        let x = ferraus_core::testing::clicks(sr, 0.25, 2.0, 0.9).data.remove(0);
        let input_clicks = click_positions(&x, 0.3);
        for mode in [Mode::Percussive, Mode::Auto] {
            let (y, rep) = stretch(&[x.clone()], sr, 2.0, mode, Quality::High);
            assert_eq!(y[0].len(), x.len() * 2);
            let got = click_positions(&y[0], 0.3);
            // first click at sample 0 cannot be anchored (no room) but is still copied
            assert_eq!(got.len(), input_clicks.len(), "{mode:?} {rep:?} {got:?}");
            for (g, c) in got.iter().zip(&input_clicks) {
                let want = *c as f64 * 2.0;
                let err_ms = (*g as f64 - want).abs() / sr as f64 * 1000.0;
                assert!(err_ms <= 1.0, "{mode:?}: click at {g}, want {want} ({err_ms} ms)");
            }
        }
    }

    #[test]
    fn tonal_stretch_keeps_pitch_and_length() {
        let sr = 16000;
        let x = crate::fixtures::vocal(sr, &[(220.0, 0.6)], crate::fixtures::Vowel::A, 0.0, 0.0);
        for mode in [Mode::Auto, Mode::Tonal, Mode::Speech, Mode::Percussive] {
            let (y, _) = stretch(&[x.clone()], sr, 0.75, mode, Quality::High);
            assert_eq!(y[0].len(), (x.len() as f64 * 0.75).round() as usize);
            let fr = crate::pyin::track(&y[0], sr, &crate::pyin::PyinOptions::default());
            let v: Vec<f64> = fr[5..fr.len() - 5].iter().filter(|f| f.voiced).map(|f| f.freq_hz).collect();
            let m = crate::util::median(&v).unwrap();
            assert!(crate::util::cents(220.0, m).abs() < 5.0, "{mode:?} {m}");
        }
    }

    #[test]
    fn tempo_of_click_track() {
        let sr = 22050;
        let x = ferraus_core::testing::clicks(sr, 0.5, 8.0, 0.8).data.remove(0);
        let (bpm, _) = estimate_tempo(&x, sr).unwrap();
        assert!((bpm - 120.0).abs() < 2.0, "{bpm}");
    }
}
