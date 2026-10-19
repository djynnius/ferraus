//! EQ and filter commands: eq, highpass, lowpass, tilt, match-eq.

use super::{per_channel, sr};
use crate::biquad::{Cascade, Coeffs, FilterType};
use crate::fir::{design_linear_phase, filter_linear_phase};
use crate::stft::{smooth_octave, welch_psd};
use ferraus_core::prelude::*;

pub fn ops() -> Vec<OpSpec> {
    vec![
        OpSpec::transform("eq", Category::Tone, "Parametric EQ (cascaded biquads or linear phase)", eq)
            .about("Each --band is type:freq:gain with optional :q=<q> and :order=<n>. Types: peak, lowshelf, highshelf, lowpass, highpass, bandpass, notch (pass and notch filters ignore the gain, which may be omitted). Bands are RBJ-cookbook biquads in series; --linear-phase instead applies the same combined magnitude response as a linear-phase FIR (no phase shift, adds pre-ringing; meant for mastering). Default Q: peak 1.0, shelves and pass filters 0.707, notch 4. order > 2 on pass filters gives Butterworth slopes (6 dB/oct per order).")
            .param(param("band", ParamKind::Str).repeatable().help("type:freq:gain[:q=..][:order=..], e.g. peak:2.5kHz:+3dB:q=1.4 or highpass:80Hz:order=4"))
            .param(param("linear-phase", ParamKind::Flag).help("Apply the response as a linear-phase FIR instead of minimum-phase biquads"))
            .example("ferraus eq vox.wav --band highpass:80Hz --band peak:3kHz:+2dB:q=1.2 --band highshelf:10kHz:+1.5dB -o vox_eq.wav", "A vocal EQ")
            .example("ferraus eq mix.wav --band lowshelf:100Hz:-2dB --linear-phase -o mix_lp.wav", "Phase-free low cut for mastering")
            .example("ferraus fx take.wav -o out.wav --fx \"eq:band=notch:60Hz,band=notch:120Hz\"", "Two notches in an fx chain"),
        OpSpec::transform("highpass", Category::Tone, "High-pass filter", highpass)
            .about("Removes content below --freq. --order 2 is 12 dB/oct; higher orders are Butterworth (6 dB/oct per order). --q shapes the corner of an order-2 filter (0.707 = flat Butterworth).")
            .param(param("freq", ParamKind::Freq).default("80Hz").help("Corner frequency"))
            .param(param("order", ParamKind::Int).default("2").range("1..8").help("Filter order (2 = 12 dB/oct, 4 = 24 dB/oct)"))
            .param(param("q", ParamKind::Float).default("0.707").help("Resonance of an order-2 filter"))
            .example("ferraus highpass vox.wav -o vox_hp.wav", "Remove rumble below 80 Hz")
            .example("ferraus highpass kick.wav --freq 30Hz --order 4 -o kick_clean.wav", "Steep sub-sonic filter"),
        OpSpec::transform("lowpass", Category::Tone, "Low-pass filter", lowpass)
            .about("Removes content above --freq. --order 2 is 12 dB/oct; higher orders are Butterworth (6 dB/oct per order). --q shapes the corner of an order-2 filter.")
            .param(param("freq", ParamKind::Freq).default("12kHz").help("Corner frequency"))
            .param(param("order", ParamKind::Int).default("2").range("1..8").help("Filter order (2 = 12 dB/oct, 4 = 24 dB/oct)"))
            .param(param("q", ParamKind::Float).default("0.707").help("Resonance of an order-2 filter"))
            .example("ferraus lowpass hats.wav --freq 9kHz -o darker.wav", "Tame harsh highs")
            .example("ferraus lowpass synth.wav --freq 800Hz --q 4 -o reso.wav", "Resonant low-pass"),
        OpSpec::transform("tilt", Category::Tone, "Tilt EQ: one-knob brighter or darker", tilt)
            .about("Raises one side of the spectrum and lowers the other around the pivot: positive --gain is brighter (highs up by gain/2, lows down by gain/2), negative is darker.")
            .param(param("pivot", ParamKind::Freq).default("1kHz").help("Frequency that stays unchanged"))
            .param(param("gain", ParamKind::Gain).default("+0dB").help("Total tilt: +4dB brightens, -4dB darkens"))
            .example("ferraus tilt mix.wav --gain +3dB -o brighter.wav", "Brighten a dull mix")
            .example("ferraus tilt pad.wav --gain -4dB --pivot 600Hz -o warmer.wav", "Warm up a pad"),
        OpSpec::transform("match-eq", Category::Tone, "Match the long-term spectrum of a reference", match_eq)
            .about("Measures the long-term average spectrum of the input and of --reference, smooths both (--smoothing, fractional octaves), and applies the difference as a linear-phase FIR. Only the spectral shape is matched (the broadband level difference is removed), boosts and cuts are capped at ±12 dB, and regions with no energy in either file are left alone. Useful for agents matching takes recorded on different days or microphones.")
            .param(param("reference", ParamKind::Path).required().help("Audio file whose tonal balance to match"))
            .param(param("amount", ParamKind::Percent).default("100%").help("How much of the measured correction to apply"))
            .param(param("smoothing", ParamKind::Str).default("1/3oct").help("Spectral smoothing width in octaves (1/3oct, 1/6oct, 1oct)"))
            .example("ferraus match-eq take2.wav --reference take1.wav -o take2_matched.wav", "Make take 2 sound like take 1")
            .example("ferraus match-eq mix.wav --reference ref.wav --amount 50% --smoothing 1oct -o mix_m.wav", "Gentle, broad matching"),
    ]
}

