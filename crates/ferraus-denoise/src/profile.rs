//! Noise profiles: learning, automatic noise-region detection, the `.noiseprof` JSON
//! format, and merging.
//!
//! A profile stores, per frequency bin, the mean and standard deviation of the STFT
//! magnitude in dB across the profile frames (per channel, plus the statistics of the
//! channel-mean power used when channels are linked), together with the FFT size, hop,
//! sample rate, channel count, and the ferraus version that wrote it. Levels use the
//! `dbfs_sine` scale: a full-scale sine reads 0 dB at its bin.

use crate::stft::{fft_size_for, hop_for, pow_db, Fft, FLOOR_DB};
use ferraus_core::{AudioBuffer, Error, Result, FERRAUS_VERSION};
use realfft::num_complex::Complex32;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::VecDeque;

pub const PROFILE_FORMAT: &str = "ferraus-noiseprof";
pub const PROFILE_FORMAT_VERSION: u32 = 1;

/// Mean and standard deviation of the magnitude in dB, per bin.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BinStats {
    pub mean_db: Vec<f32>,
    pub std_db: Vec<f32>,
}

/// Where a profile came from (informational).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct ProfileSource {
    /// `range`, `auto`, `from`, or `merge-max` / `merge-mean`.
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_s: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_s: Option<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub merged_from: Vec<String>,
}

/// A learned noise fingerprint (the `.noiseprof` file).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NoiseProfile {
    pub format: String,
    pub format_version: u32,
    pub ferraus_version: String,
    pub sample_rate: u32,
    pub fft_size: usize,
    pub hop_size: usize,
    pub window: String,
    pub magnitude_scale: String,
    pub channels: usize,
    /// Number of STFT frames the statistics were computed from.
    pub frames: usize,
    pub bin_hz: f64,
    #[serde(default)]
    pub source: ProfileSource,
    /// `mean_db[channel][bin]`.
    pub mean_db: Vec<Vec<f32>>,
    /// `std_db[channel][bin]`.
    pub std_db: Vec<Vec<f32>>,
    /// Statistics of the channel-mean power (used with `--link on`).
    pub linked: BinStats,
}

fn round3(x: f64) -> f32 {
    ((x * 1000.0).round() / 1000.0) as f32
}

impl NoiseProfile {
    pub fn bins(&self) -> usize {
        self.fft_size / 2 + 1
    }

    /// Statistics to use for mask group `group`: the linked statistics when channels are
    /// linked or when the profile's channel count does not match the input.
    pub fn stats_for(&self, group: usize, link: bool, input_channels: usize) -> (&[f32], &[f32]) {
        if link || self.channels != input_channels {
            (&self.linked.mean_db, &self.linked.std_db)
        } else {
            let c = group.min(self.channels - 1);
            (&self.mean_db[c], &self.std_db[c])
        }
    }

    /// Mean level of the profile across bins, in dB (for reports).
    pub fn mean_level_db(&self) -> f32 {
        let v = &self.linked.mean_db;
        v.iter().sum::<f32>() / v.len().max(1) as f32
    }

    /// Validate structure (used after parsing a file).
    pub fn validate(&self) -> Result<()> {
        let bad = |m: String| {
            Error::input(format!("invalid noise profile: {m}"))
                .with_hint("re-learn it with 'ferraus denoise profile <input> --auto -o room.noiseprof'")
        };
        if self.format != PROFILE_FORMAT {
            return Err(bad(format!("format is '{}', expected '{PROFILE_FORMAT}'", self.format)));
        }
        if self.format_version > PROFILE_FORMAT_VERSION {
            return Err(bad(format!(
                "format_version {} is newer than this ferraus understands ({PROFILE_FORMAT_VERSION})",
                self.format_version
            )));
        }
        if self.fft_size < 16 || !self.fft_size.is_power_of_two() {
            return Err(bad(format!("fft_size {} is not a power of two", self.fft_size)));
        }
        if self.sample_rate == 0 || self.channels == 0 {
            return Err(bad("sample_rate and channels must be positive".into()));
        }
        let bins = self.bins();
        if self.mean_db.len() != self.channels || self.std_db.len() != self.channels {
            return Err(bad(format!("expected {} channel arrays in mean_db/std_db", self.channels)));
        }
        let all = self.mean_db.iter().chain(&self.std_db).chain([&self.linked.mean_db, &self.linked.std_db]);
        for a in all {
            if a.len() != bins {
                return Err(bad(format!("expected {bins} bins (fft_size/2+1), found {}", a.len())));
            }
            if a.iter().any(|v| !v.is_finite()) {
                return Err(bad("non-finite values".into()));
            }
        }
        Ok(())
    }

