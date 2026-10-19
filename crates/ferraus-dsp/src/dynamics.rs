//! Dynamics processors: envelope followers, a log-domain feed-forward compressor with soft
//! knee and lookahead, gate (3 dB hysteresis + hold), downward expander, a true-peak
//! lookahead brickwall limiter, and soft/hard clippers.
//!
//! Every processor here works on planar channels (`&mut [Vec<f32>]`) with linked detection
//! (one gain for all channels, so the stereo image never shifts), and every one compensates
//! its own lookahead latency so the output lines up with the input sample for sample.

use crate::biquad::Cascade;
use crate::oversample::{true_peak, Oversampler};
use crate::util::{db_to_lin, flush, lin_to_db, pow_to_db, time_coeff};
use std::collections::VecDeque;

/// Level detector type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Detector {
    /// Instantaneous absolute value (max over channels).
    Peak,
    /// Mean square over a 10 ms window (mean over channels).
    Rms,
}

impl Detector {
    pub fn parse(s: &str) -> Detector {
        if s.eq_ignore_ascii_case("peak") {
            Detector::Peak
        } else {
            Detector::Rms
        }
    }
}

/// Classic peak envelope follower with separate attack and release (linear domain).
#[derive(Clone, Debug)]
pub struct EnvelopeFollower {
    att: f32,
    rel: f32,
    env: f32,
}

impl EnvelopeFollower {
    pub fn new(attack_s: f64, release_s: f64, sample_rate: f64) -> EnvelopeFollower {
        EnvelopeFollower {
            att: time_coeff(attack_s, sample_rate),
            rel: time_coeff(release_s, sample_rate),
            env: 0.0,
        }
    }

    /// Feed a non-negative level (e.g. `|x|`), get the envelope.
    #[inline(always)]
    pub fn process(&mut self, level: f32) -> f32 {
        let c = if level > self.env { self.att } else { self.rel };
        self.env = flush(level + c * (self.env - level));
        self.env
    }

    pub fn value(&self) -> f32 {
        self.env
    }

    pub fn reset(&mut self) {
        self.env = 0.0;
    }
}

/// Branching one-pole smoother in the dB domain. `rising` (towards 0 dB / less reduction)
/// uses one time constant, `falling` the other.
#[derive(Clone, Debug)]
pub struct DbSmoother {
    up: f32,
    down: f32,
    pub state: f32,
}

impl DbSmoother {
    pub fn new(up_s: f64, down_s: f64, sample_rate: f64, initial: f32) -> DbSmoother {
        DbSmoother {
            up: time_coeff(up_s, sample_rate),
            down: time_coeff(down_s, sample_rate),
            state: initial,
        }
    }

    #[inline(always)]
    pub fn process(&mut self, target: f32) -> f32 {
        let c = if target > self.state { self.up } else { self.down };
        self.state = target + c * (self.state - target);
        if (self.state - target).abs() < 1e-6 {
            self.state = target;
        }
        self.state
    }
}

/// Static compressor curve: gain change in dB (≤ 0) for an input level, with a soft knee of
/// width `knee_db` centred on the threshold (Giannoulis, Massberg & Reiss 2012).
#[inline]
pub fn compressor_curve(level_db: f32, threshold_db: f32, ratio: f32, knee_db: f32) -> f32 {
    let over = level_db - threshold_db;
    let slope = 1.0 / ratio.max(1.0) - 1.0;
    if knee_db > 0.0 && 2.0 * over.abs() <= knee_db {
        let t = over + knee_db / 2.0;
        slope * t * t / (2.0 * knee_db)
    } else if over > 0.0 {
        slope * over
    } else {
        0.0
    }
}

/// Static downward-expander curve: gain change in dB (≤ 0) below the threshold.
#[inline]
pub fn expander_curve(level_db: f32, threshold_db: f32, ratio: f32, floor_db: f32) -> f32 {
    if level_db >= threshold_db {
        0.0
    } else {
        ((level_db - threshold_db) * (ratio.max(1.0) - 1.0)).max(floor_db)
    }
}

