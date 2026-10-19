//! The `analyze ...` and `compare` commands. Each is a thin wrapper over a public analysis
//! function: it decodes the input(s), runs the analysis, records headline metrics, and
//! returns the analysis struct (numbers rounded for tidy output) as the JSON result.

use crate::{chords, compare, levels, loudness, problems, spectrum, stereo, tempo, util};
use ferraus_core::op::InputKind;
use ferraus_core::prelude::*;
use serde::Serialize;

pub fn ops() -> Vec<OpSpec> {
    vec![
        OpSpec::custom("analyze tempo", Category::Inspect, "Tempo, beat times, downbeats, and time signature", analyze_tempo)
            .about("Onset strength (spectral flux on a log-mel spectrogram) is autocorrelated and scored on a comb of metrical levels with a perceptual prior around 120 BPM; beats are placed by dynamic programming (Ellis 2007) and the tempo is refined through the beat times. Downbeats are reported only when the bar accent pattern is clear.")
            .inputs(Inputs::ONE_AUDIO)
            .param(param("range", ParamKind::Str).default("60..200").help("Tempo search range in BPM, low..high"))
            .example("ferraus analyze tempo song.wav --json", "BPM, confidence, and beat times")
            .example("ferraus analyze tempo dnb.wav --range 140..190", "Search only fast tempi"),
        OpSpec::custom("analyze loudness", Category::Inspect, "EBU R128 loudness: integrated, short-term, momentary, range, true peak", analyze_loudness)
            .about("Integrated loudness with absolute (-70 LUFS) and relative (-10 LU) gates, maximum short-term (3 s) and momentary (400 ms) loudness, loudness range (EBU Tech 3342), 4x-oversampled true peak, and sample peak. --timeline adds the momentary and short-term curves, like a loudness meter or radar.")
            .inputs(Inputs::ONE_AUDIO)
            .param(param("timeline", ParamKind::Flag).help("Add momentary and short-term loudness curves"))
            .param(param("step", ParamKind::Time).default("100ms").advanced().help("Timeline resolution (multiples of 100 ms)"))
            .example("ferraus analyze loudness master.wav --json", "Integrated LUFS, LRA, and true peak")
            .example("ferraus analyze loudness mix.wav --timeline --step 1s", "Loudness curve every second"),
        OpSpec::custom("analyze silence", Category::Inspect, "List silent regions with start, end, and floor level", analyze_silence)
            .inputs(Inputs::ONE_AUDIO)
            .param(param("threshold", ParamKind::Gain).default("-40dB").help("Level below which audio counts as silence (RMS, loudest channel)"))
            .param(param("min", ParamKind::Time).default("300ms").help("Shortest region reported"))
            .example("ferraus analyze silence podcast.wav --threshold -50dB --min 1s", "Pauses longer than a second"),
        OpSpec::custom("analyze spectrum", Category::Inspect, "Long-term average spectrum in fractional-octave bands or FFT bins", analyze_spectrum)
            .about("Welch-averaged spectrum of all channels. Band levels are in dB relative to a full-scale sine (a 0 dBFS sine in a band reads 0 dB). Also reports the spectral centroid, 85% rolloff, flatness, and strongest frequency.")
            .inputs(Inputs::ONE_AUDIO)
            .param(param("resolution", ParamKind::Choice).choices(&["1/3oct", "1/6oct", "bins"]).default("1/3oct").help("Band resolution"))
            .example("ferraus analyze spectrum mix.wav --json", "Third-octave levels from 20 Hz to 20 kHz"),
        OpSpec::custom("analyze clipping", Category::Inspect, "Count and locate clipped runs", analyze_clipping)
            .about("A clipped run is at least --min-run consecutive samples at or above --threshold with the same polarity. Flat-topped runs below the threshold (audio clipped and then turned down) are reported with kind 'flat'.")
            .inputs(Inputs::ONE_AUDIO)
            .param(param("threshold", ParamKind::Gain).default("0dBFS").help("Clip level"))
            .param(param("min-run", ParamKind::Int).default("3").range("1..1000").help("Consecutive samples that count as a clipped run"))
            .example("ferraus analyze clipping take.wav --json", "Clipped runs with channel and time"),
        OpSpec::custom("analyze chords", Category::Inspect, "Chord sequence with times and confidence (best-effort)", analyze_chords)
            .about("Peak-picked chroma matched against harmonic-aware templates for major, minor, 7, maj7, m7, dim, sus4, and sus2 chords, with a bass-note cue for slash chords and Viterbi smoothing. Best-effort: reliable on clear harmonic material, approximate on dense mixes.")
            .inputs(Inputs::ONE_AUDIO)
            .best_effort()
            .param(param("change-penalty", ParamKind::Float).default("0.5").advanced().help("Smoothing: higher values change chord less often"))
            .param(param("min-duration", ParamKind::Time).default("250ms").advanced().help("Shortest chord segment kept"))
            .example("ferraus analyze chords song.wav --json", "Chord labels such as C, Am7, G/B with times"),
        OpSpec::custom("analyze similarity", Category::Inspect, "Spectral, cepstral, and loudness similarity between two files", analyze_similarity)
            .about("For agents checking whether a processed take drifted from the original: cosine similarity of the log third-octave spectra, MFCC distance, and loudness difference combined into a 0..1 score.")
            .inputs(Inputs::exactly(2, InputKind::Audio))
            .example("ferraus analyze similarity original.wav processed.wav --json", "Score near 1 means the character is unchanged"),
        OpSpec::custom("compare", Category::Inspect, "Null test: align b to a and report RMS and peak difference and SI-SDR", compare_op)
            .about("b is aligned to a by FFT cross-correlation (within --max-offset), resampled and channel-matched if needed, then compared sample by sample: RMS and peak of the difference, null depth, best-fit gain, SI-SDR of b against a, and loudness difference. The agent's regression test.")
            .inputs(Inputs::exactly(2, InputKind::Audio))
            .param(param("max-offset", ParamKind::Time).default("1s").help("Largest alignment offset searched"))
            .param(param("no-align", ParamKind::Flag).help("Compare sample for sample without alignment"))
            .example("ferraus compare mix.wav master_v1.wav --json", "Confirm nothing was damaged"),
        OpSpec::custom("analyze problems", Category::Inspect, "Diagnose clipping, DC, clicks, hum, hiss, phase, silence, and more", analyze_problems)
            .about("Each problem has a severity (low, medium, high), details, and a suggested fix that is a runnable command or fx stage, e.g. 'dc-remove', 'declip', 'highpass:freq=30Hz', or 'silence trim --edges'.")
            .inputs(Inputs::ONE_AUDIO)
            .param(param("min-severity", ParamKind::Choice).choices(&["low", "medium", "high"]).default("low").help("Only list problems at or above this severity"))
            .example("ferraus analyze problems take.wav --json", "Diagnostics with suggested fixes")
            .example("ferraus fx take.wav -o fixed.wav --fx dc-remove --fx \"highpass:freq=30Hz\"", "Apply two suggested fixes"),
        OpSpec::custom("analyze stats", Category::Inspect, "Per-channel peak, true peak, RMS, DC, crest factor, dynamic range", analyze_stats)
            .inputs(Inputs::ONE_AUDIO)
            .param(param("histogram", ParamKind::Switch).default("off").help("Add a 61-bin dB histogram per channel (-60..0 dBFS)"))
            .example("ferraus analyze stats take.wav --histogram on --json", "Amplitude statistics with histograms"),
        OpSpec::custom("analyze stereo", Category::Inspect, "Stereo image: correlation, width per band, mono compatibility", analyze_stereo)
            .about("L/R correlation broadband and below 120 Hz, mid/side balance, width and correlation per band, and a 0..1 mono-compatibility score.")
            .inputs(Inputs::ONE_AUDIO)
            .example("ferraus analyze stereo mix.wav --json", "Check the mix folds to mono"),
    ]
}

