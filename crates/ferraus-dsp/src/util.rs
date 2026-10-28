//! Small shared helpers: decibel conversion, one-pole smoothing coefficients, denormal
//! flushing, a deterministic noise source, and a DC blocker.
//!
//! **Denormals.** Every recursive primitive in this crate passes its state through
//! [`flush`] (or [`flush64`]) once per sample, so a long run of silence after a loud
//! burst never drives the state into subnormal numbers (which are 10–100x slower on most
//! CPUs). The thresholds sit far above the subnormal range and far below audibility.

/// Values whose magnitude is below this are treated as zero in recursive state (f32).
pub const FLUSH_F32: f32 = 1.0e-20;
/// Values whose magnitude is below this are treated as zero in recursive state (f64).
pub const FLUSH_F64: f64 = 1.0e-30;

/// Flush a tiny f32 to exactly zero (denormal protection for filter and loop state).
#[inline(always)]
pub fn flush(x: f32) -> f32 {
    if x.abs() < FLUSH_F32 {
        0.0
    } else {
        x
    }
}

/// Flush a tiny f64 to exactly zero (denormal protection for filter and loop state).
#[inline(always)]
pub fn flush64(x: f64) -> f64 {
    if x.abs() < FLUSH_F64 {
        0.0
    } else {
        x
    }
}

/// dB → linear amplitude.
#[inline]
pub fn db_to_lin(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// Linear amplitude → dB (floored at -200 dB for zero).
#[inline]
pub fn lin_to_db(lin: f32) -> f32 {
    if lin <= 1.0e-10 {
        -200.0
    } else {
        20.0 * lin.log10()
    }
}

/// Power → dB (floored at -200 dB for zero).
#[inline]
pub fn pow_to_db(p: f32) -> f32 {
    if p <= 1.0e-20 {
        -200.0
    } else {
        10.0 * p.log10()
    }
}

/// One-pole smoothing coefficient for a time constant in seconds: `y += (1-c)(x-y)` reaches
/// 1-1/e of a step after `secs`. Zero or negative times give `0.0` (no smoothing).
#[inline]
pub fn time_coeff(secs: f64, sample_rate: f64) -> f32 {
    if secs <= 0.0 || sample_rate <= 0.0 {
        0.0
    } else {
        (-1.0 / (secs * sample_rate)).exp() as f32
    }
}

/// Deterministic xorshift noise source (uniform in [-1, 1)). Cheap enough for per-sample use.
#[derive(Clone, Debug)]
pub struct Noise {
    state: u64,
}

impl Noise {
    pub fn new(seed: u64) -> Noise {
        Noise { state: seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407) | 1 }
    }

    /// Next uniform value in [-1, 1).
    #[inline]
    pub fn tick(&mut self) -> f32 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        ((x >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
    }
}

/// First-order DC blocker `y[n] = x[n] - x[n-1] + r·y[n-1]` with a corner near `freq`.
#[derive(Clone, Debug)]
pub struct DcBlocker {
    r: f32,
    x1: f32,
    y1: f32,
}

impl DcBlocker {
    pub fn new(freq: f64, sample_rate: f64) -> DcBlocker {
        let r = (1.0 - 2.0 * std::f64::consts::PI * freq / sample_rate).clamp(0.0, 0.99999) as f32;
        DcBlocker { r, x1: 0.0, y1: 0.0 }
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let y = x - self.x1 + self.r * self.y1;
        self.x1 = x;
        self.y1 = flush(y);
        self.y1
    }

    pub fn reset(&mut self) {
        self.x1 = 0.0;
        self.y1 = 0.0;
    }
}

/// One-pole low-pass `y += a(x - y)` with a cutoff in Hz (used for damping and smoothing).
#[derive(Clone, Debug)]
pub struct OnePoleLp {
    a: f32,
    y: f32,
}

impl OnePoleLp {
    pub fn new(cutoff: f64, sample_rate: f64) -> OnePoleLp {
        let mut f = OnePoleLp { a: 1.0, y: 0.0 };
        f.set_cutoff(cutoff, sample_rate);
        f
    }

    pub fn set_cutoff(&mut self, cutoff: f64, sample_rate: f64) {
        let c = cutoff.clamp(0.0, sample_rate * 0.5);
        self.a = (1.0 - (-2.0 * std::f64::consts::PI * c / sample_rate).exp()) as f32;
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        self.y = flush(self.y + self.a * (x - self.y));
        self.y
    }

    pub fn reset(&mut self) {
        self.y = 0.0;
    }
}

/// Map a 0..1 knob logarithmically between two positive values.
#[inline]
pub fn log_lerp(lo: f64, hi: f64, t: f64) -> f64 {
    lo * (hi / lo).powf(t.clamp(0.0, 1.0))
}

/// RMS of a slice.
pub fn rms(x: &[f32]) -> f32 {
    if x.is_empty() {
        return 0.0;
    }
    let s: f64 = x.iter().map(|v| (*v as f64) * (*v as f64)).sum();
    (s / x.len() as f64).sqrt() as f32
}

/// Peak absolute value of a slice.
pub fn peak(x: &[f32]) -> f32 {
    x.iter().fold(0.0f32, |m, v| m.max(v.abs()))
}

/// Dot product with eight independent accumulators, so the compiler can vectorize it.
#[inline(always)]
pub(crate) fn dot8(a: &[f32], b: &[f32]) -> f32 {
    let mut acc = [0.0f32; 8];
    let (ca, cb) = (a.chunks_exact(8), b.chunks_exact(8));
    let tail: f32 = ca.remainder().iter().zip(cb.remainder()).map(|(x, y)| x * y).sum();
    for (x, y) in ca.zip(cb) {
        for (s, (p, q)) in acc.iter_mut().zip(x.iter().zip(y)) {
            *s += p * q;
        }
    }
    acc.iter().sum::<f32>() + tail
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flushes_tiny_values() {
        assert_eq!(flush(1e-30), 0.0);
        assert_eq!(flush(0.5), 0.5);
        assert_eq!(flush64(1e-40), 0.0);
    }

    #[test]
    fn noise_is_bounded_and_deterministic() {
        let mut a = Noise::new(3);
        let mut b = Noise::new(3);
        for _ in 0..1000 {
            let x = a.tick();
            assert!((-1.0..1.0).contains(&x));
            assert_eq!(x, b.tick());
        }
    }

    #[test]
    fn dc_blocker_removes_dc() {
        let mut d = DcBlocker::new(5.0, 48000.0);
        let mut y = 0.0;
        for _ in 0..48000 {
            y = d.process(0.5);
        }
        assert!(y.abs() < 1e-3);
    }
}
