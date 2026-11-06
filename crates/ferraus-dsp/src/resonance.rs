//! Dynamic resonance suppression: narrow spectral peaks are turned down only while they
//! stand out above their neighbourhood, so a resonance is followed as notes move where a
//! static EQ cut would follow nothing.
//!
//! Method (STFT, ~43 ms sqrt-Hann window, hop 1/4, zero latency):
//!
//! 1. Per bin, the power (summed over channels when linked) is smoothed in time with the
//!    attack/release times *before* detection: raw per-frame spectra of noise fluctuate by
//!    about 5 dB from bin to bin, which would make noise "resonate".
//! 2. The reference a bin is compared with comes from a peak envelope: the smoothed
//!    spectrum max-filtered over ±250 Hz (±1/24 octave where that is wider, at most a
//!    quarter of the frequency), so the gaps between the harmonics of notes up to about C5
//!    are bridged and harmonics are compared with harmonics, not with the quiet gaps
//!    between them. On each side of the bin the envelope is averaged over the
//!    neighbourhood (its width set by the sharpness), starting beyond the bin's core (its
//!    main lobe plus ±10% of its frequency), so a resonance is not compared with its own
//!    flanks. The two sides are averaged in dB, so a straight slope through the bin is not
//!    a peak; as they differ by 6 to 12 dB the louder side takes over, and at a spectral
//!    edge (sides 10 to 20 dB or more apart, or one missing: the lowest harmonic of a note,
//!    the top of a slope) up to 6 dB more excess is demanded.
//! 3. excess = level − reference − threshold; reduction = clamp(excess, 0, depth).
//! 4. The reduction is smoothed across frequency with a closing (running max, then running
//!    mean) about 1/12 octave wide: the whole peak is covered at full depth, and the curve
//!    has no isolated per-bin dips (musical noise). Outside the band it fades out over a
//!    quarter octave.
//! 5. Only the removed part, X·(1 − gain), is resynthesized and subtracted from the input,
//!    so wherever nothing is reduced the output is the input, bit for bit.

use crate::stft::Stft;
use realfft::num_complex::Complex;
use std::collections::VecDeque;

/// Settings for [`deresonate`].
#[derive(Clone, Debug)]
pub struct DeresonateParams {
    /// Frequency range worked on, in Hz; the reduction fades out over a quarter octave
    /// beyond each edge.
    pub band_hz: (f64, f64),
    /// Largest reduction at any frequency, in dB (positive).
    pub depth_db: f64,
    /// How far a peak must stand above its neighbourhood before it is turned down, in dB.
    pub threshold_db: f64,
    /// Width of the neighbourhood: 0 → about one octave (broad build-ups count too),
    /// 1 → about 1/12 octave, never under the ±250 Hz bridge (only narrow rings count).
    pub sharpness: f64,
    /// Time constant for a rise in a bin's power.
    pub attack_s: f64,
    /// Time constant for a fall in a bin's power.
    pub release_s: f64,
    /// One gain curve for all channels, detected from their summed power.
    pub link: bool,
    /// Output only what is removed (input − output).
    pub listen: bool,
}

impl Default for DeresonateParams {
    fn default() -> Self {
        DeresonateParams {
            band_hz: (100.0, 16000.0),
            depth_db: 6.0,
            threshold_db: 3.0,
            sharpness: 0.5,
            attack_s: 0.005,
            release_s: 0.08,
            link: true,
            listen: false,
        }
    }
}

/// Mean and largest reduction in one sixth-octave band.
#[derive(Clone, Debug, PartialEq)]
pub struct BandReduction {
    /// Band centre (Hz).
    pub hz: f64,
    /// Energy-weighted mean reduction over the clip (dB).
    pub mean_db: f64,
    /// Largest energy-weighted reduction in any one frame (dB).
    pub max_db: f64,
}

/// What [`deresonate`] turned down.
#[derive(Clone, Debug, Default)]
pub struct DeresonateStats {
    /// Energy-weighted mean reduction inside the band (dB).
    pub reduction_mean_db: f64,
    /// Largest reduction at any frequency and time (dB).
    pub reduction_max_db: f64,
    /// Share of (non-silent) frames in which some sixth-octave band is turned down by more
    /// than 0.5 dB.
    pub active_share: f64,
    /// Up to five sixth-octave bands with the most mean reduction, most first.
    pub top: Vec<BandReduction>,
    /// Analysis window length (samples).
    pub fft_size: usize,
}

/// Analysis window for a sample rate: the power of two closest to 43 ms (2048 at 44.1 and
/// 48 kHz, 4096 at 96 kHz), at least 256 samples.
pub fn fft_size_for(sample_rate: f64) -> usize {
    let exp = (sample_rate * 0.043).max(256.0).log2().round() as u32;
    (1usize << exp).clamp(256, 32768)
}