/// Serialise an analysis struct with tidy rounding.
fn to_json<T: Serialize>(v: &T) -> Result<Value> {
    let mut j = serde_json::to_value(v)?;
    util::tidy_json(&mut j);
    Ok(j)
}

fn dry_plan(ctx: &OpCtx) -> Option<Value> {
    if ctx.globals.dry_run {
        let inputs: Vec<String> = ctx.inputs.iter().map(|p| p.display().to_string()).collect();
        Some(json!({"analyze": ctx.command, "inputs": inputs}))
    } else {
        None
    }
}

fn read(ctx: &mut OpCtx, i: usize) -> Result<AudioBuffer> {
    let b = ctx.read_audio(i)?;
    if b.is_empty() {
        ctx.warn("empty_input", format!("input {} has no audio frames", i + 1));
    }
    Ok(b)
}

fn finite(v: f64) -> Option<f64> {
    if v.is_finite() {
        Some(util::tidy(v))
    } else {
        None
    }
}

fn parse_bpm_range(s: &str) -> Result<(f64, f64)> {
    let parse = |t: &str| -> Result<f64> {
        let t = t.trim().trim_end_matches("bpm").trim_end_matches("BPM").trim();
        t.parse::<f64>().map_err(|_| Error::usage(format!("'{t}' is not a tempo in BPM")))
    };
    let (lo, hi) = units::parse_num_range(s, parse).map_err(|e| Error::usage(format!("--range: {}", e.message)))?;
    if !(lo > 0.0 && hi > lo && hi <= 1000.0) {
        return Err(Error::usage(format!("--range must be low..high with 0 < low < high <= 1000, got '{s}'")));
    }
    Ok((lo, hi))
}

