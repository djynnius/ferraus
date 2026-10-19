//! Biquad filters from the RBJ Audio EQ Cookbook, Butterworth cascades of any order, and
//! Linkwitz-Riley crossovers / multiband splitters whose bands sum flat in magnitude.
//!
//! Coefficients are designed in `f64`; filters run with `f64` state in transposed direct
//! form II (low noise, good at low frequencies) and flush their state to avoid denormals.
//!
//! ```
//! use ferraus_dsp::biquad::{Biquad, Coeffs, FilterType};
//! let mut hp = Biquad::new(Coeffs::design(FilterType::Highpass, 80.0, 0.707, 0.0, 48000.0));
//! let y = hp.process(0.5);
//! assert!(y.is_finite());
//! ```

use crate::util::flush64;
use std::f64::consts::PI;

/// The filter shapes of the RBJ cookbook (plus first-order sections for odd orders).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilterType {
    Lowpass,
    Highpass,
    /// Constant 0 dB peak gain band-pass.
    Bandpass,
    Notch,
    /// Peaking EQ (bell).
    Peak,
    LowShelf,
    HighShelf,
    Allpass,
    /// First-order (6 dB/oct) low-pass; `q` and `gain` ignored.
    Lowpass1,
    /// First-order (6 dB/oct) high-pass; `q` and `gain` ignored.
    Highpass1,
    /// First-order all-pass (90° at `freq`); `q` and `gain` ignored.
    Allpass1,
}

impl FilterType {
    /// Parse the names used on the command line (`peak`, `lowshelf`, `highpass`, ...).
    pub fn parse(s: &str) -> Option<FilterType> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "lowpass" | "lp" | "lpf" => FilterType::Lowpass,
            "highpass" | "hp" | "hpf" => FilterType::Highpass,
            "bandpass" | "bp" | "bpf" => FilterType::Bandpass,
            "notch" | "bandstop" => FilterType::Notch,
            "peak" | "bell" | "peaking" => FilterType::Peak,
            "lowshelf" | "ls" => FilterType::LowShelf,
            "highshelf" | "hs" => FilterType::HighShelf,
            "allpass" | "ap" => FilterType::Allpass,
            _ => return None,
        })
    }

    /// Whether the gain argument affects this type.
    pub fn uses_gain(self) -> bool {
        matches!(
            self,
            FilterType::Peak | FilterType::LowShelf | FilterType::HighShelf
        )
    }
}

/// Normalized biquad coefficients (`a0 = 1`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Coeffs {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

impl Coeffs {
    /// The identity filter.
    pub const IDENTITY: Coeffs = Coeffs {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };

