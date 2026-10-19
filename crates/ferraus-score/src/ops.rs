//! Op specs for the `score` command family.

use crate::check;
use crate::engrave;
use crate::fix;
use crate::formats::{self, Format, Loaded};
use crate::midi;
use crate::model::*;
use crate::notate::{self, ChordMode, Layout, NotateOptions, Split, Triplets};
use crate::perform::{self, PlayOptions};
use crate::pitch;
use crate::solfa::{self, SolfaKind};
use ferraus_core::prelude::*;
use ferraus_core::{Inputs, OpSpec};
use std::path::{Path, PathBuf};

pub fn ops() -> Vec<OpSpec> {
    vec![
        score_spec(),
        read_spec(),
        play_spec(),
        perform_spec(),
        convert_spec(),
        to_json_spec(),
        from_json_spec(),
        check_spec(),
    ]
}

// ------------------------------------------------------------------------------------
// Specs
// ------------------------------------------------------------------------------------

fn output_params() -> Vec<ferraus_core::ParamSpec> {
    vec![
        param("midi", ParamKind::Path).help("Also write the MIDI performance of the notation to this path (.mid)"),
        param("pdf", ParamKind::Path).help("Also engrave a PDF to this path (needs the ferraus-engrave helper or LilyPond)"),
        param("svg", ParamKind::Path).help("Also engrave SVG pages to this path (page 2 onward as <name>-2.svg, ...)"),
        param("png", ParamKind::Path).help("Also engrave PNG pages to this path (page 2 onward as <name>-2.png, ...)"),
        param("abc", ParamKind::Path).help("Also write ABC notation to this path"),
        param("ly", ParamKind::Path).help("Also write LilyPond source to this path"),
        param("mei", ParamKind::Path).help("Also write MEI to this path").advanced(),
        param("solfa-txt", ParamKind::Path).help("Also write tonic sol-fa text to this path (.md writes Markdown)"),
        param("json-out", ParamKind::Path).help("Also write the agent-facing score JSON to this path (named json-out because --json is the global result flag)"),
        param("engraver", ParamKind::Choice)
            .choices(&["auto", "verovio", "lilypond", "native"])
            .default("auto")
            .help("Engraving engine for SVG/PDF/PNG: auto tries the Verovio helper, then LilyPond, then (SVG only) the minimal native renderer")
            .advanced(),
    ]
}

fn score_spec() -> OpSpec {
    OpSpec::custom("score", Category::Score, "Notation from a recording or a MIDI file", op_score)
        .about(
            "Turns a MIDI file (or a recording, by running separate, transcribe, analyze tempo, analyze key and \
analyze chords first) into readable notation: a lead sheet with chord symbols, a grand-staff piano score with a hand \
split, a drum score with the standard drum map, or tonic sol-fa. Notes are quantized to --quantize with triplet \
detection, split into at most two voices per staff, rests are filled, ties cross bar lines, pickups are detected, and \
accidentals are spelled for the key. MusicXML 4.0 is the main output; add --midi, --pdf, --svg, --png, --abc, --ly, \
--solfa-txt, or --json-out for more. Every measure carries a confidence; low ones are listed with reasons.",
        )
        .best_effort()
        .inputs(Inputs::ONE_ANY)
        .params([
            param("layout", ParamKind::Choice)
                .choices(&["auto", "lead-sheet", "grand", "treble", "bass", "drums", "solfa"])
                .default("auto")
                .help("auto: MIDI channel 10 becomes a drum score, wide-range parts a grand staff, others a lead sheet (bass clef when low); for audio, vocals → lead sheet, piano → grand staff, drums → drum score"),
            param("chords", ParamKind::Choice)
                .choices(&["on", "off", "diagrams"])
                .default("on")
                .help("on: chord symbols above the staff (C, Am7, G/B); diagrams: also guitar fretboard diagrams (with a capo suggestion when --transpose is used)"),
            param("solfa", ParamKind::Choice)
                .choices(&["off", "on", "only", "fixed", "fixed-only"])
                .default("off")
                .help("on: tonic sol-fa syllables (movable do) under the notes; only: write sol-fa text and no staff; fixed / fixed-only: fixed do (do = C)"),
            param("key", ParamKind::Str).default("auto").help("Key of the input (before --transpose) such as G, Em, Bb major; auto uses the MIDI key signature (or analyze key for audio) or a Krumhansl estimate on the notes"),
            param("time", ParamKind::Str).default("auto").help("Time signature such as 3/4 or 6/8; auto uses the MIDI time signature (or analyze tempo for audio)"),
            param("tempo", ParamKind::Str).default("auto").help("Tempo in quarter notes per minute; auto uses the MIDI tempo"),
            param("quantize", ParamKind::Grid).default("1/16").help("Quantization grid (1/8, 1/16, 1/32; 1/8T forces triplets)"),
            param("triplets", ParamKind::Choice).choices(&["auto", "on", "off"]).default("auto").help("Triplet detection per beat: auto picks triplets when they fit clearly better than the straight grid"),
            param("split", ParamKind::Str).default("auto").help("Grand-staff hand split: auto (dynamic around middle C within a hand span) or a note such as C4 (that note and above go to the right hand)"),
            param("parts", ParamKind::Str).default("auto").help("For audio: stems to notate, comma separated from vocals,piano,guitar,bass,drums (auto: all of them)"),
            param("lyrics", ParamKind::Path).help("Text file of lyrics, one syllable per melody note (hyphenate syllables: hel-lo; _ holds a syllable)"),
            param("transpose", ParamKind::Int).default("0").help("Transpose by semitones (e.g. -2, 5)"),
            param("title", ParamKind::Str).help("Title"),
            param("composer", ParamKind::Str).help("Composer"),
            param("pickup", ParamKind::Switch).default("on").help("Detect a pickup (anacrusis) bar"),
        ])
        .params(output_params())
        .example("ferraus score song.mid -o song.musicxml --pdf song.pdf", "MIDI to a MusicXML score plus an engraved PDF")
        .example("ferraus score piano.mid --layout grand --split C4 -o piano.musicxml", "Grand staff with a fixed hand split at middle C")
        .example("ferraus score beat.mid --layout drums -o drums.musicxml", "Drum score with the standard drum map")
        .example("ferraus score melody.mid --chords diagrams --transpose 3 --lyrics words.txt -o lead.musicxml", "Lead sheet with guitar diagrams, transposed up three semitones, with lyrics")
        .example("ferraus score hymn.mid --solfa only -o hymn.txt", "Tonic sol-fa text only, no staff")
        .example("ferraus score take.wav --parts vocals,bass,drums -o take.musicxml --midi take.mid", "Notation from a recording: separate, transcribe, and notate three stems")
}

