//! Space and character: `echo` (multi-tap with per-repeat EQ), `doppler`, and
//! `distort` (drawn transfer curves).

use crate::dsp::biquad::{parse_filter_spec, Coefs, Sos};
use crate::dsp::util::{read_cubic, Curve};
use ferraus_core::prelude::*;

pub fn ops() -> Vec<OpSpec> {
    vec![
        OpSpec::transform("echo", Category::Space, "Multi-tap echo with per-repeat filtering", echo)
            .about(
                "Adds --taps repeats spaced --time apart (a beat fraction such as 1/4 follows \
                 --bpm, else 120 BPM; or a time such as 375ms). Each repeat is --feedback \
                 quieter than the one before and passes through --tap-eq once more, so \
                 repeats get progressively darker. --pingpong alternates repeats between left \
                 and right (a mono input becomes stereo). The output keeps the input length.",
            )
            .param(param("taps", ParamKind::Int).default("3").range("1..32").help("Number of repeats"))
            .param(
                param("time", ParamKind::Time)
                    .default("1/4")
                    .help("Spacing between repeats: a beat fraction (1/4, 1/8D) or a time (375ms)"),
            )
            .param(
                param("feedback", ParamKind::Percent)
                    .default("40%")
                    .help("Level of each repeat relative to the previous one, in percent"),
            )
            .param(
                param("tap-eq", ParamKind::List)
                    .default("lowpass:4kHz")
                    .help("Filter(s) applied once more to every repeat, e.g. lowpass:4kHz or none"),
            )
            .param(param("pingpong", ParamKind::Flag).help("Alternate repeats between left and right"))
            .example("ferraus echo vox.wav --bpm 96 -o echo.wav", "Quarter-note echoes at 96 BPM")
            .example(
                "ferraus echo gtr.wav --taps 6 --time 375ms --feedback 55% --tap-eq highpass:300Hz,lowpass:3kHz --pingpong -o echo.wav",
                "Telephone-like ping-pong repeats",
            ),
        OpSpec::transform(
            "doppler",
            Category::Space,
            "Doppler pitch and level trajectory of a moving source",
            doppler,
        )
        .about(
            "Simulates a source moving past the listener: along a straight line that passes \
             --pass-by metres away after starting --distance metres before the closest point, \
             or around a circle of radius --distance whose centre is --pass-by metres from \
             the listener. A variable fractional delay (speed of sound 343 m/s) produces the \
             pitch change; --volume-by-distance scales level by 1/distance (loudest when \
             closest). The output keeps the input length.",
        )
        .param(
            param("path", ParamKind::Choice)
                .choices(&["line", "circle"])
                .default("line")
                .help("Trajectory shape"),
        )
        .param(param("velocity", ParamKind::Str).default("20m/s").help("Source speed in m/s"))
        .param(
            param("distance", ParamKind::Str)
                .default("10m")
                .help("Line: start distance before the closest point; circle: radius (metres)"),
        )
        .param(
            param("pass-by", ParamKind::Str)
                .default("2m")
                .help("Line: closest distance to the listener; circle: centre offset (metres)"),
        )
        .param(
            param("volume-by-distance", ParamKind::Switch)
                .default("on")
                .help("Scale the level with distance (on|off)"),
        )
        .example("ferraus doppler car.wav -o passby.wav", "A source passing 2 m away at 20 m/s")
        .example(
            "ferraus doppler tone.wav --path circle --velocity 30m/s --distance 5m --pass-by 3m -o orbit.wav",
            "Circling the listener off-centre",
        ),
        OpSpec::transform(
            "distort",
            Category::Tone,
            "Distortion from drawn transfer curves",
            distort,
        )
        .about(
            "Maps each sample through a transfer curve drawn as 'in:out' points, with \
             separate curves for the positive and negative half-waves (asymmetric \
             distortion). Points are linear amplitudes (0.5) or dB (-6dB); an implicit 0:0 \
             point anchors the curve and inputs beyond the last point hold its output (hard \
             clip). --smooth joins points with a smooth monotone curve instead of straight \
             lines. dB points below -range are clamped to -range. `saturate` is the musical \
             default; this is the precise tool.",
        )
        .param(
            param("curve-pos", ParamKind::Str)
                .default("0.25:0.3,0.5:0.55,0.75:0.7,1:0.75")
                .help("Positive half-wave curve as in:out points (linear or dB)"),
        )
        .param(
            param("curve-neg", ParamKind::Str)
                .default("same")
                .help("Negative half-wave curve as in:out magnitudes, or 'same' to mirror --curve-pos"),
        )
        .param(
            param("smooth", ParamKind::Switch)
                .default("on")
                .help("Smooth curve through the points (on) or straight segments (off)"),
        )
        .param(
            param("range", ParamKind::Gain)
                .default("60dB")
                .help("Lowest level in dB that dB-valued points can describe"),
        )
        .example("ferraus distort gtr.wav -o fuzz.wav", "Gentle symmetric soft clipping")
        .example(
            "ferraus distort bass.wav --curve-pos \"-20dB:-14dB,-6dB:-4dB,0dB:-3dB\" --curve-neg \"0.5:0.5,1:1\" -o dirty.wav",
            "Asymmetric: compress only the positive half-wave",
        ),
    ]
}