    /// Design a filter. `q` is the quality factor (for shelves it is the cookbook `Q`, with
    /// 0.707 giving the steepest monotonic shelf), `gain_db` is used by peak and shelves.
    /// Frequencies are clamped to (1 Hz, 0.49·sr).
    pub fn design(ty: FilterType, freq: f64, q: f64, gain_db: f64, sample_rate: f64) -> Coeffs {
        let f = freq.clamp(1.0, sample_rate * 0.49);
        let q = q.max(1e-3);
        let w0 = 2.0 * PI * f / sample_rate;
        let (sn, cs) = w0.sin_cos();
        let alpha = sn / (2.0 * q);
        let a = 10f64.powf(gain_db / 40.0);
        let (b0, b1, b2, a0, a1, a2) = match ty {
            FilterType::Lowpass => (
                (1.0 - cs) / 2.0,
                1.0 - cs,
                (1.0 - cs) / 2.0,
                1.0 + alpha,
                -2.0 * cs,
                1.0 - alpha,
            ),
            FilterType::Highpass => (
                (1.0 + cs) / 2.0,
                -(1.0 + cs),
                (1.0 + cs) / 2.0,
                1.0 + alpha,
                -2.0 * cs,
                1.0 - alpha,
            ),
            FilterType::Bandpass => (alpha, 0.0, -alpha, 1.0 + alpha, -2.0 * cs, 1.0 - alpha),
            FilterType::Notch => (1.0, -2.0 * cs, 1.0, 1.0 + alpha, -2.0 * cs, 1.0 - alpha),
            FilterType::Allpass => (
                1.0 - alpha,
                -2.0 * cs,
                1.0 + alpha,
                1.0 + alpha,
                -2.0 * cs,
                1.0 - alpha,
            ),
            FilterType::Peak => (
                1.0 + alpha * a,
                -2.0 * cs,
                1.0 - alpha * a,
                1.0 + alpha / a,
                -2.0 * cs,
                1.0 - alpha / a,
            ),
            FilterType::LowShelf => {
                let s = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) - (a - 1.0) * cs + s),
                    2.0 * a * ((a - 1.0) - (a + 1.0) * cs),
                    a * ((a + 1.0) - (a - 1.0) * cs - s),
                    (a + 1.0) + (a - 1.0) * cs + s,
                    -2.0 * ((a - 1.0) + (a + 1.0) * cs),
                    (a + 1.0) + (a - 1.0) * cs - s,
                )
            }
            FilterType::HighShelf => {
                let s = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) + (a - 1.0) * cs + s),
                    -2.0 * a * ((a - 1.0) + (a + 1.0) * cs),
                    a * ((a + 1.0) + (a - 1.0) * cs - s),
                    (a + 1.0) - (a - 1.0) * cs + s,
                    2.0 * ((a - 1.0) - (a + 1.0) * cs),
                    (a + 1.0) - (a - 1.0) * cs - s,
                )
            }
            FilterType::Lowpass1 | FilterType::Highpass1 | FilterType::Allpass1 => {
                // bilinear transform with prewarping
                let k = (w0 / 2.0).tan();
                let a1 = (k - 1.0) / (k + 1.0);
                let (b0, b1) = match ty {
                    FilterType::Lowpass1 => (k / (k + 1.0), k / (k + 1.0)),
                    FilterType::Highpass1 => (1.0 / (k + 1.0), -1.0 / (k + 1.0)),
                    _ => (a1, 1.0),
                };
                return Coeffs {
                    b0,
                    b1,
                    b2: 0.0,
                    a1,
                    a2: 0.0,
                };
            }
        };
        Coeffs {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
        }
    }

    /// Complex response at `freq` as (re, im).
    pub fn response(&self, freq: f64, sample_rate: f64) -> (f64, f64) {
        let w = 2.0 * PI * freq / sample_rate;
        let (s1, c1) = w.sin_cos();
        let (s2, c2) = (2.0 * w).sin_cos();
        // numerator and denominator in e^{-jw}
        let nr = self.b0 + self.b1 * c1 + self.b2 * c2;
        let ni = -(self.b1 * s1 + self.b2 * s2);
        let dr = 1.0 + self.a1 * c1 + self.a2 * c2;
        let di = -(self.a1 * s1 + self.a2 * s2);
        let den = dr * dr + di * di;
        ((nr * dr + ni * di) / den, (ni * dr - nr * di) / den)
    }

    /// Magnitude response at `freq` in dB.
    pub fn magnitude_db(&self, freq: f64, sample_rate: f64) -> f64 {
        let (re, im) = self.response(freq, sample_rate);
        10.0 * (re * re + im * im).max(1e-300).log10()
    }
}

/// One biquad section with state (transposed direct form II, f64 state).
#[derive(Clone, Debug)]
pub struct Biquad {
    pub c: Coeffs,
    z1: f64,
    z2: f64,
}

impl Biquad {
    pub fn new(c: Coeffs) -> Biquad {
        Biquad { c, z1: 0.0, z2: 0.0 }
    }

    /// Shorthand for `Biquad::new(Coeffs::design(..))`.
    pub fn design(ty: FilterType, freq: f64, q: f64, gain_db: f64, sample_rate: f64) -> Biquad {
        Biquad::new(Coeffs::design(ty, freq, q, gain_db, sample_rate))
    }