    pub fn from_json(bytes: &[u8]) -> Result<NoiseProfile> {
        let p: NoiseProfile = serde_json::from_slice(bytes).map_err(|e| {
            Error::input(format!("cannot parse noise profile: {e}"))
                .with_hint("profiles are JSON written by 'ferraus denoise profile'")
        })?;
        p.validate()?;
        Ok(p)
    }

    /// JSON with one top-level key per line and arrays kept on one line, so the file stays
    /// small and diffable while remaining plain JSON.
    pub fn to_json(&self) -> Vec<u8> {
        let v = serde_json::to_value(self).expect("profile serializes");
        let mut out = String::from("{\n");
        if let Value::Object(m) = v {
            let n = m.len();
            for (i, (k, val)) in m.iter().enumerate() {
                out.push_str("  ");
                out.push_str(&serde_json::to_string(k).unwrap());
                out.push_str(": ");
                out.push_str(&serde_json::to_string(val).unwrap());
                if i + 1 < n {
                    out.push(',');
                }
                out.push('\n');
            }
        }
        out.push_str("}\n");
        out.into_bytes()
    }
}

/// Streaming accumulator of per-bin dB statistics (Welford), constant memory.
pub struct ProfileLearner {
    fft: Fft,
    sample_rate: u32,
    channels: usize,
    frames: usize,
    mean: Vec<Vec<f64>>,
    m2: Vec<Vec<f64>>,
    lmean: Vec<f64>,
    lm2: Vec<f64>,
    spec: Vec<Complex32>,
    lpow: Vec<f32>,
}

impl ProfileLearner {
    pub fn new(sample_rate: u32, channels: usize) -> ProfileLearner {
        let fft = Fft::new(fft_size_for(sample_rate));
        let bins = fft.bins();
        let channels = channels.max(1);
        ProfileLearner {
            sample_rate,
            channels,
            frames: 0,
            mean: vec![vec![0.0; bins]; channels],
            m2: vec![vec![0.0; bins]; channels],
            lmean: vec![0.0; bins],
            lm2: vec![0.0; bins],
            spec: vec![Complex32::new(0.0, 0.0); bins],
            lpow: vec![0.0; bins],
            fft,
        }
    }

    pub fn fft_size(&self) -> usize {
        self.fft.n
    }

    pub fn frames(&self) -> usize {
        self.frames
    }

    /// Add one frame (`frame[c]` has `fft_size` samples per channel).
    pub fn add_frame(&mut self, frame: &[&[f32]]) {
        self.frames += 1;
        let nf = self.frames as f64;
        let scale2 = self.fft.mag_scale * self.fft.mag_scale;
        self.lpow.iter_mut().for_each(|p| *p = 0.0);
        for c in 0..self.channels {
            let x = frame[c.min(frame.len() - 1)];
            self.fft.forward(x, &mut self.spec);
            for (k, z) in self.spec.iter().enumerate() {
                let p = z.norm_sqr() * scale2;
                self.lpow[k] += p / self.channels as f32;
                let db = pow_db(p) as f64;
                let d = db - self.mean[c][k];
                self.mean[c][k] += d / nf;
                self.m2[c][k] += d * (db - self.mean[c][k]);
            }
        }
        for k in 0..self.lpow.len() {
            let db = pow_db(self.lpow[k]) as f64;
            let d = db - self.lmean[k];
            self.lmean[k] += d / nf;
            self.lm2[k] += d * (db - self.lmean[k]);
        }
    }

