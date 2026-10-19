//! `midi ...` commands.

use crate::edit::{self, Groove, QuantizeOptions, QuantizeWhat, SplitBy, VelocityOptions};
use crate::gm;
use crate::model::MidiFile;
use crate::smf::Timing;
use crate::tempo::us_to_bpm;
use ferraus_core::op::InputKind;
use ferraus_core::prelude::*;
use ferraus_core::units::{Grid, Time};
use std::path::{Path, PathBuf};

const MANY_MIDI: Inputs = Inputs {
    min: 2,
    max: None,
    kind: InputKind::Midi,
};

pub fn ops() -> Vec<OpSpec> {
    let tracks = || {
        param("tracks", ParamKind::List)
            .help("1-based track numbers to change, comma separated (default: every track)")
            .advanced()
    };
    vec![
        OpSpec::custom("midi info", Category::Midi, "Summarize a MIDI file", midi_info)
            .about("Prints tracks, note counts, pitch ranges, programs, tempo map, time and key signatures, and the length in seconds, beats, and bars. Writes nothing.")
            .inputs(Inputs::ONE_MIDI)
            .example("ferraus midi info song.mid --json", "Machine-readable summary of every track")
            .example("ferraus midi info part.json", "Also reads the JSON form written by midi to-json"),
        OpSpec::custom("midi to-json", Category::Midi, "Convert MIDI to editable JSON", midi_to_json)
            .about("Writes the agent-facing JSON form of a MIDI file: tracks, notes with onset and duration in ticks, beats, and seconds, pitch number and name, velocity, channel (1-16), tempo map, time and key signatures, controllers, program changes, pitch bends, sysex, and every other event, so 'midi from-json' reproduces the file's events exactly. When editing, change 'beat'/'duration_beats' (they win over ticks) or remove both tick and beat to position by 'time_s'.")
            .inputs(Inputs::ONE_MIDI)
            .param(param("compact", ParamKind::Flag).help("Write minified JSON instead of indented"))
            .example("ferraus midi to-json song.mid -o song.json", "Write the JSON model beside the file")
            .example("ferraus midi to-json song.mid -o -", "Print the JSON model to stdout"),
        OpSpec::custom("midi from-json", Category::Midi, "Write a MIDI file from JSON", midi_from_json)
            .about("Builds a Standard MIDI File from the JSON model. Minimal JSON works: {\"tempo_map\":[{\"beat\":0,\"bpm\":100}],\"tracks\":[{\"notes\":[{\"beat\":0,\"duration_beats\":1,\"name\":\"C4\",\"velocity\":100}]}]}. Positions: beat wins over tick; time_s is used when neither is present.")
            .inputs(Inputs::exactly(1, InputKind::Any))
            .param(param("ppq", ParamKind::Int).range("24..32767").help("Ticks per quarter note of the written file; ticks are rescaled (default: the JSON's ppq, else 960)"))
            .example("ferraus midi from-json song.json -o song.mid", "Round-trip an edited JSON model back to MIDI")
            .example("ferraus midi from-json idea.json -o idea.mid --ppq 960", "Write at 960 ticks per quarter note"),
        OpSpec::custom("midi quantize", Category::Midi, "Snap notes to a grid", midi_quantize)
            .about("Maps notes to ticks against the file's tempo map and moves each note-on by strength × (nearest grid point − onset). The grid is ppq × 4 / subdivision ticks (1/8T triplets and 1/8D dotted supported); --swing delays every second grid point by swing × grid; --groove replaces the fixed grid with per-position offsets and velocity deltas from 'midi groove extract'. --humanize adds seeded Gaussian jitter afterwards (--seed).")
            .inputs(Inputs::ONE_MIDI)
            .param(param("grid", ParamKind::Grid).default("1/16").help("Grid step: 1/16, 1/8T (triplet), 1/8D (dotted), or auto to detect the dominant subdivision"))
            .param(param("strength", ParamKind::Percent).default("100%").range("0..100%").help("How far notes move toward the grid: 100% lands exactly on it, 50% halves the error"))
            .param(param("swing", ParamKind::Percent).default("0%").range("0..100%").help("Delay of every second grid point as a percentage of one grid step (60% = 0.6 × 1/16)"))
            .param(param("what", ParamKind::Choice).choices(&["start", "end", "both", "duration"]).default("start").help("start: snap note-ons, note-offs follow (durations kept); end: snap note-offs only; both: snap each independently; duration: snap note-ons and round lengths to whole grid steps"))
            .param(param("humanize", ParamKind::Time).default("0ms").help("Standard deviation of Gaussian timing jitter added after quantizing (15ms); reproducible with --seed"))
            .param(param("min-length", ParamKind::Grid).default("1/64").help("Notes shorter than this note value after quantizing are lengthened to it"))
            .param(param("no-overlap", ParamKind::Switch).default("on").help("Shorten notes that overlap the next note of the same pitch and channel; drop exact duplicates"))
            .param(param("groove", ParamKind::Path).help("Groove file from 'midi groove extract'; replaces --grid and --swing"))
            .param(tracks())
            .example("ferraus midi quantize take.mid --grid 1/16 -o tight.mid", "Snap every note-on to the nearest sixteenth")
            .example("ferraus midi quantize take.mid --grid 1/16 --strength 80% --swing 60% -o swung.mid", "Pull notes 80% of the way to a swung sixteenth grid")
            .example("ferraus midi quantize part.mid --groove groove.json -o grooved.mid", "Give a stiff part the feel captured from a drummer"),
        OpSpec::custom("midi groove extract", Category::Midi, "Capture a groove from a reference part", midi_groove_extract)
            .about("Measures, for every grid position in a bar, the average timing offset (as a fraction of the grid step) and velocity deviation of the reference notes, and writes them as groove JSON for 'midi quantize --groove'.")
            .inputs(Inputs::ONE_MIDI)
            .param(param("grid", ParamKind::Grid).default("1/16").help("Grid the offsets are measured against (1/16, 1/8, 1/8T)"))
            .example("ferraus midi groove extract drums.mid -o groove.json", "Capture a drummer's sixteenth-note feel")
            .example("ferraus midi groove extract ref.mid -o groove.json --grid 1/8", "Measure the feel on an eighth-note grid"),
        OpSpec::custom("midi transpose", Category::Midi, "Shift note pitches", midi_transpose)
            .about("Moves notes by whole semitones. Channel 10 (drums) is left alone unless --include-drums; notes pushed outside 0-127 are dropped with a warning.")
            .inputs(Inputs::ONE_MIDI)
            .param(param("semitones", ParamKind::Pitch).required().help("Semitones to shift (+2, -12st); fractions round to the nearest semitone"))
            .param(param("include-drums", ParamKind::Flag).help("Also transpose channel 10"))
            .param(tracks())
            .example("ferraus midi transpose song.mid --semitones +2 -o up2.mid", "Up a whole step")
            .example("ferraus midi transpose bass.mid --semitones -12st -o low.mid", "Down an octave"),
        OpSpec::custom("midi velocity", Category::Midi, "Scale, compress, or set velocities", midi_velocity)
            .about("Changes note velocities in this order: --set, --scale, --compress toward the mean, then --range remaps the current lowest..highest onto lo..hi. Results are clamped to 1-127.")
            .inputs(Inputs::ONE_MIDI)
            .param(param("scale", ParamKind::Float).default("1.0").help("Multiply velocities (1.2 = 20% harder)"))
            .param(param("compress", ParamKind::Percent).default("0%").range("0..100%").help("Pull velocities toward their average (100% = all equal)"))
            .param(param("set", ParamKind::Int).range("1..127").help("Set every velocity to this value first"))
            .param(param("range", ParamKind::Str).help("Remap velocities onto lo..hi, e.g. 40..110"))
            .param(tracks())
            .example("ferraus midi velocity take.mid --scale 1.2 -o louder.mid", "Play 20% harder")
            .example("ferraus midi velocity take.mid --compress 50% --range 60..110 -o even.mid", "Even out dynamics into 60-110"),
        OpSpec::custom("midi legato", Category::Midi, "Extend notes to the next onset", midi_legato)
            .about("Lengthens every note to the next note-on in its track and channel (chords move together), leaving --gap of space. The last notes keep their length.")
            .inputs(Inputs::ONE_MIDI)
            .param(param("gap", ParamKind::Time).default("0ms").help("Space left before the next note (10ms, or 0.05b in beats)"))
            .param(tracks())
            .example("ferraus midi legato lead.mid -o smooth.mid", "Connect every note to the next")
            .example("ferraus midi legato pad.mid --gap 20ms -o pad-legato.mid", "Nearly legato with a 20 ms gap"),
        OpSpec::custom("midi tempo", Category::Midi, "Set or scale the tempo", midi_tempo)
            .about("--set replaces the tempo map with one tempo; --scale multiplies every tempo. Notes keep their beat positions, so the part plays faster or slower.")
            .inputs(Inputs::ONE_MIDI)
            .param(param("set", ParamKind::Float).range("1..1000").help("New constant tempo in BPM (120)"))
            .param(param("scale", ParamKind::Float).help("Multiply every tempo (1.5 = 50% faster)"))
            .example("ferraus midi tempo song.mid --set 120 -o at120.mid", "Play the whole file at 120 BPM")
            .example("ferraus midi tempo song.mid --scale 0.5 -o half.mid", "Half speed, keeping tempo changes"),
        OpSpec::custom("midi split-tracks", Category::Midi, "One file per track or channel", midi_split)
            .about("Writes one MIDI file per note-bearing track (roles track1..N) or per channel (ch1..16). Tempo, time and key signatures are copied into every part.")
            .inputs(Inputs::ONE_MIDI)
            .param(param("by", ParamKind::Choice).choices(&["track", "channel"]).default("track").help("Split by track or by MIDI channel"))
            .example("ferraus midi split-tracks song.mid --outdir parts", "parts/song.track1.mid, parts/song.track2.mid, ...")
            .example("ferraus midi split-tracks song.mid --by channel -o song.mid --outdir parts", "One file per channel"),
        OpSpec::custom("midi merge", Category::Midi, "Combine MIDI files", midi_merge)
            .about("Merges several files into one format-1 file at the first file's PPQ, keeping each track (or everything in one track with --mode single). The first file's tempo map is used.")
            .inputs(MANY_MIDI)
            .param(param("mode", ParamKind::Choice).choices(&["tracks", "single"]).default("tracks").help("tracks: keep every track; single: merge all events into one track"))
            .example("ferraus midi merge drums.mid bass.mid keys.mid -o song.mid", "Stack parts into one multitrack file")
            .example("ferraus midi merge a.mid b.mid --mode single -o one-track.mid", "Merge into a single track"),
        OpSpec::custom("midi trim", Category::Midi, "Keep a time range", midi_trim)
            .about("Keeps events inside --range (beats like 4b..12b are musical; seconds follow the tempo map). Notes crossing the edges are clipped; the tempo, signatures, programs, and controller values in effect at the start are kept.")
            .inputs(Inputs::ONE_MIDI)
            .param(param("range", ParamKind::Range).required().help("start..end in beats (8b..24b), seconds (10s..30s), or mm:ss; end = file end"))
            .param(param("shift", ParamKind::Switch).default("on").help("Move the range start to time zero"))
            .example("ferraus midi trim song.mid --range 16b..48b -o chorus.mid", "Keep beats 16 to 48 (bars 5-12 in 4/4)")
            .example("ferraus midi trim song.mid --range 0:30..end -o tail.mid", "Drop the first 30 seconds"),
    ]
}