    /// Replace the coefficients, keeping the state (for modulated filters).
    pub fn set(&mut self, c: Coeffs) {
        self.c = c;
    }

    #[inline(always)]
    pub fn process(&mut self, x: f32) -> f32 {
        let x = x as f64;
        let c = &self.c;
        let y = c.b0 * x + self.z1;
        self.z1 = flush64(c.b1 * x - c.a1 * y + self.z2);
        self.z2 = flush64(c.b2 * x - c.a2 * y);
        y as f32
    }

    pub fn process_block(&mut self, buf: &mut [f32]) {
        for s in buf {
            *s = self.process(*s);
        }
    }

    pub fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }
}

/// Q values of the second-order sections of an `order`-pole Butterworth filter (an odd
/// order adds one first-order section, not listed here).
pub fn butterworth_qs(order: usize) -> Vec<f64> {
    let n = order.max(1);
    (0..n / 2)
        .map(|k| {
            let ang = PI * (n as f64 - 2.0 * k as f64 - 1.0) / (2.0 * n as f64);
            1.0 / (2.0 * ang.cos())
        })
        .collect()
}

/// A series cascade of biquads.
#[derive(Clone, Debug, Default)]
pub struct Cascade {
    pub stages: Vec<Biquad>,
}

impl Cascade {
    pub fn new(coeffs: impl IntoIterator<Item = Coeffs>) -> Cascade {
        Cascade {
            stages: coeffs.into_iter().map(Biquad::new).collect(),
        }
    }

    /// Butterworth low-pass of any order (6 dB/oct per order).
    pub fn butterworth_lowpass(order: usize, freq: f64, sample_rate: f64) -> Cascade {
        Self::butterworth(false, order, freq, sample_rate)
    }

    /// Butterworth high-pass of any order (6 dB/oct per order).
    pub fn butterworth_highpass(order: usize, freq: f64, sample_rate: f64) -> Cascade {
        Self::butterworth(true, order, freq, sample_rate)
    }

    fn butterworth(high: bool, order: usize, freq: f64, sr: f64) -> Cascade {
        let ty = if high {
            FilterType::Highpass
        } else {
            FilterType::Lowpass
        };
        let mut cs: Vec<Coeffs> = butterworth_qs(order)
            .into_iter()
            .map(|q| Coeffs::design(ty, freq, q, 0.0, sr))
            .collect();
        if order % 2 == 1 {
            let t1 = if high {
                FilterType::Highpass1
            } else {
                FilterType::Lowpass1
            };
            cs.push(Coeffs::design(t1, freq, 0.707, 0.0, sr));
        }
        Cascade::new(cs)
    }

    /// Linkwitz-Riley low-pass of an even order (2, 4, 8): a squared Butterworth.
    pub fn linkwitz_riley_lowpass(order: usize, freq: f64, sample_rate: f64) -> Cascade {
        let half = Self::butterworth_lowpass((order / 2).max(1), freq, sample_rate);
        let mut stages = half.stages.clone();
        stages.extend(half.stages);
        Cascade { stages }
    }

    /// Linkwitz-Riley high-pass of an even order (2, 4, 8): a squared Butterworth.
    pub fn linkwitz_riley_highpass(order: usize, freq: f64, sample_rate: f64) -> Cascade {
        let half = Self::butterworth_highpass((order / 2).max(1), freq, sample_rate);
        let mut stages = half.stages.clone();
        stages.extend(half.stages);
        Cascade { stages }
    }

    pub fn push(&mut self, c: Coeffs) {
        self.stages.push(Biquad::new(c));
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let mut y = x;
        for s in &mut self.stages {
            y = s.process(y);
        }
        y
    }

    pub fn process_block(&mut self, buf: &mut [f32]) {
        for s in &mut self.stages {
            s.process_block(buf);
        }
    }

    pub fn reset(&mut self) {
        for s in &mut self.stages {
            s.reset();
        }
    }

