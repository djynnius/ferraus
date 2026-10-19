//! Dynamics commands: compress (with the `tighten` chain), limit, gate, expand, deess,
//! multiband, transient, clip.

use super::{apply_own_mix, freq_or_off, gain_or_auto, own_mix, per_band, per_channel, secs, sr};
use crate::biquad::Cascade;
use crate::dynamics::{
    self, ClipMode, Compressor, CompressorParams, DeessParams, Detector, ExpanderParams, GateParams, LimiterParams,
};
use crate::saturation::{SatType, Saturator};
use ferraus_core::prelude::*;

const VOCAL: &[(&str, &str)] = &[
    ("threshold", "-20dB"),
    ("ratio", "3:1"),
    ("attack", "5ms"),
    ("release", "80ms"),
    ("knee", "6dB"),
    ("detector", "rms"),
];
const DRUMS: &[(&str, &str)] = &[
    ("threshold", "-16dB"),
    ("ratio", "4:1"),
    ("attack", "15ms"),
    ("release", "60ms"),
    ("knee", "3dB"),
    ("detector", "peak"),
];
const BUS: &[(&str, &str)] =
    &[("threshold", "-14dB"), ("ratio", "2:1"), ("attack", "30ms"), ("release", "200ms"), ("knee", "6dB")];
const MASTER: &[(&str, &str)] = &[
    ("threshold", "-10dB"),
    ("ratio", "1.5:1"),
    ("attack", "30ms"),
    ("release", "250ms"),
    ("knee", "8dB"),
    ("lookahead", "5ms"),
];
/// "Compress, clip high frequencies, and make the sound tighter": high-pass 60 Hz →
/// compress 4:1 5 ms / 60 ms → de-ess (add `--lowpass 16kHz` for the optional top cut).
const TIGHTEN: &[(&str, &str)] =
    &[("highpass", "60Hz"), ("ratio", "4:1"), ("attack", "5ms"), ("release", "60ms"), ("deess", "on")];

