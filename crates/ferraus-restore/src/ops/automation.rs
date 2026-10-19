//! Automation over time: `envelope` (keyframed gain, pan, or pitch) and `pitch-bend`.

use crate::dsp::pitch::{shift_preserving, varispeed};
use crate::dsp::util::{parse_keyframes, resolve_keyframes, Curve};
use ferraus_core::prelude::*;
use std::f64::consts::PI;

pub fn ops() -> Vec<OpSpec> {
    vec![
        OpSpec::transform(
            "envelope",
            Category::Edit,
            "Keyframed automation of gain, pan, or pitch",
            envelope,
        )
        .about(
            "Draws an automation curve through 'time=value' keyframes (times as 2s, 1:30, \
             4b, end, or -5s from the end) and applies it: gain in dB (interpolated in dB), \
             pan from -100 (left) to 100 (right) or L30/R60/center (a mono input becomes \
             stereo with a constant-power -3 dB law; a stereo input is balanced), or pitch \
             in cents (duration-preserving granular shift; good for speech and monophonic \
             lines, rougher on dense mixes). Values hold before the first and after the last \
             keyframe; keyframes are hit sample-accurately.",
        )
        .param(
            param("points", ParamKind::List)
                .default("0s=0dB,2s=-6dB,end=-60dB")
                .help("Keyframes as time=value, e.g. 0s=0dB,2s=-6dB,end=-60dB (pan: 0s=-100,4s=100; pitch: 0s=0,2s=+100cents)"),
        )
        .param(
            param("curve", ParamKind::Choice)
                .choices(&["linear", "spline"])
                .default("linear")
                .help("Straight segments, or a smooth curve through the keyframes"),
        )
        .param(
            param("target", ParamKind::Choice)
                .choices(&["gain", "pan", "pitch"])
                .default("gain")
                .help("What the keyframes control"),
        )
        .example(
            "ferraus envelope in.wav --points 0s=-60dB,1s=0dB,end=0dB -o faded-in.wav",
            "A one-second fade in, then unity gain",
        )
        .example(
            "ferraus envelope mono.wav --target pan --points 0s=-100,4s=100 --curve spline -o sweep.wav",
            "Sweep a mono source from left to right",
        )
        .example(
            "ferraus envelope vox.wav --target pitch --points 0s=0,2s=+100cents,4s=0 -o bend.wav",
            "Bend up a semitone and back",
        ),
        OpSpec::transform(
            "pitch-bend",
            Category::Pitch,
            "Pitch envelope: tape-style varispeed or duration-preserving bend",
            pitch_bend,
        )
        .no_mix()
        .about(
            "Follows a pitch curve of 'time=cents' keyframes over the input's timeline. By \
             default it is tape-style: playback speed changes with pitch, so the duration \
             changes too. With --preserve-duration on, a granular WSOLA shifter keeps the \
             timing and only moves the pitch (good for speech and monophonic material).",
        )
        .param(
            param("points", ParamKind::List)
                .default("0s=0,4s=+200cents")
                .help("Keyframes as time=pitch, pitch in cents or semitones (0s=0,4s=+200cents)"),
        )
        .param(
            param("curve", ParamKind::Choice)
                .choices(&["linear", "spline"])
                .default("spline")
                .help("Straight segments, or a smooth curve through the keyframes"),
        )
        .param(
            param("preserve-duration", ParamKind::Switch)
                .default("off")
                .help("Keep the duration and shift only the pitch (on), or vary speed like tape (off)"),
        )
        .example("ferraus pitch-bend riff.wav -o bend.wav", "Tape-style rise of two semitones over 4 s")
        .example(
            "ferraus pitch-bend vox.wav --points 0s=0,1s=-1200cents --preserve-duration on -o low.wav",
            "Glide down an octave without changing the timing",
        )
        .example("ferraus pitch-bend vinyl.wav --points 0s=0,1.5s=-1200cents --curve linear -o stop.wav", "Turntable stop: slow down an octave over 1.5 s (the file gets longer, like a real record)")
        .example("ferraus pitch-bend speech.wav --points 0s=0,2s=+300cents,4s=0 --preserve-duration on -o rise.wav", "Lift a sentence three semitones and back without changing its timing"),
    ]
}

