//! Op specs: `tune`, `pitch`, `stretch`, `analyze pitch`, `analyze key`.

use crate::key::{self, Key, Mode};
use crate::notes::{self, NoteOptions};
use crate::psola;
use crate::pyin::PitchFrame;
use crate::stretch::{self, Quality};
use crate::tracker::{self, TrackOptions};
use crate::tune::{self, Target, TuneOptions};
use crate::util::{median, mono};
use crate::vocoder::{self, PvParams};
use ferraus_core::prelude::*;
use ferraus_core::units::{hz_to_midi, midi_to_name};
use std::path::Path;

const TRACKERS: &[&str] = crate::tracker::TrackerKind::CHOICES;

pub fn ops() -> Vec<OpSpec> {
    vec![
        OpSpec::transform("tune", Category::Pitch, "Pitch correction (auto-tune) to a key, scale or written melody", tune_op)
            .about("Tracks the pitch of a monophonic vocal (pYIN by default), splits it into notes, picks a target for each note (the nearest note of the detected or given key and scale, or the written note of a MIDI melody with --target), and moves each note there with TD-PSOLA so the voice keeps its tone colour. Correction works per note, not per frame: within a note the deviation is smoothed by --retune-speed (0ms is the hard, robotic snap), vibrato is kept out of the correction, and --strength scales the result. Key detection below 0.6 confidence falls back to the chromatic scale and says why.")
            .param(param("key", ParamKind::Str).default("auto").help("Key: auto (detect from the take), or C, F#, Bbm, 'A minor', 'Eb major'"))
            .param(param("scale", ParamKind::Str).default("auto").help("Scale: auto (the key's mode), major, minor, chromatic, a mode (dorian, mixolydian, harmonic-minor, major-pentatonic, minor-pentatonic, blues, ...), or custom notes such as \"C,D,Eb,G,A\""))
            .param(param("strength", ParamKind::Percent).default("100%").range("0%..100%").help("How far each note moves toward its target (50% halves the error)"))
            .param(param("retune-speed", ParamKind::Time).default("25ms").help("Time constant of the correction within a note: 0ms snaps instantly, 100ms or more is gentle"))
            .param(param("humanize", ParamKind::Percent).default("0%").range("0%..100%").help("Bounded random pitch drift (up to ±12 cents at 100%), seeded by --seed"))
            .param(param("vibrato-preserve", ParamKind::Switch).default("on").help("Keep the 4-8 Hz vibrato out of the correction (on/off)"))
            .param(param("formant", ParamKind::Choice).choices(&["preserve", "shift"]).default("preserve").help("preserve keeps the voice's tone colour; shift lets formants move with the pitch"))
            .param(param("range", ParamKind::Str).default("60Hz..1.2kHz").help("Pitch range searched by the tracker, low..high (Hz, kHz or note names)"))
            .param(param("tracker", ParamKind::Choice).choices(TRACKERS).default("pyin").help("Pitch tracker: pyin (built in) or a model tracker (crepe, rmvpe, swiftf0; needs 'ferraus models pull pitch/<name>')"))
            .param(param("notes-out", ParamKind::Path).help("Also write the per-note analysis (detected pitch, target, correction) as JSON to this file"))
            .param(param("target", ParamKind::Path).help("Pull notes to the melody in this MIDI file instead of a scale"))
            .param(param("tolerance", ParamKind::Pitch).default("150cents").help("With --target: notes farther than this from the written note are left alone and reported"))
            .param(param("effect", ParamKind::Choice).choices(&["natural", "hard"]).default("natural").help("hard = robotic effect: --retune-speed 0ms --strength 100% --humanize 0% (and vibrato correction unless --vibrato-preserve is given)"))
            .example("ferraus tune vocal.wav -o tuned.wav", "Detect the key and gently correct every note")
            .example("ferraus tune vocal.wav --key F# --scale minor --strength 80% -o tuned.wav", "Correct to F# minor, moving notes 80% of the way")
            .example("ferraus tune vocal.wav --effect hard --key C -o robot.wav", "The deliberate hard-tune effect in C major")
            .example("ferraus tune vocal.wav --target melody.mid --tolerance 100cents --notes-out notes.json -o tuned.wav", "Pull the take to a written melody and save the per-note report"),
        OpSpec::transform("pitch", Category::Pitch, "Shift pitch, keep duration", pitch_op)
            .about("Shifts pitch by semitones and cents without changing the length. With --formant preserve (the default) the voice keeps its tone colour: the high-quality path is a phase vocoder with identity phase locking and cepstral formant-envelope correction (its lifter follows the detected pitch); --quality fast uses TD-PSOLA for monophonic voiced material and a lighter vocoder otherwise. --formant shift moves formants with the pitch (the 'chipmunk' effect).")
            .param(param("semitones", ParamKind::Pitch).default("0").help("Shift in semitones, e.g. 4, -3, +0.5 (st)"))
            .param(param("cents", ParamKind::Pitch).default("0").help("Additional shift in cents (100 cents = 1 semitone)"))
            .param(param("formant", ParamKind::Choice).choices(&["preserve", "shift"]).default("preserve").help("preserve keeps formants in place; shift moves them with the pitch"))
            .param(param("quality", ParamKind::Choice).choices(&["high", "fast"]).default("high").help("high: 8x-overlap phase vocoder; fast: PSOLA (voice) or 4x vocoder"))
            .example("ferraus pitch vocal.wav --semitones 4 -o up.wav", "Up a major third, formants preserved")
            .example("ferraus pitch guitar.wav --semitones -2 --cents 15 -o down.wav", "Down 185 cents")
            .example("ferraus pitch voice.wav --semitones 7 --formant shift -o chipmunk.wav", "Shift formants with the pitch"),
        OpSpec::transform("stretch", Category::Pitch, "Change duration, keep pitch", stretch_op)
            .about("Time-stretches without changing pitch. auto mode detects transients, keeps every attack intact and exactly on time (onset × factor), and renders tonal stretches with a phase vocoder and percussive ones with WSOLA. --factor 1.0 returns the input bit for bit. Give a factor, a target duration (--to), or a tempo change (--bpm 120 --to-bpm 128; without --bpm the source tempo is estimated).")
            .param(param("factor", ParamKind::Float).default("1.0").range("0.05..20").help("Duration multiplier: 2.0 is twice as long, 0.5 half as long"))
            .param(param("to", ParamKind::Time).help("Target duration instead of a factor, e.g. 3:30 or 95s"))
            .param(param("bpm", ParamKind::Float).help("Source tempo for --to-bpm (also the global --bpm); estimated when omitted"))
            .param(param("to-bpm", ParamKind::Float).help("Target tempo; the factor is bpm / to-bpm"))
            .param(param("mode", ParamKind::Choice).choices(&["auto", "tonal", "percussive", "speech"]).default("auto").help("auto: per-segment vocoder/WSOLA hybrid; tonal: vocoder only; percussive: WSOLA with transient preservation; speech: hybrid with short windows"))
            .param(param("quality", ParamKind::Choice).choices(&["high", "fast"]).default("high").help("high: hybrid as above; fast: WSOLA only (tonal mode: short vocoder)"))
            .example("ferraus stretch loop.wav --factor 1.25 -o slower.wav", "25% longer, same pitch")
            .example("ferraus stretch song.wav --to 3:30 -o radio.wav", "Fit the song into 3:30")
            .example("ferraus stretch beat.wav --bpm 120 --to-bpm 128 --mode percussive -o faster.wav", "Re-tempo a drum loop from 120 to 128 BPM")
            .no_mix(),
        OpSpec::custom("analyze pitch", Category::Inspect, "Pitch curve as JSON or CSV", analyze_pitch)
            .inputs(Inputs::ONE_AUDIO)
            .about("Frame-by-frame fundamental frequency (Hz, 0 when unvoiced), voicing confidence, voiced flag and RMS, plus segmented notes and a summary. Prints JSON in the result by default; with -o file.csv writes CSV, with -o file.json writes the JSON there.")
            .param(param("tracker", ParamKind::Choice).choices(TRACKERS).default("pyin").help("Pitch tracker"))
            .param(param("hop", ParamKind::Time).default("10ms").help("Time between frames"))
            .param(param("range", ParamKind::Str).default("60Hz..1.2kHz").help("Pitch range searched, low..high (Hz, kHz or note names)"))
            .example("ferraus analyze pitch vocal.wav --json", "Pitch curve and notes as JSON")
            .example("ferraus analyze pitch hum.wav --hop 5ms --range 80Hz..800Hz -o curve.csv", "Write a 5 ms pitch curve as CSV")
            .example("ferraus analyze pitch bass.wav --range E1..G3 -o bass_pitch.json", "Track a bass line with a low range and save the JSON"),
        OpSpec::custom("analyze key", Category::Inspect, "Key and scale with confidence", analyze_key)
            .inputs(Inputs::ONE_AUDIO)
            .about("Estimates the key and mode by correlating a chroma profile (from an STFT with tuning estimation) with Krumhansl-Schmuckler major and minor profiles. Reports the confidence (normalised margin over the best key with a different scale), the top-3 alternatives, the tuning offset and the chroma. --method notes uses a duration-weighted histogram of the tracked notes instead, which suits monophonic vocals.")
            .param(param("method", ParamKind::Choice).choices(&["chroma", "notes"]).default("chroma").help("chroma: STFT chroma (any material); notes: pitch-tracked notes (monophonic)"))
            .example("ferraus analyze key song.wav --json", "Key, mode, confidence and alternatives")
            .example("ferraus analyze key vocal.wav --method notes", "Key of a solo vocal from its notes")
            .example("ferraus analyze key loop.wav --method chroma -o key.json", "Key of a loop from its chroma, saved as JSON"),
    ]
}

