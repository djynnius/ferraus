//! Long-term average spectrum (Welch), fractional-octave band levels, per-bin spectra,
//! spectral descriptors, and tonal balance against a reference spectrum or curve.

use crate::util::{self, pow_db};
use ferraus_core::AudioBuffer;
use serde::Serialize;

/// Output resolution of [`spectrum`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Resolution {
    #[serde(rename = "1/3oct")]
    ThirdOctave,
    #[serde(rename = "1/6oct")]
    SixthOctave,
    #[serde(rename = "bins")]
    Bins,
}

impl Resolution {
    /// `1/3oct`, `1/6oct`, `bins` (also `third`, `sixth`, `1/3`, `1/6`).
    pub fn parse(s: &str) -> Option<Resolution> {
        match s.trim().to_ascii_lowercase().as_str() {
            "1/3oct" | "1/3" | "third" | "1/3-oct" => Some(Resolution::ThirdOctave),
            "1/6oct" | "1/6" | "sixth" | "1/6-oct" => Some(Resolution::SixthOctave),
            "bins" | "bin" | "fft" => Some(Resolution::Bins),
            _ => None,
        }
    }
}

/// Level of one frequency band. `db` is the band's mean-square power in dB relative to a
/// full-scale sine (a 0 dBFS sine inside the band reads 0 dB). Floored at -150 dB.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct BandLevel {
    pub center_hz: f64,
    pub low_hz: f64,
    pub high_hz: f64,
    pub db: f64,
}

/// A per-bin spectrum: `db[k]` is the level at `k * bin_hz`, scaled so a full-scale sine
/// centred on a bin reads 0 dB.
#[derive(Clone, Debug, Serialize)]
pub struct BinSpectrum {
    pub bin_hz: f64,
    pub fft_size: usize,
    pub db: Vec<f64>,
}

/// Result of [`spectrum`].
#[derive(Clone, Debug, Serialize)]
pub struct SpectrumReport {
    pub resolution: Resolution,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub bands: Vec<BandLevel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bins: Option<BinSpectrum>,
    /// Power-weighted mean frequency.
    pub centroid_hz: f64,
    /// Frequency below which 85% of the power lies.
    pub rolloff_hz: f64,
    /// Spectral flatness 0..1 (1 = white noise, 0 = pure tone).
    pub flatness: f64,
    /// Frequency of the strongest bin.
    pub peak_hz: f64,
}

/// Long-term average spectrum: mean-square power per bin (summed bins give the signal's
/// mean square), averaged over channels.
#[derive(Clone, Debug)]
pub struct Ltas {
    pub bin_hz: f64,
    pub fft_size: usize,
    /// Mean-square contribution of each bin (one-sided, already doubled).
    pub power: Vec<f64>,
}

/// Compute the long-term average spectrum of all channels (averaged).
pub fn ltas(buf: &AudioBuffer, n_fft: usize) -> Ltas {
    let chans: Vec<&[f32]> = buf.data.iter().map(|c| c.as_slice()).collect();
    let (p, scale) = util::welch_power(&chans, n_fft);
    Ltas {
        bin_hz: buf.sample_rate as f64 / n_fft as f64,
        fft_size: n_fft,
        power: p.into_iter().map(|v| v * scale).collect(),
    }
}

/// FFT size giving roughly 3 Hz resolution (needed for 20 Hz third-octave bands).
fn band_fft_size(sr: u32) -> usize {
    util::next_pow2((sr as f64 / 3.0) as usize).clamp(1024, 65536)
}

/// Fractional-octave band centres (base-2, referenced to 1 kHz) from 20 Hz up to the
/// Nyquist frequency. `fraction` is 3 for third-octave, 6 for sixth-octave, 1 for octave.
pub fn band_centers(fraction: u32, sample_rate: u32) -> Vec<(f64, f64, f64)> {
    let f = fraction.max(1) as f64;
    let nyq = sample_rate as f64 / 2.0;
    let kmin = (f * (20.0f64 / 1000.0).log2()).round() as i32;
    let kmax = (f * (20000.0f64 / 1000.0).log2()).round() as i32;
    (kmin..=kmax)
        .map(|k| {
            let c = 1000.0 * 2f64.powf(k as f64 / f);
            (c, c * 2f64.powf(-0.5 / f), c * 2f64.powf(0.5 / f))
        })
        .filter(|(c, _, _)| *c < nyq)
        .map(|(c, lo, hi)| (c, lo, hi.min(nyq)))
        .collect()
}

