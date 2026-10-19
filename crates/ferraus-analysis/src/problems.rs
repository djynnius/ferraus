//! Diagnostics (`analyze problems`): each detector returns a [`Problem`] with a severity,
//! a plain-language explanation, and a suggested fix that is a runnable command or `fx`
//! stage string.

use crate::levels::{detect_clipping, detect_silence};
use crate::loudness::{self, loudness_outliers};
use crate::spectrum::{band_power_fraction, ltas};
use crate::stereo;
use crate::util::{self, amp_db, pow_db};
use ferraus_core::AudioBuffer;
use serde::Serialize;

/// How much a problem matters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Low,
    Medium,
    High,
}

/// One diagnosed problem.
#[derive(Clone, Debug, Serialize)]
pub struct Problem {
    /// Machine-readable code: `clipping`, `dc_offset`, `clicks`, `hum`, `hiss`,
    /// `noise_floor`, `phase`, `out_of_phase_bass`, `silence_head`, `silence_tail`,
    /// `silent`, `loudness_outliers`, `inter_sample_peaks`, `rumble`, `sibilance`.
    pub problem: String,
    pub severity: Severity,
    pub details: String,
    /// A command (`silence trim --edges`) or fx stage (`highpass:freq=30Hz`); empty when
    /// there is no automatic fix.
    pub suggested_fix: String,
}

/// Count of problems per severity.
#[derive(Clone, Debug, Default, Serialize)]
pub struct SeverityCounts {
    pub high: usize,
    pub medium: usize,
    pub low: usize,
}

/// Result of [`diagnose`].
#[derive(Clone, Debug, Serialize)]
pub struct ProblemsReport {
    /// No medium or high severity problems.
    pub clean: bool,
    pub counts: SeverityCounts,
    /// Sorted by severity, highest first.
    pub problems: Vec<Problem>,
}

fn problem(code: &str, severity: Severity, details: String, fix: &str) -> Problem {
    Problem { problem: code.into(), severity, details, suggested_fix: fix.into() }
}

fn fmt_times(t: &[f64]) -> String {
    let shown: Vec<String> = t.iter().take(5).map(|v| format!("{v:.2}s")).collect();
    let more = if t.len() > 5 { format!(" (+{} more)", t.len() - 5) } else { String::new() };
    format!("{}{more}", shown.join(", "))
}

/// Mean value of each channel.
pub fn dc_offsets(buf: &AudioBuffer) -> Vec<f64> {
    buf.data.iter().map(|c| c.iter().map(|v| *v as f64).sum::<f64>() / c.len().max(1) as f64).collect()
}

/// Times (seconds) of isolated clicks: samples whose second difference exceeds ten times
/// the local RMS of the second difference (±128 samples) and an absolute floor.
pub fn detect_clicks(buf: &AudioBuffer) -> Vec<f64> {
    let sr = buf.sample_rate as f64;
    let merge = (sr * 0.002) as usize;
    let mut times: Vec<usize> = Vec::new();
    for ch in &buf.data {
        let n = ch.len();
        if n < 300 {
            continue;
        }
        // second difference, computed on demand (no full-length buffers)
        let e = |i: usize| -> f64 {
            if i < 2 {
                0.0
            } else {
                ch[i] as f64 - 2.0 * ch[i - 1] as f64 + ch[i - 2] as f64
            }
        };
        // running energy of e over the window [i-128, i+128]
        let mut sum: f64 = (0..129.min(n)).map(|j| e(j).powi(2)).sum();
        let mut last: Option<usize> = None;
        for i in 0..n {
            if i > 0 {
                if i + 128 < n {
                    sum += e(i + 128).powi(2);
                }
                if i > 128 {
                    sum -= e(i - 129).powi(2);
                }
            }
            if i < 2 {
                continue;
            }
            let a = i.saturating_sub(128);
            let b = (i + 129).min(n);
            let rms = (sum.max(0.0) / (b - a) as f64).sqrt();
            let ei = e(i);
            if ei.abs() > (10.0 * rms).max(0.01) {
                if let Some(l) = last {
                    if i - l <= merge {
                        last = Some(i);
                        continue;
                    }
                }
                times.push(i);
                last = Some(i);
            }
        }
    }
    times.sort_unstable();
    times.dedup_by(|a, b| a.abs_diff(*b) <= merge);
    times.into_iter().map(|i| i as f64 / sr).collect()
}

