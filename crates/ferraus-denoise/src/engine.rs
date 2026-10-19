//! The spectral gating engine shared by profile and adaptive modes.
//!
//! Per STFT frame (Hann, 75% overlap):
//!
//! 1. The detector level of each bin is the power averaged over the bin and its two
//!    neighbours, in dB.
//! 2. The threshold is `mean + sensitivity × std` from the profile. Sensitivity is measured
//!    in "dB for stationary Gaussian noise": the profile's dB standard deviation is divided
//!    by 5.57 dB (the spread of Rayleigh-distributed magnitudes), and the mean is taken on
//!    the power scale (mean of dB + 2.51 dB, exact for Gaussian noise). So the default
//!    `--sensitivity 6` puts the threshold 6 dB above the noise's mean power for steady
//!    hiss, and proportionally higher for noise that fluctuates more. In adaptive mode the
//!    mean comes from a minimum-statistics tracker instead.
//! 3. The excess over threshold is smoothed across `±smoothing` bins (triangular kernel)
//!    and mapped through a soft sigmoid to a gain between 0 dB and `-reduction`.
//! 4. `--artifact-control` blends a 3×3 (time × frequency) median of the gain mask into it.
//! 5. A lookahead maximum over the next frames plus separate attack and release smoothing
//!    shape the mask in time, so onsets open the gate before they arrive.
//! 6. Onsets (spectral flux of the above-threshold bins) hold the gate open with a lowered
//!    threshold and instant attack (`--preserve-transients`).
//! 7. `--artifact-control` also fills spectral troughs up to a steady floor at the
//!    `-reduction` level relative to the minimum-statistics noise estimate, which removes
//!    the random flicker ("musical noise") of gated noise at the cost of a little more
//!    residual noise.
//! 8. Inverse STFT with overlap-add. With `--reduction 0dB` every gain is exactly 1.
//!
//! The engine streams: it holds a constant number of frames regardless of input length.

use crate::profile::NoiseProfile;
use crate::stft::{hop_for, pow_db, Fft, FLOOR_POW};
use ferraus_core::{AudioBuffer, Result};
use realfft::num_complex::Complex32;
use std::collections::VecDeque;

/// Mean of 10·log10 of an exponential variable is 2.507 dB below 10·log10 of its mean.
pub const RAYLEIGH_LOG_BIAS_DB: f32 = 2.507;
/// Standard deviation of 10·log10 of an exponential variable (Rayleigh magnitude in dB).
pub const RAYLEIGH_STD_DB: f32 = 5.570;
/// Width of the soft sigmoid, in dB of excess over threshold.
const SIGMOID_WIDTH_DB: f32 = 1.5;
/// The sigmoid is centred this far above the threshold, so bins at the threshold are
/// already mostly attenuated and bins a few dB above it pass.
const SIGMOID_OFFSET_DB: f32 = 1.5;
/// Lower bound on the noise spread used for the threshold, so near-constant noise such as
/// hum still gets a threshold a few dB above its level.
const SPREAD_FLOOR_DB: f32 = 2.5;
/// Bins this far above the threshold survive the frequency opening even when narrow.
const OPENING_KEEP_DB: f32 = 6.0;
/// Excess values are clamped (keeps the sigmoid in range).
const EXCESS_CLAMP_DB: f32 = 40.0;
/// The artifact-control floor may lift a bin at most this far above `-reduction`.
const FLOOR_MAX_LIFT: f32 = 4.0; // linear (≈ +12 dB)
/// Onset detection.
const ONSET_MIN_FLUX_DB: f32 = 1.0;
const ONSET_RATIO: f32 = 3.0;
const ONSET_HOLD_S: f32 = 0.04;

/// Mean-of-log bias for a noise whose dB spread is `std_db`: 2.507 dB for Gaussian noise
/// (Rayleigh magnitudes, spread 5.57 dB), shrinking towards 0 for steady tones.
pub fn log_bias_db(std_db: f32) -> f32 {
    let r = (std_db / RAYLEIGH_STD_DB).min(1.0);
    RAYLEIGH_LOG_BIAS_DB * r * r
}