/// Nominal (rounded) label for a band centre, e.g. 31.5, 63, 1250.
pub fn nominal_hz(c: f64) -> f64 {
    let digits = if c < 100.0 { 2 } else { 3 };
    let mag = 10f64.powi(c.log10().floor() as i32 - (digits - 1));
    let r = (c / mag).round() * mag;
    // conventional names
    match r as i64 {
        31 | 32 => 31.5,
        _ => {
            if (r - 3150.0).abs() < 60.0 {
                3150.0
            } else if (r - 315.0).abs() < 6.0 {
                315.0
            } else {
                r
            }
        }
    }
}

/// Band levels from an LTAS: each bin's power is shared between bands in proportion to
/// the overlap of `[f - df/2, f + df/2]` with the band, so narrow low bands still work.
pub fn bands_from_ltas(l: &Ltas, fraction: u32, sample_rate: u32) -> Vec<BandLevel> {
    let df = l.bin_hz;
    band_centers(fraction, sample_rate)
        .into_iter()
        .map(|(c, lo, hi)| {
            let k0 = ((lo / df) - 0.5).floor().max(1.0) as usize;
            let k1 = (((hi / df) + 0.5).ceil() as usize).min(l.power.len() - 1);
            let mut ms = 0.0;
            for k in k0..=k1 {
                let (a, b) = ((k as f64 - 0.5) * df, (k as f64 + 0.5) * df);
                let ov = (b.min(hi) - a.max(lo)).max(0.0) / df;
                ms += l.power[k] * ov;
            }
            BandLevel {
                center_hz: nominal_hz(c),
                low_hz: lo,
                high_hz: hi,
                db: (pow_db(ms) + 3.0103).max(-150.0),
            }
        })
        .collect()
}

/// Fractional-octave band levels of a buffer (all channels averaged).
pub fn band_levels(buf: &AudioBuffer, fraction: u32) -> Vec<BandLevel> {
    let l = ltas(buf, band_fft_size(buf.sample_rate));
    bands_from_ltas(&l, fraction, buf.sample_rate)
}

/// Third-octave band levels (20 Hz – 20 kHz, limited by Nyquist).
pub fn third_octave_levels(buf: &AudioBuffer) -> Vec<BandLevel> {
    band_levels(buf, 3)
}

fn descriptors(l: &Ltas) -> (f64, f64, f64, f64) {
    let p = &l.power;
    let k0 = ((20.0 / l.bin_hz).ceil() as usize).max(1).min(p.len());
    let total: f64 = p[k0..].iter().sum();
    if total <= 1e-30 {
        return (0.0, 0.0, 0.0, 0.0);
    }
    let centroid = p[k0..]
        .iter()
        .enumerate()
        .map(|(i, v)| (i + k0) as f64 * l.bin_hz * v)
        .sum::<f64>()
        / total;
    let mut acc = 0.0;
    let mut rolloff = 0.0;
    for (i, v) in p[k0..].iter().enumerate() {
        acc += v;
        if acc >= 0.85 * total {
            rolloff = (i + k0) as f64 * l.bin_hz;
            break;
        }
    }
    let n = (p.len() - k0) as f64;
    let geo = (p[k0..].iter().map(|v| (v + 1e-30).ln()).sum::<f64>() / n).exp();
    let flat = (geo / (total / n)).clamp(0.0, 1.0);
    let (kmax, _) = p
        .iter()
        .enumerate()
        .skip(k0)
        .fold((0, 0.0), |m, (i, v)| if *v > m.1 { (i, *v) } else { m });
    (centroid, rolloff, flat, kmax as f64 * l.bin_hz)
}