fn read_spec() -> OpSpec {
    OpSpec::custom("score read", Category::Score, "Optical music recognition: sheet music to MusicXML", op_read)
        .about(
            "Reads printed sheet music (PDF pages or photos/scans) into MusicXML with per-measure confidence. The builtin \
engine uses the oemer and homr models from the model cache (ferraus models pull omr/oemer); --engine audiveris uses the \
optional Audiveris bridge (ferraus doctor --install-audiveris). --review lists measures whose durations do not add up \
or whose symbols were uncertain; --fix applies an edit list (measure, voice, note, set) to correct them. Notation \
inputs (MusicXML, MEI, ABC, LilyPond, JSON) skip recognition, so --fix and --review work on any score.",
        )
        .best_effort()
        .inputs(Inputs::ONE_ANY)
        .params([
            param("pages", ParamKind::Str).help("Pages to read, 1-based (1-4, 2,5-6); default all"),
            param("dpi", ParamKind::Int).default("300").range("72..1200").help("Rasterization resolution for PDF pages in dots per inch"),
            param("engine", ParamKind::Choice).choices(&["builtin", "audiveris"]).default("builtin").help("builtin: ONNX models from the model cache; audiveris: the optional Java bridge (AGPL)"),
            param("review", ParamKind::Flag).help("Report per-measure duration checks, uncertain measures, and an issue list"),
            param("fix", ParamKind::Path).help("Apply an edit list (JSON: [{measure, voice, note, set: {pitch, duration, ...}}]) before writing"),
        ])
        .example("ferraus score read sonata.pdf --pages 1-4 -o sonata.musicxml --review", "Read the first four pages and list measures to check")
        .example("ferraus score read sonata.musicxml --fix edits.json -o fixed.musicxml", "Apply corrections to a score (no recognition needed)")
        .example("ferraus score read page.png --engine audiveris -o page.musicxml", "Use the Audiveris bridge on a scanned page")
}

fn play_params() -> Vec<ferraus_core::ParamSpec> {
    vec![
        param("tempo", ParamKind::Str).default("auto").help("Tempo override in quarter notes per minute; auto follows the score's tempo marks (120 when none)"),
        param("repeats", ParamKind::Switch).default("on").help("Unroll repeats and first/second endings"),
        param("dynamics", ParamKind::Switch).default("on").help("Map dynamics (pp..ff) to velocities"),
        param("articulation", ParamKind::Switch).default("on").help("Map articulations to note lengths: staccato 50%, staccatissimo 25%, tenuto and slurred (legato) 100%, other notes 95% (gap at most a 64th note); accents add velocity"),
        param("humanize", ParamKind::Percent).default("0%").help("Random timing (up to ±20 ms) and velocity (up to ±12) variation, seeded by --seed"),
        param("swing", ParamKind::Percent).default("0%").help("Delay off-beat eighths: 100% moves them to the triplet position (2:1)"),
        param("instrument-map", ParamKind::Str).help("Programs per part, e.g. \"P1=piano,P2=strings\" (GM names, short names, or numbers 0-127)"),
        param("click", ParamKind::Switch).default("off").help("Add a metronome click track (channel 10)"),
    ]
}

fn play_spec() -> OpSpec {
    OpSpec::custom("score play", Category::Score, "Notation to a MIDI performance", op_play)
        .about(
            "Performs a score (MusicXML, MXL, MEI, ABC, LilyPond, score JSON, or MIDI) as a MIDI file: tempo marks or \
--tempo, repeats and endings unrolled, dynamics to velocity, articulations to note lengths, optional swing, humanize, \
program changes per part, and a click track.",
        )
        .inputs(Inputs::ONE_ANY)
        .params(play_params())
        .example("ferraus score play song.musicxml -o song.mid", "Perform a MusicXML score as MIDI")
        .example("ferraus score play tune.abc --tempo 96 --swing 60% --click on -o tune.mid", "Swung performance at 96 bpm with a click")
        .example("ferraus score play quartet.musicxml --instrument-map \"P1=violin,P2=violin,P3=viola,P4=cello\" -o quartet.mid", "Assign instruments per part")
}