fn parse_pan(s: &str) -> Result<f64> {
    let t = s.trim().to_ascii_lowercase();
    let v = match t.as_str() {
        "left" | "l" => -100.0,
        "right" | "r" => 100.0,
        "center" | "centre" | "c" => 0.0,
        _ => {
            if let Some(n) = t.strip_prefix('l') {
                -n.parse::<f64>().map_err(|_| Error::usage(format!("bad pan '{s}'")))?
            } else if let Some(n) = t.strip_prefix('r') {
                n.parse::<f64>().map_err(|_| Error::usage(format!("bad pan '{s}'")))?
            } else {
                t.parse::<f64>()
                    .map_err(|_| Error::usage(format!("bad pan '{s}' (use -100..100, L30, R60, center)")))?
            }
        }
    };
    if !(-100.0..=100.0).contains(&v) {
        return Err(Error::usage(format!("pan '{s}' is outside -100..100")));
    }
    Ok(v)
}

/// Evaluate keyframes for every sample.
fn automation(points: &[(f64, f64)], spline: bool) -> Curve {
    Curve::new(points.to_vec(), spline)
}

fn envelope(a: &Args, mut buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let target = a.choice("target");
    let spline = a.choice("curve") == "spline";
    let items: Vec<String> =
        if !a.is_explicit("points") && target != "gain" { vec!["0s=0".to_string()] } else { a.list("points") };
    let sr = buf.sample_rate;
    let n = buf.frames();
    let kf = match target.as_str() {
        "pan" => parse_keyframes(&items, parse_pan)?,
        "pitch" => parse_keyframes(&items, units::parse_cents)?,
        _ => parse_keyframes(&items, units::parse_db)?,
    };
    let pts = resolve_keyframes(&kf, sr, n, ctx.bpm())?;
    let curve = automation(&pts, spline);
    match target.as_str() {
        "pan" => {
            if buf.channels() == 1 {
                let src = buf.data[0].clone();
                let mut l = vec![0.0f32; n];
                let mut r = vec![0.0f32; n];
                for i in 0..n {
                    let p = curve.eval(i as f64).clamp(-100.0, 100.0) / 100.0;
                    let th = (p + 1.0) * PI / 4.0;
                    l[i] = (src[i] as f64 * th.cos()) as f32;
                    r[i] = (src[i] as f64 * th.sin()) as f32;
                }
                buf = AudioBuffer::from_planar(sr, vec![l, r]);
            } else {
                for i in 0..n {
                    let p = curve.eval(i as f64).clamp(-100.0, 100.0) / 100.0;
                    let gl = 1.0 - p.max(0.0);
                    let gr = 1.0 + p.min(0.0);
                    buf.data[0][i] = (buf.data[0][i] as f64 * gl) as f32;
                    buf.data[1][i] = (buf.data[1][i] as f64 * gr) as f32;
                }
            }
        }
        "pitch" => {
            let maxc = pts.iter().map(|p| p.1).fold(f64::MIN, f64::max);
            if pts.iter().all(|p| p.1 == 0.0) {
                return Ok(buf);
            }
            let max_ratio = 2f64.powf(maxc / 1200.0).max(1.0);
            for ch in buf.data.iter_mut() {
                ctx.check_cancel()?;
                *ch = shift_preserving(ch, sr as f64, |t| 2f64.powf(curve.eval(t) / 1200.0), max_ratio);
            }
        }
        _ => {
            let gains: Vec<f32> = (0..n).map(|i| db_to_lin(curve.eval(i as f64)) as f32).collect();
            for ch in buf.data.iter_mut() {
                for (v, g) in ch.iter_mut().zip(&gains) {
                    *v *= *g;
                }
            }
        }
    }
    Ok(buf)
}