/// Mains hum found by [`detect_hum`].
#[derive(Clone, Debug, Serialize)]
pub struct HumInfo {
    /// 50 or 60 Hz.
    pub freq_hz: f64,
    /// Harmonics (1 = fundamental) that stand out from their neighbourhood.
    pub harmonics: Vec<usize>,
    /// Hum power relative to the total signal power (dB).
    pub relative_db: f64,
    /// Hum level (dBFS, sine-referenced).
    pub level_db: f64,
}

/// Look for narrow peaks at 50 or 60 Hz and their first 8 harmonics.
pub fn detect_hum(buf: &AudioBuffer) -> Option<HumInfo> {
    let sr = buf.sample_rate as f64;
    let n_fft = util::next_pow2((sr / 1.5) as usize).min(util::next_pow2(buf.frames()) / 2).max(4096);
    if buf.frames() < n_fft / 2 {
        return None;
    }
    let l = ltas(buf, n_fft);
    let df = l.bin_hz;
    let total: f64 = l.power.iter().skip(1).sum();
    if total <= 1e-20 {
        return None;
    }
    let mut best: Option<HumInfo> = None;
    for base in [50.0, 60.0] {
        let mut harmonics = Vec::new();
        let mut hum_power = 0.0;
        for h in 1..=8usize {
            let f = base * h as f64;
            if f > sr / 2.0 - 20.0 {
                break;
            }
            let k0 = ((f - 1.5) / df).floor().max(1.0) as usize;
            let k1 = ((f + 1.5) / df).ceil() as usize;
            let (kp, peak) = (k0..=k1).fold((k0, 0.0f64), |m, k| if l.power[k] > m.1 { (k, l.power[k]) } else { m });
            let mut bg: Vec<f64> = Vec::new();
            let lo = ((f - 15.0) / df).floor().max(1.0) as usize;
            let hi = (((f + 15.0) / df).ceil() as usize).min(l.power.len() - 1);
            for k in lo..=hi {
                if (k as f64 * df - f).abs() > 4.0 {
                    bg.push(l.power[k]);
                }
            }
            let background = util::median(&bg).max(1e-30);
            // power of the peak including the Hann main lobe
            let lobe: f64 = (kp.saturating_sub(2)..=(kp + 2).min(l.power.len() - 1)).map(|k| l.power[k]).sum();
            let prominence = pow_db(peak / background);
            if prominence > 15.0 && pow_db(lobe) > -100.0 {
                harmonics.push(h);
                hum_power += lobe;
            }
        }
        let strong_fundamental = harmonics.contains(&1) || harmonics.contains(&2);
        if harmonics.len() >= 2 || (strong_fundamental && !harmonics.is_empty() && hum_power / total > 1e-4) {
            let info = HumInfo {
                freq_hz: base,
                relative_db: pow_db(hum_power / total),
                level_db: pow_db(hum_power) + 3.0103,
                harmonics,
            };
            if best.as_ref().map(|b| info.relative_db > b.relative_db).unwrap_or(true) {
                best = Some(info);
            }
        }
    }
    best
}