/// Half-width of the peak-envelope bridge (see `bridge_hz`).
const BRIDGE_HZ: f64 = 250.0;

/// Half-width of a bin's core, left out of its neighbourhood, as a fraction of its frequency.
const CORE_FRACTION: f64 = 0.1;

/// Extra excess demanded at a spectral edge, where one side of the neighbourhood is far
/// quieter than the other: it ramps in from `EDGE_START_DB` of difference between the sides
/// to `EDGE_START_DB + EDGE_RAMP_DB`. A sawtooth's fundamental stands 6 dB above its second
/// harmonic with nothing below it; that is a slope, not a resonance.
const EDGE_EXTRA_DB: f64 = 6.0;
const EDGE_START_DB: f64 = 10.0;
const EDGE_RAMP_DB: f64 = 10.0;

/// A frame with this little power (relative to the window length) counts as silent.
fn power_floor(size: usize) -> f64 {
    (size as f64).powi(2) * 1e-14
}

/// Per-bin windows that stay the same for every frame.
struct Geometry {
    /// Peak-envelope max filter: bins `mlo[k]..=mhi[k]`.
    mlo: Vec<usize>,
    mhi: Vec<usize>,
    /// Neighbourhood on each side, inclusive; empty when start > end.
    left: Vec<(usize, usize)>,
    right: Vec<(usize, usize)>,
    /// Reduction smoothing (closing) window: bins `slo[k]..=shi[k]`.
    slo: Vec<usize>,
    shi: Vec<usize>,
    /// Band weight (1 inside, fading to 0 a quarter octave outside).
    taper: Vec<f64>,
    /// Detection runs only on bins `first..=last` (where the taper is above zero).
    first: usize,
    last: usize,
    /// Sixth-octave band of each bin inside the band, `usize::MAX` outside.
    band_of: Vec<usize>,
    centres: Vec<f64>,
}

/// Half-width in Hz of the peak-envelope max filter at frequency `f`: ±250 Hz, enough to
/// bridge the gaps between the harmonics of notes up to about C5 (523 Hz); ±1/24 octave
/// where that is wider; and never more than a quarter of the frequency (still bridges every
/// harmonic above the fundamental, keeps a loud bass from being smeared up into the low
/// mids, and leaves room for a neighbourhood below the bin).
fn bridge_hz(f: f64) -> f64 {
    BRIDGE_HZ.max(f * (2f64.powf(1.0 / 24.0) - 1.0)).min(0.25 * f)
}

/// Make window bounds non-decreasing so a monotone-deque running max can sweep them.
fn monotone(v: &mut [usize]) {
    for k in 1..v.len() {
        v[k] = v[k].max(v[k - 1]);
    }
}