    /// Add every frame that lies fully inside `[start, end)` of `buf`.
    pub fn add_region(&mut self, buf: &AudioBuffer, start: usize, end: usize) {
        let n = self.fft.n;
        let hop = hop_for(n);
        let end = end.min(buf.frames());
        let mut s = start;
        while s + n <= end {
            let frame: Vec<&[f32]> = buf.data.iter().map(|c| &c[s..s + n]).collect();
            self.add_frame(&frame);
            s += hop;
        }
    }

    pub fn finish(self, source: ProfileSource) -> Result<NoiseProfile> {
        if self.frames == 0 {
            let ms = self.fft.n as f64 * 1000.0 / self.sample_rate as f64;
            return Err(Error::usage(format!(
                "the noise region is too short: it needs at least {ms:.0} ms ({} samples)",
                self.fft.n
            )));
        }
        let nf = self.frames as f64;
        let std = |m2: f64| {
            if self.frames > 1 {
                (m2 / (nf - 1.0)).max(0.0).sqrt()
            } else {
                0.0
            }
        };
        let mean_db: Vec<Vec<f32>> =
            self.mean.iter().map(|c| c.iter().map(|v| round3(v.max(FLOOR_DB as f64))).collect()).collect();
        let std_db: Vec<Vec<f32>> = self.m2.iter().map(|c| c.iter().map(|v| round3(std(*v))).collect()).collect();
        let linked = BinStats {
            mean_db: self.lmean.iter().map(|v| round3(v.max(FLOOR_DB as f64))).collect(),
            std_db: self.lm2.iter().map(|v| round3(std(*v))).collect(),
        };
        let n = self.fft.n;
        Ok(NoiseProfile {
            format: PROFILE_FORMAT.into(),
            format_version: PROFILE_FORMAT_VERSION,
            ferraus_version: FERRAUS_VERSION.into(),
            sample_rate: self.sample_rate,
            fft_size: n,
            hop_size: hop_for(n),
            window: "hann".into(),
            magnitude_scale: "dbfs_sine".into(),
            channels: self.channels,
            frames: self.frames,
            bin_hz: self.sample_rate as f64 / n as f64,
            source,
            mean_db,
            std_db,
            linked,
        })
    }
}

/// Learn a profile from `[start, end)` of `buf`.
pub fn learn(buf: &AudioBuffer, start: usize, end: usize, source: ProfileSource) -> Result<NoiseProfile> {
    let mut l = ProfileLearner::new(buf.sample_rate, buf.channels());
    l.add_region(buf, start, end);
    l.finish(source)
}

/// The region chosen by `--auto`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct QuietRegion {
    pub start: usize,
    pub end: usize,
    pub start_s: f64,
    pub end_s: f64,
    /// Mean level of the region (dBFS-sine scale, averaged over bins).
    pub level_db: f32,
    /// Mean over bands of the level's standard deviation across frames (dB); lower is steadier.
    pub stability_db: f32,
}

const AUTO_BANDS: usize = 16;
/// Weight of spectral instability against loudness when ranking candidate windows.
const STABILITY_WEIGHT: f64 = 1.0;
/// Windows quieter than this are digital silence and useless as a noise sample.
const SILENCE_DB: f64 = -130.0;

