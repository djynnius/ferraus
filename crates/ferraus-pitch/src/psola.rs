//! TD-PSOLA (time-domain pitch-synchronous overlap-add) pitch shifting with a time-varying
//! ratio, driven by pitch marks derived from a pitch track.
//!
//! * **Pitch marks.** In each voiced run the first mark is the strongest peak (of the
//!   run's dominant polarity) within the first period; every next mark is predicted one
//!   local period ahead (from the pitch track) and refined within ±1/8 period by
//!   normalised cross-correlation with the previous period, so marks stay locked to the
//!   same point of each glottal cycle. Unvoiced stretches get evenly spaced marks (~5 ms).
//! * **Synthesis.** Each analysis mark owns a grain windowed by an asymmetric Hann whose
//!   halves reach the neighbouring marks. Synthesis marks advance by `period / ratio`
//!   in voiced regions and by the analysis spacing elsewhere; each takes the grain of the
//!   nearest analysis mark. The overlap-add is normalised by the summed windows, so a ratio
//!   of 1 reconstructs the input exactly. Grains carry the vocal-tract response unchanged,
//!   which is why PSOLA preserves formants.

use crate::notes::track_hop;
use crate::pyin::PitchFrame;

/// One analysis pitch mark.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mark {
    /// Sample position.
    pub pos: usize,
    /// Inside a voiced run (pitch-synchronous) or filler (unvoiced).
    pub voiced: bool,
}

/// Compute pitch marks for `x` from its pitch track.
pub fn pitch_marks(x: &[f32], sr: u32, frames: &[PitchFrame]) -> Vec<Mark> {
    let len = x.len();
    if len < 2 {
        return vec![Mark { pos: 0, voiced: false }];
    }
    let hop = track_hop(frames) * sr as f64;
    let srf = sr as f64;
    // f0 at a sample, from the nearest voiced frames
    let f0_at = |n: f64, fs: usize, fe: usize| -> f64 {
        let k = (n / hop).clamp(fs as f64, (fe - 1) as f64);
        let i = k.floor() as usize;
        let j = (i + 1).min(fe - 1);
        let f = k - i as f64;
        let a = frames[i].freq_hz;
        let b = if frames[j].voiced && frames[j].freq_hz > 0.0 {
            frames[j].freq_hz
        } else {
            a
        };
        a * (1.0 - f) + b * f
    };
    let mut voiced_marks: Vec<Vec<usize>> = Vec::new();
    let mut k = 0;
    while k < frames.len() {
        if !(frames[k].voiced && frames[k].freq_hz > 0.0) {
            k += 1;
            continue;
        }
        let fs = k;
        while k < frames.len() && frames[k].voiced && frames[k].freq_hz > 0.0 {
            k += 1;
        }
        let fe = k;
        let start = ((fs as f64 * hop) - hop / 2.0).max(0.0) as usize;
        let end = (((fe - 1) as f64 * hop + hop / 2.0) as usize).min(len);
        let t0 = srf / f0_at(start as f64, fs, fe);
        if (end as f64) - (start as f64) < 2.0 * t0 {
            continue;
        }
        // dominant polarity
        let seg = &x[start..end];
        let pmax = seg.iter().cloned().fold(0.0f32, f32::max);
        let nmax = seg.iter().cloned().fold(0.0f32, f32::min).abs();
        let sign = if pmax >= nmax { 1.0f32 } else { -1.0 };
        let first_end = (start + t0.ceil() as usize).min(end);
        let mut m = start;
        let mut best = f32::NEG_INFINITY;
        for (i, v) in x[start..first_end].iter().enumerate() {
            if sign * v > best {
                best = sign * v;
                m = start + i;
            }
        }
        let mut marks = vec![m];
        loop {
            let t = srf / f0_at(m as f64, fs, fe);
            let pred = m as f64 + t;
            if pred + t / 2.0 >= end as f64 {
                break;
            }
            let half = (t / 2.0).round() as isize;
            let search = ((t / 8.0).round() as isize).max(1);
            let pred_i = pred.round() as isize;
            let mut best = (f64::NEG_INFINITY, pred_i);
            for u in -search..=search {
                let c = pred_i + u;
                let (mut dot, mut e) = (0.0f64, 0.0f64);
                for i in -half..half {
                    let a = sample(x, m as isize + i);
                    let b = sample(x, c + i);
                    dot += a * b;
                    e += b * b;
                }
                let score = dot / e.max(1e-12).sqrt();
                if score > best.0 {
                    best = (score, c);
                }
            }
            let next = best.1.max(m as isize + 1) as usize;
            if next >= end {
                break;
            }
            marks.push(next);
            m = next;
        }
        voiced_marks.push(marks);
    }
    // assemble with unvoiced filler marks
    let u = (0.005 * srf).max(2.0);
    let mut out: Vec<Mark> = Vec::new();
    let mut last = 0usize;
    out.push(Mark { pos: 0, voiced: false });
    let fill = |out: &mut Vec<Mark>, from: usize, to: usize| {
        if to <= from + 1 {
            return;
        }
        let gap = (to - from) as f64;
        let count = (gap / u).round().max(1.0) as usize;
        for i in 1..count {
            let p = from + (gap * i as f64 / count as f64).round() as usize;
            if p > out.last().map(|m| m.pos).unwrap_or(0) && p < to {
                out.push(Mark { pos: p, voiced: false });
            }
        }
    };
    for run in voiced_marks {
        let first = run[0];
        if first <= last {
            continue;
        }
        fill(&mut out, last, first);
        for p in run {
            if p > out.last().unwrap().pos {
                out.push(Mark { pos: p, voiced: true });
            }
        }
        last = out.last().unwrap().pos;
    }
    fill(&mut out, last, len - 1);
    if out.last().unwrap().pos < len - 1 {
        out.push(Mark { pos: len - 1, voiced: false });
    }
    out
}

