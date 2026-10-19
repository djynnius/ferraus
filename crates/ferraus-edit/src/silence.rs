//! `silence detect`, `silence trim`, `silence pad`, and the shared silence detector.

use crate::range_ops::pad;
use crate::util::{self, Curve};
use ferraus_core::op::Inputs;
use ferraus_core::prelude::*;
use serde::Serialize;

pub fn ops() -> Vec<OpSpec> {
    vec![detect_spec(), trim_spec(), pad_spec()]
}

/// One silent region.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SilenceRegion {
    /// First silent frame.
    #[serde(skip)]
    pub start: usize,
    /// One past the last silent frame.
    #[serde(skip)]
    pub end: usize,
    pub start_s: f64,
    pub end_s: f64,
    pub duration_s: f64,
    /// Average level of the region in dBFS (loudest channel).
    pub floor_db: f64,
}

/// Find regions quieter than `threshold_db` (dBFS RMS over a 10 ms window, loudest channel)
/// lasting at least `min_frames`. Region edges have 1 ms resolution and are conservative:
/// a region never includes audio above the threshold.
pub fn detect_silence(buf: &AudioBuffer, threshold_db: f64, min_frames: usize) -> Vec<SilenceRegion> {
    let n = buf.frames();
    let sr = buf.sample_rate.max(1);
    if n == 0 {
        return Vec::new();
    }
    let hop = ((sr as f64 / 1000.0).round() as usize).max(1);
    let blocks = n.div_ceil(hop);
    // energy per block per channel
    let energy: Vec<Vec<f64>> = buf
        .data
        .iter()
        .map(|c| c.chunks(hop).map(|b| b.iter().map(|v| (*v as f64) * (*v as f64)).sum()).collect())
        .collect();
    let block_len = |j: usize| -> usize { hop.min(n - j * hop) };
    let (before, after) = (4usize, 5usize); // 10-block window centred on the block
    let thr = 10f64.powf(threshold_db / 10.0); // power threshold
    let mut silent = vec![false; blocks];
    for (j, s) in silent.iter_mut().enumerate() {
        let lo = j.saturating_sub(before);
        let hi = (j + after).min(blocks - 1);
        let count: usize = (lo..=hi).map(block_len).sum();
        let loudest = energy.iter().map(|e| e[lo..=hi].iter().sum::<f64>() / count.max(1) as f64).fold(0.0, f64::max);
        *s = loudest < thr;
    }
    let mut out = Vec::new();
    let mut j = 0;
    while j < blocks {
        if !silent[j] {
            j += 1;
            continue;
        }
        let j0 = j;
        while j < blocks && silent[j] {
            j += 1;
        }
        let start = j0 * hop;
        let end = (j * hop).min(n);
        if end - start >= min_frames.max(1) {
            let floor = buf
                .data
                .iter()
                .map(|c| c[start..end].iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / (end - start) as f64)
                .fold(0.0, f64::max);
            out.push(SilenceRegion {
                start,
                end,
                start_s: util::round_s(start as f64 / sr as f64),
                end_s: util::round_s(end as f64 / sr as f64),
                duration_s: util::round_s((end - start) as f64 / sr as f64),
                floor_db: util::round_db(10.0 * floor.max(1e-24).log10()),
            });
        }
    }
    out
}

// ---------------------------------------------------------------------------------------
// silence detect
// ---------------------------------------------------------------------------------------

pub fn detect_spec() -> OpSpec {
    OpSpec::custom(
        "silence detect",
        Category::Edit,
        "Find silent regions and report their start, end, and level",
        detect,
    )
    .inputs(Inputs::ONE_AUDIO)
    .about(
        "Lists every stretch quieter than --threshold that lasts at least --min, with its \
             start, end, duration, and average level. Nothing is changed. --markers also writes \
             the regions as a marker file (CSV for editors and spreadsheets, or JSON) named \
             <input>.silence-detect.csv unless -o says otherwise.",
    )
    .param(
        param("threshold", ParamKind::Gain)
            .default("-40dB")
            .help("Level (dBFS, RMS over 10 ms) below which audio counts as silence"),
    )
    .param(param("min", ParamKind::Time).default("300ms").help("Shortest silence (duration) to report"))
    .param(param("markers", ParamKind::Flag).help("Also write the regions to a marker file"))
    .param(
        param("markers-format", ParamKind::Choice)
            .choices(&["csv", "json"])
            .default("csv")
            .help("Marker file format when -o has no .csv/.json extension"),
    )
    .example("ferraus silence detect interview.wav --json", "List pauses of 300 ms or more below -40 dBFS")
    .example(
        "ferraus silence detect lecture.mp3 --threshold -50dB --min 2s --markers -o pauses.csv",
        "Write long pauses to a CSV marker file",
    )
    .example(
        "ferraus silence detect podcast.wav --min 1s --markers --markers-format json",
        "Write pauses of a second or more to a JSON marker file next to the input",
    )
    .example(
        "ferraus silence detect take.wav --threshold -60dB --min 100ms --json",
        "Find even short, very quiet gaps, such as the breaths between words of a clean studio take",
    )
}

