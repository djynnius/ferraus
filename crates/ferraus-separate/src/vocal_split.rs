//! Built-in lead/backing split of a vocal stem by melody-guided harmonic masking: the default
//! `separate --vocal-split` stage (registry entry `separate/melody-mask`, no model file).
//!
//! 1. The dominant vocal pitch is tracked on the mono mix of the stem (pYIN with a permissive
//!    voicing prior, or a model tracker the caller passes) in 30-s chunks with 1 s of context,
//!    so long inputs stream.
//! 2. STFT (about 93 ms Hann windows, 75 % overlap). For each frame the lead's harmonic comb is
//!    built from the pitch: a Gaussian around each harmonic `h·f0` whose width covers the
//!    window's main lobe, a small pitch tolerance, and the pitch movement within the window
//!    (both grow with `h`); the comb decays over a short memory so the lead's ringing
//!    harmonics (reverb) stay with it.
//! 3. Energy on the comb stays lead. While the lead is voiced, the energy between its
//!    harmonics below 4 kHz goes mostly (80 %) to the backing; elsewhere spectral peaks off the
//!    comb (other voices' harmonics) go to the backing and non-tonal energy (consonants,
//!    breath, diffuse reverb) stays lead.
//! 4. Stereo input: the mask is scaled by how centred each bin is: the centre measure
//!    `2·Re(L·R*) / (|L|² + |R|²)` mapped linearly from 0 at 0.3 to 1 at 0.8, since a lead is
//!    usually centred and backing parts are often panned or wide (moderately decorrelated
//!    energy such as the lead's stereo reverb stays mostly lead).
//! 5. `lead = iSTFT(mask · X)` and `backing = vocals − lead`, so the two add back to the stem.
//!
//! It is a heuristic. Harmonies in unison or at the octave share the lead's harmonics; a
//! backing part singing alone is taken for the lead when nothing else sings; pitch-tracker
//! errors go straight into the mask; a lead's hard-panned doubles count as backing.
//! On synthetic mixes (six MUSDB18 vocal excerpts as the lead, pitch-shifted copies or other
//! singers 6-9 dB down as backing) the median lead SI-SDR rose by 1.3 dB (harmonies) and
//! 0.5 dB (other singers) on mono input over calling the whole stem the lead, and by 3.4-4.4 dB
//! on stereo input with panned backing, about what a centre extraction alone gives there.
//! Results are reported as best effort.

use crate::stft::{istft, stft};
use crate::stream::{mono_of, Track};
use ferraus_core::resample::{resample_channel, Quality};
use ferraus_core::Result;
use ferraus_pitch::pyin::{self, PitchFrame, PyinOptions};
use ferraus_pitch::tracker::{PitchTracker, TrackOptions};
use realfft::num_complex::Complex32;

/// `result.cascades[].quality.method` of this stage.
pub const METHOD: &str = "melody_mask";
/// Registry name of the built-in stage.
pub const MODEL: &str = "separate/melody-mask";

const PITCH_RATE: u32 = 16_000;
const PITCH_HOP_S: f64 = 0.010;
const PITCH_CHUNK_S: f64 = 30.0;
const PITCH_CONTEXT_S: f64 = 1.0;

