//! EBU R128 / ITU-R BS.1770-4 loudness, implemented from the standard: K-weighting
//! biquads derived for any sample rate, 400 ms momentary and 3 s short-term windows on a
//! 100 ms grid, absolute (-70 LUFS) and relative (-10 LU) gating for integrated
//! loudness, loudness range (EBU Tech 3342, 10th–95th percentile of gated short-term
//! values), and true peak by polyphase oversampling.

use crate::util::Biquad;
use ferraus_core::AudioBuffer;
use serde::Serialize;
use std::f64::consts::PI;

/// The R128 measurement of one buffer. Loudness values that cannot be measured (silence,
/// input shorter than one window) are `-inf` and serialise as `null`.
#[derive(Clone, Debug, Serialize)]
pub struct LoudnessReport {
    /// Gated integrated loudness (LUFS).
    pub integrated_lufs: f64,
    /// Highest 3 s short-term loudness (LUFS).
    pub short_term_max_lufs: f64,
    /// Highest 400 ms momentary loudness (LUFS).
    pub momentary_max_lufs: f64,
    /// Loudness range (LU), EBU Tech 3342.
    pub loudness_range_lu: f64,
    /// Relative gate used for integrated loudness (LUFS).
    pub relative_gate_lufs: f64,
    /// Highest inter-sample peak over all channels (dBTP), 4x oversampled at 44.1/48 kHz.
    pub true_peak_dbtp: f64,
    /// Highest sample magnitude over all channels (dBFS).
    pub sample_peak_dbfs: f64,
    /// Peak-to-loudness ratio: true peak minus integrated loudness (dB).
    pub plr_db: f64,
    pub duration_s: f64,
    /// Momentary and short-term curves, present when requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeline: Option<LoudnessTimeline>,
}

/// Loudness curves on a regular grid. Value `i` is the loudness of the window ending at
/// `t_s[i]`; windows that start before the file are treated as silence-padded, like a
/// meter that starts at zero.
#[derive(Clone, Debug, Serialize)]
pub struct LoudnessTimeline {
    pub step_s: f64,
    pub t_s: Vec<f64>,
    pub momentary_lufs: Vec<f64>,
    pub short_term_lufs: Vec<f64>,
}

/// One short-term section and its loudness (used for loudness-outlier reports).
#[derive(Clone, Debug, Serialize)]
pub struct LoudnessSection {
    pub start_s: f64,
    pub end_s: f64,
    pub lufs: f64,
    /// Difference from the integrated loudness (LU).
    pub delta_lu: f64,
}

/// The two K-weighting stages (high shelf, then RLB high-pass) for a sample rate, using
/// the analog prototypes of BS.1770 so any rate gives the standard response.
pub fn k_weighting(sample_rate: f64) -> (Biquad, Biquad) {
    // stage 1: high shelf (+4 dB above ~1.5 kHz)
    let f0 = 1_681.974_450_955_533;
    let g = 3.999_843_853_973_347;
    let q = 0.707_175_236_955_419_6;
    let k = (PI * f0 / sample_rate).tan();
    let vh = 10f64.powf(g / 20.0);
    let vb = vh.powf(0.499_666_774_154_541_6);
    let a0 = 1.0 + k / q + k * k;
    let shelf = Biquad {
        b0: (vh + vb * k / q + k * k) / a0,
        b1: 2.0 * (k * k - vh) / a0,
        b2: (vh - vb * k / q + k * k) / a0,
        a1: 2.0 * (k * k - 1.0) / a0,
        a2: (1.0 - k / q + k * k) / a0,
    };
    // stage 2: RLB high-pass (~38 Hz)
    let f0 = 38.135_470_876_024_44;
    let q = 0.500_327_037_323_877_3;
    let k = (PI * f0 / sample_rate).tan();
    let d = 1.0 + k / q + k * k;
    let hp = Biquad { b0: 1.0, b1: -2.0, b2: 1.0, a1: 2.0 * (k * k - 1.0) / d, a2: (1.0 - k / q + k * k) / d };
    (shelf, hp)
}

/// BS.1770 channel weights: 1.0 for L, R, C; 0 for LFE (channel 4 of 6); 1.41 (+1.5 dB)
/// for surrounds.
pub fn channel_weights(channels: usize) -> Vec<f64> {
    match channels {
        5 => vec![1.0, 1.0, 1.0, 1.41, 1.41],
        6 => vec![1.0, 1.0, 1.0, 0.0, 1.41, 1.41],
        n => vec![1.0; n],
    }
}