// -------------------------------------------------------------------------------------
// shared helpers
// -------------------------------------------------------------------------------------

/// Read input `i` as a MIDI file or JSON model (stdin with `-`).
pub fn load_input(ctx: &mut OpCtx, i: usize) -> Result<MidiFile> {
    let path = ctx.input(i)?.to_path_buf();
    load_path(ctx, &path)
}

pub fn load_path(ctx: &mut OpCtx, path: &Path) -> Result<MidiFile> {
    if path.as_os_str() == "-" {
        let bytes = ctx
            .stdin
            .clone()
            .ok_or_else(|| Error::input("stdin is empty"))?;
        ctx.note_input(path);
        return MidiFile::from_bytes(&bytes);
    }
    ctx.check_allowed(path)?;
    let f = MidiFile::read(path)?;
    ctx.note_input(path);
    Ok(f)
}

fn wants_json(ctx: &OpCtx, path: &Path) -> bool {
    ctx.globals.format.as_deref() == Some("json")
        || path
            .extension()
            .map(|e| e.eq_ignore_ascii_case("json"))
            .unwrap_or(false)
}

/// Write a MIDI file (or the JSON model when the path ends in `.json`). Returns the path.
pub fn write_midi(ctx: &mut OpCtx, f: &MidiFile, role: Option<&str>) -> Result<PathBuf> {
    let ext = if ctx.globals.format.as_deref() == Some("json") {
        "json"
    } else {
        "mid"
    };
    let path = ctx.output_path(role, ext)?;
    let bytes = if wants_json(ctx, &path) {
        f.to_json_string().into_bytes()
    } else {
        f.to_smf_bytes()
    };
    ctx.write_file(&path, &bytes, role)?;
    Ok(path)
}

