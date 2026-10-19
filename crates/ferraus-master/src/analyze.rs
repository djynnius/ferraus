//! `master analyze`: the measurement report an agent decides from (PRD "What master analyze
//! reports"), and the before/after summaries in the `master --report`.

use crate::chain::{intensity_bound, resolve_genre, resolve_intensity};
use crate::genres;
use crate::targets::Target;
use crate::util::{jnum, round_to};
use ferraus_analysis::spectrum::{self, ToneBalance};
use ferraus_analysis::{mastering_analysis, MasteringAnalysis};
use ferraus_core::prelude::*;
use ferraus_core::Registry;

/// Inputs to the report.
pub struct AnalyzeRequest<'a> {
    pub buf: &'a AudioBuffer,
    pub path: &'a str,
    pub bit_depth: Option<u16>,
    pub reference: Option<(&'a str, &'a AudioBuffer)>,
    pub genre: &'a str,
    pub target: &'a Target,
    pub registry: Option<&'a Registry>,
}

/// Run the measurements once (shared by `master`, `master plan`, and `master analyze`).
pub fn measure(buf: &AudioBuffer, reference: Option<&AudioBuffer>) -> MasteringAnalysis {
    mastering_analysis(buf, reference)
}

/// Tonal balance against the reference (when given) or the genre curve.
pub fn tone_against(m: &MasteringAnalysis, genre: &str, has_reference: bool) -> (String, ToneBalance) {
    match (&m.tone_balance, has_reference) {
        (Some(t), true) => ("reference".to_string(), t.clone()),
        _ => {
            let g = genres::genre(genre).unwrap_or(&genres::GENRES[0]);
            (format!("genre curve: {}", g.name), spectrum::tone_balance_vs_curve(&m.third_octave, g.curve))
        }
    }
}

/// Convert a suggested fix (`declip`, `highpass:freq=30Hz`, `silence trim --edges`) into an
/// `fx` stage string, and say whether this build has that effect.
pub fn fix_to_stage(registry: Option<&Registry>, fix: &str) -> (String, bool) {
    let fix = fix.trim();
    if fix.is_empty() {
        return (String::new(), false);
    }
    let (name, pairs): (String, Vec<String>) = if fix.contains(" --") || fix.starts_with("--") {
        let toks: Vec<&str> = fix.split_whitespace().collect();
        let n = toks.iter().position(|t| t.starts_with("--")).unwrap_or(toks.len());
        let name = toks[..n].join(" ");
        let mut pairs = Vec::new();
        let mut i = n;
        while i < toks.len() {
            let k = toks[i].trim_start_matches("--");
            if i + 1 < toks.len() && !toks[i + 1].starts_with("--") {
                pairs.push(format!("{k}={}", toks[i + 1]));
                i += 2;
            } else {
                pairs.push(k.to_string());
                i += 1;
            }
        }
        (name, pairs)
    } else if let Some((n, rest)) = fix.split_once(':') {
        (n.trim().to_string(), vec![rest.trim().to_string()])
    } else {
        (fix.to_string(), Vec::new())
    };
    let available = registry.and_then(|r| r.get(&name)).map(|s| s.is_transform()).unwrap_or(false);
    let stage = if pairs.is_empty() { name } else { format!("{name}:{}", pairs.join(",")) };
    (stage, available)
}

fn tone_json(label: &str, t: &ToneBalance) -> Value {
    let band = |b: &ferraus_analysis::spectrum::BandDelta| {
        json!({"center_hz": spectrum::nominal_hz(b.center_hz), "level_db": jnum(b.level_db, 1),
               "reference_db": jnum(b.reference_db, 1), "delta_db": jnum(b.delta_db, 1)})
    };
    json!({
        "against": label,
        "offset_db": jnum(t.offset_db, 2),
        "rms_deviation_db": jnum(t.rms_deviation_db, 2),
        "largest_deviations": t.largest_deviations.iter().map(|b| {
            let mut v = band(b);
            v["direction"] = json!(if b.delta_db > 0.0 { "too much" } else { "too little" });
            v
        }).collect::<Vec<_>>(),
        "bands": t.bands.iter().filter(|b| b.level_db > -140.0).map(band).collect::<Vec<_>>(),
    })
}