/// One parsed `--band`.
#[derive(Clone, Debug)]
pub(crate) struct Band {
    pub ty: FilterType,
    pub freq: f64,
    pub gain: f64,
    pub q: f64,
    pub order: usize,
}

pub(crate) fn parse_band(s: &str) -> Result<Band> {
    let parts: Vec<&str> = s.split(':').map(|p| p.trim()).collect();
    let bad = |m: &str| Error::usage(format!("--band '{s}': {m}. Expected type:freq:gain[:q=..][:order=..]"));
    if parts.len() < 2 {
        return Err(bad("missing frequency"));
    }
    let ty = FilterType::parse(parts[0])
        .ok_or_else(|| bad("unknown type (peak, lowshelf, highshelf, lowpass, highpass, bandpass, notch)"))?;
    let freq = units::parse_freq(parts[1]).map_err(|e| bad(&e.message))?;
    let mut gain = 0.0;
    let mut q = None;
    let mut order = 2usize;
    for p in &parts[2..] {
        if let Some((k, v)) = p.split_once('=') {
            match k.trim().to_ascii_lowercase().as_str() {
                "q" => q = Some(v.trim().parse::<f64>().map_err(|_| bad("q must be a number"))?),
                "order" => order = v.trim().parse::<usize>().map_err(|_| bad("order must be an integer"))?,
                "gain" => gain = units::parse_db(v).map_err(|e| bad(&e.message))?,
                other => return Err(bad(&format!("unknown option '{other}'"))),
            }
        } else if !p.is_empty() {
            gain = units::parse_db(p).map_err(|e| bad(&e.message))?;
        }
    }
    if !(1..=8).contains(&order) {
        return Err(bad("order must be 1..8"));
    }
    let q = q.unwrap_or(match ty {
        FilterType::Peak => 1.0,
        FilterType::Notch => 4.0,
        _ => std::f64::consts::FRAC_1_SQRT_2,
    });
    if q <= 0.0 {
        return Err(bad("q must be positive"));
    }
    Ok(Band { ty, freq, gain, q, order })
}

/// Biquad cascade implementing one band.
pub(crate) fn band_cascade(b: &Band, rate: f64) -> Cascade {
    match b.ty {
        FilterType::Lowpass | FilterType::Highpass => {
            pass_filter(b.ty == FilterType::Highpass, b.freq, b.order, b.q, rate)
        }
        FilterType::Bandpass | FilterType::Notch => {
            let c = Coeffs::design(b.ty, b.freq, b.q, 0.0, rate);
            Cascade::new(std::iter::repeat_n(c, (b.order / 2).max(1)))
        }
        _ => Cascade::new([Coeffs::design(b.ty, b.freq, b.q, b.gain, rate)]),
    }
}