pub fn ops() -> Vec<OpSpec> {
    vec![
        OpSpec::transform("compress", Category::Dynamics, "Compress dynamic range (feed-forward, soft knee)", compress)
            .about("Feed-forward log-domain compressor with a soft knee, lookahead, RMS or peak detection, and a sidechain high-pass so bass does not pump the gain. 'auto' makeup adds back half the reduction a 0 dBFS signal would get. --mix below 100% gives parallel (New York) compression. Optional stages run around the compressor in this order: --highpass, compression, --character tube saturation, --deess, --lowpass; the 'tighten' preset (high-pass 60 Hz, 4:1, 5 ms / 60 ms, de-ess) uses them.")
            .param(param("threshold", ParamKind::Gain).default("-18dB").help("Level above which gain is reduced (dBFS)"))
            .param(param("ratio", ParamKind::Ratio).default("3:1").help("Input dB over threshold per output dB (4:1, or 4)"))
            .param(param("attack", ParamKind::Time).default("10ms").help("Time to clamp down on a level rise"))
            .param(param("release", ParamKind::Time).default("100ms").help("Time to recover after the level falls"))
            .param(param("knee", ParamKind::Gain).default("6dB").help("Soft-knee width centred on the threshold (0dB = hard knee)"))
            .param(param("makeup", ParamKind::Str).default("auto").help("Output gain in dB, or 'auto'"))
            .param(param("lookahead", ParamKind::Time).default("5ms").help("How far ahead the detector looks (latency is compensated)"))
            .param(param("detector", ParamKind::Choice).choices(&["rms", "peak"]).default("rms").help("Level detector: rms (10 ms, smooth) or peak (fast)"))
            .param(param("sidechain-hpf", ParamKind::Freq).default("80Hz").help("High-pass on the detector so lows do not drive the gain (0Hz = off)"))
            .param(param("mix", ParamKind::Percent).default("100%").help("Dry/wet: below 100% blends in the uncompressed signal (parallel compression); 0% returns the input"))
            .param(param("character", ParamKind::Choice).choices(&["clean", "tube"]).default("clean").help("clean, or tube: adds a tube-style saturation stage after the gain"))
            .param(param("highpass", ParamKind::Freq).advanced().help("Optional high-pass before compression, e.g. 60Hz (set by the tighten preset)"))
            .param(param("deess", ParamKind::Switch).default("off").advanced().help("De-ess after compression (6.5 kHz, -20 dB, 6 dB range; set by the tighten preset)"))
            .param(param("lowpass", ParamKind::Freq).advanced().help("Optional low-pass at the end of the chain, e.g. 16kHz (the tighten preset's optional top cut)"))
            .preset("vocal", "Smooth vocal leveling: -20 dB, 3:1, 5 ms / 80 ms, RMS", VOCAL)
            .preset("drums", "Punchy drums: -16 dB, 4:1, 15 ms attack lets transients through, peak detector", DRUMS)
            .preset("bus", "Mix-bus glue: -14 dB, 2:1, 30 ms / 200 ms", BUS)
            .preset("master", "Gentle mastering: -10 dB, 1.5:1, 30 ms / 250 ms, wide knee", MASTER)
            .preset("tighten", "Compress and tighten: high-pass 60 Hz, compress 4:1 5 ms/60 ms, de-ess (add --lowpass 16kHz to also cut the top)", TIGHTEN)
            .example("ferraus compress vox.wav --preset vocal -o vox_comp.wav", "Vocal preset")
            .example("ferraus compress drums.wav --ratio 6:1 --attack 1ms --mix 40% -o drums_par.wav", "Parallel compression")
            .example("ferraus compress take.wav --preset tighten --lowpass 16kHz -o tight.wav", "Compress, cut the extremes, and tighten"),
        OpSpec::transform("limit", Category::Dynamics, "True-peak brickwall limiter", limit)
            .about("Lookahead brickwall limiter that detects inter-sample peaks with oversampling, so the output never exceeds the ceiling when measured as true peak. The gain envelope ramps down over the lookahead window (no clicks) and recovers with the release time.")
            .param(param("ceiling", ParamKind::Gain).default("-1dBTP").help("Maximum output level (true peak, dBTP)"))
            .param(param("release", ParamKind::Time).default("50ms").help("Recovery time after a peak"))
            .param(param("lookahead", ParamKind::Time).default("5ms").help("Attack ramp length (latency is compensated)"))
            .param(param("oversample", ParamKind::Choice).choices(&["1x", "2x", "4x", "8x"]).default("4x").help("True-peak detection oversampling (1x = sample peak)"))
            .example("ferraus limit mix.wav -o limited.wav", "Limit to -1 dBTP")
            .example("ferraus limit master.wav --ceiling -0.3dBTP --release 100ms -o hot.wav", "Hotter ceiling, slower release")
            .example("ferraus limit podcast.wav --ceiling -2dBTP -o podcast_safe.wav", "Leave extra headroom for lossy encoding (MP3 and AAC can add peaks)")
            .example("ferraus limit drums.wav --ceiling -1dB --oversample 1x --lookahead 1ms -o drums_lim.wav", "Fast sample-peak limiting with a short lookahead"),
        OpSpec::transform("gate", Category::Dynamics, "Noise gate with hysteresis and hold", gate)
            .about("Mutes the signal while it stays below the threshold. The gate opens at the threshold and closes 3 dB lower (hysteresis) after the hold time, so it does not chatter.")
            .param(param("threshold", ParamKind::Gain).default("-40dB").help("Opening level (closes 3 dB lower)"))
            .param(param("attack", ParamKind::Time).default("1ms").help("Opening time"))
            .param(param("hold", ParamKind::Time).default("50ms").help("Minimum open time after the level drops"))
            .param(param("release", ParamKind::Time).default("100ms").help("Closing time"))
            .param(param("range", ParamKind::Gain).default("-80dB").help("Attenuation when closed"))
            .param(param("sidechain-hpf", ParamKind::Freq).default("0Hz").help("High-pass on the detector (0Hz = off)"))
            .example("ferraus gate tom.wav --threshold -35dB -o tom_gated.wav", "Gate a tom mic")
            .example("ferraus gate vo.wav --range -12dB --release 200ms -o vo_soft.wav", "Gentle gate that only dips the background")
            .example("ferraus gate kick.wav --threshold -30dB --hold 20ms --release 50ms -o kick_tight.wav", "Short, tight gate that cuts the spill between kick hits")
            .example("ferraus gate snare.wav --threshold -28dB --sidechain-hpf 200Hz -o snare_gated.wav", "High-pass the detector so kick bleed does not open the gate, only snare hits do"),
        OpSpec::transform("expand", Category::Dynamics, "Downward expander", expand)
            .about("Turns quiet passages down further: every dB below the threshold becomes 'ratio' dB. The gentle alternative to a gate.")
            .param(param("threshold", ParamKind::Gain).default("-40dB").help("Level below which expansion starts"))
            .param(param("ratio", ParamKind::Ratio).default("2:1").help("Expansion ratio below the threshold"))
            .param(param("attack", ParamKind::Time).default("5ms").help("Time to open when the level rises"))
            .param(param("release", ParamKind::Time).default("100ms").help("Time to close when the level falls"))
            .example("ferraus expand vo.wav -o vo_exp.wav", "Reduce room noise between phrases")
            .example("ferraus expand drums.wav --threshold -30dB --ratio 3:1 -o tighter.wav", "Stronger expansion")
            .example("ferraus expand podcast.wav --threshold -45dB --ratio 1.5:1 --release 250ms -o podcast_exp.wav", "Very gentle, slow expansion that lowers room tone without chopping word endings")
            .example("ferraus expand tom.wav --threshold -35dB --ratio 4:1 --attack 1ms -o tom_exp.wav", "Gate-like expansion with a fast attack so the tom's hit is not softened"),
        OpSpec::transform("deess", Category::Dynamics, "Reduce sibilance", deess)
            .about("A band-limited detector around the sibilance frequency drives a dynamic high shelf (split: only the highs are turned down, via a phase-matched crossover) or the whole signal (wideband).")
            .param(param("center", ParamKind::Freq).default("6.5kHz").help("Detector centre frequency"))
            .param(param("bandwidth", ParamKind::Str).default("1oct").help("Detector bandwidth in octaves (e.g. 1oct, 0.5oct)"))
            .param(param("threshold", ParamKind::Gain).default("-20dB").help("Detector level where reduction starts"))
            .param(param("range", ParamKind::Gain).default("6dB").help("Maximum reduction"))
            .param(param("mode", ParamKind::Choice).choices(&["split", "wideband"]).default("split").help("split: turn down only the highs; wideband: turn down everything"))
            .example("ferraus deess vox.wav -o vox_deessed.wav", "Default de-essing")
            .example("ferraus deess vox.wav --center 5kHz --range 10dB --mode wideband -o vox_dw.wav", "Stronger wideband de-essing for a darker voice")
            .example("ferraus deess vox_bright.wav --center 8kHz --bandwidth 0.5oct --threshold -24dB -o vox_deessed.wav", "Narrow detector tuned higher, for a bright voice")
            .example("ferraus deess vox.wav --range 4dB --mix 60% -o vox_light.wav", "Light touch: at most 4 dB of reduction, blended with the dry signal"),
        OpSpec::transform("multiband", Category::Dynamics, "Multiband compressor", multiband)
            .about("Splits the signal with 4th-order Linkwitz-Riley crossovers (bands sum flat), compresses each band, and sums them. Per-band --threshold, --ratio, --attack, --release accept one value for every band or a comma list (low to high; a short list repeats its last value). Defaults are a gentle 2:1 on each band, with slower times on lower bands. --bands 4 picks 120Hz,2kHz,10kHz crossovers; --limiter adds a true-peak limiter at the output.")
            .param(param("crossovers", ParamKind::List).default("150Hz,3kHz").help("Crossover frequencies, ascending; N crossovers make N+1 bands"))
            .param(param("bands", ParamKind::Int).range("2..6").help("Band count with default crossovers (2: 1kHz; 3: 150Hz,3kHz; 4: 120Hz,2kHz,10kHz; 5; 6)"))
            .param(param("threshold", ParamKind::List).default("-20dB").help("Per-band threshold(s) in dB"))
            .param(param("ratio", ParamKind::List).default("2:1").help("Per-band ratio(s)"))
            .param(param("attack", ParamKind::List).default("auto").help("Per-band attack time(s); auto = 20 ms (lowest) to 5 ms (highest)"))
            .param(param("release", ParamKind::List).default("auto").help("Per-band release time(s); auto = 200 ms (lowest) to 80 ms (highest)"))
            .param(param("makeup", ParamKind::Gain).default("0dB").help("Output gain after the bands are summed"))
            .param(param("limiter", ParamKind::Flag).help("Add a true-peak output limiter"))
            .param(param("limiter-ceiling", ParamKind::Gain).default("-1dBTP").help("Ceiling of the output limiter"))
            .example("ferraus multiband mix.wav -o mb.wav", "Gentle three-band compression")
            .example("ferraus multiband master.wav --bands 4 --limiter --threshold -24dB,-20dB,-20dB,-18dB --ratio 3:1,2:1 -o mb4.wav", "Audition-style 4 bands plus limiter")
            .example("ferraus multiband vox.wav --crossovers 250Hz,4kHz --threshold -24dB,-18dB,-26dB --ratio 3:1,2:1,4:1 -o vox_mb.wav", "Three custom bands: hold down boominess and harsh highs harder than the mids")
            .example("ferraus multiband bass.wav --bands 2 --ratio 4:1,1:1 --makeup 2dB -o bass_mb.wav", "Two bands split at 1 kHz: compress only the lows and leave the string noise alone"),
        OpSpec::transform("transient", Category::Dynamics, "Shape attack and sustain", transient)
            .about("Envelope-based transient shaper: positive --attack makes hits punchier, negative softens them; --sustain lengthens or shortens the ring after each hit. Works on drums and plucked instruments without a threshold.")
            .param(param("attack", ParamKind::Gain).default("+0dB").help("Gain applied to note onsets"))
            .param(param("sustain", ParamKind::Gain).default("+0dB").help("Gain applied to the decay after onsets"))
            .example("ferraus transient snare.wav --attack +4dB -o snare_punch.wav", "Punchier snare")
            .example("ferraus transient room.wav --sustain -6dB -o drier.wav", "Shorten the room ring")
            .example("ferraus transient kick.wav --attack -3dB --sustain +3dB -o kick_boom.wav", "Softer click and a longer boom on a kick")
            .example("ferraus transient gtr.wav --attack +2dB --mix 50% -o gtr_pluck.wav", "A little extra pluck definition on an acoustic guitar, blended in parallel"),
        OpSpec::transform("clip", Category::Dynamics, "Soft or hard clipper", clip)
            .about("Clips peaks at the ceiling, with 4x oversampling to limit aliasing. 'soft' rounds the top over a knee; 'hard' cuts flat. The right tool when 'louder and tighter' is the goal and a limiter sounds too smooth.")
            .param(param("ceiling", ParamKind::Gain).default("-0.3dB").help("Clip level (sample peak, dBFS)"))
            .param(param("mode", ParamKind::Choice).choices(&["soft", "hard"]).default("soft").help("Clip shape"))
            .param(param("knee", ParamKind::Gain).default("3dB").help("Soft-clip knee below the ceiling where rounding starts"))
            .example("ferraus clip drums.wav -o clipped.wav", "Soft clip at -0.3 dBFS")
            .example("ferraus clip bass.wav --ceiling -6dB --mode hard -o fuzz.wav", "Hard clipping as an effect")
            .example("ferraus clip mix.wav --ceiling -1dB --knee 6dB -o mix_clip.wav", "Shave peaks before a limiter with a wide, gentle knee")
            .example("ferraus clip snare.wav --ceiling -3dB --mix 50% -o snare_crunch.wav", "Parallel soft clipping for a crunchier snare"),
    ]
}

