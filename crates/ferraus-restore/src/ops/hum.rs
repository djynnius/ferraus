//! `dehum` (tracking harmonic notches with mains detection) and `notch`.

use crate::dsp::biquad::{butterworth, Biquad, Coefs};
use crate::dsp::util::{goertzel_amp, median};
use ferraus_core::prelude::*;
use std::path::PathBuf;

pub fn ops() -> Vec<OpSpec> {
    vec![
        OpSpec::transform(
            "dehum",
            Category::Restoration,
            "Remove mains hum (50/60 Hz) and its harmonics",
            dehum,
        )
        .about(
            "Detects the mains frequency (50 or 60 Hz) from the spectrum, measures it \
             precisely, follows its slow drift, and removes it with a cascade of narrow \
             notches at the fundamental and its harmonics. --depth is the attenuation floor: \
             each notch centre is cut 6 dB deeper than --depth so small frequency drift stays \
             below it. --slope makes each higher harmonic's notch that much deeper (negative) \
             or shallower (positive). --residual writes the removed hum to a file.",
        )
        .param(
            param("freq", ParamKind::Str)
                .default("auto")
                .help("Hum fundamental in Hz (50Hz, 60Hz, 59.94Hz) or 'auto' to detect 50 or 60 Hz"),
        )
        .param(
            param("harmonics", ParamKind::Int)
                .default("8")
                .range("1..32")
                .help("Number of harmonics to notch, counting the fundamental"),
        )
        .param(
            param("q", ParamKind::Float)
                .default("30")
                .help("Notch quality factor (centre frequency / bandwidth); higher is narrower"),
        )
        .param(
            param("depth", ParamKind::Gain)
                .default("-40dB")
                .help("Minimum attenuation of each hum line in dB"),
        )
        .param(
            param("slope", ParamKind::Gain)
                .default("-3dB")
                .help("Depth change per harmonic in dB (negative cuts higher harmonics deeper)"),
        )
        .param(
            param("track", ParamKind::Switch)
                .default("on")
                .help("Follow slow drift of the mains frequency over time (on|off)"),
        )
        .param(
            param("residual", ParamKind::Path)
                .help("Also write the removed hum to this file"),
        )
        .example("ferraus dehum interview.wav -o clean.wav", "Detect and remove hum automatically")
        .example(
            "ferraus dehum gtr.wav --freq 50Hz --harmonics 12 --residual hum.wav -o clean.wav",
            "Remove 50 Hz hum and 12 harmonics and keep what was removed",
        ),
        OpSpec::transform(
            "notch",
            Category::Tone,
            "Narrow notch filter, optionally with harmonics",
            notch,
        )
        .about(
            "Cuts a narrow band around --freq by exactly --depth at the centre (and returns to \
             unity away from it). --harmonics adds that many notches at integer multiples of \
             the frequency. `dehum` is the tuned, self-detecting version for mains hum.",
        )
        .param(
            param("freq", ParamKind::Freq)
                .default("60Hz")
                .help("Notch centre frequency (Hz, kHz, or a note name)"),
        )
        .param(
            param("q", ParamKind::Float)
                .default("30")
                .help("Quality factor (centre / bandwidth); higher is narrower"),
        )
        .param(
            param("depth", ParamKind::Gain)
                .default("-40dB")
                .help("Attenuation at the centre in dB"),
        )
        .param(
            param("harmonics", ParamKind::Int)
                .default("0")
                .range("0..64")
                .help("Additional notches at 2x, 3x, ... the frequency"),
        )
        .example("ferraus notch in.wav --freq 1kHz -o out.wav", "Remove a 1 kHz whine")
        .example(
            "ferraus notch in.wav --freq 60Hz --harmonics 4 --depth -60dB -o out.wav",
            "Deep notches at 60, 120, 180, 240, and 300 Hz",
        ),
    ]
}

