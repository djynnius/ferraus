//! The `render` command.

use crate::render::{plan, render_with_report, RenderOptions, Selector, VelocityCurve};
use crate::source::{sounds_summary, InstrumentSource, Resolved, Resolver};
use ferraus_core::ctx::OutputRecord;
use ferraus_core::op::parse_stage_string;
use ferraus_core::prelude::*;
use ferraus_midi::{gm, MidiFile};
use serde_json::Map;

pub fn ops() -> Vec<OpSpec> {
    vec![OpSpec::custom("render", Category::Midi, "Render MIDI to audio with an instrument", render_op)
        .about("Plays a MIDI file through a SoundFont (sf2, via the pure-Rust rustysynth engine), an SFZ library (native sampler), or the built-in preview synth, which needs no files, so render always works. --instrument takes a name resolved in this order: a 'sounds alias', an installed or --sound-path sound whose name or category matches (strings, nylon guitar), a General MIDI program name or number (\"String Ensemble 1\", gm:48) played by the gm-basic soundfont if installed or else the built-in synth, or an explicit source (sf2:<path>[@bank:program], sfz:<path>, builtin:<timbre>, plugin:\"Name\"). The default, gm, follows the file's own program changes. The output is the MIDI length plus --tail; the resolved instrument is recorded in the sidecar.")
        .inputs(Inputs::ONE_MIDI)
        .param(param("instrument", ParamKind::Str).default("gm").help("Instrument name or source: piano, strings, \"String Ensemble 1\", gm:48, sf2:<path>, sfz:<path>, builtin:organ, plugin:\"Surge XT\"; gm follows the file's programs"))
        .param(param("map", ParamKind::List).repeatable().help("Instruments per track or channel: \"1=piano,2=strings,10=drums\" (t2= forces a track, ch10= a channel; bare numbers are tracks in multitrack files, else channels)"))
        .param(param("swap", ParamKind::List).repeatable().help("Substitute by General MIDI family: piano=strings plays every piano-family channel with strings"))
        .param(param("tail", ParamKind::Time).default("2s").help("Time rendered after the last MIDI event so releases ring out (2s)"))
        .param(param("velocity-curve", ParamKind::Choice).choices(&["linear", "soft", "hard"]).default("linear").help("Velocity response: soft makes quiet notes louder, hard needs more velocity to get loud"))
        .param(param("fx", ParamKind::Str).repeatable().help("Effect stage applied after rendering, like the fx command: \"reverb:mix=0.2\" (repeatable, in order)"))
        .example("ferraus render part.mid -o part.wav --instrument piano", "Render with a piano (installed piano sound, gm-basic, or the built-in synth)")
        .example("ferraus render song.mid --map \"1=piano,2=strings,10=drums\" -o song.wav", "Different instruments per track or channel")
        .example("ferraus render song.mid --swap piano=strings -o strings.wav", "Replace every piano-family part with strings")
        .example("ferraus render idea.mid --instrument \"String Ensemble 1\" --tail 3s --fx \"reverb:mix=0.2\" -o idea.wav", "General MIDI program by name, longer tail, reverb in the same run")]
}