/// Compressor parameters from args (makeup resolved).
pub(crate) fn compressor_params(a: &Args) -> Result<CompressorParams> {
    let ratio = a.ratio("ratio")?;
    if ratio < 1.0 {
        return Err(Error::usage("--ratio must be at least 1:1 (use expand for ratios below 1)"));
    }
    let mut p = CompressorParams {
        threshold_db: a.db("threshold")? as f32,
        ratio: ratio as f32,
        knee_db: a.db("knee")?.max(0.0) as f32,
        attack_s: secs(a, "attack")?,
        release_s: secs(a, "release")?,
        makeup_db: 0.0,
        lookahead_s: secs(a, "lookahead")?,
        detector: Detector::parse(&a.choice("detector")),
        sidechain_hpf: a.freq("sidechain-hpf")?,
    };
    p.makeup_db = match gain_or_auto(a, "makeup")? {
        Some(db) => db as f32,
        None => p.auto_makeup_db(),
    };
    Ok(p)
}

fn compress(a: &Args, buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let m = own_mix(a)?;
    if m <= 0.0 {
        return Ok(buf);
    }
    let rate = sr(&buf);
    let p = compressor_params(a)?;
    let dry = buf.clone();
    let mut wet = buf;
    if let Some(f) = freq_or_off(a, "highpass")? {
        per_channel(&mut wet, |_, ch| Cascade::butterworth_highpass(2, f, rate).process_block(ch));
    }
    let stats = Compressor::new(p.clone(), rate).process(&mut wet.data);
    if a.choice("character") == "tube" {
        per_channel(&mut wet, |_, ch| Saturator::new(SatType::Tube, 6.0, 0.0, rate).process(ch));
    }
    if a.switch("deess", false) {
        dynamics::deess(&mut wet.data, rate, &DeessParams::default());
    }
    if let Some(f) = freq_or_off(a, "lowpass")? {
        per_channel(&mut wet, |_, ch| Cascade::butterworth_lowpass(2, f, rate).process_block(ch));
    }
    if a.raw("makeup").map(|v| v.eq_ignore_ascii_case("auto")).unwrap_or(false) {
        ctx.note("makeup_resolved", format!("{:.2}dB", p.makeup_db));
    }
    ctx.metric("gain_reduction_max_db", round2(stats.max_reduction_db));
    ctx.metric("gain_reduction_mean_db", round2(stats.mean_reduction_db));
    Ok(apply_own_mix(m, dry, wet))
}