/// Sidechain level detection shared by the compressor and expander: per-channel optional
/// high-pass, stereo linking, peak or 10 ms RMS. Produces a level in dB per frame.
#[derive(Clone, Debug)]
pub struct LevelDetector {
    detector: Detector,
    hpf: Vec<Cascade>,
    ms: f32,
    ms_coeff: f32,
}

impl LevelDetector {
    /// `hpf_hz <= 0` disables the sidechain high-pass.
    pub fn new(detector: Detector, hpf_hz: f64, channels: usize, sample_rate: f64) -> Self {
        let hpf = if hpf_hz > 0.0 {
            (0..channels)
                .map(|_| Cascade::butterworth_highpass(2, hpf_hz, sample_rate))
                .collect()
        } else {
            Vec::new()
        };
        LevelDetector {
            detector,
            hpf,
            ms: 0.0,
            ms_coeff: time_coeff(0.010, sample_rate),
        }
    }

    /// Level in dB of one frame (one sample per channel).
    #[inline]
    pub fn level_db(&mut self, frame: &[f32]) -> f32 {
        let mut pk = 0.0f32;
        let mut sq = 0.0f32;
        for (c, x) in frame.iter().enumerate() {
            let v = if let Some(f) = self.hpf.get_mut(c) {
                f.process(*x)
            } else {
                *x
            };
            pk = pk.max(v.abs());
            sq += v * v;
        }
        match self.detector {
            Detector::Peak => lin_to_db(pk),
            Detector::Rms => {
                let m = sq / frame.len().max(1) as f32;
                self.ms = flush(m + self.ms_coeff * (self.ms - m));
                pow_to_db(self.ms)
            }
        }
    }
}

/// Compressor settings.
#[derive(Clone, Debug)]
pub struct CompressorParams {
    pub threshold_db: f32,
    pub ratio: f32,
    pub knee_db: f32,
    pub attack_s: f64,
    pub release_s: f64,
    pub makeup_db: f32,
    pub lookahead_s: f64,
    pub detector: Detector,
    /// Sidechain high-pass corner in Hz (0 = off).
    pub sidechain_hpf: f64,
}

impl Default for CompressorParams {
    fn default() -> Self {
        CompressorParams {
            threshold_db: -18.0,
            ratio: 3.0,
            knee_db: 6.0,
            attack_s: 0.010,
            release_s: 0.100,
            makeup_db: 0.0,
            lookahead_s: 0.005,
            detector: Detector::Rms,
            sidechain_hpf: 80.0,
        }
    }
}

impl CompressorParams {
    /// The PRD's `auto` makeup: half the static gain reduction a 0 dBFS signal would get,
    /// which restores the level lost above the threshold for typical program material.
    pub fn auto_makeup_db(&self) -> f32 {
        -0.5 * compressor_curve(0.0, self.threshold_db, self.ratio, self.knee_db)
    }
}

/// Statistics returned by the dynamics processors.
#[derive(Clone, Copy, Debug, Default)]
pub struct DynamicsStats {
    /// Largest gain reduction in dB (positive number).
    pub max_reduction_db: f32,
    /// Mean gain reduction in dB (positive number).
    pub mean_reduction_db: f32,
}

fn stats_from(gains_db: &[f32]) -> DynamicsStats {
    if gains_db.is_empty() {
        return DynamicsStats::default();
    }
    let max = gains_db.iter().fold(0.0f32, |m, g| m.max(-g));
    let mean = gains_db.iter().map(|g| -g as f64).sum::<f64>() / gains_db.len() as f64;
    DynamicsStats {
        max_reduction_db: max,
        mean_reduction_db: mean as f32,
    }
}

