//! Level tools: `leveler` (slow speech leveling) and `dynamics` (transfer-curve dynamics).

use crate::dsp::biquad::{butterworth, Sos};
use crate::dsp::util::{coef, Curve, Follower};
use ferraus_core::prelude::*;

pub fn ops() -> Vec<OpSpec> {
    vec![
        OpSpec::transform(
            "leveler",
            Category::Restoration,
            "Slow speech volume leveler",
            leveler,
        )
        .about(
            "Rides the gain slowly so quiet and loud passages of speech end up near \
             --target. A 300 ms RMS detector drives a gain that moves at most --amount of \
             the way to the target (boosts settle in about 0.5 s, cuts in 0.2 s). Material \
             below --gate is treated as background noise and never boosted; material \
             between --gate and --range (low-level signals) is only boosted with \
             --boost-low on.",
        )
        .param(param("target", ParamKind::Gain).default("-18dB").help("Target RMS level in dBFS"))
        .param(
            param("amount", ParamKind::Percent)
                .default("50%")
                .help("How far to move toward the target, in percent (100% levels fully)"),
        )
        .param(
            param("range", ParamKind::Gain)
                .default("-50dB")
                .help("Level (dBFS RMS) below which signals count as low-level and are not boosted"),
        )
        .param(
            param("boost-low", ParamKind::Switch)
                .default("off")
                .help("Also level signals between --gate and --range (on|off)"),
        )
        .param(
            param("gate", ParamKind::Gain)
                .default("-60dB")
                .help("Noise floor in dBFS RMS: below it the gain is frozen and never boosts"),
        )
        .example("ferraus leveler podcast.wav -o level.wav", "Even out a speech recording")
        .example(
            "ferraus leveler lecture.wav --target -16dB --amount 80% --boost-low on -o level.wav",
            "Stronger leveling for a distant speaker",
        ),
        OpSpec::transform(
            "dynamics",
            Category::Dynamics,
            "Transfer-curve dynamics: compressor, expander, and gate from one curve",
            dynamics,
        )
        .about(
            "Maps the detected input level to an output level through a curve of \
             'input:output' dB points (a slope below 1 compresses, above 1 expands, a drop \
             to -100 gates). Outside the points the curve continues at unity slope. \
             --spline smooths the curve through the points. The detector can be limited to \
             a frequency band with --band; the gain is applied to the full signal, with \
             lookahead so attacks are caught. The rms detector reads a full-scale sine at \
             -3 dBFS.",
        )
        .param(
            param("curve", ParamKind::Str)
                .default("-100:-100,-40:-50,0:-10")
                .help("Transfer curve as input:output dB points, e.g. -100:-100,-20:-20,0:-10"),
        )
        .param(
            param("spline", ParamKind::Switch)
                .default("on")
                .help("Smooth the curve through the points (on) or join them with straight lines (off)"),
        )
        .param(param("attack", ParamKind::Time).default("10ms").help("Detector attack time"))
        .param(param("release", ParamKind::Time).default("100ms").help("Detector release time"))
        .param(
            param("lookahead", ParamKind::Time)
                .default("5ms")
                .help("How far ahead the gain reacts (time); no added latency"),
        )
        .param(
            param("detector", ParamKind::Choice)
                .choices(&["rms", "peak"])
                .default("rms")
                .help("Level detector"),
        )
        .param(
            param("band", ParamKind::Str)
                .default("20Hz..20kHz")
                .help("Frequency band the detector listens to (low..high)"),
        )
        .example(
            "ferraus dynamics vox.wav --curve \"-100:-100,-20:-20,0:-10\" -o comp.wav",
            "2:1 compression above -20 dB",
        )
        .example(
            "ferraus dynamics drums.wav --curve \"-100:-100,-50:-100,-45:-45,0:0\" --spline off --detector peak -o gated.wav",
            "A gate at -50 dB from the same command",
        ),
    ]
}