fn perform_spec() -> OpSpec {
    OpSpec::custom("score perform", Category::Score, "One shot: read, play, render", op_perform)
        .about(
            "Reads sheet music (a PDF or image through score read, or any notation file), performs it as MIDI, and renders \
the MIDI to audio through `render` with the chosen instrument. The MusicXML and MIDI are written beside the audio \
(same name, .musicxml and .mid).",
        )
        .best_effort()
        .inputs(Inputs::ONE_ANY)
        .params(play_params())
        .params([
            param("instrument", ParamKind::Str).default("piano").help("Instrument for render: an installed sound name, a .sf2/.sfz path, or plugin:<name>"),
            param("pages", ParamKind::Str).help("Pages to read from a PDF (1-4); default all"),
            param("dpi", ParamKind::Int).default("300").help("Rasterization resolution for PDF pages in dots per inch"),
            param("engine", ParamKind::Choice).choices(&["builtin", "audiveris"]).default("builtin").help("OMR engine for PDF and image inputs"),
            param("fix", ParamKind::Path).help("Apply an edit list before performing"),
        ])
        .example("ferraus score perform sonata.pdf --instrument piano -o sonata.wav", "Read a printed score and render it (writes sonata.musicxml and sonata.mid too)")
        .example("ferraus score perform tune.abc --tempo 100 -o tune.wav", "Render an ABC tune at 100 bpm")
        .example("ferraus score perform quartet.musicxml --instrument strings --humanize 30% --dynamics on -o quartet.wav", "Render a MusicXML quartet on strings with a little human timing")
        .example("ferraus score perform lead.musicxml --instrument salamander-grand-piano --swing 60% --repeats off -o lead.wav", "Swung piano performance of a lead sheet, playing each section once")
}

fn convert_spec() -> OpSpec {
    OpSpec::custom("score convert", Category::Score, "Convert between notation formats", op_convert)
        .about(
            "Converts between MusicXML (.musicxml/.xml), compressed MusicXML (.mxl), MEI, ABC, LilyPond (.ly), MIDI, score \
JSON, tonic sol-fa text (.txt) or Markdown (.md), and engraved SVG, PDF, and PNG, chosen from the file extensions. Give \
the output as a second path or with -o.",
        )
        .inputs(Inputs { min: 1, max: Some(2), kind: InputKind::Any })
        .params([
            param("pretty", ParamKind::Flag).help("Indent JSON output"),
            param("solfa", ParamKind::Choice).choices(&["movable", "fixed"]).default("movable").help("Sol-fa system for .txt/.md outputs"),
            param("engraver", ParamKind::Choice).choices(&["auto", "verovio", "lilypond", "native"]).default("auto").help("Engraving engine for SVG/PDF/PNG (native: built-in SVG fallback)").advanced(),
        ])
        .example("ferraus score convert song.musicxml song.abc", "MusicXML to ABC (output as the second path)")
        .example("ferraus score convert tune.abc -o tune.pdf", "Engrave an ABC tune to PDF (output with -o)")
        .example("ferraus score convert song.mid song.ly", "Notate a MIDI file as LilyPond source")
}

fn to_json_spec() -> OpSpec {
    OpSpec::custom("score to-json", Category::Score, "Score as the agent-facing JSON model", op_to_json)
        .about(
            "Prints (or writes with -o) the score model: parts, staves, measures, voices, events with pitches (step, alter, \
octave, MIDI), durations in divisions per quarter, ties, tuplets, chord symbols, lyrics, sol-fa syllables, dynamics, \
articulations, repeats, and per-measure confidence. Edit it and convert back with score from-json.",
        )
        .inputs(Inputs::ONE_ANY)
        .param(param("pretty", ParamKind::Flag).help("Indent the JSON"))
        .example("ferraus score to-json song.musicxml --pretty -o song.json", "Write the score model to a file")
        .example("ferraus score to-json song.mid --json", "Notate a MIDI file and return the model in the result envelope")
        .example("ferraus score to-json tune.abc -o tune.json", "Turn an ABC tune into compact score JSON for an agent to edit")
}

fn from_json_spec() -> OpSpec {
    OpSpec::custom("score from-json", Category::Score, "Score JSON back to notation", op_from_json)
        .about("Reads the score JSON model and writes MusicXML (default) or any format chosen by the -o extension.")
        .inputs(Inputs::ONE_ANY)
        .param(param("pretty", ParamKind::Flag).help("Indent JSON output when -o ends in .json"))
        .example("ferraus score from-json song.json -o song.musicxml", "Back to MusicXML after editing")
        .example("ferraus score from-json song.json -o song.pdf", "Engrave the edited model")
        .example(
            "ferraus score from-json edited.json -o clean.json --pretty",
            "Normalize and indent an edited score model",
        )
        .example("ferraus score from-json song.json -o song.abc", "Write the model as ABC notation")
}

fn check_spec() -> OpSpec {
    OpSpec::custom(
        "score check",
        Category::Score,
        "Validate a score: measure durations, voices, unsupported constructs",
        op_check,
    )
    .about(
        "Checks every measure's voices against the time signature (pickups and a final bar completing a pickup are \
allowed), voice numbering, ties, pitch/MIDI consistency, chord symbols, and constructs the reader skipped. Returns a \
JSON report; exits with code 9 (verification) when any error is found. -o writes the report to a file first.",
    )
    .inputs(Inputs::ONE_ANY)
    .param(param("strict", ParamKind::Flag).help("Treat warnings as failures too"))
    .example("ferraus score check song.musicxml --json", "Validate a score and get the report")
    .example("ferraus score check scan.musicxml -o report.json", "Write the report to a file (exit 9 when it fails)")
    .example("ferraus score check tune.abc --strict", "Fail on warnings as well as errors before publishing a score")
}