/// Decimate for low-frequency analysis: returns the signal and its new rate.
fn decimate(x: &[f64], sr: f64, max_f: f64) -> (Vec<f64>, f64) {
    let target = (max_f * 2.8).max(300.0);
    let factor = (sr / target).floor().max(1.0) as usize;
    if factor <= 1 {
        return (x.to_vec(), sr);
    }
    let new_sr = sr / factor as f64;
    let lp = butterworth(8, 0.42 * new_sr, sr, false);
    let mut f = lp.filter();
    let mut out = Vec::with_capacity(x.len() / factor + 1);
    for (i, v) in x.iter().enumerate() {
        let y = f.tick(*v);
        if i % factor == 0 {
            out.push(y);
        }
    }
    (out, new_sr)
}

/// Summed harmonic power of a hum candidate.
fn hum_score(x: &[f64], sr: f64, f0: f64, nh: usize) -> f64 {
    (1..=nh)
        .map(|k| k as f64 * f0)
        .filter(|f| *f < sr * 0.45)
        .map(|f| goertzel_amp(x, sr, f).powi(2))
        .sum()
}

/// Locate the hum fundamental near `guess` (grid then golden-section search).
fn refine(x: &[f64], sr: f64, guess: f64, span: f64, nh: usize) -> (f64, f64) {
    let steps = 24;
    let mut best = (guess, -1.0);
    for i in 0..=steps {
        let f = guess - span + 2.0 * span * i as f64 / steps as f64;
        let s = hum_score(x, sr, f, nh);
        if s > best.1 {
            best = (f, s);
        }
    }
    let step = 2.0 * span / steps as f64;
    let (mut a, mut b) = (best.0 - step, best.0 + step);
    let g = 0.618_033_988_749_895;
    let mut c = b - g * (b - a);
    let mut d = a + g * (b - a);
    let mut fc = hum_score(x, sr, c, nh);
    let mut fd = hum_score(x, sr, d, nh);
    for _ in 0..40 {
        if fc > fd {
            b = d;
            d = c;
            fd = fc;
            c = b - g * (b - a);
            fc = hum_score(x, sr, c, nh);
        } else {
            a = c;
            c = d;
            fc = fd;
            d = a + g * (b - a);
            fd = hum_score(x, sr, d, nh);
        }
    }
    let f = 0.5 * (a + b);
    (f, hum_score(x, sr, f, nh))
}

/// A bank of harmonic notches whose frequencies can be retuned.
struct NotchBank {
    sections: Vec<(f64, f64, Biquad)>, // (harmonic number, depth dB, filter)
    q: f64,
    sr: f64,
    f0: f64,
}

impl NotchBank {
    fn new(f0: f64, nh: usize, q: f64, depth: impl Fn(usize) -> f64, sr: f64) -> NotchBank {
        let mut sections = Vec::new();
        for k in 1..=nh {
            let f = f0 * k as f64;
            if f >= sr * 0.45 {
                break;
            }
            let d = depth(k);
            sections.push((k as f64, d, Biquad::new(Coefs::notch_depth(f, q, d, sr))));
        }
        NotchBank {
            sections,
            q,
            sr,
            f0,
        }
    }

    fn retune(&mut self, f0: f64) {
        if (f0 - self.f0).abs() < 1e-7 {
            return;
        }
        self.f0 = f0;
        for (k, d, bq) in &mut self.sections {
            bq.set(Coefs::notch_depth(f0 * *k, self.q, *d, self.sr));
        }
    }

    #[inline]
    fn tick(&mut self, x: f64) -> f64 {
        let mut y = x;
        for (_, _, bq) in &mut self.sections {
            y = bq.tick(y);
        }
        y
    }
}

/// Mains hum fundamental if a clear 50/60 Hz hum is present (used by `enhance`).
pub(crate) fn detect_hum(buf: &AudioBuffer) -> Option<f64> {
    let sr = buf.sample_rate as f64;
    let mono: Vec<f64> = buf.mono_mix().iter().map(|v| *v as f64).collect();
    if mono.len() < (sr * 0.25) as usize {
        return None;
    }
    let (dec, dsr) = decimate(&mono, sr, 61.0 * 6.5);
    let head = &dec[..dec.len().min((dsr * 30.0) as usize)];
    let (f, s, strength) = mains_candidate(head, dsr, 6);
    // a clear line: 15 dB above the spectrum just beside it, and above about -70 dBFS
    if strength > 30.0 && s > 1e-7 {
        Some(f.round())
    } else {
        None
    }
}

