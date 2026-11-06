//! `patch`: replace one stretch of a file with audio from elsewhere in the same file (a clean
//! repeat of the same part) or from another take, keeping the length and the timing.

use crate::util::{self, Curve};
use ferraus_core::prelude::*;
use ferraus_core::resample::{resample, Quality};

pub fn ops() -> Vec<OpSpec> {
    vec![patch_spec()]
}

pub fn patch_spec() -> OpSpec {
    OpSpec::transform("patch", Category::Edit, "Replace a time range with audio from elsewhere or another take", patch)
        .about(
            "The edit an engineer makes for a bad note, a fret squeak or a noise on one pass of a \
             part that repeats: copy the same bars from a clean pass over it. The replacement \
             starts at --from in the source (the input itself, or --source for another take) and \
             is exactly as long as --range, so nothing after the edit moves. Each edge gets a \
             crossfade centred on it. `analyze sections` lists repeated sections with \
             `offset_s` (start of the later section minus the earlier): a bar at time t in \
             the later pass has its twin at t - offset_s.",
        )
        .param(
            param("range", ParamKind::Range)
                .required()
                .help("The stretch to replace as start..end, e.g. 53.2s..54.4s or 33b..34b"),
        )
        .param(
            param("from", ParamKind::Time)
                .required()
                .help("Where the replacement starts in the source (time, e.g. 21.4s or 17b); it runs as long as --range"),
        )
        .param(
            param("source", ParamKind::Path)
                .help("Take the replacement from this file (another take, a double); default: the input itself"),
        )
        .param(
            param("crossfade", ParamKind::Time)
                .default("10ms")
                .help("Crossfade (duration) centred on each edge of the range; 0ms for a hard splice"),
        )
        .param(
            param("crossfade-curve", ParamKind::Choice)
                .choices(&["equal-power", "linear"])
                .default("equal-power")
                .advanced()
                .help("equal-power for material that differs (another take), linear for near-identical material (the same loop again)"),
        )
        .param(
            param("gain", ParamKind::Gain)
                .default("0dB")
                .help("Level change applied to the replacement, for a pass played louder or softer"),
        )
        .no_mix()
        .example(
            "ferraus patch bass.wav --range 53.2s..54.4s --from 21.4s -o bass_fixed.wav",
            "Replace a bad slide with the same bar from the first chorus",
        )
        .example(
            "ferraus patch bass.wav --range 33b..34b --from 17b --bpm 108 -o bass_fixed.wav",
            "The same edit in beats: bar 9 replaced by bar 5",
        )
        .example(
            "ferraus patch vocal_take2.wav --range 1:12.4..1:13.1 --from 1:12.4 --source vocal_take3.wav -o vocal_comp.wav",
            "Comp one word from another take recorded to the same click",
        )
        .example(
            "ferraus patch guitar.wav --range 40s..41s --from 8s --gain -1.5dB --crossfade 20ms -o guitar_fixed.wav",
            "A repeat played a little louder, turned down to match, with longer crossfades",
        )
}