/// High/low-pass: order 2 uses `q`; other orders are Butterworth.
pub(crate) fn pass_filter(high: bool, freq: f64, order: usize, q: f64, rate: f64) -> Cascade {
    if order == 2 {
        let ty = if high { FilterType::Highpass } else { FilterType::Lowpass };
        Cascade::new([Coeffs::design(ty, freq, q, 0.0, rate)])
    } else if high {
        Cascade::butterworth_highpass(order, freq, rate)
    } else {
        Cascade::butterworth_lowpass(order, freq, rate)
    }
}

/// FIR length for linear-phase filtering: ~6 Hz resolution, odd.
fn fir_taps(rate: f64) -> usize {
    ((rate / 6.0).round() as usize) | 1
}

fn eq(a: &Args, mut buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let bands: Vec<Band> = a.all("band").iter().map(|s| parse_band(s)).collect::<Result<_>>()?;
    if bands.is_empty() {
        ctx.warn("no_bands", "eq has no --band, so the audio is unchanged");
        return Ok(buf);
    }
    let rate = sr(&buf);
    let cascades: Vec<Cascade> = bands.iter().map(|b| band_cascade(b, rate)).collect();
    if a.flag("linear-phase") {
        let h = design_linear_phase(fir_taps(rate), rate, |f| {
            let db: f64 = cascades.iter().map(|c| c.magnitude_db(f.max(0.5), rate)).sum();
            10f64.powf(db / 20.0)
        });
        per_channel(&mut buf, |_, ch| filter_linear_phase(&h, ch));
        ctx.note("fir_taps", h.len());
    } else {
        per_channel(&mut buf, |_, ch| {
            for c in &cascades {
                c.clone().process_block(ch);
            }
        });
    }
    Ok(buf)
}

fn pass(a: &Args, mut buf: AudioBuffer, ctx: &mut OpCtx, high: bool) -> Result<AudioBuffer> {
    let rate = sr(&buf);
    let f = a.freq("freq")?;
    let order = a.usize("order")?;
    if !(1..=8).contains(&order) {
        return Err(Error::usage("--order must be between 1 and 8"));
    }
    let q = a.f64("q")?;
    if q <= 0.0 {
        return Err(Error::usage("--q must be positive"));
    }
    if order != 2 && a.is_explicit("q") {
        ctx.warn("q_ignored", "--q only shapes order-2 filters; higher orders are Butterworth");
    }
    let c = pass_filter(high, f, order, q, rate);
    per_channel(&mut buf, |_, ch| c.clone().process_block(ch));
    Ok(buf)
}

fn highpass(a: &Args, buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    pass(a, buf, ctx, true)
}

fn lowpass(a: &Args, buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    pass(a, buf, ctx, false)
}

fn tilt(a: &Args, mut buf: AudioBuffer, _ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let g = a.db("gain")?;
    if g == 0.0 {
        return Ok(buf);
    }
    let rate = sr(&buf);
    let pivot = a.freq("pivot")?;
    let c = Cascade::new([
        Coeffs::design(FilterType::LowShelf, pivot, 0.5, -g / 2.0, rate),
        Coeffs::design(FilterType::HighShelf, pivot, 0.5, g / 2.0, rate),
    ]);
    per_channel(&mut buf, |_, ch| c.clone().process_block(ch));
    Ok(buf)
}

