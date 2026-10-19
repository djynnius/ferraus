//! `gain`, `normalize`, and `resample`.

use crate::loudness::{self, LimiterOptions, LimiterReport};
use crate::util;
use ferraus_core::prelude::*;
use ferraus_core::resample::{resample, Quality};
use ferraus_core::units::Gain;

pub fn ops() -> Vec<OpSpec> {
    vec![gain_spec(), normalize_spec(), resample_spec()]
}

// ---------------------------------------------------------------------------------------
// gain
// ---------------------------------------------------------------------------------------

pub fn gain_spec() -> OpSpec {
    OpSpec::transform("gain", Category::Edit, "Apply a fixed gain, overall or per channel", gain)
        .about(
            "Makes the audio louder or quieter by a fixed amount, in decibels (-6dB) or as a \
             factor (0.5x). Give per-channel values as L=-3dB,R=0dB (or 1=..,2=.. for any \
             channel number; unlisted channels are unchanged), like Audition's Amplify. Warns \
             when the result clips.",
        )
        .param(
            param("by", ParamKind::Str)
                .default("0dB")
                .help("Gain in dB (-6dB, +3dB) or as a factor (0.5x); per channel as L=-3dB,R=0dB"),
        )
        .example("ferraus gain vocal.wav --by -6dB -o vocal_quiet.wav", "Turn the vocal down 6 dB")
        .example("ferraus gain mix.wav --by L=-1.5dB,R=0dB -o mix_balanced.wav", "Pull the left channel down 1.5 dB")
        .example(
            "ferraus fx in.wav -o out.wav --fx gain:by=2x --fx fade:out=3s",
            "Double the amplitude inside an effects chain",
        )
}

fn gain_value(s: &str) -> Result<f64> {
    let g = Gain::parse(s).map_err(|e| Error::usage(format!("--by: {}", e.message)))?;
    match g {
        Gain::Lufs(_) | Gain::DbTp(_) => {
            Err(Error::usage(format!("--by takes a fixed gain such as -6dB or 0.5x, not a target ('{s}')"))
                .with_hint("ferraus normalize <input> --lufs -14"))
        }
        other => Ok(other.linear()),
    }
}

/// Channel index from a name: `L`/`left` = 0, `R`/`right` = 1, `C` = 2, or a 1-based number.
pub fn channel_index(name: &str, channels: usize) -> Result<usize> {
    let k = name.trim().to_ascii_lowercase();
    let idx = match k.as_str() {
        "l" | "left" => 0,
        "r" | "right" => 1,
        "c" | "center" | "centre" => 2,
        other => {
            let v: usize = other.parse().map_err(|_| {
                Error::usage(format!("unknown channel '{name}': use L, R, C, or a channel number from 1"))
            })?;
            if v == 0 {
                return Err(Error::usage("channel numbers start at 1"));
            }
            v - 1
        }
    };
    if idx >= channels {
        return Err(Error::usage(format!("channel '{name}' does not exist: the input has {channels} channel(s)")));
    }
    Ok(idx)
}

/// Parse `--by` into one linear gain per channel.
pub fn parse_gain_spec(s: &str, channels: usize) -> Result<Vec<f64>> {
    if s.contains('=') {
        let mut g = vec![1.0; channels];
        for part in units::split_list(s) {
            let (k, v) =
                part.split_once('=').ok_or_else(|| Error::usage(format!("--by: '{part}' is not CHANNEL=GAIN")))?;
            g[channel_index(k, channels)?] = gain_value(v)?;
        }
        Ok(g)
    } else {
        Ok(vec![gain_value(s)?; channels])
    }
}

fn gain(a: &Args, mut buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let gains = parse_gain_spec(&a.str_or("by", "0dB"), buf.channels())?;
    for (c, g) in buf.data.iter_mut().zip(&gains) {
        if *g != 1.0 {
            let g = *g as f32;
            for s in c.iter_mut() {
                *s *= g;
            }
        }
    }
    let peak = buf.peak();
    ctx.metric("peak_db", util::round_db(lin_to_db(peak as f64)));
    if peak > 1.0 {
        ctx.warn(
            "clipping",
            format!("the result peaks at {:+.2} dBFS and will clip in fixed-point formats", lin_to_db(peak as f64)),
        );
    }
    Ok(buf)
}