// ---------------------------------------------------------------------------------------
// shared helpers
// ---------------------------------------------------------------------------------------

fn track_opts(a: &Args, sr: u32, hop_s: f64) -> Result<TrackOptions> {
    let (lo, hi) = a.freq_range("range")?;
    if !(lo >= 20.0 && hi > lo * 1.5) {
        return Err(Error::usage(format!("--range {lo:.0}Hz..{hi:.0}Hz: need low ≥ 20 Hz and high above 1.5 × low")));
    }
    let hi = hi.min(sr as f64 * 0.45);
    if hi <= lo {
        return Err(Error::usage("--range is above the Nyquist frequency of the input"));
    }
    Ok(TrackOptions { fmin: lo, fmax: hi, hop_s })
}

fn run_tracker(a: &Args, x: &[f32], sr: u32, hop_s: f64, ctx: &mut OpCtx) -> Result<Vec<PitchFrame>> {
    let t = tracker::by_name_for(ctx, &a.choice("tracker"))?;
    if let Some((name, version, sha)) = t.model() {
        ctx.model_used(&name, &version, &sha);
    }
    let opts = track_opts(a, sr, hop_s)?;
    t.track(x, sr, &opts)
}

/// Fraction of non-silent frames that are voiced, mean voiced confidence, median f0.
fn voicing_stats(frames: &[PitchFrame]) -> (f64, f64, Option<f64>) {
    let loud: Vec<&PitchFrame> = frames.iter().filter(|f| f.rms > 1e-3).collect();
    let voiced: Vec<&PitchFrame> = loud.iter().copied().filter(|f| f.voiced).collect();
    let frac = if loud.is_empty() { 0.0 } else { voiced.len() as f64 / loud.len() as f64 };
    let conf =
        if voiced.is_empty() { 0.0 } else { voiced.iter().map(|f| f.confidence).sum::<f64>() / voiced.len() as f64 };
    let f0: Vec<f64> = voiced.iter().map(|f| f.freq_hz).collect();
    (frac, conf, median(&f0))
}