/// Quiet-passage noise floor estimates.
#[derive(Clone, Debug, Serialize)]
pub struct NoiseFloor {
    /// High band (6–16 kHz) level in the quietest 10% of frames (dBFS).
    pub hiss_db: f64,
    /// Spectral flatness of that band in those frames (1 = noise-like).
    pub hiss_flatness: f64,
    /// Broadband level of the quietest 10% of frames (dBFS).
    pub floor_db: f64,
    /// Broadband spectral flatness of those frames.
    pub floor_flatness: f64,
    /// Level of the loudest 5% of frames (dBFS).
    pub loud_db: f64,
}

/// Estimate hiss and broadband noise floor from the quietest frames.
pub fn noise_floor(buf: &AudioBuffer) -> NoiseFloor {
    let sr = buf.sample_rate as f64;
    let x = buf.mono_mix();
    let n_fft = 2048usize;
    let frames = util::stft_mag(&x, n_fft, n_fft / 2);
    let df = sr / n_fft as f64;
    let wss = 3.0 * n_fft as f64 / 8.0;
    let scale = 2.0 / (n_fft as f64 * wss);
    let k6 = (6000.0 / df) as usize;
    let k16 = ((16000.0f64.min(sr / 2.0 * 0.95)) / df) as usize;
    let mut rows: Vec<(f64, f64, f64, f64)> = frames
        .iter()
        .skip(1)
        .take(frames.len().saturating_sub(2))
        .map(|f| {
            let p: Vec<f64> = f.iter().map(|m| (*m as f64).powi(2) * scale).collect();
            let total: f64 = p[1..].iter().sum();
            let flat = |s: &[f64]| -> f64 {
                if s.is_empty() {
                    return 0.0;
                }
                let am = s.iter().sum::<f64>() / s.len() as f64;
                if am <= 1e-30 {
                    return 0.0;
                }
                let gm = (s.iter().map(|v| (v + 1e-30).ln()).sum::<f64>() / s.len() as f64).exp();
                (gm / am).clamp(0.0, 1.0)
            };
            let (hi, hflat) =
                if k16 > k6 + 4 { (p[k6..k16].iter().sum::<f64>(), flat(&p[k6..k16])) } else { (0.0, 0.0) };
            (total, flat(&p[1..]), hi, hflat)
        })
        .collect();
    if rows.is_empty() {
        return NoiseFloor {
            hiss_db: -240.0,
            hiss_flatness: 0.0,
            floor_db: -240.0,
            floor_flatness: 0.0,
            loud_db: -240.0,
        };
    }
    rows.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let q = (rows.len() / 10).max(1);
    let quiet = &rows[..q];
    let loud = &rows[rows.len() - (rows.len() / 20).max(1)..];
    let med = |v: Vec<f64>| util::median(&v);
    NoiseFloor {
        hiss_db: pow_db(med(quiet.iter().map(|r| r.2).collect())),
        hiss_flatness: med(quiet.iter().map(|r| r.3).collect()),
        floor_db: pow_db(med(quiet.iter().map(|r| r.0).collect())),
        floor_flatness: med(quiet.iter().map(|r| r.1).collect()),
        loud_db: pow_db(med(loud.iter().map(|r| r.0).collect())),
    }
}

/// Sibilance estimate: share of loud frames dominated by 5–10 kHz energy.
#[derive(Clone, Debug, Serialize)]
pub struct Sibilance {
    /// Fraction of loud frames that are sibilant.
    pub sibilant_fraction: f64,
    /// Loudest 5–10 kHz level in a sibilant frame (dBFS).
    pub peak_db: f64,
}

