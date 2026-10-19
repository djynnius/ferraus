//! Arbitrary-ratio windowed-sinc resampling, used for tape-style speed changes (pitch and
//! duration change together). Anti-aliasing follows the ratio automatically.

/// Kaiser-windowed sinc table, sampled `RES` times per zero crossing.
struct Kernel {
    table: Vec<f32>,
    half_width: usize,
}

const RES: usize = 512;

impl Kernel {
    fn new(half_width: usize, beta: f64) -> Kernel {
        let n = half_width * RES + 2;
        let i0 = |x: f64| {
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
        };
        let i0b = i0(beta);
        let table = (0..n)
            .map(|i| {
                let x = i as f64 / RES as f64;
                let sinc = if x == 0.0 { 1.0 } else { (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x) };
                let r = x / half_width as f64;
                let w = if r >= 1.0 { 0.0 } else { i0(beta * (1.0 - r * r).sqrt()) / i0b };
                (sinc * w) as f32
            })
            .collect();
        Kernel { table, half_width }
    }

    #[inline(always)]
    fn at(&self, x: f64) -> f32 {
        let a = x.abs() * RES as f64;
        let i = a as usize;
        if i + 1 >= self.table.len() {
            return 0.0;
        }
        let f = (a - i as f64) as f32;
        self.table[i] + f * (self.table[i + 1] - self.table[i])
    }
}

/// Play `x` back `factor` times faster (2.0 = an octave up and half as long). Output length
/// is `ceil(len / factor)`.
pub fn varispeed(x: &[f32], factor: f64) -> Vec<f32> {
    let factor = factor.clamp(1e-3, 1e3);
    if (factor - 1.0).abs() < 1e-12 || x.is_empty() {
        return x.to_vec();
    }
    let k = Kernel::new(16, 8.0);
    let fc = (1.0 / factor).min(1.0) * 0.97;
    let span = (k.half_width as f64 / fc).ceil() as isize;
    let out_len = (x.len() as f64 / factor).ceil() as usize;
    let n = x.len() as isize;
    (0..out_len)
        .map(|j| {
            let t = j as f64 * factor;
            let base = t.floor() as isize;
            let mut acc = 0.0f32;
            let mut wsum = 0.0f32;
            for i in (base - span + 1)..=(base + span) {
                let w = k.at((t - i as f64) * fc);
                wsum += w;
                if i >= 0 && i < n {
                    acc += x[i as usize] * w;
                }
            }
            if wsum.abs() > 1e-9 {
                acc / wsum
            } else {
                0.0
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doubles_frequency_and_halves_length() {
        let sr = 8000.0;
        let x: Vec<f32> =
            (0..8000).map(|i| (2.0 * std::f64::consts::PI * 300.0 * i as f64 / sr).sin() as f32).collect();
        let y = varispeed(&x, 2.0);
        assert_eq!(y.len(), 4000);
        let zc = y[100..3900].windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
        // 600 Hz over 3800 samples at 8 kHz = 285 cycles
        assert!((280..=290).contains(&zc), "{zc}");
    }
}