// ------------------------------------------------------------------------------------
// Helpers
// ------------------------------------------------------------------------------------

fn read_bytes(ctx: &mut OpCtx, path: &Path) -> Result<Vec<u8>> {
    if path.as_os_str() == "-" {
        return ctx.stdin.clone().ok_or_else(|| Error::input("stdin is empty"));
    }
    ctx.check_allowed(path)?;
    if !path.exists() {
        return Err(Error::new(
            ErrorKind::InputMissing,
            "input_not_found",
            format!("input not found: {}", path.display()),
        ));
    }
    ctx.note_input(path);
    Ok(std::fs::read(path)?)
}

fn input_format(path: &Path, bytes: &[u8]) -> Option<Format> {
    match formats::format_of(path) {
        Some(Format::MusicXml) | None => formats::sniff(bytes).or(formats::format_of(path)),
        f => f,
    }
}

fn load_input(ctx: &mut OpCtx, i: usize) -> Result<Loaded> {
    let path = ctx.input(i)?.to_path_buf();
    let bytes = read_bytes(ctx, &path)?;
    match input_format(&path, &bytes) {
        Some(f) if f.readable() => formats::load_as(f, &bytes),
        Some(Format::Pdf) | Some(Format::Image) => {
            Err(Error::unsupported(format!("{} is sheet music to recognize, not a notation file", path.display()))
                .with_hint(format!("ferraus score read {} -o score.musicxml", path.display())))
        }
        Some(Format::Audio) => Err(Error::unsupported(format!("{} is audio", path.display()))
            .with_hint(format!("ferraus score {} -o score.musicxml", path.display()))),
        _ => Err(Error::unsupported(format!("cannot tell the notation format of {}", path.display()))
            .with_hint("notation inputs: .musicxml .mxl .mei .abc .ly .mid .json")),
    }
}

/// Resolve an extra output path given as a parameter (honours --outdir).
fn extra_path(ctx: &OpCtx, p: PathBuf) -> PathBuf {
    match &ctx.globals.outdir {
        Some(d) if p.is_relative() => d.join(p),
        _ => p,
    }
}

fn role_for(fmt: Format) -> &'static str {
    match fmt {
        Format::MusicXml | Format::Mxl => "musicxml",
        Format::Mei => "mei",
        Format::Abc => "abc",
        Format::Lily => "lilypond",
        Format::Midi => "midi",
        Format::Json => "json",
        Format::SolfaText | Format::SolfaMarkdown => "solfa",
        Format::Svg => "svg",
        Format::Pdf => "pdf",
        Format::Png => "png",
        _ => "score",
    }
}

/// Write `score` to `path` in format `fmt` (engraving when needed). Returns written paths.
#[allow(clippy::too_many_arguments)]
fn write_as(
    ctx: &mut OpCtx,
    score: &Score,
    path: &Path,
    fmt: Format,
    pretty: bool,
    kind: SolfaKind,
    engraver: &str,
    role: Option<&str>,
) -> Result<Vec<String>> {
    let role = role.unwrap_or(role_for(fmt));
    match fmt {
        Format::Svg | Format::Pdf | Format::Png => {
            let to = match fmt {
                Format::Svg => "svg",
                Format::Pdf => "pdf",
                _ => "png",
            };
            if ctx.globals.dry_run {
                ctx.prepare_output(path)?;
                ctx.record_output(path, Some(role));
                return Ok(vec![path.display().to_string()]);
            }
            let eng = engrave::engrave(score, to, engraver)?;
            ctx.note("engraver", &eng.engine);
            let paths = engrave::page_paths(path, eng.pages.len());
            let mut out = Vec::new();
            for (p, bytes) in paths.iter().zip(eng.pages.iter()) {
                ctx.write_file(p, bytes, Some(role))?;
                out.push(p.display().to_string());
            }
            Ok(out)
        }
        Format::Image | Format::Audio => {
            Err(Error::unsupported(format!("cannot write notation as {}", path.display())))
        }
        f => {
            let bytes = formats::save(score, f, pretty, kind)?;
            ctx.write_file(path, &bytes, Some(role))?;
            Ok(vec![path.display().to_string()])
        }
    }
}

fn out_format(path: &Path) -> Result<Format> {
    if path.as_os_str() == "-" {
        return Ok(Format::MusicXml);
    }
    formats::format_of(path).ok_or_else(|| {
        Error::usage(format!("cannot tell the output format of {}", path.display()))
            .with_hint("use .musicxml .mxl .mei .abc .ly .mid .json .txt .md .svg .pdf .png")
    })
}

