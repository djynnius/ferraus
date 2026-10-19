//! `trim`, `cut`, `split`, and `concat`.

use crate::silence::detect_silence;
use crate::util::{self, Curve};
use ferraus_core::op::Inputs;
use ferraus_core::prelude::*;
use ferraus_core::resample::{resample, Quality};
use ferraus_core::units::Time;

pub fn ops() -> Vec<OpSpec> {
    vec![trim_spec(), cut_spec(), split_spec(), concat_spec()]
}

// ---------------------------------------------------------------------------------------
// trim
// ---------------------------------------------------------------------------------------

pub fn trim_spec() -> OpSpec {
    OpSpec::transform("trim", Category::Edit, "Keep one time range of the input", trim)
        .about(
            "Keeps the part of the input between the two ends of --range and drops the rest. \
             By default each cut point moves to the nearest zero crossing (within 5 ms) so the \
             new edges start and stop cleanly without a click; use --snap off for exact \
             sample positions. --pad adds silence around the result. Times accept seconds, \
             mm:ss, samples, beats (with --bpm), and negative values counted from the end.",
        )
        .param(
            param("range", ParamKind::Range)
                .required()
                .help("Range to keep as start..end, e.g. 0s..30s, 1:00..end, -10s..end, 8b..24b"),
        )
        .param(param("pad", ParamKind::Time).help(
            "Silence added to both ends of the result (duration, e.g. 500ms); --pad-start/--pad-end override one side",
        ))
        .param(param("pad-start", ParamKind::Time).help("Silence added before the result (duration, e.g. 1s)"))
        .param(param("pad-end", ParamKind::Time).help("Silence added after the result (duration, e.g. 2s)"))
        .param(param("snap", ParamKind::Choice).choices(&["zero", "off"]).default("zero").help(
            "zero: move cut points to the nearest zero crossing so the edges never click; off: cut at the exact sample",
        ))
        .param(
            param("snap-window", ParamKind::Time)
                .default("5ms")
                .advanced()
                .help("How far (duration) a cut point may move when snapping to a zero crossing"),
        )
        .no_mix()
        .example("ferraus trim song.wav --range 0:30..1:15 -o chorus.wav", "Keep 0:30 to 1:15, with click-free edges")
        .example(
            "ferraus trim take.wav --range 2.5s..end --pad 250ms -o take_clean.wav",
            "Drop the first 2.5 seconds and add a quarter second of silence at each end",
        )
        .example(
            "ferraus trim loop.wav --range 0b..16b --bpm 128 --snap off -o loop4bars.wav",
            "Keep exactly four bars at 128 BPM, sample-accurate",
        )
}

/// Does the edge of `buf` jump away from silence by more than the signal's own local slope?
/// (If so, starting or stopping playback there would click.)
pub(crate) fn edge_needs_fade(buf: &AudioBuffer, at_start: bool) -> bool {
    let n = buf.frames();
    if n < 2 {
        return n == 1 && buf.peak() > 1e-4;
    }
    let win = ((0.002 * buf.sample_rate as f64) as usize).clamp(2, n);
    buf.data.iter().any(|c| {
        let (edge, seg) = if at_start { (c[0], &c[..win]) } else { (c[n - 1], &c[n - win..]) };
        let slope = seg.windows(2).fold(0.0f32, |m, w| m.max((w[1] - w[0]).abs()));
        edge.abs() > slope * 1.05 + 1e-5
    })
}

fn pad_frames(a: &Args, name: &str, sr: u32, bpm: Option<f64>) -> Result<Option<usize>> {
    if a.has(name) {
        Ok(Some(util::duration_frames(a, name, sr, bpm)?))
    } else {
        Ok(None)
    }
}

/// Prepend and append silence.
pub(crate) fn pad(buf: &AudioBuffer, start: usize, end: usize) -> AudioBuffer {
    if start == 0 && end == 0 {
        return buf.clone();
    }
    let data = buf
        .data
        .iter()
        .map(|c| {
            let mut v = Vec::with_capacity(start + c.len() + end);
            v.resize(start, 0.0);
            v.extend_from_slice(c);
            v.resize(start + c.len() + end, 0.0);
            v
        })
        .collect();
    AudioBuffer { sample_rate: buf.sample_rate, data }
}