/// Resolve every `--map` entry into assignments.
fn parse_map(entries: &[String], midi: &MidiFile, r: &Resolver) -> Result<Vec<(Selector, Resolved, String)>> {
    let note_tracks = midi.tracks.iter().filter(|t| !t.notes.is_empty()).count();
    let mut out = Vec::new();
    for e in entries {
        let (k, v) = e
            .split_once('=')
            .ok_or_else(|| Error::usage(format!("--map: '{e}' is not N=instrument (1=piano, ch10=drums)")))?;
        let k = k.trim().to_ascii_lowercase();
        let num = |s: &str| -> Result<usize> {
            s.trim_start_matches(':')
                .trim()
                .parse::<usize>()
                .map_err(|_| Error::usage(format!("--map: '{k}' is not a track or channel number")))
        };
        let (sel, how) = if let Some(n) = k.strip_prefix("track").or_else(|| k.strip_prefix('t')) {
            let n = num(n)?;
            if n == 0 || n > midi.tracks.len() {
                return Err(Error::usage(format!(
                    "--map: track {n} does not exist (the file has {} tracks)",
                    midi.tracks.len()
                )));
            }
            (Selector::Track(n - 1), format!("track {n}"))
        } else if let Some(n) =
            k.strip_prefix("channel").or_else(|| k.strip_prefix("ch")).or_else(|| k.strip_prefix('c'))
        {
            let n = num(n)?;
            if !(1..=16).contains(&n) {
                return Err(Error::usage(format!("--map: channel {n} is not 1..16")));
            }
            (Selector::Channel(n as u8 - 1), format!("channel {n}"))
        } else {
            let n = num(&k)?;
            let is_track = note_tracks > 1 && n >= 1 && n <= midi.tracks.len() && !midi.tracks[n - 1].notes.is_empty();
            if is_track {
                (Selector::Track(n - 1), format!("track {n}"))
            } else if (1..=16).contains(&n) {
                (Selector::Channel(n as u8 - 1), format!("channel {n}"))
            } else {
                return Err(Error::usage(format!("--map: '{n}' is neither a track with notes nor a channel 1..16")));
            }
        };
        out.push((sel, r.resolve(v.trim())?, how));
    }
    Ok(out)
}

/// Program in effect at each channel's first note (0-based channel → program).
fn first_programs(midi: &MidiFile) -> Vec<(u8, u8)> {
    let mut out = Vec::new();
    for ch in 0u8..16 {
        let first = midi.tracks.iter().flat_map(|t| t.notes.iter()).filter(|n| n.channel == ch).map(|n| n.tick).min();
        let Some(first) = first else { continue };
        let mut progs: Vec<(u64, u8)> = midi
            .tracks
            .iter()
            .flat_map(|t| t.programs.iter())
            .filter(|p| p.data.channel == ch)
            .map(|p| (p.tick, p.data.program))
            .collect();
        progs.sort();
        let p = progs.iter().rev().find(|(t, _)| *t <= first).or(progs.first()).map(|x| x.1).unwrap_or(0);
        out.push((ch, p));
    }
    out
}

fn parse_swaps(entries: &[String], midi: &MidiFile, r: &Resolver) -> Result<Vec<(Selector, Resolved, String)>> {
    let progs = first_programs(midi);
    let mut out = Vec::new();
    for e in entries {
        let (from, to) =
            e.split_once('=').ok_or_else(|| Error::usage(format!("--swap: '{e}' is not from=to (piano=strings)")))?;
        let target = r.resolve(to.trim())?;
        let fname = gm::normalize(from);
        if matches!(fname.as_str(), "drums" | "drum" | "kit" | "percussion") {
            if progs.iter().any(|(c, _)| *c == 9) {
                out.push((Selector::Channel(9), target.clone(), format!("drums ({e})")));
            }
            continue;
        }
        let fp = gm::lookup_program(from).map_err(|c| {
            if c.is_empty() {
                Error::usage(format!("--swap: '{from}' is not a General MIDI instrument or family"))
            } else {
                Error::usage(format!("--swap: '{from}' matches several General MIDI programs; use a family word such as piano, strings, guitar, bass"))
            }
        })?;
        let fam = gm::family_of(fp);
        for (ch, p) in &progs {
            if *ch != 9 && gm::family_of(*p) == fam {
                out.push((
                    Selector::Channel(*ch),
                    target.clone(),
                    format!("channel {} ({}, {})", ch + 1, gm::program_name(*p), e),
                ));
            }
        }
    }
    Ok(out)
}

fn check_allowed(ctx: &OpCtx, r: &Resolved) -> Result<()> {
    if r.via != "explicit" {
        return Ok(());
    }
    match &r.source {
        InstrumentSource::Sf2 { path, .. } | InstrumentSource::Sfz { path } => ctx.check_allowed(path),
        _ => Ok(()),
    }
}