/// Measure sibilance (requires a sample rate of at least 22.05 kHz).
pub fn sibilance(buf: &AudioBuffer) -> Sibilance {
    let sr = buf.sample_rate as f64;
    let none = Sibilance { sibilant_fraction: 0.0, peak_db: -240.0 };
    if sr < 22050.0 {
        return none;
    }
    let x = buf.mono_mix();
    let n_fft = 1024usize;
    let frames = util::stft_mag(&x, n_fft, n_fft / 2);
    let df = sr / n_fft as f64;
    let scale = 2.0 / (n_fft as f64 * (3.0 * n_fft as f64 / 8.0));
    let (k5, k10) = ((5000.0 / df) as usize, ((10000.0f64.min(sr / 2.0)) / df) as usize);
    let rows: Vec<(f64, f64)> = frames
        .iter()
        .map(|f| {
            let p: Vec<f64> = f.iter().map(|m| (*m as f64).powi(2) * scale).collect();
            (p[1..].iter().sum(), p[k5..k10].iter().sum())
        })
        .collect();
    let max = rows.iter().map(|r| r.0).fold(0.0, f64::max);
    if max <= 0.0 {
        return none;
    }
    let loud: Vec<&(f64, f64)> = rows.iter().filter(|r| pow_db(r.0) > (pow_db(max) - 30.0).max(-45.0)).collect();
    if loud.is_empty() {
        return none;
    }
    let sib: Vec<&&(f64, f64)> = loud.iter().filter(|r| r.1 > 0.5 * r.0).collect();
    Sibilance {
        sibilant_fraction: sib.len() as f64 / loud.len() as f64,
        peak_db: sib.iter().map(|r| pow_db(r.1) + 3.0103).fold(-240.0, f64::max),
    }
}