impl Geometry {
    fn new(bins: usize, df: f64, p: &DeresonateParams, nyquist: f64) -> Geometry {
        let (lo, hi) = (p.band_hz.0, p.band_hz.1.min(nyquist));
        let s = p.sharpness.clamp(0.0, 1.0);
        let width_oct = 12f64.powf(-s);
        let half = 2f64.powf(width_oct / 2.0) - 1.0;
        let smooth = 2f64.powf(1.0 / 24.0) - 1.0;
        let fade_oct = 0.25;
        let mut g = Geometry {
            mlo: vec![0; bins],
            mhi: vec![0; bins],
            left: vec![(1, 0); bins],
            right: vec![(1, 0); bins],
            slo: vec![0; bins],
            shi: vec![0; bins],
            taper: vec![0.0; bins],
            first: bins,
            last: 0,
            band_of: vec![usize::MAX; bins],
            centres: Vec::new(),
        };
        let j_lo = (6.0 * (lo / 1000.0).log2()).round() as i64;
        let mut m = vec![1usize; bins];
        for k in 0..bins {
            m[k] = ((bridge_hz(k as f64 * df) / df).round() as usize).max(1);
            g.mlo[k] = k.saturating_sub(m[k]);
            g.mhi[k] = (k + m[k]).min(bins - 1);
        }
        monotone(&mut g.mlo);
        monotone(&mut g.mhi);
        for k in 0..bins {
            let f = k as f64 * df;
            // the neighbourhood on each side starts at the first envelope bin whose
            // max-filter window leaves out this bin's core (the main lobe, ±2 bins, plus
            // ±10% of the frequency up to the bridging width, so a resonance up to about a
            // fifth of an octave wide is not compared with its own flanks) and is the
            // sharpness width (at least one bridging width) wide, at most down to a third
            // and up to twice the frequency
            let width = ((f * half / df).ceil() as usize).max(m[k]);
            let core = 2 + m[k].min((CORE_FRACTION * k as f64).round() as usize);
            let mut l1 = k.saturating_sub(3);
            while l1 > 0 && g.mhi[l1] + core >= k {
                l1 -= 1;
            }
            let l0 = l1.saturating_sub(width).max(k.div_ceil(3)).max(1);
            if l1 >= 1 && g.mhi[l1] + core < k && l0 <= l1 {
                g.left[k] = (l0, l1);
            }
            let mut r0 = k + 3;
            while r0 < bins && g.mlo[r0] <= k + core {
                r0 += 1;
            }
            let r1 = (r0 + width).min(2 * k).min(bins - 1);
            if r0 <= r1 {
                g.right[k] = (r0, r1);
            }
            let d = ((f * smooth / df).round() as usize).max(2);
            g.slo[k] = k.saturating_sub(d);
            g.shi[k] = (k + d).min(bins - 1);
            if f <= 0.0 {
                continue;
            }
            let t = if f >= lo && f <= hi {
                1.0
            } else {
                let dist = if f < lo { (lo / f).log2() } else { (f / hi).log2() };
                if dist < fade_oct {
                    0.5 + 0.5 * (std::f64::consts::PI * dist / fade_oct).cos()
                } else {
                    0.0
                }
            };
            g.taper[k] = t;
            if t > 0.0 {
                g.first = g.first.min(k);
                g.last = k;
            }
            if f >= lo && f <= hi {
                let j = (6.0 * (f / 1000.0).log2()).round() as i64;
                let b = (j - j_lo).max(0) as usize;
                while g.centres.len() <= b {
                    let jj = j_lo + g.centres.len() as i64;
                    g.centres.push(1000.0 * 2f64.powf(jj as f64 / 6.0));
                }
                g.band_of[k] = b;
            }
        }
        monotone(&mut g.slo);
        monotone(&mut g.shi);
        g
    }
}

/// `out[k] = max(v[lo[k]..=hi[k]])` for non-decreasing `lo` and `hi` (monotone deque, O(n)).
fn running_max(v: &[f64], lo: &[usize], hi: &[usize], out: &mut [f64], dq: &mut VecDeque<usize>) {
    dq.clear();
    let mut next = 0;
    for k in 0..v.len() {
        while next <= hi[k] {
            while dq.back().is_some_and(|&b| v[b] <= v[next]) {
                dq.pop_back();
            }
            dq.push_back(next);
            next += 1;
        }
        while dq.front().is_some_and(|&f| f < lo[k]) {
            dq.pop_front();
        }
        out[k] = dq.front().map_or(0.0, |&i| v[i]);
    }
}

/// Inclusive prefix sums: `prefix[i]` = sum of `v[..i]`.
fn prefix_sums(v: &[f64], prefix: &mut [f64]) {
    prefix[0] = 0.0;
    for (i, x) in v.iter().enumerate() {
        prefix[i + 1] = prefix[i] + x;
    }
}

fn range_mean(prefix: &[f64], (a, b): (usize, usize)) -> Option<f64> {
    (a <= b).then(|| (prefix[b + 1] - prefix[a]) / (b + 1 - a) as f64)
}

/// Scratch buffers for one detection pass.
struct Detector {
    env: Vec<f64>,
    prefix: Vec<f64>,
    raw: Vec<f64>,
    dilated: Vec<f64>,
    dq: VecDeque<usize>,
}

impl Detector {
    fn new(bins: usize) -> Detector {
        Detector {
            env: vec![0.0; bins],
            prefix: vec![0.0; bins + 1],
            raw: vec![0.0; bins],
            dilated: vec![0.0; bins],
            dq: VecDeque::with_capacity(bins),
        }
    }