/// Feed-forward log-domain compressor.
#[derive(Clone, Debug)]
pub struct Compressor {
    pub params: CompressorParams,
    sample_rate: f64,
}

impl Compressor {
    pub fn new(params: CompressorParams, sample_rate: f64) -> Compressor {
        Compressor {
            params,
            sample_rate,
        }
    }

    /// Lookahead in samples.
    pub fn lookahead(&self) -> usize {
        (self.params.lookahead_s * self.sample_rate).round().max(0.0) as usize
    }

    /// Gain change in dB per frame computed from `sidechain` channels, already shifted so
    /// that `gain[n]` applies to frame `n` (lookahead compensated). Excludes makeup.
    pub fn gain_curve(&self, sidechain: &[Vec<f32>]) -> Vec<f32> {
        let p = &self.params;
        let n = sidechain.first().map(|c| c.len()).unwrap_or(0);
        let la = self.lookahead();
        let mut det = LevelDetector::new(
            p.detector,
            p.sidechain_hpf,
            sidechain.len(),
            self.sample_rate,
        );
        // attack = moving towards more reduction (down), release = back up
        let mut sm = DbSmoother::new(p.release_s, p.attack_s, self.sample_rate, 0.0);
        let mut frame = vec![0.0f32; sidechain.len()];
        let mut out = Vec::with_capacity(n);
        for i in 0..n + la {
            for (c, ch) in sidechain.iter().enumerate() {
                frame[c] = if i < n { ch[i] } else { 0.0 };
            }
            let lvl = det.level_db(&frame);
            let target = compressor_curve(lvl, p.threshold_db, p.ratio, p.knee_db);
            let g = sm.process(target);
            if i >= la {
                out.push(g);
            }
        }
        out
    }

    /// Compress `channels` in place (self-sidechained, linked).
    pub fn process(&self, channels: &mut [Vec<f32>]) -> DynamicsStats {
        let gains = self.gain_curve(channels);
        apply_gain_db(channels, &gains, self.params.makeup_db);
        stats_from(&gains)
    }

    /// Compress `channels` with an external sidechain (same length).
    pub fn process_sidechain(&self, channels: &mut [Vec<f32>], sidechain: &[Vec<f32>]) -> DynamicsStats {
        let gains = self.gain_curve(sidechain);
        apply_gain_db(channels, &gains, self.params.makeup_db);
        stats_from(&gains)
    }
}

/// Multiply every channel by `10^((gain_db[n] + offset_db)/20)`.
pub fn apply_gain_db(channels: &mut [Vec<f32>], gains_db: &[f32], offset_db: f32) {
    for ch in channels.iter_mut() {
        for (x, g) in ch.iter_mut().zip(gains_db) {
            *x *= db_to_lin(*g + offset_db);
        }
    }
}

/// Gate settings.
#[derive(Clone, Debug)]
pub struct GateParams {
    /// Opening threshold; the gate closes 3 dB lower (hysteresis).
    pub threshold_db: f32,
    pub attack_s: f64,
    pub hold_s: f64,
    pub release_s: f64,
    /// Attenuation when closed (e.g. -80 dB).
    pub range_db: f32,
    pub sidechain_hpf: f64,
}

/// Hysteresis between the open and close thresholds of [`gate`].
pub const GATE_HYSTERESIS_DB: f32 = 3.0;