// ---------------------------------------------------------------------------------
// echo
// ---------------------------------------------------------------------------------

fn echo(a: &Args, buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let sr = buf.sample_rate as f64;
    let taps = a.usize("taps")?.clamp(1, 32);
    let t = a.sync_time("time")?.seconds(buf.sample_rate, ctx.bpm(), 120.0)?;
    if ctx.bpm().is_none() && a.raw("time").map(|v| v.contains('/')).unwrap_or(false) {
        ctx.note("bpm", 120);
    }
    let d = (t * sr).round().max(1.0) as usize;
    let fb = a.percent("feedback")?.clamp(0.0, 1.5);
    let mut eq: Vec<Coefs> = Vec::new();
    for spec in a.list("tap-eq") {
        if let Some(c) = parse_filter_spec(&spec, sr)? {
            eq.push(c);
        }
    }
    let eq = Sos::new(eq);
    let pingpong = a.flag("pingpong");
    let n = buf.frames();
    let nch = if pingpong { buf.channels().max(2) } else { buf.channels() };
    let dry = buf.with_channels(nch);
    let mut out = dry.clone();
    // the repeat sources: each channel itself, or the mono sum for ping-pong
    let sources: Vec<Vec<f32>> = if pingpong { vec![buf.mono_mix()] } else { buf.data.clone() };
    for (si, src) in sources.iter().enumerate() {
        let mut rep = src.clone();
        let mut g = 1.0f64;
        for k in 1..=taps {
            if k * d >= n {
                break;
            }
            if !eq.sections.is_empty() {
                eq.apply_in_place(&mut rep);
            }
            g *= fb;
            let targets: Vec<usize> = if pingpong { vec![(k - 1) % 2] } else { vec![si] };
            for c in targets {
                let o = &mut out.data[c];
                for i in k * d..n {
                    o[i] += (rep[i - k * d] as f64 * g) as f32;
                }
            }
        }
    }
    ctx.metric("delay_ms", (t * 1000.0 * 100.0).round() / 100.0);
    Ok(out)
}

// ---------------------------------------------------------------------------------
// doppler
// ---------------------------------------------------------------------------------

fn metres(a: &Args, name: &str, units_: &[&str]) -> Result<f64> {
    units::parse_with_unit(&a.str_or(name, ""), units_).map_err(|e| Error::usage(format!("--{name}: {}", e.message)))
}