// ---------------------------------------------------------------------------------------
// normalize
// ---------------------------------------------------------------------------------------

pub fn normalize_spec() -> OpSpec {
    OpSpec::transform("normalize", Category::Edit, "Set loudness (LUFS), peak, or RMS level to a target", normalize)
        .about(
            "Measures the input and applies the gain that lands it on the target. The default \
             mode is integrated loudness per EBU R128 (--lufs -14, the usual streaming level). \
             --peak -1dBTP normalizes the true (inter-sample, 4x oversampled) peak and --peak \
             -1dB the sample peak; --rms -20dB the average level. Give one mode only. For --lufs \
             and --rms, if the gain pushes the true peak over --ceiling a transparent lookahead \
             true-peak limiter holds it under the ceiling (--true-peak-limit off disables that). \
             Several inputs are normalized one by one to the same target (Match Loudness). \
             Before and after measurements are reported as metrics.",
        )
        .param(
            param("lufs", ParamKind::Gain)
                .default("-14")
                .help("Integrated loudness target in LUFS (default mode), e.g. -14, -16, -23"),
        )
        .param(param("peak", ParamKind::Gain).help("Peak target: -1dBTP for true peak, -1dB for sample peak"))
        .param(param("rms", ParamKind::Gain).help("RMS level target in dBFS, e.g. -20dB"))
        .param(
            param("true-peak-limit", ParamKind::Switch)
                .default("on")
                .help("With --lufs/--rms: limit inter-sample peaks to --ceiling after the gain"),
        )
        .param(param("ceiling", ParamKind::Gain).default("-1dBTP").help("True-peak ceiling in dBTP for the limiter"))
        .param(
            param("lookahead", ParamKind::Time).default("3ms").advanced().help("Limiter lookahead (attack) duration"),
        )
        .param(
            param("release", ParamKind::Time)
                .default("80ms")
                .advanced()
                .help("Limiter release time constant (duration)"),
        )
        .example(
            "ferraus normalize song.wav -o song_streaming.wav",
            "Integrated loudness to -14 LUFS, true peak at most -1 dBTP",
        )
        .example("ferraus normalize ep/*.wav --lufs -16 --outdir matched/", "Match a batch of tracks to -16 LUFS")
        .example("ferraus normalize sample.wav --peak -0.1dB -o sample_max.wav", "Sample peak to -0.1 dBFS")
        .example(
            "ferraus normalize vo.wav --lufs -23 --ceiling -2dBTP --json",
            "Broadcast loudness with a -2 dBTP ceiling and a JSON report",
        )
}

/// Which explicit mode the user picked. Explicit flags win over the default `--lufs`; two
/// explicit modes are a usage error.
fn pick_mode(a: &Args) -> Result<&'static str> {
    let explicit: Vec<&'static str> = ["lufs", "peak", "rms"].into_iter().filter(|m| a.is_explicit(m)).collect();
    match explicit.as_slice() {
        [] => {
            if a.has("peak") {
                Ok("peak")
            } else if a.has("rms") {
                Ok("rms")
            } else {
                Ok("lufs")
            }
        }
        [one] => Ok(one),
        many => Err(Error::usage(format!(
            "choose one normalize mode; got {}",
            many.iter().map(|m| format!("--{m}")).collect::<Vec<_>>().join(" and ")
        ))),
    }
}

fn scaled(buf: &AudioBuffer, gain_db: f64) -> AudioBuffer {
    let mut b = buf.clone();
    b.scale(db_to_lin(gain_db) as f32);
    b
}

