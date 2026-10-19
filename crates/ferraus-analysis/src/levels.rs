//! Level-based analyses: silence regions, clipping runs, and per-channel amplitude
//! statistics with an optional dB histogram.

use crate::util::{self, amp_db, pow_db};
use ferraus_core::{db_to_lin, AudioBuffer};
use serde::Serialize;

// ---------------------------------------------------------------------------------
// Silence
// ---------------------------------------------------------------------------------

/// One silent region.
#[derive(Clone, Debug, Serialize)]
pub struct SilenceRegion {
    pub start_s: f64,
    pub end_s: f64,
    pub duration_s: f64,
    /// RMS level of the region (loudest channel), dBFS.
    pub floor_db: f64,
}

/// Result of [`detect_silence`].
#[derive(Clone, Debug, Serialize)]
pub struct SilenceReport {
    pub threshold_db: f64,
    pub min_duration_s: f64,
    pub regions: Vec<SilenceRegion>,
    pub total_silence_s: f64,
    pub silent_fraction: f64,
    /// Silence before the first frame above the threshold (regardless of `min`).
    pub head_silence_s: f64,
    /// Silence after the last frame above the threshold.
    pub tail_silence_s: f64,
}

/// Frame RMS levels (dB, loudest channel) on a 10 ms grid; returns `(frame_len, levels, energies)`.
fn frame_levels(buf: &AudioBuffer, frame_s: f64) -> (usize, Vec<f64>, Vec<f64>) {
    let fl = ((buf.sample_rate as f64 * frame_s).round() as usize).max(1);
    let n = buf.frames();
    let count = n.div_ceil(fl);
    let mut energies = vec![0.0f64; count];
    for ch in &buf.data {
        for (f, e) in energies.iter_mut().enumerate() {
            let a = f * fl;
            let b = (a + fl).min(n);
            let ms = ch[a..b].iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / (b - a) as f64;
            *e = e.max(ms);
        }
    }
    let levels = energies.iter().map(|e| pow_db(*e)).collect();
    (fl, levels, energies)
}

/// Find regions quieter than `threshold_db` (RMS over 10 ms frames, loudest channel)
/// lasting at least `min_duration_s`.
pub fn detect_silence(buf: &AudioBuffer, threshold_db: f64, min_duration_s: f64) -> SilenceReport {
    let sr = buf.sample_rate as f64;
    let n = buf.frames();
    let dur = buf.duration_s();
    let (fl, levels, energies) = frame_levels(buf, 0.01);
    let mut regions = Vec::new();
    let mut i = 0;
    while i < levels.len() {
        if levels[i] < threshold_db {
            let s = i;
            while i < levels.len() && levels[i] < threshold_db {
                i += 1;
            }
            let start = (s * fl) as f64 / sr;
            let end = ((i * fl).min(n)) as f64 / sr;
            if end - start + 1e-9 >= min_duration_s {
                let e = util::mean(&energies[s..i]);
                regions.push(SilenceRegion {
                    start_s: start,
                    end_s: end,
                    duration_s: end - start,
                    floor_db: pow_db(e),
                });
            }
        } else {
            i += 1;
        }
    }
    let first = levels.iter().position(|l| *l >= threshold_db);
    let last = levels.iter().rposition(|l| *l >= threshold_db);
    let (head, tail) = match (first, last) {
        (Some(f), Some(l)) => (
            (f * fl) as f64 / sr,
            (dur - ((l + 1) * fl).min(n) as f64 / sr).max(0.0),
        ),
        _ => (dur, dur),
    };
    let total: f64 = regions.iter().map(|r| r.duration_s).sum();
    SilenceReport {
        threshold_db,
        min_duration_s,
        total_silence_s: total,
        silent_fraction: if dur > 0.0 { total / dur } else { 0.0 },
        regions,
        head_silence_s: head,
        tail_silence_s: tail,
    }
}

// ---------------------------------------------------------------------------------
// Clipping
// ---------------------------------------------------------------------------------