fn round2(x: f32) -> f64 {
    (x as f64 * 100.0).round() / 100.0
}

fn oversample_factor(a: &Args) -> usize {
    a.choice("oversample").trim_end_matches('x').parse().unwrap_or(4)
}

fn limit(a: &Args, mut buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let p = LimiterParams {
        ceiling_db: a.db("ceiling")? as f32,
        release_s: secs(a, "release")?,
        lookahead_s: secs(a, "lookahead")?,
        oversample: oversample_factor(a),
    };
    let rate = sr(&buf);
    let stats = dynamics::limit(&mut buf.data, rate, &p);
    ctx.metric("gain_reduction_max_db", round2(stats.max_reduction_db));
    Ok(buf)
}

fn gate(a: &Args, mut buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let p = GateParams {
        threshold_db: a.db("threshold")? as f32,
        attack_s: secs(a, "attack")?,
        hold_s: secs(a, "hold")?,
        release_s: secs(a, "release")?,
        range_db: a.db("range")? as f32,
        sidechain_hpf: a.freq("sidechain-hpf")?,
    };
    let rate = sr(&buf);
    let stats = dynamics::gate(&mut buf.data, rate, &p);
    ctx.metric("gain_reduction_mean_db", round2(stats.mean_reduction_db));
    Ok(buf)
}