fn lifter_for(sr: u32, f0: Option<f64>) -> usize {
    match f0 {
        Some(f) if f > 0.0 => ((0.6 * sr as f64 / f) as usize).clamp(8, sr as usize / 150),
        _ => (sr as usize / 700).max(8),
    }
}

fn r4(v: f64) -> f64 {
    (v * 1e4).round() / 1e4
}

// ---------------------------------------------------------------------------------------
// tune
// ---------------------------------------------------------------------------------------

fn resolve_scale(a: &Args, notes: &[notes::Note], ctx: &mut OpCtx) -> Result<Target> {
    let scale = a.str_or("scale", "auto").trim().to_string();
    let scale_l = scale.to_lowercase();
    let key_s = a.str_or("key", "auto").trim().to_string();
    let chromatic = || Target::Scale((0..12).collect());
    if scale_l == "chromatic" {
        ctx.note("scale", "chromatic");
        return Ok(chromatic());
    }
    if scale_l != "auto" && !key::is_scale_name(&scale_l) {
        // custom note list
        let pcs = key::scale_pitch_classes(&scale, 0)?;
        ctx.note("scale", format!("custom ({})", key::pitch_class_names(&pcs)));
        return Ok(Target::Scale(pcs));
    }
    let want_mode = match scale_l.as_str() {
        "major" => Some(Mode::Major),
        "minor" => Some(Mode::Minor),
        _ => None,
    };
    let (tonic, mode) = if key_s.eq_ignore_ascii_case("auto") {
        let chroma = key::chroma_from_notes(notes);
        let est = key::detect_from_chroma(&chroma, want_mode);
        if est.confidence < 0.6 {
            let why = format!(
                "key detection confidence {:.2} is below 0.6 (best guess {}); correcting to the chromatic scale. Pass --key to choose the key",
                est.confidence, est.key
            );
            ctx.warn("low_key_confidence", why.clone());
            ctx.note("key", format!("{} (detected, confidence {:.2}; not used)", est.key, est.confidence));
            ctx.note("scale", "chromatic");
            ctx.metric("key_confidence", est.confidence);
            ctx.metric("key_fallback_reason", why);
            return Ok(chromatic());
        }
        ctx.note("key", format!("{} (detected, confidence {:.2})", est.key, est.confidence));
        ctx.metric("key_confidence", est.confidence);
        (est.best.tonic, est.best.mode)
    } else {
        let (t, m) = Key::parse(&key_s)?;
        let mode = want_mode.or(m).unwrap_or(Mode::Major);
        ctx.note("key", Key { tonic: t, mode }.name());
        (t, mode)
    };
    let scale_name = if scale_l == "auto" || want_mode.is_some() { mode.as_str().to_string() } else { scale_l.clone() };
    let pcs = key::scale_pitch_classes(&scale_name, tonic)?;
    ctx.note("scale", format!("{} {}", Key { tonic, mode }.tonic_name(), scale_name));
    Ok(Target::Scale(pcs))
}