fn trim(a: &Args, buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let range = a.range("range")?.ok_or_else(|| Error::usage("trim needs --range start..end"))?;
    let sr = buf.sample_rate;
    let bpm = ctx.bpm();
    let n = buf.frames();
    let both = pad_frames(a, "pad", sr, bpm)?.unwrap_or(0);
    let pad_start = pad_frames(a, "pad-start", sr, bpm)?.unwrap_or(both);
    let pad_end = pad_frames(a, "pad-end", sr, bpm)?.unwrap_or(both);
    if n == 0 {
        ctx.warn("empty_input", "the input has no audio; only padding is written");
        return Ok(pad(&buf, pad_start, pad_end));
    }
    let (mut s, mut e, clamped) = util::resolve_range(&range, sr, n, bpm)?;
    if clamped {
        ctx.warn(
            "range_clamped",
            format!("--range reaches past the end of the input; keeping up to {:.3} s", n as f64 / sr as f64),
        );
    }
    let snap = a.choice("snap") != "off";
    let mut edge_fades = Vec::new();
    if snap {
        let w = util::duration_frames(a, "snap-window", sr, bpm)?;
        let (s0, e0) = (s, e);
        if s > 0 {
            s = util::snap_to_zero(&buf, s, w);
        }
        if e < n {
            e = util::snap_to_zero(&buf, e, w);
        }
        if e < s {
            // a range shorter than the snap window: keep the exact points
            s = s0;
            e = e0;
        }
        ctx.note("start_s", util::round_s(s as f64 / sr as f64));
        ctx.note("end_s", util::round_s(e as f64 / sr as f64));
    }
    let mut out = buf.slice(s, e);
    if snap && !out.is_empty() {
        let fade = ((0.002 * sr as f64) as usize).clamp(1, (out.frames() / 2).max(1));
        if s > 0 && edge_needs_fade(&out, true) {
            util::fade_in(&mut out, fade);
            edge_fades.push("start");
        }
        if e < n && edge_needs_fade(&out, false) {
            util::fade_out(&mut out, fade);
            edge_fades.push("end");
        }
        if !edge_fades.is_empty() {
            ctx.note("edge_fades", &edge_fades);
        }
    }
    if out.is_empty() {
        ctx.warn("empty_result", "the range is empty; only padding is written");
    }
    let out = pad(&out, pad_start, pad_end);
    ctx.metric("duration_s", util::round_s(out.duration_s()));
    Ok(out)
}

// ---------------------------------------------------------------------------------------
// cut
// ---------------------------------------------------------------------------------------

pub fn cut_spec() -> OpSpec {
    OpSpec::transform("cut", Category::Edit, "Remove one or more time ranges and join the rest", cut)
        .about(
            "Deletes each --range and joins what is left with a short equal-power crossfade so \
             the joins are seamless. The result is exactly as long as the input minus the \
             removed time. Overlapping ranges are merged.",
        )
        .param(
            param("range", ParamKind::Range)
                .required()
                .repeatable()
                .help("Range to remove as start..end (repeatable), e.g. 12s..15.5s"),
        )
        .param(
            param("crossfade", ParamKind::Time)
                .default("10ms")
                .help("Crossfade length (duration) at each join; 0ms for a hard splice"),
        )
        .param(
            param("crossfade-curve", ParamKind::Choice)
                .choices(&["equal-power", "linear"])
                .default("equal-power")
                .advanced()
                .help("Crossfade shape: equal-power keeps loudness steady, linear keeps amplitude steady"),
        )
        .no_mix()
        .example(
            "ferraus cut interview.wav --range 1:02..1:09 --range 3:40..3:44 -o interview_edit.wav",
            "Remove two stumbles and splice the rest seamlessly",
        )
        .example(
            "ferraus cut song.wav --range 0s..4b --bpm 120 -o song_nointro.wav",
            "Remove the first beat-bar of a 120 BPM song",
        )
}