fn track_filter(a: &Args, f: &MidiFile) -> Result<Option<Vec<usize>>> {
    let l = a.list("tracks");
    if l.is_empty() {
        return Ok(None);
    }
    let mut v = Vec::new();
    for s in l {
        let n: usize = s
            .trim()
            .parse()
            .map_err(|_| Error::usage(format!("--tracks: '{s}' is not a track number")))?;
        if n == 0 || n > f.tracks.len() {
            return Err(Error::usage(format!(
                "--tracks: track {n} does not exist (the file has {} tracks)",
                f.tracks.len()
            )));
        }
        v.push(n - 1);
    }
    Ok(Some(v))
}

/// A compact summary used in results.
pub fn summary(f: &MidiFile) -> Value {
    json!({
        "tracks": f.tracks.len(),
        "notes": f.note_count(),
        "duration_s": round6(f.duration_seconds()),
        "ppq": f.ppq(),
    })
}

fn round6(x: f64) -> f64 {
    (x * 1e6).round() / 1e6
}

fn out_result(path: &Path, f: &MidiFile, extra: Value) -> Value {
    let mut v = summary(f);
    v["output"] = json!(path.display().to_string());
    if let Value::Object(m) = extra {
        for (k, x) in m {
            v[k] = x;
        }
    }
    v
}