/// Tuning of the mask (defaults chosen on synthetic lead + harmony mixes).
#[derive(Clone, Debug)]
pub struct Options {
    /// Pitch range searched by the tracker (Hz).
    pub fmin: f64,
    pub fmax: f64,
    /// Main-lobe half width of the analysis window as a Gaussian sigma, in bins.
    pub lobe_bins: f64,
    /// Pitch tolerance around each harmonic (cents), on top of the main lobe.
    pub tolerance_cents: f64,
    /// Highest frequency the harmonic comb and the peak search cover (Hz).
    pub comb_max_hz: f64,
    /// Time constant of the comb's memory (keeps the lead's ringing harmonics), seconds.
    pub memory_s: f64,
    /// Half width (bins) of a spectral peak, and of the local-mean window around it.
    pub peak_lobe_bins: usize,
    pub floor_bins: usize,
    /// Peak prominence over the local mean (dB) where a peak starts to count as another
    /// voice's harmonic, and where it counts fully.
    pub peak_min_db: f64,
    pub peak_full_db: f64,
    /// Share of non-tonal energy (consonants, breath, reverb) given to the lead.
    pub diffuse_gain: f64,
    /// In voiced lead frames below `gap_max_hz`, the share of the energy between the lead's
    /// harmonics given to the lead.
    pub gap_gain: f64,
    pub gap_max_hz: f64,
    /// Exponent on the mapped centre measure of stereo input (0 ignores stereo).
    pub stereo_power: f64,
    /// Exponent (times the above) of the centre weighting applied to energy off the comb as
    /// well (0 = comb bins only).
    pub stereo_diffuse: f64,
    /// Centre measure at or below which a bin counts as fully off-centre, and at or above
    /// which it counts as centred.
    pub stereo_lo: f64,
    pub stereo_hi: f64,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            fmin: 60.0,
            fmax: 1200.0,
            lobe_bins: 2.0,
            tolerance_cents: 15.0,
            comb_max_hz: 6000.0,
            memory_s: 0.15,
            peak_lobe_bins: 2,
            floor_bins: 6,
            peak_min_db: 4.0,
            peak_full_db: 10.0,
            diffuse_gain: 1.0,
            gap_gain: 0.2,
            gap_max_hz: 4000.0,
            stereo_power: 1.0,
            stereo_diffuse: 1.0,
            stereo_lo: 0.3,
            stereo_hi: 0.8,
        }
    }
}

/// The lead pitch on a uniform grid (`hop_s` apart, frame `k` centred on `k·hop_s`).
#[derive(Clone, Debug, Default)]
pub struct F0Track {
    pub hop_s: f64,
    /// Hz; 0 where unvoiced.
    pub f0: Vec<f32>,
    /// Voicing confidence of the tracker (0..1).
    pub conf: Vec<f32>,
    /// Frame RMS of the mono mix (linear).
    pub rms: Vec<f32>,
}

impl F0Track {
    fn len(&self) -> usize {
        self.f0.len()
    }

    /// Pitch at time `t` (s): interpolated between two voiced grid points, else the nearest.
    pub fn f0_at(&self, t: f64) -> f64 {
        if self.f0.is_empty() || t < 0.0 {
            return 0.0;
        }
        let x = t / self.hop_s;
        let i = x.floor() as usize;
        if i + 1 >= self.len() {
            return self.f0.last().map(|v| *v as f64).unwrap_or(0.0);
        }
        let (a, b) = (self.f0[i] as f64, self.f0[i + 1] as f64);
        let w = x - i as f64;
        if a > 0.0 && b > 0.0 {
            // geometric interpolation (pitch is logarithmic)
            (a.ln() * (1.0 - w) + b.ln() * w).exp()
        } else if w < 0.5 {
            a
        } else {
            b
        }
    }

    fn voiced(&self, g: usize) -> bool {
        self.f0.get(g).map(|f| *f > 0.0).unwrap_or(false)
    }
}

/// pYIN tuned for a lead over other voices: a more permissive threshold prior (mean 0.3
/// instead of 0.15) keeps the lead voiced where backing voices blur the YIN dips. On the
/// synthetic evaluation set it matched the CREPE and SwiftF0 trackers.
#[derive(Clone, Copy, Debug, Default)]
pub struct LeadPyin;

impl PitchTracker for LeadPyin {
    fn name(&self) -> &str {
        "pyin"
    }
    fn track(&self, samples: &[f32], sr: u32, opts: &TrackOptions) -> Result<Vec<PitchFrame>> {
        let o =
            PyinOptions { fmin: opts.fmin, fmax: opts.fmax, hop_s: opts.hop_s, beta_mean: 0.3, ..Default::default() };
        Ok(pyin::track(samples, sr, &o))
    }
}