/// Noise gate with 3 dB hysteresis and hold. Returns stats.
pub fn gate(channels: &mut [Vec<f32>], sample_rate: f64, p: &GateParams) -> DynamicsStats {
    let n = channels.first().map(|c| c.len()).unwrap_or(0);
    let mut det = LevelDetector::new(Detector::Peak, p.sidechain_hpf, channels.len(), sample_rate);
    // peak hold envelope so the detector does not chatter inside one waveform cycle
    let mut env = EnvelopeFollower::new(0.0, 0.010, sample_rate);
    let open_db = p.threshold_db;
    let close_db = p.threshold_db - GATE_HYSTERESIS_DB;
    let hold = (p.hold_s * sample_rate) as usize;
    let mut is_open = false;
    let mut hold_left = 0usize;
    let floor = p.range_db.min(0.0);
    let mut sm = DbSmoother::new(p.attack_s, p.release_s, sample_rate, floor);
    let mut frame = vec![0.0f32; channels.len()];
    let mut gains = Vec::with_capacity(n);
    for i in 0..n {
        for (c, ch) in channels.iter().enumerate() {
            frame[c] = ch[i];
        }
        let lvl_lin = db_to_lin(det.level_db(&frame));
        let lvl = lin_to_db(env.process(lvl_lin));
        if lvl >= open_db {
            is_open = true;
            hold_left = hold;
        } else if lvl < close_db && is_open {
            if hold_left > 0 {
                hold_left -= 1;
            } else {
                is_open = false;
            }
        } else if is_open {
            hold_left = hold;
        }
        let target = if is_open { 0.0 } else { floor };
        gains.push(sm.process(target));
    }
    apply_gain_db(channels, &gains, 0.0);
    stats_from(&gains)
}

/// Downward expander settings.
#[derive(Clone, Debug)]
pub struct ExpanderParams {
    pub threshold_db: f32,
    pub ratio: f32,
    pub attack_s: f64,
    pub release_s: f64,
    /// Most attenuation the expander applies (e.g. -60 dB).
    pub floor_db: f32,
    pub detector: Detector,
}

/// Downward expander (the gentle alternative to a gate).
pub fn expand(channels: &mut [Vec<f32>], sample_rate: f64, p: &ExpanderParams) -> DynamicsStats {
    let n = channels.first().map(|c| c.len()).unwrap_or(0);
    let mut det = LevelDetector::new(p.detector, 0.0, channels.len(), sample_rate);
    // opening (gain rising) uses attack, closing uses release
    let mut sm = DbSmoother::new(p.attack_s, p.release_s, sample_rate, 0.0);
    let mut frame = vec![0.0f32; channels.len()];
    let mut gains = Vec::with_capacity(n);
    for i in 0..n {
        for (c, ch) in channels.iter().enumerate() {
            frame[c] = ch[i];
        }
        let lvl = det.level_db(&frame);
        gains.push(sm.process(expander_curve(lvl, p.threshold_db, p.ratio, p.floor_db)));
    }
    apply_gain_db(channels, &gains, 0.0);
    stats_from(&gains)
}

/// Limiter settings.
#[derive(Clone, Debug)]
pub struct LimiterParams {
    /// Ceiling in dB (true peak when `oversample > 1`).
    pub ceiling_db: f32,
    pub release_s: f64,
    pub lookahead_s: f64,
    /// True-peak detection oversampling factor (1, 2, 4, 8).
    pub oversample: usize,
}

impl Default for LimiterParams {
    fn default() -> Self {
        LimiterParams {
            ceiling_db: -1.0,
            release_s: 0.050,
            lookahead_s: 0.005,
            oversample: 4,
        }
    }
}

/// Per-frame peak (max over channels) including inter-sample peaks at `factor`x.
fn frame_true_peaks(channels: &[Vec<f32>], factor: usize) -> Vec<f32> {
    let n = channels.first().map(|c| c.len()).unwrap_or(0);
    let mut tp = vec![0.0f32; n];
    for ch in channels {
        for (t, x) in tp.iter_mut().zip(ch) {
            *t = t.max(x.abs());
        }
        if factor > 1 {
            let mut os = Oversampler::new(factor, 12);
            let lat = os.up_latency().ceil() as usize;
            let mut block = vec![0f32; factor];
            for i in 0..n + lat + 1 {
                let s = if i < n { ch[i] } else { 0.0 };
                os.up(s, &mut block);
                let m = block.iter().fold(0.0f32, |m, v| m.max(v.abs()));
                // the block lies between input samples i-lat-1 .. i-lat+1; mark both neighbours
                for j in [i as isize - lat as isize - 1, i as isize - lat as isize, i as isize - lat as isize + 1] {
                    if j >= 0 && (j as usize) < n {
                        tp[j as usize] = tp[j as usize].max(m);
                    }
                }
            }
        }
    }
    tp
}

