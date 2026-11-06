//! The `analyze ...` and `compare` commands. Each is a thin wrapper over a public analysis
//! function: it decodes the input(s), runs the analysis, records headline metrics, and
//! returns the analysis struct (numbers rounded for tidy output) as the JSON result.

use crate::{chords, compare, levels, loudness, masking, problems, sections, spectrum, stereo, tempo, util};
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
            .example("ferraus analyze tempo dnb.wav --range 140..190", "Search only fast tempi")
            .example("ferraus analyze tempo ballad.wav --range 50..100 --json", "Keep a slow ballad from being read at double time"),
        OpSpec::custom("analyze loudness", Category::Inspect, "EBU R128 loudness: integrated, short-term, momentary, range, true peak", analyze_loudness)
            .about("Integrated loudness with absolute (-70 LUFS) and relative (-10 LU) gates, maximum short-term (3 s) and momentary (400 ms) loudness, loudness range (EBU Tech 3342), 4x-oversampled true peak, and sample peak. --timeline adds the momentary and short-term curves, like a loudness meter or radar.")
            .inputs(Inputs::ONE_AUDIO)
            .param(param("timeline", ParamKind::Flag).help("Add momentary and short-term loudness curves"))
            .param(param("step", ParamKind::Time).default("100ms").advanced().help("Timeline resolution (multiples of 100 ms)"))
            .example("ferraus analyze loudness master.wav --json", "Integrated LUFS, LRA, and true peak")
            .example("ferraus analyze loudness mix.wav --timeline --step 1s", "Loudness curve every second")
            .example("ferraus analyze loudness episode.mp3 --json > loudness.json", "Save the loudness report of a podcast episode for a delivery check"),
        OpSpec::custom("analyze silence", Category::Inspect, "List silent regions with start, end, and floor level", analyze_silence)
            .inputs(Inputs::ONE_AUDIO)
            .param(param("threshold", ParamKind::Gain).default("-40dB").help("Level below which audio counts as silence (RMS, loudest channel)"))
            .param(param("min", ParamKind::Time).default("300ms").help("Shortest region reported"))
            .example("ferraus analyze silence podcast.wav --threshold -50dB --min 1s", "Pauses longer than a second")
            .example("ferraus analyze silence take.wav --json", "Silent regions with the defaults (-40 dB, at least 300 ms)")
            .example("ferraus analyze silence field.wav --threshold -60dB --min 100ms --json", "Short gaps in a quiet field recording"),
        OpSpec::custom("analyze spectrum", Category::Inspect, "Long-term average spectrum in fractional-octave bands or FFT bins", analyze_spectrum)
            .about("Welch-averaged spectrum of all channels. Band levels are in dB relative to a full-scale sine (a 0 dBFS sine in a band reads 0 dB). Also reports the spectral centroid, 85% rolloff, flatness, and strongest frequency.")
            .inputs(Inputs::ONE_AUDIO)
            .param(param("resolution", ParamKind::Choice).choices(&["1/3oct", "1/6oct", "bins"]).default("1/3oct").help("Band resolution"))
            .example("ferraus analyze spectrum mix.wav --json", "Third-octave levels from 20 Hz to 20 kHz")
            .example("ferraus analyze spectrum vocal.wav --resolution 1/6oct --json", "Finer sixth-octave bands to find a resonance")
            .example("ferraus analyze spectrum hum.wav --resolution bins --json", "Raw FFT bins, for example to locate a hum line exactly"),
        OpSpec::custom("analyze clipping", Category::Inspect, "Count and locate clipped runs", analyze_clipping)
            .about("A clipped run is at least --min-run consecutive samples at or above --threshold with the same polarity. Flat-topped runs below the threshold (audio clipped and then turned down) are reported with kind 'flat'.")
            .inputs(Inputs::ONE_AUDIO)
            .param(param("threshold", ParamKind::Gain).default("0dBFS").help("Clip level"))
            .param(param("min-run", ParamKind::Int).default("3").range("1..1000").help("Consecutive samples that count as a clipped run"))
            .example("ferraus analyze clipping take.wav --json", "Clipped runs with channel and time")
            .example("ferraus analyze clipping master.wav --threshold -0.1dB --min-run 2 --json", "Count near-full-scale runs of two or more samples in a loud master")
            .example("ferraus analyze clipping old_transfer.wav --min-run 1", "Report every sample at full scale, even single ones"),
        OpSpec::custom("analyze chords", Category::Inspect, "Chord sequence with times and confidence (best-effort)", analyze_chords)
            .about("Peak-picked chroma matched against harmonic-aware templates for major, minor, 7, maj7, m7, dim, sus4, and sus2 chords, with a bass-note cue for slash chords and Viterbi smoothing. Best-effort: reliable on clear harmonic material, approximate on dense mixes.")
            .inputs(Inputs::ONE_AUDIO)
            .best_effort()
            .param(param("change-penalty", ParamKind::Float).default("0.5").advanced().help("Smoothing: higher values change chord less often"))
            .param(param("min-duration", ParamKind::Time).default("250ms").advanced().help("Shortest chord segment kept"))
            .example("ferraus analyze chords song.wav --json", "Chord labels such as C, Am7, G/B with times")
            .example("ferraus analyze chords guitar.wav --change-penalty 1.5 --json", "Fewer, steadier chord changes on a strummed part")
            .example("ferraus analyze chords piano.wav --min-duration 500ms --json > chords.json", "Drop chord segments shorter than half a second and save the result"),
        OpSpec::custom("analyze similarity", Category::Inspect, "Spectral, cepstral, and loudness similarity between two files", analyze_similarity)
            .about("For agents checking whether a processed take drifted from the original: cosine similarity of the log third-octave spectra, MFCC distance, and loudness difference combined into a 0..1 score.")
            .inputs(Inputs::exactly(2, InputKind::Audio))
            .example("ferraus analyze similarity original.wav processed.wav --json", "Score near 1 means the character is unchanged")
            .example("ferraus analyze similarity mix.wav master.wav --json", "Check that mastering kept the character of the mix")
            .example("ferraus analyze similarity take1.wav take2.wav", "Compare two takes of the same part"),
        OpSpec::custom("analyze masking", Category::Inspect, "Where stem b masks stem a (same bands, same time), with an EQ cut for b", analyze_masking)
            .about("For mixing by numbers: a is the stem that keeps its place (vocal, kick), b the one that yields. Per third-octave band (31.5 Hz to 16 kHz) the spectral overlap is 1 when both stems peak there and falls to 0 when either is 18 dB below its own loudest band. It is weighted by co-activity, the share of the --window frames in which a plays that b plays too (a frame plays when its RMS is within --floor of the stem's loudest frame), so a kick that hits a fifth of the time still counts fully against a bass that is always under it, and parts that never play together cannot score above 0.25. With --levels as-is (the default, for balanced or processed stems) it is also weighted by level: margin_db is how far a sits above b in the band while each plays, and the band stops counting as b falls 10 dB under a, so a cut or a fader move on b lowers the score; with --levels ignore (raw stems whose gains mean nothing yet) only the spectral shapes count. score = overlap x (0.25 + 0.75 x co-activity) x level. Runs of bands at or above --threshold are reported worst first with a peak cut for b as a runnable fx stage in suggested_fix (level-aware: the cut that takes the band under the threshold; shape-only: 2 dB at the threshold, up to --max-cut at a score of 1); the top-level suggested_fix joins every region's cut into one eq stage. Both stems are mixed to mono and read from their first sample, so they must start together, as exported stems do.")
            .inputs(Inputs::exactly(2, InputKind::Audio))
            .param(param("threshold", ParamKind::Percent).default("50%").help("Band score at or above which a region is reported (0..100%)"))
            .param(param("window", ParamKind::Time).default("100ms").help("Frame length for deciding when each stem is playing"))
            .param(param("floor", ParamKind::Gain).default("-30dB").help("A frame counts as playing when its RMS is within this many dB of the stem's loudest frame"))
            .param(param("max-cut", ParamKind::Gain).default("-4dB").help("Deepest suggested cut for b"))
            .param(param("levels", ParamKind::Choice).choices(&["as-is", "ignore"]).default("as-is").help("as-is: a band only counts while b is within 10 dB of a there (balanced or processed stems; cuts and fader moves lower the score); ignore: compare spectral shapes only (raw, unbalanced stems)"))
            .example("ferraus analyze masking kick.wav bass.wav --json", "Where the bass crowds the kick; every suggested_fix is an EQ cut for the bass")
            .example("ferraus analyze masking vocal.wav keys.wav --threshold 40% --json", "Also report milder overlap between a lead vocal and a keys part")
            .example("ferraus analyze masking vocals.wav instrumental.wav --max-cut -3dB --json", "Carve room for the vocal in a full instrumental, at most 3 dB per cut")
            .example("ferraus analyze masking lead.wav pad.wav --window 250ms --floor -24dB --json", "Longer frames and a stricter activity floor for sustained parts")
            .example("ferraus analyze masking vocal_raw.wav guitar_raw.wav --levels ignore --json", "Raw stems whose levels mean nothing yet: where could they clash once balanced")
            .example("ferraus fx bass.wav -o bass_eq.wav --fx \"eq:band=peak:80Hz:-3dB:q=1.4\"", "Apply a region's suggested_fix to b"),
        OpSpec::custom("analyze sections", Category::Inspect, "Song structure: sections, repeats, and loudness per section", analyze_sections)
            .about("For section-aware mixing: where the verses and choruses are, which sections repeat, and how loud each one is, from a mix, a rough mix, or an instrumental. Plain DSP, no model, best-effort. The input is mixed to mono. Beats come from the tempo tracker; --bpm sets tempo_bpm and the beats are taken from the tracked pulse at that tempo, half, or double (every other beat, or midpoints), else tracked again within 4% of it. Without --bpm, a reading above 160 BPM whose half tempo scores nearly as well is taken at half speed, since bars shorter than 1.5 s are rare. Bars start on the tracker's downbeats when the bar accent is clear (grid downbeats), else on the beat phase with the strongest kick and chord-change accents (grid beats), and with no steady beat on a fixed 2 s grid of four 0.5 s units (grid seconds). Per beat it measures chroma, spectral shape (cepstrum of a 40-band log-mel spectrum), onset pattern (sixteenths, below and above 200 Hz), and level. Boundaries are peaks, on bar starts and at least --min-bars apart, of Foote checkerboard novelty (Gaussian-tapered kernels at two scales) on a bar similarity matrix of spectral shape and onset pattern, plus harmonic change (pitch content of the 4 and 8 bars before against the bars after) and level jumps; a higher --sensitivity finds more. Sections share a letter (A, B, ... by first appearance) when their similarity is at least --similarity and, for a letter that already has several sections, no more than 0.1 below how alike those sections are to each other (a verse at 0.85 to two choruses that match at 0.96 keeps its own letter). The similarity weighs most the chroma compared beat by beat, bar by bar (best aligned within the length difference plus one bar), since the melody over the chords tells a verse from a chorus on the same progression; then onset patterns, mean spectral shape, level, and, with --vocal, how much of each section is sung. each same-letter pair is listed in repeats with offset_s, the time from a bar in one section to the same bar in its twin, so a bad note at t in section a can be patched from t + offset_s. lufs and delta_lu (against the whole song) are measured per section. Roles are heuristics with a role_confidence: the loudest repeating label (sung, with --vocal) is the chorus; another repeating label first heard before it is the verse (or, when no other label repeats, the section right before the first chorus); a short section between a verse and a chorus is the pre-chorus; a unique section in the second half between choruses is the bridge, or instrumental when the vocal stem is silent there; distinct first and last sections are the intro and outro; anything else is 'section'. --vocal (the lead vocal stem, starting with the mix) adds vocal_activity per section and makes the roles far more reliable. Limits: a tempo read at half or double speed halves or doubles the bars (pass --bpm); a verse and chorus over the same chords and the same arrangement can merge into one section; a key change splits repeats into different letters; sections shorter than --min-bars are absorbed by a neighbour; roles are guesses (check role_confidence and listen).")
            .inputs(Inputs::ONE_AUDIO)
            .best_effort()
            .param(param("min-bars", ParamKind::Int).default("4").range("1..64").help("Shortest section, in bars"))
            .param(param("sensitivity", ParamKind::Percent).default("50%").advanced().help("Boundary sensitivity: higher finds more boundaries (0..100%)"))
            .param(param("similarity", ParamKind::Percent).default("80%").advanced().help("How alike two sections must be to share a label (0..100%)"))
            .param(param("beats-per-bar", ParamKind::Int).range("1..16").advanced().help("Beats per bar, overriding the detected meter"))
            .param(param("vocal", ParamKind::Path).help("Lead vocal stem of the same song, starting together with the mix: adds vocal_activity per section and improves the role guesses"))
            .example("ferraus analyze sections mix.wav --json", "Sections with labels, roles, bars, loudness per section, and the repeats")
            .example("ferraus analyze sections mix.wav --bpm 108 --json", "Use the known tempo instead of the detected one (fixes bars read at half or double speed)")
            .example("ferraus analyze sections mix.wav --vocal stems/vocals.wav --json", "Add vocal activity per section for surer verse, chorus, and instrumental roles")
            .example("ferraus analyze sections demo.wav --min-bars 8 --similarity 70% --json", "Only sections of 8 bars or more, and looser matching of repeats in a rough demo"),
        OpSpec::custom("compare", Category::Inspect, "Null test: align b to a and report RMS and peak difference and SI-SDR", compare_op)
            .about("b is aligned to a by FFT cross-correlation (within --max-offset), resampled and channel-matched if needed, then compared sample by sample: RMS and peak of the difference, null depth, best-fit gain, SI-SDR of b against a, and loudness difference. The agent's regression test.")
            .inputs(Inputs::exactly(2, InputKind::Audio))
            .param(param("max-offset", ParamKind::Time).default("1s").help("Largest alignment offset searched"))
            .param(param("no-align", ParamKind::Flag).help("Compare sample for sample without alignment"))
            .example("ferraus compare mix.wav master_v1.wav --json", "Confirm nothing was damaged")
            .example("ferraus compare original.wav roundtrip.wav --no-align --json", "Sample-for-sample null test of a format round trip")
            .example("ferraus compare stem_sum.wav mix.wav --max-offset 5s", "Allow up to five seconds of offset when the files start at different times"),
        OpSpec::custom("analyze problems", Category::Inspect, "Diagnose clipping, DC, clicks, hum, hiss, phase, silence, and more", analyze_problems)
            .about("Each problem has a severity (low, medium, high), details, and a suggested fix that is a runnable command or fx stage, e.g. 'dc-remove', 'declip', 'highpass:freq=30Hz', or 'silence trim --edges'.")
            .inputs(Inputs::ONE_AUDIO)
            .param(param("min-severity", ParamKind::Choice).choices(&["low", "medium", "high"]).default("low").help("Only list problems at or above this severity"))
            .example("ferraus analyze problems take.wav --json", "Diagnostics with suggested fixes")
            .example("ferraus fx take.wav -o fixed.wav --fx dc-remove --fx \"highpass:freq=30Hz\"", "Apply two suggested fixes")
            .example("ferraus analyze problems interview.wav --min-severity medium --json", "Only the medium and high severity problems"),
        OpSpec::custom("analyze stats", Category::Inspect, "Per-channel peak, true peak, RMS, DC, crest factor, dynamic range", analyze_stats)
            .inputs(Inputs::ONE_AUDIO)
            .param(param("histogram", ParamKind::Switch).default("off").help("Add a 61-bin dB histogram per channel (-60..0 dBFS)"))
            .example("ferraus analyze stats take.wav --histogram on --json", "Amplitude statistics with histograms")
            .example("ferraus analyze stats mix.wav --json", "Peak, true peak, RMS, DC offset, and crest factor per channel")
            .example("ferraus analyze stats master.wav --json > stats.json", "Save the statistics of a master as JSON"),
        OpSpec::custom("analyze stereo", Category::Inspect, "Stereo image: correlation, width per band, mono compatibility", analyze_stereo)
            .about("L/R correlation broadband and below 120 Hz, mid/side balance, width and correlation per band, and a 0..1 mono-compatibility score.")
            .inputs(Inputs::ONE_AUDIO)
            .example("ferraus analyze stereo mix.wav --json", "Check the mix folds to mono")
            .example("ferraus analyze stereo master.wav", "Correlation and width per band printed for a person")
            .example("ferraus analyze stereo widened.wav --json > stereo.json", "Save the stereo report after widening to check mono compatibility"),
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