/// Convert a time value to ticks at a position (beats are musical; seconds follow the tempo map).
fn time_to_ticks(f: &MidiFile, t: &Time, at_tick: u64) -> Result<u64> {
    let map = f.tempo_map();
    Ok(match t {
        Time::Start => 0,
        Time::Beats { beats, .. } => (beats * f.ticks_per_beat()).round().max(0.0) as u64,
        Time::Seconds(s) => map.seconds_to_ticks_at(at_tick as f64, *s).round().max(0.0) as u64,
        Time::Samples(_) => return Err(Error::usage("MIDI times take beats (4b) or seconds (2s), not samples")),
        Time::End | Time::FromEnd(_) => return Err(Error::usage("'end' is not a duration")),
    })
}

/// Resolve a time point in a file to an absolute tick.
fn point_to_tick(f: &MidiFile, t: &Time) -> Result<u64> {
    let total = f.duration_ticks();
    let map = f.tempo_map();
    Ok(match t {
        Time::Start => 0,
        Time::End => total,
        Time::Beats { beats, .. } => (beats * f.ticks_per_beat()).round().max(0.0) as u64,
        Time::Seconds(s) => map.seconds_to_tick(*s).round().max(0.0) as u64,
        Time::FromEnd(inner) => {
            let back = match inner.as_ref() {
                Time::Beats { beats, .. } => beats * f.ticks_per_beat(),
                Time::Seconds(s) => {
                    let end_s = map.tick_to_seconds(total as f64);
                    total as f64 - map.seconds_to_tick((end_s - s).max(0.0))
                }
                _ => return Err(Error::usage("unsupported time from the end")),
            };
            (total as f64 - back).round().max(0.0) as u64
        }
        Time::Samples(_) => return Err(Error::usage("MIDI times take beats (4b) or seconds (2s), not samples")),
    }
    .min(total.max(1)))
}

// -------------------------------------------------------------------------------------
// ops
// -------------------------------------------------------------------------------------

