//! Polyphase windowed-sinc oversampling (2x, 4x, 8x) for nonlinear stages (saturation,
//! clipping) and for true-peak measurement.
//!
//! An up/down round trip has an integer latency of exactly [`Oversampler::latency`] input
//! samples, so callers can compensate it precisely.

/// Doubled ring buffer giving a contiguous view of the last `len` samples (newest first
/// when read through [`History::slice`]).
#[derive(Clone, Debug)]
struct History {
    buf: Vec<f32>,
    len: usize,
    pos: usize,
}

impl History {
    fn new(len: usize) -> History {
        History { buf: vec![0.0; 2 * len], len, pos: 0 }
    }

    /// Push a sample; after this, `slice()[0]` is the newest.
    #[inline(always)]
    fn push(&mut self, x: f32) {
        self.pos = if self.pos == 0 { self.len - 1 } else { self.pos - 1 };
        self.buf[self.pos] = x;
        self.buf[self.pos + self.len] = x;
    }

    #[inline(always)]
    fn slice(&self) -> &[f32] {
        &self.buf[self.pos..self.pos + self.len]
    }
}

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let y = x * x / 4.0;
    for k in 1..60 {
        term *= y / (k as f64 * k as f64);
        sum += term;
        if term < sum * 1e-16 {
            break;
        }
    }
    sum
}

/// Kaiser-windowed sinc low-pass of odd length `len`, cutoff `fc` as a fraction of the
/// sample rate (0..0.5), unity DC gain.
pub fn kaiser_lowpass(len: usize, fc: f64, beta: f64) -> Vec<f64> {
    let m = (len - 1) as f64 / 2.0;
    let i0b = bessel_i0(beta);
    let mut h: Vec<f64> = (0..len)
        .map(|i| {
            let x = i as f64 - m;
            let sinc = if x.abs() < 1e-12 {
                2.0 * fc
            } else {
                (2.0 * std::f64::consts::PI * fc * x).sin() / (std::f64::consts::PI * x)
            };
            let r = if m > 0.0 { x / m } else { 0.0 };
            let w = bessel_i0(beta * (1.0 - r * r).max(0.0).sqrt()) / i0b;
            sinc * w
        })
        .collect();
    let s: f64 = h.iter().sum();
    for v in &mut h {
        *v /= s;
    }
    h
}

/// Integer-factor up/down sampler for one channel.
#[derive(Clone, Debug)]
pub struct Oversampler {
    factor: usize,
    taps: usize,
    /// Prototype filter, zero-padded to a multiple of 8 taps for [`crate::util::dot8`].
    h: Vec<f32>,
    /// Polyphase rows: `phases[p][k] = factor * h[k*factor + p]`, zero-padded the same way.
    phases: Vec<Vec<f32>>,
    up: History,
    down: History,
}

impl Oversampler {
    /// `factor` 1, 2, 4, or 8; `taps` per phase (12 is a good default; 8 is cheaper).
    pub fn new(factor: usize, taps: usize) -> Oversampler {
        let factor = factor.max(1);
        let taps = taps.max(2) & !1; // even, so the round-trip latency is an integer
        let len = factor * taps + 1;
        // cutoff just below the original Nyquist; longer filters afford more stopband
        let beta = if taps >= 16 { 7.0 } else { 5.5 };
        let proto = kaiser_lowpass(len, 0.5 / factor as f64 * 0.98, beta);
        let mut h = vec![0f32; factor * (taps + 1)];
        for (i, v) in proto.iter().enumerate() {
            h[i] = *v as f32;
        }
        // the padding taps are zero and multiply the oldest history samples
        let (row_len, down_len) = ((taps + 1).next_multiple_of(8), len.next_multiple_of(8));
        let phases = (0..factor)
            .map(|p| {
                let mut row: Vec<f32> = (0..=taps).map(|k| factor as f32 * h[k * factor + p]).collect();
                row.resize(row_len, 0.0);
                row
            })
            .collect();
        h.truncate(len);
        h.resize(down_len, 0.0);
        Oversampler { factor, taps, h, phases, up: History::new(row_len), down: History::new(down_len) }
    }

