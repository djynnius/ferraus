//! Filters: `graphic-eq` (linear-phase FIR), `fft-filter` (drawn curve), and `filter`
//! (scientific IIR designs).

use crate::dsp::design::{self, Band, Design};
use crate::dsp::fft::{design_fir, fir_response, fir_zero_delay};
use crate::dsp::util::Curve;
use ferraus_core::prelude::*;

pub fn ops() -> Vec<OpSpec> {
    vec![
        OpSpec::transform(
            "graphic-eq",
            Category::Tone,
            "Linear-phase graphic equalizer (10, 20, or 30 bands)",
            graphic_eq,
        )
        .about(
            "Sets a gain at each ISO band centre (octave, half-octave, or third-octave), joins \
             them with a smooth curve over log-frequency, and applies it as one linear-phase \
             FIR filter, so there is no phase distortion and the output stays aligned with \
             the input. --accuracy is the FIR length: longer resolves low bands more \
             precisely. The metrics report the realised gain at each centre.",
        )
        .param(
            param("bands", ParamKind::Choice)
                .choices(&["10", "20", "30"])
                .default("10")
                .help("Number of bands: 10 (octaves), 20 (half octaves), or 30 (third octaves)"),
        )
        .param(
            param("gains", ParamKind::List)
                .default("0")
                .help("Gain per band in dB from the lowest band up, e.g. \"+2,0,-3\"; missing bands are 0"),
        )
        .param(
            param("accuracy", ParamKind::Time)
                .default("1000ms")
                .help("FIR length (time); longer is more precise in the low bands"),
        )
        .example("ferraus graphic-eq mix.wav --gains \"+3,+2,0,0,-2,0,0,+1,+2,+2\" -o eq.wav", "Ten-band smile curve")
        .example(
            "ferraus graphic-eq vox.wav --bands 30 --gains \"0,0,0,0,-3,-3\" --accuracy 500ms -o eq.wav",
            "Third-octave cut at 63 and 80 Hz with a shorter filter",
        )
        .example(
            "ferraus graphic-eq vox.wav --bands 20 --gains \"0,0,-4,-2,0,0,0,0,0,0,0,0,0,+2,+3\" -o vox_eq.wav",
            "Half-octave EQ: cut mud at 63-88 Hz and lift presence at 2.8-4 kHz",
        )
        .example(
            "ferraus graphic-eq tape.wav --gains \"0,0,0,0,0,0,0,0,-3,-6\" -o tape_eq.wav",
            "Roll off the top two octave bands (8 and 16 kHz) to tame hiss",
        ),
        OpSpec::transform("fft-filter", Category::Tone, "Filter by a drawn magnitude curve", fft_filter)
            .about(
                "Draws a magnitude response from 'frequency:gain' points (joined linearly over \
             log-frequency and held flat beyond the ends), designs a linear-phase FIR of half \
             the FFT size, and applies it by FFT overlap-add without shifting the signal.",
            )
            .param(
                param("curve", ParamKind::Str)
                    .default("20Hz:0dB,120Hz:-6dB,1kHz:0dB,8kHz:+3dB")
                    .help("Points as frequency:gain, e.g. \"20Hz:0dB,120Hz:-6dB,1kHz:0dB,8kHz:+3dB\""),
            )
            .param(
                param("fft", ParamKind::Int)
                    .default("4096")
                    .range("256..65536")
                    .help("FFT size (power of two); the filter is half as long"),
            )
            .example(
                "ferraus fft-filter in.wav --curve \"20Hz:-30dB,80Hz:0dB,12kHz:0dB,16kHz:-30dB\" -o band.wav",
                "Steep band-limit between 80 Hz and 12 kHz",
            )
            .example(
                "ferraus fft-filter in.wav --curve \"200Hz:0dB,400Hz:-8dB,800Hz:0dB\" --fft 16384 -o out.wav",
                "Narrow cut around 400 Hz with a long filter",
            )
            .example(
                "ferraus fft-filter voice.wav --curve \"100Hz:-12dB,300Hz:0dB,3kHz:0dB,5kHz:-12dB\" -o radio.wav",
                "A gentle radio-voice band shape",
            )
            .example(
                "ferraus fft-filter mix.wav --curve \"40Hz:0dB,60Hz:-4dB,90Hz:0dB\" --fft 32768 -o mix_dip.wav",
                "A shallow bass dip, with a long FFT for resolution in the low end",
            ),
        OpSpec::transform(
            "filter",
            Category::Tone,
            "Scientific filters: Butterworth, Chebyshev, Bessel, elliptic",
            filter,
        )
        .about(
            "Designs an analog prototype, transforms it to the requested band, maps it to \
             digital with the prewarped bilinear transform, and runs it as a cascade of \
             second-order sections. --freq is the -3 dB point for butterworth and bessel, the \
             passband edge (end of the ripple) for chebyshev1 and elliptic, and the stopband \
             edge for chebyshev2. Band filters take low..high and have twice --order poles. \
             The metrics report the realised response and group delay at a few frequencies.",
        )
        .param(
            param("design", ParamKind::Choice)
                .choices(&["butterworth", "chebyshev1", "chebyshev2", "bessel", "elliptic"])
                .default("butterworth")
                .help("Filter family"),
        )
        .param(
            param("type", ParamKind::Choice)
                .choices(&["lowpass", "highpass", "bandpass", "bandstop"])
                .default("lowpass")
                .help("Response type"),
        )
        .param(
            param("freq", ParamKind::Str).default("auto").help(
                "Cutoff (1kHz) or band (300Hz..3.4kHz); auto = 8 kHz LP, 80 Hz HP, 300 Hz-3.4 kHz BP, 50-70 Hz BS",
            ),
        )
        .param(param("order", ParamKind::Int).default("4").range("1..24").help("Prototype order (steepness)"))
        .param(param("ripple", ParamKind::Gain).default("0.5dB").help("Passband ripple in dB (chebyshev1, elliptic)"))
        .param(
            param("stopband", ParamKind::Gain)
                .default("60dB")
                .help("Stopband attenuation in dB (chebyshev2, elliptic)"),
        )
        .example(
            "ferraus filter in.wav --design elliptic --type lowpass --freq 4kHz --order 6 -o lp.wav",
            "Very steep elliptic low-pass",
        )
        .example(
            "ferraus filter in.wav --design bessel --type highpass --freq 40Hz -o hp.wav",
            "Phase-friendly Bessel high-pass",
        )
        .example(
            "ferraus filter in.wav --design chebyshev1 --type bandpass --freq 300Hz..3.4kHz --ripple 1dB -o phone.wav",
            "Telephone band",
        ),
    ]
}