fn cut(a: &Args, buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let sr = buf.sample_rate;
    let n = buf.frames();
    let bpm = ctx.bpm();
    let ranges = a.ranges("range")?;
    if ranges.is_empty() {
        return Err(Error::usage("cut needs at least one --range start..end"));
    }
    if n == 0 {
        ctx.warn("empty_input", "the input has no audio");
        return Ok(buf);
    }
    let mut frames = Vec::new();
    for r in &ranges {
        let (s, e, clamped) = util::resolve_range(r, sr, n, bpm)?;
        if clamped {
            ctx.warn("range_clamped", "a --range reaches past the end of the input and was clamped");
        }
        frames.push((s, e));
    }
    let merged = util::merge_ranges(frames);
    let removed: usize = merged.iter().map(|(s, e)| e - s).sum();
    if removed >= n {
        return Err(Error::usage("the --range values remove the whole input"));
    }
    let xfade = util::duration_frames(a, "crossfade", sr, bpm)?;
    let curve = Curve::parse(&a.choice("crossfade-curve"));
    let (mut out, joins) = util::remove_ranges(&buf, &merged, xfade, curve);
    // new edges created by cutting the start or end
    let fade = (xfade / 2).clamp(1, (out.frames() / 2).max(1));
    if merged.first().map(|r| r.0 == 0).unwrap_or(false) && edge_needs_fade(&out, true) {
        util::fade_in(&mut out, fade);
    }
    if merged.last().map(|r| r.1 == n).unwrap_or(false) && edge_needs_fade(&out, false) {
        util::fade_out(&mut out, fade);
    }
    if joins.iter().any(|&j| j < xfade.saturating_sub(1)) {
        ctx.warn("crossfade_shortened", "a crossfade was shortened because a kept piece is shorter than the crossfade");
    }
    ctx.note(
        "removed",
        merged
            .iter()
            .map(|(s, e)| {
                json!({"start_s": util::round_s(*s as f64 / sr as f64), "end_s": util::round_s(*e as f64 / sr as f64)})
            })
            .collect::<Vec<_>>(),
    );
    ctx.metric("removed_s", util::round_s(removed as f64 / sr as f64));
    ctx.metric("duration_s", util::round_s(out.duration_s()));
    Ok(out)
}

// ---------------------------------------------------------------------------------------
// split
// ---------------------------------------------------------------------------------------

pub fn split_spec() -> OpSpec {
    OpSpec::custom(
        "split",
        Category::Edit,
        "Split a file into parts at times, at silences, or into equal chunks",
        split,
    )
    .inputs(Inputs::ONE_AUDIO)
    .about(
        "Writes one file per part, named <input>.001.<ext>, <input>.002.<ext>, ... (or from \
             an -o template using {stem} for the part number). Split points come from --at \
             (repeatable), --every (equal chunks), and/or --on-silence (the middle of each \
             silent gap); several sources are combined. Points snap to zero crossings so no \
             part starts or ends with a click. Nothing is lost: the parts join back into the \
             input exactly.",
    )
    .param(param("at", ParamKind::Time).repeatable().help("Split point (time, repeatable), e.g. 1:30 or 64b"))
    .param(param("every", ParamKind::Time).help("Split into equal chunks of this duration, e.g. 30s"))
    .param(param("on-silence", ParamKind::Flag).help("Split in the middle of each silent gap"))
    .param(
        param("silence-threshold", ParamKind::Gain)
            .default("-40dB")
            .help("Level (dBFS) below which audio counts as silence for --on-silence"),
    )
    .param(
        param("min-silence", ParamKind::Time)
            .default("500ms")
            .help("Shortest gap (duration) that counts as a split point for --on-silence"),
    )
    .param(
        param("snap", ParamKind::Choice)
            .choices(&["zero", "off"])
            .default("zero")
            .help("zero: move split points to the nearest zero crossing (within 5 ms); off: exact samples"),
    )
    .example("ferraus split podcast.wav --every 10:00 --outdir parts/", "Cut a long recording into 10-minute files")
    .example(
        "ferraus split side_a.flac --on-silence --min-silence 2s -o 'track{stem}.flac'",
        "Split a vinyl side into tracks at the gaps (track001.flac, track002.flac, ...)",
    )
    .example("ferraus split song.wav --at 0:45 --at 2:10 --json", "Split at two times and report the parts as JSON")
}