fn expand(a: &Args, mut buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let p = ExpanderParams {
        threshold_db: a.db("threshold")? as f32,
        ratio: a.ratio("ratio")? as f32,
        attack_s: secs(a, "attack")?,
        release_s: secs(a, "release")?,
        floor_db: -80.0,
        detector: Detector::Rms,
    };
    let rate = sr(&buf);
    let stats = dynamics::expand(&mut buf.data, rate, &p);
    ctx.metric("gain_reduction_mean_db", round2(stats.mean_reduction_db));
    Ok(buf)
}

fn deess(a: &Args, mut buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let bw = units::parse_with_unit(&a.str_or("bandwidth", "1oct"), &["octaves", "octave", "oct"])
        .map_err(|e| Error::usage(format!("--bandwidth: {}", e.message)))?;
    if !(0.05..=4.0).contains(&bw) {
        return Err(Error::usage("--bandwidth must be between 0.05oct and 4oct"));
    }
    let p = DeessParams {
        center: a.freq("center")?,
        bandwidth_oct: bw,
        threshold_db: a.db("threshold")? as f32,
        range_db: a.db("range")?.abs() as f32,
        split: a.choice("mode") != "wideband",
    };
    let rate = sr(&buf);
    let stats = dynamics::deess(&mut buf.data, rate, &p);
    ctx.metric("gain_reduction_max_db", round2(stats.max_reduction_db));
    Ok(buf)
}