/// Compute the limiter gain envelope (linear) for target ceiling `c`.
fn limiter_gains(tp: &[f32], c: f32, la: usize, release_coeff: f32) -> Vec<f32> {
    let n = tp.len();
    let req: Vec<f32> = tp
        .iter()
        .map(|p| if *p > c { c / *p } else { 1.0 })
        .collect();
    // future minimum over [n, n+la] with a monotonic deque
    let mut fmin = vec![1.0f32; n];
    let mut dq: VecDeque<usize> = VecDeque::new();
    let mut next = 0usize;
    for (i, f) in fmin.iter_mut().enumerate() {
        while next < n && next <= i + la {
            while let Some(&b) = dq.back() {
                if req[b] >= req[next] {
                    dq.pop_back();
                } else {
                    break;
                }
            }
            dq.push_back(next);
            next += 1;
        }
        while let Some(&f0) = dq.front() {
            if f0 < i {
                dq.pop_front();
            } else {
                break;
            }
        }
        *f = dq.front().map(|&k| req[k]).unwrap_or(1.0);
    }
    // instant attack / smooth release on the held minimum
    let mut rel = vec![1.0f32; n];
    let mut st = 1.0f32;
    for (r, h) in rel.iter_mut().zip(&fmin) {
        st = if *h < st { *h } else { *h + release_coeff * (st - *h) };
        *r = st;
    }
    // box filter over the past la+1 samples: smooth attack that is never above `req`
    let w = la + 1;
    let mut out = vec![1.0f32; n];
    // samples before the start take the first gain, so a peak at the very start is covered
    let first = rel.first().copied().unwrap_or(1.0) as f64;
    let mut sum = w as f64 * first;
    for i in 0..n {
        sum += rel[i] as f64;
        let old = if i >= w { rel[i - w] as f64 } else { first };
        sum -= old;
        out[i] = (sum / w as f64) as f32;
    }
    out
}

/// True-peak lookahead brickwall limiter. The output never exceeds the ceiling when measured
/// with [`true_peak`] at the same oversampling factor (verified, with up to six refinement
/// passes when gain modulation creates new inter-sample peaks).
pub fn limit(channels: &mut [Vec<f32>], sample_rate: f64, p: &LimiterParams) -> DynamicsStats {
    let n = channels.first().map(|c| c.len()).unwrap_or(0);
    if n == 0 {
        return DynamicsStats::default();
    }
    let factor = p.oversample.max(1);
    let ceiling = db_to_lin(p.ceiling_db);
    let la = ((p.lookahead_s * sample_rate).round() as usize).max(1);
    let rc = time_coeff(p.release_s, sample_rate);
    let tp = frame_true_peaks(channels, factor);
    let orig: Vec<Vec<f32>> = channels.to_vec();
    let mut target = ceiling * db_to_lin(-0.05);
    let mut gains = vec![1.0f32; n];
    for _pass in 0..6 {
        gains = limiter_gains(&tp, target, la, rc);
        for (ch, o) in channels.iter_mut().zip(&orig) {
            for ((y, x), g) in ch.iter_mut().zip(o).zip(&gains) {
                *y = x * g;
            }
        }
        let measured = channels
            .iter()
            .map(|c| if factor > 1 { true_peak(c, factor) } else { crate::util::peak(c) })
            .fold(0.0f32, f32::max);
        if measured <= ceiling {
            break;
        }
        target *= ceiling / measured * db_to_lin(-0.02);
    }
    // absolute guarantee on the sample grid
    for ch in channels.iter_mut() {
        for y in ch.iter_mut() {
            *y = y.clamp(-ceiling, ceiling);
        }
    }
    let gdb: Vec<f32> = gains.iter().map(|g| lin_to_db(*g)).collect();
    stats_from(&gdb)
}