fn split(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let buf = ctx.read_audio(0)?;
    let sr = buf.sample_rate;
    let n = buf.frames();
    let bpm = ctx.bpm();
    let at = a.all("at");
    let every = a.has("every");
    let on_silence = a.flag("on-silence");
    if at.is_empty() && !every && !on_silence {
        return Err(
            Error::usage("split needs --at, --every, or --on-silence").with_hint("ferraus split <input> --every 30s")
        );
    }
    let mut points: Vec<usize> = Vec::new();
    for s in &at {
        let t = Time::parse(s).map_err(|e| Error::usage(format!("--at: {}", e.message)))?;
        let f = util::unclamped_frames(&t, sr, n, bpm)?;
        if f >= n as f64 || f <= 0.0 {
            ctx.warn(
                "split_point_outside",
                format!("--at {s} is outside the input ({:.3} s) and was ignored", n as f64 / sr as f64),
            );
            continue;
        }
        points.push(f.round() as usize);
    }
    if every {
        let step = util::duration_frames(a, "every", sr, bpm)?;
        if step == 0 {
            return Err(Error::usage("--every must be longer than zero"));
        }
        let mut p = step;
        while p < n {
            points.push(p);
            p += step;
        }
    }
    if on_silence {
        let thr = a.db("silence-threshold")?;
        let min = util::duration_frames(a, "min-silence", sr, bpm)?;
        for r in detect_silence(&buf, thr, min) {
            if r.start == 0 || r.end >= n {
                continue; // leading/trailing silence is not a gap between parts
            }
            points.push((r.start + r.end) / 2);
        }
    }
    if a.choice("snap") != "off" {
        let w = (0.005 * sr as f64).round() as usize;
        for p in &mut points {
            *p = util::snap_to_zero(&buf, *p, w);
        }
    }
    points.retain(|&p| p > 0 && p < n);
    points.sort_unstable();
    points.dedup();
    let mut bounds = vec![0];
    bounds.extend(points.iter().copied());
    bounds.push(n);
    let ext = util::audio_ext(ctx);
    let mut parts = Vec::new();
    for (i, w) in bounds.windows(2).enumerate() {
        let (s, e) = (w[0], w[1]);
        let role = format!("{:03}", i + 1);
        let path = ctx.output_path(Some(&role), &ext)?;
        let part = util::conform(buf.slice(s, e), ctx);
        ctx.write_audio(&path, &part, Some(&role))?;
        parts.push(json!({
            "index": i + 1,
            "path": path.display().to_string(),
            "start_s": util::round_s(s as f64 / sr as f64),
            "end_s": util::round_s(e as f64 / sr as f64),
            "duration_s": util::round_s((e - s) as f64 / sr as f64),
        }));
    }
    ctx.metric("parts", parts.len());
    Ok(json!({ "count": parts.len(), "parts": parts }))
}

// ---------------------------------------------------------------------------------------
// concat
// ---------------------------------------------------------------------------------------

pub fn concat_spec() -> OpSpec {
    OpSpec::custom("concat", Category::Edit, "Join files end to end, optionally with crossfades or gaps", concat)
        .inputs(Inputs::MANY_AUDIO)
        .about(
            "Joins the inputs in the order given. --crossfade overlaps the end of each file with \
             the start of the next (each overlap shortens the result by the crossfade length); \
             --gap inserts silence instead. With --match on (the default) every input is \
             resampled and remixed to the first file's sample rate and channel count.",
        )
        .param(
            param("crossfade", ParamKind::Time)
                .default("0ms")
                .help("Overlap (duration) between consecutive files, e.g. 2s"),
        )
        .param(
            param("crossfade-curve", ParamKind::Choice)
                .choices(&["equal-power", "linear"])
                .default("equal-power")
                .help("Crossfade shape: equal-power keeps perceived loudness constant through the overlap"),
        )
        .param(param("gap", ParamKind::Time).default("0ms").help("Silence (duration) inserted between files"))
        .param(
            param("match", ParamKind::Switch).default("on").help(
                "Resample and remix inputs to the first file's sample rate and channels; off fails on a mismatch",
            ),
        )
        .example("ferraus concat intro.wav body.wav outro.wav -o episode.wav", "Join three files back to back")
        .example(
            "ferraus concat a.flac b.mp3 --crossfade 3s -o mix.flac",
            "DJ-style 3-second equal-power crossfade; b.mp3 is converted to a.flac's format",
        )
        .example(
            "ferraus concat take*.wav --gap 1s -o all_takes.wav",
            "Join every take with a second of silence between them",
        )
}