    /// Combined magnitude at `freq` in dB.
    pub fn magnitude_db(&self, freq: f64, sample_rate: f64) -> f64 {
        self.stages
            .iter()
            .map(|s| s.c.magnitude_db(freq, sample_rate))
            .sum()
    }

    pub fn is_empty(&self) -> bool {
        self.stages.is_empty()
    }
}

/// A 4th-order Linkwitz-Riley two-way crossover. `low + high` is an all-pass (flat magnitude).
#[derive(Clone, Debug)]
pub struct Crossover {
    lp: Cascade,
    hp: Cascade,
}

impl Crossover {
    pub fn new(freq: f64, sample_rate: f64) -> Crossover {
        Crossover {
            lp: Cascade::linkwitz_riley_lowpass(4, freq, sample_rate),
            hp: Cascade::linkwitz_riley_highpass(4, freq, sample_rate),
        }
    }

    /// Split one sample into (low, high).
    #[inline]
    pub fn split(&mut self, x: f32) -> (f32, f32) {
        (self.lp.process(x), self.hp.process(x))
    }
}

/// The all-pass equal to the sum of an LR4 crossover at `freq` (2nd order, Q = 1/√2).
pub fn lr4_allpass(freq: f64, sample_rate: f64) -> Coeffs {
    Coeffs::design(FilterType::Allpass, freq, std::f64::consts::FRAC_1_SQRT_2, 0.0, sample_rate)
}

/// Splits a signal into `crossovers.len() + 1` bands with LR4 crossovers and all-pass phase
/// compensation, so the bands sum to an all-pass version of the input (flat magnitude).
#[derive(Clone, Debug)]
pub struct MultibandSplitter {
    xovers: Vec<Crossover>,
    /// `comp[i]` are the all-passes applied to band `i` (one per later crossover).
    comp: Vec<Cascade>,
}

impl MultibandSplitter {
    /// `crossovers` must be ascending; they are sorted and de-duplicated defensively.
    pub fn new(crossovers: &[f64], sample_rate: f64) -> MultibandSplitter {
        let mut fs: Vec<f64> = crossovers.to_vec();
        fs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        fs.dedup();
        let xovers = fs.iter().map(|f| Crossover::new(*f, sample_rate)).collect();
        let comp = (0..=fs.len())
            .map(|i| {
                Cascade::new(
                    fs.iter()
                        .skip(i + 1)
                        .map(|f| lr4_allpass(*f, sample_rate)),
                )
            })
            .collect();
        MultibandSplitter { xovers, comp }
    }

    pub fn bands(&self) -> usize {
        self.xovers.len() + 1
    }

    /// Split one sample into `out` (length `bands()`).
    #[inline]
    pub fn split(&mut self, x: f32, out: &mut [f32]) {
        let mut rest = x;
        for (i, xo) in self.xovers.iter_mut().enumerate() {
            let (lo, hi) = xo.split(rest);
            out[i] = self.comp[i].process(lo);
            rest = hi;
        }
        let last = self.xovers.len();
        out[last] = rest;
    }

    /// Split a whole channel into band signals.
    pub fn split_buffer(&mut self, x: &[f32]) -> Vec<Vec<f32>> {
        let nb = self.bands();
        let mut bands = vec![vec![0.0f32; x.len()]; nb];
        let mut tmp = vec![0.0f32; nb];
        for (i, s) in x.iter().enumerate() {
            self.split(*s, &mut tmp);
            for b in 0..nb {
                bands[b][i] = tmp[b];
            }
        }
        bands
    }
}