/// Parameters of the gate (PRD defaults in `Default`).
#[derive(Clone, Debug, PartialEq)]
pub struct GateConfig {
    pub reduction_db: f32,
    pub sensitivity: f32,
    pub smoothing: usize,
    pub attack_s: f32,
    pub release_s: f32,
    /// 0..1
    pub artifact_control: f32,
    pub link: bool,
    pub preserve_transients: bool,
}

impl Default for GateConfig {
    fn default() -> Self {
        GateConfig {
            reduction_db: 12.0,
            sensitivity: 6.0,
            smoothing: 3,
            attack_s: 0.020,
            release_s: 0.120,
            artifact_control: 0.5,
            link: true,
            preserve_transients: true,
        }
    }
}

/// Where the noise estimate comes from.
#[derive(Clone, Debug)]
pub enum NoiseModel {
    Profile(Box<NoiseProfile>),
    /// Minimum-statistics tracking with no profile. `noisiness` 0..1 raises the threshold
    /// above the tracked floor (20 dB × noisiness); `floor_db` shifts the tracked floor.
    Adaptive {
        noisiness: f32,
        floor_db: f32,
    },
}

/// Minimum-statistics noise power tracker (after Martin 2001): smoothed periodogram,
/// minimum over a ~1.5 s window kept as U sub-window minima, and bias compensation.
#[derive(Clone, Debug)]
pub struct MinStats {
    alpha: f32,
    smoothed: Vec<f32>,
    sub_min: Vec<f32>,
    ring: Vec<Vec<f32>>,
    ring_len: usize,
    ring_pos: usize,
    sub_count: usize,
    sub_len: usize,
    bias: f32,
    pub noise: Vec<f32>,
    init: bool,
}

/// Bias compensation for the minimum of the smoothed periodogram (calibrated on Gaussian
/// noise for the window and smoothing used here).
const MINSTATS_BIAS: f32 = 2.2;
const MINSTATS_SUBWINDOWS: usize = 8;

impl MinStats {
    /// `frame_s` is the hop duration; the window spans about 1.5 s.
    pub fn new(bins: usize, frame_s: f32) -> MinStats {
        let window_frames = (1.5 / frame_s).round().max(8.0) as usize;
        let sub_len = (window_frames / MINSTATS_SUBWINDOWS).max(1);
        // ~ 70 ms smoothing time constant
        let alpha = (-frame_s / 0.07).exp().clamp(0.0, 0.95);
        MinStats {
            alpha,
            smoothed: vec![0.0; bins],
            sub_min: vec![f32::INFINITY; bins],
            ring: vec![vec![f32::INFINITY; bins]; MINSTATS_SUBWINDOWS],
            ring_len: 0,
            ring_pos: 0,
            sub_count: 0,
            sub_len,
            bias: MINSTATS_BIAS,
            noise: vec![FLOOR_POW; bins],
            init: false,
        }
    }

    /// Start from a known noise power (profile mode).
    pub fn seed(&mut self, noise_pow: &[f32]) {
        self.smoothed.copy_from_slice(noise_pow);
        for r in &mut self.ring {
            for (m, n) in r.iter_mut().zip(noise_pow) {
                *m = n / self.bias;
            }
        }
        self.ring_len = self.ring.len();
        self.sub_min.iter_mut().for_each(|m| *m = f32::INFINITY);
        self.noise.copy_from_slice(noise_pow);
        self.init = true;
    }