/// Track the dominant pitch of a (possibly spilled) stem. `tick(fraction)` runs per chunk.
pub fn track_f0(
    tracker: &dyn PitchTracker,
    src: &Track,
    sr: u32,
    opts: &Options,
    tick: &mut dyn FnMut(f64) -> Result<()>,
) -> Result<F0Track> {
    let n = src.frames;
    let hop = PITCH_HOP_S;
    let n_grid = if n == 0 { 0 } else { (n as f64 / sr as f64 / hop).ceil() as usize + 1 };
    let mut out = F0Track { hop_s: hop, f0: vec![0.0; n_grid], conf: vec![0.0; n_grid], rms: vec![0.0; n_grid] };
    let chunk = ((PITCH_CHUNK_S * sr as f64) as usize).max(1);
    let context = (PITCH_CONTEXT_S * sr as f64) as usize;
    let topts = TrackOptions { fmin: opts.fmin, fmax: opts.fmax, hop_s: hop };
    let mut a = 0usize;
    while a < n {
        let b = (a + chunk).min(n);
        let (ra, rb) = (a.saturating_sub(context), (b + context).min(n));
        let mono = mono_of(&src.read(ra, rb)?);
        let x = if sr == PITCH_RATE { mono } else { resample_channel(&mono, sr, PITCH_RATE, Quality::High) };
        let frames = tracker.track(&x, PITCH_RATE, &topts)?;
        let t0 = ra as f64 / sr as f64;
        let (ta, tb) = (a as f64 / sr as f64, b as f64 / sr as f64);
        for fr in frames {
            let g = ((t0 + fr.time_s) / hop).round();
            if g < 0.0 {
                continue;
            }
            let g = g as usize;
            let gt = g as f64 * hop;
            // each grid point is owned by the chunk whose core holds it (the last one to the end)
            if g >= n_grid || gt < ta - 1e-9 || (gt >= tb - 1e-9 && b < n) {
                continue;
            }
            let f = fr.freq_hz;
            out.f0[g] = if fr.voiced && f.is_finite() && f > 0.0 { f as f32 } else { 0.0 };
            out.conf[g] = fr.confidence.clamp(0.0, 1.0) as f32;
            out.rms[g] = fr.rms as f32;
        }
        tick(b as f64 / n as f64)?;
        a = b;
    }
    Ok(out)
}

/// What the split measured, for the quality report and warnings.
#[derive(Clone, Debug, Default)]
pub struct Stats {
    /// Grid frames with signal (within 40 dB of the loudest frame).
    pub active_frames: usize,
    /// Active frames with a voiced lead pitch.
    pub voiced_frames: usize,
    /// Mean tracker confidence over voiced active frames.
    pub mean_confidence: f64,
    /// Energy of the stem, the lead, and the backing (sums over all channels).
    pub vocals_energy: f64,
    pub lead_energy: f64,
    pub backing_energy: f64,
    /// STFT size and hop used.
    pub n_fft: usize,
    pub hop: usize,
    pub stereo: bool,
}

impl Stats {
    pub fn voiced_fraction(&self) -> f64 {
        if self.active_frames == 0 {
            0.0
        } else {
            self.voiced_frames as f64 / self.active_frames as f64
        }
    }
    /// Backing energy relative to the stem (dB).
    pub fn backing_ratio_db(&self) -> f64 {
        10.0 * (self.backing_energy.max(1e-20) / self.vocals_energy.max(1e-20)).log10()
    }
    pub fn lead_ratio_db(&self) -> f64 {
        10.0 * (self.lead_energy.max(1e-20) / self.vocals_energy.max(1e-20)).log10()
    }
}

/// STFT size for a rate: about 93 ms (4096 at 44.1/48 kHz), a power of two.
pub fn n_fft_for(sr: u32) -> usize {
    let exp = (sr.max(1) as f64 * 0.0929).log2().round().clamp(9.0, 14.0);
    1usize << exp as u32
}

/// Voicing statistics of a pitch track (counted on the grid).
fn voicing_stats(f0: &F0Track, st: &mut Stats) {
    let peak = f0.rms.iter().cloned().fold(0f32, f32::max) as f64;
    let floor = (peak * 0.01).max(1e-5);
    let mut conf = 0.0;
    for g in 0..f0.len() {
        if (f0.rms[g] as f64) < floor {
            continue;
        }
        st.active_frames += 1;
        if f0.voiced(g) {
            st.voiced_frames += 1;
            conf += f0.conf[g] as f64;
        }
    }
    st.mean_confidence = if st.voiced_frames > 0 { conf / st.voiced_frames as f64 } else { 0.0 };
}

