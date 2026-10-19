//! Genre data: tonal-balance reference curves (1/3-octave band levels, dB, relative), crest
//! factor targets, and the `--genre auto` heuristic.
//!
//! # Provenance (read before trusting these numbers)
//!
//! The curves and crest targets are **hand-designed starting points**, not measurements of a
//! reference corpus. They encode widely shared engineering rules of thumb about how the
//! long-term average spectrum of finished music is shaped when read in 1/3-octave bands:
//! the energy peaks in the kick/bass region (roughly 50–120 Hz), then falls by about
//! 3–5 dB per octave towards the top; electronic and hip-hop masters carry more sub and
//! low bass, rock less sub and more low-mid density, acoustic, jazz, and classical
//! recordings much less low end and a gentler top, and speech is band-limited around
//! 150 Hz–5 kHz. Crest targets follow the same intuition (dense genres are mastered with
//! less peak-to-RMS than acoustic and orchestral music).
//!
//! The PRD lists "a few reference masters you consider production-ready, to calibrate the
//! genre curves" under *Information that would sharpen the spec*. Until that calibration
//! happens these curves are deliberately applied at `--match-amount 50%` by default and
//! every correction is capped at 4 dB per band, so a curve that is off by a few dB moves a
//! mix only gently. Replace the tables below with measured averages when references exist.

use ferraus_analysis::spectrum::BandLevel;
use ferraus_analysis::MasteringAnalysis;
use serde::Serialize;

/// Genre names accepted by `--genre` (plus `auto`).
pub const GENRE_NAMES: &[&str] = &["pop", "hiphop", "edm", "rock", "acoustic", "classical", "jazz", "podcast"];

/// Per-genre mastering data.
#[derive(Clone, Copy, Debug)]
pub struct GenreData {
    pub name: &'static str,
    /// `(frequency_hz, level_db)` points of the 1/3-octave tonal-balance curve, interpolated
    /// linearly in log frequency. Only the shape matters (the overall offset is removed).
    pub curve: &'static [(f64, f64)],
    /// Crest factor (sample peak minus RMS, dB) above which the glue compressor engages.
    pub crest_target_db: f64,
    /// Multiband ratio offset relative to the intensity's ratio (dynamic genres compress less).
    pub ratio_offset: f64,
}

const POP: &[(f64, f64)] = &[
    (20.0, -10.0),
    (25.0, -6.0),
    (40.0, -1.5),
    (63.0, 0.0),
    (100.0, 0.0),
    (160.0, -1.5),
    (250.0, -3.0),
    (400.0, -4.5),
    (630.0, -6.0),
    (1000.0, -7.5),
    (1600.0, -9.0),
    (2500.0, -10.5),
    (4000.0, -12.0),
    (6300.0, -13.5),
    (10000.0, -16.0),
    (16000.0, -22.0),
    (20000.0, -30.0),
];
const HIPHOP: &[(f64, f64)] = &[
    (20.0, -7.0),
    (25.0, -3.0),
    (40.0, 1.0),
    (63.0, 1.5),
    (100.0, 0.5),
    (160.0, -2.0),
    (250.0, -4.0),
    (400.0, -6.0),
    (630.0, -7.5),
    (1000.0, -9.0),
    (1600.0, -10.5),
    (2500.0, -12.0),
    (4000.0, -13.5),
    (6300.0, -15.0),
    (10000.0, -17.5),
    (16000.0, -23.0),
    (20000.0, -31.0),
];
const EDM: &[(f64, f64)] = &[
    (20.0, -7.0),
    (25.0, -3.0),
    (40.0, 1.0),
    (63.0, 1.0),
    (100.0, 0.0),
    (160.0, -2.0),
    (250.0, -4.0),
    (400.0, -5.5),
    (630.0, -7.0),
    (1000.0, -8.0),
    (1600.0, -9.0),
    (2500.0, -10.0),
    (4000.0, -11.0),
    (6300.0, -12.0),
    (10000.0, -14.0),
    (16000.0, -19.0),
    (20000.0, -27.0),
];
const ROCK: &[(f64, f64)] = &[
    (20.0, -15.0),
    (25.0, -10.0),
    (40.0, -4.0),
    (63.0, -1.0),
    (100.0, 0.0),
    (160.0, -1.0),
    (250.0, -2.5),
    (400.0, -4.0),
    (630.0, -5.0),
    (1000.0, -6.5),
    (1600.0, -8.0),
    (2500.0, -9.0),
    (4000.0, -10.5),
    (6300.0, -12.5),
    (10000.0, -15.5),
    (16000.0, -22.0),
    (20000.0, -30.0),
];
const ACOUSTIC: &[(f64, f64)] = &[
    (20.0, -20.0),
    (25.0, -14.0),
    (40.0, -8.0),
    (63.0, -4.0),
    (100.0, -2.0),
    (160.0, -1.0),
    (250.0, -1.5),
    (400.0, -3.0),
    (630.0, -4.5),
    (1000.0, -6.0),
    (1600.0, -7.5),
    (2500.0, -9.0),
    (4000.0, -10.5),
    (6300.0, -12.5),
    (10000.0, -16.0),
    (16000.0, -23.0),
    (20000.0, -31.0),
];
const CLASSICAL: &[(f64, f64)] = &[
    (20.0, -20.0),
    (25.0, -14.0),
    (40.0, -9.0),
    (63.0, -5.0),
    (100.0, -3.0),
    (160.0, -2.0),
    (250.0, -2.0),
    (400.0, -3.0),
    (630.0, -4.5),
    (1000.0, -6.0),
    (1600.0, -8.0),
    (2500.0, -10.0),
    (4000.0, -12.0),
    (6300.0, -14.5),
    (10000.0, -18.0),
    (16000.0, -26.0),
    (20000.0, -34.0),
];
const JAZZ: &[(f64, f64)] = &[
    (20.0, -18.0),
    (25.0, -12.0),
    (40.0, -6.0),
    (63.0, -3.0),
    (100.0, -1.5),
    (160.0, -1.0),
    (250.0, -2.0),
    (400.0, -3.5),
    (630.0, -5.0),
    (1000.0, -6.5),
    (1600.0, -8.0),
    (2500.0, -9.5),
    (4000.0, -11.0),
    (6300.0, -13.0),
    (10000.0, -16.5),
    (16000.0, -24.0),
    (20000.0, -32.0),
];
const PODCAST: &[(f64, f64)] = &[
    (20.0, -40.0),
    (40.0, -26.0),
    (63.0, -15.0),
    (100.0, -6.0),
    (160.0, -2.0),
    (250.0, 0.0),
    (400.0, 0.0),
    (630.0, -1.0),
    (1000.0, -3.0),
    (1600.0, -5.0),
    (2500.0, -6.0),
    (4000.0, -8.0),
    (6300.0, -11.0),
    (10000.0, -18.0),
    (16000.0, -28.0),
    (20000.0, -36.0),
];

