//! `declip` and `declick`: autoregressive detection and interpolation.

use crate::dsp::lpc;
use crate::dsp::util::median;
use ferraus_core::prelude::*;

pub fn ops() -> Vec<OpSpec> {
    vec![
        OpSpec::transform(
            "declip",
            Category::Restoration,
            "Repair clipped peaks by autoregressive interpolation",
            declip,
        )
        .about(
            "Finds flat-topped (clipped) runs and re-synthesises each one from the samples \
             around it with a least-squares autoregressive model, so the restored waveform \
             rises above the old clip level. The output gain is then lowered by --headroom \
             (and further if needed) so the restored peaks fit below full scale.",
        )
        .param(
            param("threshold", ParamKind::Str)
                .default("auto")
                .help("Clip level as a gain (-0.1dB, 0.98x), or 'auto' to detect flat-topped runs"),
        )
        .param(
            param("max-gap", ParamKind::Time)
                .default("10ms")
                .help("Longest clipped run to repair (time, e.g. 10ms); longer runs are left alone"),
        )
        .param(
            param("headroom", ParamKind::Gain)
                .default("3dB")
                .help("Output gain reduction in dB applied when clipping was repaired, so restored peaks fit"),
        )
        .param(
            param("order", ParamKind::Int)
                .default("32")
                .range("2..128")
                .advanced()
                .help("Autoregressive model order (higher follows complex material better, slower)"),
        )
        .example("ferraus declip loud.wav -o fixed.wav", "Detect and repair clipping automatically")
        .example(
            "ferraus declip take.wav --threshold -0.3dB --headroom 6dB -o fixed.wav",
            "Treat everything above -0.3 dBFS as clipped and leave 6 dB of room",
        )
        .example(
            "ferraus declip interview.wav --max-gap 4ms -o fixed.wav",
            "Repair only short clipped runs (up to 4 ms) and leave longer ones untouched",
        )
        .example(
            "ferraus declip live.wav --threshold 0.95x --order 64 -o fixed.wav",
            "Treat samples above 95% of full scale as clipped and use a higher model order for dense music",
        ),
        OpSpec::transform("declick", Category::Restoration, "Remove clicks, pops, and crackle", declick)
            .about(
                "Fits an autoregressive model to the audio and flags samples whose forward and \
             backward prediction errors are both outliers (a click), then re-synthesises \
             each flagged run by least-squares AR interpolation. Several passes catch \
             clicks hidden by bigger ones. --auto is the vinyl-crackle preset; --big-pops \
             also repairs wider pops.",
            )
            .param(
                param("sensitivity", ParamKind::Percent)
                    .default("30%")
                    .help("Detection sensitivity in percent: higher finds quieter clicks (and risks transients)"),
            )
            .param(
                param("complexity", ParamKind::Int)
                    .default("16")
                    .range("4..64")
                    .help("AR model order used for detection and interpolation"),
            )
            .param(
                param("max-width", ParamKind::Time)
                    .default("2ms")
                    .help("Widest click to repair (time); wider events are treated as music"),
            )
            .param(
                param("big-pops", ParamKind::Switch).default("off").help("Also repair wide pops up to 20 ms (on|off)"),
            )
            .param(
                param("passes", ParamKind::Int).default("4").range("1..16").help("Number of detect-and-repair passes"),
            )
            .param(
                param("auto", ParamKind::Flag)
                    .help("Vinyl crackle preset: higher sensitivity, more passes, higher order"),
            )
            .example("ferraus declick lp-rip.wav -o clean.wav", "Remove clicks with the defaults")
            .example("ferraus declick lp-rip.wav --auto -o clean.wav", "Vinyl crackle preset")
            .example(
                "ferraus declick tape.wav --big-pops on --sensitivity 50% -o clean.wav",
                "Catch quieter clicks and wide pops",
            ),
    ]
}

// ---------------------------------------------------------------------------------
// declip
// ---------------------------------------------------------------------------------

/// Clipping mask for one channel plus the clip level at each masked sample.
struct ClipMask {
    mask: Vec<bool>,
    level: Vec<f32>,
    runs: usize,
    too_long: usize,
}

fn runs_of(x: &[f32], pred: impl Fn(f32) -> bool) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < x.len() {
        if pred(x[i]) {
            let s = i;
            while i < x.len() && pred(x[i]) {
                i += 1;
            }
            out.push((s, i));
        } else {
            i += 1;
        }
    }
    out
}