/// Parse `1/3oct`, `1/6`, `1oct`, `0.5oct` → octaves.
fn parse_smoothing(s: &str) -> Result<f64> {
    let t = s.trim().to_ascii_lowercase();
    let t = t.trim_end_matches("octaves").trim_end_matches("octave").trim_end_matches("oct").trim();
    let v = if let Some((n, d)) = t.split_once('/') {
        let n: f64 = n.trim().parse().map_err(|_| Error::usage(format!("--smoothing '{s}': expected e.g. 1/3oct")))?;
        let d: f64 = d.trim().parse().map_err(|_| Error::usage(format!("--smoothing '{s}': expected e.g. 1/3oct")))?;
        n / d
    } else {
        t.parse().map_err(|_| Error::usage(format!("--smoothing '{s}': expected e.g. 1/3oct")))?
    };
    if !(0.01..=3.0).contains(&v) {
        return Err(Error::usage("--smoothing must be between 1/96oct and 3oct"));
    }
    Ok(v)
}

/// Correction curve (dB per bin) taking `input` towards `reference`.
pub(crate) fn match_curve(
    input: &[f32],
    reference: &[f32],
    rate: f64,
    size: usize,
    smoothing: f64,
    amount: f64,
) -> Vec<f64> {
    let df = rate / size as f64;
    let pi = smooth_octave(&welch_psd(input, size), df, smoothing);
    let pr = smooth_octave(&welch_psd(reference, size), df, smoothing);
    let max_i = pi.iter().cloned().fold(0.0, f64::max).max(1e-30);
    let max_r = pr.iter().cloned().fold(0.0, f64::max).max(1e-30);
    let hi = 20000f64.min(rate * 0.45);
    let lo = 20.0;
    let db = |p: f64| 10.0 * p.max(1e-30).log10();
    let mut diff: Vec<f64> = (0..pi.len())
        .map(|k| {
            let f = k as f64 * df;
            if f < lo || f > hi || pi[k] < max_i * 1e-8 || pr[k] < max_r * 1e-8 {
                f64::NAN
            } else {
                db(pr[k]) - db(pi[k])
            }
        })
        .collect();
    // remove the broadband level difference: mean over 100 Hz..10 kHz, log-frequency weighted
    let (mut s, mut w) = (0.0, 0.0);
    for (k, d) in diff.iter().enumerate() {
        let f = k as f64 * df;
        if (100.0..=10000.0).contains(&f) && d.is_finite() {
            let wk = 1.0 / f;
            s += d * wk;
            w += wk;
        }
    }
    let offset = if w > 0.0 { s / w } else { 0.0 };
    for d in diff.iter_mut() {
        *d = if d.is_finite() { ((*d - offset) * amount).clamp(-12.0, 12.0) } else { 0.0 };
    }
    diff
}