    pub fn update(&mut self, p: &[f32]) {
        if !self.init {
            self.smoothed.copy_from_slice(p);
            self.init = true;
        } else {
            let a = self.alpha;
            for (s, x) in self.smoothed.iter_mut().zip(p) {
                *s = a * *s + (1.0 - a) * x;
            }
        }
        for (m, s) in self.sub_min.iter_mut().zip(&self.smoothed) {
            if *s < *m {
                *m = *s;
            }
        }
        self.sub_count += 1;
        if self.sub_count >= self.sub_len {
            self.ring[self.ring_pos].copy_from_slice(&self.sub_min);
            self.ring_pos = (self.ring_pos + 1) % self.ring.len();
            self.ring_len = (self.ring_len + 1).min(self.ring.len());
            self.sub_min.iter_mut().for_each(|m| *m = f32::INFINITY);
            self.sub_count = 0;
        }
        for k in 0..self.noise.len() {
            let mut m = self.sub_min[k];
            for r in self.ring.iter().take(self.ring_len) {
                m = m.min(r[k]);
            }
            if !m.is_finite() {
                m = self.smoothed[k];
            }
            self.noise[k] = (m * self.bias).max(FLOOR_POW);
        }
    }
}

struct Frame {
    spec: Vec<Vec<Complex32>>,
    pow: Vec<Vec<f32>>,
    noise: Vec<Vec<f32>>,
    raw: Vec<Vec<f32>>,
    open: Vec<Vec<f32>>,
    blended: Vec<Vec<f32>>,
    flux: f32,
    onset: bool,
}

impl Frame {
    fn new(channels: usize, groups: usize, bins: usize) -> Frame {
        let g = || vec![vec![0f32; bins]; groups];
        Frame {
            spec: vec![vec![Complex32::new(0.0, 0.0); bins]; channels],
            pow: g(),
            noise: g(),
            raw: g(),
            open: g(),
            blended: g(),
            flux: 0.0,
            onset: false,
        }
    }
}

enum Thresholds {
    /// Per group: main threshold, lowered (transient) threshold, in dB.
    Profile {
        main: Vec<Vec<f32>>,
        open: Vec<Vec<f32>>,
    },
    Adaptive {
        offset_db: f32,
        floor_db: f32,
    },
}

/// Statistics gathered while processing (for metrics).
#[derive(Clone, Debug, Default)]
pub struct EngineStats {
    pub frames: usize,
    pub onsets: usize,
    /// Mean applied gain in dB over all bins and frames.
    pub mean_gain_db: f64,
}

/// Streaming spectral gate.
pub struct Engine {
    cfg: GateConfig,
    channels: usize,
    groups: usize,
    n: usize,
    hop: usize,
    bins: usize,
    fft: Fft,
    scale2: f32,
    inbuf: Vec<Vec<f32>>,
    ola: Vec<Vec<f32>>,
    thresholds: Thresholds,
    trackers: Vec<MinStats>,
    queue: VecDeque<Frame>,
    pool: Vec<Frame>,
    blended_count: usize,
    blend_prev: Vec<Vec<f32>>,
    prev_level: Vec<Vec<f32>>,
    prev_flux: f32,
    smooth_db: Vec<Vec<f32>>,
    flux_avg: f32,
    flux_alpha: f32,
    hold: usize,
    hold_frames: usize,
    lookahead: usize,
    a_att: f32,
    a_rel: f32,
    gmin: f32,
    kernel: Vec<f32>,
    frame_index: usize,
    // scratch
    level: Vec<f32>,
    excess: Vec<f32>,
    scratch: Vec<f32>,
    scratch2: Vec<f32>,
    target: Vec<f32>,
    gain: Vec<Vec<f32>>,
    pub stats: EngineStats,
    gain_sum: f64,
}