/// The extra outputs requested with --midi/--pdf/--svg/--png/--abc/--ly/--mei/--solfa-txt/--json-out.
fn write_extras(ctx: &mut OpCtx, a: &Args, score: &Score, kind: SolfaKind) -> Result<Vec<serde_json::Value>> {
    let engraver = a.str_or("engraver", "auto");
    let mut written = Vec::new();
    let table: [(&str, Option<Format>); 9] = [
        ("midi", Some(Format::Midi)),
        ("abc", Some(Format::Abc)),
        ("ly", Some(Format::Lily)),
        ("mei", Some(Format::Mei)),
        ("solfa-txt", None),
        ("json-out", Some(Format::Json)),
        ("svg", Some(Format::Svg)),
        ("png", Some(Format::Png)),
        ("pdf", Some(Format::Pdf)),
    ];
    for (name, fmt) in table {
        let Some(p) = a.path(name) else { continue };
        let p = extra_path(ctx, p);
        let fmt = match fmt {
            Some(f) => f,
            None => {
                if formats::ext_of(&p) == "md" || formats::ext_of(&p) == "markdown" {
                    Format::SolfaMarkdown
                } else {
                    Format::SolfaText
                }
            }
        };
        let paths = write_as(ctx, score, &p, fmt, true, kind, &engraver, None)?;
        written.push(json!({"format": fmt.name(), "paths": paths}));
    }
    Ok(written)
}

fn parse_time(s: &str) -> Result<Option<TimeSig>> {
    if s == "auto" || s.is_empty() {
        return Ok(None);
    }
    let (a, b) = s.split_once('/').ok_or_else(|| Error::usage(format!("--time: '{s}' is not like 3/4")))?;
    let a: u32 = a.trim().parse().map_err(|_| Error::usage(format!("--time: '{s}' is not like 3/4")))?;
    let b: u32 = b.trim().parse().map_err(|_| Error::usage(format!("--time: '{s}' is not like 3/4")))?;
    if a == 0 || !matches!(b, 1 | 2 | 4 | 8 | 16 | 32) {
        return Err(Error::usage(format!("--time: '{s}' is not a valid time signature")));
    }
    Ok(Some(TimeSig::new(a, b)))
}

fn parse_tempo_arg(s: &str) -> Result<Option<f64>> {
    if s == "auto" || s.is_empty() {
        return Ok(None);
    }
    let v: f64 = s
        .trim_end_matches("bpm")
        .trim()
        .parse()
        .map_err(|_| Error::usage(format!("--tempo: '{s}' is not a number of quarter notes per minute")))?;
    if !(1.0..=1000.0).contains(&v) {
        return Err(Error::usage("--tempo must be between 1 and 1000"));
    }
    Ok(Some(v))
}

fn solfa_mode(a: &Args) -> (bool, bool, SolfaKind) {
    // (annotate, only, kind)
    match a.choice("solfa").as_str() {
        "on" => (true, false, SolfaKind::Movable),
        "only" => (true, true, SolfaKind::Movable),
        "fixed" => (true, false, SolfaKind::Fixed),
        "fixed-only" => (true, true, SolfaKind::Fixed),
        _ => (false, false, SolfaKind::Movable),
    }
}

/// Transpose a loaded score by semitones (respelled in the new key).
pub fn transpose_score(score: &mut Score, semis: i32) {
    if semis == 0 {
        return;
    }
    for p in score.parts.iter_mut().filter(|p| !p.percussion) {
        for st in p.staves.iter_mut() {
            let mut key = KeySig::major(0);
            for m in st.measures.iter_mut() {
                if let Some(k) = m.key {
                    let nk = pitch::transpose_key(k, semis);
                    m.key = Some(nk);
                    key = nk;
                }
                for h in m.harmonies.iter_mut() {
                    h.root = pitch::spell_pc(h.root.pc() + semis, key);
                    h.bass = h.bass.map(|b| pitch::spell_pc(b.pc() + semis, key));
                    h.text = crate::chords::chord_text(h.root, &h.quality, h.bass);
                    h.frame = None;
                }
                for v in m.voices.iter_mut() {
                    for e in v.events.iter_mut() {
                        for n in e.notes.iter_mut() {
                            n.midi += semis;
                            n.pitch = pitch::spell_midi(n.midi, key);
                        }
                    }
                }
            }
        }
    }
}

fn notate_options(a: &Args) -> Result<NotateOptions> {
    let mut o = NotateOptions {
        triplets: match a.choice("triplets").as_str() {
            "on" => Triplets::On,
            "off" => Triplets::Off,
            _ => Triplets::Auto,
        },
        ..NotateOptions::default()
    };
    if let Some(g) = a.grid("quantize")? {
        o.set_grid(g);
    }
    o.time = parse_time(&a.str_or("time", "auto"))?;
    let key = a.str_or("key", "auto");
    if key != "auto" {
        o.key = Some(
            pitch::parse_key(&key)
                .ok_or_else(|| Error::usage(format!("--key: '{key}' is not a key (G, Em, Bb major, F# minor)")))?,
        );
    }
    o.tempo = parse_tempo_arg(&a.str_or("tempo", "auto"))?;
    let split = a.str_or("split", "auto");
    o.split = if split == "auto" {
        Split::Auto
    } else {
        Split::At(
            units::parse_note_name(&split)
                .ok_or_else(|| Error::usage(format!("--split: '{split}' is not auto or a note such as C4")))?,
        )
    };
    o.layout = Layout::parse(&a.choice("layout")).unwrap_or(Layout::Auto);
    o.chords = match a.choice("chords").as_str() {
        "off" => ChordMode::Off,
        "diagrams" => ChordMode::Diagrams,
        _ => ChordMode::On,
    };
    o.transpose = a.int("transpose")? as i32;
    if o.transpose.abs() > 48 {
        return Err(Error::usage("--transpose must be within ±48 semitones"));
    }
    o.title = a.str("title");
    o.composer = a.str("composer");
    o.pickup = a.switch("pickup", true);
    Ok(o)
}

// ------------------------------------------------------------------------------------
// score
// ------------------------------------------------------------------------------------

