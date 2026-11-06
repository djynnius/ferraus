//! Spectral dynamics: deresonate (dynamic resonance suppression).

use super::{percent_in, secs, sr};
use crate::resonance::{self, DeresonateParams};
use ferraus_core::prelude::*;

pub fn ops() -> Vec<OpSpec> {
    vec![
        OpSpec::transform("deresonate", Category::Dynamics, "Dynamic resonance suppressor: tame rings or harsh peaks as they happen", deresonate)
            .about("Turns down narrow spectral peaks only while they stick out above their neighbourhood, so a vocal's boomy low-mid build-up or a guitar's harsh 3 kHz ring is tamed as the notes move, where a static EQ cut would follow nothing and dull everything else. Method: an STFT (about 43 ms window, hop 1/4, no latency); each frequency's power is smoothed with --attack/--release before detection, so noise does not count as resonant; the reference for a frequency is its neighbourhood on each side, beyond the peak's own width (about an octave in all at --sharpness 0%, about 1/12 octave at 100%, never under ±250 Hz), measured on a peak envelope that bridges the gaps between harmonics, so a voice's harmonics are compared with each other rather than with the quiet gaps between them; the two sides are averaged, so a slope is not a peak, and more is demanded at a spectral edge such as the lowest harmonic of a note. Whatever stands more than --threshold above that reference is turned down by the excess, at most --depth, with the reduction smoothed across about 1/12 octave (no musical noise) and faded out a quarter octave beyond --band. Only the removed part is resynthesized, so wherever nothing is turned down the output is the input, bit for bit. --link (on) applies one gain curve to every channel so the stereo image stays put; --listen outputs only what is removed. Prefer eq for a fixed tonal change that should always apply, or for a broad bump; prefer deess for sibilance (it keys on the 's' band as a whole and turns the highs down together); use deresonate when the problem frequency moves with the notes or rings only now and then. Metrics give the energy-weighted mean reduction in the band, the largest reduction, and the share of frames where some sixth-octave band was turned down by more than 0.5 dB; result.top lists the sixth-octave bands turned down most (mean and largest). Limits: narrow rings and tonal peaks are turned down by up to the full depth, but broad resonances (wider than about a fifth of an octave) and peaks inside noise-like sound (cymbals, breath) only partly; notes above about C5 have their lowest harmonics compared with the gaps between them and can lose a little level there; below about 150 Hz the frequency resolution (23 Hz at 48 kHz) is too coarse to tell a peak from its surroundings.")
            .param(param("band", ParamKind::FreqRange).default("100Hz..16kHz").help("Frequency range worked on (low..high); the reduction fades out over a quarter octave beyond each edge"))
            .param(param("depth", ParamKind::Gain).default("6dB").help("Largest reduction at any frequency"))
            .param(param("threshold", ParamKind::Gain).default("3dB").help("How far a peak must stand above its neighbourhood before it is turned down (higher: only the most prominent peaks)"))
            .param(param("sharpness", ParamKind::Percent).default("50%").help("Width of the neighbourhood a peak is compared with: 0% about an octave (broad build-ups count too), 100% about 1/12 octave but never under ±250 Hz (only narrow rings count)"))
            .param(param("attack", ParamKind::Time).default("5ms").help("How fast each frequency's level estimate rises, i.e. how fast a reduction engages"))
            .param(param("release", ParamKind::Time).default("80ms").help("How fast each frequency's level estimate falls, i.e. how fast a reduction lets go"))
            .param(param("link", ParamKind::Switch).default("on").help("One gain curve for all channels, from their summed power, so the stereo image stays put (off: each channel on its own)"))
            .param(param("listen", ParamKind::Flag).help("Output only what is removed (input minus output), for checking"))
            .example("ferraus deresonate vocal.wav --band 150Hz..800Hz -o vocal_dr.wav", "Tame a vocal's low-mid build-up as the notes move")
            .example("ferraus deresonate guitar.wav --band 2kHz..6kHz -o guitar_dr.wav", "Soften harsh rings in the presence range")
            .example("ferraus deresonate mix.wav --depth 3dB -o mix_dr.wav", "A gentle pass over a whole mix bus")
            .example("ferraus deresonate vocal.wav --band 2kHz..6kHz --listen -o removed.wav", "Hear only what would be removed")
            .example("ferraus deresonate snare.wav --band 150Hz..1kHz --sharpness 100% --threshold 6dB -o snare_dr.wav", "Only narrow, prominent rings, such as a drum's ring")
            .example("ferraus fx vocal.wav --fx \"deresonate:band=2kHz..6kHz,depth=4dB\" --fx \"compress:preset=vocal\" -o vocal_fx.wav", "As the first stage of a vocal chain"),
    ]
}