#[inline]
fn sample(x: &[f32], i: isize) -> f64 {
    if i >= 0 && (i as usize) < x.len() {
        x[i as usize] as f64
    } else {
        0.0
    }
}

/// Overlap-add synthesis of every channel with the same marks. `ratio_at(sample)` gives
/// the pitch ratio at an (output = input) sample position; unvoiced marks use ratio 1.
pub fn render(channels: &[Vec<f32>], marks: &[Mark], ratio_at: &dyn Fn(f64) -> f64) -> Vec<Vec<f32>> {
    let len = channels.first().map(|c| c.len()).unwrap_or(0);
    let mut out = vec![vec![0.0f32; len]; channels.len()];
    if len == 0 || marks.len() < 2 {
        return channels.to_vec();
    }
    let mut wsum = vec![0.0f32; len];
    let mlen = marks.len();
    let left = |k: usize| -> usize {
        if k > 0 {
            marks[k].pos - marks[k - 1].pos
        } else {
            marks[1].pos - marks[0].pos
        }
    };
    let right = |k: usize| -> usize {
        if k + 1 < mlen {
            marks[k + 1].pos - marks[k].pos
        } else {
            marks[k].pos - marks[k - 1].pos
        }
    };
    let mut t = marks[0].pos as f64;
    let mut k = 0usize;
    while t < len as f64 {
        while k + 1 < mlen && (marks[k + 1].pos as f64 - t).abs() <= (marks[k].pos as f64 - t).abs() {
            k += 1;
        }
        let (l, r) = (left(k).max(1), right(k).max(1));
        let a = marks[k].pos as isize;
        let s = t.round() as isize;
        for i in -(l as isize) + 1..r as isize {
            let o = s + i;
            if o < 0 || o as usize >= len {
                continue;
            }
            let src = a + i;
            if src < 0 || src as usize >= len {
                continue;
            }
            let u = if i < 0 { -i as f64 / l as f64 } else { i as f64 / r as f64 };
            let w = taper(u) as f32;
            wsum[o as usize] += w;
            for (oc, ic) in out.iter_mut().zip(channels) {
                oc[o as usize] += w * ic[src as usize];
            }
        }
        let step = if marks[k].voiced {
            r as f64 / ratio_at(t).clamp(0.25, 4.0)
        } else {
            r as f64
        };
        t += step.max(1.0);
    }
    // Normalise by the window sum averaged over one typical period: this removes the
    // level change of denser/sparser grains without imposing a pitch-synchronous gain
    // modulation (a constant sum of 1, as in unshifted regions, stays exactly 1).
    let mut sp: Vec<usize> = marks
        .windows(2)
        .filter(|w| w[0].voiced && w[1].voiced)
        .map(|w| w[1].pos - w[0].pos)
        .collect();
    sp.sort_unstable();
    let box_len = sp.get(sp.len() / 2).copied().unwrap_or(1).max(1);
    let smooth = box_mean(&wsum, box_len);
    for oc in out.iter_mut() {
        for ((v, w), s) in oc.iter_mut().zip(&wsum).zip(&smooth) {
            let d = if (*w - 1.0).abs() < 1e-6 { *w } else { *s };
            *v /= d.max(0.35);
        }
    }
    out
}