impl Engine {
    pub fn new(cfg: GateConfig, noise: &NoiseModel, sample_rate: u32, channels: usize) -> Engine {
        let channels = channels.max(1);
        let n = match noise {
            NoiseModel::Profile(p) => p.fft_size,
            NoiseModel::Adaptive { .. } => crate::stft::fft_size_for(sample_rate),
        };
        let hop = hop_for(n);
        let fft = Fft::new(n);
        let bins = fft.bins();
        let groups = if cfg.link { 1 } else { channels };
        let frame_s = hop as f32 / sample_rate.max(1) as f32;
        let mut trackers = vec![MinStats::new(bins, frame_s); groups];
        let thresholds = match noise {
            NoiseModel::Profile(p) => {
                let mut main = Vec::new();
                let mut open = Vec::new();
                for (g, tracker) in trackers.iter_mut().enumerate() {
                    let (mean, std) = p.stats_for(g, cfg.link, channels);
                    let mean_pow_db: Vec<f32> = mean.iter().zip(std).map(|(m, s)| m + log_bias_db(*s)).collect();
                    main.push(
                        mean_pow_db
                            .iter()
                            .zip(std)
                            .map(|(m, s)| m + cfg.sensitivity * s.max(SPREAD_FLOOR_DB) / RAYLEIGH_STD_DB)
                            .collect(),
                    );
                    let seed: Vec<f32> = mean_pow_db.iter().map(|d| 10f32.powf(d / 10.0).max(FLOOR_POW)).collect();
                    tracker.seed(&seed);
                    open.push(mean_pow_db);
                }
                Thresholds::Profile { main, open }
            }
            NoiseModel::Adaptive { noisiness, floor_db } => {
                Thresholds::Adaptive { offset_db: 20.0 * noisiness.clamp(0.0, 1.0), floor_db: *floor_db }
            }
        };
        // attack/release as slew rates: the full reduction range per attack/release time
        let slew = |t: f32| {
            if t <= 0.0 {
                f32::INFINITY
            } else {
                cfg.reduction_db.abs() * frame_s / t
            }
        };
        let lookahead = ((cfg.attack_s / frame_s).ceil() as usize).clamp(1, 8);
        let r = cfg.smoothing.min(64);
        // Hann-shaped kernel over ±r bins (outer taps light, so peaks keep their level)
        let kernel: Vec<f32> = (0..=2 * r)
            .map(|i| {
                let x = std::f32::consts::PI * 2.0 * (i + 1) as f32 / (2 * r + 2) as f32;
                0.5 - 0.5 * x.cos()
            })
            .collect();
        let flux_tau = 0.5f32;
        Engine {
            channels,
            groups,
            n,
            hop,
            bins,
            scale2: fft.mag_scale * fft.mag_scale,
            fft,
            inbuf: vec![vec![0.0; n]; channels],
            ola: vec![vec![0.0; n]; channels],
            thresholds,
            trackers,
            queue: VecDeque::new(),
            pool: Vec::new(),
            blended_count: 0,
            blend_prev: vec![vec![0.0; bins]; groups],
            prev_level: vec![vec![f32::NEG_INFINITY; bins]; groups],
            prev_flux: 0.0,
            smooth_db: vec![vec![-cfg.reduction_db.abs(); bins]; groups],
            flux_avg: 0.0,
            flux_alpha: (-frame_s / flux_tau).exp(),
            hold: 0,
            hold_frames: (ONSET_HOLD_S / frame_s).ceil() as usize,
            lookahead,
            a_att: slew(cfg.attack_s),
            a_rel: slew(cfg.release_s),
            gmin: 10f32.powf(-cfg.reduction_db.abs() / 20.0),
            kernel,
            frame_index: 0,
            level: vec![0.0; bins],
            excess: vec![0.0; bins],
            scratch: vec![0.0; bins],
            scratch2: vec![0.0; bins],
            target: vec![0.0; bins],
            gain: vec![vec![1.0; bins]; groups],
            stats: EngineStats::default(),
            gain_sum: 0.0,
            cfg,
        }
    }

    pub fn fft_size(&self) -> usize {
        self.n
    }

    pub fn hop(&self) -> usize {
        self.hop
    }

    /// Samples of output to discard at the start (the output stream starts `n - hop`
    /// samples before the input).
    pub fn latency(&self) -> usize {
        self.n - self.hop
    }

