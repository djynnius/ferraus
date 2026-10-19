//! `hum`: one-shot hum-to-instrument. Transcribes a voice recording in mono, quantizes it
//! (`midi quantize --grid 1/16 --strength 80%` by default), writes the `.mid` beside the
//! output so it can be edited and re-rendered, and renders it with the named sound through
//! the `render` command (looked up in the registry at run time, so this crate does not
//! depend on `ferraus-synth`).

use crate::edit::{self, QuantizeOptions};
use crate::transcribe::{self, Mode, QuantizeSetting};
use ferraus_core::ctx::OutputRecord;
use ferraus_core::exec::run_nested;
use ferraus_core::prelude::*;
use std::path::PathBuf;

pub fn ops() -> Vec<OpSpec> {
    vec![OpSpec::custom("hum", Category::Midi, "Turn a hummed idea into an instrument part", hum_op)
        .about("Runs 'transcribe' in mono mode on a voice recording, quantizes the notes ('midi quantize --grid 1/16 --strength 80%'), and renders them with the named sound ('render --instrument'). The intermediate MIDI file is written beside the audio output (idea.wav → idea.mid) so the part can be edited and re-rendered with 'ferraus render idea.mid --instrument strings'. Sounds are found the same way as for render: installed sounds and --sound-path directories first, then General MIDI names played by gm-basic or the built-in synth.")
        .inputs(Inputs::ONE_AUDIO)
        .param(param("to", ParamKind::Str).default("piano").help("Instrument to render with: piano, strings, \"Acoustic Bass\", gm:48, sf2:<path>, an installed sound name (as render --instrument)"))
        .param(param("grid", ParamKind::Grid).default("1/16").help("Quantize grid (1/16, 1/8, 1/8T), auto to detect, or off"))
        .param(param("strength", ParamKind::Percent).default("80%").range("0..100%").help("How far notes move toward the grid (80% keeps some feel)"))
        .param(param("bpm", ParamKind::Str).default("auto").help("Tempo in BPM, or auto to detect it from the humming (also set by the global --bpm)"))
        .param(param("key-snap", ParamKind::Switch).default("on").help("Snap out-of-key notes to the detected key when its confidence is above 0.6"))
        .param(param("octave", ParamKind::Str).default("auto").help("auto moves the part into the instrument's comfortable range; or a fixed shift in octaves (-1, +1)"))
        .param(param("min-note", ParamKind::Time).default("60ms").help("Shortest note kept (60ms)"))
        .param(param("onset-sensitivity", ParamKind::Percent).default("50%").range("0..100%").help("How readily repeated or sliding notes are split (see transcribe)"))
        .param(param("tail", ParamKind::Time).default("2s").help("Time rendered after the last note so it rings out (2s)"))
        .example("ferraus hum voice.wav --to piano -o idea.wav", "Hum to piano; also writes idea.mid")
        .example("ferraus hum voice.wav --to strings --grid 1/8 --strength 100% -o strings.wav", "Strings, tightly quantized to eighths")
        .example("ferraus hum bass-idea.wav --to \"Acoustic Bass\" --bpm 92 -o bass.wav", "A bass line at a fixed tempo (moved into the bass range automatically)")]
}

fn hum_op(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let has_render = ctx.registry.map(|r| r.get("render").is_some()).unwrap_or(false);
    if !has_render {
        return Err(Error::unsupported(
            "hum needs the 'render' command (ferraus-synth) to play the transcribed part, and it is not available in this build; use 'ferraus transcribe' and 'ferraus midi quantize' to get the MIDI part",
        )
        .with_hint("ferraus transcribe voice.wav -o idea.mid"));
    }
    let to = a.str_or("to", "piano");
    let mut o = transcribe::options_from_args(a, ctx)?;
    o.mode = Mode::Mono;
    o.quantize = QuantizeSetting::Off;
    if let Some(p) = transcribe::program_for_instrument(&to) {
        o.program = p;
    }
    let grid_raw = a.str_or("grid", "1/16").trim().to_ascii_lowercase();
    let strength = a.percent("strength")?;
    let tail = a.str_or("tail", "2s");

    let ext = ctx.globals.format.clone().unwrap_or_else(|| "wav".into());
    let out = ctx.output_path(None, &ext)?;
    let mid_path: PathBuf =
        if out.as_os_str() == "-" { ctx.output_path(Some("midi"), "mid")? } else { out.with_extension("mid") };
    if mid_path == out {
        return Err(Error::usage(
            "the audio output must not have a .mid extension (the MIDI part is written beside it)",
        ));
    }
    if ctx.globals.dry_run {
        ctx.prepare_output(&mid_path)?;
        ctx.record_output(&mid_path, Some("midi"));
        ctx.prepare_output(&out)?;
        ctx.outputs.push(OutputRecord {
            path: out.display().to_string(),
            role: None,
            duration_s: None,
            sample_rate: None,
            channels: None,
            extra: Default::default(),
        });
        return Ok(
            json!({"dry_run": true, "midi": mid_path.display().to_string(), "audio": out.display().to_string(), "instrument": to}),
        );
    }
    // fail before any work if the audio output can't be written
    ctx.prepare_output(&out)?;

    let buf = ctx.read_audio(0)?;
    let mut t = transcribe::transcribe(&buf, &o)?;
    ctx.set_progress(0.6);
    transcribe::record(ctx, &t);
    let grid_used = if grid_raw == "off" || t.midi.note_count() == 0 {
        None
    } else {
        let qo = QuantizeOptions {
            grid: if grid_raw == "auto" { None } else { a.grid("grid")? },
            strength,
            seed: ctx.seed(),
            ..Default::default()
        };
        let rep = edit::quantize(&mut t.midi, &qo)?;
        Some(rep.grid)
    };
    let bytes = transcribe::midi_bytes(ctx, &mid_path, &t.midi);
    ctx.write_file(&mid_path, &bytes, Some("midi"))?;
    if t.midi.note_count() == 0 {
        ctx.warn("no_notes", "no notes were found in the recording; the rendered audio is silent apart from the tail");
    }
    ctx.set_progress(0.7);
    let (render_result, outs) = run_nested(
        ctx,
        "render",
        &[("instrument", to.as_str()), ("tail", tail.as_str())],
        vec![mid_path.clone()],
        Some(&out),
    )?;
    // the nested render recorded its output on the child context; carry it over
    for r in outs {
        ctx.progress.output(&r.path, r.role.as_deref());
        ctx.outputs.push(r);
    }
    let mut v = transcribe::summary(&t);
    v["midi"] = json!(mid_path.display().to_string());
    v["audio"] = json!(out.display().to_string());
    v["instrument"] = json!(to);
    v["quantize_grid"] = json!(grid_used);
    v["quantize_strength"] = json!(strength);
    v["render"] = render_result;
    Ok(v)
}