const ISO10: [f64; 10] = [31.5, 63.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0];
const ISO20: [f64; 20] = [
    31.5, 44.0, 63.0, 88.0, 125.0, 177.0, 250.0, 355.0, 500.0, 710.0, 1000.0, 1400.0, 2000.0, 2800.0, 4000.0, 5600.0,
    8000.0, 11200.0, 16000.0, 22400.0,
];
const ISO30: [f64; 30] = [
    25.0, 31.5, 40.0, 50.0, 63.0, 80.0, 100.0, 125.0, 160.0, 200.0, 250.0, 315.0, 400.0, 500.0, 630.0, 800.0, 1000.0,
    1250.0, 1600.0, 2000.0, 2500.0, 3150.0, 4000.0, 5000.0, 6300.0, 8000.0, 10000.0, 12500.0, 16000.0, 20000.0,
];

/// Gain curve (dB) over log-frequency through `(freq, dB)` points.
fn log_curve(pts: &[(f64, f64)], spline: bool) -> impl Fn(f64) -> f64 {
    let c = Curve::new(pts.iter().map(|(f, g)| (f.max(1e-3).ln(), *g)).collect(), spline);
    move |f: f64| c.eval(f.max(1e-3).ln())
}

fn graphic_eq(a: &Args, mut buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let sr = buf.sample_rate;
    let nyq = sr as f64 / 2.0;
    let centres: &[f64] = match a.choice("bands").as_str() {
        "20" => &ISO20,
        "30" => &ISO30,
        _ => &ISO10,
    };
    let gains: Vec<f64> = a
        .list("gains")
        .iter()
        .map(|g| units::parse_db(g))
        .collect::<Result<_>>()
        .map_err(|e| Error::usage(format!("--gains: {}", e.message)))?;
    if gains.len() > centres.len() {
        return Err(Error::usage(format!(
            "--gains has {} values but there are only {} bands",
            gains.len(),
            centres.len()
        )));
    }
    let mut pts = Vec::new();
    for (i, f) in centres.iter().enumerate() {
        let g = gains.get(i).copied().unwrap_or(0.0);
        if *f < nyq * 0.95 {
            pts.push((*f, g));
        } else if g != 0.0 {
            ctx.warn(
                "band_above_nyquist",
                format!("the {f} Hz band is above this sample rate's range and was ignored"),
            );
        }
    }
    if pts.iter().all(|p| p.1 == 0.0) {
        return Ok(buf);
    }
    let curve = log_curve(&pts, true);
    let taps = ((a.seconds("accuracy")? * sr as f64).round() as usize).clamp(255, 4 * sr as usize);
    let h = design_fir(taps, sr, |f| db_to_lin(curve(f)));
    for ch in buf.data.iter_mut() {
        ctx.check_cancel()?;
        *ch = fir_zero_delay(ch, &h);
    }
    let realised: serde_json::Map<String, Value> = pts
        .iter()
        .map(|(f, _)| {
            (format!("{f}Hz"), json!((20.0 * fir_response(&h, *f, sr).max(1e-12).log10() * 100.0).round() / 100.0))
        })
        .collect();
    ctx.metric("realised_gain_db", Value::Object(realised));
    ctx.metric("fir_taps", h.len());
    Ok(buf)
}