fn op_score(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let input = ctx.input(0)?.to_path_buf();
    let mut opts = notate_options(a)?;
    let (annotate, only, kind) = solfa_mode(a);
    let layout_solfa = a.choice("layout") == "solfa";
    let only = only || layout_solfa;
    if let Some(lp) = a.path("lyrics") {
        ctx.check_allowed(&lp)?;
        let t = std::fs::read_to_string(&lp)
            .map_err(|e| Error::input(format!("cannot read lyrics {}: {e}", lp.display())))?;
        opts.lyrics = Some(t);
    }
    let fmt = if input.as_os_str() == "-" {
        ctx.stdin.as_deref().and_then(formats::sniff)
    } else {
        formats::format_of(&input).or_else(|| std::fs::read(&input).ok().and_then(|b| formats::sniff(&b)))
    };
    let mut audio_info = Value::Null;
    let (mut score, info) = match fmt {
        Some(Format::Audio) => {
            if ctx.globals.dry_run {
                let main = ctx.output_path(None, if only { "txt" } else { "musicxml" })?;
                ctx.prepare_output(&main)?;
                ctx.record_output(&main, Some("musicxml"));
                return Ok(
                    json!({"plan": ["analyze tempo", "analyze key", "analyze chords", "separate", "transcribe (per stem)", "drum transcription (native)", "notate"], "parts": crate::audio::parts_list(&a.str_or("parts", "auto"))}),
                );
            }
            ctx.note_input(&input);
            let front = crate::audio::front(ctx, &input, &a.str_or("parts", "auto"))?;
            if opts.key.is_none() {
                opts.key = front.key;
            }
            if opts.time.is_none() {
                opts.time = front.time.map(|(n, d)| TimeSig::new(n, d));
            }
            if opts.tempo.is_none() {
                opts.tempo = Some((front.bpm * 10.0).round() / 10.0);
            }
            opts.track_layouts = front.layouts.clone();
            if opts.layout != Layout::Auto {
                opts.track_layouts.clear();
            }
            opts.external_chords = front.chords.clone();
            opts.note_confidence = Some(front.note_confidence.clone());
            audio_info = json!({"stems": front.stems, "bpm": front.bpm});
            let (s, i) = notate::notate(&front.midi, &opts);
            (s, Some(i))
        }
        Some(Format::Midi) => {
            let bytes = read_bytes(ctx, &input)?;
            let m = midi::read_midi(&bytes)?;
            let (s, i) = notate::notate(&m, &opts);
            (s, Some(i))
        }
        Some(f) if f.readable() => {
            let l = load_input(ctx, 0)?;
            let mut s = l.score;
            transpose_score(&mut s, opts.transpose);
            if let Some(t) = &opts.title {
                s.title = Some(t.clone());
            }
            if let Some(c) = &opts.composer {
                s.composer = Some(c.clone());
            }
            if let Some(ly) = &opts.lyrics {
                if let Some(p) = s.parts.iter_mut().find(|p| !p.percussion) {
                    notate::apply_lyrics(&mut p.staves[0], ly);
                }
            }
            (s, None)
        }
        Some(Format::Pdf) | Some(Format::Image) => {
            return Err(Error::unsupported(format!("{} is sheet music; recognize it first", input.display()))
                .with_hint(format!("ferraus score read {} -o score.musicxml", input.display())));
        }
        _ => {
            return Err(Error::unsupported(format!("cannot tell what {} is", input.display()))
                .with_hint("score accepts audio, MIDI, or notation files"))
        }
    };
    if annotate {
        solfa::annotate(&mut score, kind);
    }
    // main output
    let main = ctx.output_path(None, if only { "txt" } else { "musicxml" })?;
    let mut main_fmt = out_format(&main)?;
    if only {
        main_fmt = match main_fmt {
            Format::SolfaMarkdown => Format::SolfaMarkdown,
            Format::SolfaText => Format::SolfaText,
            other => {
                return Err(Error::usage(format!(
                    "--solfa only writes sol-fa text and no staff, but -o asks for {}",
                    other.name()
                ))
                .with_hint("use -o song.txt or -o song.md"));
            }
        };
    }
    let engraver = a.str_or("engraver", "auto");
    let main_paths = write_as(
        ctx,
        &score,
        &main,
        main_fmt,
        true,
        kind,
        &engraver,
        Some(if only { "solfa" } else { role_for(main_fmt) }),
    )?;
    let extras = if only {
        // only the requested text outputs; no staff notation
        let mut v = Vec::new();
        for name in ["json-out", "midi"] {
            if a.path(name).is_some() {
                ctx.warn("solfa_only", format!("--{name} ignored with --solfa only"));
            }
        }
        if let Some(p) = a.path("solfa-txt") {
            let p = extra_path(ctx, p);
            let f = if formats::ext_of(&p) == "md" { Format::SolfaMarkdown } else { Format::SolfaText };
            v.push(json!({"format": f.name(), "paths": write_as(ctx, &score, &p, f, true, kind, &engraver, None)?}));
        }
        v
    } else {
        write_extras(ctx, a, &score, kind)?
    };
    let report = check::check(&score, &[]);
    let mut result = json!({
        "main": {"format": main_fmt.name(), "paths": main_paths},
        "extra_outputs": extras,
        "parts": score.parts.iter().map(|p| json!({"id": p.id, "name": p.name, "staves": p.staves.len(), "percussion": p.percussion})).collect::<Vec<_>>(),
        "measures": score.measure_count(),
        "check": {"ok": report.ok, "errors": report.errors, "warnings": report.warnings},
        "low_confidence": score.low_confidence,
    });
    if let Some(i) = info {
        ctx.note("key", &i.key);
        ctx.note("time", &i.time);
        result["notation"] = serde_json::to_value(&i)?;
    }
    if !audio_info.is_null() {
        result["audio"] = audio_info;
    }
    Ok(result)
}