fn round(x: f64, places: i32) -> f64 {
    let s = 10f64.powi(places);
    (x * s).round() / s
}

fn deresonate(a: &Args, mut buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let rate = sr(&buf);
    let (lo, hi) = a.freq_range("band")?;
    if !(lo > 0.0 && hi > lo) {
        return Err(Error::usage("--band must be low..high with low above 0 Hz, e.g. 2kHz..6kHz"));
    }
    let threshold = a.db("threshold")?;
    if threshold < 0.0 {
        return Err(Error::usage("--threshold must be 0dB or more (how far a peak stands above its neighbourhood)"));
    }
    let nyquist = rate / 2.0;
    let mut p = DeresonateParams {
        band_hz: (lo, hi.min(nyquist)),
        depth_db: a.db("depth")?.abs(),
        threshold_db: threshold,
        sharpness: percent_in(a, "sharpness", 0.0, 1.0)?,
        attack_s: secs(a, "attack")?,
        release_s: secs(a, "release")?,
        link: a.switch("link", true),
        listen: a.flag("listen"),
    };
    if lo >= nyquist {
        ctx.warn(
            "band_above_nyquist",
            format!("--band starts at {lo} Hz, above the highest frequency of this {rate} Hz file ({nyquist} Hz); nothing was changed"),
        );
        p.depth_db = 0.0;
    } else if hi > nyquist {
        ctx.note("band_resolved", format!("{lo}Hz..{nyquist}Hz"));
    }
    let st = resonance::deresonate(&mut buf.data, rate, &p);
    let mean = round(st.reduction_mean_db, 2);
    let max = round(st.reduction_max_db, 2);
    let active = round(st.active_share, 3);
    ctx.metric("reduction_mean_db", mean);
    ctx.metric("reduction_max_db", max);
    ctx.metric("active_share", active);
    let top: Vec<Value> = st
        .top
        .iter()
        .map(|b| json!({"hz": b.hz.round(), "mean_db": round(b.mean_db, 2), "max_db": round(b.max_db, 2)}))
        .collect();
    ctx.set_result(json!({
        "band_hz": [round(p.band_hz.0, 1), round(p.band_hz.1, 1)],
        "reduction_mean_db": mean,
        "reduction_max_db": max,
        "active_share": active,
        "top": top,
    }));
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferraus_core::testing::{add, noise, run, run_ctx, sine, stereo, tone_level};

    fn spec() -> OpSpec {
        ops().remove(0)
    }

    /// Noise with a 3 kHz ring well above it.
    fn ringing(secs: f64) -> AudioBuffer {
        add(&noise(48000, secs, 0.05, 2), &sine(48000, 3000.0, secs, 0.1))
    }

    #[test]
    fn documented_with_runnable_examples() {
        let s = spec();
        assert!(s.summary.len() <= 70, "{}", s.summary.len());
        assert!(s.examples.len() >= 3);
        for p in &s.params {
            assert!(!p.help.is_empty(), "--{}", p.name);
        }
        // the generic --mix is added (auto-mix transform)
        assert!(s.effective_params().iter().any(|p| p.name == "mix"));
        for ex in &s.examples {
            assert!(ex.cmd.starts_with("ferraus "), "{}", ex.cmd);
        }
    }

    #[test]
    fn metrics_and_result_describe_what_was_tamed() {
        let x = stereo(ringing(2.0));
        let (y, c) = run_ctx(&spec(), &[], x.clone()).unwrap();
        assert_eq!(y.frames(), x.frames());
        assert_eq!(y.channels(), 2);
        let m = &c.metrics;
        assert!(m["reduction_max_db"].as_f64().unwrap() > 5.9, "{m:?}");
        assert!(m["reduction_mean_db"].as_f64().unwrap() > 1.0, "{m:?}");
        assert!(m["active_share"].as_f64().unwrap() > 0.9, "{m:?}");
        let r = c.result.as_ref().expect("result");
        assert_eq!(r["band_hz"], json!([100.0, 16000.0]));
        assert_eq!(r["reduction_max_db"], m["reduction_max_db"]);
        let top = r["top"].as_array().unwrap();
        assert!(!top.is_empty() && top.len() <= 5);
        let hz = top[0]["hz"].as_f64().unwrap();
        assert!((hz - 3000.0).abs() < 400.0, "{top:?}");
        assert!(top[0]["max_db"].as_f64().unwrap() >= top[0]["mean_db"].as_f64().unwrap());
        // the ring itself is turned down by about the depth
        let lvl = |b: &AudioBuffer| tone_level(&b.data[0][24000..], 48000, 3000.0);
        let d = 20.0 * (lvl(&y) / lvl(&x)).log10();
        assert!(d < -4.5, "{d}");
    }

    #[test]
    fn listen_and_mix() {
        let x = ringing(1.0);
        let y = run(&spec(), &[], x.clone()).unwrap();
        let l = run(&spec(), &[("listen", "true")], x.clone()).unwrap();
        for i in 0..x.frames() {
            assert!((y.data[0][i] + l.data[0][i] - x.data[0][i]).abs() < 1e-6);
        }
        assert_eq!(run(&spec(), &[("mix", "0%")], x.clone()).unwrap(), x);
        let half = run(&spec(), &[("mix", "50%")], x.clone()).unwrap();
        let i = 20000;
        assert!((half.data[0][i] - 0.5 * (x.data[0][i] + y.data[0][i])).abs() < 1e-6);
        // depth 0 is the input itself
        assert_eq!(run(&spec(), &[("depth", "0dB")], x.clone()).unwrap(), x);
    }

    #[test]
    fn bad_values_are_usage_errors() {
        let x = ringing(0.2);
        for (k, v) in [("band", "6kHz..2kHz"), ("threshold", "-3dB"), ("sharpness", "150%"), ("attack", "-5ms")] {
            let e = run(&spec(), &[(k, v)], x.clone()).unwrap_err();
            assert_eq!(e.exit_code, 2, "--{k} {v}: {}", e.message);
        }
    }

    #[test]
    fn band_beyond_nyquist() {
        // an 8 kHz file: the default band is clamped to 4 kHz and noted
        let x = add(&noise(8000, 1.0, 0.05, 3), &sine(8000, 1000.0, 1.0, 0.1));
        let (y, c) = run_ctx(&spec(), &[], x.clone()).unwrap();
        assert_eq!(c.notes["band_resolved"], json!("100Hz..4000Hz"));
        assert_eq!(c.result.as_ref().unwrap()["band_hz"], json!([100.0, 4000.0]));
        assert!(tone_level(&y.data[0][4000..], 8000, 1000.0) < tone_level(&x.data[0][4000..], 8000, 1000.0) * 0.7);
        // a band entirely above it changes nothing and warns
        let (y, c) = run_ctx(&spec(), &[("band", "5kHz..8kHz")], x.clone()).unwrap();
        assert_eq!(y, x);
        assert!(c.warnings.iter().any(|w| w.code == "band_above_nyquist"), "{:?}", c.warnings);
    }
}