fn midi_info(_a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let f = load_input(ctx, 0)?;
    let map = f.tempo_map();
    let dur_ticks = f.duration_ticks();
    let (num, den) = f.time_signature_at(0);
    let bar_ticks = f.ticks_per_beat() * num as f64 * 4.0 / den.max(1) as f64;
    let tracks: Vec<Value> = f
        .tracks
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let lo = t.notes.iter().map(|n| n.pitch).min();
            let hi = t.notes.iter().map(|n| n.pitch).max();
            let programs: Vec<Value> = t
                .programs
                .iter()
                .map(|p| {
                    json!({
                        "channel": p.data.channel + 1,
                        "program": p.data.program,
                        "name": if p.data.channel == 9 { "Drum kit" } else { gm::program_name(p.data.program) },
                    })
                })
                .collect();
            json!({
                "index": i + 1,
                "name": t.name(),
                "notes": t.notes.len(),
                "channels": t.channels().iter().map(|c| c + 1).collect::<Vec<_>>(),
                "pitch_range": lo.zip(hi).map(|(l, h)| json!([units::midi_to_name(l as i32), units::midi_to_name(h as i32)])),
                "programs": programs,
                "controllers": t.controllers.len(),
                "pitch_bends": t.pitch_bends.len(),
                "sysex": t.sysex.len(),
                "first_note_s": t.notes.iter().map(|n| n.tick).min().map(|x| round6(map.tick_to_seconds(x as f64))),
                "last_note_end_s": t.notes.iter().map(|n| n.end()).max().map(|x| round6(map.tick_to_seconds(x as f64))),
            })
        })
        .collect();
    let tempo: Vec<Value> = f
        .tempos
        .iter()
        .take(64)
        .map(|t| {
            json!({"tick": t.tick, "beat": map.tick_to_beats(t.tick as f64), "time_s": round6(map.tick_to_seconds(t.tick as f64)), "bpm": round6(us_to_bpm(t.data.us_per_quarter))})
        })
        .collect();
    let dur_s = f.duration_seconds();
    ctx.metric("notes", f.note_count());
    ctx.metric("duration_s", round6(dur_s));
    Ok(json!({
        "format": f.format,
        "ppq": f.ppq(),
        "smpte": match f.timing { Timing::Smpte { fps, ticks_per_frame } => json!({"fps": fps, "ticks_per_frame": ticks_per_frame}), _ => Value::Null },
        "tracks": tracks,
        "notes": f.note_count(),
        "duration": {
            "ticks": dur_ticks,
            "beats": map.tick_to_beats(dur_ticks as f64),
            "bars": if bar_ticks > 0.0 { round6(dur_ticks as f64 / bar_ticks) } else { 0.0 },
            "seconds": round6(dur_s),
        },
        "tempo": {
            "initial_bpm": round6(map.bpm_at(0)),
            "changes": f.tempos.len(),
            "map": tempo,
        },
        "time_signatures": f.time_signatures.iter().map(|t| json!({"tick": t.tick, "signature": format!("{}/{}", t.data.numerator, t.data.denominator)})).collect::<Vec<_>>(),
        "key_signatures": f.key_signatures.iter().map(|k| json!({"tick": k.tick, "key": gm::key_name(k.data.sharps, k.data.minor)})).collect::<Vec<_>>(),
    }))
}

fn midi_to_json(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let f = load_input(ctx, 0)?;
    let path = ctx.output_path(None, "json")?;
    let text = if a.flag("compact") {
        serde_json::to_string(&f.to_json_model())? + "\n"
    } else {
        f.to_json_string()
    };
    ctx.write_file(&path, text.as_bytes(), None)?;
    Ok(out_result(&path, &f, json!({})))
}

fn midi_from_json(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let mut f = load_input(ctx, 0)?;
    if a.has("ppq") {
        let p = a.int("ppq")?;
        if !(1..=32767).contains(&p) {
            return Err(Error::usage("--ppq must be 1..32767"));
        }
        f.set_ppq(p as u16);
    }
    let path = ctx.output_path(None, "mid")?;
    let bytes = if wants_json(ctx, &path) {
        f.to_json_string().into_bytes()
    } else {
        f.to_smf_bytes()
    };
    ctx.write_file(&path, &bytes, None)?;
    Ok(out_result(&path, &f, json!({})))
}