// ------------------------------------------------------------------------------------
// score read
// ------------------------------------------------------------------------------------

fn load_or_recognize(ctx: &mut OpCtx, a: &Args) -> Result<(Score, Vec<String>, String)> {
    let input = ctx.input(0)?.to_path_buf();
    let bytes = read_bytes(ctx, &input)?;
    match input_format(&input, &bytes) {
        Some(f) if f.readable() => {
            let l = formats::load_as(f, &bytes)?;
            Ok((l.score, l.unsupported, format!("none ({} input)", f.name())))
        }
        Some(Format::Pdf) | Some(Format::Image) => {
            let dpi = a.int("dpi").unwrap_or(300).clamp(72, 1200) as u32;
            let engine = a.str_or("engine", "builtin");
            let r = crate::omr::recognize(&input, &engine, a.str("pages").as_deref(), dpi)?;
            if r.movements > 1 {
                ctx.warn("movements_merged", format!("{} movements were appended into one score", r.movements));
            }
            Ok((r.score, r.unsupported, r.engine))
        }
        _ => Err(Error::unsupported(format!("{} is not a PDF, image, or notation file", input.display()))),
    }
}

fn apply_fix(ctx: &mut OpCtx, a: &Args, score: &mut Score) -> Result<Option<Value>> {
    let Some(fp) = a.path("fix") else { return Ok(None) };
    ctx.check_allowed(&fp)?;
    let text = std::fs::read(&fp).map_err(|e| Error::input(format!("cannot read edits {}: {e}", fp.display())))?;
    let edits: Value = serde_json::from_slice(&text)?;
    let results = fix::apply(score, &edits)?;
    let failed = results.iter().filter(|r| r["ok"] == json!(false)).count();
    if failed > 0 {
        ctx.warn("edits_failed", format!("{failed} edit(s) could not be applied; see result.fixes"));
    }
    Ok(Some(Value::Array(results)))
}

fn op_read(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let (mut score, unsupported, engine) = load_or_recognize(ctx, a)?;
    let fixes = apply_fix(ctx, a, &mut score)?;
    score.refresh_low_confidence(0.6);
    let report = check::check(&score, &unsupported);
    let out = ctx.output_path(None, "musicxml")?;
    let fmt = out_format(&out)?;
    let paths = write_as(ctx, &score, &out, fmt, true, SolfaKind::Movable, "auto", None)?;
    let mut result = json!({
        "engine": engine,
        "outputs": paths,
        "parts": score.parts.len(),
        "measures": score.measure_count(),
        "check_ok": report.ok,
        "failing_measures": report.failing_measures,
    });
    if let Some(f) = fixes {
        result["fixes"] = f;
    }
    if a.flag("review") {
        result["review"] = serde_json::to_value(&report)?;
    }
    Ok(result)
}

// ------------------------------------------------------------------------------------
// score play / perform
// ------------------------------------------------------------------------------------

fn play_options(a: &Args, ctx: &OpCtx, score: &Score) -> Result<PlayOptions> {
    let mut o = PlayOptions {
        tempo: parse_tempo_arg(&a.str_or("tempo", "auto"))?,
        repeats: a.switch("repeats", true),
        dynamics: a.switch("dynamics", true),
        articulation: a.switch("articulation", true),
        humanize: a.percent("humanize")?.clamp(0.0, 1.0),
        swing: a.percent("swing")?.clamp(0.0, 1.0),
        click: a.switch("click", false),
        seed: ctx.seed(),
        ..PlayOptions::default()
    };
    if let Some(map) = a.str("instrument-map") {
        for item in map.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
            let (pid, inst) = item
                .split_once('=')
                .ok_or_else(|| Error::usage(format!("--instrument-map: '{item}' is not like P1=piano")))?;
            let pid = pid.trim();
            if !score.parts.iter().any(|p| p.id == pid) {
                let ids: Vec<&str> = score.parts.iter().map(|p| p.id.as_str()).collect();
                return Err(Error::usage(format!("--instrument-map: no part '{pid}' (parts: {})", ids.join(", "))));
            }
            let prog = crate::drums::program_for(inst.trim()).ok_or_else(|| Error::usage(format!("--instrument-map: '{}' is not a known instrument (piano, strings, violin, flute, guitar, bass, or 0-127)", inst.trim())))?;
            o.instrument_map.insert(pid.to_string(), prog);
        }
    }
    Ok(o)
}

fn op_play(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let l = load_input(ctx, 0)?;
    let opts = play_options(a, ctx, &l.score)?;
    let (m, info) = perform::perform(&l.score, &opts);
    let bytes = midi::write_midi(&m)?;
    let out = ctx.output_path(None, "mid")?;
    ctx.write_file(&out, &bytes, Some("midi"))?;
    ctx.metric("duration_s", (info.duration_s * 1000.0).round() / 1000.0);
    Ok(serde_json::to_value(&info)?)
}