/// The better of the 50 and 60 Hz families: (frequency, summed harmonic power, line
/// strength = power of the first three harmonics over the spectrum just beside them).
fn mains_candidate(x: &[f64], sr: f64, nh: usize) -> (f64, f64, f64) {
    let (f50, s50) = refine(x, sr, 50.0, 0.6, nh);
    let (f60, s60) = refine(x, sr, 60.0, 0.6, nh);
    let (f, s) = if s50 > s60 { (f50, s50) } else { (f60, s60) };
    // beside the line: three Hann bins away, but never beyond a quarter of the spacing
    let delta = (3.0 * sr / x.len().max(1) as f64).max(2.0).min(f / 4.0);
    let mut line = 0.0;
    let mut floor = 0.0;
    for k in 1..=3 {
        let fk = f * k as f64;
        if fk + delta >= sr * 0.45 {
            break;
        }
        line += goertzel_amp(x, sr, fk).powi(2);
        floor += 0.5 * (goertzel_amp(x, sr, fk - delta).powi(2) + goertzel_amp(x, sr, fk + delta).powi(2));
    }
    (f, s, line / floor.max(1e-30))
}

fn parse_hz(s: &str) -> Result<f64> {
    units::parse_freq(s).map_err(|e| Error::usage(format!("--freq: {}", e.message)))
}