fn read_groove(ctx: &mut OpCtx, p: &Path) -> Result<Groove> {
    ctx.check_allowed(p)?;
    let text = std::fs::read_to_string(p).map_err(|e| {
        Error::new(
            ErrorKind::InputMissing,
            "input_not_found",
            format!("cannot read groove {}: {e}", p.display()),
        )
        .with_hint(format!("ferraus midi groove extract ref.mid -o {}", p.display()))
    })?;
    ctx.note_input(p);
    let g: Groove = serde_json::from_str(&text)
        .map_err(|e| Error::input(format!("{} is not a groove file: {e}", p.display())))?;
    g.grid()?;
    Ok(g)
}

fn midi_quantize(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let mut f = load_input(ctx, 0)?;
    let groove = match a.path("groove") {
        Some(p) => Some(read_groove(ctx, &p)?),
        None => None,
    };
    if groove.is_some() && a.is_explicit("swing") {
        ctx.warn("swing_ignored", "--swing is ignored when --groove is given");
    }
    let o = QuantizeOptions {
        grid: a.grid("grid")?,
        strength: a.percent("strength")?,
        swing: a.percent("swing")?,
        what: QuantizeWhat::parse(&a.choice("what"))?,
        humanize_s: a.seconds("humanize")?,
        min_length: a.grid("min-length")?,
        no_overlap: a.switch("no-overlap", true),
        groove,
        seed: ctx.seed(),
        tracks: track_filter(a, &f)?,
    };
    let rep = edit::quantize(&mut f, &o)?;
    if rep.grid_detected {
        ctx.note("grid", &rep.grid);
    }
    ctx.metric("notes_moved", rep.notes_moved);
    ctx.metric("mean_shift_ticks", round6(rep.mean_shift_ticks));
    let path = write_midi(ctx, &f, None)?;
    Ok(out_result(&path, &f, serde_json::to_value(&rep)?))
}

fn midi_groove_extract(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let f = load_input(ctx, 0)?;
    let grid = a.grid("grid")?.unwrap_or(Grid {
        num: 1,
        den: 16,
        triplet: false,
        dotted: false,
    });
    let mut g = edit::extract_groove(&f, grid)?;
    g.source = ctx.inputs.first().map(|p| p.display().to_string());
    let path = ctx.output_path(None, "json")?;
    let text = serde_json::to_string_pretty(&g)? + "\n";
    ctx.write_file(&path, text.as_bytes(), None)?;
    let max_off = g.positions.iter().map(|p| p.offset.abs()).fold(0.0, f64::max);
    ctx.metric("max_offset_grid_fraction", round6(max_off));
    Ok(json!({"output": path.display().to_string(), "grid": g.grid, "steps": g.steps, "notes": f.note_count()}))
}

fn midi_transpose(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let mut f = load_input(ctx, 0)?;
    let st = a.semitones("semitones")?;
    let n = st.round() as i32;
    if (st - n as f64).abs() > 1e-9 {
        ctx.warn("semitones_rounded", format!("{st} semitones rounded to {n}"));
    }
    let tr = track_filter(a, &f)?;
    let r = edit::transpose(&mut f, n, a.flag("include-drums"), &tr);
    if r.notes_dropped > 0 {
        ctx.warn(
            "notes_out_of_range",
            format!("{} notes moved outside 0-127 and were dropped", r.notes_dropped),
        );
    }
    let path = write_midi(ctx, &f, None)?;
    Ok(out_result(&path, &f, serde_json::to_value(&r)?))
}

fn parse_vel_range(s: &str) -> Result<(u8, u8)> {
    let (lo, hi) = s
        .split_once("..")
        .ok_or_else(|| Error::usage(format!("--range: '{s}' is not lo..hi (40..110)")))?;
    let p = |x: &str| -> Result<u8> {
        x.trim()
            .parse::<u8>()
            .ok()
            .filter(|v| (1..=127).contains(v))
            .ok_or_else(|| Error::usage(format!("--range: '{x}' is not a velocity 1..127")))
    };
    let (lo, hi) = (p(lo)?, p(hi)?);
    if lo > hi {
        return Err(Error::usage("--range: lo must not exceed hi"));
    }
    Ok((lo, hi))
}