/// Convert a weighted mean-square energy to LUFS (`-inf` for zero).
#[inline]
pub fn energy_to_lufs(e: f64) -> f64 {
    if e <= 0.0 {
        f64::NEG_INFINITY
    } else {
        -0.691 + 10.0 * e.log10()
    }
}

#[inline]
fn lufs_to_energy(l: f64) -> f64 {
    10f64.powf((l + 0.691) / 10.0)
}

/// K-weighted energies of consecutive 100 ms sub-blocks. Sub-block `k` spans samples
/// `[round(k*h), round((k+1)*h))` with `h = sr/10`, so any sample rate works.
struct SubBlocks {
    /// Channel-weighted sum of squares per sub-block.
    sums: Vec<f64>,
    /// Sample count of each sub-block.
    lens: Vec<usize>,
    sr: f64,
}

impl SubBlocks {
    fn new(buf: &AudioBuffer) -> SubBlocks {
        let sr = buf.sample_rate as f64;
        let n = buf.frames();
        let h = sr / 10.0;
        let count = (n as f64 / h).floor() as usize;
        let bounds: Vec<usize> = (0..=count).map(|k| (k as f64 * h).round() as usize).collect();
        let weights = channel_weights(buf.channels());
        let (shelf, hp) = k_weighting(sr);
        let mut sums = vec![0.0f64; count];
        for (c, ch) in buf.data.iter().enumerate() {
            let w = weights[c];
            if w == 0.0 {
                continue;
            }
            // run both stages inline in f64 to avoid two passes and an allocation
            let (mut x1, mut x2, mut y1, mut y2) = (0.0f64, 0.0, 0.0, 0.0);
            let (mut u1, mut u2, mut z1, mut z2) = (0.0f64, 0.0, 0.0, 0.0);
            let mut blk = 0usize;
            let mut acc = 0.0f64;
            for (i, &v) in ch.iter().enumerate() {
                let v = v as f64;
                let y = shelf.b0 * v + shelf.b1 * x1 + shelf.b2 * x2 - shelf.a1 * y1 - shelf.a2 * y2;
                x2 = x1;
                x1 = v;
                y2 = y1;
                y1 = y;
                let z = hp.b0 * y + hp.b1 * u1 + hp.b2 * u2 - hp.a1 * z1 - hp.a2 * z2;
                u2 = u1;
                u1 = y;
                z2 = z1;
                z1 = z;
                if blk >= count {
                    break;
                }
                acc += z * z;
                if i + 1 == bounds[blk + 1] {
                    sums[blk] += w * acc;
                    acc = 0.0;
                    blk += 1;
                }
            }
        }
        let lens = bounds.windows(2).map(|w| w[1] - w[0]).collect();
        SubBlocks { sums, lens, sr }
    }

    fn count(&self) -> usize {
        self.sums.len()
    }

    /// Mean-square energy of the window of `m` sub-blocks ending at sub-block `j`
    /// (inclusive). With `pad`, missing sub-blocks before the start count as silence of
    /// nominal length.
    fn window(&self, j: usize, m: usize, pad: bool) -> f64 {
        let start = (j + 1).saturating_sub(m);
        let s: f64 = self.sums[start..=j].iter().sum();
        let mut len: usize = self.lens[start..=j].iter().sum();
        if pad && j + 1 < m {
            len += ((m - (j + 1)) as f64 * self.sr / 10.0).round() as usize;
        }
        if len == 0 {
            0.0
        } else {
            s / len as f64
        }
    }

    /// Energies of all complete windows of `m` sub-blocks (hop = 1 sub-block).
    fn windows(&self, m: usize) -> Vec<f64> {
        if self.count() < m {
            return Vec::new();
        }
        (m - 1..self.count()).map(|j| self.window(j, m, false)).collect()
    }
}