fn patch(a: &Args, buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let sr = buf.sample_rate;
    let n = buf.frames();
    let bpm = ctx.bpm();
    if n == 0 {
        ctx.warn("empty_input", "the input has no audio");
        return Ok(buf);
    }
    let range = a.range("range")?.ok_or_else(|| Error::usage("patch needs --range start..end"))?;
    let (s, e, clamped) = util::resolve_range(&range, sr, n, bpm)?;
    if clamped {
        ctx.warn("range_clamped", "--range reaches past the end of the input and was clamped");
    }
    let len = e - s;
    if len == 0 {
        return Err(Error::usage("--range is empty: nothing to replace"));
    }
    let src = match a.path("source") {
        Some(p) => {
            let mut b = ctx.read_audio_path(&p)?;
            if b.sample_rate != sr {
                ctx.note("resampled_source_from_hz", b.sample_rate);
                b = resample(&b, sr, Quality::High);
            }
            ctx.note("source", p.display().to_string());
            match_channels(b, buf.channels())?
        }
        None => buf.clone(),
    };
    let m = src.frames();
    let from = a.time("from")?.frames(sr, m, bpm).map_err(|err| Error::usage(format!("--from: {}", err.message)))?;
    if from + len > m {
        return Err(Error::usage(format!(
            "the replacement needs {:.3} s of source from {:.3} s, but the source ends at {:.3} s",
            len as f64 / sr as f64,
            from as f64 / sr as f64,
            m as f64 / sr as f64
        )));
    }
    let gain = a.gain("gain")?.linear() as f32;
    let curve = Curve::parse(&a.choice("crossfade-curve"));
    let xfade = util::duration_frames(a, "crossfade", sr, bpm)?;
    // Half a crossfade either side of each edge, shortened where the input or the source ends,
    // and never longer than the replacement itself.
    let half = (xfade / 2).min(len / 2);
    let head = if s == 0 { 0 } else { half.min(s).min(from) };
    let tail = if e == n { 0 } else { half.min(n - e).min(m - (from + len)) };
    if (s > 0 && head < half) || (e < n && tail < half) {
        ctx.warn("crossfade_shortened", "a crossfade was shortened where the input or the source ends");
    }

    let rms_db = |b: &AudioBuffer, at: usize, g: f32| -> f64 {
        let mut sum = 0.0f64;
        for c in &b.data {
            for x in &c[at..at + len] {
                sum += (*x as f64 * g as f64).powi(2);
            }
        }
        10.0 * (sum / (len * b.channels()).max(1) as f64).max(1e-20).log10()
    };
    let replaced_db = rms_db(&buf, s, 1.0);
    let patch_db = rms_db(&src, from, gain);

    let mut out = buf;
    let src_at = |i: usize| from + i - s; // source frame for output frame i (i >= s - head)
    for (ch, data) in out.data.iter_mut().enumerate() {
        let sc = &src.data[ch];
        // leading crossfade: input fades out, replacement fades in
        for k in 0..2 * head {
            let i = s - head + k;
            let (fo, fi) = curve.gains((k as f64 + 0.5) / (2 * head) as f64);
            data[i] = data[i] * fo + sc[src_at(i)] * gain * fi;
        }
        // the body
        for i in s + head..e - tail {
            data[i] = sc[src_at(i)] * gain;
        }
        // trailing crossfade: replacement fades out, input fades in
        for k in 0..2 * tail {
            let i = e - tail + k;
            let (fo, fi) = curve.gains((k as f64 + 0.5) / (2 * tail) as f64);
            data[i] = sc[src_at(i)] * gain * fo + data[i] * fi;
        }
    }

    let round = |x: f64| (x * 100.0).round() / 100.0;
    let diff = patch_db - replaced_db;
    if diff.abs() > 3.0 && replaced_db > -80.0 && patch_db > -80.0 {
        ctx.warn(
            "level_mismatch",
            format!(
                "the replacement is {:.1} dB {} than the stretch it replaces; if that stretch is representative, set --gain {:+.1}dB",
                diff.abs(),
                if diff > 0.0 { "louder" } else { "quieter" },
                -diff
            ),
        );
    }
    ctx.note(
        "patch",
        json!({
            "start_s": util::round_s(s as f64 / sr as f64),
            "end_s": util::round_s(e as f64 / sr as f64),
            "from_s": util::round_s(from as f64 / sr as f64),
            "crossfade_ms": round((head.max(tail) * 2) as f64 * 1000.0 / sr as f64),
        }),
    );
    ctx.metric("replaced_s", util::round_s(len as f64 / sr as f64));
    ctx.metric("replaced_rms_db", round(replaced_db));
    ctx.metric("patch_rms_db", round(patch_db));
    ctx.metric("level_difference_db", round(diff));
    Ok(out)
}

/// Give the source the input's channel count: mono is copied to every channel, several
/// channels into mono are averaged; other layouts must already match.
fn match_channels(b: AudioBuffer, channels: usize) -> Result<AudioBuffer> {
    let have = b.channels();
    if have == channels {
        return Ok(b);
    }
    if have == 1 {
        let mono = b.data[0].clone();
        return Ok(AudioBuffer { sample_rate: b.sample_rate, data: vec![mono; channels] });
    }
    if channels == 1 {
        return Ok(AudioBuffer::from_mono(b.sample_rate, b.mono_mix()));
    }
    Err(Error::usage(format!(
        "the source has {have} channels and the input {channels}; convert one with `ferraus channels`"
    )))
}