fn detect(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let buf = ctx.read_audio(0)?;
    let sr = buf.sample_rate;
    let thr = a.db("threshold")?;
    let min = util::duration_frames(a, "min", sr, ctx.bpm())?;
    let regions = detect_silence(&buf, thr, min);
    let total: f64 = regions.iter().map(|r| r.duration_s).sum();
    ctx.metric("regions", regions.len());
    ctx.metric("total_silence_s", util::round_s(total));
    if a.flag("markers") {
        let fmt = a.choice("markers-format");
        let path = ctx.output_path(None, &fmt)?;
        let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        let as_json = match ext.as_str() {
            "json" => true,
            "csv" | "txt" | "tsv" => false,
            _ => fmt == "json",
        };
        let bytes = if as_json {
            serde_json::to_vec_pretty(&json!({ "markers": markers_json(&regions) }))?
        } else {
            markers_csv(&regions).into_bytes()
        };
        ctx.write_file(&path, &bytes, Some("markers"))?;
    }
    Ok(json!({
        "count": regions.len(),
        "total_silence_s": util::round_s(total),
        "duration_s": util::round_s(buf.duration_s()),
        "regions": regions,
    }))
}

fn markers_json(regions: &[SilenceRegion]) -> Vec<Value> {
    regions
        .iter()
        .enumerate()
        .map(|(i, r)| {
            json!({"name": format!("Silence {}", i + 1), "start_s": r.start_s, "end_s": r.end_s, "duration_s": r.duration_s, "floor_db": r.floor_db})
        })
        .collect()
}

/// Marker CSV: one row per region (seconds), readable by spreadsheets and DAW importers.
pub fn markers_csv(regions: &[SilenceRegion]) -> String {
    let mut s = String::from("name,start_s,end_s,duration_s,floor_db\n");
    for (i, r) in regions.iter().enumerate() {
        s.push_str(&format!(
            "Silence {},{:.6},{:.6},{:.6},{:.2}\n",
            i + 1,
            r.start_s,
            r.end_s,
            r.duration_s,
            r.floor_db
        ));
    }
    s
}

// ---------------------------------------------------------------------------------------
// silence trim
// ---------------------------------------------------------------------------------------

pub fn trim_spec() -> OpSpec {
    OpSpec::transform(
        "silence trim",
        Category::Edit,
        "Remove leading and trailing silence, optionally long pauses too",
        silence_trim,
    )
    .about(
        "Removes silence below --threshold from the start and end, leaving --keep of it so \
             the sound does not start abruptly (--edges, the default). With --all, pauses inside \
             the audio longer than --min are also shortened to --max-gap, joined with a short \
             crossfade (Audition's Delete Silence).",
    )
    .param(param("edges", ParamKind::Flag).help("Trim leading and trailing silence (the default behaviour)"))
    .param(param("all", ParamKind::Flag).help("Also shorten long silences inside the audio"))
    .param(
        param("threshold", ParamKind::Gain).default("-60dB").help("Level (dBFS) below which audio counts as silence"),
    )
    .param(
        param("keep", ParamKind::Time)
            .default("10ms")
            .help("Silence (duration) kept before the first and after the last sound"),
    )
    .param(
        param("min", ParamKind::Time)
            .default("1s")
            .help("With --all: inner silences longer than this (duration) are shortened"),
    )
    .param(
        param("max-gap", ParamKind::Time)
            .default("500ms")
            .help("With --all: length (duration) inner silences are shortened to"),
    )
    .no_mix()
    .example("ferraus silence trim take.wav -o take_trimmed.wav", "Strip dead air from both ends, keeping 10 ms")
    .example(
        "ferraus silence trim podcast.wav --all --threshold -50dB --max-gap 700ms -o tight.wav",
        "Also shorten every pause longer than 1 s to 700 ms",
    )
    .example(
        "ferraus silence trim sample.wav --threshold -50dB --keep 0ms -o sample_tight.wav",
        "Cut a one-shot sample right at its first and last sound so it plays instantly",
    )
    .example(
        "ferraus silence trim lecture.wav --all --min 3s --max-gap 1s -o lecture_tight.wav",
        "Keep natural pauses but shorten any silence over 3 s down to 1 s",
    )
}