/// Apply `gain_db`, then (when `limit` is set) limit true peaks and iterate the gain so the
/// measured level still lands on `target`. Returns the output, the total gain, and the
/// limiter report when it ran.
pub fn level_with_limiter(
    buf: &AudioBuffer,
    target: f64,
    gain_db: f64,
    measure: &dyn Fn(&AudioBuffer) -> Result<f64>,
    limit: Option<LimiterOptions>,
) -> Result<(AudioBuffer, f64, Option<LimiterReport>)> {
    let out = scaled(buf, gain_db);
    let Some(opts) = limit else {
        return Ok((out, gain_db, None));
    };
    if loudness::true_peak_conservative(&out)? <= db_to_lin(opts.ceiling_db) as f32 {
        return Ok((out, gain_db, None));
    }
    let max_gain = gain_db + 6.0;
    let mut g = gain_db;
    let mut prev: Option<(f64, f64)> = None; // (gain, measured)
    let mut best: Option<(f64, AudioBuffer, f64, LimiterReport)> = None;
    for _ in 0..8 {
        let mut y = scaled(buf, g);
        let rep = loudness::limit_true_peak(&mut y, &opts)?;
        let m = measure(&y)?;
        let err = target - m;
        let better = best.as_ref().map(|b| err.abs() < b.0.abs()).unwrap_or(true);
        if better {
            best = Some((err, y, g, rep));
        }
        if err.abs() < 0.05 {
            break;
        }
        // secant step: the limiter makes loudness grow slower than the gain
        let slope = match prev {
            Some((pg, pm)) if (g - pg).abs() > 1e-6 => ((m - pm) / (g - pg)).clamp(0.1, 1.0),
            _ => 1.0,
        };
        prev = Some((g, m));
        let next = (g + err / slope).min(max_gain);
        if (next - g).abs() < 1e-4 {
            break;
        }
        g = next;
    }
    let (_, y, g, rep) = best.expect("at least one iteration");
    Ok((y, g, Some(rep)))
}