fn leveler(a: &Args, mut buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let sr = buf.sample_rate as f64;
    let target = a.db("target")?;
    let amount = a.percent("amount")?.clamp(0.0, 1.0);
    let range = a.db("range")?;
    let gate = a.db("gate")?;
    let boost_low = a.switch("boost-low", false);
    let n = buf.frames();
    let nch = buf.channels().max(1) as f64;
    let det = coef(0.3, sr);
    let up = coef(0.5, sr);
    let down = coef(0.2, sr);
    let mut ms = 0.0f64;
    let mut warm = true;
    let mut g = 0.0f64; // dB
    let (mut gmin, mut gmax) = (0.0f64, 0.0f64);
    let mut gains = vec![0.0f32; n];
    for (i, gv) in gains.iter_mut().enumerate() {
        let p: f64 = buf.data.iter().map(|c| (c[i] as f64).powi(2)).sum::<f64>() / nch;
        if warm {
            // initialise the detector on the first 50 ms so the start is not boosted
            let w = (0.05 * sr) as usize;
            let e = (i + w).min(n);
            ms = (i..e)
                .map(|j| buf.data.iter().map(|c| (c[j] as f64).powi(2)).sum::<f64>() / nch)
                .sum::<f64>()
                / (e - i).max(1) as f64;
            warm = false;
        }
        ms = p + det * (ms - p);
        let level = 10.0 * ms.max(1e-20).log10();
        let desired = if level < gate {
            g.min(0.0)
        } else if level < range && !boost_low {
            g
        } else {
            (amount * (target - level)).clamp(-24.0, 24.0)
        };
        let c = if desired > g { up } else { down };
        g = desired + c * (g - desired);
        gmin = gmin.min(g);
        gmax = gmax.max(g);
        *gv = db_to_lin(g) as f32;
    }
    for ch in buf.data.iter_mut() {
        for (v, gv) in ch.iter_mut().zip(&gains) {
            *v *= *gv;
        }
    }
    ctx.metric("gain_min_db", (gmin * 10.0).round() / 10.0);
    ctx.metric("gain_max_db", (gmax * 10.0).round() / 10.0);
    Ok(buf)
}

/// Parse `in:out,in:out` dB points.
fn parse_curve(s: &str) -> Result<Vec<(f64, f64)>> {
    let mut pts = Vec::new();
    for item in units::split_list(s) {
        let (i, o) = item
            .split_once(':')
            .ok_or_else(|| Error::usage(format!("--curve point '{item}' should be in:out in dB, e.g. -20:-20")))?;
        pts.push((units::parse_db(i)?, units::parse_db(o)?));
    }
    if pts.is_empty() {
        return Err(Error::usage("--curve needs at least one in:out point"));
    }
    Ok(pts)
}

/// Transfer function with unity-slope extrapolation outside the points.
pub(crate) struct Transfer {
    curve: Curve,
}

impl Transfer {
    pub(crate) fn new(pts: Vec<(f64, f64)>, spline: bool) -> Transfer {
        Transfer {
            curve: Curve::new(pts, spline),
        }
    }
    pub(crate) fn out(&self, x: f64) -> f64 {
        let xs = &self.curve.xs;
        let ys = &self.curve.ys;
        if x <= xs[0] {
            ys[0] + (x - xs[0])
        } else if x >= xs[xs.len() - 1] {
            ys[ys.len() - 1] + (x - xs[xs.len() - 1])
        } else {
            self.curve.eval(x)
        }
    }
}

fn band_filter(band: &str, sr: f64) -> Result<Option<(Option<Sos>, Option<Sos>)>> {
    let (lo, hi) = units::parse_num_range(band, units::parse_freq)
        .map_err(|e| Error::usage(format!("--band: {}", e.message)))?;
    let (lo, hi) = (lo.min(hi), lo.max(hi));
    let hp = if lo > 25.0 { Some(butterworth(2, lo, sr, true)) } else { None };
    let lp = if hi < sr * 0.45 && hi < 19_000.0 {
        Some(butterworth(2, hi, sr, false))
    } else {
        None
    };
    Ok(if hp.is_none() && lp.is_none() { None } else { Some((hp, lp)) })
}

