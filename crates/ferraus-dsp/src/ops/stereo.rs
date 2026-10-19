//! Stereo placement commands: pan (static and keyframed), autopan, widen.

use super::{degrees, percent_in, rate_hz, sr};
use crate::biquad::{lr4_allpass, Biquad, Cascade};
use crate::modulation::{Lfo, Shape};
use crate::pan::{balance_gains, pan_gains, parse_position, Curve, Keyframes, PanLaw};
use ferraus_core::prelude::*;

const LAWS: &[&str] = &["-3dB", "-4.5dB", "-6dB", "linear"];

pub fn ops() -> Vec<OpSpec> {
    vec![
        OpSpec::transform("pan", Category::Panning, "Place a source in the stereo field (static or keyframed)", pan)
            .about("--mode pan places a mono (or mono-summed) source with the chosen pan law: -3dB is constant power, -6dB constant voltage, -4.5dB in between, linear keeps the centre at full level. --mode balance instead scales the left and right channels of a stereo source, keeping its image. Movement over time uses --points (same keyframe syntax as envelope: 0s=-100,4s=0,8s=100, with --curve linear or spline) or --from/--to/--over; keyframe values are hit exactly at their sample. Output is stereo.")
            .param(param("position", ParamKind::Str).default("0").help("-100 (hard left) .. 100 (hard right), or left, center, right, L30, R60"))
            .param(param("law", ParamKind::Choice).choices(LAWS).default("-3dB").help("Pan law: level of a centred source in each channel"))
            .param(param("mode", ParamKind::Choice).choices(&["pan", "balance"]).default("pan").help("pan: place a mono/mono-summed source; balance: scale the channels of a stereo source"))
            .param(param("points", ParamKind::Str).help("Keyframes time=position, e.g. 0s=-100,4s=0,8s=100 (time may be end or -2s)"))
            .param(param("curve", ParamKind::Choice).choices(&["linear", "spline"]).default("linear").help("Interpolation between keyframes"))
            .param(param("from", ParamKind::Str).help("Start position of a sweep (with --to)"))
            .param(param("to", ParamKind::Str).help("End position of a sweep"))
            .param(param("over", ParamKind::Range).default("0s..end").help("Time range of the --from/--to sweep"))
            .example("ferraus pan gtr.wav --position L30 -o gtr_l30.wav", "Place a guitar 30% left")
            .example("ferraus pan fx.wav --points 0s=-100,4s=0,8s=100 --curve spline -o sweep.wav", "Sweep left to right over 8 seconds")
            .example("ferraus pan mix.wav --mode balance --position 20 -o mix_bal.wav", "Shift a stereo mix's balance to the right"),
        OpSpec::transform("autopan", Category::Panning, "Cyclic automatic panning", autopan)
            .about("Moves the source left and right with an LFO. Mono sources are panned with the pan law; stereo sources are balanced so their image is kept. --rate accepts Hz or beat values (1/2 = one cycle per half note) with --bpm.")
            .param(param("rate", ParamKind::Str).default("0.5Hz").help("Cycle rate in Hz, or a beat value such as 1/2"))
            .param(param("depth", ParamKind::Percent).default("100%").help("Swing width (100% = hard left to hard right)"))
            .param(param("shape", ParamKind::Choice).choices(&["sine", "triangle", "square"]).default("sine").help("LFO waveform"))
            .param(param("phase", ParamKind::Str).default("0deg").help("Starting phase in degrees (90deg starts hard right)"))
            .param(param("law", ParamKind::Choice).choices(LAWS).default("-3dB").help("Pan law"))
            .example("ferraus autopan keys.wav -o keys_auto.wav", "Slow sine autopan")
            .example("ferraus autopan hats.wav --rate 1/4 --bpm 124 --shape triangle --depth 60% -o hats_auto.wav", "Tempo-synced triangle autopan"),
        OpSpec::transform("widen", Category::Panning, "Stereo widener (mid/side) with mono bass", widen)
            .about("Scales the side (L-R) signal by --amount while keeping everything below --mono-below in mono, so the low end stays solid and mono-compatible. --center-pan moves the centre (mid) image like Audition's Stereo Expander. A mono input has no side signal; only --center-pan affects it.")
            .param(param("amount", ParamKind::Percent).default("130%").help("Side level: 0% mono, 100% unchanged, 200% very wide"))
            .param(param("mono-below", ParamKind::Freq).default("120Hz").help("Collapse the side signal below this frequency (0Hz = off)"))
            .param(param("center-pan", ParamKind::Str).default("0").help("Pan of the centre image, -100..100 or L30/R60"))
            .example("ferraus widen mix.wav -o wider.wav", "Default 130% width with mono bass")
            .example("ferraus widen pad.wav --amount 180% --mono-below 200Hz --center-pan R10 -o pad_wide.wav", "Extra-wide pad, centre nudged right"),
    ]
}