/// One run of clipped samples.
#[derive(Clone, Debug, Serialize)]
pub struct ClipRun {
    pub channel: usize,
    pub start_s: f64,
    pub start_sample: usize,
    pub length: usize,
    /// Level of the run (dBFS).
    pub level_db: f64,
    /// `threshold` (at or above the threshold) or `flat` (a flat top below it).
    pub kind: &'static str,
}

/// Result of [`detect_clipping`].
#[derive(Clone, Debug, Serialize)]
pub struct ClippingReport {
    pub threshold_db: f64,
    pub min_run: usize,
    /// Number of clipped runs.
    pub count: usize,
    pub clipped_samples: usize,
    pub clipped_fraction: f64,
    pub runs: Vec<ClipRun>,
    /// True when `runs` was cut to the first 1000 entries.
    pub truncated: bool,
}

const MAX_RUNS: usize = 1000;

/// Find runs of at least `min_run` consecutive samples at or above `threshold_db`
/// (same polarity), plus flat-topped runs (identical samples at the channel's peak, above
/// −12 dBFS) that indicate clipping below full scale.
pub fn detect_clipping(buf: &AudioBuffer, threshold_db: f64, min_run: usize) -> ClippingReport {
    let sr = buf.sample_rate as f64;
    let thr = (db_to_lin(threshold_db) * (1.0 - 1e-4)) as f32;
    let min_run = min_run.max(1);
    let mut runs: Vec<ClipRun> = Vec::new();
    let mut count = 0usize;
    let mut clipped = 0usize;
    for (c, ch) in buf.data.iter().enumerate() {
        let peak = ch.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        let mut push = |start: usize, len: usize, level: f32, kind: &'static str| {
            count += 1;
            clipped += len;
            if runs.len() < MAX_RUNS {
                runs.push(ClipRun {
                    channel: c,
                    start_s: start as f64 / sr,
                    start_sample: start,
                    length: len,
                    level_db: amp_db(level as f64),
                    kind,
                });
            }
        };
        let mut i = 0;
        while i < ch.len() {
            let v = ch[i];
            if v.abs() >= thr {
                let s = i;
                let sign = v.signum();
                let mut lvl = 0.0f32;
                while i < ch.len() && ch[i].abs() >= thr && ch[i].signum() == sign {
                    lvl = lvl.max(ch[i].abs());
                    i += 1;
                }
                if i - s >= min_run {
                    push(s, i - s, lvl, "threshold");
                }
                continue;
            }
            // flat top below the threshold
            if peak >= 0.25 && v.abs() >= 0.98 * peak && v.abs() < thr {
                let s = i;
                while i + 1 < ch.len() && (ch[i + 1] - v).abs() <= 1e-7 {
                    i += 1;
                }
                let len = i - s + 1;
                if len >= min_run.max(3) {
                    push(s, len, v.abs(), "flat");
                }
                i += 1;
                continue;
            }
            i += 1;
        }
    }
    let total = buf.frames() * buf.channels();
    ClippingReport {
        threshold_db,
        min_run,
        count,
        clipped_samples: clipped,
        clipped_fraction: if total > 0 {
            clipped as f64 / total as f64
        } else {
            0.0
        },
        truncated: count > runs.len(),
        runs,
    }
}

// ---------------------------------------------------------------------------------
// Amplitude statistics
// ---------------------------------------------------------------------------------

/// Sample-level histogram in dBFS: `counts[i]` holds samples whose level lies in
/// `(bins_db[i] - 1, bins_db[i]]`; the first bin also holds everything quieter
/// (including digital silence) and the last everything louder.
#[derive(Clone, Debug, Serialize)]
pub struct Histogram {
    pub bins_db: Vec<f64>,
    pub counts: Vec<u64>,
}

/// Statistics of one channel.
#[derive(Clone, Debug, Serialize)]
pub struct ChannelStats {
    pub channel: usize,
    pub peak_db: f64,
    pub true_peak_dbtp: f64,
    pub rms_db: f64,
    /// Mean sample value (linear).
    pub dc_offset: f64,
    pub dc_offset_db: f64,
    /// Peak minus RMS (dB).
    pub crest_factor_db: f64,
    /// Spread of 50 ms RMS windows: 95th minus 10th percentile of non-silent windows (dB).
    pub dynamic_range_db: f64,
    /// Quietest non-silent 50 ms RMS window (dBFS).
    pub min_rms_db: f64,
    /// Loudest 50 ms RMS window (dBFS).
    pub max_rms_db: f64,
    /// Samples at or above −0.001 dBFS.
    pub full_scale_samples: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub histogram: Option<Histogram>,
}