fn midi_velocity(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let mut f = load_input(ctx, 0)?;
    let set = if a.has("set") {
        let v = a.int("set")?;
        if !(1..=127).contains(&v) {
            return Err(Error::usage("--set must be 1..127"));
        }
        Some(v as u8)
    } else {
        None
    };
    let scale = a.f64("scale")?;
    if !(scale.is_finite() && scale >= 0.0) {
        return Err(Error::usage("--scale must be a non-negative number"));
    }
    let o = VelocityOptions {
        set,
        scale,
        compress: a.percent("compress")?,
        range: match a.str("range") {
            Some(s) => Some(parse_vel_range(&s)?),
            None => None,
        },
    };
    let tr = track_filter(a, &f)?;
    let r = edit::velocity(&mut f, &o, &tr);
    let path = write_midi(ctx, &f, None)?;
    Ok(out_result(&path, &f, serde_json::to_value(&r)?))
}

fn midi_legato(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let mut f = load_input(ctx, 0)?;
    let gap = a.time("gap")?;
    let tr = track_filter(a, &f)?;
    let snapshot = f.clone();
    time_to_ticks(&snapshot, &gap, 0)?;
    let r = edit::legato(
        &mut f,
        &|at| time_to_ticks(&snapshot, &gap, at).unwrap_or(0),
        &tr,
    );
    let path = write_midi(ctx, &f, None)?;
    Ok(out_result(&path, &f, serde_json::to_value(&r)?))
}

fn midi_tempo(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let mut f = load_input(ctx, 0)?;
    if f.ppq().is_none() {
        return Err(Error::unsupported("SMPTE-timed files have no tempo to change"));
    }
    let before = f.tempo_map().bpm_at(0);
    match (a.has("set"), a.has("scale")) {
        (true, false) => {
            let b = a.f64("set")?;
            if !(b.is_finite() && b > 0.0 && b <= 1000.0) {
                return Err(Error::usage("--set must be a tempo in BPM between 1 and 1000"));
            }
            edit::tempo_set(&mut f, b);
        }
        (false, true) => {
            let s = a.f64("scale")?;
            if !(s.is_finite() && s > 0.0) {
                return Err(Error::usage("--scale must be positive"));
            }
            edit::tempo_scale(&mut f, s);
        }
        (true, true) => return Err(Error::usage("give --set or --scale, not both")),
        (false, false) => {
            return Err(Error::usage("midi tempo needs --set <bpm> or --scale <factor>")
                .with_hint("ferraus midi tempo song.mid --set 120 -o out.mid"))
        }
    }
    let after = f.tempo_map().bpm_at(0);
    let path = write_midi(ctx, &f, None)?;
    Ok(out_result(&path, &f, json!({"bpm_before": round6(before), "bpm_after": round6(after)})))
}

fn midi_split(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let f = load_input(ctx, 0)?;
    let by = if a.choice("by") == "channel" {
        SplitBy::Channel
    } else {
        SplitBy::Track
    };
    let parts = edit::split(&f, by);
    if parts.is_empty() {
        return Err(Error::input("the file has no notes to split"));
    }
    let mut outs = Vec::new();
    for (role, part) in &parts {
        let p = write_midi(ctx, part, Some(role))?;
        outs.push(json!({"role": role, "output": p.display().to_string(), "notes": part.note_count()}));
    }
    Ok(json!({"parts": outs}))
}

fn midi_merge(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let mut files = Vec::new();
    for i in 0..ctx.inputs.len() {
        files.push(load_input(ctx, i)?);
    }
    let (f, warns) = edit::merge(&files, a.choice("mode") == "single")?;
    for w in warns {
        ctx.warn("tempo_map_differs", w);
    }
    let path = write_midi(ctx, &f, None)?;
    Ok(out_result(&path, &f, json!({"inputs": files.len()})))
}

fn midi_trim(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let f = load_input(ctx, 0)?;
    let r = a
        .range("range")?
        .ok_or_else(|| Error::usage("midi trim needs --range start..end"))?;
    let s = point_to_tick(&f, &r.start)?;
    let e = point_to_tick(&f, &r.end)?;
    if s >= e {
        return Err(Error::usage(format!("--range: start (tick {s}) must be before end (tick {e})")));
    }
    let g = edit::trim(&f, s, e, a.switch("shift", true));
    let path = write_midi(ctx, &g, None)?;
    Ok(out_result(&path, &g, json!({"start_tick": s, "end_tick": e})))
}