/// The genre table.
pub const GENRES: &[GenreData] = &[
    GenreData { name: "pop", curve: POP, crest_target_db: 12.0, ratio_offset: 0.0 },
    GenreData { name: "hiphop", curve: HIPHOP, crest_target_db: 11.0, ratio_offset: 0.0 },
    GenreData { name: "edm", curve: EDM, crest_target_db: 10.0, ratio_offset: 0.0 },
    GenreData { name: "rock", curve: ROCK, crest_target_db: 12.0, ratio_offset: 0.0 },
    GenreData { name: "acoustic", curve: ACOUSTIC, crest_target_db: 15.0, ratio_offset: -0.25 },
    GenreData { name: "classical", curve: CLASSICAL, crest_target_db: 18.0, ratio_offset: -0.25 },
    GenreData { name: "jazz", curve: JAZZ, crest_target_db: 16.0, ratio_offset: -0.25 },
    GenreData { name: "podcast", curve: PODCAST, crest_target_db: 14.0, ratio_offset: 0.0 },
];

pub fn genre(name: &str) -> Option<&'static GenreData> {
    GENRES.iter().find(|g| g.name == name)
}

/// The curve value at `f` (linear interpolation in log frequency).
pub fn curve_at(curve: &[(f64, f64)], f: f64) -> f64 {
    if curve.is_empty() {
        return 0.0;
    }
    if f <= curve[0].0 {
        return curve[0].1;
    }
    for w in curve.windows(2) {
        if f <= w[1].0 {
            let t = (f.ln() - w[0].0.ln()) / (w[1].0.ln() - w[0].0.ln());
            return w[0].1 + t * (w[1].1 - w[0].1);
        }
    }
    curve.last().unwrap().1
}

/// The result of `--genre auto`.
#[derive(Clone, Debug, Serialize)]
pub struct GenreGuess {
    pub genre: String,
    /// Always `heuristic`: the guess comes from a few hand-set rules, not a classifier.
    pub method: &'static str,
    pub reasons: Vec<String>,
    pub features: GenreFeatures,
}

/// The measurements the heuristic reads.
#[derive(Clone, Debug, Serialize)]
pub struct GenreFeatures {
    pub tempo_bpm: f64,
    pub tempo_confidence: f64,
    /// Regression slope of the 1/3-octave levels from 100 Hz to 10 kHz (dB per octave).
    pub spectral_tilt_db_per_oct: f64,
    /// Share of energy below 120 Hz (0..1).
    pub low_share: f64,
    /// Share of energy between 150 Hz and 5 kHz (0..1).
    pub voice_band_share: f64,
    pub crest_factor_db: f64,
    pub loudness_range_lu: f64,
    pub transient_density_per_s: f64,
    pub correlation: f64,
}

fn share(bands: &[BandLevel], lo: f64, hi: f64) -> f64 {
    let p = |b: &BandLevel| 10f64.powf(b.db / 10.0);
    let total: f64 = bands.iter().filter(|b| b.center_hz >= 20.0).map(p).sum();
    if total <= 0.0 {
        return 0.0;
    }
    bands.iter().filter(|b| b.center_hz >= lo && b.center_hz < hi).map(p).sum::<f64>() / total
}