/// Hann half-window on the normalised distance `u` (0 at the mark, 1 at the neighbouring
/// mark). `taper(u) + taper(1 − u) = 1`, so unshifted grains reconstruct the input exactly.
#[inline]
fn taper(u: f64) -> f64 {
    0.5 * (1.0 + (std::f64::consts::PI * u.min(1.0)).cos())
}

/// Centred moving average with window `n` (shrinking at the edges).
fn box_mean(x: &[f32], n: usize) -> Vec<f32> {
    let len = x.len();
    let mut pre = vec![0.0f64; len + 1];
    for (i, v) in x.iter().enumerate() {
        pre[i + 1] = pre[i] + *v as f64;
    }
    let h = n / 2;
    (0..len)
        .map(|i| {
            let a = i.saturating_sub(h);
            let b = (i + n - h).min(len);
            ((pre[b] - pre[a]) / (b - a).max(1) as f64) as f32
        })
        .collect()
}

/// Shift every channel with a per-frame ratio curve aligned with `frames` (analysis is
/// done on the mono mix `analysis`).
pub fn shift_with_track(
    channels: &[Vec<f32>],
    analysis: &[f32],
    sr: u32,
    frames: &[PitchFrame],
    ratio_per_frame: &[f64],
) -> Vec<Vec<f32>> {
    let marks = pitch_marks(analysis, sr, frames);
    let hop = track_hop(frames) * sr as f64;
    render(channels, &marks, &|pos| crate::util::interp(ratio_per_frame, pos / hop))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::*;
    use crate::pyin::{track, PyinOptions};

    #[test]
    fn ratio_one_is_exact() {
        let sr = 16000;
        let x = vocal(sr, &[(140.0, 0.3), (180.0, 0.3)], Vowel::A, 0.0, 0.0);
        let fr = track(&x, sr, &PyinOptions::default());
        let y = shift_with_track(&[x.clone()], &x, sr, &fr, &vec![1.0; fr.len()]);
        for (a, b) in x.iter().zip(&y[0]) {
            assert!((a - b).abs() < 1e-5);
        }
    }

    #[test]
    fn marks_follow_periods() {
        let sr = 16000;
        let x = vocal(sr, &[(160.0, 0.4)], Vowel::A, 0.0, 0.0);
        let fr = track(&x, sr, &PyinOptions::default());
        let marks = pitch_marks(&x, sr, &fr);
        let v: Vec<usize> = marks.iter().filter(|m| m.voiced).map(|m| m.pos).collect();
        assert!(v.len() > 50);
        let mean = (v[v.len() - 1] - v[0]) as f64 / (v.len() - 1) as f64;
        assert!((mean - 100.0).abs() < 0.5, "mean period {mean}");
    }

    #[test]
    fn shifts_pitch() {
        let sr = 16000;
        let x = vocal(sr, &[(150.0, 0.8)], Vowel::A, 0.0, 0.0);
        let fr = track(&x, sr, &PyinOptions::default());
        for st in [4.0f64, -2.0, 0.4] {
            let r = 2f64.powf(st / 12.0);
            let y = shift_with_track(&[x.clone()], &x, sr, &fr, &vec![r; fr.len()]);
            let fy = track(&y[0], sr, &PyinOptions::default());
            let v: Vec<f64> = fy[10..fy.len() - 10].iter().filter(|f| f.voiced).map(|f| f.freq_hz).collect();
            let m = crate::util::median(&v).unwrap();
            assert!(crate::util::cents(150.0 * r, m).abs() < 3.0, "{st}: {m}");
        }
    }
}
