//! `master --stems`: best-effort stem-aware mastering when only a mix exists. Runs
//! `separate --tier quality`, gives each stem a short corrective chain, and sums the stems
//! back into a mix that the normal chain then masters.

use crate::chain::band_p90_db;
use crate::plan::{run_stage, Stage};
use crate::util::fmt_db;
use ferraus_core::exec::run_nested;
use ferraus_core::prelude::*;
use std::path::Path;

fn stem_stages(stem: &str, buf: &AudioBuffer) -> Vec<Stage> {
    let p90 = band_p90_db(buf, &[])[0];
    let thr = |depth: f64| fmt_db((p90 - depth).clamp(-60.0, -6.0).round());
    match stem {
        "vocals" => vec![
            Stage::new("stems", "deess", vec![], "vocals: tame sibilance before the master limiter exaggerates it"),
            Stage::new(
                "stems",
                "compress",
                vec![
                    ("threshold", thr(4.0)),
                    ("ratio", "2:1".into()),
                    ("attack", "10ms".into()),
                    ("release", "120ms".into()),
                    ("makeup", "0dB".into()),
                ],
                "vocals: steady the level so the voice sits on top",
            ),
        ],
        "drums" => vec![Stage::new(
            "stems",
            "compress",
            vec![
                ("threshold", thr(3.0)),
                ("ratio", "2:1".into()),
                ("attack", "25ms".into()),
                ("release", "80ms".into()),
                ("detector", "peak".into()),
                ("makeup", "0dB".into()),
            ],
            "drums: slow attack keeps the transients, controls the body",
        )],
        "bass" => {
            let mut v = Vec::new();
            if buf.channels() >= 2 {
                v.push(Stage::new(
                    "stems",
                    "widen",
                    vec![("amount", "0%".into()), ("mono-below", "0Hz".into())],
                    "bass: mono for a solid, mono-compatible low end",
                ));
            }
            v.push(Stage::new(
                "stems",
                "compress",
                vec![
                    ("threshold", thr(3.0)),
                    ("ratio", "2:1".into()),
                    ("attack", "30ms".into()),
                    ("release", "200ms".into()),
                    ("makeup", "0dB".into()),
                ],
                "bass: even out note-to-note level",
            ));
            v
        }
        _ => {
            if buf.channels() >= 2 {
                vec![Stage::new(
                    "stems",
                    "widen",
                    vec![("amount", "110%".into()), ("mono-below", "200Hz".into())],
                    "other: a little extra width around the centre elements",
                )]
            } else {
                Vec::new()
            }
        }
    }
}

/// Separate, process each stem, and sum. Errors from `separate` (missing command, model not
/// cached) are returned unchanged.
pub fn stem_aware_mix(ctx: &mut OpCtx, input: &Path, mix: &AudioBuffer) -> Result<(AudioBuffer, Value)> {
    let base = ctx.output_path(None, "wav")?;
    let dir = base.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    let tmp = dir.join(format!(".ferraus-master-stems-{}", std::process::id()));
    if !ctx.globals.dry_run {
        std::fs::create_dir_all(&tmp)?;
    }
    let template = tmp.join("{stem}.wav");
    let saved = ctx.globals.overwrite;
    ctx.globals.overwrite = true;
    let res = run_nested(ctx, "separate", &[("tier", "quality")], vec![input.to_path_buf()], Some(&template));
    ctx.globals.overwrite = saved;
    let cleanup = |outs: &[ferraus_core::ctx::OutputRecord]| {
        for o in outs {
            let _ = std::fs::remove_file(&o.path);
            let _ = std::fs::remove_file(format!("{}.ferraus.json", o.path));
        }
        let _ = std::fs::remove_dir_all(&tmp);
    };
    let (_, outs) = match res {
        Ok(r) => r,
        Err(e) => {
            cleanup(&[]);
            return Err(e);
        }
    };
    // the stems are temporary: do not report them as outputs of `master`
    ctx.outputs.retain(|o| !outs.iter().any(|x| x.path == o.path));
    let mut sum = AudioBuffer::new(mix.sample_rate, mix.channels(), mix.frames());
    let mut report = Vec::new();
    for o in &outs {
        let path = Path::new(&o.path);
        let stem = o
            .role
            .clone()
            .unwrap_or_else(|| path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default());
        let (mut buf, _) = match ctx.io.read(path) {
            Ok(r) => r,
            Err(e) => {
                cleanup(&outs);
                return Err(e);
            }
        };
        if buf.sample_rate != mix.sample_rate {
            buf = ferraus_core::resample::resample(&buf, mix.sample_rate, ferraus_core::resample::Quality::High);
        }
        let stages = stem_stages(&stem, &buf);
        for s in &stages {
            buf = run_stage(ctx, s, buf)?.0;
        }
        let buf = buf.with_channels(mix.channels());
        for (o, s) in sum.data.iter_mut().zip(&buf.data) {
            for (x, y) in o.iter_mut().zip(s) {
                *x += *y;
            }
        }
        report.push(json!({"stem": stem, "stages": stages.iter().map(|s| s.fx_string()).collect::<Vec<_>>()}));
    }
    cleanup(&outs);
    if report.is_empty() {
        return Err(Error::processing("separate produced no stems"));
    }
    Ok((sum, json!({"best_effort": true, "separator": "separate --tier quality", "stems": report})))
}