/// Least-squares slope of band level against log2 frequency (dB/octave).
pub fn tilt(bands: &[BandLevel], lo: f64, hi: f64) -> f64 {
    let pts: Vec<(f64, f64)> = bands
        .iter()
        .filter(|b| b.center_hz >= lo && b.center_hz <= hi && b.db > -140.0)
        .map(|b| (b.center_hz.log2(), b.db))
        .collect();
    if pts.len() < 3 {
        return 0.0;
    }
    let n = pts.len() as f64;
    let mx = pts.iter().map(|p| p.0).sum::<f64>() / n;
    let my = pts.iter().map(|p| p.1).sum::<f64>() / n;
    let sxy: f64 = pts.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum();
    let sxx: f64 = pts.iter().map(|p| (p.0 - mx).powi(2)).sum();
    if sxx <= 0.0 {
        0.0
    } else {
        sxy / sxx
    }
}

/// Guess the genre from tempo, spectral tilt, low-end share, and crest factor.
pub fn guess(m: &MasteringAnalysis) -> GenreGuess {
    let f = GenreFeatures {
        tempo_bpm: m.context.tempo_bpm,
        tempo_confidence: m.context.tempo_confidence,
        spectral_tilt_db_per_oct: tilt(&m.third_octave, 100.0, 10000.0),
        low_share: share(&m.third_octave, 20.0, 120.0),
        voice_band_share: share(&m.third_octave, 150.0, 5000.0),
        crest_factor_db: m.dynamics.crest_factor_db,
        loudness_range_lu: m.loudness.loudness_range_lu,
        transient_density_per_s: m.dynamics.transient_density_per_s,
        correlation: m.stereo.correlation,
    };
    let mut reasons = Vec::new();
    let bpm = f.tempo_bpm;
    let rhythmic = f.tempo_confidence >= 0.35;
    let g = if f.correlation >= 0.97 && f.low_share < 0.08 && f.voice_band_share > 0.75 && !rhythmic {
        reasons.push(format!(
            "near-mono ({:.2}), {:.0}% of the energy in the 150 Hz-5 kHz voice band, little low end, no steady beat",
            f.correlation,
            f.voice_band_share * 100.0
        ));
        "podcast"
    } else if f.crest_factor_db >= 18.0 && f.low_share < 0.25 && f.transient_density_per_s < 2.0 {
        reasons.push(format!(
            "very high crest factor ({:.1} dB) with few transients ({:.1}/s) and modest low end",
            f.crest_factor_db, f.transient_density_per_s
        ));
        "classical"
    } else if rhythmic && (118.0..=150.0).contains(&bpm) && f.low_share >= 0.6 {
        reasons.push(format!(
            "steady {bpm:.0} BPM four-on-the-floor range with a heavy low end ({:.0}% below 120 Hz)",
            f.low_share * 100.0
        ));
        "edm"
    } else if f.low_share >= 0.6
        && f.spectral_tilt_db_per_oct <= -3.0
        && ((60.0..=105.0).contains(&bpm) || (140.0..=185.0).contains(&bpm))
    {
        reasons.push(format!(
            "{bpm:.0} BPM (hip-hop tempo or half-time) with a heavy low end ({:.0}% below 120 Hz)",
            f.low_share * 100.0
        ));
        "hiphop"
    } else if f.crest_factor_db >= 15.0 && f.low_share < 0.45 {
        if rhythmic {
            reasons.push(format!(
                "high crest factor ({:.1} dB), light low end, steady pulse: acoustic",
                f.crest_factor_db
            ));
            "acoustic"
        } else {
            reasons.push(format!(
                "high crest factor ({:.1} dB), light low end, loose pulse (tempo confidence {:.2}): jazz",
                f.crest_factor_db, f.tempo_confidence
            ));
            "jazz"
        }
    } else if f.spectral_tilt_db_per_oct > -3.0 && f.crest_factor_db < 16.0 && f.low_share < 0.7 {
        reasons.push(format!(
            "bright, mid-dense spectrum (tilt {:.1} dB/oct) with moderate crest factor",
            f.spectral_tilt_db_per_oct
        ));
        "rock"
    } else {
        reasons.push("no stronger cue; pop is the neutral default".into());
        "pop"
    };
    GenreGuess { genre: g.into(), method: "heuristic", reasons, features: f }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_curve_is_sorted_and_has_a_crest_target() {
        for g in GENRES {
            assert!(g.curve.windows(2).all(|w| w[1].0 > w[0].0), "{}", g.name);
            assert!(g.crest_target_db > 6.0);
            assert!(GENRE_NAMES.contains(&g.name));
        }
        assert_eq!(GENRES.len(), GENRE_NAMES.len());
        assert!((curve_at(POP, 63.0) - 0.0).abs() < 1e-9);
        let mid = curve_at(POP, (63.0f64 * 100.0).sqrt());
        assert!(mid.abs() < 1e-9);
    }
}