/// Statistics over all channels together.
#[derive(Clone, Debug, Serialize)]
pub struct OverallStats {
    pub peak_db: f64,
    pub true_peak_dbtp: f64,
    pub rms_db: f64,
    pub crest_factor_db: f64,
    pub dynamic_range_db: f64,
}

/// Result of [`stats`].
#[derive(Clone, Debug, Serialize)]
pub struct StatsReport {
    pub sample_rate: u32,
    pub channels: usize,
    pub frames: usize,
    pub duration_s: f64,
    pub overall: OverallStats,
    pub per_channel: Vec<ChannelStats>,
}

fn window_rms_db(x: &[f32], win: usize) -> Vec<f64> {
    x.chunks(win.max(1))
        .filter(|c| c.len() * 2 >= win)
        .map(|c| pow_db(c.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / c.len() as f64))
        .collect()
}

fn dynamic_range(levels: &[f64]) -> (f64, f64, f64) {
    let active: Vec<f64> = levels.iter().copied().filter(|l| *l > -90.0).collect();
    if active.is_empty() {
        return (0.0, -240.0, -240.0);
    }
    let min = active.iter().copied().fold(f64::INFINITY, f64::min);
    let max = active.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    (
        util::percentile(&active, 95.0) - util::percentile(&active, 10.0),
        min,
        max,
    )
}

/// Build a 61-bin dB histogram (−60..0 dBFS in 1 dB steps).
pub fn histogram(x: &[f32]) -> Histogram {
    let mut counts = vec![0u64; 61];
    for v in x {
        let a = v.abs() as f64;
        let idx = if a <= 1e-3 {
            0
        } else {
            let db = 20.0 * a.log10();
            ((db + 60.0).ceil().clamp(0.0, 60.0)) as usize
        };
        counts[idx] += 1;
    }
    Histogram {
        bins_db: (0..61).map(|i| -60.0 + i as f64).collect(),
        counts,
    }
}

/// Per-channel and overall amplitude statistics.
pub fn stats(buf: &AudioBuffer, with_histogram: bool) -> StatsReport {
    let sr = buf.sample_rate;
    let win = ((sr as f64 * 0.05).round() as usize).max(1);
    let per_channel: Vec<ChannelStats> = buf
        .data
        .iter()
        .enumerate()
        .map(|(c, ch)| {
            let n = ch.len().max(1) as f64;
            let peak = ch.iter().fold(0.0f32, |m, v| m.max(v.abs())) as f64;
            let ms = ch.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / n;
            let dc = ch.iter().map(|v| *v as f64).sum::<f64>() / n;
            let tp = crate::loudness::true_peak_channel(ch, sr);
            let (dr, mn, mx) = dynamic_range(&window_rms_db(ch, win));
            let rms_db = pow_db(ms);
            ChannelStats {
                channel: c,
                peak_db: amp_db(peak),
                true_peak_dbtp: amp_db(tp),
                rms_db,
                dc_offset: dc,
                dc_offset_db: amp_db(dc.abs()),
                crest_factor_db: if ms > 0.0 { amp_db(peak) - rms_db } else { 0.0 },
                dynamic_range_db: dr,
                min_rms_db: mn,
                max_rms_db: mx,
                full_scale_samples: ch.iter().filter(|v| v.abs() >= 0.99988).count(),
                histogram: if with_histogram { Some(histogram(ch)) } else { None },
            }
        })
        .collect();
    let peak = buf.peak() as f64;
    let rms = buf.rms() as f64;
    let mono = buf.mono_mix();
    let (dr, _, _) = dynamic_range(&window_rms_db(&mono, win));
    let overall = OverallStats {
        peak_db: amp_db(peak),
        true_peak_dbtp: per_channel
            .iter()
            .map(|c| c.true_peak_dbtp)
            .fold(-240.0, f64::max),
        rms_db: amp_db(rms),
        crest_factor_db: if rms > 0.0 { amp_db(peak) - amp_db(rms) } else { 0.0 },
        dynamic_range_db: dr,
    };
    StatsReport {
        sample_rate: sr,
        channels: buf.channels(),
        frames: buf.frames(),
        duration_s: buf.duration_s(),
        overall,
        per_channel,
    }
}