fn silence_trim(a: &Args, buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let sr = buf.sample_rate;
    let bpm = ctx.bpm();
    let n = buf.frames();
    if n == 0 {
        ctx.warn("empty_input", "the input has no audio");
        return Ok(buf);
    }
    let thr_db = a.db("threshold")?;
    let thr = db_to_lin(thr_db) as f32;
    let keep = util::duration_frames(a, "keep", sr, bpm)?;
    // the same 10 ms RMS measure as `silence detect`, so both agree on what is silent
    let regions = detect_silence(&buf, thr_db, (0.01 * sr as f64) as usize);
    if regions.len() == 1 && regions[0].start == 0 && regions[0].end >= n {
        ctx.warn("all_silent", format!("nothing in the input is above {thr_db} dB; it was left unchanged"));
        return Ok(buf);
    }
    // locate edges with the windowed measure, then refine to the first/last sample above the
    // threshold within one window of that boundary (sample-accurate, noise-robust)
    let win = (0.01 * sr as f64) as usize;
    let first = match regions.iter().find(|r| r.start == 0) {
        Some(r) => {
            let from = r.end.saturating_sub(win);
            (from..n).find(|&i| util::frame_mag(&buf, i) > thr).unwrap_or(r.end).min(r.end + win)
        }
        None => 0,
    };
    let last = match regions.iter().find(|r| r.end >= n && r.start > 0) {
        Some(r) => {
            let to = (r.start + win).min(n);
            (0..to)
                .rev()
                .find(|&i| util::frame_mag(&buf, i) > thr)
                .unwrap_or(r.start.saturating_sub(1))
                .max(r.start.saturating_sub(win))
        }
        None => n - 1,
    };
    if first > last {
        ctx.warn("all_silent", format!("nothing in the input is above {thr_db} dB; it was left unchanged"));
        return Ok(buf);
    }
    let s = first.saturating_sub(keep);
    let e = (last + 1 + keep).min(n);
    let mut out = buf.slice(s, e);
    ctx.metric("leading_removed_s", util::round_s(s as f64 / sr as f64));
    ctx.metric("trailing_removed_s", util::round_s((n - e) as f64 / sr as f64));
    if a.flag("all") {
        let min = util::duration_frames(a, "min", sr, bpm)?;
        let gap = util::duration_frames(a, "max-gap", sr, bpm)?;
        let longer_than = min.max(gap) + 1;
        let regions = detect_silence(&out, thr_db, longer_than);
        let ranges: Vec<(usize, usize)> = regions
            .iter()
            .filter(|r| r.start > 0 && r.end < out.frames())
            .map(|r| (r.start + gap / 2, r.end - (gap - gap / 2)))
            .filter(|(a, b)| b > a)
            .collect();
        let removed: usize = ranges.iter().map(|(a, b)| b - a).sum();
        if !ranges.is_empty() {
            let xfade = ((0.005 * sr as f64) as usize).min(gap.max(2));
            out = util::remove_ranges(&out, &ranges, xfade, Curve::EqualPower).0;
        }
        ctx.metric("pauses_shortened", ranges.len());
        ctx.metric("inner_removed_s", util::round_s(removed as f64 / sr as f64));
    }
    ctx.metric("duration_s", util::round_s(out.duration_s()));
    Ok(out)
}

// ---------------------------------------------------------------------------------------
// silence pad
// ---------------------------------------------------------------------------------------

pub fn pad_spec() -> OpSpec {
    OpSpec::transform("silence pad", Category::Edit, "Add silence to the start and/or end", silence_pad)
        .about("Inserts digital silence before and after the audio, e.g. to give a track lead-in time or meet a platform's spacing rules.")
        .param(param("start", ParamKind::Time).default("0s").help("Silence (duration) added before the audio, e.g. 1s or 2b"))
        .param(param("end", ParamKind::Time).default("0s").help("Silence (duration) added after the audio, e.g. 2s"))
        .no_mix()
        .example("ferraus silence pad song.wav --start 1s --end 2s -o song_padded.wav", "One second of lead-in, two of tail")
        .example("ferraus silence pad loop.wav --end 4b --bpm 90 -o loop_tail.wav", "Add one bar of silence at 90 BPM for a reverb tail")
        .example("ferraus silence pad episode.wav --start 500ms -o episode_lead.wav", "Half a second of lead-in so players do not swallow the first word")
        .example("ferraus silence pad tracks/*.wav --end 2s --outdir spaced/", "Add two seconds of tail to every track of an album")
}

fn silence_pad(a: &Args, buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let sr = buf.sample_rate;
    let bpm = ctx.bpm();
    let s = util::duration_frames(a, "start", sr, bpm)?;
    let e = util::duration_frames(a, "end", sr, bpm)?;
    let out = pad(&buf, s, e);
    ctx.metric("duration_s", util::round_s(out.duration_s()));
    Ok(out)
}