/// Split `src` into `lead` and `backing` (tracks of the same shape), block by block with
/// `n_fft` of margin so the result equals a whole-signal STFT. `backing = src − lead` exactly.
#[allow(clippy::too_many_arguments)]
pub fn split(
    src: &Track,
    sr: u32,
    f0: &F0Track,
    opts: &Options,
    block: usize,
    lead: &mut Track,
    backing: &mut Track,
    tick: &mut dyn FnMut(f64) -> Result<()>,
) -> Result<Stats> {
    let (n, ch) = (src.frames, src.channels);
    let n_fft = n_fft_for(sr);
    let hop = n_fft / 4;
    let mut st = Stats { n_fft, hop, stereo: ch == 2, ..Default::default() };
    voicing_stats(f0, &mut st);
    if n == 0 {
        return Ok(st);
    }
    let masker = Masker::new(f0, sr, n_fft, opts);
    let block = (block / hop).max(4) * hop;
    let margin = n_fft;
    let mut a = 0usize;
    while a < n {
        let b = (a + block).min(n);
        let (ra, rb) = (a.saturating_sub(margin), (b + margin).min(n));
        let data = src.read(ra, rb)?;
        let mut specs: Vec<_> = data.iter().map(|c| stft(c, n_fft, hop)).collect();
        let n_frames = specs[0].frames.len();
        let t_of = |f: isize| (ra as f64 + f as f64 * hop as f64) / sr as f64;
        // the comb's memory starts `horizon` frames before the block
        let mut memory = vec![0f32; masker.bins];
        let mut comb = vec![0f32; masker.bins];
        for f in -(masker.horizon as isize)..0 {
            masker.comb(t_of(f), &mut comb);
            masker.remember(&mut memory, &comb);
        }
        let mut mask = vec![0f32; n_fft / 2 + 1];
        for f in 0..n_frames {
            let voiced = masker.comb(t_of(f as isize), &mut comb);
            masker.remember(&mut memory, &comb);
            let bins: Vec<&[Complex32]> = specs.iter().map(|s| s.frames[f].as_slice()).collect();
            masker.mask(&bins, &memory, voiced, &mut mask);
            for sp in specs.iter_mut() {
                for (v, m) in sp.frames[f].iter_mut().zip(&mask) {
                    *v *= *m;
                }
            }
        }
        let mut lead_blk = Vec::with_capacity(ch);
        let mut back_blk = Vec::with_capacity(ch);
        for (c, sp) in specs.iter().enumerate() {
            let y = istft(sp, rb - ra);
            let core = &y[a - ra..b - ra];
            let x = &data[c][a - ra..b - ra];
            let bk: Vec<f32> = x.iter().zip(core).map(|(x, l)| x - l).collect();
            for ((x, l), k) in x.iter().zip(core).zip(&bk) {
                st.vocals_energy += (*x as f64) * (*x as f64);
                st.lead_energy += (*l as f64) * (*l as f64);
                st.backing_energy += (*k as f64) * (*k as f64);
            }
            lead_blk.push(core.to_vec());
            back_blk.push(bk);
        }
        lead.write(a, &lead_blk)?;
        backing.write(a, &back_blk)?;
        tick(b as f64 / n as f64)?;
        a = b;
    }
    Ok(st)
}

/// Per-frame mask computation.
struct Masker<'a> {
    f0: &'a F0Track,
    o: &'a Options,
    sr: f64,
    n_fft: usize,
    bin_hz: f64,
    /// Bins covered by the comb and the peak search.
    bins: usize,
    /// Per-frame decay of the comb memory, and the frames it reaches back.
    decay: f32,
    horizon: usize,
}