/// Run every detector and collect the problems found.
pub fn diagnose(buf: &AudioBuffer) -> ProblemsReport {
    let mut out: Vec<Problem> = Vec::new();
    let dur = buf.duration_s();
    let sp = buf.peak() as f64;
    if buf.is_empty() || sp == 0.0 {
        out.push(problem("silent", Severity::High, "the file contains only digital silence".into(), ""));
        return finish(out);
    }

    // clipping
    let clip = detect_clipping(buf, 0.0, 3);
    if clip.count > 0 {
        let sev = if clip.clipped_fraction >= 1e-3 || clip.count > 50 {
            Severity::High
        } else if clip.count >= 5 {
            Severity::Medium
        } else {
            Severity::Low
        };
        let times: Vec<f64> = clip.runs.iter().map(|r| r.start_s).collect();
        out.push(problem(
            "clipping",
            sev,
            format!(
                "{} clipped run(s), {} samples ({:.3}%), at {}",
                clip.count,
                clip.clipped_samples,
                clip.clipped_fraction * 100.0,
                fmt_times(&times)
            ),
            "declip",
        ));
    }

    // DC offset
    let dcs = dc_offsets(buf);
    let worst = dcs.iter().fold(0.0f64, |m, v| m.max(v.abs()));
    if worst >= 0.001 {
        let sev = if worst >= 0.02 {
            Severity::High
        } else if worst >= 0.005 {
            Severity::Medium
        } else {
            Severity::Low
        };
        let per: Vec<String> = dcs.iter().enumerate().map(|(c, d)| format!("ch{} {:+.4}", c + 1, d)).collect();
        out.push(problem(
            "dc_offset",
            sev,
            format!("DC offset up to {:.4} ({:.1} dBFS): {}", worst, amp_db(worst), per.join(", ")),
            "dc-remove",
        ));
    }

    // inter-sample peaks
    let tp = loudness::true_peak(buf);
    let (tp_db, sp_db) = (amp_db(tp), amp_db(sp));
    if tp_db > -1.0 && tp_db > sp_db + 0.3 {
        let sev = if tp_db > 0.0 {
            Severity::High
        } else if tp_db > -0.5 {
            Severity::Medium
        } else {
            Severity::Low
        };
        out.push(problem(
            "inter_sample_peaks",
            sev,
            format!("true peak {tp_db:.2} dBTP is above the sample peak {sp_db:.2} dBFS"),
            "limit:ceiling=-1dBTP",
        ));
    }

    // clicks
    let clicks = detect_clicks(buf);
    if !clicks.is_empty() {
        let rate = clicks.len() as f64 / (dur / 60.0).max(1e-9);
        let sev = if clicks.len() >= 20 || (rate > 10.0 && clicks.len() >= 5) {
            Severity::High
        } else if clicks.len() >= 3 {
            Severity::Medium
        } else {
            Severity::Low
        };
        out.push(problem("clicks", sev, format!("{} click(s) at {}", clicks.len(), fmt_times(&clicks)), "declick"));
    }

    // hum
    if let Some(h) = detect_hum(buf) {
        let sev = if h.relative_db > -20.0 {
            Severity::High
        } else if h.relative_db > -40.0 {
            Severity::Medium
        } else {
            Severity::Low
        };
        out.push(problem(
            "hum",
            sev,
            format!(
                "{:.0} Hz hum (harmonics {:?}) at {:.1} dB relative to the signal",
                h.freq_hz, h.harmonics, h.relative_db
            ),
            if h.freq_hz == 50.0 { "dehum:freq=50Hz" } else { "dehum:freq=60Hz" },
        ));
    }

    // hiss and noise floor
    let nf = noise_floor(buf);
    if nf.hiss_db > -72.0 && nf.hiss_flatness > 0.3 {
        let sev = if nf.hiss_db > -45.0 {
            Severity::High
        } else if nf.hiss_db > -58.0 {
            Severity::Medium
        } else {
            Severity::Low
        };
        out.push(problem(
            "hiss",
            sev,
            format!("high-frequency noise floor of {:.1} dBFS (6–16 kHz) in quiet passages", nf.hiss_db),
            "dehiss",
        ));
    }
    if nf.floor_db > -50.0 && nf.floor_flatness > 0.4 && nf.loud_db - nf.floor_db < 30.0 {
        out.push(problem(
            "noise_floor",
            if nf.loud_db - nf.floor_db < 15.0 { Severity::High } else { Severity::Medium },
            format!(
                "broadband noise floor of {:.1} dBFS, only {:.1} dB below the loud passages",
                nf.floor_db,
                nf.loud_db - nf.floor_db
            ),
            "denoise:adaptive",
        ));
    }

    // phase
    if buf.channels() >= 2 {
        let st = stereo::stereo_analysis(buf);
        if st.correlation < -0.3 {
            out.push(problem(
                "phase",
                Severity::High,
                format!(
                    "left and right are largely out of phase (correlation {:.2}); the mono sum cancels",
                    st.correlation
                ),
                "channels mix --invert R",
            ));
        } else if st.correlation < 0.1 && st.width > 0.45 {
            out.push(problem(
                "phase",
                Severity::Medium,
                format!("weak L/R correlation ({:.2}); mono playback loses {:.1} dB", st.correlation, -st.mono_loss_db),
                "phase-align",
            ));
        }
        let l = ltas(buf, util::next_pow2((buf.sample_rate / 4) as usize).max(2048));
        let low_share = band_power_fraction(&l, 20.0, 120.0);
        let cl = st.correlation_below_120hz;
        if low_share > 0.01 && cl < 0.8 {
            let sev = if cl < 0.0 {
                Severity::High
            } else if cl < 0.5 {
                Severity::Medium
            } else {
                Severity::Low
            };
            out.push(problem(
                "out_of_phase_bass",
                sev,
                format!("L/R correlation below 120 Hz is {cl:.2}; bass will thin out or cancel in mono"),
                "widen:mono-below=120Hz",
            ));
        }
    }

    // silence at the edges
    let sil = detect_silence(buf, -60.0, 0.1);
    if sil.head_silence_s > 0.5 {
        out.push(problem(
            "silence_head",
            if sil.head_silence_s >= 2.0 { Severity::Medium } else { Severity::Low },
            format!("{:.2} s of silence (below -60 dBFS) at the start", sil.head_silence_s),
            "silence trim --edges",
        ));
    }
    if sil.tail_silence_s > 1.0 {
        out.push(problem(
            "silence_tail",
            if sil.tail_silence_s >= 3.0 { Severity::Medium } else { Severity::Low },
            format!("{:.2} s of silence (below -60 dBFS) at the end", sil.tail_silence_s),
            "silence trim --edges",
        ));
    }

    // loudness outliers
    if dur >= 20.0 {
        let o = loudness_outliers(buf, 8.0);
        if !o.is_empty() {
            let worst = o.iter().fold(0.0f64, |m, s| m.max(s.delta_lu.abs()));
            let desc: Vec<String> =
                o.iter().take(5).map(|s| format!("{:.1}-{:.1}s {:+.1} LU", s.start_s, s.end_s, s.delta_lu)).collect();
            out.push(problem(
                "loudness_outliers",
                if worst > 12.0 { Severity::Medium } else { Severity::Low },
                format!("sections far from the integrated loudness: {}", desc.join(", ")),
                "leveler",
            ));
        }
    }

    // sub-30 Hz rumble
    let lr = ltas(buf, util::next_pow2((buf.sample_rate / 2) as usize).max(4096));
    let frac = band_power_fraction(&lr, 8.0, 30.0);
    let rel = pow_db(frac);
    let rumble_level = pow_db(frac * lr.power.iter().skip(1).sum::<f64>());
    if rel > -30.0 && rumble_level > -70.0 {
        out.push(problem(
            "rumble",
            if rel > -10.0 {
                Severity::High
            } else if rel > -20.0 {
                Severity::Medium
            } else {
                Severity::Low
            },
            format!("energy below 30 Hz is {rel:.1} dB relative to the whole signal"),
            "highpass:freq=30Hz",
        ));
    }

    // sibilance
    let s = sibilance(buf);
    if s.sibilant_fraction >= 0.02 && s.sibilant_fraction <= 0.3 && s.peak_db > -25.0 {
        out.push(problem(
            "sibilance",
            if s.sibilant_fraction > 0.1 && s.peak_db > -15.0 { Severity::Medium } else { Severity::Low },
            format!(
                "{:.0}% of loud frames are dominated by 5-10 kHz energy (peak {:.1} dBFS)",
                s.sibilant_fraction * 100.0,
                s.peak_db
            ),
            "deess",
        ));
    }
    finish(out)
}