fn pitch_bend(a: &Args, buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let spline = a.choice("curve") == "spline";
    let preserve = a.switch("preserve-duration", false);
    let sr = buf.sample_rate;
    let n = buf.frames();
    let kf = parse_keyframes(&a.list("points"), units::parse_cents)?;
    let pts = resolve_keyframes(&kf, sr, n, ctx.bpm())?;
    if pts.iter().all(|p| p.1 == 0.0) || n == 0 {
        return Ok(buf);
    }
    let curve = automation(&pts, spline);
    let maxc = pts.iter().map(|p| p.1).fold(f64::MIN, f64::max);
    // monotone splines never overshoot, so the keyframe maximum bounds the ratio
    let max_ratio = 2f64.powf(maxc / 1200.0).max(1.0);
    let ratio = |pos: f64| 2f64.powf(curve.eval(pos) / 1200.0);
    let mut out = Vec::with_capacity(buf.channels());
    for ch in &buf.data {
        ctx.check_cancel()?;
        out.push(if preserve {
            shift_preserving(ch, sr as f64, ratio, max_ratio)
        } else {
            varispeed(ch, sr as f64, ratio, max_ratio)
        });
    }
    let res = AudioBuffer::from_planar(sr, out);
    ctx.metric("duration_s", (res.duration_s() * 1000.0).round() / 1000.0);
    Ok(res)
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
    fn envelope_gain_hits_keyframes_exactly() {
        let sr = 48000;
        let x = AudioBuffer::from_mono(sr, vec![1.0; 96000]);
        let pts = [("points", "0s=0dB,1s=-6dB,end=-60dB")];
        let y = run(&spec("envelope"), &pts, x.clone()).unwrap();
        assert!((y.data[0][0] - 1.0).abs() < 1e-6);
        assert!((y.data[0][24000] as f64 - db_to_lin(-3.0)).abs() < 1e-5, "halfway in dB");
        assert!((y.data[0][48000] as f64 - db_to_lin(-6.0)).abs() < 1e-5, "on the keyframe");
        assert!((y.data[0][72000] as f64 - db_to_lin(-33.0)).abs() < 1e-5);
        let y = run(&spec("envelope"), &[], x.clone()).unwrap();
        assert!(y.data[0][95999] < 0.0011, "default ends at -60 dB");
        let y = run(&spec("envelope"), &[("points", "0s=-6dB,1s=-6dB")], x.clone()).unwrap();
        assert!((y.data[0][70000] as f64 - db_to_lin(-6.0)).abs() < 1e-6, "holds after last");
        assert_eq!(run(&spec("envelope"), &[("mix", "0%")], x.clone()).unwrap(), x);
        assert!(run(&spec("envelope"), &[("points", "nope")], x).is_err());
    }

    #[test]
    fn envelope_pan_and_pitch() {
        let sr = 48000;
        let x = AudioBuffer::from_mono(sr, vec![1.0; 48000]);
        let y = run(&spec("envelope"), &[("target", "pan"), ("points", "0s=-100,1s=100")], x.clone()).unwrap();
        assert_eq!(y.channels(), 2);
        assert!(y.data[1][0].abs() < 1e-6, "hard left at the first keyframe");
        assert!((y.data[0][24000] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-3, "-3 dB at centre");
        let st = AudioBuffer::from_planar(sr, vec![vec![1.0; 100], vec![1.0; 100]]);
        let y = run(&spec("envelope"), &[("target", "pan"), ("points", "0s=R50")], st).unwrap();
        assert!((y.data[0][10] - 0.5).abs() < 1e-6 && (y.data[1][10] - 1.0).abs() < 1e-6);
        let s = testing::sine(sr, 300.0, 1.0, 0.5);
        let y = run(&spec("envelope"), &[("target", "pitch"), ("points", "0s=+1200cents")], s.clone()).unwrap();
        assert_eq!(y.frames(), s.frames());
        let f = dominant_freq(&y.data[0][6000..42000], sr, 400.0, 800.0);
        assert!((f - 600.0).abs() < 3.0, "{f}");
        // default pitch target with no points is the identity
        assert_eq!(run(&spec("envelope"), &[("target", "pitch")], s.clone()).unwrap(), s);
    }

    #[test]
    fn pitch_bend_tape_and_preserved() {
        let sr = 48000;
        let x = testing::sine(sr, 440.0, 1.0, 0.5);
        let sp = spec("pitch-bend");
        // constant +1200 cents: tape halves the length and doubles the frequency
        let y = run(&sp, &[("points", "0s=+1200cents")], x.clone()).unwrap();
        assert_eq!(y.frames(), 24000);
        assert!((dominant_freq(&y.data[0][2000..22000], sr, 800.0, 960.0) - 880.0).abs() < 1.0);
        let y = run(&sp, &[("points", "0s=-1200cents"), ("preserve-duration", "on")], x.clone()).unwrap();
        assert_eq!(y.frames(), x.frames());
        assert!((dominant_freq(&y.data[0][6000..42000], sr, 150.0, 300.0) - 220.0).abs() < 2.0);
        // the default rises: later audio is higher than earlier
        let y = run(&sp, &[], testing::sine(sr, 440.0, 2.0, 0.5)).unwrap();
        assert!(y.frames() < 96000);
        let early = dominant_freq(&y.data[0][0..9600], sr, 400.0, 520.0);
        let late = dominant_freq(&y.data[0][y.frames() - 9600..], sr, 400.0, 520.0);
        assert!(late > early + 15.0, "{early} -> {late}");
        // zero bend is the identity
        assert_eq!(run(&sp, &[("points", "0s=0")], x.clone()).unwrap(), x);
        assert!(sp.find_param("mix").is_none() && !sp.auto_mix);
    }
}