/// Find the quietest spectrally stable window of `window_s` seconds (or a shorter window
/// when the input is short). Streams over the input with constant memory.
pub fn find_quiet_region(buf: &AudioBuffer, window_s: f64) -> Option<QuietRegion> {
    let sr = buf.sample_rate;
    let n = fft_size_for(sr);
    let hop = hop_for(n);
    let total = buf.frames();
    if total < n {
        return None;
    }
    let available = (total - n) / hop + 1;
    let want = ((window_s * sr as f64 - n as f64) / hop as f64).floor() as isize + 1;
    let w = (want.max(1) as usize).min(available);

    let mut fft = Fft::new(n);
    let bins = fft.bins();
    let scale2 = fft.mag_scale * fft.mag_scale;
    // log-spaced band edges between 60 Hz and min(16 kHz, Nyquist)
    let lo = 60.0f64;
    let hi = (sr as f64 / 2.0).min(16_000.0).max(lo * 2.0);
    let bin_hz = sr as f64 / n as f64;
    let edges: Vec<usize> = (0..=AUTO_BANDS)
        .map(|b| {
            let f = lo * (hi / lo).powf(b as f64 / AUTO_BANDS as f64);
            ((f / bin_hz).round() as usize).clamp(1, bins - 1)
        })
        .collect();
    let mut mono = vec![0f32; n];
    let mut spec = vec![Complex32::new(0.0, 0.0); bins];
    // ring of per-frame feature vectors: [total_db, band_db...]
    let mut ring: VecDeque<[f64; AUTO_BANDS + 1]> = VecDeque::with_capacity(w + 1);
    let mut sum = [0f64; AUTO_BANDS + 1];
    let mut sum2 = [0f64; AUTO_BANDS + 1];
    let mut best: Option<(f64, usize, f64, f64)> = None; // (score, frame index, level, stab)
    let mut best_silent: Option<(usize, f64)> = None;
    let chans = buf.channels() as f32;
    for f in 0..available {
        let s = f * hop;
        for (i, m) in mono.iter_mut().enumerate() {
            *m = buf.data.iter().map(|c| c[s + i]).sum::<f32>() / chans;
        }
        fft.forward(&mono, &mut spec);
        let mut feat = [0f64; AUTO_BANDS + 1];
        let mut tot = 0f64;
        for z in &spec {
            tot += (z.norm_sqr() * scale2) as f64;
        }
        feat[0] = pow_db((tot / bins as f64) as f32) as f64;
        for b in 0..AUTO_BANDS {
            let (a, e) = (edges[b], edges[b + 1].max(edges[b] + 1));
            let mut p = 0f64;
            for z in &spec[a..e.min(bins)] {
                p += (z.norm_sqr() * scale2) as f64;
            }
            feat[b + 1] = pow_db((p / (e - a) as f64) as f32) as f64;
        }
        for i in 0..=AUTO_BANDS {
            sum[i] += feat[i];
            sum2[i] += feat[i] * feat[i];
        }
        ring.push_back(feat);
        if ring.len() > w {
            let old = ring.pop_front().unwrap();
            for i in 0..=AUTO_BANDS {
                sum[i] -= old[i];
                sum2[i] -= old[i] * old[i];
            }
        }
        if ring.len() == w {
            let wn = w as f64;
            let level = sum[0] / wn;
            let mut stab = 0f64;
            for i in 1..=AUTO_BANDS {
                let m = sum[i] / wn;
                stab += (sum2[i] / wn - m * m).max(0.0).sqrt();
            }
            stab /= AUTO_BANDS as f64;
            let first = f + 1 - w;
            if level < SILENCE_DB {
                if best_silent.is_none() {
                    best_silent = Some((first, level));
                }
                continue;
            }
            let score = level + STABILITY_WEIGHT * stab;
            if best.map(|b| score < b.0).unwrap_or(true) {
                best = Some((score, first, level, stab));
            }
        }
    }
    let (first, level, stab) = match (best, best_silent) {
        (Some((_, f, l, s)), _) => (f, l, s),
        (None, Some((f, l))) => (f, l, 0.0),
        (None, None) => return None,
    };
    let start = first * hop;
    let end = (start + (w - 1) * hop + n).min(total);
    Some(QuietRegion {
        start,
        end,
        start_s: start as f64 / sr as f64,
        end_s: end as f64 / sr as f64,
        level_db: level as f32,
        stability_db: stab as f32,
    })
}

/// How `denoise profile merge` combines profiles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergeMethod {
    /// Element-wise maximum of means and of standard deviations (the most conservative
    /// fingerprint: removes the loudest noise seen in any take).
    Max,
    /// Pooled statistics weighted by frame count (as if all frames had been profiled together).
    Mean,
}

impl MergeMethod {
    pub fn parse(s: &str) -> Result<MergeMethod> {
        match s {
            "max" => Ok(MergeMethod::Max),
            "mean" => Ok(MergeMethod::Mean),
            other => Err(Error::usage(format!("--method must be max or mean, not '{other}'"))),
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            MergeMethod::Max => "max",
            MergeMethod::Mean => "mean",
        }
    }
}