/// Per-frame position track (−100..100) from --points, --from/--to, or --position.
fn position_track(a: &Args, rate: u32, n: usize, ctx: &OpCtx) -> Result<Option<Vec<f64>>> {
    let curve = Curve::parse(&a.choice("curve"));
    if let Some(p) = a.str("points") {
        let k = Keyframes::parse(&p, rate, n, ctx.bpm(), curve, parse_position)?;
        return Ok(Some(k.render(n)));
    }
    if a.has("from") || a.has("to") {
        let base = parse_position(&a.str_or("position", "0"))?;
        let from = a.str("from").map(|s| parse_position(&s)).transpose()?.unwrap_or(base);
        let to = a.str("to").map(|s| parse_position(&s)).transpose()?.unwrap_or(base);
        let over = a.range("over")?.unwrap_or(units::TimeRange { start: units::Time::Start, end: units::Time::End });
        let (s, e) = over.frames(rate, n, ctx.bpm())?;
        let k = Keyframes::new(vec![(s, from), (e, to)], curve)?;
        return Ok(Some(k.render(n)));
    }
    Ok(None)
}

fn pan(a: &Args, buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let law = PanLaw::parse(&a.choice("law"))?;
    ctx.note("law_resolved", law.name());
    let n = buf.frames();
    let balance = a.choice("mode") == "balance";
    let track = position_track(a, buf.sample_rate, n, ctx)?;
    let static_pos = parse_position(&a.str_or("position", "0"))?;
    let gains_fn = |p: f64| if balance { balance_gains(p / 100.0, law) } else { pan_gains(p / 100.0, law) };
    let (l_in, r_in): (Vec<f32>, Vec<f32>) = if balance {
        let st = buf.with_channels(2);
        (st.data[0].clone(), st.data[1].clone())
    } else {
        if buf.channels() > 1 {
            ctx.warn("mono_summed", "pan mode sums a stereo source to mono before placing it; use --mode balance to keep the stereo image");
        }
        let m = buf.mono_mix();
        (m.clone(), m)
    };
    let mut l = Vec::with_capacity(n);
    let mut r = Vec::with_capacity(n);
    match track {
        Some(t) => {
            for i in 0..n {
                let (gl, gr) = gains_fn(t[i]);
                l.push(l_in[i] * gl as f32);
                r.push(r_in[i] * gr as f32);
            }
        }
        None => {
            let (gl, gr) = gains_fn(static_pos);
            l.extend(l_in.iter().map(|v| v * gl as f32));
            r.extend(r_in.iter().map(|v| v * gr as f32));
        }
    }
    Ok(AudioBuffer::from_planar(buf.sample_rate, vec![l, r]))
}

fn autopan(a: &Args, buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let rate = sr(&buf);
    let f = rate_hz(a, "rate", ctx)?;
    let depth = percent_in(a, "depth", 0.0, 1.0)?;
    let shape = Shape::parse(&a.choice("shape"));
    let phase = degrees(a, "phase")? / 360.0;
    let law = PanLaw::parse(&a.choice("law"))?;
    let mut lfo = Lfo::new(f, rate, shape, phase);
    let stereo_in = buf.channels() >= 2;
    let st = buf.with_channels(2);
    let mono = buf.mono_mix();
    let n = buf.frames();
    let mut l = Vec::with_capacity(n);
    let mut r = Vec::with_capacity(n);
    for i in 0..n {
        let p = depth * lfo.tick();
        if stereo_in {
            let (gl, gr) = balance_gains(p, law);
            l.push(st.data[0][i] * gl as f32);
            r.push(st.data[1][i] * gr as f32);
        } else {
            let (gl, gr) = pan_gains(p, law);
            l.push(mono[i] * gl as f32);
            r.push(mono[i] * gr as f32);
        }
    }
    Ok(AudioBuffer::from_planar(buf.sample_rate, vec![l, r]))
}