fn finish(mut out: Vec<Problem>) -> ProblemsReport {
    out.sort_by_key(|p| std::cmp::Reverse(p.severity));
    let mut counts = SeverityCounts::default();
    for p in &out {
        match p.severity {
            Severity::High => counts.high += 1,
            Severity::Medium => counts.medium += 1,
            Severity::Low => counts.low += 1,
        }
    }
    ProblemsReport { clean: counts.high == 0 && counts.medium == 0, counts, problems: out }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferraus_core::testing::{add, noise, sine, stereo};

    fn has(r: &ProblemsReport, code: &str) -> bool {
        r.problems.iter().any(|p| p.problem == code)
    }

    #[test]
    fn clean_sine_has_no_serious_problems() {
        for f in [440.0, 1000.0] {
            let b = stereo(sine(48000, f, 4.0, 0.5));
            let r = diagnose(&b);
            assert!(r.problems.iter().all(|p| p.severity != Severity::High), "{r:?}");
            assert!(r.clean, "{r:?}");
        }
    }

    #[test]
    fn detects_clipping_and_dc() {
        let mut b = sine(48000, 200.0, 2.0, 1.6);
        b.data[0].iter_mut().for_each(|v| *v = v.clamp(-1.0, 1.0));
        let r = diagnose(&b);
        assert!(has(&r, "clipping"), "{r:?}");
        assert_eq!(r.problems[0].suggested_fix, "declip");
        let mut d = stereo(sine(48000, 440.0, 2.0, 0.4));
        d.data.iter_mut().for_each(|c| c.iter_mut().for_each(|v| *v += 0.05));
        let r = diagnose(&d);
        let p = r.problems.iter().find(|p| p.problem == "dc_offset").expect("dc");
        assert_eq!(p.severity, Severity::High);
        assert_eq!(p.suggested_fix, "dc-remove");
    }

    #[test]
    fn detects_hum() {
        let mut hum = sine(48000, 60.0, 3.0, 0.03);
        for h in 2..=4 {
            hum = add(&hum, &sine(48000, 60.0 * h as f64, 3.0, 0.03 / h as f32));
        }
        let b = add(&sine(48000, 440.0, 3.0, 0.3), &hum);
        let r = diagnose(&b);
        let p = r.problems.iter().find(|p| p.problem == "hum").expect("hum");
        assert_eq!(p.suggested_fix, "dehum:freq=60Hz");
        // a 50 Hz fundamental alone
        let b = add(&sine(48000, 1000.0, 3.0, 0.3), &sine(48000, 50.0, 3.0, 0.05));
        let r = diagnose(&b);
        assert!(r.problems.iter().any(|p| p.suggested_fix == "dehum:freq=50Hz"), "{r:?}");
    }

    #[test]
    fn detects_head_silence() {
        let mut b = AudioBuffer::new(48000, 2, 48000 * 3);
        b.append(&stereo(sine(48000, 440.0, 2.0, 0.5)));
        let r = diagnose(&b);
        let p = r.problems.iter().find(|p| p.problem == "silence_head").expect("head");
        assert_eq!(p.severity, Severity::Medium);
        assert_eq!(p.suggested_fix, "silence trim --edges");
    }

    #[test]
    fn detects_out_of_phase_bass() {
        let bass = sine(48000, 60.0, 3.0, 0.4).data[0].clone();
        let hi = sine(48000, 1500.0, 3.0, 0.3).data[0].clone();
        let l: Vec<f32> = bass.iter().zip(&hi).map(|(b, h)| b + h).collect();
        let r: Vec<f32> = bass.iter().zip(&hi).map(|(b, h)| -b + h).collect();
        let rep = diagnose(&AudioBuffer::from_planar(48000, vec![l, r]));
        let p = rep.problems.iter().find(|p| p.problem == "out_of_phase_bass").expect("bass");
        assert_eq!(p.severity, Severity::High);
        assert_eq!(p.suggested_fix, "widen:mono-below=120Hz");
    }

    #[test]
    fn detects_clicks_hiss_rumble_and_silence() {
        let mut b = sine(48000, 440.0, 3.0, 0.3);
        for t in [0.5, 1.2, 2.1, 2.7] {
            b.data[0][(t * 48000.0) as usize] += 0.6;
        }
        let r = diagnose(&b);
        let p = r.problems.iter().find(|p| p.problem == "clicks").expect("clicks");
        assert!(p.details.starts_with("4 click"), "{}", p.details);
        let h = add(&sine(48000, 440.0, 3.0, 0.3), &noise(48000, 3.0, 0.02, 3));
        assert!(has(&diagnose(&h), "hiss"));
        let rb = add(&sine(48000, 440.0, 3.0, 0.2), &sine(48000, 18.0, 3.0, 0.3));
        assert!(has(&diagnose(&rb), "rumble"));
        let s = diagnose(&AudioBuffer::new(48000, 2, 4800));
        assert!(has(&s, "silent"));
        assert!(!s.clean);
    }

    #[test]
    fn inter_sample_peaks() {
        let x: Vec<f32> = (0..48000)
            .map(|i| (0.99 * (std::f64::consts::PI / 2.0 * i as f64 + std::f64::consts::PI / 4.0).sin()) as f32)
            .collect();
        let r = diagnose(&AudioBuffer::from_mono(48000, x));
        let p = r.problems.iter().find(|p| p.problem == "inter_sample_peaks").expect("isp");
        assert_eq!(p.severity, Severity::Medium);
    }
}