    /// Push `hop` samples per channel; appends any completed output samples to `out`.
    pub fn push_hop(&mut self, input: &[&[f32]], out: &mut [Vec<f32>]) {
        let (n, hop) = (self.n, self.hop);
        for c in 0..self.channels {
            let buf = &mut self.inbuf[c];
            buf.copy_within(hop.., 0);
            let src = input[c.min(input.len() - 1)];
            buf[n - hop..].copy_from_slice(&src[..hop]);
        }
        let mut frame = self.pool.pop().unwrap_or_else(|| Frame::new(self.channels, self.groups, self.bins));
        self.analyze(&mut frame);
        self.queue.push_back(frame);
        // stage B: blend (median) the frame before the newest
        let len = self.queue.len();
        if len >= 2 && self.blended_count < len - 1 {
            self.blend(len - 2);
            self.blended_count = len - 1;
        }
        // stage C: output when the lookahead window is blended
        if self.blended_count > self.lookahead {
            self.output_front(out);
        }
    }

    fn analyze(&mut self, frame: &mut Frame) {
        let idx = self.frame_index;
        self.frame_index += 1;
        let full = idx + 1 >= self.n / self.hop; // frame contains no initial zero padding
        for c in 0..self.channels {
            self.fft.forward(&self.inbuf[c], &mut frame.spec[c]);
        }
        let scale2 = self.scale2;
        let bins = self.bins;
        let rdb = self.cfg.reduction_db.abs();
        let mut max_flux = 0f32;
        for g in 0..self.groups {
            // group power
            let pow = &mut frame.pow[g];
            if self.cfg.link {
                pow.iter_mut().for_each(|p| *p = 0.0);
                let inv = 1.0 / self.channels as f32;
                for c in 0..self.channels {
                    for (p, z) in pow.iter_mut().zip(&frame.spec[c]) {
                        *p += z.norm_sqr() * scale2 * inv;
                    }
                }
            } else {
                for (p, z) in pow.iter_mut().zip(&frame.spec[g]) {
                    *p = z.norm_sqr() * scale2;
                }
            }
            // detector level: the bin's own power in dB (same statistic as the profile)
            for (l, p) in self.level.iter_mut().zip(pow.iter()) {
                *l = pow_db(*p);
            }
            if full {
                self.trackers[g].update(pow);
            }
            frame.noise[g].copy_from_slice(&self.trackers[g].noise);
            // thresholds → raw gains
            let (main_off, open_off) = match &self.thresholds {
                Thresholds::Profile { .. } => (0.0, 0.0),
                Thresholds::Adaptive { offset_db, floor_db } => (offset_db + floor_db, *floor_db),
            };
            // main gate
            for k in 0..bins {
                let thr = match &self.thresholds {
                    Thresholds::Profile { main, .. } => main[g][k],
                    Thresholds::Adaptive { .. } => pow_db(frame.noise[g][k]) + main_off,
                };
                self.excess[k] = (self.level[k] - thr).clamp(-EXCESS_CLAMP_DB, EXCESS_CLAMP_DB);
            }
            // spectral flux of above-threshold bins (onset detector)
            let mut flux = 0f32;
            if full {
                let prev = &self.prev_level[g];
                for ((e, l), p) in self.excess.iter().zip(&self.level).zip(prev) {
                    if *e > 0.0 && p.is_finite() {
                        flux += (l - p).max(0.0);
                    }
                }
                flux /= bins as f32;
            }
            self.prev_level[g].copy_from_slice(&self.level);
            max_flux = max_flux.max(flux);
            smooth_to_gain(&self.excess, &self.kernel, rdb, &mut frame.raw[g], &mut self.scratch, &mut self.scratch2);
            if self.cfg.preserve_transients {
                for k in 0..bins {
                    let thr = match &self.thresholds {
                        Thresholds::Profile { open, .. } => open[g][k],
                        Thresholds::Adaptive { .. } => pow_db(frame.noise[g][k]) + open_off,
                    };
                    self.excess[k] = (self.level[k] - thr).clamp(-EXCESS_CLAMP_DB, EXCESS_CLAMP_DB);
                }
                smooth_to_gain(
                    &self.excess,
                    &self.kernel,
                    rdb,
                    &mut frame.open[g],
                    &mut self.scratch,
                    &mut self.scratch2,
                );
            }
        }
        frame.flux = max_flux;
        frame.onset = false;
    }