/// Clipper shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipMode {
    Soft,
    Hard,
}

/// Soft clip: linear below `ceiling·10^(-knee/20)`, then a tanh curve that approaches the
/// ceiling asymptotically.
#[inline]
pub fn soft_clip(x: f32, ceiling: f32, knee_start: f32) -> f32 {
    let a = x.abs();
    if a <= knee_start {
        x
    } else {
        let room = (ceiling - knee_start).max(1e-6);
        let y = knee_start + room * ((a - knee_start) / room).tanh();
        y.copysign(x)
    }
}

/// Clip `x` in place with 4x oversampling (less aliasing) and a final sample-peak guard.
pub fn clip(x: &mut [f32], ceiling_db: f32, mode: ClipMode, knee_db: f32) {
    let c = db_to_lin(ceiling_db);
    let ks = c * db_to_lin(-knee_db.max(0.0));
    let mut os = Oversampler::new(4, 8);
    match mode {
        ClipMode::Hard => os.process_with(x, |v| v.clamp(-c, c)),
        ClipMode::Soft => os.process_with(x, |v| soft_clip(v, c, ks)),
    }
    for v in x.iter_mut() {
        *v = v.clamp(-c, c);
    }
}

/// De-esser settings.
#[derive(Clone, Debug)]
pub struct DeessParams {
    /// Centre of the sibilance detector band.
    pub center: f64,
    /// Detector bandwidth in octaves.
    pub bandwidth_oct: f64,
    pub threshold_db: f32,
    /// Most reduction applied, dB (positive).
    pub range_db: f32,
    /// `true`: only the band above the detector's lower edge is turned down (dynamic shelf);
    /// `false`: the whole signal is turned down.
    pub split: bool,
}

impl Default for DeessParams {
    fn default() -> Self {
        DeessParams {
            center: 6500.0,
            bandwidth_oct: 1.0,
            threshold_db: -20.0,
            range_db: 6.0,
            split: true,
        }
    }
}

/// De-esser: a band-limited detector drives a dynamic high shelf (split, via an LR4
/// crossover at the band's lower edge, so the untouched path stays flat) or the whole
/// signal (wideband).
pub fn deess(channels: &mut [Vec<f32>], sample_rate: f64, p: &DeessParams) -> DynamicsStats {
    use crate::biquad::{octaves_to_q, Coeffs, Crossover, FilterType};
    let n = channels.first().map(|c| c.len()).unwrap_or(0);
    let nch = channels.len().max(1);
    let center = p.center.clamp(1000.0, sample_rate * 0.45);
    let q = octaves_to_q(p.bandwidth_oct);
    let bp = Coeffs::design(FilterType::Bandpass, center, q, 0.0, sample_rate);
    let mut det = Cascade::new([bp, bp]);
    let mut env = EnvelopeFollower::new(0.0005, 0.040, sample_rate);
    let mut sm = DbSmoother::new(0.040, 0.0005, sample_rate, 0.0);
    let mut gains = Vec::with_capacity(n);
    for i in 0..n {
        let m: f32 = channels.iter().map(|c| c[i]).sum::<f32>() / nch as f32;
        let lvl = lin_to_db(env.process(det.process(m).abs()));
        let target = compressor_curve(lvl, p.threshold_db, 4.0, 4.0).max(-p.range_db.abs());
        gains.push(sm.process(target));
    }
    if p.split {
        let edge = (center / 2f64.powf(p.bandwidth_oct / 2.0)).max(500.0);
        for ch in channels.iter_mut() {
            let mut xo = Crossover::new(edge, sample_rate);
            for (x, g) in ch.iter_mut().zip(&gains) {
                let (lo, hi) = xo.split(*x);
                *x = lo + hi * db_to_lin(*g);
            }
        }
    } else {
        apply_gain_db(channels, &gains, 0.0);
    }
    stats_from(&gains)
}