/// Read input `i` mixed to mono (masking works on mono; dropping the channels early halves
/// the memory held for long stereo stems).
fn read_mono(ctx: &mut OpCtx, i: usize) -> Result<AudioBuffer> {
    let b = read(ctx, i)?;
    Ok(if b.channels() > 1 { AudioBuffer::from_mono(b.sample_rate, b.mono_mix()) } else { b })
}

fn analyze_masking(args: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let threshold = args.percent("threshold")?;
    if !(threshold > 0.0 && threshold <= 1.0) {
        return Err(Error::usage(format!(
            "--threshold must be above 0% and at most 100%, got '{}'",
            args.str_or("threshold", "")
        )));
    }
    let window_s = args.seconds("window")?;
    if !(0.01..=10.0).contains(&window_s) {
        return Err(Error::usage(format!(
            "--window must be between 10ms and 10s, got '{}'",
            args.str_or("window", "")
        )));
    }
    let floor_db = -args.db("floor")?.abs();
    if !(1.0..=120.0).contains(&-floor_db) {
        return Err(Error::usage(format!(
            "--floor must be between -1dB and -120dB, got '{}'",
            args.str_or("floor", "")
        )));
    }
    let max_cut_db = -args.db("max-cut")?.abs();
    if !(0.5..=24.0).contains(&-max_cut_db) {
        return Err(Error::usage(format!(
            "--max-cut must be between -0.5dB and -24dB, got '{}'",
            args.str_or("max-cut", "")
        )));
    }
    if let Some(p) = dry_plan(ctx) {
        return Ok(p);
    }
    let name = |ctx: &OpCtx, i: usize, d: &str| -> String {
        ctx.inputs
            .get(i)
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| d.into())
    };
    let (a_label, b_label) = (name(ctx, 0, "a"), name(ctx, 1, "b"));
    let a = read_mono(ctx, 0)?;
    let b = read_mono(ctx, 1)?;
    let opts = masking::MaskingOptions {
        threshold,
        window_s,
        floor_db,
        max_cut_db,
        levels: args.choice("levels") != "ignore",
        a_label: a_label.clone(),
        b_label: b_label.clone(),
    };
    if b.sample_rate != a.sample_rate {
        ctx.note("resampled_b_from_hz", b.sample_rate);
    }
    let r = masking::masking(&a, &b, &opts);
    drop((a, b));
    for (silent, label) in [(r.a_silent, &a_label), (r.b_silent, &b_label)] {
        if silent {
            ctx.warn("silent_input", format!("{label} has no audio above -80 dBFS; it cannot mask or be masked"));
        }
    }
    if !r.a_silent && !r.b_silent && r.co_activity < 0.2 {
        ctx.warn(
            "low_co_activity",
            format!(
                "{b_label} plays during only {:.0}% of the time {a_label} plays; masking between them is unlikely to matter",
                r.co_activity * 100.0
            ),
        );
    }
    if r.length_difference_s.abs() > 1.0 {
        let d = r.length_difference_s;
        let rel = if d > 0.0 { "longer" } else { "shorter" };
        ctx.warn(
            "length_differs",
            format!("{b_label} is {:.1} s {rel} than {a_label}; both are read from their first sample, so they must start together", d.abs()),
        );
    }
    ctx.metric("overall", util::tidy(r.overall));
    ctx.metric("co_activity", util::tidy(r.co_activity));
    ctx.metric("regions", r.regions.len());
    let mut out = serde_json::Map::new();
    for i in 0..2 {
        let p = ctx.inputs.get(i).map(|p| p.display().to_string()).unwrap_or_default();
        out.insert(if i == 0 { "a" } else { "b" }.into(), Value::String(p));
    }
    if let Value::Object(m) = to_json(&r)? {
        out.extend(m);
    }
    Ok(Value::Object(out))
}