fn op_perform(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let (mut score, _unsupported, engine) = load_or_recognize(ctx, a)?;
    let fixes = apply_fix(ctx, a, &mut score)?;
    let opts = play_options(a, ctx, &score)?;
    let (m, info) = perform::perform(&score, &opts);
    let audio = ctx.output_path(None, "wav")?;
    let xml_path = audio.with_extension("musicxml");
    let mid_path = audio.with_extension("mid");
    // the notation input itself may already sit at the MusicXML/MIDI path beside the audio
    let is_input = |p: &Path, ctx: &OpCtx| {
        ctx.inputs.iter().any(|i| ferraus_core::ctx::absolutize(i) == ferraus_core::ctx::absolutize(p))
    };
    if !is_input(&xml_path, ctx) {
        ctx.write_file(&xml_path, crate::musicxml::write(&score).as_bytes(), Some("musicxml"))?;
    }
    if is_input(&mid_path, ctx) {
        return Err(Error::usage(format!("the performance MIDI would overwrite the input {}", mid_path.display()))
            .with_hint("choose another -o name for the audio"));
    }
    ctx.write_file(&mid_path, &midi::write_midi(&m)?, Some("midi"))?;
    let inst = a.str_or("instrument", "piano");
    let mut render = Value::Null;
    if ctx.globals.dry_run {
        ctx.prepare_output(&audio)?;
        ctx.record_output(&audio, Some("audio"));
    } else {
        let (v, outs) = ferraus_core::exec::run_nested(
            ctx,
            "render",
            &[("instrument", inst.as_str())],
            vec![mid_path.clone()],
            Some(&audio),
        )?;
        for mut o in outs {
            if o.role.is_none() {
                o.role = Some("audio".into());
            }
            ctx.outputs.push(o);
        }
        render = v;
    }
    let mut result = json!({
        "engine": engine,
        "musicxml": xml_path.display().to_string(),
        "midi": mid_path.display().to_string(),
        "audio": audio.display().to_string(),
        "performance": info,
        "render": render,
    });
    if let Some(f) = fixes {
        result["fixes"] = f;
    }
    Ok(result)
}

// ------------------------------------------------------------------------------------
// convert / to-json / from-json / check
// ------------------------------------------------------------------------------------

fn op_convert(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let out = match ctx.inputs.get(1).cloned() {
        Some(o) => {
            if ctx.globals.output.is_some() {
                return Err(Error::usage("give the output either as the second path or with -o, not both"));
            }
            ctx.inputs.truncate(1);
            extra_path(ctx, o)
        }
        None => match &ctx.globals.output {
            Some(_) => ctx.output_path(None, "musicxml")?,
            None => {
                return Err(Error::usage(
                    "score convert needs an output: ferraus score convert in.musicxml out.abc (or -o out.abc)",
                ))
            }
        },
    };
    let l = load_input(ctx, 0)?;
    let fmt = out_format(&out)?;
    let kind = if a.choice("solfa") == "fixed" { SolfaKind::Fixed } else { SolfaKind::Movable };
    let paths = write_as(ctx, &l.score, &out, fmt, a.flag("pretty"), kind, &a.str_or("engraver", "auto"), None)?;
    let report = check::check(&l.score, &l.unsupported);
    if !l.unsupported.is_empty() {
        ctx.warn("unsupported_constructs", format!("{} construct(s) were skipped by the reader", l.unsupported.len()));
    }
    Ok(
        json!({"from": l.format.name(), "to": fmt.name(), "outputs": paths, "measures": l.score.measure_count(), "check_ok": report.ok}),
    )
}

fn op_to_json(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let l = load_input(ctx, 0)?;
    let v = serde_json::to_value(&l.score)?;
    if ctx.globals.output.is_some() {
        let out = ctx.output_path(None, "json")?;
        let bytes = if a.flag("pretty") { serde_json::to_vec_pretty(&l.score)? } else { serde_json::to_vec(&l.score)? };
        ctx.write_file(&out, &bytes, Some("json"))?;
        return Ok(
            json!({"output": out.display().to_string(), "parts": l.score.parts.len(), "measures": l.score.measure_count()}),
        );
    }
    Ok(v)
}

fn op_from_json(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let path = ctx.input(0)?.to_path_buf();
    let bytes = read_bytes(ctx, &path)?;
    let score: Score =
        serde_json::from_slice(&bytes).map_err(|e| Error::input(format!("not a ferraus score JSON: {e}")))?;
    let out = ctx.output_path(None, "musicxml")?;
    let fmt = out_format(&out)?;
    let paths = write_as(ctx, &score, &out, fmt, a.flag("pretty"), SolfaKind::Movable, "auto", None)?;
    let report = check::check(&score, &[]);
    Ok(json!({"outputs": paths, "format": fmt.name(), "check_ok": report.ok, "errors": report.errors}))
}

fn op_check(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let l = load_input(ctx, 0)?;
    let report = check::check(&l.score, &l.unsupported);
    let strict = a.flag("strict");
    let v = serde_json::to_value(&report)?;
    if ctx.globals.output.is_some() {
        let out = ctx.output_path(None, "json")?;
        ctx.write_file(&out, &serde_json::to_vec_pretty(&v)?, Some("report"))?;
    }
    if !report.ok || (strict && report.warnings > 0) {
        let msg = if report.ok {
            format!("{} warning(s) with --strict", report.warnings)
        } else {
            check::summary(&report, 5)
        };
        return Err(Error::verification(format!("score check failed: {msg}")).with_hint(
            "fix the listed measures (score read --fix edits.json, or score from-json) and run score check again",
        ));
    }
    Ok(v)
}