fn detect_clipping(x: &[f32], explicit: Option<f32>, max_gap: usize) -> ClipMask {
    let n = x.len();
    let mut cm = ClipMask { mask: vec![false; n], level: vec![0.0; n], runs: 0, too_long: 0 };
    for sign in [1.0f32, -1.0] {
        let level = match explicit {
            Some(l) => l,
            None => {
                let peak = x.iter().map(|v| v * sign).fold(0.0f32, f32::max);
                if peak < 0.05 {
                    continue;
                }
                let tol = peak * 1e-5 + 1e-7;
                let runs = runs_of(x, |v| v * sign >= peak - tol);
                let long2 = runs.iter().filter(|(a, b)| b - a >= 2).count();
                let long4 = runs.iter().any(|(a, b)| b - a >= 4);
                if long2 < 3 && !long4 {
                    continue;
                }
                peak - tol
            }
        };
        for (a, b) in runs_of(x, |v| v * sign >= level) {
            if b - a > max_gap {
                cm.too_long += 1;
                continue;
            }
            // a single sample at the level is only "clipped" when a threshold was given
            if explicit.is_none() && b - a < 2 {
                continue;
            }
            cm.runs += 1;
            for i in a..b {
                cm.mask[i] = true;
                cm.level[i] = sign * level;
            }
        }
    }
    cm
}

fn declip(a: &Args, mut buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let sr = buf.sample_rate as f64;
    let max_gap = (a.seconds("max-gap")? * sr).round().max(1.0) as usize;
    let headroom = a.db("headroom")?.abs();
    let order = a.usize("order")?.clamp(2, 128);
    let thr = match a.str_or("threshold", "auto").as_str() {
        "auto" => None,
        s => {
            Some(units::Gain::parse(s).map_err(|e| Error::usage(format!("--threshold: {}", e.message)))?.linear() as f32)
        }
    };
    let mut total = 0usize;
    let mut runs = 0usize;
    let mut too_long = 0usize;
    let nch = buf.channels() as f64;
    for (ci, ch) in buf.data.iter_mut().enumerate() {
        ctx.check_cancel()?;
        let cm = detect_clipping(ch, thr, max_gap);
        runs += cm.runs;
        too_long += cm.too_long;
        if cm.runs == 0 {
            continue;
        }
        total += lpc::repair_channel(ch, &cm.mask, order, 2048);
        // restored samples must not fall below the clip level they replace
        for ((v, m), l) in ch.iter_mut().zip(&cm.mask).zip(&cm.level) {
            if *m && ((*l > 0.0 && *v < *l) || (*l < 0.0 && *v > *l)) {
                *v = *l;
            }
        }
        ctx.set_progress((ci + 1) as f64 / nch);
    }
    let mut gain_db = 0.0;
    if total > 0 {
        let peak = buf.peak() as f64;
        let mut g = db_to_lin(-headroom);
        if peak * g > 0.999 {
            let fit = 0.999 / peak;
            ctx.warn(
                "headroom_raised",
                format!(
                    "restored peaks reach {:+.1} dBFS; output lowered {:.1} dB instead of {:.1} dB so they fit",
                    lin_to_db(peak),
                    -lin_to_db(fit),
                    headroom
                ),
            );
            g = fit;
        }
        buf.scale(g as f32);
        gain_db = lin_to_db(g);
    }
    if too_long > 0 {
        ctx.warn("clip_runs_too_long", format!("{too_long} clipped run(s) longer than --max-gap were left unrepaired"));
    }
    ctx.metric("clipped_runs", runs);
    ctx.metric("samples_repaired", total);
    ctx.metric("gain_db", (gain_db * 100.0).round() / 100.0);
    Ok(buf)
}

// ---------------------------------------------------------------------------------
// declick
// ---------------------------------------------------------------------------------