impl<'a> Masker<'a> {
    fn new(f0: &'a F0Track, sr: u32, n_fft: usize, o: &'a Options) -> Masker<'a> {
        let bin_hz = sr as f64 / n_fft as f64;
        let top = o.comb_max_hz.min(sr as f64 / 2.0);
        let bins = ((top / bin_hz).floor() as usize + 1).min(n_fft / 2 + 1);
        let hop_s = (n_fft / 4) as f64 / sr as f64;
        let (decay, horizon) = if o.memory_s > 0.0 {
            let d = (-hop_s / o.memory_s).exp();
            // frames until the memory falls below 1 %
            (d as f32, (0.01f64.ln() / d.ln()).ceil() as usize)
        } else {
            (0.0, 0)
        };
        Masker { f0, o, sr: sr as f64, n_fft, bin_hz, bins, decay, horizon }
    }

    /// The lead's harmonic comb at time `t` (zeros when unvoiced).
    fn comb(&self, t: f64, out: &mut [f32]) -> bool {
        out.iter_mut().for_each(|v| *v = 0.0);
        let p = self.f0.f0_at(t);
        if p <= 0.0 {
            return false;
        }
        // pitch movement within the window's effective length (half the window)
        let half = self.n_fft as f64 / self.sr / 4.0;
        let (p0, p1) = (self.f0.f0_at(t - half), self.f0.f0_at(t + half));
        let glide = if p0 > 0.0 && p1 > 0.0 { (p1 - p0).abs() / p } else { 0.0 };
        let rel = (2f64.powf(self.o.tolerance_cents / 1200.0) - 1.0) + glide / 2.0;
        let lobe = self.o.lobe_bins * self.bin_hz;
        for (k, m) in out.iter_mut().enumerate() {
            let fk = k as f64 * self.bin_hz;
            let h = (fk / p).round().max(1.0);
            let d = fk - h * p;
            let spread = h * p * rel;
            *m = (-0.5 * d * d / (lobe * lobe + spread * spread)).exp() as f32;
        }
        true
    }

    fn remember(&self, memory: &mut [f32], comb: &[f32]) {
        for (m, c) in memory.iter_mut().zip(comb) {
            *m = c.max(*m * self.decay);
        }
    }

    /// The lead mask of one frame: comb bins stay lead (scaled by how centred they are for
    /// stereo input), spectral peaks off the comb (other voices' harmonics) go to the backing,
    /// and the remaining, non-tonal energy (consonants, breath, reverb) stays lead.
    fn mask(&self, bins: &[&[Complex32]], comb: &[f32], voiced: bool, mask: &mut [f32]) {
        let nb = mask.len();
        let power: Vec<f32> = (0..nb).map(|k| bins.iter().map(|b| b[k].norm_sqr()).sum()).collect();
        // tonal peaks below the comb's top: prominence over the local mean power
        let w = self.o.floor_bins.max(1);
        let lobe = self.o.peak_lobe_bins;
        let mut tonal = vec![0f32; nb];
        let mut acc = 0f64;
        let (mut lo, mut hi) = (0usize, 0usize);
        for k in 0..self.bins.min(nb) {
            // sliding sum of power over [k - w, k + w]
            let (nlo, nhi) = (k.saturating_sub(w), (k + w + 1).min(nb));
            while hi < nhi {
                acc += power[hi] as f64;
                hi += 1;
            }
            while lo < nlo {
                acc -= power[lo] as f64;
                lo += 1;
            }
            let p = power[k];
            if p <= 1e-20 {
                continue;
            }
            let is_peak = (1..=lobe).all(|j| (k < j || power[k - j] <= p) && (k + j >= nb || power[k + j] <= p));
            if !is_peak {
                continue;
            }
            let mean = (acc / (nhi - nlo) as f64).max(1e-30);
            let prom = 10.0 * (p as f64 / mean).log10();
            let span = (self.o.peak_full_db - self.o.peak_min_db).max(1e-6);
            let wgt = ((prom - self.o.peak_min_db) / span).clamp(0.0, 1.0) as f32;
            if wgt > 0.0 {
                for t in tonal.iter_mut().take((k + lobe + 1).min(nb)).skip(k.saturating_sub(lobe)) {
                    *t = t.max(wgt);
                }
            }
        }
        let stereo = bins.len() == 2 && self.o.stereo_power > 0.0;
        let diffuse = self.o.diffuse_gain as f32;
        let gap_bins = if voiced { (self.o.gap_max_hz / self.bin_hz).ceil() as usize } else { 0 };
        let gap = self.o.gap_gain as f32;
        for k in 0..nb {
            let other = if k < gap_bins { gap } else { (1.0 - tonal[k]) * diffuse };
            let c = if k < self.bins { comb[k] } else { 0.0 };
            let mut s = 1f32;
            if stereo {
                let (l, r) = (bins[0][k], bins[1][k]);
                let den = l.norm_sqr() + r.norm_sqr();
                if den > 1e-20 {
                    let c = 2.0 * (l.re * r.re + l.im * r.im) / den;
                    let x = (c - self.o.stereo_lo as f32) / (self.o.stereo_hi - self.o.stereo_lo).max(1e-3) as f32;
                    s = x.clamp(0.0, 1.0).powf(self.o.stereo_power as f32);
                }
            }
            let sd = if stereo && self.o.stereo_diffuse > 0.0 { s.powf(self.o.stereo_diffuse as f32) } else { 1.0 };
            mask[k] = c * s + (1.0 - c) * other * sd;
        }
    }
}

#[cfg(test)]
mod tests;