fn concat(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let count = ctx.inputs.len();
    if count == 0 {
        return Err(Error::usage("concat needs at least one input"));
    }
    let mut bufs = Vec::with_capacity(count);
    for i in 0..count {
        bufs.push(ctx.read_audio(i)?);
    }
    let sr = bufs[0].sample_rate;
    let ch = bufs[0].channels();
    let matching = a.switch("match", true);
    for (i, b) in bufs.iter_mut().enumerate().skip(1) {
        if b.sample_rate != sr || b.channels() != ch {
            if !matching {
                return Err(Error::usage(format!(
                    "input {} is {} Hz / {} ch but the first is {} Hz / {} ch",
                    i + 1,
                    b.sample_rate,
                    b.channels(),
                    sr,
                    ch
                ))
                .with_hint("pass --match on to convert inputs automatically"));
            }
            if b.sample_rate != sr {
                *b = resample(b, sr, Quality::High);
            }
            if b.channels() != ch {
                *b = b.with_channels(ch);
            }
            ctx.note(&format!("converted_input_{}", i + 1), json!({"sample_rate": sr, "channels": ch}));
        }
    }
    let bpm = ctx.bpm();
    let xfade = util::duration_frames(a, "crossfade", sr, bpm)?;
    let gap = util::duration_frames(a, "gap", sr, bpm)?;
    if xfade > 0 && gap > 0 {
        return Err(Error::usage("--crossfade and --gap cannot both be used; pick one"));
    }
    let curve = Curve::parse(&a.choice("crossfade-curve"));
    let (out, joins) = concat_buffers(&bufs, xfade, gap, curve);
    if joins.iter().any(|&(_, l)| l < xfade) {
        ctx.warn(
            "crossfade_shortened",
            "a crossfade was shortened to the length of a file that is shorter than --crossfade",
        );
    }
    let ext = util::audio_ext(ctx);
    let path = ctx.output_path(None, &ext)?;
    let out = util::conform(out, ctx);
    ctx.write_audio(&path, &out, None)?;
    ctx.metric("duration_s", util::round_s(out.duration_s()));
    Ok(json!({
        "inputs": count,
        "duration_s": util::round_s(out.duration_s()),
        "joins": joins.iter().map(|(at, l)| json!({
            "at_s": util::round_s(*at as f64 / sr as f64),
            "crossfade_s": util::round_s(*l as f64 / sr as f64),
        })).collect::<Vec<_>>(),
    }))
}

/// Join buffers (all at the same rate and channel count). Returns the output and, per join,
/// (start frame of the join in the output, crossfade frames used).
pub fn concat_buffers(
    bufs: &[AudioBuffer],
    xfade: usize,
    gap: usize,
    curve: Curve,
) -> (AudioBuffer, Vec<(usize, usize)>) {
    let mut out = bufs[0].clone();
    let mut joins = Vec::new();
    for b in &bufs[1..] {
        if gap > 0 {
            out.resize(out.frames() + gap);
        }
        let l = xfade.min(out.frames()).min(b.frames());
        let start = out.frames() - l;
        for (c, oc) in out.data.iter_mut().enumerate() {
            let src = &b.data[c.min(b.channels() - 1)];
            for i in 0..l {
                let t = (i as f64 + 0.5) / l as f64;
                let (go, gi) = curve.gains(t);
                oc[start + i] = oc[start + i] * go + src[i] * gi;
            }
            oc.extend_from_slice(&src[l..]);
        }
        joins.push((start, l));
    }
    (out, joins)
}