fn parse_freq_gain_points(s: &str) -> Result<Vec<(f64, f64)>> {
    let mut pts = Vec::new();
    for item in units::split_list(s) {
        let (f, g) = item
            .split_once(':')
            .ok_or_else(|| Error::usage(format!("--curve point '{item}' should be freq:gain, e.g. 1kHz:-3dB")))?;
        pts.push((units::parse_freq(f)?, units::parse_db(g)?));
    }
    if pts.is_empty() {
        return Err(Error::usage("--curve needs at least one freq:gain point"));
    }
    Ok(pts)
}

fn fft_filter(a: &Args, mut buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let sr = buf.sample_rate;
    let pts = parse_freq_gain_points(&a.str_or("curve", ""))?;
    let fft = a.usize("fft")?.clamp(256, 65536).next_power_of_two();
    let curve = log_curve(&pts, false);
    let h = design_fir(fft / 2 + 1, sr, |f| db_to_lin(curve(f)));
    for ch in buf.data.iter_mut() {
        ctx.check_cancel()?;
        *ch = fir_zero_delay(ch, &h);
    }
    let realised: serde_json::Map<String, Value> = pts
        .iter()
        .filter(|(f, _)| *f < sr as f64 / 2.0)
        .map(|(f, _)| {
            (format!("{f}Hz"), json!((20.0 * fir_response(&h, *f, sr).max(1e-12).log10() * 100.0).round() / 100.0))
        })
        .collect();
    ctx.metric("realised_gain_db", Value::Object(realised));
    Ok(buf)
}