    /// Median-blend frame `i` (needs frames i-1 via `blend_prev` and i+1 in the queue) and
    /// confirm onsets as local maxima of the flux.
    fn blend(&mut self, i: usize) {
        let alpha = self.cfg.artifact_control.clamp(0.0, 1.0);
        let bins = self.bins;
        let (head, tail) = self.queue.make_contiguous().split_at_mut(i + 1);
        let cur = &mut head[i];
        let next = &tail[0];
        for g in 0..self.groups {
            let prev = &self.blend_prev[g];
            let raw = &cur.raw[g];
            let nxt = &next.raw[g];
            let out = &mut cur.blended[g];
            if alpha <= 0.0 {
                out.copy_from_slice(raw);
            } else {
                for k in 0..bins {
                    let lo = k.saturating_sub(1);
                    let hi = (k + 1).min(bins - 1);
                    let mut v = [0f32; 9];
                    let mut m = 0;
                    for j in lo..=hi {
                        v[m] = prev[j];
                        v[m + 1] = raw[j];
                        v[m + 2] = nxt[j];
                        m += 3;
                    }
                    let med = median(&mut v[..m]);
                    out[k] = raw[k] + alpha * (med - raw[k]);
                }
            }
            self.blend_prev[g].copy_from_slice(&cur.raw[g]);
        }
        // onset: flux above an adaptive threshold and a local maximum
        if self.cfg.preserve_transients {
            let f = cur.flux;
            let thr = (ONSET_RATIO * self.flux_avg).max(ONSET_MIN_FLUX_DB);
            cur.onset = f > thr && f >= self.prev_flux && f > next.flux;
            self.prev_flux = f;
            self.flux_avg = self.flux_alpha * self.flux_avg + (1.0 - self.flux_alpha) * f;
        }
    }

    fn output_front(&mut self, out: &mut [Vec<f32>]) {
        let la = self.lookahead;
        let bins = self.bins;
        let transient_ahead = self.queue.iter().take(la + 1).any(|f| f.onset);
        if self.queue[0].onset {
            self.hold = self.hold_frames;
            self.stats.onsets += 1;
        }
        let transient = self.cfg.preserve_transients && (transient_ahead || self.hold > 0);
        self.hold = self.hold.saturating_sub(1);
        let alpha = self.cfg.artifact_control.clamp(0.0, 1.0);
        let gmin = self.gmin;
        let floor_cap = (gmin * FLOOR_MAX_LIFT).min(1.0);
        let mut target = std::mem::take(&mut self.target);
        for g in 0..self.groups {
            // lookahead maximum of the blended mask
            target.copy_from_slice(&self.queue[0].blended[g]);
            for f in self.queue.iter().skip(1).take(la) {
                for (t, b) in target.iter_mut().zip(&f.blended[g]) {
                    if *b > *t {
                        *t = *b;
                    }
                }
            }
            let f0 = &self.queue[0];
            let smooth = &mut self.smooth_db[g];
            let gain = &mut self.gain[g];
            for k in 0..bins {
                let target = target[k];
                let s = smooth[k];
                let ns = if transient {
                    let t = target.max(f0.open[g][k]);
                    if t > s {
                        t
                    } else {
                        t.max(s - self.a_rel)
                    }
                } else if target > s {
                    target.min(s + self.a_att)
                } else {
                    target.max(s - self.a_rel)
                };
                smooth[k] = ns;
                let mut gl = if ns == 0.0 { 1.0 } else { 10f32.powf(ns / 20.0) };
                if alpha > 0.0 {
                    let p = f0.pow[g][k].max(FLOOR_POW);
                    let gf = (gmin * (f0.noise[g][k] / p).sqrt()).min(floor_cap);
                    if gf > gl {
                        gl += alpha * (gf - gl);
                    }
                }
                gain[k] = gl;
                self.gain_sum += (20.0 * gl.max(1e-6).log10()) as f64;
            }
        }
        self.target = target;
        self.stats.frames += 1;
        let mut frame = self.queue.pop_front().expect("queue not empty");
        self.blended_count -= 1;
        let (n, hop) = (self.n, self.hop);
        for (c, (spec, o)) in frame.spec.iter_mut().zip(out.iter_mut()).enumerate() {
            let g = if self.cfg.link { 0 } else { c };
            for (z, gl) in spec.iter_mut().zip(&self.gain[g]) {
                *z *= *gl;
            }
            let t = self.fft.inverse_windowed(spec);
            let acc = &mut self.ola[c];
            for (a, x) in acc.iter_mut().zip(t) {
                *a += x;
            }
            o.extend_from_slice(&acc[..hop]);
            acc.copy_within(hop.., 0);
            acc[n - hop..].iter_mut().for_each(|v| *v = 0.0);
        }
        self.pool.push(frame);
    }

