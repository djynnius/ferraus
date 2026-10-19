//! `dereverb`: late-reverberation suppression by statistical spectral subtraction.
//!
//! The late reverberant power in each bin is predicted from the power `T_l` earlier, decayed
//! by the room's exponential energy decay (Lebart et al. 2001, Habets 2007):
//! `λ(t,k) = e^{-2δ T_l} · P(t − T_l, k)` with `δ = 3 ln 10 / RT60`. A floored, smoothed
//! subtraction gain removes it. RT60 is measured from the input's free decays when not given.

use crate::dsp::fft::Stft;
use crate::dsp::util::median;
use ferraus_core::prelude::*;

pub fn ops() -> Vec<OpSpec> {
    vec![OpSpec::transform("dereverb", Category::Restoration, "Reduce room reverberation", dereverb)
        .about(
            "Predicts the late reverberation in each frequency band from the sound that came \
             before it and the room's decay time, then subtracts it. Direct sound and early \
             reflections are kept, so speech and instruments get drier and closer without \
             sounding gated. The decay time (RT60) is measured from the recording's own pauses \
             unless you give it. Works offline with no model; a model-based path for heavy \
             reverb is planned (model slot denoise/dereverb-*).",
        )
        .param(
            param("rt60", ParamKind::Str)
                .default("auto")
                .help("Room decay time to -60 dB, e.g. 0.8s, or 'auto' to measure it"),
        )
        .param(
            param("amount", ParamKind::Percent)
                .default("100%")
                .help("How much of the predicted reverberation to remove (above 100% removes more aggressively)"),
        )
        .param(
            param("late", ParamKind::Time)
                .default("50ms")
                .help("Where late reverberation starts after the direct sound; earlier reflections are kept"),
        )
        .param(
            param("floor", ParamKind::Gain)
                .default("-18dB")
                .help("Maximum attenuation of any time-frequency cell, which limits artifacts"),
        )
        .param(
            param("link", ParamKind::Switch)
                .default("on")
                .help("Use one gain for all channels so the stereo image does not shift"),
        )
        .best_effort()
        .example("ferraus dereverb lecture.wav -o drier.wav", "Measure the room and remove its tail")
        .example(
            "ferraus dereverb vocal.wav --rt60 1.2s --amount 80% --floor -12dB -o vocal.dry.wav",
            "Gentler removal for a known 1.2 s room",
        )]
}

/// Estimate RT60 from free decays: the median slope of 100 ms linear fits over every
/// steadily falling stretch of the smoothed energy envelope (dB), excluding the noise floor.
/// Short cliffs (direct sound stopping) occupy few windows, so the median follows the
/// reverberant decay.
pub fn estimate_rt60(x: &[f32], sr: u32) -> Option<f64> {
    let hop = (sr as usize / 100).max(1);
    let pow: Vec<f64> =
        x.chunks(hop).map(|c| c.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / c.len().max(1) as f64).collect();
    if pow.len() < 30 {
        return None;
    }
    // 30 ms moving average in the power domain
    let env: Vec<f64> = (0..pow.len())
        .map(|i| {
            let a = i.saturating_sub(1);
            let b = (i + 2).min(pow.len());
            10.0 * (pow[a..b].iter().sum::<f64>() / (b - a) as f64).max(1e-20).log10()
        })
        .collect();
    let mut sorted = env.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let floor = sorted[sorted.len() / 10];
    let w = 10usize;
    let mut slopes = Vec::new();
    for i in 0..env.len().saturating_sub(w) {
        let seg = &env[i..i + w];
        if seg.iter().any(|&v| v < floor + 10.0) {
            continue;
        }
        let n = w as f64;
        let mx = (n - 1.0) / 2.0;
        let my = seg.iter().sum::<f64>() / n;
        let mut num = 0.0;
        let mut den = 0.0;
        for (k, &v) in seg.iter().enumerate() {
            num += (k as f64 - mx) * (v - my);
            den += (k as f64 - mx).powi(2);
        }
        let slope = num / den;
        let ss_tot: f64 = seg.iter().map(|v| (v - my).powi(2)).sum();
        let ss_res: f64 = seg.iter().enumerate().map(|(k, v)| (v - (my + slope * (k as f64 - mx))).powi(2)).sum();
        let r2 = if ss_tot > 0.0 { 1.0 - ss_res / ss_tot } else { 0.0 };
        // falling by at least 3 dB over the window, and close to a straight line
        if slope * n < -3.0 && r2 > 0.9 {
            slopes.push(slope);
        }
    }
    if slopes.len() < 3 {
        return None;
    }
    let s = median(&slopes);
    Some((-60.0 / s * hop as f64 / sr as f64).clamp(0.15, 5.0))
}