fn analyze_tempo(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let (lo, hi) = parse_bpm_range(&a.str_or("range", "60..200"))?;
    if let Some(p) = dry_plan(ctx) {
        return Ok(p);
    }
    let buf = read(ctx, 0)?;
    let opts = tempo::TempoOptions { min_bpm: lo, max_bpm: hi, ..Default::default() };
    let r = tempo::detect_tempo(&buf, &opts);
    if r.beats.is_empty() {
        ctx.warn("no_beats", "no regular beat was found");
    } else if r.confidence < 0.3 {
        ctx.warn("low_confidence", format!("tempo confidence is low ({:.2})", r.confidence));
    }
    ctx.metric("bpm", util::tidy(r.bpm));
    ctx.metric("confidence", util::tidy(r.confidence));
    ctx.metric("beats", r.beats.len());
    ctx.metric("time_signature", &r.time_signature);
    to_json(&r)
}

fn analyze_loudness(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let step = if a.flag("timeline") { Some(a.seconds("step")?.max(0.1)) } else { None };
    if let Some(p) = dry_plan(ctx) {
        return Ok(p);
    }
    let buf = read(ctx, 0)?;
    let r = loudness::loudness_with(&buf, step);
    if !r.integrated_lufs.is_finite() {
        ctx.warn("unmeasurable", "integrated loudness is undefined (silence or shorter than 400 ms)");
    }
    ctx.metric("integrated_lufs", finite(r.integrated_lufs));
    ctx.metric("loudness_range_lu", util::tidy(r.loudness_range_lu));
    ctx.metric("true_peak_dbtp", util::tidy(r.true_peak_dbtp));
    ctx.metric("short_term_max_lufs", finite(r.short_term_max_lufs));
    to_json(&r)
}

fn analyze_silence(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let thr = a.db("threshold")?;
    let min = a.seconds("min")?;
    if let Some(p) = dry_plan(ctx) {
        return Ok(p);
    }
    let buf = read(ctx, 0)?;
    let r = levels::detect_silence(&buf, thr, min);
    ctx.metric("regions", r.regions.len());
    ctx.metric("total_silence_s", util::tidy(r.total_silence_s));
    to_json(&r)
}

fn analyze_spectrum(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let res = spectrum::Resolution::parse(&a.choice("resolution"))
        .ok_or_else(|| Error::usage("--resolution must be 1/3oct, 1/6oct, or bins"))?;
    if let Some(p) = dry_plan(ctx) {
        return Ok(p);
    }
    let buf = read(ctx, 0)?;
    let r = spectrum::spectrum(&buf, res);
    ctx.metric("centroid_hz", util::tidy(r.centroid_hz));
    ctx.metric("rolloff_hz", util::tidy(r.rolloff_hz));
    to_json(&r)
}

fn analyze_clipping(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let thr = a.db("threshold")?;
    let min_run = a.usize("min-run")?.max(1);
    if let Some(p) = dry_plan(ctx) {
        return Ok(p);
    }
    let buf = read(ctx, 0)?;
    let r = levels::detect_clipping(&buf, thr, min_run);
    if r.truncated {
        ctx.warn("truncated", format!("only the first {} of {} runs are listed", r.runs.len(), r.count));
    }
    ctx.metric("clipped_runs", r.count);
    ctx.metric("clipped_samples", r.clipped_samples);
    to_json(&r)
}