fn render_op(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let midi = ferraus_midi::ops::load_input(ctx, 0)?;
    let sr = ctx.globals.sample_rate.unwrap_or(48000);
    let resolver = Resolver::discover(&ctx.globals.sound_paths);
    let given = a.str_or("instrument", "gm");
    let main = resolver.resolve(&given)?;
    check_allowed(ctx, &main)?;
    let mut assigned = parse_map(&a.list("map"), &midi, &resolver)?;
    assigned.extend(parse_swaps(&a.list("swap"), &midi, &resolver)?);
    for (_, r, _) in &assigned {
        check_allowed(ctx, r)?;
    }
    let tail = a.seconds("tail")?;
    if tail > 600.0 {
        return Err(Error::usage("--tail is limited to 600s"));
    }
    let opts = RenderOptions {
        sample_rate: sr,
        tail_s: tail,
        velocity_curve: VelocityCurve::parse(&a.choice("velocity-curve"))?,
        assignments: assigned.iter().map(|(s, r, _)| (*s, r.source.clone())).collect(),
    };
    ctx.note("instrument", main.to_json());
    if !assigned.is_empty() {
        let list: Vec<Value> = assigned
            .iter()
            .map(|(_, r, how)| json!({"part": how, "given": r.given, "via": r.via, "source": r.source}))
            .collect();
        ctx.note("instruments", list);
    }
    let ext = ctx.globals.format.clone().unwrap_or_else(|| "wav".into());
    let out = ctx.output_path(None, &ext)?;
    let groups = plan(&midi, &main.source, &opts);
    let plan_json: Vec<Value> =
        groups.iter().map(|g| json!({"source": g.source, "notes": g.notes, "explicit": g.explicit})).collect();
    let midi_len = midi.duration_seconds();
    if ctx.globals.dry_run {
        ctx.prepare_output(&out)?;
        ctx.outputs.push(OutputRecord {
            path: out.display().to_string(),
            role: None,
            duration_s: Some(midi_len + tail),
            sample_rate: Some(sr),
            channels: Some(ctx.globals.channels.unwrap_or(2)),
            extra: Map::new(),
        });
        return Ok(json!({
            "plan": plan_json,
            "midi_duration_s": midi_len,
            "duration_s": midi_len + tail,
            "sounds": sounds_summary(&resolver),
        }));
    }
    ctx.prepare_output(&out)?;
    let (mut buf, rep) = {
        let c: &OpCtx = ctx;
        render_with_report(&midi, &main.source, &opts, &mut |f| {
            c.set_progress(f * 0.95);
            c.check_cancel()
        })?
    };
    if rep.safety_gain < 1.0 {
        ctx.warn(
            "render_attenuated",
            format!(
                "the render peaked at {:.2} and was turned down by {:.1} dB to avoid clipping",
                rep.peak,
                -20.0 * (rep.safety_gain as f64).log10()
            ),
        );
    }
    for stage in a.all("fx") {
        let (name, pairs) = parse_stage_string(&stage)?;
        let refs: Vec<(&str, &str)> = pairs.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        buf = ferraus_core::exec::run_transform(ctx, &name, &refs, buf)?;
    }
    if let Some(ch) = ctx.globals.channels {
        if ch != buf.channels() {
            buf = buf.with_channels(ch);
        }
    }
    let mut eo = ctx.encode_options(&out);
    if eo.bit_depth.is_none() {
        eo.bit_depth = Some(24);
    }
    ctx.write_audio_with(&out, &buf, None, &eo)?;
    let peak = buf.peak().max(1e-12) as f64;
    ctx.metric("duration_s", buf.duration_s());
    ctx.metric("midi_duration_s", midi_len);
    ctx.metric("notes", rep.notes);
    ctx.metric("peak_dbfs", (20.0 * peak.log10() * 100.0).round() / 100.0);
    Ok(json!({
        "output": out.display().to_string(),
        "instrument": main.source.display(),
        "plan": plan_json,
        "duration_s": buf.duration_s(),
        "midi_duration_s": midi_len,
    }))
}
