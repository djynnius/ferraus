//! Measurements the mastering crate decides from: dynamics (crest factor, PLR, transient
//! density, loudness sections), the "already mastered" heuristic, and a one-call
//! [`mastering_analysis`] that bundles loudness, tonal balance, dynamics, stereo,
//! problems, and tempo.

use crate::loudness::{self, LoudnessReport, LoudnessSection};
use crate::problems::{self, ProblemsReport};
use crate::spectrum::{self, BandLevel, ToneBalance};
use crate::stereo::{self, StereoReport};
use crate::tempo::{self, TempoOptions};
use ferraus_core::AudioBuffer;
use serde::Serialize;

/// Whether the input already looks mastered (limited, loud, little headroom).
#[derive(Clone, Debug, Serialize)]
pub struct MasteredHeuristic {
    pub looks_mastered: bool,
    /// 0..1 share of the mastered indicators that apply.
    pub score: f64,
    pub reasons: Vec<String>,
}

/// Dynamics measurements.
#[derive(Clone, Debug, Serialize)]
pub struct DynamicsReport {
    /// Peak minus RMS (dB).
    pub crest_factor_db: f64,
    /// True peak minus integrated loudness (dB).
    pub plr_db: f64,
    pub loudness_range_lu: f64,
    /// Onsets per second.
    pub transient_density_per_s: f64,
    /// Distance from the true peak to 0 dBTP (dB).
    pub headroom_db: f64,
    /// Sections more than 6 LU louder or quieter than the whole.
    pub sections: Vec<LoudnessSection>,
    pub mastered: MasteredHeuristic,
}

/// Peak-to-loudness ratio: true peak (dBTP) minus integrated loudness (LUFS).
pub fn plr_db(buf: &AudioBuffer) -> f64 {
    let r = loudness::loudness(buf);
    r.plr_db
}

/// Score the "already mastered" indicators from a loudness report and crest factor.
pub fn looks_mastered(l: &LoudnessReport, crest_factor_db: f64) -> MasteredHeuristic {
    let mut reasons = Vec::new();
    let mut hits = 0.0;
    let total = 5.0;
    if l.integrated_lufs.is_finite() && l.integrated_lufs > -14.0 {
        hits += 1.0;
        reasons.push(format!("loud: {:.1} LUFS integrated", l.integrated_lufs));
    }
    if l.true_peak_dbtp > -1.5 {
        hits += 1.0;
        reasons.push(format!("little headroom: true peak {:.1} dBTP", l.true_peak_dbtp));
    }
    if l.plr_db.is_finite() && l.plr_db < 11.0 {
        hits += 1.0;
        reasons.push(format!("low peak-to-loudness ratio: {:.1} dB", l.plr_db));
    }
    if crest_factor_db < 10.0 {
        hits += 1.0;
        reasons.push(format!("low crest factor: {crest_factor_db:.1} dB (limited)"));
    }
    if l.loudness_range_lu < 6.0 && l.duration_s >= 10.0 {
        hits += 1.0;
        reasons.push(format!("narrow loudness range: {:.1} LU", l.loudness_range_lu));
    }
    let score = hits / total;
    MasteredHeuristic {
        looks_mastered: score >= 0.6 && l.integrated_lufs.is_finite() && l.integrated_lufs > -16.0,
        score,
        reasons,
    }
}

/// Dynamics report of a buffer.
pub fn dynamics(buf: &AudioBuffer) -> DynamicsReport {
    let l = loudness::loudness(buf);
    dynamics_with(buf, &l)
}

fn dynamics_with(buf: &AudioBuffer, l: &LoudnessReport) -> DynamicsReport {
    let crest = crate::levels::crest_factor_db(buf);
    DynamicsReport {
        crest_factor_db: crest,
        plr_db: l.plr_db,
        loudness_range_lu: l.loudness_range_lu,
        transient_density_per_s: tempo::transient_density(buf),
        headroom_db: -l.true_peak_dbtp,
        sections: loudness::loudness_outliers(buf, 6.0),
        mastered: looks_mastered(l, crest),
    }
}

/// Context numbers for a mastering report.
#[derive(Clone, Debug, Serialize)]
pub struct MasteringContext {
    pub duration_s: f64,
    pub sample_rate: u32,
    pub channels: usize,
    pub tempo_bpm: f64,
    pub tempo_confidence: f64,
}

/// Everything `master analyze` reports that this crate can measure (key comes from the
/// pitch crate, bit depth from the decoder).
#[derive(Clone, Debug, Serialize)]
pub struct MasteringAnalysis {
    pub loudness: LoudnessReport,
    pub third_octave: Vec<BandLevel>,
    /// Present when a reference was given.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tone_balance: Option<ToneBalance>,
    pub dynamics: DynamicsReport,
    pub stereo: StereoReport,
    pub problems: ProblemsReport,
    pub context: MasteringContext,
}

/// One-call analysis for mastering. `reference` adds a tonal-balance comparison.
pub fn mastering_analysis(buf: &AudioBuffer, reference: Option<&AudioBuffer>) -> MasteringAnalysis {
    let l = loudness::loudness(buf);
    let bands = spectrum::third_octave_levels(buf);
    let tone_balance = reference.map(|r| spectrum::tone_balance(&bands, &spectrum::third_octave_levels(r)));
    let t = tempo::detect_tempo(buf, &TempoOptions::default());
    MasteringAnalysis {
        dynamics: dynamics_with(buf, &l),
        loudness: l,
        third_octave: bands,
        tone_balance,
        stereo: stereo::stereo_analysis(buf),
        problems: problems::diagnose(buf),
        context: MasteringContext {
            duration_s: buf.duration_s(),
            sample_rate: buf.sample_rate,
            channels: buf.channels(),
            tempo_bpm: t.bpm,
            tempo_confidence: t.confidence,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferraus_core::testing::{noise, sine, stereo};

    #[test]
    fn heuristic_flags_loud_limited_audio() {
        // hard-limited loud noise: loud, no headroom, low crest
        let mut b = noise(48000, 12.0, 1.4, 1);
        b.data[0].iter_mut().for_each(|v| *v = v.clamp(-0.98, 0.98));
        let d = dynamics(&stereo(b));
        assert!(d.mastered.looks_mastered, "{:?}", d.mastered);
        // quiet sine with lots of headroom
        let q = stereo(sine(48000, 440.0, 12.0, 0.05));
        let d = dynamics(&q);
        assert!(!d.mastered.looks_mastered, "{:?}", d.mastered);
        assert!(d.headroom_db > 20.0);
    }

    #[test]
    fn bundle_with_reference() {
        let a = stereo(sine(48000, 440.0, 3.0, 0.3));
        let r = stereo(noise(48000, 3.0, 0.2, 2));
        let m = mastering_analysis(&a, Some(&r));
        assert!(m.tone_balance.is_some());
        assert!(m.loudness.integrated_lufs.is_finite());
        assert_eq!(m.context.channels, 2);
        let v = serde_json::to_value(&m).unwrap();
        assert!(v["stereo"]["correlation"].as_f64().unwrap() > 0.99);
    }
}