fn widen(a: &Args, buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let rate = sr(&buf);
    let amount = percent_in(a, "amount", 0.0, 3.0)? as f32;
    let mono_below = a.freq("mono-below")?;
    let cp = parse_position(&a.str_or("center-pan", "0"))?;
    if buf.channels() < 2 {
        if cp == 0.0 {
            ctx.warn("mono_input", "widen has no side signal to widen in a mono input; returned unchanged");
            return Ok(buf);
        }
    } else if (amount - 1.0).abs() < 1e-9 && mono_below <= 0.0 && cp == 0.0 {
        return Ok(buf);
    }
    let st = buf.with_channels(2);
    let (c0l, c0r) = pan_gains(0.0, PanLaw::Minus3);
    let (pl, pr) = pan_gains(cp / 100.0, PanLaw::Minus3);
    let (gl, gr) = ((pl / c0l) as f32, (pr / c0r) as f32);
    let n = buf.frames();
    let mut side_hp = (mono_below > 0.0).then(|| Cascade::linkwitz_riley_highpass(4, mono_below, rate));
    let mut mid_ap = (mono_below > 0.0).then(|| Biquad::new(lr4_allpass(mono_below, rate)));
    let mut l = Vec::with_capacity(n);
    let mut r = Vec::with_capacity(n);
    for i in 0..n {
        let (x0, x1) = (st.data[0][i], st.data[1][i]);
        let mut m = 0.5 * (x0 + x1);
        let mut s = 0.5 * (x0 - x1);
        if let (Some(hp), Some(ap)) = (&mut side_hp, &mut mid_ap) {
            s = hp.process(s);
            m = ap.process(m);
        }
        s *= amount;
        l.push(m * gl + s);
        r.push(m * gr - s);
    }
    Ok(AudioBuffer::from_planar(buf.sample_rate, vec![l, r]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferraus_core::testing::{noise, run, sine};

    fn spec(name: &str) -> OpSpec {
        ops().into_iter().find(|o| o.name == name).unwrap()
    }

    #[test]
    fn hard_left_silences_right() {
        let y = run(&spec("pan"), &[("position", "-100")], sine(48000, 440.0, 0.2, 0.5)).unwrap();
        assert_eq!(y.channels(), 2);
        assert!(y.data[1].iter().all(|v| *v == 0.0));
        assert!(crate::util::rms(&y.data[0]) > 0.3);
        let y = run(&spec("pan"), &[("position", "left"), ("law", "-6dB")], sine(48000, 440.0, 0.2, 0.5)).unwrap();
        assert!(y.data[1].iter().all(|v| *v == 0.0));
    }

    #[test]
    fn minus_3db_law_keeps_power_constant() {
        let x = AudioBuffer::from_mono(1000, vec![0.5; 100]);
        let p0 = 0.25f64;
        for pos in (-100..=100).step_by(5) {
            let ps = pos.to_string();
            let y = run(&spec("pan"), &[("position", &ps)], x.clone()).unwrap();
            let pw = (y.data[0][50] as f64).powi(2) + (y.data[1][50] as f64).powi(2);
            assert!((10.0 * (pw / p0).log10()).abs() < 0.1, "{pos}");
        }
    }

    #[test]
    fn keyframes_are_sample_accurate() {
        let x = AudioBuffer::from_mono(1000, vec![1.0; 3000]);
        let y = run(&spec("pan"), &[("points", "0s=-100,1s=0,2s=100"), ("curve", "spline")], x).unwrap();
        let c = std::f32::consts::FRAC_1_SQRT_2;
        assert_eq!(y.data[1][0], 0.0);
        assert!((y.data[0][1000] - c).abs() < 1e-6 && (y.data[1][1000] - c).abs() < 1e-6);
        assert!(y.data[0][2000].abs() < 1e-6 && (y.data[1][2000] - 1.0).abs() < 1e-6);
        assert!(y.data[0][999] > y.data[0][1000]);
        let y = run(&spec("pan"), &[("from", "L50"), ("to", "R50"), ("over", "1s..2s")], AudioBuffer::from_mono(1000, vec![1.0; 3000])).unwrap();
        let (gl, gr) = pan_gains(-0.5, PanLaw::Minus3);
        assert!((y.data[0][1000] as f64 - gl).abs() < 1e-6 && (y.data[1][1000] as f64 - gr).abs() < 1e-6);
    }

    #[test]
    fn balance_keeps_stereo_and_centre_is_identity() {
        let x = ferraus_core::testing::stereo(noise(48000, 0.1, 0.3, 2));
        let y = run(&spec("pan"), &[("mode", "balance")], x.clone()).unwrap();
        assert_eq!(y, x);
        let y = run(&spec("pan"), &[("mode", "balance"), ("position", "100")], x.clone()).unwrap();
        assert!(y.data[0].iter().all(|v| *v == 0.0));
        assert_eq!(y.data[1], x.data[1]);
    }

    #[test]
    fn autopan_moves() {
        let x = AudioBuffer::from_mono(1000, vec![1.0; 4000]);
        let y = run(&spec("autopan"), &[("rate", "1Hz"), ("phase", "90deg")], x).unwrap();
        // starts hard right
        assert!(y.data[0][0].abs() < 1e-6 && (y.data[1][0] - 1.0).abs() < 1e-6);
        assert!(y.data[0][500] > 0.99);
    }

    #[test]
    fn widen_scales_side_and_keeps_mono_bass() {
        let mut l = noise(48000, 0.5, 0.3, 1);
        let r = noise(48000, 0.5, 0.3, 2);
        l.data.push(r.data[0].clone());
        let y = run(&spec("widen"), &[("amount", "200%"), ("mono-below", "0Hz")], l.clone()).unwrap();
        let side = |b: &AudioBuffer, i: usize| 0.5 * (b.data[0][i] - b.data[1][i]);
        assert!((side(&y, 1000) - 2.0 * side(&l, 1000)).abs() < 1e-5);
        // 50 Hz side content collapses to mono
        let mut s = sine(48000, 50.0, 1.0, 0.5);
        let neg: Vec<f32> = s.data[0].iter().map(|v| -v).collect();
        s.data.push(neg);
        let y = run(&spec("widen"), &[], s).unwrap();
        let resid = crate::util::rms(&y.data[0][24000..]);
        assert!(resid < 0.05, "{resid}");
    }
}