fn dynamics(a: &Args, mut buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let sr = buf.sample_rate as f64;
    let tf = Transfer::new(parse_curve(&a.str_or("curve", "-100:-100,-40:-50,0:-10"))?, a.switch("spline", true));
    let att = a.seconds("attack")?;
    let rel = a.seconds("release")?;
    let la = (a.seconds("lookahead")? * sr).round().max(0.0) as usize;
    let rms = a.choice("detector") != "peak";
    let filt = band_filter(&a.str_or("band", "20Hz..20kHz"), sr)?;
    let n = buf.frames();
    if n == 0 {
        return Ok(buf);
    }
    // linked sidechain
    let nch = buf.channels() as f64;
    let mut side: Vec<f32> = if rms {
        buf.mono_mix()
    } else {
        (0..n)
            .map(|i| buf.data.iter().map(|c| c[i].abs()).fold(0.0f32, f32::max))
            .collect()
    };
    if let Some((hp, lp)) = &filt {
        // band-limit a mono sum for the detector (peak mode uses the sum too)
        let mut s = buf.mono_mix();
        if let Some(h) = hp {
            h.apply_in_place(&mut s);
        }
        if let Some(l) = lp {
            l.apply_in_place(&mut s);
        }
        side = s;
    }
    let _ = nch;
    // detector: symmetric mean-square (rms) or instant-attack peak with release decay;
    // the computed gain is then smoothed with attack (more reduction) / release
    let avg = coef(att.max(0.002), sr);
    let peak_rel = coef(rel.max(0.001), sr);
    let mut gs = Follower::new(rel, att, sr); // "rising" gain = release, falling = attack
    let mut ms = 0.0f64;
    let mut env = 0.0f64;
    let mut gains = vec![0.0f64; n];
    let mut max_red = 0.0f64;
    let warm = (att.max(0.002) * sr * 3.0) as usize;
    for (i, s) in side.iter().enumerate() {
        let x = *s as f64;
        let lvl = if rms {
            if i == 0 {
                let w = warm.min(n);
                ms = side[..w].iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / w.max(1) as f64;
            }
            ms = x * x + avg * (ms - x * x);
            10.0 * ms.max(1e-20).log10()
        } else {
            if i == 0 {
                let w = warm.min(n);
                env = side[..w].iter().map(|v| (*v as f64).abs()).fold(0.0, f64::max);
            }
            env = x.abs().max(env * peak_rel);
            20.0 * env.max(1e-10).log10()
        };
        let target = (tf.out(lvl) - lvl).clamp(-120.0, 60.0);
        if i == 0 {
            gs.y = target;
        }
        let g = gs.tick(target);
        max_red = max_red.min(g);
        gains[i] = g;
    }
    for ch in buf.data.iter_mut() {
        for (i, v) in ch.iter_mut().enumerate() {
            let g = gains[(i + la).min(n - 1)];
            *v = (*v as f64 * db_to_lin(g)) as f32;
        }
    }
    ctx.metric("max_gain_reduction_db", (-max_red * 10.0).round() / 10.0);
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferraus_core::testing::{self, run};

    fn spec(name: &str) -> OpSpec {
        ops().into_iter().find(|o| o.name == name).unwrap()
    }

    fn seg_db(x: &[f32], a: f64, b: f64, sr: u32) -> f64 {
        testing::rms_db(&x[(a * sr as f64) as usize..(b * sr as f64) as usize])
    }

    #[test]
    fn leveler_reaches_target_and_narrows_spread() {
        let sr = 48000;
        // constant -30 dB RMS tone, amount 100% -> about -18 dB
        let quiet = testing::sine(sr, 300.0, 2.0, 0.0447);
        let out = run(&spec("leveler"), &[("amount", "100%")], quiet).unwrap();
        let l = seg_db(&out.data[0], 1.7, 2.0, sr);
        assert!((l + 18.0).abs() < 1.0, "steady level {l:.2}");
        // 1 s at -30 dB then 1 s at -10 dB: spread shrinks by at least 6 dB
        let mut x = testing::sine(sr, 300.0, 2.0, 1.0);
        for (i, v) in x.data[0].iter_mut().enumerate() {
            *v *= if i < 48000 { 0.0447 } else { 0.447 };
        }
        let out = run(&spec("leveler"), &[("amount", "100%")], x.clone()).unwrap();
        let spread_in = seg_db(&x.data[0], 1.7, 2.0, sr) - seg_db(&x.data[0], 0.7, 1.0, sr);
        let spread_out = seg_db(&out.data[0], 1.7, 2.0, sr) - seg_db(&out.data[0], 0.7, 1.0, sr);
        assert!(spread_out < spread_in - 6.0, "{spread_in:.1} -> {spread_out:.1}");
    }

    #[test]
    fn leveler_never_boosts_noise_floor_and_nulls() {
        let sr = 48000;
        let n = testing::noise(sr, 1.0, 0.0005, 1); // about -70 dBFS
        let out = run(&spec("leveler"), &[], n.clone()).unwrap();
        assert!(out.peak() <= n.peak() * 1.0001);
        assert_eq!(run(&spec("leveler"), &[("mix", "0%")], n.clone()).unwrap(), n);
        let s = AudioBuffer::new(sr, 2, 100);
        assert_eq!(run(&spec("leveler"), &[], s.clone()).unwrap(), s);
    }

    #[test]
    fn dynamics_static_curve_matches_analytic_gain() {
        let sr = 48000;
        let x = testing::sine(sr, 1000.0, 1.0, 1.0);
        let out = run(
            &spec("dynamics"),
            &[("curve", "-100:-100,-20:-20,0:-10"), ("spline", "off")],
            x.clone(),
        )
        .unwrap();
        // rms -3.01 dB -> output -11.505 dB -> gain -8.495 dB
        let g = seg_db(&out.data[0], 0.5, 1.0, sr) - seg_db(&x.data[0], 0.5, 1.0, sr);
        assert!((g + 8.495).abs() < 0.5, "gain {g:.2}");
        // below the knee nothing happens
        let q = testing::sine(sr, 1000.0, 1.0, 0.05);
        let out = run(
            &spec("dynamics"),
            &[("curve", "-100:-100,-20:-20,0:-10"), ("spline", "off")],
            q.clone(),
        )
        .unwrap();
        let g = seg_db(&out.data[0], 0.5, 1.0, sr) - seg_db(&q.data[0], 0.5, 1.0, sr);
        assert!(g.abs() < 0.1, "{g}");
        // defaults: about -10 dB at moderate levels
        let m = testing::sine(sr, 1000.0, 1.0, 0.5);
        let out = run(&spec("dynamics"), &[], m.clone()).unwrap();
        let g = seg_db(&out.data[0], 0.5, 1.0, sr) - seg_db(&m.data[0], 0.5, 1.0, sr);
        assert!((g + 10.0).abs() < 1.0, "default curve gain {g}");
    }

    #[test]
    fn dynamics_gate_band_null() {
        let sr = 48000;
        let sp = spec("dynamics");
        let gate = [("curve", "-100:-100,-50:-100,-45:-45,0:0"), ("spline", "off"), ("detector", "peak")];
        let q = testing::sine(sr, 1000.0, 0.5, 0.001); // -60 dBFS peak
        let out = run(&sp, &gate, q.clone()).unwrap();
        assert!(out.peak() < q.peak() * 0.0105, "gated by 40 dB");
        let loud = testing::sine(sr, 1000.0, 0.5, 0.5);
        let out = run(&sp, &gate, loud.clone()).unwrap();
        assert!((out.data[0][20000] - loud.data[0][20000]).abs() < 1e-3);
        // a detector band away from the tone leaves it alone
        let out = run(
            &sp,
            &[("curve", "-100:-100,-20:-20,0:-10"), ("band", "5kHz..10kHz")],
            testing::sine(sr, 200.0, 0.5, 1.0),
        )
        .unwrap();
        assert!(out.peak() > 0.95);
        assert_eq!(run(&sp, &[("mix", "0%")], loud.clone()).unwrap(), loud);
        assert!(run(&sp, &[("curve", "nope")], loud).is_err());
    }
}