fn merge_stats(parts: &[(&[f32], &[f32], usize)], method: MergeMethod) -> BinStats {
    let bins = parts[0].0.len();
    let mut mean_db = vec![0f32; bins];
    let mut std_db = vec![0f32; bins];
    match method {
        MergeMethod::Max => {
            for k in 0..bins {
                mean_db[k] = parts.iter().map(|p| p.0[k]).fold(f32::MIN, f32::max);
                std_db[k] = parts.iter().map(|p| p.1[k]).fold(f32::MIN, f32::max);
            }
        }
        MergeMethod::Mean => {
            let total: f64 = parts.iter().map(|p| p.2.max(1) as f64).sum();
            for k in 0..bins {
                let mut m = 0f64;
                let mut s2 = 0f64;
                for p in parts {
                    let wt = p.2.max(1) as f64 / total;
                    let (mu, sd) = (p.0[k] as f64, p.1[k] as f64);
                    m += wt * mu;
                    s2 += wt * (sd * sd + mu * mu);
                }
                mean_db[k] = round3(m);
                std_db[k] = round3((s2 - m * m).max(0.0).sqrt());
            }
        }
    }
    BinStats { mean_db, std_db }
}

/// Merge two or more profiles. Returns the merged profile and any warnings.
pub fn merge(profiles: &[NoiseProfile], names: &[String], method: MergeMethod) -> Result<(NoiseProfile, Vec<String>)> {
    if profiles.len() < 2 {
        return Err(Error::usage("merge needs at least two profiles"));
    }
    let first = &profiles[0];
    for (p, name) in profiles.iter().zip(names).skip(1) {
        if p.sample_rate != first.sample_rate || p.fft_size != first.fft_size {
            return Err(Error::usage(format!(
                "{name} was learned at {} Hz with FFT size {}, but the first profile uses {} Hz / {}; profiles can only be merged when both match",
                p.sample_rate, p.fft_size, first.sample_rate, first.fft_size
            ))
            .with_hint("convert the takes to one sample rate ('ferraus convert in.wav --sample-rate 48000 -o out.wav') and re-learn the profiles"));
        }
    }
    let mut warnings = Vec::new();
    let same_channels = profiles.iter().all(|p| p.channels == first.channels);
    let channels = profiles.iter().map(|p| p.channels).max().unwrap_or(1);
    if !same_channels {
        warnings.push(format!(
            "profiles have different channel counts; per-channel statistics were built from each profile's linked statistics ({channels} channels)"
        ));
    }
    let mut mean_db = Vec::with_capacity(channels);
    let mut std_db = Vec::with_capacity(channels);
    for c in 0..channels {
        let parts: Vec<(&[f32], &[f32], usize)> = profiles
            .iter()
            .map(|p| {
                if same_channels {
                    (p.mean_db[c].as_slice(), p.std_db[c].as_slice(), p.frames)
                } else {
                    (p.linked.mean_db.as_slice(), p.linked.std_db.as_slice(), p.frames)
                }
            })
            .collect();
        let s = merge_stats(&parts, method);
        mean_db.push(s.mean_db);
        std_db.push(s.std_db);
    }
    let lparts: Vec<(&[f32], &[f32], usize)> =
        profiles.iter().map(|p| (p.linked.mean_db.as_slice(), p.linked.std_db.as_slice(), p.frames)).collect();
    let linked = merge_stats(&lparts, method);
    let merged = NoiseProfile {
        format: PROFILE_FORMAT.into(),
        format_version: PROFILE_FORMAT_VERSION,
        ferraus_version: FERRAUS_VERSION.into(),
        sample_rate: first.sample_rate,
        fft_size: first.fft_size,
        hop_size: first.hop_size,
        window: first.window.clone(),
        magnitude_scale: first.magnitude_scale.clone(),
        channels,
        frames: profiles.iter().map(|p| p.frames).sum(),
        bin_hz: first.bin_hz,
        source: ProfileSource {
            method: format!("merge-{}", method.name()),
            merged_from: names.to_vec(),
            ..Default::default()
        },
        mean_db,
        std_db,
        linked,
    };
    Ok((merged, warnings))
}