fn analyze_sections(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let min_bars = a.int("min-bars")?;
    if !(1..=64).contains(&min_bars) {
        return Err(Error::usage(format!("--min-bars must be between 1 and 64, got {min_bars}")));
    }
    let percent = |name: &str| -> Result<f64> {
        let v = a.percent(name)?;
        if (0.0..=1.0).contains(&v) {
            Ok(v)
        } else {
            Err(Error::usage(format!("--{name} must be between 0% and 100%, got '{}'", a.str_or(name, ""))))
        }
    };
    let sensitivity = percent("sensitivity")?;
    let similarity = percent("similarity")?;
    let beats_per_bar = match a.raw("beats-per-bar") {
        None => None,
        Some(_) => {
            let v = a.int("beats-per-bar")?;
            if !(1..=16).contains(&v) {
                return Err(Error::usage(format!("--beats-per-bar must be between 1 and 16, got {v}")));
            }
            Some(v as usize)
        }
    };
    let bpm = ctx.bpm();
    if let Some(b) = bpm {
        if !(b.is_finite() && (20.0..=400.0).contains(&b)) {
            return Err(Error::usage(format!("--bpm must be between 20 and 400 for analyze sections, got {b}")));
        }
    }
    if let Some(mut p) = dry_plan(ctx) {
        if let Some(v) = a.path("vocal") {
            p["vocal"] = Value::String(v.display().to_string());
        }
        return Ok(p);
    }
    let buf = read(ctx, 0)?;
    let vocal = match a.path("vocal") {
        None => None,
        Some(p) => {
            let v = ctx.read_audio_path(&p)?;
            if v.is_empty() {
                return Err(Error::input(format!("the vocal stem {} has no audio", p.display())));
            }
            Some(if v.channels() > 1 { AudioBuffer::from_mono(v.sample_rate, v.mono_mix()) } else { v })
        }
    };
    let opts = sections::SectionOptions { min_bars: min_bars as usize, sensitivity, similarity, beats_per_bar, bpm };
    let r = sections::sections(&buf, vocal.as_ref(), &opts);
    drop((buf, vocal));
    for w in &r.warnings {
        ctx.warn(w.code, w.message.clone());
    }
    ctx.metric("sections", r.sections.len());
    ctx.metric("labels", r.labels.len());
    ctx.metric("confidence", util::tidy(r.confidence));
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
        let d = r.length_difference_s;
        let rel = if d > 0.0 { "longer" } else { "shorter" };
        ctx.warn("length_differs", format!("b is {:.3} s {rel} than a", d.abs()));
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
            "analyze masking",
            "analyze sections",
            "compare",
            "analyze problems",
            "analyze stats",
            "analyze stereo",
        ] {
            assert!(r.get(name).is_some(), "{name}");
        }
        assert!(spec("analyze chords").best_effort);
        assert!(spec("analyze sections").best_effort);
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
    fn masking_op_end_to_end() {
        let d = TempDir::new("analysis-masking");
        let (kick, bass, apart) = (d.path("kick.wav"), d.path("bass.wav"), d.path("apart.wav"));
        testing::write_wav(&kick, &stereo(sine(22050, 63.0, 2.0, 0.5)));
        testing::write_wav(&bass, &sine(22050, 60.0, 2.0, 0.3));
        let (v, c) = run("analyze masking", &[("levels", "ignore")], vec![kick.clone(), bass.clone()]);
        assert_eq!(v["a"], kick.display().to_string());
        assert_eq!(v["b"], bass.display().to_string());
        let regions = v["regions"].as_array().unwrap();
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0]["center_hz"].as_f64().unwrap(), 63.0);
        assert_eq!(regions[0]["suggested_fix"], "eq:band=peak:63Hz:-4dB:q=1.4");
        assert_eq!(regions[0]["apply_to"], "b");
        assert!(v["bands"].as_array().unwrap().iter().all(|b| b["score"].as_f64().unwrap() <= 1.0));
        assert!(v["summary"].as_str().unwrap().starts_with("bass.wav masks kick.wav"), "{}", v["summary"]);
        assert_eq!(c.metrics["regions"], 1);
        assert_eq!(c.metrics["overall"].as_f64().unwrap(), 1.0);
        assert!(c.warnings.is_empty());
        let (v, _) =
            run("analyze masking", &[("max-cut", "3dB"), ("levels", "ignore")], vec![kick.clone(), bass.clone()]);
        assert_eq!(v["regions"][0]["cut_db"].as_f64().unwrap(), -3.0);
        // level-aware (the default): the bass sits about 4.4 dB under the kick, so it masks
        // less and a smaller cut clears it
        let (v, c) = run("analyze masking", &[], vec![kick.clone(), bass.clone()]);
        let g = &v["regions"][0];
        assert!(g["margin_db"].as_f64().is_some_and(|m| (m - 4.4).abs() < 0.5), "{v}");
        let cut = g["cut_db"].as_f64().unwrap();
        assert!((-2.0..=-0.5).contains(&cut), "{cut}");
        assert!(c.metrics["overall"].as_f64().unwrap() < 1.0);

        // the same band, never at the same time
        let mut x = sine(22050, 60.0, 2.0, 0.3);
        for (i, s) in x.data[0].iter_mut().enumerate() {
            if (i / 11025) % 2 == 0 {
                *s = 0.0;
            }
        }
        let mut y = sine(22050, 63.0, 2.0, 0.5);
        for (i, s) in y.data[0].iter_mut().enumerate() {
            if (i / 11025) % 2 == 1 {
                *s = 0.0;
            }
        }
        testing::write_wav(&kick, &y);
        testing::write_wav(&apart, &x);
        let (v, c) = run("analyze masking", &[], vec![kick.clone(), apart]);
        assert!(v["co_activity"].as_f64().unwrap() < 0.05);
        assert!(v["regions"].as_array().unwrap().is_empty());
        assert!(v["suggested_fix"].is_null());
        assert!(c.warnings.iter().any(|w| w.code == "low_co_activity"));

        // silence and a different length
        let quiet = d.path("quiet.wav");
        testing::write_wav(&quiet, &AudioBuffer::new(22050, 1, 22050 * 4));
        let (v, c) = run("analyze masking", &[], vec![kick, quiet]);
        assert!(v["regions"].as_array().unwrap().is_empty());
        assert!(c.warnings.iter().any(|w| w.code == "silent_input"));
        assert!(c.warnings.iter().any(|w| w.code == "length_differs"));
        assert!(!c.warnings.iter().any(|w| w.code == "low_co_activity"));
    }

    #[test]
    fn masking_rejects_bad_params_and_plans_a_dry_run() {
        let s = spec("analyze masking");
        let Runner::Custom(f) = s.runner else { unreachable!() };
        for bad in [("threshold", "0%"), ("threshold", "150%"), ("window", "1ms"), ("floor", "0dB"), ("max-cut", "0dB")]
        {
            let a = testing::args(&s, &[bad]);
            let mut c = testing::ctx();
            c.globals.dry_run = true;
            c.inputs = vec!["/nonexistent/a.wav".into(), "/nonexistent/b.wav".into()];
            let e = f(&a, &mut c).unwrap_err();
            assert_eq!(e.kind, ferraus_core::ErrorKind::Usage, "{bad:?}");
        }
        let a = testing::args(&s, &[]);
        let mut c = testing::ctx();
        c.command = s.name.to_string();
        c.globals.dry_run = true;
        c.inputs = vec!["/nonexistent/a.wav".into(), "/nonexistent/b.wav".into()];
        let v = f(&a, &mut c).unwrap();
        assert_eq!(v["analyze"], "analyze masking");
        assert_eq!(v["inputs"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn sections_op_end_to_end() {
        let d = TempDir::new("analysis-sections");
        let (mix, vocal) = (d.path("mix.wav"), d.path("vocal.wav"));
        let (m, v) = crate::sections::tests::synth_song("ABAB", 4, "A");
        testing::write_wav(&mix, &stereo(m));
        testing::write_wav(&vocal, &v);
        let (r, c) = run("analyze sections", &[("min-bars", "2")], vec![mix.clone()]);
        assert_eq!(r["tempo_source"], "detected");
        assert!((r["tempo_bpm"].as_f64().unwrap() - 120.0).abs() < 1.0, "{r}");
        assert_eq!(r["beats_per_bar"], 4);
        assert!(r["integrated_lufs"].as_f64().is_some());
        let secs = r["sections"].as_array().unwrap();
        let labels: Vec<&str> = secs.iter().map(|s| s["label"].as_str().unwrap()).collect();
        assert_eq!(labels, ["A", "B", "A", "B"], "{r}");
        assert_eq!(secs[0]["start_bar"], 1);
        assert_eq!(secs[2]["start_bar"], 9);
        assert_eq!(secs[2]["repeat_of"], 0);
        assert!(secs[0]["repeat_of"].is_null() && secs[0]["similarity_to_repeat"].is_null());
        assert!(secs[0].get("vocal_activity").is_none());
        assert_eq!(secs[1]["role"], "chorus");
        assert!(secs.iter().all(|s| s["delta_lu"].is_number() && s["role_confidence"].is_number()));
        assert_eq!(r["labels"]["B"]["count"], 2);
        assert_eq!(r["labels"]["B"]["role"], "chorus");
        let rep = r["repeats"].as_array().unwrap();
        assert_eq!(rep.len(), 2);
        assert!((rep[0]["offset_s"].as_f64().unwrap() - 16.0).abs() < 0.1, "{r}");
        assert_eq!(r["boundaries_s"].as_array().unwrap().len(), 3);
        assert_eq!(c.metrics["sections"], 4);
        assert_eq!(c.metrics["labels"], 2);
        assert!(c.metrics["confidence"].as_f64().unwrap() > 0.5);
        assert!(c.warnings.is_empty(), "{:?}", c.warnings);

        // a given tempo, a vocal stem, and a forced meter
        let s = spec("analyze sections");
        let Runner::Custom(f) = s.runner else { unreachable!() };
        let args = testing::args(&s, &[("min-bars", "2"), ("vocal", vocal.to_str().unwrap())]);
        let mut c = testing::ctx();
        c.command = s.name.to_string();
        c.globals.bpm = Some(120.0);
        c.inputs = vec![mix.clone()];
        let r = f(&args, &mut c).unwrap();
        assert_eq!(r["tempo_source"], "given");
        assert_eq!(r["tempo_bpm"].as_f64().unwrap(), 120.0);
        let secs = r["sections"].as_array().unwrap();
        assert!(secs[0]["vocal_activity"].as_f64().unwrap() > 0.5, "{r}");
        assert!(secs[1]["vocal_activity"].as_f64().unwrap() < 0.05, "{r}");
        assert_eq!(c.input_records.len(), 2);
        let (r, _) = run("analyze sections", &[("min-bars", "2"), ("beats-per-bar", "2")], vec![mix.clone()]);
        assert_eq!(r["beats_per_bar"], 2);
        assert!((r["bar_s"].as_f64().unwrap() - 1.0).abs() < 0.05, "{r}");

        // too short for two sections
        let short = d.path("short.wav");
        testing::write_wav(&short, &sine(22050, 440.0, 3.0, 0.3));
        let (r, c) = run("analyze sections", &[], vec![short]);
        assert_eq!(r["sections"].as_array().unwrap().len(), 1);
        assert!(c.warnings.iter().any(|w| w.code == "too_short"), "{:?}", c.warnings);
    }

    #[test]
    fn sections_rejects_bad_params_and_plans_a_dry_run() {
        let s = spec("analyze sections");
        let Runner::Custom(f) = s.runner else { unreachable!() };
        for bad in [
            ("min-bars", "0"),
            ("min-bars", "65"),
            ("sensitivity", "150%"),
            ("similarity", "-10%"),
            ("beats-per-bar", "0"),
            ("beats-per-bar", "17"),
        ] {
            let a = testing::args(&s, &[bad]);
            let mut c = testing::ctx();
            c.globals.dry_run = true;
            c.inputs = vec!["/nonexistent/a.wav".into()];
            let e = f(&a, &mut c).unwrap_err();
            assert_eq!(e.kind, ferraus_core::ErrorKind::Usage, "{bad:?}");
        }
        let a = testing::args(&s, &[]);
        let mut c = testing::ctx();
        c.globals.dry_run = true;
        c.globals.bpm = Some(-5.0);
        c.inputs = vec!["/nonexistent/a.wav".into()];
        assert_eq!(f(&a, &mut c).unwrap_err().kind, ferraus_core::ErrorKind::Usage);
        let a = testing::args(&s, &[("vocal", "/nonexistent/v.wav")]);
        let mut c = testing::ctx();
        c.command = s.name.to_string();
        c.globals.dry_run = true;
        c.inputs = vec!["/nonexistent/a.wav".into()];
        let v = f(&a, &mut c).unwrap();
        assert_eq!(v["analyze"], "analyze sections");
        assert_eq!(v["vocal"], "/nonexistent/v.wav");
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