/// The full `master analyze` report.
pub fn report(req: &AnalyzeRequest, m: &MasteringAnalysis) -> Value {
    let l = &m.loudness;
    let d = &m.dynamics;
    let st = &m.stereo;
    let genre = resolve_genre(req.genre, m);
    let (label, tone) = tone_against(m, &genre.name, req.reference.is_some());
    let label = match req.reference {
        Some((p, _)) if label == "reference" => format!("reference: {p}"),
        _ => label,
    };
    let t = req.target;
    // headroom to the target: the gain needed, and how much limiting that gain implies
    let gain = t.lufs - l.integrated_lufs;
    let peak_after = l.true_peak_dbtp + gain;
    let limiting = (peak_after - t.true_peak_dbtp).max(0.0);
    let intensity = resolve_intensity(None, m);
    let fits = ["light", "medium", "heavy"].into_iter().find(|i| limiting <= intensity_bound(i)).unwrap_or("none");
    let key = {
        let mono = req.buf.mono_mix();
        ferraus_pitch::key::detect(&mono, req.buf.sample_rate)
    };
    let problems: Vec<Value> = m
        .problems
        .problems
        .iter()
        .map(|p| {
            let (stage, available) = fix_to_stage(req.registry, &p.suggested_fix);
            let mut v = json!({
                "problem": p.problem,
                "severity": p.severity,
                "details": p.details,
                "suggested_fix": stage,
                "fix_available": available,
            });
            if !stage.is_empty() {
                v["command"] = json!(format!("ferraus fx {} -o fixed.wav --fx \"{stage}\"", req.path));
            }
            v
        })
        .collect();
    let mastered = &d.mastered;
    json!({
        "loudness": {
            "integrated_lufs": jnum(l.integrated_lufs, 2),
            "short_term_max_lufs": jnum(l.short_term_max_lufs, 2),
            "momentary_max_lufs": jnum(l.momentary_max_lufs, 2),
            "loudness_range_lu": jnum(l.loudness_range_lu, 2),
            "true_peak_dbtp": jnum(l.true_peak_dbtp, 2),
            "sample_peak_dbfs": jnum(l.sample_peak_dbfs, 2),
            "crest_factor_db": jnum(d.crest_factor_db, 2),
            "plr_db": jnum(l.plr_db, 2),
        },
        "tonal_balance": tone_json(&label, &tone),
        "dynamics": {
            "target": {"name": t.name, "lufs": t.lufs, "true_peak_dbtp": t.true_peak_dbtp},
            "gain_to_target_db": jnum(gain, 2),
            "limiter_reduction_needed_db": jnum(limiting, 2),
            "headroom_to_target_db": jnum(t.true_peak_dbtp - peak_after, 2),
            "smallest_intensity_that_fits": fits,
            "crest_factor_db": jnum(d.crest_factor_db, 2),
            "plr_db": jnum(d.plr_db, 2),
            "loudness_range_lu": jnum(d.loudness_range_lu, 2),
            "transient_density_per_s": jnum(d.transient_density_per_s, 2),
            "sections": d.sections.iter().map(|s| json!({
                "start_s": round_to(s.start_s, 2), "end_s": round_to(s.end_s, 2),
                "lufs": jnum(s.lufs, 1), "delta_lu": jnum(s.delta_lu, 1),
                "kind": if s.delta_lu > 0.0 { "louder" } else { "quieter" },
            })).collect::<Vec<_>>(),
        },
        "stereo": {
            "channels": st.channels,
            "correlation": jnum(st.correlation, 3),
            "correlation_below_120hz": jnum(st.correlation_below_120hz, 3),
            "side_to_mid_db": jnum(st.side_to_mid_db, 2),
            "width": jnum(st.width, 3),
            "mono_loss_db": jnum(st.mono_loss_db, 2),
            "mono_compatibility": jnum(st.mono_compatibility, 3),
            "bands": st.bands.iter().map(|b| json!({
                "low_hz": b.low_hz, "high_hz": b.high_hz, "width": jnum(b.width, 3),
                "correlation": jnum(b.correlation, 3), "level_db": jnum(b.level_db, 1),
            })).collect::<Vec<_>>(),
        },
        "problems": problems,
        "problems_clean": m.problems.clean,
        "context": {
            "tempo_bpm": jnum(m.context.tempo_bpm, 1),
            "tempo_confidence": jnum(m.context.tempo_confidence, 2),
            "key": key.key,
            "key_confidence": jnum(key.confidence, 2),
            "duration_s": jnum(m.context.duration_s, 3),
            "sample_rate": m.context.sample_rate,
            "channels": m.context.channels,
            "bit_depth": req.bit_depth,
            "looks_mastered": mastered.looks_mastered,
            "mastered_score": jnum(mastered.score, 2),
            "mastered_reasons": mastered.reasons,
            "master_default_intensity": intensity.name,
            "intensity_reason": intensity.why,
            "genre": genre.name,
            "genre_source": genre.source,
            "genre_reasons": genre.reasons,
        },
    })
}

/// A compact summary (for the before/after report and the `master` result).
pub fn summary(m: &MasteringAnalysis, tone: &ToneBalance) -> Value {
    json!({
        "integrated_lufs": jnum(m.loudness.integrated_lufs, 2),
        "short_term_max_lufs": jnum(m.loudness.short_term_max_lufs, 2),
        "loudness_range_lu": jnum(m.loudness.loudness_range_lu, 2),
        "true_peak_dbtp": jnum(m.loudness.true_peak_dbtp, 2),
        "sample_peak_dbfs": jnum(m.loudness.sample_peak_dbfs, 2),
        "crest_factor_db": jnum(m.dynamics.crest_factor_db, 2),
        "plr_db": jnum(m.loudness.plr_db, 2),
        "correlation": jnum(m.stereo.correlation, 3),
        "correlation_below_120hz": jnum(m.stereo.correlation_below_120hz, 3),
        "mono_compatibility": jnum(m.stereo.mono_compatibility, 3),
        "tonal_rms_deviation_db": jnum(tone.rms_deviation_db, 2),
        "largest_tonal_deviations": tone.largest_deviations.iter().map(|b| json!({
            "center_hz": spectrum::nominal_hz(b.center_hz), "delta_db": jnum(b.delta_db, 1)})).collect::<Vec<_>>(),
        "problems": m.problems.problems.iter().map(|p| json!({"problem": p.problem, "severity": p.severity})).collect::<Vec<_>>(),
        "duration_s": jnum(m.context.duration_s, 3),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixes_become_fx_stages() {
        assert_eq!(fix_to_stage(None, "declip"), ("declip".to_string(), false));
        assert_eq!(fix_to_stage(None, "highpass:freq=30Hz").0, "highpass:freq=30Hz");
        assert_eq!(fix_to_stage(None, "silence trim --edges").0, "silence trim:edges");
        assert_eq!(fix_to_stage(None, "channels mix --invert R").0, "channels mix:invert=R");
        assert_eq!(fix_to_stage(None, "").0, "");
    }
}
