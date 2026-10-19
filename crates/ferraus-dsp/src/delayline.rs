//! Delay lines: a power-of-two ring buffer with integer, linear, and cubic (Hermite)
//! fractional reads, plus Schroeder all-pass diffusers built on it.

use crate::util::flush;

/// A ring-buffer delay line. Capacity is rounded up to a power of two.
#[derive(Clone, Debug)]
pub struct DelayLine {
    buf: Vec<f32>,
    mask: usize,
    write: usize,
}

impl DelayLine {
    /// A delay line able to delay by at least `max_delay` samples.
    pub fn new(max_delay: usize) -> DelayLine {
        let cap = (max_delay + 4).next_power_of_two();
        DelayLine { buf: vec![0.0; cap], mask: cap - 1, write: 0 }
    }

    /// Longest supported delay in samples.
    pub fn max_delay(&self) -> usize {
        self.buf.len() - 4
    }

    /// Write one sample (advances the write head).
    #[inline(always)]
    pub fn push(&mut self, x: f32) {
        self.buf[self.write] = flush(x);
        self.write = (self.write + 1) & self.mask;
    }

    /// Sample written `d` pushes ago (`d = 1` is the most recent sample).
    #[inline(always)]
    pub fn read(&self, d: usize) -> f32 {
        self.buf[(self.write.wrapping_sub(d)) & self.mask]
    }

    /// Linear-interpolated read at a fractional delay `d >= 1`.
    #[inline(always)]
    pub fn read_linear(&self, d: f32) -> f32 {
        let d = d.max(1.0);
        let i = d as usize;
        let f = d - i as f32;
        let a = self.read(i);
        let b = self.read(i + 1);
        a + f * (b - a)
    }

    /// 4-point Hermite-interpolated read at a fractional delay `d >= 2`.
    #[inline(always)]
    pub fn read_cubic(&self, d: f32) -> f32 {
        let d = d.max(2.0);
        let i = d as usize;
        let f = d - i as f32;
        let xm1 = self.read(i - 1);
        let x0 = self.read(i);
        let x1 = self.read(i + 1);
        let x2 = self.read(i + 2);
        let c1 = 0.5 * (x1 - xm1);
        let c2 = xm1 - 2.5 * x0 + 2.0 * x1 - 0.5 * x2;
        let c3 = 0.5 * (x2 - xm1) + 1.5 * (x0 - x1);
        ((c3 * f + c2) * f + c1) * f + x0
    }

    pub fn reset(&mut self) {
        self.buf.iter_mut().for_each(|v| *v = 0.0);
    }
}

/// Schroeder all-pass diffuser: `y = -g·x + d[n-D] ; d[n] = x + g·y`.
#[derive(Clone, Debug)]
pub struct Allpass {
    line: DelayLine,
    delay: usize,
    pub gain: f32,
}

impl Allpass {
    pub fn new(delay: usize, gain: f32) -> Allpass {
        Allpass { line: DelayLine::new(delay.max(1)), delay: delay.max(1), gain }
    }

    #[inline(always)]
    pub fn process(&mut self, x: f32) -> f32 {
        let d = self.line.read(self.delay);
        let v = x + self.gain * d;
        let y = d - self.gain * v;
        self.line.push(v);
        y
    }

    /// Process with a modulated (fractional) delay around the nominal delay.
    #[inline(always)]
    pub fn process_mod(&mut self, x: f32, offset: f32) -> f32 {
        let d = self.line.read_linear((self.delay as f32 + offset).clamp(1.0, self.line.max_delay() as f32));
        let v = x + self.gain * d;
        let y = d - self.gain * v;
        self.line.push(v);
        y
    }

    /// Read a tap inside the all-pass delay (used by the Dattorro plate's output taps).
    #[inline(always)]
    pub fn tap(&self, d: usize) -> f32 {
        self.line.read(d)
    }

    pub fn delay(&self) -> usize {
        self.delay
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_and_fractional_reads() {
        let mut d = DelayLine::new(16);
        for i in 0..10 {
            d.push(i as f32);
        }
        assert_eq!(d.read(1), 9.0);
        assert_eq!(d.read(3), 7.0);
        assert!((d.read_linear(2.5) - 7.5).abs() < 1e-6);
        assert!((d.read_cubic(3.5) - 6.5).abs() < 1e-5);
    }

    #[test]
    fn allpass_preserves_energy() {
        let mut ap = Allpass::new(37, 0.6);
        let mut e = 0.0f64;
        for i in 0..20000 {
            let y = ap.process(if i == 0 { 1.0 } else { 0.0 });
            e += (y as f64).powi(2);
        }
        assert!((e - 1.0).abs() < 1e-3, "{e}");
    }
}