fn tune_op(a: &Args, buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let hard = a.choice("effect") == "hard";
    let strength = if hard { 1.0 } else { a.percent("strength")? };
    if !(0.0..=1.0).contains(&strength) {
        return Err(Error::usage("--strength must be between 0% and 100%"));
    }
    let retune = if hard { 0.0 } else { a.seconds("retune-speed")? };
    let humanize = if hard { 0.0 } else { a.percent("humanize")?.clamp(0.0, 1.0) };
    let vib = if hard && !a.is_explicit("vibrato-preserve") { false } else { a.switch("vibrato-preserve", true) };
    let sr = buf.sample_rate;
    if buf.frames() == 0 {
        return Ok(buf);
    }
    let x = mono(&buf.data);
    let frames = run_tracker(a, &x, sr, 0.010, ctx)?;
    ctx.check_cancel()?;
    ctx.set_progress(0.3);
    let notes = notes::segment(&frames, &NoteOptions::default());
    ctx.metric("notes", notes.len());
    if notes.is_empty() {
        ctx.warn("no_voiced_notes", "no voiced notes were found; the audio is returned unchanged");
        return Ok(buf);
    }
    let target = if let Some(path) = a.path("target") {
        ctx.check_allowed(&path)?;
        let bytes = std::fs::read(&path).map_err(|e| {
            Error::new(ErrorKind::InputMissing, "input_not_found", format!("cannot read {}: {e}", path.display()))
        })?;
        ctx.note_input(&path);
        let mel = tune::parse_midi_melody(&bytes)?;
        if mel.is_empty() {
            return Err(Error::input(format!("{} has no notes", path.display())));
        }
        ctx.note("target", path.display().to_string());
        Target::Melody { notes: mel, tolerance_cents: a.cents("tolerance")?.abs() }
    } else {
        resolve_scale(a, &notes, ctx)?
    };
    let plans = tune::plan_targets(&notes, &target);
    let opts = TuneOptions { strength, retune_s: retune.max(0.0), humanize, vibrato_preserve: vib, seed: ctx.seed() };
    let curve = tune::correction_curve(&frames, &notes, &plans, &opts);
    let ratios: Vec<f64> = curve.iter().map(|c| 2f64.powf(c / 1200.0)).collect();

    // report
    let corrected: Vec<&tune::NotePlan> = plans.iter().filter(|p| p.target_midi.is_some()).collect();
    let devs: Vec<f64> = corrected.iter().filter_map(|p| p.deviation_cents).map(f64::abs).collect();
    ctx.metric("corrected_notes", corrected.len());
    if !devs.is_empty() {
        ctx.metric("mean_abs_deviation_cents", r4(devs.iter().sum::<f64>() / devs.len() as f64));
        ctx.metric("max_abs_deviation_cents", r4(devs.iter().cloned().fold(0.0, f64::max)));
    }
    let left: Vec<Value> = plans
        .iter()
        .filter(|p| p.target_midi.is_none())
        .map(|p| json!({"start_s": p.start_s, "end_s": p.end_s, "note": p.note, "midi": p.midi, "reason": p.reason}))
        .collect();
    if matches!(target, Target::Melody { .. }) {
        if !left.is_empty() {
            ctx.warn(
                "notes_left_alone",
                format!(
                    "{} note(s) were farther than the tolerance from the written melody and were left alone",
                    left.len()
                ),
            );
        }
        ctx.metric("left_alone", Value::Array(left));
    }
    if let Some(path) = a.path("notes-out") {
        let doc = json!({
            "key": ctx.notes.get("key").cloned().unwrap_or(Value::Null),
            "scale": ctx.notes.get("scale").cloned().unwrap_or(Value::Null),
            "strength": strength,
            "retune_speed_s": retune,
            "notes": plans,
        });
        let bytes = serde_json::to_vec_pretty(&doc)?;
        ctx.write_file(&path, &bytes, Some("notes"))?;
    }
    ctx.set_progress(0.5);

    // stage 3: resynthesis
    let (vfrac, vconf, f0) = voicing_stats(&frames);
    let preserve = a.choice("formant") != "shift";
    let use_psola = preserve && vfrac >= 0.5 && vconf >= 0.5;
    let data = if use_psola {
        ctx.metric("resynthesis", "td_psola");
        psola::shift_with_track(&buf.data, &x, sr, &frames, &ratios)
    } else {
        ctx.metric("resynthesis", "phase_vocoder");
        let p = PvParams { formant_preserve: preserve, lifter: lifter_for(sr, f0), ..PvParams::high(sr) };
        let hop = crate::pyin::hop_samples(sr, 0.010);
        buf.data.iter().map(|c| vocoder::pitch_shift_curve(c, &ratios, hop, &p)).collect()
    };
    Ok(AudioBuffer::from_planar(sr, data))
}