fn dehum(a: &Args, mut buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let sr = buf.sample_rate as f64;
    let nh = a.usize("harmonics")?.clamp(1, 32);
    let q = a.f64("q")?.clamp(1.0, 500.0);
    let depth = a.db("depth")?.min(-1.0);
    let slope = a.db("slope")?;
    let track = a.switch("track", true);
    let n = buf.frames();
    if n == 0 {
        return Ok(buf);
    }
    let mono: Vec<f64> = buf.mono_mix().iter().map(|v| *v as f64).collect();
    let est_h = nh.min(6);
    // analysis signal: decimated mono, at most the first 60 s for detection
    let (dec, dsr) = decimate(&mono, sr, 61.0 * (est_h as f64 + 0.5));
    let freq_arg = a.str_or("freq", "auto");
    let nominal = if freq_arg == "auto" {
        let head = &dec[..dec.len().min((dsr * 30.0) as usize)];
        let (f, s, strength) = mains_candidate(head, dsr, est_h);
        if strength < 10.0 || s < 1e-14 {
            ctx.warn(
                "hum_not_detected",
                "no clear 50/60 Hz hum found; assuming 60 Hz (pass --freq to choose)",
            );
            60.0
        } else {
            f.round()
        }
    } else {
        parse_hz(&freq_arg)?
    };
    let (f0, global_score) = refine(&dec, dsr, nominal, (nominal * 0.01).max(0.05), est_h);
    ctx.note("freq", format!("{f0:.3}Hz"));
    ctx.metric("freq_hz", (f0 * 1000.0).round() / 1000.0);

    // frequency track: one estimate per 0.25 s from 1 s windows
    let mut track_pts: Vec<(f64, f64)> = Vec::new(); // (sample position, f0)
    let win = dsr as usize;
    if track && dec.len() > win * 2 {
        let hop = (dsr * 0.25) as usize;
        let global_amp = (global_score / est_h as f64).sqrt();
        let mut prev = f0;
        let mut raw = Vec::new();
        let mut start = 0;
        while start + win <= dec.len() {
            let seg = &dec[start..start + win];
            let (f, s) = refine(seg, dsr, prev, (f0 * 0.004).max(0.05), est_h);
            let amp = (s / est_h as f64).sqrt();
            let est = if amp > 0.3 * global_amp && (f - f0).abs() < f0 * 0.01 {
                f
            } else {
                prev
            };
            prev = est;
            let centre = (start + win / 2) as f64 * sr / dsr;
            raw.push((centre, est));
            start += hop;
            ctx.check_cancel()?;
        }
        for i in 0..raw.len() {
            let lo = i.saturating_sub(1);
            let hi = (i + 2).min(raw.len());
            let fs: Vec<f64> = raw[lo..hi].iter().map(|p| p.1).collect();
            track_pts.push((raw[i].0, median(&fs)));
        }
    }
    let f_at = |pos: f64| -> f64 {
        if track_pts.is_empty() {
            return f0;
        }
        if pos <= track_pts[0].0 {
            return track_pts[0].1;
        }
        for w in track_pts.windows(2) {
            if pos <= w[1].0 {
                let t = (pos - w[0].0) / (w[1].0 - w[0].0);
                return w[0].1 + (w[1].1 - w[0].1) * t;
            }
        }
        track_pts.last().unwrap().1
    };

    let depth_k = |k: usize| -> f64 { ((depth + slope * (k as f64 - 1.0)).min(-1.0) - 6.0).max(-140.0) };
    let input = if a.path("residual").is_some() {
        Some(buf.clone())
    } else {
        None
    };
    let nch = buf.channels();
    const CHUNK: usize = 32;
    for (ci, ch) in buf.data.iter_mut().enumerate() {
        let mut bank = NotchBank::new(f_at(0.0), nh, q, depth_k, sr);
        for (i, v) in ch.iter_mut().enumerate() {
            if !track_pts.is_empty() && i % CHUNK == 0 {
                bank.retune(f_at(i as f64));
            }
            *v = bank.tick(*v as f64) as f32;
        }
        ctx.set_progress((ci + 1) as f64 / nch as f64);
    }
    // report achieved reduction on the mono mix (skipping the filters' settling time)
    let settle = ((q / (std::f64::consts::PI * f0)) * 3.0 * sr) as usize;
    if n > settle + 1000 {
        let out_mono: Vec<f64> = buf.mono_mix().iter().map(|v| *v as f64).collect();
        let e = (settle + (sr * 10.0) as usize).min(n);
        let before = hum_score(&mono[settle..e], sr, f0, nh);
        let after = hum_score(&out_mono[settle..e], sr, f0, nh);
        if before > 1e-14 {
            ctx.metric(
                "hum_reduction_db",
                (10.0 * (before / after.max(1e-30)).log10() * 10.0).round() / 10.0,
            );
        }
    }
    if let (Some(path), Some(inp)) = (a.path("residual"), input) {
        let mut res = inp;
        for (r, o) in res.data.iter_mut().zip(&buf.data) {
            for (rv, ov) in r.iter_mut().zip(o) {
                *rv -= *ov;
            }
        }
        ctx.write_audio(&PathBuf::from(path), &res, Some("residual"))?;
    }
    Ok(buf)
}