fn analyze_chords(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let opts = chords::ChordOptions {
        change_penalty: a.f64("change-penalty")?.max(0.0),
        min_duration_s: a.seconds("min-duration")?.max(0.0),
    };
    if let Some(p) = dry_plan(ctx) {
        return Ok(p);
    }
    let buf = read(ctx, 0)?;
    let r = chords::detect_chords(&buf, &opts);
    ctx.metric("segments", r.segments.len());
    ctx.metric("distinct_chords", r.distinct_chords);
    to_json(&r)
}

fn analyze_similarity(_a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    if let Some(p) = dry_plan(ctx) {
        return Ok(p);
    }
    let a = read(ctx, 0)?;
    let b = read(ctx, 1)?;
    let r = compare::similarity(&a, &b);
    ctx.metric("score", util::tidy(r.score));
    to_json(&r)
}

fn compare_op(args: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let opts =
        compare::CompareOptions { max_offset_s: args.seconds("max-offset")?.max(0.0), no_align: args.flag("no-align") };
    if let Some(p) = dry_plan(ctx) {
        return Ok(p);
    }
    let a = read(ctx, 0)?;
    let b = read(ctx, 1)?;
    let r = compare::compare(&a, &b, &opts);
    for n in &r.notes {
        ctx.warn("conformed", n.clone());
    }
    if r.length_difference_s.abs() > 0.01 {
        ctx.warn("length_differs", format!("b is {:+.3} s longer than a", r.length_difference_s));
    }
    ctx.metric("si_sdr_db", util::tidy(r.si_sdr_db));
    ctx.metric("rms_diff_db", util::tidy(r.rms_diff_db));
    ctx.metric("offset_samples", r.offset_samples);
    ctx.metric("verdict", &r.verdict);
    to_json(&r)
}

fn analyze_problems(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let min = match a.choice("min-severity").as_str() {
        "high" => problems::Severity::High,
        "medium" => problems::Severity::Medium,
        _ => problems::Severity::Low,
    };
    if let Some(p) = dry_plan(ctx) {
        return Ok(p);
    }
    let buf = read(ctx, 0)?;
    let mut r = problems::diagnose(&buf);
    r.problems.retain(|p| p.severity >= min);
    ctx.metric("high", r.counts.high);
    ctx.metric("medium", r.counts.medium);
    ctx.metric("low", r.counts.low);
    ctx.metric("clean", r.clean);
    to_json(&r)
}

fn analyze_stats(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let hist = a.switch("histogram", false);
    if let Some(p) = dry_plan(ctx) {
        return Ok(p);
    }
    let buf = read(ctx, 0)?;
    let r = levels::stats(&buf, hist);
    ctx.metric("peak_db", util::tidy(r.overall.peak_db));
    ctx.metric("true_peak_dbtp", util::tidy(r.overall.true_peak_dbtp));
    ctx.metric("rms_db", util::tidy(r.overall.rms_db));
    ctx.metric("crest_factor_db", util::tidy(r.overall.crest_factor_db));
    to_json(&r)
}