    /// Reduction in dB per bin (`out`) for the time-smoothed power spectrum `level`.
    /// Returns the largest value.
    fn reduction(&mut self, level: &[f64], g: &Geometry, p: &DeresonateParams, floor: f64, out: &mut [f64]) -> f64 {
        out.fill(0.0);
        if g.first > g.last || level[g.first..=g.last].iter().all(|&v| v <= floor) {
            return 0.0;
        }
        running_max(level, &g.mlo, &g.mhi, &mut self.env, &mut self.dq);
        prefix_sums(&self.env, &mut self.prefix);
        self.raw.fill(0.0);
        let mut any = false;
        let over = 10f64.powf(p.threshold_db / 10.0);
        for k in g.first..=g.last {
            if level[k] <= floor {
                continue;
            }
            let (left, right) = (range_mean(&self.prefix, g.left[k]), range_mean(&self.prefix, g.right[k]));
            // the reference: the two sides averaged in dB (a straight slope through the bin
            // gives its own level), moving to the louder side as they differ by 6 to 12 dB
            let (reference_db, gap_db) = match (left, right) {
                (Some(a), Some(b)) => {
                    let (a, b) = (10.0 * a.max(floor).log10(), 10.0 * b.max(floor).log10());
                    let (hi, gap) = (a.max(b), (a - b).abs());
                    let w = ((gap - 6.0) / 6.0).clamp(0.0, 1.0);
                    (hi - (1.0 - w) * gap / 2.0, gap)
                }
                (Some(a), None) | (None, Some(a)) => (10.0 * a.max(floor).log10(), f64::INFINITY),
                (None, None) => continue,
            };
            let ratio = level[k] / 10f64.powf(reference_db / 10.0);
            if ratio <= over {
                continue;
            }
            // at an edge (one side far quieter than the other, or missing) the bin may be
            // the lowest harmonic of a note or the top of a slope rather than a resonance:
            // demand up to EDGE_EXTRA_DB more
            let edge = EDGE_EXTRA_DB * ((gap_db - EDGE_START_DB) / EDGE_RAMP_DB).clamp(0.0, 1.0);
            let excess = 10.0 * ratio.log10() - p.threshold_db - edge;
            if excess > 0.0 {
                self.raw[k] = excess.min(p.depth_db);
                any = true;
            }
        }
        if !any {
            return 0.0;
        }
        // closing: running max then running mean over the same window keeps a peak's full
        // reduction across its main lobe and rounds the edges
        running_max(&self.raw, &g.slo, &g.shi, &mut self.dilated, &mut self.dq);
        prefix_sums(&self.dilated, &mut self.prefix);
        let mut max = 0.0f64;
        for k in g.first..=g.last {
            let r = range_mean(&self.prefix, (g.slo[k], g.shi[k])).unwrap_or(0.0) * g.taper[k];
            out[k] = r;
            max = max.max(r);
        }
        max
    }
}

/// Per-band accumulators for the statistics.
struct Meter {
    num: f64,
    den: f64,
    band_num: Vec<f64>,
    band_den: Vec<f64>,
    band_max: Vec<f64>,
    frame_num: Vec<f64>,
    frame_den: Vec<f64>,
    frames: usize,
    active: usize,
    max: f64,
}

impl Meter {
    fn new(bands: usize) -> Meter {
        Meter {
            num: 0.0,
            den: 0.0,
            band_num: vec![0.0; bands],
            band_den: vec![0.0; bands],
            band_max: vec![0.0; bands],
            frame_num: vec![0.0; bands],
            frame_den: vec![0.0; bands],
            frames: 0,
            active: 0,
            max: 0.0,
        }
    }

    /// Add one detector's frame: instantaneous power `power`, reduction `red` (dB).
    fn add(&mut self, g: &Geometry, power: &[f64], red: &[f64]) {
        for (k, &b) in g.band_of.iter().enumerate() {
            if b != usize::MAX {
                let w = power[k];
                self.frame_num[b] += w * red[k];
                self.frame_den[b] += w;
            }
        }
    }

    /// Close the current frame.
    fn end_frame(&mut self, floor: f64, frame_max: f64) {
        self.max = self.max.max(frame_max);
        let total: f64 = self.frame_den.iter().sum();
        if total > floor {
            self.frames += 1;
            let mut active = false;
            for b in 0..self.frame_num.len() {
                let (n, d) = (self.frame_num[b], self.frame_den[b]);
                self.num += n;
                self.den += d;
                self.band_num[b] += n;
                self.band_den[b] += d;
                if d > floor * 1e-3 {
                    let r = n / d;
                    self.band_max[b] = self.band_max[b].max(r);
                    active |= r > 0.5;
                }
            }
            if active {
                self.active += 1;
            }
        }
        self.frame_num.fill(0.0);
        self.frame_den.fill(0.0);
    }

    fn stats(&self, g: &Geometry, fft_size: usize) -> DeresonateStats {
        let mut top: Vec<BandReduction> = (0..self.band_num.len())
            .filter(|&b| self.band_den[b] > 0.0)
            .map(|b| BandReduction {
                hz: g.centres[b],
                mean_db: self.band_num[b] / self.band_den[b],
                max_db: self.band_max[b],
            })
            .filter(|r| r.mean_db >= 0.05)
            .collect();
        top.sort_by(|a, b| b.mean_db.total_cmp(&a.mean_db));
        top.truncate(5);
        DeresonateStats {
            reduction_mean_db: if self.den > 0.0 { self.num / self.den } else { 0.0 },
            reduction_max_db: self.max,
            active_share: if self.frames > 0 { self.active as f64 / self.frames as f64 } else { 0.0 },
            top,
            fft_size,
        }
    }
}