/// Gated integrated loudness from 400 ms block energies: returns `(integrated, relative_gate)`.
fn gated_integrated(blocks: &[f64]) -> (f64, f64) {
    let abs_e = lufs_to_energy(-70.0);
    let above: Vec<f64> = blocks.iter().copied().filter(|e| *e > abs_e).collect();
    if above.is_empty() {
        return (f64::NEG_INFINITY, f64::NEG_INFINITY);
    }
    let rel_gate = energy_to_lufs(above.iter().sum::<f64>() / above.len() as f64) - 10.0;
    let rel_e = lufs_to_energy(rel_gate);
    let gated: Vec<f64> = above.into_iter().filter(|e| *e > rel_e).collect();
    if gated.is_empty() {
        return (f64::NEG_INFINITY, rel_gate);
    }
    (energy_to_lufs(gated.iter().sum::<f64>() / gated.len() as f64), rel_gate)
}

/// Loudness range (LU) from short-term window energies (EBU Tech 3342).
fn loudness_range(st: &[f64]) -> f64 {
    let abs_e = lufs_to_energy(-70.0);
    let above: Vec<f64> = st.iter().copied().filter(|e| *e > abs_e).collect();
    if above.is_empty() {
        return 0.0;
    }
    let rel = energy_to_lufs(above.iter().sum::<f64>() / above.len() as f64) - 20.0;
    let rel_e = lufs_to_energy(rel);
    let mut l: Vec<f64> = above.into_iter().filter(|e| *e > rel_e).map(energy_to_lufs).collect();
    if l.is_empty() {
        return 0.0;
    }
    l.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = l.len();
    let lo = ((n - 1) as f64 * 0.10 + 0.5) as usize;
    let hi = ((n - 1) as f64 * 0.95 + 0.5) as usize;
    l[hi] - l[lo]
}

/// Measure a buffer without timeline curves.
pub fn loudness(buf: &AudioBuffer) -> LoudnessReport {
    loudness_with(buf, None)
}

/// Measure a buffer; `timeline_step_s` adds momentary and short-term curves sampled every
/// step (rounded to a multiple of 100 ms, minimum 100 ms).
pub fn loudness_with(buf: &AudioBuffer, timeline_step_s: Option<f64>) -> LoudnessReport {
    let sb = SubBlocks::new(buf);
    let mom = sb.windows(4);
    let st = sb.windows(30);
    let (integrated, rel_gate) = gated_integrated(&mom);
    let momentary_max = mom.iter().copied().map(energy_to_lufs).fold(f64::NEG_INFINITY, f64::max);
    let short_term_max = if !st.is_empty() {
        st.iter().copied().map(energy_to_lufs).fold(f64::NEG_INFINITY, f64::max)
    } else if sb.count() > 0 {
        energy_to_lufs(sb.window(sb.count() - 1, 30, true))
    } else {
        f64::NEG_INFINITY
    };
    let lra = loudness_range(&st);
    let tp = true_peak(buf);
    let sp = buf.peak() as f64;
    let tp_db = crate::util::amp_db(tp);
    let timeline = timeline_step_s.map(|step| {
        let every = ((step / 0.1).round() as usize).max(1);
        let mut t_s = Vec::new();
        let mut m = Vec::new();
        let mut s = Vec::new();
        let mut j = every - 1;
        while j < sb.count() {
            t_s.push((j + 1) as f64 * 0.1);
            m.push(energy_to_lufs(sb.window(j, 4, true)));
            s.push(energy_to_lufs(sb.window(j, 30, true)));
            j += every;
        }
        LoudnessTimeline { step_s: every as f64 * 0.1, t_s, momentary_lufs: m, short_term_lufs: s }
    });
    LoudnessReport {
        integrated_lufs: integrated,
        short_term_max_lufs: short_term_max,
        momentary_max_lufs: momentary_max,
        loudness_range_lu: lra,
        relative_gate_lufs: rel_gate,
        true_peak_dbtp: tp_db,
        sample_peak_dbfs: crate::util::amp_db(sp),
        plr_db: if integrated.is_finite() { tp_db - integrated } else { f64::NAN },
        duration_s: buf.duration_s(),
        timeline,
    }
}

/// Integrated loudness only (LUFS, `-inf` when everything is gated out).
pub fn integrated_lufs(buf: &AudioBuffer) -> f64 {
    let sb = SubBlocks::new(buf);
    gated_integrated(&sb.windows(4)).0
}

/// Short-term (3 s) loudness on a 100 ms grid with complete windows only: `(end_time_s, lufs)`.
pub fn short_term_curve(buf: &AudioBuffer) -> Vec<(f64, f64)> {
    let sb = SubBlocks::new(buf);
    if sb.count() < 30 {
        return Vec::new();
    }
    (29..sb.count()).map(|j| ((j + 1) as f64 * 0.1, energy_to_lufs(sb.window(j, 30, false)))).collect()
}