    /// Process a whole buffer in hop-sized chunks (constant engine memory). `progress` is
    /// called periodically with the fraction done and may abort by returning an error.
    pub fn process_buffer(
        &mut self,
        buf: &AudioBuffer,
        progress: &mut dyn FnMut(f64) -> Result<()>,
    ) -> Result<AudioBuffer> {
        let total = buf.frames();
        let ch = self.channels;
        let hop = self.hop;
        let skip = self.latency();
        let mut out: Vec<Vec<f32>> = (0..ch).map(|_| Vec::with_capacity(total + self.n)).collect();
        let mut chunk = vec![vec![0f32; hop]; ch];
        let mut pos = 0usize;
        let mut tick = 0usize;
        while out[0].len() < skip + total {
            for (c, dst) in chunk.iter_mut().enumerate() {
                let src = &buf.data[c.min(buf.channels() - 1)];
                for (i, d) in dst.iter_mut().enumerate() {
                    *d = src.get(pos + i).copied().unwrap_or(0.0);
                }
            }
            pos += hop;
            let refs: Vec<&[f32]> = chunk.iter().map(|c| c.as_slice()).collect();
            self.push_hop(&refs, &mut out);
            tick += 1;
            if tick.is_multiple_of(256) {
                progress((pos as f64 / total.max(1) as f64).min(1.0))?;
            }
        }
        if self.stats.frames > 0 {
            self.stats.mean_gain_db = self.gain_sum / (self.stats.frames * self.bins * self.groups) as f64;
        }
        let data = out
            .into_iter()
            .map(|mut c| {
                c.drain(..skip);
                c.truncate(total);
                c
            })
            .collect();
        Ok(AudioBuffer::from_planar(buf.sample_rate, data))
    }
}