fn dereverb(a: &Args, buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    if buf.is_empty() {
        return Ok(buf);
    }
    let sr = buf.sample_rate;
    let rt60 = match a.str_or("rt60", "auto").as_str() {
        "auto" => match estimate_rt60(&buf.mono_mix(), sr) {
            Some(t) => {
                ctx.note("rt60", format!("{t:.2}s (measured)"));
                t
            }
            None => {
                ctx.warn("rt60_not_measured", "no clear free decay found; assuming RT60 0.6s (pass --rt60 to set it)");
                ctx.note("rt60", "0.60s (assumed)");
                0.6
            }
        },
        s => units::parse_duration_s(s).map_err(|e| Error::usage(format!("--rt60: {}", e.message)))?.max(0.05),
    };
    let amount = a.percent("amount")?.max(0.0);
    let late = a.seconds("late")?.max(0.0);
    let floor = db_to_lin(-a.db("floor")?.abs());
    let link = a.switch("link", true);
    let st = Stft::for_rate(sr, 32.0, 4);
    let bins = st.bins();
    let hop_s = st.hop as f64 / sr as f64;
    let delay_frames = ((late / hop_s).round() as usize).max(1);
    let delta = 3.0 * std::f64::consts::LN_10 / rt60;
    let decay = (-2.0 * delta * delay_frames as f64 * hop_s).exp();
    let nch = buf.channels();
    // smoothed power history per channel (or linked)
    let groups = if link { 1 } else { nch };
    let mut history: Vec<Vec<Vec<f64>>> = vec![Vec::new(); groups];
    let mut smooth: Vec<Vec<f64>> = vec![vec![0.0; bins]; groups];
    let mut gains_prev: Vec<Vec<f64>> = vec![vec![1.0; bins]; groups];
    let alpha = 0.7; // power smoothing across frames
    let beta = 1.3 * amount; // over-subtraction
    let mut removed_db = Vec::new();
    let out = st.process(&buf.data, |_, spectra| {
        for g in 0..groups {
            let chans: Vec<usize> = if link { (0..nch).collect() } else { vec![g] };
            let p: Vec<f64> = (0..bins)
                .map(|k| chans.iter().map(|&c| spectra[c][k].norm_sqr()).sum::<f64>() / chans.len() as f64)
                .collect();
            for k in 0..bins {
                // smooth across +-2 bins, then across time
                let a = k.saturating_sub(2);
                let b = (k + 3).min(bins);
                let pf = p[a..b].iter().sum::<f64>() / (b - a) as f64;
                smooth[g][k] = alpha * smooth[g][k] + (1.0 - alpha) * pf;
            }
            let hist = &mut history[g];
            hist.push(smooth[g].clone());
            let past = if hist.len() > delay_frames { Some(&hist[hist.len() - 1 - delay_frames]) } else { None };
            let mut frame_removed = 0.0;
            let mut frame_total = 0.0;
            for k in 0..bins {
                let lambda = past.map(|h| decay * h[k]).unwrap_or(0.0);
                let pk = smooth[g][k].max(1e-20);
                let raw = (1.0 - beta * lambda / pk).max(0.0).sqrt().max(floor);
                // attack fast, release slower, to avoid fluttering
                let gk = if raw < gains_prev[g][k] {
                    0.5 * gains_prev[g][k] + 0.5 * raw
                } else {
                    0.3 * gains_prev[g][k] + 0.7 * raw
                };
                gains_prev[g][k] = gk;
                for &c in &chans {
                    spectra[c][k] = spectra[c][k].scale(gk);
                }
                frame_removed += p[k] * (1.0 - gk * gk);
                frame_total += p[k];
            }
            if frame_total > 1e-12 {
                removed_db.push(frame_removed / frame_total);
            }
            // keep the history bounded
            if hist.len() > delay_frames + 2 {
                hist.remove(0);
            }
        }
    });
    let mean_removed =
        if removed_db.is_empty() { 0.0 } else { removed_db.iter().sum::<f64>() / removed_db.len() as f64 };
    ctx.metric("energy_removed_percent", (mean_removed * 1000.0).round() / 10.0);
    ctx.metric("rt60_s", (rt60 * 100.0).round() / 100.0);
    let n = buf.frames();
    let data = out.into_iter().map(|mut c| {
        c.truncate(n);
        c
    });
    Ok(AudioBuffer::from_planar(sr, data.collect()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferraus_core::testing;

    /// Dry tone bursts convolved with an exponentially decaying noise tail of `rt60`.
    fn reverberant(sr: u32, rt60: f64) -> (AudioBuffer, AudioBuffer) {
        let n = (sr as f64 * 3.0) as usize;
        let mut dry = vec![0.0f32; n];
        for (b, start) in [0.2f64, 1.2, 2.2].iter().enumerate() {
            let s = (start * sr as f64) as usize;
            let f = [330.0, 440.0, 550.0][b];
            for i in 0..(0.25 * sr as f64) as usize {
                let env = (i as f64 / (0.01 * sr as f64)).min(1.0);
                dry[s + i] = (0.5 * env * (2.0 * std::f64::consts::PI * f * i as f64 / sr as f64).sin()) as f32;
            }
        }
        let ir_len = (rt60 * sr as f64) as usize;
        let noise = testing::noise(sr, rt60, 1.0, 7);
        let delta = 3.0 * std::f64::consts::LN_10 / rt60;
        let mut ir: Vec<f32> =
            (0..ir_len).map(|i| noise.data[0][i] * (-delta * i as f64 / sr as f64).exp() as f32 * 0.08).collect();
        ir[0] = 1.0;
        // direct convolution in blocks (short fixture, fine in tests)
        let mut wet = vec![0.0f32; n];
        for (i, &x) in dry.iter().enumerate() {
            if x == 0.0 {
                continue;
            }
            for (j, &h) in ir.iter().enumerate() {
                if i + j < n {
                    wet[i + j] += x * h;
                }
            }
        }
        (AudioBuffer::from_mono(sr, dry), AudioBuffer::from_mono(sr, wet))
    }

    #[test]
    fn measures_rt60() {
        let sr = 16000;
        let (_, wet) = reverberant(sr, 0.8);
        let t = estimate_rt60(&wet.data[0], sr).unwrap();
        assert!((t - 0.8).abs() < 0.25, "rt60 estimate {t}");
    }

    #[test]
    fn reduces_the_tail_and_keeps_the_direct_sound() {
        let sr = 16000;
        let (dry, wet) = reverberant(sr, 0.8);
        let spec = ops().remove(0);
        let out = testing::run(&spec, &[("rt60", "0.8s")], wet.clone()).unwrap();
        // tail region: 0.6 s .. 1.1 s after the first burst ends (no direct sound there)
        let tail = (0.6 * sr as f64) as usize..(1.1 * sr as f64) as usize;
        let before = testing::rms_db(&wet.data[0][tail.clone()]);
        let after = testing::rms_db(&out.data[0][tail]);
        assert!(before - after >= 4.5, "tail reduced by {:.1} dB", before - after);
        // direct sound region: middle of the second burst
        let body = (1.3 * sr as f64) as usize..(1.4 * sr as f64) as usize;
        let lost = testing::rms_db(&wet.data[0][body.clone()]) - testing::rms_db(&out.data[0][body.clone()]);
        assert!(lost < 4.0, "direct sound dropped {lost:.1} dB");
        // closer to the dry signal overall
        let s_in = testing::si_sdr(&dry.data[0], &wet.data[0]);
        let s_out = testing::si_sdr(&dry.data[0], &out.data[0]);
        assert!(s_out > s_in, "SI-SDR {s_in:.1} -> {s_out:.1}");
    }

    #[test]
    fn null_and_edge_cases() {
        let spec = ops().remove(0);
        let x = testing::stereo(testing::sine(16000, 440.0, 0.3, 0.3));
        assert_eq!(testing::run(&spec, &[("mix", "0%")], x.clone()).unwrap(), x);
        let empty = AudioBuffer::new(16000, 2, 0);
        assert!(testing::run(&spec, &[], empty).unwrap().is_empty());
        let out = testing::run(&spec, &[], x.clone()).unwrap();
        assert_eq!(out.frames(), x.frames());
        assert_eq!(out.channels(), 2);
    }
}