// ---------------------------------------------------------------------------------------
// pitch
// ---------------------------------------------------------------------------------------

fn pitch_op(a: &Args, buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let st = a.semitones("semitones")? + a.cents("cents")? / 100.0;
    if st.abs() < 1e-9 || buf.frames() == 0 {
        return Ok(buf);
    }
    if st.abs() > 24.0 {
        return Err(Error::usage("pitch shifts are limited to ±24 semitones"));
    }
    let ratio = 2f64.powf(st / 12.0);
    let sr = buf.sample_rate;
    let preserve = a.choice("formant") != "shift";
    let fast = a.choice("quality") == "fast";
    let x = mono(&buf.data);
    let frames = crate::pyin::track(&x, sr, &crate::pyin::PyinOptions::default());
    let (vfrac, vconf, f0) = voicing_stats(&frames);
    let voice = vfrac >= 0.5 && vconf >= 0.5;
    ctx.metric("ratio", r4(ratio));
    let data = if fast && preserve && voice && (0.6..=1.7).contains(&ratio) {
        ctx.metric("method", "td_psola");
        psola::shift_with_track(&buf.data, &x, sr, &frames, &vec![ratio; frames.len()])
    } else {
        let base = if fast { PvParams::fast(sr) } else { PvParams::high(sr) };
        let p = PvParams { formant_preserve: preserve, lifter: lifter_for(sr, if voice { f0 } else { None }), ..base };
        ctx.metric("method", if preserve { "phase_vocoder_formant" } else { "phase_vocoder" });
        buf.data.iter().map(|c| vocoder::pitch_shift(c, ratio, &p)).collect()
    };
    Ok(AudioBuffer::from_planar(sr, data))
}