/// Default crossovers for `--bands N`.
fn default_crossovers(bands: usize) -> Result<Vec<f64>> {
    Ok(match bands {
        2 => vec![1000.0],
        3 => vec![150.0, 3000.0],
        4 => vec![120.0, 2000.0, 10000.0],
        5 => vec![80.0, 400.0, 2000.0, 8000.0],
        6 => vec![80.0, 300.0, 1200.0, 4000.0, 10000.0],
        _ => return Err(Error::usage("--bands must be between 2 and 6")),
    })
}

fn multiband(a: &Args, buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let rate = sr(&buf);
    let mut xs: Vec<f64> = if a.has("bands") && !a.is_explicit("crossovers") {
        default_crossovers(a.usize("bands")?)?
    } else {
        a.list("crossovers")
            .iter()
            .map(|s| units::parse_freq(s).map_err(|e| Error::usage(format!("--crossovers: {}", e.message))))
            .collect::<Result<_>>()?
    };
    if a.has("bands") && a.is_explicit("crossovers") && a.usize("bands")? != xs.len() + 1 {
        return Err(Error::usage(format!(
            "--bands {} does not match {} crossovers (N crossovers make N+1 bands)",
            a.usize("bands")?,
            xs.len()
        )));
    }
    if xs.is_empty() {
        return Err(Error::usage("--crossovers needs at least one frequency"));
    }
    if xs.windows(2).any(|w| w[1] <= w[0]) {
        return Err(Error::usage("--crossovers must be ascending, e.g. 150Hz,3kHz"));
    }
    xs.retain(|f| *f < rate * 0.45);
    let nb = xs.len() + 1;
    let thr = per_band(a, "threshold", nb, units::parse_db)?;
    let ratio = per_band(a, "ratio", nb, units::parse_ratio)?;
    let auto = |lo: f64, hi: f64, i: usize| lo * (hi / lo).powf(i as f64 / (nb - 1).max(1) as f64);
    let parse_t = |s: &str| -> Result<Option<f64>> {
        if s.eq_ignore_ascii_case("auto") {
            Ok(None)
        } else {
            units::parse_duration_s(s).map(Some)
        }
    };
    let att = per_band(a, "attack", nb, parse_t)?;
    let rel = per_band(a, "release", nb, parse_t)?;
    ctx.note("crossovers", xs.iter().map(|f| format!("{f}Hz")).collect::<Vec<_>>().join(","));

    // split every channel into bands: bands[b][ch]
    let nch = buf.channels();
    let n = buf.frames();
    let mut bands: Vec<Vec<Vec<f32>>> = vec![Vec::with_capacity(nch); nb];
    for ch in &buf.data {
        let mut sp = crate::biquad::MultibandSplitter::new(&xs, rate);
        for (b, sig) in sp.split_buffer(ch).into_iter().enumerate() {
            bands[b].push(sig);
        }
    }
    let mut out = AudioBuffer::new(buf.sample_rate, nch, n);
    let mut reductions = Vec::new();
    for (b, band) in bands.iter_mut().enumerate() {
        let p = CompressorParams {
            threshold_db: thr[b] as f32,
            ratio: ratio[b].max(1.0) as f32,
            knee_db: 6.0,
            attack_s: att[b].unwrap_or_else(|| auto(0.020, 0.005, b)),
            release_s: rel[b].unwrap_or_else(|| auto(0.200, 0.080, b)),
            makeup_db: 0.0,
            lookahead_s: 0.005,
            detector: Detector::Rms,
            sidechain_hpf: 0.0,
        };
        let st = Compressor::new(p, rate).process(band);
        reductions.push(round2(st.max_reduction_db));
        for (o, s) in out.data.iter_mut().zip(band.iter()) {
            for (x, y) in o.iter_mut().zip(s) {
                *x += y;
            }
        }
    }
    let mk = a.db("makeup")?;
    if mk != 0.0 {
        out.scale(db_to_lin(mk) as f32);
    }
    if a.flag("limiter") {
        let lp = LimiterParams { ceiling_db: a.db("limiter-ceiling")? as f32, ..LimiterParams::default() };
        dynamics::limit(&mut out.data, rate, &lp);
    }
    ctx.metric("band_gain_reduction_max_db", reductions);
    Ok(out)
}