fn normalize(a: &Args, buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let mode = pick_mode(a)?;
    let target = a.gain(mode)?;
    ctx.note("mode", mode);
    if buf.is_empty() || buf.peak() == 0.0 {
        ctx.warn("silent_input", "the input is silent; nothing to normalize");
        return Ok(buf);
    }
    let ceiling = a.gain("ceiling")?;
    if matches!(ceiling, Gain::Lufs(_)) {
        return Err(Error::usage("--ceiling is a peak level such as -1dBTP, not LUFS"));
    }
    let sr = buf.sample_rate as f64;
    let limit = if a.switch("true-peak-limit", true) && mode != "peak" {
        Some(LimiterOptions {
            ceiling_db: ceiling.db(),
            lookahead_s: util::duration_frames(a, "lookahead", buf.sample_rate, None)? as f64 / sr,
            release_s: util::duration_frames(a, "release", buf.sample_rate, None)? as f64 / sr,
        })
    } else {
        None
    };
    let before = loudness::analyze(&buf)?;
    ctx.metric("before", &before);
    let (out, gain_db, report) = match mode {
        "lufs" => {
            if matches!(target, Gain::Linear(_) | Gain::DbTp(_)) {
                return Err(Error::usage("--lufs takes a loudness such as -14 or -14LUFS"));
            }
            let t = target.db();
            ctx.note("target_lufs", t);
            let measured = loudness::integrated_lufs(&buf)?;
            if !measured.is_finite() {
                ctx.warn("silent_input", "the input is below the loudness gate; nothing to normalize");
                return Ok(buf);
            }
            if buf.duration_s() < 3.0 {
                ctx.warn(
                    "short_input",
                    "inputs shorter than 3 s are measured by looping them; loudness is approximate",
                );
            }
            level_with_limiter(&buf, t, t - measured, &|b| loudness::integrated_lufs(b), limit)?
        }
        "rms" => {
            if matches!(target, Gain::Lufs(_) | Gain::DbTp(_)) {
                return Err(Error::usage("--rms takes a level in dB such as -20dB"));
            }
            let t = target.db();
            ctx.note("target_rms_db", t);
            let measured = loudness::rms_db(&buf);
            level_with_limiter(&buf, t, t - measured, &|b| Ok(loudness::rms_db(b)), limit)?
        }
        _ => {
            let (measured, kind) = match target {
                Gain::DbTp(_) => (lin_to_db(loudness::true_peak_conservative(&buf)? as f64), "true_peak"),
                Gain::Lufs(_) => {
                    return Err(Error::usage("--peak takes a peak level such as -1dBTP or -1dB"));
                }
                _ => (lin_to_db(buf.peak() as f64), "sample_peak"),
            };
            ctx.note("peak_kind", kind);
            let t = target.db();
            let mut out = scaled(&buf, t - measured);
            let check = if kind == "true_peak" { loudness::true_peak_conservative(&out)? } else { out.peak() };
            // guard against float rounding pushing the peak a hair over the target
            let tl = db_to_lin(t) as f32 * 0.99999;
            if check > tl {
                out.scale(tl / check);
            }
            (out, t - measured, None)
        }
    };
    ctx.metric("gain_db", util::round_db(gain_db));
    if let Some(r) = &report {
        ctx.metric("limiter", r);
        ctx.note("limiter_ceiling_dbtp", ceiling.db());
    }
    let after = loudness::analyze(&out)?;
    if mode == "lufs" {
        let t = target.db();
        if (after.integrated_lufs - t).abs() > 0.3 {
            ctx.warn(
                "target_not_reached",
                format!(
                    "reached {:.2} LUFS instead of {t:.2}: the true-peak ceiling of {:.1} dBTP does not allow more without audible limiting",
                    after.integrated_lufs,
                    ceiling.db()
                ),
            );
        }
    }
    if limit.is_none() && mode != "peak" && after.true_peak_dbtp > ceiling.db() {
        ctx.warn(
            "true_peak_over_ceiling",
            format!("true peak is {:.2} dBTP, above the {:.1} dBTP ceiling", after.true_peak_dbtp, ceiling.db()),
        );
    }
    if out.peak() > 1.0 {
        ctx.warn("clipping", "the result peaks above 0 dBFS and will clip in fixed-point formats");
    }
    ctx.metric("after", &after);
    Ok(out)
}

// ---------------------------------------------------------------------------------------
// resample
// ---------------------------------------------------------------------------------------

pub fn resample_spec() -> OpSpec {
    OpSpec::transform(
        "resample",
        Category::Edit,
        "Change the sample rate with a high-quality sinc resampler",
        resample_op,
    )
    .about(
        "Converts to a new sample rate with a windowed-sinc (Kaiser) resampler that keeps the \
             audible band intact and removes everything above the new Nyquist frequency. \
             Rates accept 48000, 48k, or 44.1kHz.",
    )
    .param(param("to", ParamKind::Freq).default("48000").help("Target sample rate in Hz: 48000, 48k, 44.1kHz"))
    .param(
        param("quality", ParamKind::Choice)
            .choices(&["high", "medium", "fast"])
            .default("high")
            .help("Filter length: high (best, slowest), medium, fast"),
    )
    .no_mix()
    .example("ferraus resample voice.wav --to 48k -o voice48.wav", "Convert to 48 kHz for video")
    .example("ferraus resample master96.flac --to 44.1kHz -o master44.flac", "Down-convert a 96 kHz master for CD")
}

fn resample_op(a: &Args, buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let to = a.freq("to")?;
    if !(1000.0..=768_000.0).contains(&to) {
        return Err(Error::usage(format!("--to {to} Hz is not a usable sample rate (1000 to 768000)")));
    }
    let to = to.round() as u32;
    let q = Quality::parse(&a.choice("quality"));
    ctx.note("from_hz", buf.sample_rate);
    ctx.note("to_hz", to);
    if to == buf.sample_rate {
        return Ok(buf);
    }
    Ok(resample(&buf, to, q))
}