fn doppler(a: &Args, mut buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    const C_SOUND: f64 = 343.0;
    let sr = buf.sample_rate as f64;
    let v = metres(a, "velocity", &["m/s", "mps"])?;
    let dist = metres(a, "distance", &["m"])?.abs();
    let pass = metres(a, "pass-by", &["m"])?.abs();
    if v.abs() >= C_SOUND * 0.9 {
        return Err(Error::usage("--velocity must stay well below the speed of sound (343 m/s)"));
    }
    let circle = a.choice("path") == "circle";
    let vol = a.switch("volume-by-distance", true);
    let rmin_floor = 0.5;
    let r_at = |t: f64| -> f64 {
        if circle {
            let w = if dist > 0.0 { v / dist } else { 0.0 };
            let (x, y) = (pass + dist * (w * t).cos(), dist * (w * t).sin());
            (x * x + y * y).sqrt().max(rmin_floor)
        } else {
            let x = -dist + v * t;
            (x * x + pass * pass).sqrt().max(rmin_floor)
        }
    };
    let r_min = if circle { (dist - pass).abs().max(rmin_floor) } else { pass.max(rmin_floor) };
    let r0 = r_at(0.0);
    let n = buf.frames();
    let delay: Vec<f64> = (0..n).map(|i| (r_at(i as f64 / sr) - r0) / C_SOUND * sr).collect();
    let gain: Vec<f64> = (0..n).map(|i| if vol { (r_min / r_at(i as f64 / sr)).min(1.0) } else { 1.0 }).collect();
    for ch in buf.data.iter_mut() {
        let src = ch.clone();
        for i in 0..n {
            ch[i] = (read_cubic(&src, i as f64 - delay[i]) * gain[i]) as f32;
        }
    }
    let (fmin, fmax) =
        delay.windows(2).map(|w| 1.0 - (w[1] - w[0])).fold((f64::MAX, f64::MIN), |(lo, hi), r| (lo.min(r), hi.max(r)));
    if n > 1 {
        ctx.metric("pitch_ratio_min", (fmin * 10000.0).round() / 10000.0);
        ctx.metric("pitch_ratio_max", (fmax * 10000.0).round() / 10000.0);
    }
    Ok(buf)
}

// ---------------------------------------------------------------------------------
// distort
// ---------------------------------------------------------------------------------

fn parse_amp(s: &str, range_db: f64) -> Result<f64> {
    let t = s.trim();
    if t.to_ascii_lowercase().ends_with("db") {
        let db = units::parse_db(t)?.max(-range_db);
        Ok(db_to_lin(db))
    } else {
        let v: f64 = t.parse().map_err(|_| Error::usage(format!("'{t}' is not a level (0.5 or -6dB)")))?;
        Ok(v.abs())
    }
}

fn shaper(spec: &str, smooth: bool, range_db: f64) -> Result<Curve> {
    let mut pts = vec![(0.0, 0.0)];
    for item in units::split_list(spec) {
        let (i, o) = item
            .split_once(':')
            .ok_or_else(|| Error::usage(format!("curve point '{item}' should be in:out, e.g. 0.5:0.6 or -6dB:-4dB")))?;
        pts.push((parse_amp(i, range_db)?, parse_amp(o, range_db)?));
    }
    if pts.len() < 2 {
        return Err(Error::usage("a distortion curve needs at least one in:out point"));
    }
    Ok(Curve::new(pts, smooth))
}