fn transient(a: &Args, mut buf: AudioBuffer, _ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let at = a.db("attack")? as f32;
    let su = a.db("sustain")? as f32;
    if at == 0.0 && su == 0.0 {
        return Ok(buf);
    }
    let rate = sr(&buf);
    dynamics::transient_shape(&mut buf.data, rate, at, su);
    Ok(buf)
}

fn clip(a: &Args, mut buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let c = a.db("ceiling")? as f32;
    let mode = if a.choice("mode") == "hard" { ClipMode::Hard } else { ClipMode::Soft };
    let knee = a.db("knee")?.max(0.0) as f32;
    let before = buf.peak();
    per_channel(&mut buf, |_, ch| dynamics::clip(ch, c, mode, knee));
    let over = crate::util::lin_to_db(before) - c;
    ctx.metric("input_over_ceiling_db", round2(over.max(0.0)));
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oversample::true_peak;
    use ferraus_core::testing::{noise, run, sine};

    fn spec(name: &str) -> OpSpec {
        ops().into_iter().find(|o| o.name == name).unwrap()
    }

    /// Steady-state gain reduction on a 1 kHz sine at `amp` dBFS peak (RMS detector).
    fn measured_gr(level_db: f64, extra: &[(&str, &str)]) -> f64 {
        let s = spec("compress");
        let amp = 10f64.powf(level_db / 20.0) as f32;
        let x = sine(48000, 1000.0, 1.0, amp);
        let mut pairs = vec![("makeup", "0dB"), ("sidechain-hpf", "0Hz")];
        pairs.extend_from_slice(extra);
        let y = run(&s, &pairs, x.clone()).unwrap();
        let a = &x.data[0][36000..];
        let b = &y.data[0][36000..];
        20.0 * (crate::util::rms(b) as f64 / crate::util::rms(a) as f64).log10()
    }

    #[test]
    fn gain_reduction_matches_static_curve() {
        for level in [-30.0, -21.0, -18.0, -12.0, -6.0, 0.0] {
            let rms_db = level - 3.0103; // sine RMS
            let want = dynamics::compressor_curve(rms_db as f32, -18.0, 3.0, 6.0) as f64;
            let got = measured_gr(level, &[]);
            assert!((got - want).abs() < 0.5, "level {level}: got {got}, want {want}");
        }
        // hard knee, peak detector on a step (square wave avoids ripple)
        let s = spec("compress");
        let x = AudioBuffer::from_mono(48000, (0..48000).map(|i| if (i / 24) % 2 == 0 { 0.5 } else { -0.5 }).collect());
        let y = run(
            &s,
            &[
                ("makeup", "0dB"),
                ("sidechain-hpf", "0Hz"),
                ("detector", "peak"),
                ("knee", "0dB"),
                ("ratio", "4:1"),
                ("threshold", "-20dB"),
            ],
            x,
        )
        .unwrap();
        let want = dynamics::compressor_curve(-6.0206, -20.0, 4.0, 0.0) as f64;
        let got = 20.0 * (y.data[0][40000].abs() as f64 / 0.5).log10();
        assert!((got - want).abs() < 0.5, "{got} vs {want}");
    }

    #[test]
    fn compress_null_and_parallel() {
        let s = spec("compress");
        let x = noise(48000, 0.3, 0.5, 1);
        assert_eq!(run(&s, &[("mix", "0%")], x.clone()).unwrap(), x);
        let full = run(&s, &[], x.clone()).unwrap();
        let half = run(&s, &[("mix", "50%")], x.clone()).unwrap();
        let i = 10000;
        let want = 0.5 * (x.data[0][i] + full.data[0][i]);
        assert!((half.data[0][i] - want).abs() < 1e-5);
    }

    #[test]
    fn tighten_preset_applies_the_chain() {
        let s = spec("compress");
        // 30 Hz rumble + 1 kHz tone: the 60 Hz high-pass must remove most of the rumble
        let x = ferraus_core::testing::add(&sine(48000, 30.0, 1.0, 0.5), &sine(48000, 1000.0, 1.0, 0.2));
        let y = run(&s, &[("preset", "tighten")], x.clone()).unwrap();
        let lvl = |b: &AudioBuffer, f: f64| ferraus_core::testing::tone_level(&b.data[0][24000..], 48000, f);
        // rumble relative to the tone drops by the 60 Hz high-pass's ~12 dB at 30 Hz
        let before = lvl(&x, 30.0) / lvl(&x, 1000.0);
        let after = lvl(&y, 30.0) / lvl(&y, 1000.0);
        assert!(after / before < 0.3, "rumble ratio {after} vs {before}");
        assert!(lvl(&y, 1000.0) > 0.05);
        let a = ferraus_core::testing::args(&s, &[("preset", "tighten")]);
        assert_eq!(a.ratio("ratio").unwrap(), 4.0);
        assert!(a.switch("deess", false));
        // the optional low-pass is off unless asked for
        assert!(a.raw("lowpass").is_none());
    }

    #[test]
    fn limiter_never_exceeds_ceiling() {
        let s = spec("limit");
        let x = ferraus_core::testing::add(&noise(48000, 0.5, 1.5, 4), &sine(48000, 15000.0, 0.5, 0.9));
        let y = run(&s, &[], ferraus_core::testing::stereo(x)).unwrap();
        for ch in &y.data {
            let tp = true_peak(ch, 4);
            assert!(tp <= db_to_lin(-1.0) as f32 + 1e-6, "{}", crate::util::lin_to_db(tp));
        }
    }

    #[test]
    fn multiband_defaults_and_bands_4() {
        let s = spec("multiband");
        let x = noise(48000, 0.3, 0.05, 2);
        // quiet signal under every threshold: bands sum back to (an all-pass of) the input
        let y = run(&s, &[], x.clone()).unwrap();
        let ein = crate::util::rms(&x.data[0]);
        let eout = crate::util::rms(&y.data[0]);
        assert!((20.0 * (eout / ein).log10()).abs() < 0.3);
        let y4 = run(&s, &[("bands", "4"), ("limiter", "true")], noise(48000, 0.3, 1.0, 3)).unwrap();
        assert!(true_peak(&y4.data[0], 4) <= db_to_lin(-1.0) as f32 + 1e-6);
        assert!(run(&s, &[("bands", "3"), ("crossovers", "100Hz")], x.clone()).is_err());
        assert!(run(&s, &[("threshold", "-20dB,-10dB,-5dB,-1dB")], x).is_err());
    }

    #[test]
    fn gate_and_expand_reduce_quiet_parts() {
        let quiet = noise(48000, 0.5, 0.001, 5);
        for name in ["gate", "expand"] {
            let y = run(&spec(name), &[], quiet.clone()).unwrap();
            assert!(crate::util::rms(&y.data[0][12000..]) < crate::util::rms(&quiet.data[0][12000..]) * 0.5, "{name}");
        }
    }

    #[test]
    fn deess_turns_down_sibilance_not_lows() {
        let s = spec("deess");
        let x = ferraus_core::testing::add(&sine(48000, 200.0, 1.0, 0.3), &sine(48000, 7000.0, 1.0, 0.3));
        let y = run(&s, &[], x.clone()).unwrap();
        let lo = ferraus_core::testing::tone_level(&y.data[0][24000..], 48000, 200.0);
        let hi = ferraus_core::testing::tone_level(&y.data[0][24000..], 48000, 7000.0);
        assert!((lo - 0.3).abs() < 0.02, "{lo}");
        assert!(hi < 0.25, "{hi}");
    }

    #[test]
    fn clip_and_transient() {
        let y = run(&spec("clip"), &[], sine(48000, 100.0, 0.2, 2.0)).unwrap();
        assert!(y.peak() <= db_to_lin(-0.3) as f32 + 1e-6);
        let x = ferraus_core::testing::clicks(48000, 0.25, 1.0, 0.8);
        assert_eq!(run(&spec("transient"), &[], x.clone()).unwrap(), x);
        let y = run(&spec("transient"), &[("attack", "+6dB")], sine(48000, 200.0, 0.5, 0.3)).unwrap();
        assert!(y.peak() > 0.3);
    }
}