// ---------------------------------------------------------------------------------------
// stretch
// ---------------------------------------------------------------------------------------

fn stretch_factor(a: &Args, buf: &AudioBuffer, ctx: &mut OpCtx) -> Result<f64> {
    let has_to = a.raw("to").is_some();
    let has_to_bpm = a.raw("to-bpm").is_some();
    let has_factor = a.is_explicit("factor");
    if [has_to, has_to_bpm, has_factor].iter().filter(|v| **v).count() > 1 {
        return Err(Error::usage("give only one of --factor, --to, or --to-bpm"));
    }
    if has_to {
        let target = a.seconds("to")?;
        let dur = buf.duration_s();
        if dur <= 0.0 {
            return Err(Error::input("the input is empty"));
        }
        if target <= 0.0 {
            return Err(Error::usage("--to must be a positive duration"));
        }
        return Ok(target / dur);
    }
    if has_to_bpm {
        let to = a.f64("to-bpm")?;
        if to.is_nan() || to <= 0.0 {
            return Err(Error::usage("--to-bpm must be positive"));
        }
        let from = match a.opt_f64("bpm")?.or(ctx.bpm()) {
            Some(b) if b > 0.0 => b,
            Some(_) => return Err(Error::usage("--bpm must be positive")),
            None => {
                let x = mono(&buf.data);
                let (bpm, conf) = stretch::estimate_tempo(&x, buf.sample_rate).ok_or_else(|| {
                    Error::usage("could not estimate the source tempo; pass --bpm")
                        .with_hint("ferraus analyze tempo <input>")
                })?;
                let bpm = (bpm * 10.0).round() / 10.0;
                ctx.warn(
                    "bpm_estimated",
                    format!("source tempo estimated as {bpm} BPM (confidence {conf:.2}); pass --bpm to set it"),
                );
                ctx.note("bpm", bpm);
                bpm
            }
        };
        return Ok(from / to);
    }
    a.f64("factor")
}

fn stretch_op(a: &Args, buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let factor = stretch_factor(a, &buf, ctx)?;
    if !(0.05..=20.0).contains(&factor) || !factor.is_finite() {
        return Err(Error::usage(format!("stretch factor {factor:.3} is outside 0.05..20")));
    }
    ctx.metric("factor", r4(factor));
    if factor == 1.0 {
        return Ok(buf);
    }
    let mode = stretch::Mode::parse(&a.choice("mode"));
    let quality = if a.choice("quality") == "fast" { Quality::Fast } else { Quality::High };
    let (data, rep) = stretch::stretch(&buf.data, buf.sample_rate, factor, mode, quality);
    ctx.metric("engine", rep.engine);
    ctx.metric("transients", rep.transients);
    ctx.metric("anchored_transients", rep.anchored);
    Ok(AudioBuffer::from_planar(buf.sample_rate, data))
}

// ---------------------------------------------------------------------------------------
// analyze pitch / analyze key
// ---------------------------------------------------------------------------------------