fn distort(a: &Args, mut buf: AudioBuffer, _ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let smooth = a.switch("smooth", true);
    let range = a.db("range")?.abs().max(1.0);
    let pos = shaper(&a.str_or("curve-pos", ""), smooth, range)
        .map_err(|e| Error::usage(format!("--curve-pos: {}", e.message)))?;
    let neg_spec = a.str_or("curve-neg", "same");
    let neg = if neg_spec == "same" {
        pos.clone()
    } else {
        shaper(&neg_spec, smooth, range).map_err(|e| Error::usage(format!("--curve-neg: {}", e.message)))?
    };
    for ch in buf.data.iter_mut() {
        for v in ch.iter_mut() {
            let x = *v as f64;
            *v = if x >= 0.0 { pos.eval(x) } else { -neg.eval(-x) } as f32;
        }
    }
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::dominant_freq;
    use ferraus_core::testing::{self, run};

    fn spec(name: &str) -> OpSpec {
        ops().into_iter().find(|o| o.name == name).unwrap()
    }

    #[test]
    fn echo_impulse_response_is_analytic() {
        let sr = 48000;
        let mut x = AudioBuffer::new(sr, 1, sr as usize * 2);
        x.data[0][0] = 1.0;
        // 1/4 at the fallback 120 BPM = 0.5 s = 24000 samples
        let y = run(&spec("echo"), &[("tap-eq", "none")], x.clone()).unwrap();
        assert_eq!(y.data[0][0], 1.0);
        assert!((y.data[0][24000] - 0.4).abs() < 1e-6);
        assert!((y.data[0][48000] - 0.16).abs() < 1e-6);
        assert!((y.data[0][72000] - 0.064).abs() < 1e-6);
        let nonzero = y.data[0].iter().filter(|v| v.abs() > 1e-9).count();
        assert_eq!(nonzero, 4);
        // per-repeat filtering: each repeat is darker than the last
        let y = run(&spec("echo"), &[("time", "100ms"), ("taps", "3")], x.clone()).unwrap();
        let energy = |s: usize| -> f64 { y.data[0][s..s + 2000].iter().map(|v| (*v as f64).powi(2)).sum() };
        let hf = |s: usize| -> f64 {
            y.data[0][s..s + 2000].windows(2).map(|w| ((w[1] - w[0]) as f64).powi(2)).sum::<f64>() / energy(s)
        };
        assert!(hf(9600) < hf(4800), "second repeat darker than the first");
        // ping-pong makes a mono input stereo and alternates sides
        let y = run(&spec("echo"), &[("tap-eq", "none"), ("pingpong", "true")], x.clone()).unwrap();
        assert_eq!(y.channels(), 2);
        assert!(y.data[0][24000].abs() > 0.39 && y.data[1][24000].abs() < 1e-9);
        assert!(y.data[1][48000].abs() > 0.15 && y.data[0][48000].abs() < 1e-9);
        assert_eq!(run(&spec("echo"), &[("mix", "0%")], x.clone()).unwrap(), x);
    }

    #[test]
    fn doppler_pitch_rises_then_falls() {
        let sr = 48000;
        let x = testing::sine(sr, 1000.0, 1.0, 0.5);
        let mut c = testing::ctx();
        let sp = spec("doppler");
        let y = ferraus_core::exec::apply_spec(&sp, &testing::args(&sp, &[]), x.clone(), &mut c).unwrap();
        let early = dominant_freq(&y.data[0][2000..12000], sr, 900.0, 1100.0);
        let late = dominant_freq(&y.data[0][38000..47000], sr, 900.0, 1100.0);
        assert!(early > 1040.0, "approaching {early}");
        assert!(late < 975.0, "receding {late}");
        // closest at 0.5 s is loudest
        let lvl = |s: usize| testing::rms_db(&y.data[0][s..s + 2400]);
        assert!(lvl(22800) > lvl(1000) + 6.0);
        assert!(c.metrics["pitch_ratio_max"].as_f64().unwrap() > 1.04);
        // no motion: identity
        let still = run(&sp, &[("velocity", "0m/s"), ("volume-by-distance", "off")], x.clone()).unwrap();
        assert!(testing::rms_diff(&still, &x) < 1e-6);
        let circ = run(&sp, &[("path", "circle")], x.clone()).unwrap();
        assert_eq!(circ.frames(), x.frames());
        assert_eq!(run(&sp, &[("mix", "0%")], x.clone()).unwrap(), x);
        assert!(run(&sp, &[("velocity", "400m/s")], x).is_err());
    }

    #[test]
    fn distort_curves_are_exact() {
        let x = AudioBuffer::from_mono(48000, vec![0.0, 0.5, -0.5, 1.0, -1.0, 0.25, 2.0]);
        let sp = spec("distort");
        let y = run(&sp, &[("curve-pos", "1:0.5"), ("smooth", "off")], x.clone()).unwrap();
        assert_eq!(y.data[0], vec![0.0, 0.25, -0.25, 0.5, -0.5, 0.125, 0.5]);
        // asymmetric: negative half untouched
        let y = run(&sp, &[("curve-pos", "1:0.5"), ("curve-neg", "1:1"), ("smooth", "off")], x.clone()).unwrap();
        assert_eq!(y.data[0][2], -0.5);
        assert_eq!(y.data[0][1], 0.25);
        // dB points: -6dB in maps to -12dB out
        let y = run(&sp, &[("curve-pos", "-6dB:-12dB,0dB:0dB"), ("smooth", "off")], x.clone()).unwrap();
        assert!((y.data[0][1] as f64 - 0.5 * db_to_lin(-12.0) / db_to_lin(-6.0)).abs() < 0.01);
        // default is a gentle, monotone soft clip that never exceeds 0.75
        let s = testing::sine(48000, 100.0, 0.1, 1.0);
        let y = run(&sp, &[], s.clone()).unwrap();
        assert!(y.peak() <= 0.7501);
        assert_eq!(run(&sp, &[("mix", "0%")], s.clone()).unwrap(), s);
        assert!(run(&sp, &[("curve-pos", "x")], s).is_err());
    }
}