/// Long-term average spectrum at the requested resolution, with descriptors.
pub fn spectrum(buf: &AudioBuffer, resolution: Resolution) -> SpectrumReport {
    match resolution {
        Resolution::Bins => {
            let n_fft = 4096.min(util::next_pow2(buf.frames().max(256)));
            let l = ltas(buf, n_fft);
            let (centroid, rolloff, flatness, peak) = descriptors(&l);
            // mean square per bin → sine-peak scaling (one-sided x Hann ENBW of 1.5 bins)
            let db = l.power.iter().map(|p| pow_db(p * 3.0).max(-150.0)).collect();
            SpectrumReport {
                resolution,
                bands: Vec::new(),
                bins: Some(BinSpectrum {
                    bin_hz: l.bin_hz,
                    fft_size: n_fft,
                    db,
                }),
                centroid_hz: centroid,
                rolloff_hz: rolloff,
                flatness,
                peak_hz: peak,
            }
        }
        _ => {
            let fraction = if resolution == Resolution::ThirdOctave { 3 } else { 6 };
            let l = ltas(buf, band_fft_size(buf.sample_rate));
            let (centroid, rolloff, flatness, peak) = descriptors(&l);
            SpectrumReport {
                resolution,
                bands: bands_from_ltas(&l, fraction, buf.sample_rate),
                bins: None,
                centroid_hz: centroid,
                rolloff_hz: rolloff,
                flatness,
                peak_hz: peak,
            }
        }
    }
}

/// One band of a tonal-balance comparison.
#[derive(Clone, Debug, Serialize)]
pub struct BandDelta {
    pub center_hz: f64,
    pub level_db: f64,
    pub reference_db: f64,
    /// Level minus reference after removing the overall offset (positive = too much).
    pub delta_db: f64,
}

/// Tonal balance against a reference: per-band deltas with the overall level difference
/// removed (the mean delta over 60 Hz – 12 kHz), and the three largest deviations.
#[derive(Clone, Debug, Serialize)]
pub struct ToneBalance {
    /// Mean level difference (input minus reference) that was removed before the deltas.
    pub offset_db: f64,
    pub bands: Vec<BandDelta>,
    pub largest_deviations: Vec<BandDelta>,
    /// RMS of the deltas over 60 Hz – 12 kHz.
    pub rms_deviation_db: f64,
}

/// Compare band levels with reference band levels (matched by centre frequency).
pub fn tone_balance(levels: &[BandLevel], reference: &[BandLevel]) -> ToneBalance {
    let curve: Vec<(f64, f64)> = reference.iter().map(|b| (b.center_hz, b.db)).collect();
    tone_balance_vs_curve(levels, &curve)
}