/// Bandwidth in octaves → cookbook Q.
pub fn octaves_to_q(bw: f64) -> f64 {
    let p = 2f64.powf(bw.max(0.01));
    p.sqrt() / (p - 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustfft::{num_complex::Complex, FftPlanner};

    const SR: f64 = 48000.0;

    #[test]
    fn highpass_attenuation_order_2_and_4() {
        // Butterworth: |H(f)| = 1/sqrt(1 + (fc/f)^(2N)) for a high-pass
        for order in [2usize, 4] {
            let c = Cascade::butterworth_highpass(order, 1000.0, SR);
            let got = c.magnitude_db(100.0, SR);
            let want = -10.0 * (1.0 + 10f64.powi(2 * order as i32)).log10();
            assert!((got - want).abs() < 0.2, "order {order}: {got} vs {want}");
            assert!((c.magnitude_db(1000.0, SR) + 3.01).abs() < 0.05);
        }
    }

    #[test]
    fn simulated_highpass_matches_design() {
        // drive a real sine through the filter
        let mut c = Cascade::butterworth_highpass(4, 1000.0, SR);
        let n = 48000;
        let x: Vec<f32> = (0..n)
            .map(|i| (2.0 * PI * 100.0 * i as f64 / SR).sin() as f32)
            .collect();
        let y: Vec<f32> = x.iter().map(|v| c.process(*v)).collect();
        let level = crate::util::rms(&y[n / 2..]) / crate::util::rms(&x[n / 2..]);
        let db = 20.0 * (level as f64).log10();
        assert!((db + 80.0).abs() < 1.0, "{db}");
    }

    #[test]
    fn peak_gain_at_center() {
        for g in [-12.0, -3.0, 3.0, 9.0] {
            let c = Coeffs::design(FilterType::Peak, 1000.0, 1.0, g, SR);
            assert!((c.magnitude_db(1000.0, SR) - g).abs() < 0.1);
        }
        let ls = Coeffs::design(FilterType::LowShelf, 200.0, 0.707, 6.0, SR);
        assert!((ls.magnitude_db(10.0, SR) - 6.0).abs() < 0.1);
        let hs = Coeffs::design(FilterType::HighShelf, 5000.0, 0.707, -6.0, SR);
        assert!((hs.magnitude_db(20000.0, SR) + 6.0).abs() < 0.2);
    }

    #[test]
    fn odd_order_butterworth() {
        let c = Cascade::butterworth_lowpass(3, 1000.0, SR);
        assert!((c.magnitude_db(1000.0, SR) + 3.01).abs() < 0.05);
        // bilinear transform: compare against the analog prototype at the warped frequency
        let warp = (PI * 10000.0 / SR).tan() / (PI * 1000.0 / SR).tan();
        let want = -10.0 * (1.0 + warp.powi(6)).log10();
        assert!((c.magnitude_db(10000.0, SR) - want).abs() < 0.1);
    }

    #[test]
    fn linkwitz_riley_bands_sum_flat() {
        let mut sp = MultibandSplitter::new(&[150.0, 1000.0, 3000.0, 9000.0], SR);
        let n = 1 << 16;
        let mut sum = vec![0.0f32; n];
        let mut tmp = vec![0.0; sp.bands()];
        for (i, s) in sum.iter_mut().enumerate() {
            sp.split(if i == 0 { 1.0 } else { 0.0 }, &mut tmp);
            *s = tmp.iter().sum();
        }
        let mut buf: Vec<Complex<f64>> = sum.iter().map(|v| Complex::new(*v as f64, 0.0)).collect();
        FftPlanner::new().plan_fft_forward(n).process(&mut buf);
        for (k, c) in buf.iter().enumerate().take(n / 2) {
            let f = k as f64 * SR / n as f64;
            if !(20.0..=20000.0).contains(&f) {
                continue;
            }
            let db = 20.0 * c.norm().log10();
            assert!(db.abs() < 0.1, "{f} Hz: {db} dB");
        }
    }

    #[test]
    fn silence_after_burst_flushes_state() {
        let mut c = Cascade::butterworth_lowpass(8, 30.0, SR);
        for i in 0..1000 {
            c.process(if i % 2 == 0 { 1.0 } else { -1.0 });
        }
        for _ in 0..2_000_000 {
            c.process(0.0);
        }
        for s in &c.stages {
            assert!(s.z1 == 0.0 || s.z1.abs() > 1e-30);
        }
    }
}