/// Flag click samples in `x`: both forward and backward AR prediction errors exceed
/// `k` robust standard deviations. Returns merged regions `[start, end)`.
fn detect_clicks(x: &[f32], order: usize, k: f64, max_width: usize) -> Vec<(usize, usize)> {
    let n = x.len();
    let block = 4096usize.max(order * 32);
    let mut flag = vec![false; n];
    let xd: Vec<f64> = x.iter().map(|v| *v as f64).collect();
    let mut start = 0;
    while start < n {
        let end = (start + block).min(n);
        let s0 = start.saturating_sub(order);
        let e0 = (end + order).min(n);
        let seg = &xd[s0..e0];
        if seg.len() <= order * 2 {
            break;
        }
        let a = lpc::ar_autocorr(seg, order);
        let p = a.len() - 1;
        // forward and backward errors over the segment
        let mut ef = vec![0.0; seg.len()];
        let mut eb = vec![0.0; seg.len()];
        for t in 0..seg.len() {
            let mut f = 0.0;
            let mut b = 0.0;
            for (j, aj) in a.iter().enumerate() {
                if t >= j {
                    f += aj * seg[t - j];
                }
                if t + j < seg.len() {
                    b += aj * seg[t + j];
                }
            }
            ef[t] = if t >= p { f } else { 0.0 };
            eb[t] = if t + p < seg.len() { b } else { 0.0 };
        }
        let abs_f: Vec<f64> = ef[p..seg.len() - p].iter().map(|v| v.abs()).collect();
        let sigma = (median(&abs_f) / 0.6745).max(1e-7);
        let thr = k * sigma;
        for i in start..end {
            let t = i - s0;
            if t < p || t + p >= seg.len() {
                continue;
            }
            if ef[t].abs() > thr && eb[t].abs() > thr && ef[t].abs() > 1e-6 {
                flag[i] = true;
            }
        }
        start = end;
    }
    // merge flagged samples into regions, dilated so low-level click tails are included
    const DILATE: usize = 3;
    let mut regions: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < n {
        if flag[i] {
            let s = i;
            while i < n && flag[i] {
                i += 1;
            }
            let a = s.saturating_sub(DILATE);
            let b = (i + DILATE).min(n);
            match regions.last_mut() {
                Some(last) if a <= last.1 + 3 => last.1 = b,
                _ => regions.push((a, b)),
            }
        } else {
            i += 1;
        }
    }
    regions.retain(|(a, b)| b - a <= max_width + 2 * DILATE);
    regions
}