fn filter(a: &Args, mut buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let sr = buf.sample_rate as f64;
    let nyq = sr / 2.0;
    let d = Design::parse(&a.choice("design")).ok_or_else(|| Error::usage("unknown --design"))?;
    let band = match a.choice("type").as_str() {
        "highpass" => Band::Highpass,
        "bandpass" => Band::Bandpass,
        "bandstop" => Band::Bandstop,
        _ => Band::Lowpass,
    };
    let order = a.usize("order")?.clamp(1, 24);
    let ripple = a.db("ripple")?.abs().max(0.01);
    let stop = a.db("stopband")?.abs().max(ripple + 1.0);
    let fstr = a.str_or("freq", "auto");
    let (f1, f2) = if fstr == "auto" {
        match band {
            Band::Lowpass => (8000.0f64.min(nyq * 0.8), 0.0),
            Band::Highpass => (80.0, 0.0),
            Band::Bandpass => (300.0, 3400.0f64.min(nyq * 0.8)),
            Band::Bandstop => (50.0, 70.0),
        }
    } else if fstr.contains("..") {
        if matches!(band, Band::Lowpass | Band::Highpass) {
            return Err(Error::usage("--freq for a lowpass or highpass is one frequency, e.g. 1kHz"));
        }
        let (lo, hi) = units::parse_num_range(&fstr, units::parse_freq)
            .map_err(|e| Error::usage(format!("--freq: {}", e.message)))?;
        (lo.min(hi), lo.max(hi))
    } else {
        if matches!(band, Band::Bandpass | Band::Bandstop) {
            return Err(Error::usage("--freq for a band filter is a range, e.g. 300Hz..3.4kHz"));
        }
        (units::parse_freq(&fstr).map_err(|e| Error::usage(format!("--freq: {}", e.message)))?, 0.0)
    };
    let top = f1.max(f2);
    if f1 <= 0.0 || top >= nyq {
        return Err(Error::usage(format!("--freq must be between 0 and the Nyquist frequency ({nyq} Hz)")));
    }
    let sos = design::design(d, band, order, f1, f2, ripple, stop, sr);
    for ch in buf.data.iter_mut() {
        sos.apply_in_place(ch);
    }
    let probe: Vec<f64> = match band {
        Band::Lowpass | Band::Highpass => vec![f1 / 4.0, f1 / 2.0, f1, f1 * 2.0, f1 * 4.0],
        _ => vec![f1 / 2.0, f1, (f1 * f2).sqrt(), f2, f2 * 2.0],
    };
    let mut resp = serde_json::Map::new();
    let mut gd = serde_json::Map::new();
    for f in probe.into_iter().filter(|f| *f > 0.0 && *f < nyq) {
        let key = if f >= 1000.0 {
            format!("{}kHz", (f / 10.0).round() / 100.0)
        } else {
            format!("{}Hz", (f * 10.0).round() / 10.0)
        };
        resp.insert(key.clone(), json!((sos.magnitude_db(f, sr) * 100.0).round() / 100.0));
        gd.insert(key, json!((sos.group_delay(f, sr) / sr * 1000.0 * 1000.0).round() / 1000.0));
    }
    ctx.metric("response_db", Value::Object(resp));
    ctx.metric("group_delay_ms", Value::Object(gd));
    ctx.metric("sections", sos.sections.len());
    ctx.metric("poles", if matches!(band, Band::Bandpass | Band::Bandstop) { order * 2 } else { order });
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferraus_core::testing::{self, run, tone_level};

    fn spec(name: &str) -> OpSpec {
        ops().into_iter().find(|o| o.name == name).unwrap()
    }

    fn gain_db(sp: &OpSpec, pairs: &[(&str, &str)], f: f64) -> f64 {
        let sr = 48000;
        let x = testing::sine(sr, f, 1.0, 0.5);
        let y = run(sp, pairs, x.clone()).unwrap();
        20.0 * (tone_level(&y.data[0][12000..36000], sr, f) / tone_level(&x.data[0][12000..36000], sr, f)).log10()
    }

    #[test]
    fn graphic_eq_hits_band_gains_with_zero_delay() {
        let sp = spec("graphic-eq");
        let g = [("gains", "0,0,0,0,0,+6,0,0,0,-6")];
        assert!((gain_db(&sp, &g, 1000.0) - 6.0).abs() < 0.3);
        assert!((gain_db(&sp, &g, 16000.0) + 6.0).abs() < 0.3);
        assert!(gain_db(&sp, &g, 125.0).abs() < 0.3);
        // linear phase and aligned: a band-flat region is unchanged sample for sample
        let x = testing::sine(48000, 250.0, 1.0, 0.5);
        let y = run(&sp, &g, x.clone()).unwrap();
        let d: Vec<f32> = (12000..36000).map(|i| y.data[0][i] - x.data[0][i]).collect();
        assert!(testing::rms_db(&d) < -50.0);
        // all-zero gains are the identity; null; too many gains
        assert_eq!(run(&sp, &[], x.clone()).unwrap(), x);
        assert_eq!(run(&sp, &[("gains", "+3"), ("mix", "0%")], x.clone()).unwrap(), x);
        assert!(run(&sp, &[("gains", "1,1,1,1,1,1,1,1,1,1,1")], x.clone()).is_err());
        let g30 = [("bands", "30"), ("gains", "0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,-6"), ("accuracy", "500ms")];
        assert!((gain_db(&sp, &g30, 1000.0) + 6.0).abs() < 0.5);
    }

    #[test]
    fn fft_filter_follows_curve() {
        let sp = spec("fft-filter");
        assert!((gain_db(&sp, &[], 120.0) + 6.0).abs() < 0.5);
        assert!(gain_db(&sp, &[], 1000.0).abs() < 0.3);
        assert!((gain_db(&sp, &[], 10000.0) - 3.0).abs() < 0.3);
        let flat = [("curve", "100Hz:-12dB")];
        assert!((gain_db(&sp, &flat, 3000.0) + 12.0).abs() < 0.1);
        let x = testing::sine(48000, 440.0, 0.2, 0.5);
        assert_eq!(run(&sp, &[("mix", "0%")], x.clone()).unwrap(), x);
        assert!(run(&sp, &[("curve", "oops")], x).is_err());
    }

    #[test]
    fn filter_realised_response_matches_metrics() {
        let sp = spec("filter");
        let sr = 48000;
        for d in ["butterworth", "chebyshev1", "chebyshev2", "bessel", "elliptic"] {
            let pairs = [("design", d), ("freq", "1kHz")];
            let mut c = testing::ctx();
            let x = testing::sine(sr, 2000.0, 1.0, 0.5);
            let y = ferraus_core::exec::apply_spec(&sp, &testing::args(&sp, &pairs), x.clone(), &mut c).unwrap();
            let reported = c.metrics["response_db"]["2kHz"].as_f64().unwrap();
            let measured = 20.0
                * (tone_level(&y.data[0][24000..], sr, 2000.0) / tone_level(&x.data[0][24000..], sr, 2000.0)).log10();
            if reported > -80.0 {
                assert!((measured - reported).abs() < 0.3, "{d}: measured {measured:.2} reported {reported:.2}");
            }
            assert!(c.metrics["group_delay_ms"]["1kHz"].as_f64().unwrap() > 0.0);
        }
        // butterworth -3 dB at the cutoff, elliptic ripple and stopband
        assert!((gain_db(&sp, &[("freq", "1kHz")], 1000.0) + 3.01).abs() < 0.2);
        let e = [("design", "elliptic"), ("freq", "1kHz"), ("order", "6")];
        assert!(gain_db(&sp, &e, 500.0) > -0.6);
        assert!(gain_db(&sp, &e, 2500.0) < -55.0);
        let hp = [("type", "highpass"), ("freq", "1kHz"), ("design", "chebyshev2")];
        assert!(gain_db(&sp, &hp, 300.0) < -55.0);
        let bp = [("type", "bandpass"), ("freq", "500Hz..2kHz")];
        assert!(gain_db(&sp, &bp, 1000.0).abs() < 0.2);
        assert!(gain_db(&sp, &bp, 100.0) < -40.0);
        let x = testing::sine(sr, 440.0, 0.2, 0.5);
        assert_eq!(run(&sp, &[("mix", "0%")], x.clone()).unwrap(), x);
        assert!(run(&sp, &[("freq", "30kHz")], x.clone()).is_err());
        assert!(run(&sp, &[("type", "bandpass"), ("freq", "1kHz")], x.clone()).is_err());
        assert!(run(&sp, &[], x).is_ok());
    }
}