fn match_eq(a: &Args, mut buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let path = a.path("reference").ok_or_else(|| Error::usage("match-eq needs --reference <file>"))?;
    let amount = a.percent("amount")?.clamp(0.0, 2.0);
    let smoothing = parse_smoothing(&a.str_or("smoothing", "1/3oct"))?;
    let mut reference = ctx.read_audio_path(&path)?;
    if reference.sample_rate != buf.sample_rate {
        reference =
            ferraus_core::resample::resample(&reference, buf.sample_rate, ferraus_core::resample::Quality::High);
    }
    let rate = sr(&buf);
    let size = ((8192.0 * rate / 48000.0) as usize).next_power_of_two().max(1024);
    let curve = match_curve(&buf.mono_mix(), &reference.mono_mix(), rate, size, smoothing, amount);
    let df = rate / size as f64;
    let h = design_linear_phase(fir_taps(rate), rate, |f| {
        let x = f / df;
        let k = x.floor() as usize;
        let t = x - k as f64;
        let a0 = curve.get(k).copied().unwrap_or(0.0);
        let a1 = curve.get(k + 1).copied().unwrap_or(a0);
        10f64.powf((a0 + t * (a1 - a0)) / 20.0)
    });
    per_channel(&mut buf, |_, ch| filter_linear_phase(&h, ch));
    let maxb = curve.iter().cloned().fold(0.0, f64::max);
    let maxc = curve.iter().cloned().fold(0.0, f64::min);
    ctx.metric("max_boost_db", (maxb * 10.0).round() / 10.0);
    ctx.metric("max_cut_db", (maxc * 10.0).round() / 10.0);
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferraus_core::testing::{noise, run, sine, tone_level};

    fn spec(name: &str) -> OpSpec {
        ops().into_iter().find(|o| o.name == name).unwrap()
    }

    #[test]
    fn band_parsing() {
        let b = parse_band("peak:2.5kHz:+3dB:q=1.4").unwrap();
        assert_eq!(b.ty, FilterType::Peak);
        assert_eq!(b.freq, 2500.0);
        assert_eq!(b.gain, 3.0);
        assert_eq!(b.q, 1.4);
        let b = parse_band("highpass:80Hz:order=4").unwrap();
        assert_eq!(b.order, 4);
        assert!(parse_band("wobble:1kHz:3dB").is_err());
        assert!(parse_band("peak").is_err());
    }

    #[test]
    fn eq_peak_gain_at_centre() {
        let s = spec("eq");
        let x = sine(48000, 1000.0, 1.0, 0.25);
        for lp in [false, true] {
            let mut pairs = vec![("band", "peak:1kHz:+6dB:q=1")];
            if lp {
                pairs.push(("linear-phase", "true"));
            }
            let y = run(&s, &pairs, x.clone()).unwrap();
            let g = 20.0
                * (tone_level(&y.data[0][12000..36000], 48000, 1000.0)
                    / tone_level(&x.data[0][12000..36000], 48000, 1000.0))
                .log10();
            assert!((g - 6.0).abs() < 0.1, "linear_phase={lp}: {g}");
        }
    }

    #[test]
    fn highpass_slopes() {
        let s = spec("highpass");
        let x = sine(48000, 100.0, 1.0, 0.5);
        for (order, want) in [("2", -40.0), ("4", -80.0)] {
            let y = run(&s, &[("freq", "1kHz"), ("order", order)], x.clone()).unwrap();
            let g =
                20.0 * (crate::util::rms(&y.data[0][24000..]) / crate::util::rms(&x.data[0][24000..])).log10() as f64;
            assert!((g - want).abs() < 1.0, "order {order}: {g}");
        }
    }

    #[test]
    fn tilt_zero_is_identity_and_tilts() {
        let s = spec("tilt");
        let x = noise(48000, 0.2, 0.3, 1);
        assert_eq!(run(&s, &[], x.clone()).unwrap(), x);
        let lo = sine(48000, 50.0, 1.0, 0.3);
        let y = run(&s, &[("gain", "+6dB")], lo.clone()).unwrap();
        assert!(crate::util::rms(&y.data[0][24000..]) < crate::util::rms(&lo.data[0][24000..]));
    }

    #[test]
    fn match_eq_moves_towards_reference() {
        let dir = ferraus_core::testing::TempDir::new("match-eq");
        let rate = 48000;
        // reference: noise with the highs boosted by a +9 dB high shelf
        let mut r = noise(rate, 3.0, 0.2, 9);
        Cascade::new([Coeffs::design(FilterType::HighShelf, 3000.0, 0.707, 9.0, rate as f64)])
            .process_block(&mut r.data[0]);
        let path = dir.path("ref.wav");
        ferraus_core::testing::write_wav(&path, &r);
        let x = noise(rate, 3.0, 0.2, 10);
        let s = spec("match-eq");
        let y = run(&s, &[("reference", path.to_str().unwrap())], x.clone()).unwrap();
        let df = rate as f64 / 8192.0;
        let c = match_curve(&y.mono_mix(), &r.mono_mix(), rate as f64, 8192, 1.0 / 3.0, 1.0);
        let c0 = match_curve(&x.mono_mix(), &r.mono_mix(), rate as f64, 8192, 1.0 / 3.0, 1.0);
        let at = |c: &[f64], f: f64| c[(f / df) as usize];
        // before: highs differ by ~9 dB relative to lows; after: within ~1.5 dB
        assert!((at(&c0, 10000.0) - at(&c0, 300.0)) > 7.0);
        assert!((at(&c, 10000.0) - at(&c, 300.0)).abs() < 1.5, "{}", at(&c, 10000.0) - at(&c, 300.0));
    }
}