fn declick(a: &Args, mut buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let sr = buf.sample_rate as f64;
    let auto = a.flag("auto");
    let big = a.switch("big-pops", false);
    let mut sens = a.percent("sensitivity")?.clamp(0.0, 1.0);
    let mut order = a.usize("complexity")?.clamp(4, 64);
    let mut passes = a.usize("passes")?.clamp(1, 16);
    let mut width_s = a.seconds("max-width")?;
    if auto {
        if !a.is_explicit("sensitivity") {
            sens = 0.5;
        }
        if !a.is_explicit("passes") {
            passes = 6;
        }
        if !a.is_explicit("complexity") {
            order = 24;
        }
    }
    if big {
        width_s = width_s.max(0.020);
        order = order.max(32);
    }
    let k = 3.0 + 15.0 * (1.0 - sens).powi(2);
    let max_width = (width_s * sr).round().max(1.0) as usize;
    let mut clicks = 0usize;
    let mut repaired = 0usize;
    let nch = buf.channels();
    for (ci, ch) in buf.data.iter_mut().enumerate() {
        for pass in 0..passes {
            ctx.check_cancel()?;
            let regions = detect_clicks(ch, order, k, max_width);
            if regions.is_empty() {
                break;
            }
            if pass == 0 {
                clicks += regions.len();
            }
            let mut mask = vec![false; ch.len()];
            for (s, e) in &regions {
                for m in &mut mask[*s..*e] {
                    *m = true;
                }
            }
            repaired += lpc::repair_channel(ch, &mask, order.max(16), 2048);
            ctx.set_progress((ci * passes + pass + 1) as f64 / (nch * passes) as f64);
        }
    }
    ctx.metric("clicks_found", clicks);
    ctx.metric("samples_repaired", repaired);
    ctx.note("sensitivity", format!("{:.0}%", sens * 100.0));
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use ferraus_core::testing::{self, run};

    fn spec(name: &str) -> OpSpec {
        ops().into_iter().find(|o| o.name == name).unwrap()
    }

    #[test]
    fn declip_restores_clipped_sine_below_1pct_thd() {
        let sr = 48000;
        let clean = testing::sine(sr, 1000.0, 0.5, 2.0);
        let clipped = AudioBuffer::from_mono(sr, clean.data[0].iter().map(|v| v.clamp(-1.0, 1.0)).collect());
        let before = thd(&clipped.data[0][4800..20000], sr, 1000.0);
        assert!(before > 0.05, "fixture should be heavily distorted: {before}");
        let sp = spec("declip");
        let mut c = testing::ctx();
        let out = declip(&testing::args(&sp, &[]), clipped, &mut c).unwrap();
        assert_eq!(c.metrics["clipped_runs"], json!(1000));
        let after = thd(&out.data[0][4800..20000], sr, 1000.0);
        assert!(after < 0.01, "THD after declip {after} (before {before})");
        assert!(out.peak() <= 1.0);
    }

    #[test]
    fn declip_leaves_clean_audio_alone_and_nulls() {
        let x = music(48000, 0.5, 0.5);
        let out = run(&spec("declip"), &[], x.clone()).unwrap();
        assert_eq!(out, x, "no clipping found means no change");
        let clipped = AudioBuffer::from_mono(
            48000,
            testing::sine(48000, 440.0, 0.2, 1.5).data[0].iter().map(|v| v.clamp(-1.0, 1.0)).collect(),
        );
        let out = run(&spec("declip"), &[("mix", "0%")], clipped.clone()).unwrap();
        assert_eq!(out, clipped);
        // silence and stereo
        let s = AudioBuffer::new(48000, 2, 1000);
        assert_eq!(run(&spec("declip"), &[], s.clone()).unwrap(), s);
    }

    #[test]
    fn declick_removes_clicks_and_keeps_clean_regions() {
        for seed in [7, 21] {
            declick_case(seed);
        }
    }

    fn declick_case(seed: u64) {
        let sr = 48000;
        let clean = music(sr, 1.5, 0.4);
        let (dirty, positions) = add_clicks(&clean, 25, seed);
        let out = run(&spec("declick"), &[], dirty.clone()).unwrap();
        // a click counts as removed when the residual error in its region drops by 20 dB
        let mut removed = 0;
        for &(p, w) in &positions {
            let s = p.saturating_sub(2);
            let e = (p + w + 2).min(clean.frames());
            let err_in: f64 = (s..e).map(|i| (dirty.data[0][i] - clean.data[0][i]).powi(2) as f64).sum();
            let err_out: f64 = (s..e).map(|i| (out.data[0][i] - clean.data[0][i]).powi(2) as f64).sum();
            if err_out < err_in * 0.01 {
                removed += 1;
            } else {
                eprintln!("missed click at {p} w {w}: in {err_in:.3e} out {err_out:.3e}");
            }
        }
        let frac = removed as f64 / positions.len() as f64;
        assert!(frac >= 0.95, "removed {removed}/{}", positions.len());
        // click-free regions (more than 5 ms from any click) change by < 0.1 dB
        let guard = 240;
        let mut idx = Vec::new();
        for i in 0..clean.frames() {
            if positions.iter().all(|&(p, w)| i + guard < p || i > p + w + guard) {
                idx.push(i);
            }
        }
        let a: Vec<f32> = idx.iter().map(|&i| dirty.data[0][i]).collect();
        let b: Vec<f32> = idx.iter().map(|&i| out.data[0][i]).collect();
        let d = (testing::rms_db(&a) - testing::rms_db(&b)).abs();
        assert!(d < 0.1, "clean-region level change {d} dB");
        let diff: Vec<f32> = a.iter().zip(&b).map(|(x, y)| x - y).collect();
        assert!(testing::rms_db(&diff) < testing::rms_db(&a) - 40.0);
    }

    #[test]
    fn declick_null_and_edges() {
        let x = music(48000, 0.3, 0.3);
        let out = run(&spec("declick"), &[("mix", "0%")], x.clone()).unwrap();
        assert_eq!(out, x);
        let s = AudioBuffer::new(48000, 2, 2000);
        assert_eq!(run(&spec("declick"), &[], s.clone()).unwrap(), s);
        let tiny = AudioBuffer::from_mono(48000, vec![0.1, -0.2, 0.3]);
        assert_eq!(run(&spec("declick"), &[("auto", "true")], tiny.clone()).unwrap(), tiny);
    }
}
