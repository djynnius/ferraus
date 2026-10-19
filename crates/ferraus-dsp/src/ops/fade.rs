//! `fade`: fade in and/or out, computed in gain (amplitude).

use ferraus_core::prelude::*;

pub fn ops() -> Vec<OpSpec> {
    vec![OpSpec::transform("fade", Category::Edit, "Fade in and/or out", fade)
        .about("Fades are computed in gain, not dB, so 'linear' is a straight amplitude ramp. 'log' changes the level evenly in dB (60 dB range), which matches the ear's loudness curve and is the recommended choice for musical fades; 'exp' is its mirror (fast start), 'scurve' eases in and out, and 'equal-power' (sine) suits crossfades. With --in 0s and --out 0s the input is returned unchanged.")
        .param(param("in", ParamKind::Time).default("0s").help("Fade-in length (seconds, ms, beats, or mm:ss)"))
        .param(param("out", ParamKind::Time).default("0s").help("Fade-out length"))
        .param(param("curve", ParamKind::Choice).choices(&["linear", "log", "exp", "scurve", "equal-power"]).default("linear").help("Fade shape (gain curve)"))
        .param(param("in-start", ParamKind::Time).default("start").help("Where the fade-in begins; audio before it is silenced"))
        .param(param("out-end", ParamKind::Time).default("end").help("Where the fade-out reaches silence (e.g. -2s = two seconds before the end); audio after it is silenced"))
        .example("ferraus fade song.wav --in 2s --out 4s --curve log -o faded.wav", "Musical fade in and out")
        .example("ferraus fade take.wav --out 500ms --out-end -1s -o cut.wav", "Fade to silence one second before the end")
        .example("ferraus fade loop.wav --in 1b --bpm 120 -o loop_in.wav", "One-beat fade-in at 120 BPM")]
}

/// Fade curve value for progress `t` in 0..1 (0 = silent, 1 = full level).
pub(crate) fn curve(kind: &str, t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    let log = |t: f64| (10f64.powf(3.0 * (t - 1.0)) - 1e-3) / (1.0 - 1e-3);
    match kind {
        "log" => log(t),
        "exp" => 1.0 - log(1.0 - t),
        "scurve" => 0.5 - 0.5 * (std::f64::consts::PI * t).cos(),
        "equal-power" => (std::f64::consts::FRAC_PI_2 * t).sin(),
        _ => t,
    }
}

fn fade(a: &Args, mut buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let sr = buf.sample_rate;
    let n = buf.frames();
    let bpm = ctx.bpm();
    let fin = a.time("in")?.duration_frames(sr, bpm)?;
    let fout = a.time("out")?.duration_frames(sr, bpm)?;
    if fin == 0 && fout == 0 {
        return Ok(buf);
    }
    let kind = a.choice("curve");
    let in_start = a.time("in-start")?.frames(sr, n, bpm)?;
    let out_end = a.time("out-end")?.frames(sr, n, bpm)?;
    let mut gain = vec![1.0f32; n];
    if fin > 0 {
        for (i, g) in gain.iter_mut().enumerate() {
            if i < in_start {
                *g = 0.0;
            } else if i < in_start + fin {
                *g *= curve(&kind, (i - in_start) as f64 / fin as f64) as f32;
            }
        }
    }
    if fout > 0 {
        let start = out_end.saturating_sub(fout);
        for (i, g) in gain.iter_mut().enumerate().skip(start) {
            if i >= out_end {
                *g = 0.0;
            } else {
                *g *= curve(&kind, (out_end - i) as f64 / fout as f64) as f32;
            }
        }
    }
    for ch in &mut buf.data {
        for (x, g) in ch.iter_mut().zip(&gain) {
            *x *= g;
        }
    }
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferraus_core::testing::{run, sine};

    #[test]
    fn zero_fades_are_identity() {
        let spec = &ops()[0];
        let x = sine(48000, 440.0, 0.5, 0.5);
        assert_eq!(run(spec, &[("in", "0s"), ("out", "0s")], x.clone()).unwrap(), x);
        assert_eq!(run(spec, &[], x.clone()).unwrap(), x);
    }

    #[test]
    fn linear_fade_is_an_amplitude_ramp() {
        let spec = &ops()[0];
        let x = AudioBuffer::from_mono(1000, vec![1.0; 1000]);
        let y = run(spec, &[("in", "100ms"), ("out", "100ms")], x).unwrap();
        assert_eq!(y.data[0][0], 0.0);
        assert!((y.data[0][50] - 0.5).abs() < 1e-6);
        assert_eq!(y.data[0][500], 1.0);
        assert!((y.data[0][950] - 0.5).abs() < 1e-6);
        assert!((y.data[0][999] - 0.01).abs() < 1e-6);
    }

    #[test]
    fn curves_hit_endpoints() {
        for k in ["linear", "log", "exp", "scurve", "equal-power"] {
            assert!(curve(k, 0.0).abs() < 1e-9, "{k}");
            assert!((curve(k, 1.0) - 1.0).abs() < 1e-9, "{k}");
        }
        // log is even in dB: halfway is -30 dB (ignoring the tiny offset)
        assert!((20.0 * curve("log", 0.5).log10() + 30.0).abs() < 0.5);
    }

    #[test]
    fn out_end_silences_after() {
        let spec = &ops()[0];
        let x = AudioBuffer::from_mono(1000, vec![1.0; 1000]);
        let y = run(spec, &[("out", "100ms"), ("out-end", "-200ms")], x).unwrap();
        assert_eq!(y.data[0][850], 0.0);
        assert_eq!(y.data[0][650], 1.0);
    }
}