fn analyze_stereo(_a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    if let Some(p) = dry_plan(ctx) {
        return Ok(p);
    }
    let buf = read(ctx, 0)?;
    if buf.channels() < 2 {
        ctx.warn("mono_input", "the input is mono; it folds to mono perfectly");
    }
    let r = stereo::stereo_analysis(&buf);
    ctx.metric("correlation", util::tidy(r.correlation));
    ctx.metric("correlation_below_120hz", util::tidy(r.correlation_below_120hz));
    ctx.metric("mono_compatibility", util::tidy(r.mono_compatibility));
    to_json(&r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferraus_core::op::Runner;
    use ferraus_core::testing::{self, sine, stereo, TempDir};

    fn spec(name: &str) -> OpSpec {
        ops().into_iter().find(|o| o.name == name).unwrap()
    }

    fn run(name: &str, pairs: &[(&str, &str)], inputs: Vec<std::path::PathBuf>) -> (Value, OpCtx<'static>) {
        let s = spec(name);
        let a = testing::args(&s, pairs);
        let mut c = testing::ctx();
        c.command = s.name.to_string();
        c.inputs = inputs;
        let v = match s.runner {
            Runner::Custom(f) => f(&a, &mut c).unwrap(),
            _ => unreachable!(),
        };
        (v, c)
    }

    #[test]
    fn registry_accepts_all_ops() {
        let r = testing::leak_registry(ops());
        for name in [
            "analyze tempo",
            "analyze loudness",
            "analyze silence",
            "analyze spectrum",
            "analyze clipping",
            "analyze chords",
            "analyze similarity",
            "compare",
            "analyze problems",
            "analyze stats",
            "analyze stereo",
        ] {
            assert!(r.get(name).is_some(), "{name}");
        }
        assert!(spec("analyze chords").best_effort);
        assert!(ops().iter().all(|o| o.category == Category::Inspect));
    }

    #[test]
    fn loudness_op_end_to_end() {
        let d = TempDir::new("analysis-loud");
        let p = d.path("tone.wav");
        testing::write_wav(&p, &stereo(sine(48000, 1000.0, 5.0, ferraus_core::db_to_lin(-23.0) as f32)));
        let (v, c) = run("analyze loudness", &[("timeline", "true"), ("step", "1s")], vec![p]);
        assert!((v["integrated_lufs"].as_f64().unwrap() + 23.0).abs() < 0.1);
        assert_eq!(v["timeline"]["t_s"].as_array().unwrap().len(), 5);
        assert!(c.metrics.contains_key("integrated_lufs"));
    }

    #[test]
    fn tempo_and_compare_ops() {
        let d = TempDir::new("analysis-ops");
        let a = d.path("a.wav");
        let b = d.path("b.wav");
        let clicks = testing::clicks(22050, 0.5, 6.0, 0.8);
        testing::write_wav(&a, &clicks);
        let mut delayed = AudioBuffer::new(22050, 1, 100);
        delayed.append(&clicks);
        testing::write_wav(&b, &delayed);
        let (v, c) = run("analyze tempo", &[("range", "60..200")], vec![a.clone()]);
        assert!((v["bpm"].as_f64().unwrap() - 120.0).abs() < 1.0);
        assert!(c.metrics.contains_key("bpm"));
        let (v, _) = run("compare", &[], vec![a.clone(), b.clone()]);
        assert_eq!(v["offset_samples"].as_i64().unwrap(), 100);
        let (v, _) = run("analyze similarity", &[], vec![a.clone(), b]);
        assert!(v["score"].as_f64().unwrap() > 0.9);
        // bad range
        let s = spec("analyze tempo");
        let args = testing::args(&s, &[("range", "200..60")]);
        let mut c = testing::ctx();
        c.inputs = vec![a];
        if let Runner::Custom(f) = s.runner {
            assert!(f(&args, &mut c).is_err());
        }
    }

    #[test]
    fn other_ops_return_results() {
        let d = TempDir::new("analysis-misc");
        let p = d.path("x.wav");
        let mut buf = AudioBuffer::new(48000, 2, 48000);
        buf.append(&stereo(sine(48000, 440.0, 2.0, 0.5)));
        testing::write_wav(&p, &buf);
        let (v, _) = run("analyze silence", &[], vec![p.clone()]);
        assert_eq!(v["regions"].as_array().unwrap().len(), 1);
        let (v, _) = run("analyze spectrum", &[("resolution", "1/6oct")], vec![p.clone()]);
        assert!(v["bands"].as_array().unwrap().len() > 50);
        let (v, _) = run("analyze clipping", &[], vec![p.clone()]);
        assert_eq!(v["count"].as_u64().unwrap(), 0);
        let (v, _) = run("analyze stats", &[("histogram", "on")], vec![p.clone()]);
        assert!(v["per_channel"][0]["histogram"]["counts"].is_array());
        let (v, _) = run("analyze problems", &[], vec![p.clone()]);
        assert!(v["problems"].as_array().unwrap().iter().any(|x| x["problem"] == "silence_head"));
        let (v, _) = run("analyze problems", &[("min-severity", "high")], vec![p.clone()]);
        assert!(v["problems"].as_array().unwrap().is_empty());
        let (v, _) = run("analyze stereo", &[], vec![p.clone()]);
        assert!(v["correlation"].as_f64().unwrap() > 0.99);
        let (v, _) = run("analyze chords", &[], vec![p]);
        assert_eq!(v["best_effort"], true);
    }

    #[test]
    fn dry_run_does_not_decode() {
        let s = spec("analyze loudness");
        let a = testing::args(&s, &[]);
        let mut c = testing::ctx();
        c.globals.dry_run = true;
        c.inputs = vec!["/nonexistent.wav".into()];
        if let Runner::Custom(f) = s.runner {
            assert!(f(&a, &mut c).is_ok());
        }
    }
}