/// Transient shaper: boosts or cuts the attack and the sustain of each note, detected by
/// comparing fast and slow envelopes (linked across channels). Gains in dB.
pub fn transient_shape(channels: &mut [Vec<f32>], sample_rate: f64, attack_db: f32, sustain_db: f32) {
    let n = channels.first().map(|c| c.len()).unwrap_or(0);
    let mut fast_a = EnvelopeFollower::new(0.0005, 0.050, sample_rate);
    let mut slow_a = EnvelopeFollower::new(0.020, 0.050, sample_rate);
    let mut fast_r = EnvelopeFollower::new(0.0005, 0.030, sample_rate);
    let mut slow_r = EnvelopeFollower::new(0.0005, 0.300, sample_rate);
    let mut sm = DbSmoother::new(0.002, 0.002, sample_rate, 0.0);
    let mut gains = Vec::with_capacity(n);
    for i in 0..n {
        let pk = channels.iter().fold(0.0f32, |m, c| m.max(c[i].abs()));
        let da = lin_to_db(fast_a.process(pk)) - lin_to_db(slow_a.process(pk));
        let ds = lin_to_db(slow_r.process(pk)) - lin_to_db(fast_r.process(pk));
        let gate = if pk > 1e-5 || slow_r.value() > 1e-5 { 1.0 } else { 0.0 };
        let g = gate * (attack_db * (da / 6.0).clamp(0.0, 1.0) + sustain_db * (ds / 6.0).clamp(0.0, 1.0));
        gains.push(sm.process(g));
    }
    apply_gain_db(channels, &gains, 0.0);
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn soft_knee_curve_is_continuous() {
        let f = |l| compressor_curve(l, -18.0, 3.0, 6.0);
        assert_eq!(f(-30.0), 0.0);
        assert!((f(-21.0)).abs() < 1e-6);
        assert!((f(-15.0) - (-2.0)).abs() < 1e-5);
        assert!((f(0.0) - (-12.0)).abs() < 1e-5);
        // hard knee
        assert!((compressor_curve(-8.0, -18.0, 4.0, 0.0) + 7.5).abs() < 1e-6);
    }

    #[test]
    fn limiter_respects_true_peak_ceiling() {
        let sr = 48000.0;
        // loud near-Nyquist content with intersample peaks
        let n = 24000;
        let mut ch = vec![(0..n)
            .map(|i| {
                let t = i as f64 / sr;
                (1.6 * (2.0 * std::f64::consts::PI * 11025.0 * t + 0.7).sin()
                    + 0.8 * (2.0 * std::f64::consts::PI * 97.0 * t).sin()) as f32
            })
            .collect::<Vec<f32>>()];
        limit(&mut ch, sr, &LimiterParams::default());
        let tp = true_peak(&ch[0], 4);
        assert!(tp <= db_to_lin(-1.0) + 1e-6, "{}", lin_to_db(tp));
    }

    #[test]
    fn gate_closes_on_silence_and_opens_on_signal() {
        let sr = 48000.0;
        let mut ch = vec![vec![0.001f32; 24000]];
        for (i, v) in ch[0].iter_mut().enumerate().skip(12000) {
            *v = 0.5 * ((i as f32) * 0.1).sin();
        }
        gate(
            &mut ch,
            sr,
            &GateParams {
                threshold_db: -40.0,
                attack_s: 0.001,
                hold_s: 0.05,
                release_s: 0.1,
                range_db: -80.0,
                sidechain_hpf: 0.0,
            },
        );
        assert!(ch[0][6000].abs() < 1e-6);
        assert!(crate::util::rms(&ch[0][18000..]) > 0.3);
    }
}