/// Map `excess` through the soft sigmoid to a gain between `-reduction` and 0 dB, then
/// smooth the mask across frequency:
///
/// * a morphological opening over 3 bins removes isolated open bins narrower than a
///   window main lobe (random noise peaks, the seeds of musical noise) while leaving real
///   partials, which are at least a main lobe wide, untouched;
/// * a Hann-weighted average over `±smoothing` bins then lifts the bins next to open
///   regions (soft skirts instead of hard spectral edges); it never pulls an open bin down,
///   so partials keep their level.
fn smooth_to_gain(
    excess: &[f32],
    kernel: &[f32],
    reduction_db: f32,
    out: &mut [f32],
    lin: &mut [f32],
    tmp: &mut [f32],
) {
    let bins = excess.len();
    let r = kernel.len() / 2;
    if reduction_db == 0.0 {
        out.iter_mut().for_each(|g| *g = 0.0);
        return;
    }
    let to_lin = std::f32::consts::LN_10 / 20.0;
    for (l, e) in lin.iter_mut().zip(excess) {
        let sig = 1.0 / (1.0 + (-(e - SIGMOID_OFFSET_DB) / SIGMOID_WIDTH_DB).exp());
        *l = (-reduction_db * (1.0 - sig) * to_lin).exp();
    }
    if r == 0 {
        for (o, l) in out.iter_mut().zip(lin.iter()) {
            *o = 20.0 * l.max(1e-9).log10();
        }
        return;
    }
    // opening = erosion then dilation, 3-bin structuring element; bins well above the
    // threshold are kept as they are (noise peaks never get there, weak narrow partials do)
    for (k, t) in tmp.iter_mut().enumerate() {
        let lo = k.saturating_sub(1);
        let hi = (k + 1).min(bins - 1);
        *t = lin[lo..=hi].iter().copied().fold(f32::INFINITY, f32::min);
    }
    for (k, o) in out.iter_mut().enumerate() {
        let lo = k.saturating_sub(1);
        let hi = (k + 1).min(bins - 1);
        *o = tmp[lo..=hi].iter().copied().fold(0.0, f32::max);
    }
    for ((t, e), (l, o)) in tmp.iter_mut().zip(excess).zip(lin.iter().zip(out.iter())) {
        *t = if *e > OPENING_KEEP_DB { *l } else { *o };
    }
    for k in 0..bins {
        let lo = k.saturating_sub(r);
        let hi = (k + r).min(bins - 1);
        let mut acc = 0f32;
        let mut wsum = 0f32;
        for (j, g) in tmp.iter().enumerate().take(hi + 1).skip(lo) {
            let w = kernel[j + r - k];
            acc += w * g;
            wsum += w;
        }
        out[k] = 20.0 * (acc / wsum).max(tmp[k]).max(1e-9).log10();
    }
}

fn median(v: &mut [f32]) -> f32 {
    v.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let m = v.len();
    if m % 2 == 1 {
        v[m / 2]
    } else {
        0.5 * (v[m / 2 - 1] + v[m / 2])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferraus_core::testing::{noise, sine};

    #[test]
    fn unity_gain_reconstructs() {
        let x = ferraus_core::testing::add(&sine(48_000, 440.0, 0.5, 0.5), &noise(48_000, 0.5, 0.1, 3));
        let cfg = GateConfig { reduction_db: 0.0, ..Default::default() };
        let mut e = Engine::new(cfg, &NoiseModel::Adaptive { noisiness: 0.3, floor_db: 0.0 }, 48_000, 1);
        let y = e.process_buffer(&x, &mut |_| Ok(())).unwrap();
        assert_eq!(y.frames(), x.frames());
        let err = ferraus_core::testing::rms_diff(&x, &y);
        assert!(20.0 * err.log10() < -120.0, "error {} dB", 20.0 * err.log10());
    }

    #[test]
    fn minstats_tracks_white_noise_power() {
        let sr = 48_000;
        let x = noise(sr, 3.0, 0.3, 9);
        let n = 2048;
        let mut fft = Fft::new(n);
        let bins = fft.bins();
        let mut ms = MinStats::new(bins, 512.0 / sr as f32);
        let mut spec = vec![Complex32::new(0.0, 0.0); bins];
        let mut p = vec![0f32; bins];
        let mut true_mean = vec![0f64; bins];
        let mut frames = 0;
        let mut s = 0;
        while s + n <= x.frames() {
            fft.forward(&x.data[0][s..s + n], &mut spec);
            for (pp, z) in p.iter_mut().zip(&spec) {
                *pp = z.norm_sqr() * fft.mag_scale * fft.mag_scale;
            }
            for (t, v) in true_mean.iter_mut().zip(&p) {
                *t += *v as f64;
            }
            frames += 1;
            ms.update(&p);
            s += 512;
        }
        let est: f64 = ms.noise[20..1000].iter().map(|v| *v as f64).sum::<f64>();
        let tru: f64 = true_mean[20..1000].iter().sum::<f64>() / frames as f64;
        let ratio_db = 10.0 * (est / tru).log10();
        assert!(ratio_db.abs() < 1.5, "min-stats bias {ratio_db:.2} dB");
    }
}