fn notch(a: &Args, mut buf: AudioBuffer, _ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let sr = buf.sample_rate as f64;
    let f = a.freq("freq")?;
    let q = a.f64("q")?.clamp(0.1, 1000.0);
    let depth = a.db("depth")?.min(0.0);
    let extra = a.usize("harmonics")?.min(64);
    if f <= 0.0 || f >= sr / 2.0 {
        return Err(Error::usage(format!(
            "--freq {f} Hz must be between 0 and the Nyquist frequency ({} Hz)",
            sr / 2.0
        )));
    }
    for ch in buf.data.iter_mut() {
        let mut bank = NotchBank::new(f, extra + 1, q, |_| depth, sr);
        for v in ch.iter_mut() {
            *v = bank.tick(*v as f64) as f32;
        }
    }
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferraus_core::testing::{self, run, tone_level};

    fn spec(name: &str) -> OpSpec {
        ops().into_iter().find(|o| o.name == name).unwrap()
    }

    fn hum_fixture(sr: u32, secs: f64, f0: f64) -> (AudioBuffer, AudioBuffer) {
        let tone = testing::sine(sr, 1000.0, secs, 0.3);
        let mut hum = AudioBuffer::new(sr, 1, tone.frames());
        for k in 1..=6 {
            let h = testing::sine(sr, f0 * k as f64, secs, 0.1 / k as f32);
            for (o, v) in hum.data[0].iter_mut().zip(&h.data[0]) {
                *o += *v;
            }
        }
        (testing::add(&tone, &hum), tone)
    }

    #[test]
    fn dehum_removes_hum_and_harmonics_by_40db() {
        let sr = 48000;
        let (x, _) = hum_fixture(sr, 2.0, 60.0);
        let mut c = testing::ctx();
        let sp = spec("dehum");
        let out = ferraus_core::exec::apply_spec(&sp, &testing::args(&sp, &[]), x.clone(), &mut c)
            .unwrap();
        assert_eq!(c.notes["freq"], json!("60.000Hz"));
        let a = &x.data[0][48000..];
        let b = &out.data[0][48000..];
        for k in 1..=6 {
            let f = 60.0 * k as f64;
            let red = 20.0 * (tone_level(a, sr, f) / tone_level(b, sr, f)).log10();
            assert!(red >= 40.0, "harmonic {k}: only {red:.1} dB");
        }
        let t = 20.0 * (tone_level(b, sr, 1000.0) / tone_level(a, sr, 1000.0)).log10();
        assert!(t.abs() < 0.5, "1 kHz changed by {t} dB");
    }

    #[test]
    fn dehum_detects_50hz_and_writes_residual() {
        let sr = 48000;
        let (x, tone) = hum_fixture(sr, 1.5, 50.0);
        let dir = testing::TempDir::new("dehum");
        let res_path = dir.path("hum.wav");
        let sp = spec("dehum");
        let mut c = testing::ctx();
        let out = ferraus_core::exec::apply_spec(
            &sp,
            &testing::args(&sp, &[("residual", res_path.to_str().unwrap()), ("track", "off")]),
            x.clone(),
            &mut c,
        )
        .unwrap();
        assert_eq!(c.notes["freq"], json!("50.000Hz"));
        let res = testing::read_wav(&res_path);
        // residual + output reconstructs the input
        let sum = testing::add(&res, &out);
        assert!(testing::rms_diff(&sum, &x) < 1e-5);
        // the residual is mostly hum, not the tone
        let r = &res.data[0][24000..];
        assert!(tone_level(r, sr, 50.0) > 0.09);
        assert!(tone_level(r, sr, 1000.0) < 0.03, "phase-only leakage of the tone");
        let _ = tone;
    }

    #[test]
    fn dehum_null_silence_and_stereo() {
        let (x, _) = hum_fixture(48000, 0.5, 60.0);
        let out = run(&spec("dehum"), &[("mix", "0%")], x.clone()).unwrap();
        assert_eq!(out, x);
        let s = AudioBuffer::new(48000, 2, 4800);
        let out = run(&spec("dehum"), &[], s.clone()).unwrap();
        assert_eq!(out, s);
        let st = testing::stereo(x);
        let out = run(&spec("dehum"), &[], st).unwrap();
        assert_eq!(out.channels(), 2);
        assert!(run(&spec("dehum"), &[], AudioBuffer::new(48000, 1, 0)).is_ok());
    }

    #[test]
    fn notch_exact_depth_and_harmonics() {
        let sr = 48000;
        let sp = spec("notch");
        for (f, args) in [
            (1000.0, vec![("freq", "1kHz"), ("depth", "-30dB")]),
            (180.0, vec![("freq", "60Hz"), ("harmonics", "2"), ("depth", "-30dB")]),
        ] {
            let x = testing::sine(sr, f, 2.0, 0.5);
            let out = run(&sp, &args, x.clone()).unwrap();
            let g = 20.0
                * (tone_level(&out.data[0][48000..], sr, f) / tone_level(&x.data[0][48000..], sr, f))
                    .log10();
            assert!((g + 30.0).abs() < 0.5, "{f}: {g}");
        }
        let x = testing::sine(sr, 3000.0, 1.0, 0.5);
        let out = run(&sp, &[], x.clone()).unwrap();
        assert!(testing::rms_diff(&out, &x) < 1e-3);
        assert_eq!(run(&sp, &[("mix", "0%")], x.clone()).unwrap(), x);
        assert!(run(&sp, &[("freq", "30kHz")], x).is_err());
    }
}