/// Sections (non-overlapping 3 s windows) whose short-term loudness differs from the
/// integrated loudness by more than `threshold_lu`. Sections more than 20 LU below the
/// integrated level (silence, intentional fades) are ignored.
pub fn loudness_outliers(buf: &AudioBuffer, threshold_lu: f64) -> Vec<LoudnessSection> {
    let sb = SubBlocks::new(buf);
    let (integrated, _) = gated_integrated(&sb.windows(4));
    if !integrated.is_finite() || sb.count() < 30 {
        return Vec::new();
    }
    let mut out: Vec<LoudnessSection> = Vec::new();
    let mut j = 29;
    while j < sb.count() {
        let l = energy_to_lufs(sb.window(j, 30, false));
        let d = l - integrated;
        if d.abs() > threshold_lu && d > -20.0 {
            let start = (j + 1 - 30) as f64 * 0.1;
            let end = (j + 1) as f64 * 0.1;
            // merge with the previous section when adjacent and on the same side
            if let Some(last) = out.last_mut() {
                if (last.end_s - start).abs() < 1e-9 && last.delta_lu.signum() == d.signum() {
                    last.end_s = end;
                    if d.abs() > last.delta_lu.abs() {
                        last.delta_lu = d;
                        last.lufs = l;
                    }
                    j += 30;
                    continue;
                }
            }
            out.push(LoudnessSection { start_s: start, end_s: end, lufs: l, delta_lu: d });
        }
        j += 30;
    }
    out
}

// ---------------------------------------------------------------------------------
// True peak
// ---------------------------------------------------------------------------------

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let q = x * x / 4.0;
    for k in 1..50 {
        term *= q / (k * k) as f64;
        sum += term;
        if term < 1e-12 * sum {
            break;
        }
    }
    sum
}

/// Polyphase interpolation filter: `factor` phases of `taps` coefficients each, stored
/// reversed so each output is a dot product with a contiguous input slice.
fn polyphase(factor: usize, taps: usize) -> Vec<Vec<f32>> {
    let total = factor * taps;
    let fc = 0.5 / factor as f64;
    let beta = 8.0;
    let mid = (total - 1) as f64 / 2.0;
    let i0b = bessel_i0(beta);
    let mut h: Vec<f64> = (0..total)
        .map(|i| {
            let t = i as f64 - mid;
            let sinc = if t.abs() < 1e-12 { 2.0 * fc } else { (2.0 * PI * fc * t).sin() / (PI * t) };
            let r = (i as f64 - mid) / mid;
            let win = bessel_i0(beta * (1.0 - r * r).max(0.0).sqrt()) / i0b;
            sinc * win
        })
        .collect();
    let s: f64 = h.iter().sum();
    for v in h.iter_mut() {
        *v *= factor as f64 / s;
    }
    (0..factor)
        .map(|p| {
            let mut ph: Vec<f32> = (0..taps).map(|k| h[p + k * factor] as f32).collect();
            ph.reverse();
            ph
        })
        .collect()
}

/// Oversampling factor used for true peak at a sample rate: 4x up to 48 kHz, 2x up to
/// 96 kHz, none above.
pub fn true_peak_factor(sample_rate: u32) -> usize {
    if sample_rate <= 48_000 {
        4
    } else if sample_rate <= 96_000 {
        2
    } else {
        1
    }
}

/// True peak of one channel (linear), never below its sample peak.
pub fn true_peak_channel(x: &[f32], sample_rate: u32) -> f64 {
    let sp = x.iter().fold(0.0f32, |m, v| m.max(v.abs())) as f64;
    let factor = true_peak_factor(sample_rate);
    if factor == 1 || x.is_empty() {
        return sp;
    }
    let taps = 32;
    let phases = polyphase(factor, taps);
    let mut padded = vec![0.0f32; taps - 1];
    padded.extend_from_slice(x);
    padded.extend(std::iter::repeat_n(0.0, taps - 1));
    let mut peak = 0.0f32;
    for m in 0..x.len() + taps - 1 {
        let seg = &padded[m..m + taps];
        for ph in &phases {
            let y: f32 = seg.iter().zip(ph).map(|(a, b)| a * b).sum();
            peak = peak.max(y.abs());
        }
    }
    (peak as f64).max(sp)
}