    pub fn factor(&self) -> usize {
        self.factor
    }

    /// Round-trip (up then down) latency in input samples.
    pub fn latency(&self) -> usize {
        if self.factor == 1 {
            0
        } else {
            self.taps
        }
    }

    /// Latency of the up-sampler alone, in input samples (may be fractional).
    pub fn up_latency(&self) -> f64 {
        if self.factor == 1 {
            0.0
        } else {
            self.taps as f64 / 2.0
        }
    }

    /// Produce `factor` oversampled samples for one input sample.
    #[inline]
    pub fn up(&mut self, x: f32, out: &mut [f32]) {
        if self.factor == 1 {
            out[0] = x;
            return;
        }
        self.up.push(x);
        let hist = self.up.slice();
        for (p, o) in out.iter_mut().enumerate().take(self.factor) {
            *o = crate::util::dot8(&self.phases[p], hist);
        }
    }

    /// Consume `factor` oversampled samples and return one output sample.
    #[inline]
    pub fn down(&mut self, block: &[f32]) -> f32 {
        if self.factor == 1 {
            return block[0];
        }
        self.down.push(block[0]);
        let hist = self.down.slice();
        let acc = crate::util::dot8(&self.h, hist);
        for v in &block[1..self.factor] {
            self.down.push(*v);
        }
        acc
    }

    /// Run `f` on every oversampled sample of `x` (in place, latency compensated, same length).
    pub fn process_with(&mut self, x: &mut [f32], mut f: impl FnMut(f32) -> f32) {
        if self.factor == 1 {
            for v in x.iter_mut() {
                *v = f(*v);
            }
            return;
        }
        let lat = self.latency();
        let n = x.len();
        let mut block = vec![0f32; self.factor];
        let mut out = Vec::with_capacity(n);
        for i in 0..n + lat {
            let s = if i < n { x[i] } else { 0.0 };
            self.up(s, &mut block);
            for b in block.iter_mut() {
                *b = f(*b);
            }
            let y = self.down(&block);
            if i >= lat {
                out.push(y);
            }
        }
        x.copy_from_slice(&out);
    }
}

/// True peak (linear) of `x` measured with `factor`x oversampling (BS.1770-style).
pub fn true_peak(x: &[f32], factor: usize) -> f32 {
    let mut os = Oversampler::new(factor, 12);
    let f = os.factor();
    let mut block = vec![0f32; f];
    let mut peak = 0f32;
    for i in 0..x.len() + 13 {
        let s = if i < x.len() { x[i] } else { 0.0 };
        if i < x.len() {
            peak = peak.max(s.abs());
        }
        os.up(s, &mut block);
        for b in &block {
            peak = peak.max(b.abs());
        }
    }
    peak
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_is_transparent_in_band() {
        let sr = 48000.0;
        let n = 4800;
        let x: Vec<f32> =
            (0..n).map(|i| (0.5 * (2.0 * std::f64::consts::PI * 1000.0 * i as f64 / sr).sin()) as f32).collect();
        for f in [2usize, 4, 8] {
            let mut y = x.clone();
            Oversampler::new(f, 12).process_with(&mut y, |v| v);
            let err: f32 = x[200..n - 200].iter().zip(&y[200..n - 200]).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
            assert!(err < 2e-3, "factor {f}: {err}");
        }
    }

    #[test]
    fn true_peak_finds_intersample_overs() {
        // a sine at fs/4 with 45° phase: samples at ±0.707, true peak 1.0
        let x: Vec<f32> = (0..4800)
            .map(|i| (std::f64::consts::PI / 2.0 * i as f64 + std::f64::consts::PI / 4.0).sin() as f32)
            .collect();
        let sp = crate::util::peak(&x);
        let tp = true_peak(&x, 4);
        assert!(sp < 0.71);
        assert!((tp - 1.0).abs() < 0.02, "{tp}");
    }
}