fn analyze_pitch(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let hop = a.seconds("hop")?;
    if !(0.001..=0.1).contains(&hop) {
        return Err(Error::usage("--hop must be between 1ms and 100ms"));
    }
    let out = if ctx.globals.output.is_some() { Some(ctx.output_path(None, "json")?) } else { None };
    if ctx.globals.dry_run {
        if let Some(p) = &out {
            ctx.prepare_output(p)?;
        }
        return Ok(
            json!({"plan": {"tracker": a.choice("tracker"), "hop_s": hop, "output": out.map(|p| p.display().to_string())}}),
        );
    }
    let buf = ctx.read_audio(0)?;
    let x = mono(&buf.data);
    let frames = run_tracker(a, &x, buf.sample_rate, hop, ctx)?;
    let notes = notes::segment(&frames, &NoteOptions::default());
    let (vfrac, vconf, f0) = voicing_stats(&frames);
    let voiced: Vec<f64> = frames.iter().filter(|f| f.voiced).map(|f| f.freq_hz).collect();
    let summary = json!({
        "frames": frames.len(),
        "hop_s": hop,
        "tracker": a.choice("tracker"),
        "voiced_fraction": r4(vfrac),
        "mean_confidence": r4(vconf),
        "median_hz": f0.map(r4),
        "median_note": f0.map(|f| midi_to_name(hz_to_midi(f).round() as i32)),
        "min_hz": voiced.iter().cloned().reduce(f64::min).map(r4),
        "max_hz": voiced.iter().cloned().reduce(f64::max).map(r4),
    });
    let notes_v: Vec<Value> = notes
        .iter()
        .map(|n| json!({"start_s": r4(n.start_s), "end_s": r4(n.end_s), "midi": r4(n.midi), "note": midi_to_name(n.midi.round() as i32), "hz": r4(n.hz()), "confidence": r4(n.confidence), "peak_rms": r4(n.peak_rms)}))
        .collect();
    let frames_v: Vec<Value> = frames
        .iter()
        .map(|f| json!({"t": r4(f.time_s), "hz": r4(f.freq_hz), "confidence": r4(f.confidence), "voiced": f.voiced, "rms": (f.rms * 1e6).round() / 1e6}))
        .collect();
    ctx.metric("voiced_fraction", r4(vfrac));
    ctx.metric("notes", notes.len());
    match out {
        Some(path) => {
            let is_csv = path.extension().map(|e| e.eq_ignore_ascii_case("csv")).unwrap_or(false);
            let bytes = if is_csv {
                let mut s = String::from("time_s,freq_hz,midi,confidence,voiced,rms\n");
                for f in &frames {
                    let midi = f.midi().map(|m| format!("{:.3}", m)).unwrap_or_default();
                    s.push_str(&format!(
                        "{:.4},{:.3},{},{:.4},{},{:.6}\n",
                        f.time_s, f.freq_hz, midi, f.confidence, f.voiced as u8, f.rms
                    ));
                }
                s.into_bytes()
            } else {
                serde_json::to_vec_pretty(&json!({"summary": summary, "notes": notes_v, "frames": frames_v}))?
            };
            write_out(ctx, &path, &bytes)?;
            Ok(json!({"summary": summary, "notes": notes_v, "format": if is_csv { "csv" } else { "json" }}))
        }
        None => Ok(json!({"summary": summary, "notes": notes_v, "frames": frames_v})),
    }
}

fn write_out(ctx: &mut OpCtx, path: &Path, bytes: &[u8]) -> Result<()> {
    ctx.write_file(path, bytes, None)
}

fn analyze_key(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let out = if ctx.globals.output.is_some() { Some(ctx.output_path(None, "json")?) } else { None };
    if ctx.globals.dry_run {
        if let Some(p) = &out {
            ctx.prepare_output(p)?;
        }
        return Ok(json!({"plan": {"method": a.choice("method")}}));
    }
    let buf = ctx.read_audio(0)?;
    let x = mono(&buf.data);
    let method = a.choice("method");
    let est = if method == "notes" {
        let frames = crate::pyin::track(&x, buf.sample_rate, &crate::pyin::PyinOptions::default());
        let notes = notes::segment(&frames, &NoteOptions::default());
        key::detect_from_chroma(&key::chroma_from_notes(&notes), None)
    } else {
        key::detect(&x, buf.sample_rate)
    };
    let mut v = serde_json::to_value(&est)?;
    if let Value::Object(m) = &mut v {
        m.insert("method".into(), json!(method));
        m.insert("reliable".into(), json!(est.confidence >= 0.6));
    }
    if est.confidence < 0.6 {
        ctx.warn(
            "low_key_confidence",
            format!("key confidence {:.2} is below 0.6; '{}' is only a best guess", est.confidence, est.key),
        );
    }
    ctx.metric("confidence", est.confidence);
    if let Some(path) = out {
        let bytes = serde_json::to_vec_pretty(&v)?;
        write_out(ctx, &path, &bytes)?;
    }
    Ok(v)
}