/// Highest true peak over all channels (linear).
pub fn true_peak(buf: &AudioBuffer) -> f64 {
    buf.data.iter().map(|c| true_peak_channel(c, buf.sample_rate)).fold(0.0, f64::max)
}

/// Highest true peak over all channels in dBTP.
pub fn true_peak_db(buf: &AudioBuffer) -> f64 {
    crate::util::amp_db(true_peak(buf))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferraus_core::testing::{sine, stereo};

    fn tone(db: f64, secs: f64, sr: u32) -> AudioBuffer {
        stereo(sine(sr, 1000.0, secs, ferraus_core::db_to_lin(db) as f32))
    }

    fn seq(parts: &[(f64, f64)], sr: u32) -> AudioBuffer {
        let mut b = tone(parts[0].0, parts[0].1, sr);
        for (db, s) in &parts[1..] {
            b.append(&tone(*db, *s, sr));
        }
        b
    }

    fn reference(buf: &AudioBuffer) -> (f64, f64, f64, f64, f64) {
        use ebur128::{EbuR128, Mode};
        let mut m = EbuR128::new(
            buf.channels() as u32,
            buf.sample_rate,
            Mode::I | Mode::LRA | Mode::M | Mode::S | Mode::TRUE_PEAK | Mode::SAMPLE_PEAK,
        )
        .unwrap();
        let inter = buf.to_interleaved();
        // feed in 100 ms chunks so momentary/short-term maxima can be tracked
        let chunk = (buf.sample_rate / 10) as usize * buf.channels();
        let mut mmax = f64::NEG_INFINITY;
        let mut smax = f64::NEG_INFINITY;
        for (i, c) in inter.chunks(chunk).enumerate() {
            m.add_frames_f32(c).unwrap();
            if i >= 3 {
                mmax = mmax.max(m.loudness_momentary().unwrap());
            }
            if i >= 29 {
                smax = smax.max(m.loudness_shortterm().unwrap());
            }
        }
        let tp = (0..buf.channels() as u32).map(|c| m.true_peak(c).unwrap()).fold(0.0, f64::max);
        (m.loudness_global().unwrap(), mmax, smax, m.loudness_range().unwrap(), 20.0 * tp.log10())
    }

    #[test]
    fn k_weighting_matches_spec_coefficients_at_48k() {
        let (s, h) = k_weighting(48000.0);
        assert!((s.b0 - 1.53512485958697).abs() < 1e-6);
        assert!((s.b1 + 2.69169618940638).abs() < 1e-6);
        assert!((s.b2 - 1.19839281085285).abs() < 1e-6);
        assert!((s.a1 + 1.69065929318241).abs() < 1e-6);
        assert!((s.a2 - 0.73248077421585).abs() < 1e-6);
        assert!((h.a1 + 1.99004745483398).abs() < 1e-6);
        assert!((h.a2 - 0.99007225036621).abs() < 1e-6);
    }

    #[test]
    fn tech3341_tone_minus_23_and_minus_33() {
        for (db, sr) in [(-23.0, 48000), (-33.0, 48000), (-23.0, 44100), (-23.0, 96000)] {
            let b = tone(db, 20.0, sr);
            let r = loudness(&b);
            assert!((r.integrated_lufs - db).abs() < 0.1, "{sr} {db}: {}", r.integrated_lufs);
            assert!((r.momentary_max_lufs - db).abs() < 0.1);
            assert!((r.short_term_max_lufs - db).abs() < 0.1);
            assert!(r.loudness_range_lu < 0.1);
        }
    }

    #[test]
    fn tech3341_gating_cases() {
        let sr = 48000;
        // case 3: relative gate removes the -36 dBFS parts
        let c3 = seq(&[(-36.0, 10.0), (-23.0, 60.0), (-36.0, 10.0)], sr);
        // case 4: absolute gate removes -72 dBFS
        let c4 = seq(&[(-72.0, 10.0), (-36.0, 10.0), (-23.0, 60.0), (-36.0, 10.0), (-72.0, 10.0)], sr);
        // case 5: -26 / -20 / -26
        let c5 = seq(&[(-26.0, 20.0), (-20.0, 20.1), (-26.0, 20.0)], sr);
        for (name, b) in [("c3", &c3), ("c4", &c4), ("c5", &c5)] {
            let r = loudness(b);
            assert!((r.integrated_lufs + 23.0).abs() < 0.1, "{name}: {}", r.integrated_lufs);
            let (ri, rm, rs, _, _) = reference(b);
            assert!((r.integrated_lufs - ri).abs() < 0.1, "{name} vs ebur128: {} {ri}", r.integrated_lufs);
            assert!((r.momentary_max_lufs - rm).abs() < 0.1, "{name} M: {} {rm}", r.momentary_max_lufs);
            assert!((r.short_term_max_lufs - rs).abs() < 0.1, "{name} S: {} {rs}", r.short_term_max_lufs);
        }
    }

    #[test]
    fn tech3342_loudness_range() {
        let sr = 48000;
        let cases: Vec<(Vec<(f64, f64)>, f64)> = vec![
            (vec![(-20.0, 20.0), (-30.0, 20.0)], 10.0),
            (vec![(-20.0, 20.0), (-15.0, 20.0)], 5.0),
            (vec![(-40.0, 20.0), (-20.0, 20.0)], 20.0),
            (vec![(-50.0, 20.0), (-35.0, 20.0), (-20.0, 20.0), (-35.0, 20.0), (-50.0, 20.0)], 15.0),
        ];
        for (parts, want) in cases {
            let b = seq(&parts, sr);
            let r = loudness(&b);
            assert!((r.loudness_range_lu - want).abs() < 1.0, "LRA {} want {want}", r.loudness_range_lu);
            let (_, _, _, rl, _) = reference(&b);
            assert!((r.loudness_range_lu - rl).abs() < 0.5, "LRA {} vs ebur128 {rl}", r.loudness_range_lu);
        }
    }

    #[test]
    fn true_peak_finds_inter_sample_peaks() {
        let sr = 48000;
        let n = sr as usize;
        // fs/4 sine with a 45 degree phase: samples hit 0.707 of the true peak
        let amp = 0.5f32;
        let x: Vec<f32> = (0..n).map(|i| amp * (PI / 2.0 * i as f64 + PI / 4.0).sin() as f32).collect();
        let b = stereo(AudioBuffer::from_mono(sr, x));
        let r = loudness(&b);
        let want = 20.0 * (amp as f64).log10();
        assert!((r.true_peak_dbtp - want).abs() < 0.2, "tp {}", r.true_peak_dbtp);
        assert!(r.true_peak_dbtp > r.sample_peak_dbfs + 2.5, "{} {}", r.true_peak_dbtp, r.sample_peak_dbfs);
        let (_, _, _, _, rtp) = reference(&b);
        assert!((r.true_peak_dbtp - rtp).abs() < 0.3, "tp {} vs ebur128 {rtp}", r.true_peak_dbtp);
    }

    #[test]
    fn silence_and_short_inputs() {
        let b = AudioBuffer::new(48000, 2, 48000);
        let r = loudness(&b);
        assert!(r.integrated_lufs.is_infinite());
        assert_eq!(r.true_peak_dbtp, -240.0);
        let short = tone(-20.0, 0.2, 48000);
        let r = loudness(&short);
        assert!(r.integrated_lufs.is_infinite());
        let mono = sine(48000, 1000.0, 5.0, ferraus_core::db_to_lin(-20.0) as f32);
        let r = loudness(&mono);
        // one channel at -20 dBFS peak: half the power of the stereo case
        assert!((r.integrated_lufs + 23.01).abs() < 0.1, "{}", r.integrated_lufs);
    }

    #[test]
    fn timeline_has_expected_points() {
        let b = tone(-23.0, 5.0, 48000);
        let r = loudness_with(&b, Some(0.1));
        let t = r.timeline.unwrap();
        assert_eq!(t.t_s.len(), 50);
        assert!((t.momentary_lufs[10] + 23.0).abs() < 0.1);
        assert!(t.short_term_lufs[2] < -30.0, "padded short-term starts low");
    }

    #[test]
    fn outlier_sections() {
        let b = seq(&[(-20.0, 12.0), (-6.0, 3.0), (-20.0, 15.0)], 48000);
        let o = loudness_outliers(&b, 6.0);
        assert_eq!(o.len(), 1, "{o:?}");
        assert!(o[0].delta_lu > 6.0 && o[0].start_s > 11.9 && o[0].end_s < 15.1);
    }
}