/// Smoothing coefficient per hop for a time constant.
fn coeff(tau_s: f64, hop: usize, sample_rate: f64) -> f64 {
    if tau_s > 0.0 {
        (-(hop as f64) / (tau_s * sample_rate)).exp()
    } else {
        0.0
    }
}

/// Turn down resonances in `data` (planar channels) in place; see the module docs.
/// The output has the input's length and no delay. With `depth_db` 0 the input is
/// returned untouched.
pub fn deresonate(data: &mut [Vec<f32>], sample_rate: f64, p: &DeresonateParams) -> DeresonateStats {
    let size = fft_size_for(sample_rate);
    let nch = data.len();
    let len = data.first().map_or(0, |c| c.len());
    if nch == 0 || len == 0 || p.depth_db <= 0.0 {
        if p.listen {
            data.iter_mut().for_each(|c| c.fill(0.0));
        }
        return DeresonateStats { fft_size: size, ..Default::default() };
    }
    let stft = Stft::new(size, size / 4);
    let bins = stft.bins();
    let df = sample_rate / size as f64;
    let geo = Geometry::new(bins, df, p, sample_rate / 2.0);
    let floor = power_floor(size);
    let (att, rel) = (coeff(p.attack_s, stft.hop, sample_rate), coeff(p.release_s, stft.hop, sample_rate));
    let detectors = if p.link { 1 } else { nch };

    let mut buf = vec![0.0f32; size];
    let mut specs = vec![vec![Complex::new(0.0f32, 0.0); bins]; nch];
    let mut power = vec![vec![0.0f64; bins]; detectors];
    let mut level = vec![vec![0.0f64; bins]; detectors];
    let mut red = vec![vec![0.0f64; bins]; detectors];
    let mut removal = vec![0.0f32; bins];
    let mut det = Detector::new(bins);
    let mut meter = Meter::new(geo.centres.len());
    let mut removed = vec![vec![0.0f32; len]; nch];

    for t in 0..stft.frame_count(len) {
        for (c, spec) in specs.iter_mut().enumerate() {
            stft.analyze_frame(&data[c], t, &mut buf, spec);
        }
        for pw in power.iter_mut() {
            pw.fill(0.0);
        }
        for (c, spec) in specs.iter().enumerate() {
            let pw = &mut power[if p.link { 0 } else { c }];
            for (a, z) in pw.iter_mut().zip(spec) {
                *a += z.norm_sqr() as f64;
            }
        }
        let mut frame_max = 0.0f64;
        for d in 0..detectors {
            for (s, &x) in level[d].iter_mut().zip(&power[d]) {
                let a = if x > *s { att } else { rel };
                *s = a * *s + (1.0 - a) * x;
                if *s < 1e-30 {
                    *s = 0.0;
                }
            }
            frame_max = frame_max.max(det.reduction(&level[d], &geo, p, floor, &mut red[d]));
            meter.add(&geo, &power[d], &red[d]);
        }
        meter.end_frame(floor, frame_max);
        if frame_max <= 0.0 {
            continue;
        }
        for (c, spec) in specs.iter_mut().enumerate() {
            let r = &red[if p.link { 0 } else { c }];
            if r.iter().all(|&v| v <= 0.0) {
                continue;
            }
            for (m, &db) in removal.iter_mut().zip(r) {
                *m = if db > 0.0 { 1.0 - 10f64.powf(-db / 20.0) as f32 } else { 0.0 };
            }
            for (z, &m) in spec.iter_mut().zip(&removal) {
                *z *= m;
            }
            stft.overlap_add_frame(spec, t, &mut buf, &mut removed[c]);
        }
    }

    for (ch, rem) in data.iter_mut().zip(removed) {
        if p.listen {
            *ch = rem;
        } else {
            for (x, r) in ch.iter_mut().zip(&rem) {
                *x -= r;
            }
        }
    }
    meter.stats(&geo, size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stft::welch_psd;
    use ferraus_core::testing::{chirp, noise, sine, tone_level};

    const SR: u32 = 48000;

    /// Pink-ish noise (Paul Kellet's filter on white noise), unit-ish RMS times `amp`.
    fn pink(secs: f64, amp: f32, seed: u64) -> Vec<f32> {
        let w = noise(SR, secs, 1.0, seed).data.remove(0);
        let (mut b0, mut b1, mut b2, mut b3, mut b4, mut b5, mut b6) = (0f32, 0f32, 0f32, 0f32, 0f32, 0f32, 0f32);
        let mut out: Vec<f32> = w
            .iter()
            .map(|&x| {
                b0 = 0.99886 * b0 + x * 0.0555179;
                b1 = 0.99332 * b1 + x * 0.0750759;
                b2 = 0.969 * b2 + x * 0.153852;
                b3 = 0.8665 * b3 + x * 0.3104856;
                b4 = 0.55 * b4 + x * 0.5329522;
                b5 = -0.7616 * b5 - x * 0.016898;
                let y = b0 + b1 + b2 + b3 + b4 + b5 + b6 + x * 0.5362;
                b6 = x * 0.115926;
                y
            })
            .collect();
        let rms = (out.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / out.len() as f64).sqrt() as f32;
        out.iter_mut().for_each(|v| *v *= amp / rms);
        out
    }

    fn add(a: &[f32], b: &[f32]) -> Vec<f32> {
        a.iter().zip(b).map(|(x, y)| x + y).collect()
    }

    fn run(x: Vec<f32>, p: &DeresonateParams) -> (Vec<f32>, DeresonateStats) {
        let mut d = vec![x];
        let st = deresonate(&mut d, SR as f64, p);
        (d.remove(0), st)
    }

    /// Power in dB of `x` between `lo` and `hi` Hz (Welch, 4096-point).
    fn band_db(x: &[f32], lo: f64, hi: f64) -> f64 {
        let n = 4096;
        let psd = welch_psd(x, n);
        let df = SR as f64 / n as f64;
        let e: f64 = psd
            .iter()
            .enumerate()
            .filter(|(k, _)| (*k as f64 * df) >= lo && (*k as f64 * df) < hi)
            .map(|(_, v)| v)
            .sum();
        10.0 * e.max(1e-30).log10()
    }

    /// Third-octave band change (dB) of `y` against `x` at centre `fc`.
    fn third_change(x: &[f32], y: &[f32], fc: f64) -> f64 {
        let (lo, hi) = (fc / 2f64.powf(1.0 / 6.0), fc * 2f64.powf(1.0 / 6.0));
        band_db(y, lo, hi) - band_db(x, lo, hi)
    }

    const THIRDS: [f64; 11] = [125.0, 250.0, 500.0, 1000.0, 2000.0, 3150.0, 4000.0, 6300.0, 8000.0, 10000.0, 12500.0];

    /// A sine at `f` whose spectral peak stands `above_db` above the noise `x` around it, as
    /// seen by the processor's own 2048-point window.
    fn tone_above(x: &[f32], f: f64, above_db: f64, secs: f64) -> Vec<f32> {
        let n = fft_size_for(SR as f64);
        // noise power per bin of a sqrt-Hann window: PSD (Hann Welch, per bin) scaled by the
        // window energies; sine peak bin power = (A·Σw/2)²
        let psd = welch_psd(x, n);
        let k = (f / (SR as f64 / n as f64)).round() as usize;
        let hann_e: f64 = crate::stft::hann(n).iter().map(|w| (*w as f64).powi(2)).sum();
        let sqrt_e: f64 = crate::stft::hann(n).iter().map(|w| *w as f64).sum();
        let sqrt_sum: f64 = crate::stft::hann(n).iter().map(|w| (*w as f64).sqrt()).sum();
        let noise_bin = psd[k - 3..=k + 3].iter().sum::<f64>() / 7.0 / hann_e * sqrt_e;
        let amp = (noise_bin * 10f64.powf(above_db / 10.0)).sqrt() * 2.0 / sqrt_sum;
        sine(SR, f, secs, amp as f32).data.remove(0)
    }

    #[test]
    fn fft_size_scales_with_the_rate() {
        assert_eq!(fft_size_for(44100.0), 2048);
        assert_eq!(fft_size_for(48000.0), 2048);
        assert_eq!(fft_size_for(96000.0), 4096);
        assert_eq!(fft_size_for(8000.0), 256);
    }

    #[test]
    fn a_steady_ring_in_noise_is_turned_down_and_nothing_else() {
        let secs = 3.0;
        let n = pink(secs, 0.1, 1);
        for above in [15.0, 20.0] {
            let x = add(&n, &tone_above(&n, 3000.0, above, secs));
            let (y, st) = run(x.clone(), &DeresonateParams::default());
            let tone =
                20.0 * (tone_level(&y[SR as usize..], SR, 3000.0) / tone_level(&x[SR as usize..], SR, 3000.0)).log10();
            assert!(tone < -4.5 && tone > -6.5, "+{above} dB ring: tone changed {tone:.2} dB");
            // the third-octave band holding the ring (the noise in it is about half its
            // energy at +15 dB) drops by about half that or more
            let band = third_change(&x, &y, 3150.0);
            assert!(band < -2.5, "+{above} dB ring: 3.15 kHz band {band:.2} dB");
            for fc in [500.0, 1000.0, 10000.0] {
                let d = third_change(&x, &y, fc);
                assert!(d.abs() < 0.5, "+{above} dB ring: {fc} Hz band moved {d:.2} dB");
            }
            assert!((st.reduction_max_db - 6.0).abs() < 0.01, "{}", st.reduction_max_db);
            let top = &st.top[0];
            assert!((top.hz - 3000.0).abs() < 400.0 && top.max_db > 4.0, "{:?}", st.top);
            assert!(st.active_share > 0.9, "{}", st.active_share);
        }
    }

    #[test]
    fn noise_is_left_alone() {
        let secs = 3.0;
        for (name, x) in [("white", noise(SR, secs, 0.3, 7).data.remove(0)), ("pink", pink(secs, 0.1, 8))] {
            let (y, st) = run(x.clone(), &DeresonateParams::default());
            for fc in THIRDS {
                let d = third_change(&x, &y, fc);
                assert!(d.abs() < 0.5, "{name}: {fc} Hz band moved {d:.2} dB");
            }
            assert!(st.reduction_mean_db < 0.1, "{name}: mean reduction {}", st.reduction_mean_db);
            assert!(st.active_share < 0.2, "{name}: active {}", st.active_share);
        }
    }

    #[test]
    fn the_band_limits_what_is_touched() {
        let secs = 2.0;
        let n = pink(secs, 0.1, 3);
        let x = add(&n, &tone_above(&n, 300.0, 20.0, secs));
        let tone = |y: &[f32]| {
            20.0 * (tone_level(&y[SR as usize..], SR, 300.0) / tone_level(&x[SR as usize..], SR, 300.0)).log10()
        };
        let (y, _) = run(x.clone(), &DeresonateParams { band_hz: (2000.0, 8000.0), ..Default::default() });
        assert!(tone(&y).abs() < 0.05, "300 Hz outside the band moved {:.3} dB", tone(&y));
        // the same ring inside the band is turned down
        let (y, _) = run(x.clone(), &DeresonateParams::default());
        assert!(tone(&y) < -4.0, "{}", tone(&y));
    }

    #[test]
    fn a_moving_ring_is_followed() {
        // a note gliding 200 → 400 Hz over 3 s, like a sung slide
        let secs = 3.0;
        let n = pink(secs, 0.05, 4);
        let glide = chirp(SR, 200.0, 400.0, secs, 0.2).data.remove(0);
        let x = add(&n, &glide);
        let (y, _) = run(x.clone(), &DeresonateParams::default());
        let seg = SR as usize / 4;
        for i in 1..11 {
            let (a, b) = (i * seg, (i + 1) * seg);
            let f = 200.0 + 200.0 * ((a + b) as f64 / 2.0 / SR as f64) / secs;
            let d = band_db(&y[a..b], f - 40.0, f + 40.0) - band_db(&x[a..b], f - 40.0, f + 40.0);
            assert!(d < -4.0, "{f:.0} Hz at {:.2} s: {d:.2} dB", a as f64 / SR as f64);
        }
        // where the note was at the start, only noise is left at the end: not turned down
        let (a, b) = (11 * seg, 12 * seg);
        let d = band_db(&y[a..b], 180.0, 240.0) - band_db(&x[a..b], 180.0, 240.0);
        assert!(d.abs() < 1.0, "200 Hz after the note left: {d:.2} dB");
    }

    #[test]
    fn passthrough_alignment_and_listen() {
        let secs = 1.5;
        let n = pink(secs, 0.1, 5);
        let x = add(&n, &tone_above(&n, 2000.0, 20.0, secs));
        // depth 0: the input itself
        let (y, st) = run(x.clone(), &DeresonateParams { depth_db: 0.0, ..Default::default() });
        assert_eq!(y, x);
        assert_eq!(st.reduction_max_db, 0.0);
        // nothing above threshold: bit-exact as well (only removed parts are resynthesized)
        let (y, st) = run(x.clone(), &DeresonateParams { threshold_db: 60.0, ..Default::default() });
        assert_eq!(st.reduction_max_db, 0.0);
        assert_eq!(y, x);
        // processing keeps the length, adds no delay, and listen + output = input
        let (y, _) = run(x.clone(), &DeresonateParams::default());
        let (l, _) = run(x.clone(), &DeresonateParams { listen: true, ..Default::default() });
        assert_eq!(y.len(), x.len());
        assert_eq!(l.len(), x.len());
        let err = x.iter().zip(y.iter().zip(&l)).map(|(a, (b, c))| (a - b - c).abs()).fold(0.0, f32::max);
        assert!(err < 1e-6, "{err}");
        // what is removed is the ring itself, in phase with the input: the removed signal
        // correlates best with the input's tone at lag 0
        let t = sine(SR, 2000.0, secs, 1.0).data.remove(0);
        let corr = |lag: isize| -> f64 {
            (SR as usize / 4..x.len() - SR as usize / 4)
                .map(|i| l[i] as f64 * t[(i as isize + lag) as usize] as f64)
                .sum()
        };
        let best = (-12isize..=12).max_by(|a, b| corr(*a).total_cmp(&corr(*b))).unwrap();
        assert_eq!(best, 0);
    }

    #[test]
    fn stereo_link_turns_both_channels_down_together() {
        let secs = 2.0;
        let nl = pink(secs, 0.1, 6);
        let nr = pink(secs, 0.1, 9);
        let left = add(&nl, &tone_above(&nl, 3000.0, 20.0, secs));
        let x = vec![left.clone(), nr.clone()];
        let narrow = |c: &[f32], d: &[f32]| {
            band_db(&d[SR as usize / 2..], 2950.0, 3050.0) - band_db(&c[SR as usize / 2..], 2950.0, 3050.0)
        };
        let mut y = x.clone();
        deresonate(&mut y, SR as f64, &DeresonateParams::default());
        let (dl, dr) = (narrow(&x[0], &y[0]), narrow(&x[1], &y[1]));
        assert!(dl < -4.0, "left {dl:.2}");
        assert!((dl - dr).abs() < 1.0, "linked: left {dl:.2} dB, right {dr:.2} dB");
        // unlinked: the right channel has no ring and keeps its level
        let mut y = x.clone();
        deresonate(&mut y, SR as f64, &DeresonateParams { link: false, ..Default::default() });
        let (dl, dr) = (narrow(&x[0], &y[0]), narrow(&x[1], &y[1]));
        assert!(dl < -4.0, "left {dl:.2}");
        assert!(dr.abs() < 0.5, "unlinked right {dr:.2}");
    }

    #[test]
    fn harmonics_of_an_even_tone_are_not_resonances() {
        // a sawtooth-like tone (harmonics at 1/n) with no resonance: compared with each
        // other, its harmonics do not stick out, at any sharpness; raising the two harmonics
        // near 2.5 kHz by 7-10 dB makes that region the one that is turned down
        let secs = 2.0;
        let n = (SR as f64 * secs) as usize;
        let f0 = 220.0;
        let tone = |boost: f64| -> Vec<f32> {
            (0..n)
                .map(|i| {
                    let t = i as f64 / SR as f64;
                    (1..60)
                        .map(|h| {
                            let f = f0 * h as f64;
                            let g = 1.0 + boost * (-((f - 2500.0) / 150.0).powi(2)).exp();
                            g * (2.0 * std::f64::consts::PI * f * t + h as f64).sin() / h as f64
                        })
                        .sum::<f64>() as f32
                        * 0.05
                })
                .collect()
        };
        for sharpness in [0.0, 0.5, 1.0] {
            let p = DeresonateParams { sharpness, ..Default::default() };
            let (_, flat) = run(tone(0.0), &p);
            assert!(flat.reduction_mean_db < 0.1, "even tone at {sharpness}: {flat:?}");
        }
        let (_, peaked) = run(tone(3.0), &DeresonateParams::default());
        let top = &peaked.top[0];
        assert!((top.hz - 2500.0).abs() < 300.0 && top.mean_db > 1.5, "{:?}", peaked.top);
    }

    #[test]
    fn silence_and_tiny_inputs() {
        let (y, st) = run(vec![0.0; 4800], &DeresonateParams::default());
        assert!(y.iter().all(|v| *v == 0.0));
        assert_eq!(st.active_share, 0.0);
        let (y, _) = run(vec![0.1, -0.2, 0.3], &DeresonateParams::default());
        assert_eq!(y.len(), 3);
        assert!(y.iter().all(|v| v.is_finite()));
        let mut empty: Vec<Vec<f32>> = vec![vec![]];
        deresonate(&mut empty, 48000.0, &DeresonateParams::default());
        // other sample rates use a window of about the same duration
        let x = sine(22050, 1000.0, 1.0, 0.3).data.remove(0);
        let mut d = vec![x.clone()];
        let st = deresonate(&mut d, 22050.0, &DeresonateParams::default());
        assert_eq!(st.fft_size, 1024);
        assert_eq!(d[0].len(), x.len());
    }
}