/// Compare band levels with a reference curve of `(frequency_hz, db)` points, interpolated
/// linearly in log frequency (a genre target curve, for instance).
pub fn tone_balance_vs_curve(levels: &[BandLevel], curve: &[(f64, f64)]) -> ToneBalance {
    let mut pts: Vec<(f64, f64)> = curve.iter().copied().filter(|p| p.0 > 0.0).collect();
    pts.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let at = |f: f64| -> f64 {
        if pts.is_empty() {
            return 0.0;
        }
        if f <= pts[0].0 {
            return pts[0].1;
        }
        for w in pts.windows(2) {
            if f <= w[1].0 {
                let t = (f.ln() - w[0].0.ln()) / (w[1].0.ln() - w[0].0.ln());
                return w[0].1 + t * (w[1].1 - w[0].1);
            }
        }
        pts.last().unwrap().1
    };
    let raw: Vec<(f64, f64, f64)> = levels.iter().map(|b| (b.center_hz, b.db, at(b.center_hz))).collect();
    let mid: Vec<f64> = raw
        .iter()
        .filter(|(c, l, r)| *c >= 60.0 && *c <= 12500.0 && *l > -140.0 && *r > -140.0)
        .map(|(_, l, r)| l - r)
        .collect();
    let offset = util::mean(&mid);
    let bands: Vec<BandDelta> = raw
        .iter()
        .map(|(c, l, r)| BandDelta {
            center_hz: *c,
            level_db: *l,
            reference_db: *r,
            delta_db: l - r - offset,
        })
        .collect();
    let in_range: Vec<&BandDelta> = bands
        .iter()
        .filter(|b| b.center_hz >= 60.0 && b.center_hz <= 12500.0)
        .collect();
    let rms = if in_range.is_empty() {
        0.0
    } else {
        (in_range.iter().map(|b| b.delta_db.powi(2)).sum::<f64>() / in_range.len() as f64).sqrt()
    };
    let mut largest: Vec<BandDelta> = in_range.into_iter().cloned().collect();
    largest.sort_by(|a, b| {
        b.delta_db
            .abs()
            .partial_cmp(&a.delta_db.abs())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    largest.truncate(3);
    ToneBalance {
        offset_db: offset,
        bands,
        largest_deviations: largest,
        rms_deviation_db: rms,
    }
}

/// Fraction of the signal power (0..1, all channels) between `lo` and `hi` Hz.
pub fn band_power_fraction(l: &Ltas, lo: f64, hi: f64) -> f64 {
    let total: f64 = l.power.iter().skip(1).sum();
    if total <= 1e-30 {
        return 0.0;
    }
    let k0 = ((lo / l.bin_hz).round() as usize).max(1);
    let k1 = ((hi / l.bin_hz).round() as usize).min(l.power.len() - 1);
    if k1 < k0 {
        return 0.0;
    }
    l.power[k0..=k1].iter().sum::<f64>() / total
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferraus_core::testing::{add, noise, sine};

    #[test]
    fn third_octave_sine_lands_in_its_band() {
        let b = sine(48000, 1000.0, 2.0, 0.5);
        let bands = third_octave_levels(&b);
        let k = bands.iter().position(|x| x.center_hz == 1000.0).unwrap();
        assert!((bands[k].db - 20.0 * 0.5f64.log10()).abs() < 0.3, "{}", bands[k].db);
        assert!(bands[k - 2].db < bands[k].db - 40.0);
        assert_eq!(bands[0].center_hz, 20.0);
        assert!(bands.iter().any(|b| b.center_hz == 31.5));
        assert!(bands.iter().any(|b| b.center_hz == 3150.0));
    }

    #[test]
    fn low_band_tone_is_measured() {
        let b = sine(44100, 25.0, 3.0, 0.5);
        let bands = third_octave_levels(&b);
        let k = bands.iter().position(|x| x.center_hz == 25.0).unwrap();
        assert!((bands[k].db + 6.02).abs() < 1.0, "{}", bands[k].db);
    }

    #[test]
    fn bins_and_descriptors() {
        let b = sine(48000, 3000.0, 1.0, 1.0);
        let r = spectrum(&b, Resolution::Bins);
        let bins = r.bins.unwrap();
        let k = (3000.0 / bins.bin_hz).round() as usize;
        assert!(bins.db[k] > -2.0 && bins.db[k] < 0.5, "{}", bins.db[k]);
        assert!((r.peak_hz - 3000.0).abs() < bins.bin_hz);
        assert!(r.flatness < 0.01);
        let n = noise(48000, 1.0, 0.5, 1);
        let r = spectrum(&n, Resolution::SixthOctave);
        assert!(r.flatness > 0.5, "{}", r.flatness);
        assert!(r.centroid_hz > 9000.0);
        assert!(r.bands.len() > 55);
    }

    #[test]
    fn tone_balance_finds_boost() {
        let a = add(&noise(48000, 2.0, 0.3, 1), &sine(48000, 2000.0, 2.0, 0.3));
        let r = noise(48000, 2.0, 0.15, 2);
        let tb = tone_balance(&third_octave_levels(&a), &third_octave_levels(&r));
        assert_eq!(tb.largest_deviations[0].center_hz, 2000.0);
        assert!(tb.largest_deviations[0].delta_db > 6.0);
        assert!(tb.offset_db > 4.0);
    }

    #[test]
    fn silence_is_floored() {
        let b = AudioBuffer::new(48000, 2, 4800);
        let r = spectrum(&b, Resolution::ThirdOctave);
        assert!(r.bands.iter().all(|b| b.db == -150.0));
    }
}