/// Crest factor of the whole buffer: peak minus RMS over all channels (dB).
pub fn crest_factor_db(buf: &AudioBuffer) -> f64 {
    let rms = buf.rms() as f64;
    if rms <= 0.0 {
        return 0.0;
    }
    amp_db(buf.peak() as f64) - amp_db(rms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferraus_core::testing::{noise, sine, stereo};

    #[test]
    fn silence_regions_and_edges() {
        let sr = 48000;
        let mut b = AudioBuffer::new(sr, 1, sr as usize); // 1 s silence
        b.append(&sine(sr, 440.0, 1.0, 0.5));
        b.append(&AudioBuffer::new(sr, 1, (sr / 5) as usize)); // 200 ms gap
        b.append(&sine(sr, 440.0, 1.0, 0.5));
        b.append(&AudioBuffer::new(sr, 1, sr as usize / 2));
        let r = detect_silence(&b, -40.0, 0.3);
        assert_eq!(r.regions.len(), 2, "{:?}", r.regions);
        assert!((r.regions[0].start_s - 0.0).abs() < 0.011);
        assert!((r.regions[0].end_s - 1.0).abs() < 0.011);
        assert!((r.head_silence_s - 1.0).abs() < 0.011);
        assert!((r.tail_silence_s - 0.5).abs() < 0.011);
        assert_eq!(r.regions[0].floor_db, -240.0);
        let r = detect_silence(&b, -40.0, 0.1);
        assert_eq!(r.regions.len(), 3);
    }

    #[test]
    fn clipping_runs() {
        let mut b = sine(48000, 100.0, 0.1, 1.4);
        for v in b.data[0].iter_mut() {
            *v = v.clamp(-1.0, 1.0);
        }
        let r = detect_clipping(&b, 0.0, 3);
        assert_eq!(r.count, 20, "{}", r.count); // 10 cycles x 2 polarities
        assert!(r.runs.iter().all(|x| x.kind == "threshold"));
        // flat-topped at 0.5 (clipped then gained down)
        let mut b = sine(48000, 100.0, 0.1, 0.8);
        for v in b.data[0].iter_mut() {
            *v = v.clamp(-0.5, 0.5);
        }
        let r = detect_clipping(&b, 0.0, 3);
        assert_eq!(r.count, 20);
        assert!(r.runs.iter().all(|x| x.kind == "flat"));
        let clean = sine(48000, 1000.0, 0.5, 1.0);
        assert_eq!(detect_clipping(&clean, 0.0, 3).count, 0);
    }

    #[test]
    fn stats_on_sine_and_noise() {
        let b = stereo(sine(48000, 1000.0, 1.0, 0.5));
        let r = stats(&b, true);
        let c = &r.per_channel[0];
        assert!((c.peak_db + 6.02).abs() < 0.01);
        assert!((c.rms_db + 9.03).abs() < 0.01);
        assert!((c.crest_factor_db - 3.01).abs() < 0.02);
        assert!(c.dynamic_range_db < 0.1);
        let h = c.histogram.as_ref().unwrap();
        assert_eq!(h.counts.len(), 61);
        assert_eq!(h.counts.iter().sum::<u64>(), 48000);
        assert!(h.counts[54] > 0 && h.counts[55] == 0, "{:?}", &h.counts[50..]);
        let mut d = noise(48000, 1.0, 0.1, 1);
        d.data[0].iter_mut().for_each(|v| *v += 0.05);
        let r = stats(&d, false);
        assert!((r.per_channel[0].dc_offset - 0.05).abs() < 0.002);
        assert!(r.per_channel[0].histogram.is_none());
    }
}
