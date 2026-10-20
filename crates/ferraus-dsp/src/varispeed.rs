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
    let taps = (2 * span) as usize;
    // Polyphase bank: row p holds the tap weights for a fractional position of p / PHASES
    // (tap m sits at distance frac + span - 1 - m); positions in between interpolate two rows.
    const PHASES: usize = 256;
    let bank: Vec<f32> = (0..=PHASES)
        .flat_map(|p| {
            let frac = p as f64 / PHASES as f64;
            let k = &k;
            (0..taps).map(move |m| k.at((frac + (span - 1) as f64 - m as f64) * fc))
        })
        .collect();
    let mut w = vec![0.0f32; taps];
    (0..out_len)
        .map(|j| {
            let t = j as f64 * factor;
            let lo = t.floor() as isize - span + 1;
            let pf = (t - t.floor()) * PHASES as f64;
            let p = (pf as usize).min(PHASES - 1);
            let g = (pf - p as f64) as f32;
            let (r0, r1) = (&bank[p * taps..(p + 1) * taps], &bank[(p + 1) * taps..(p + 2) * taps]);
            for ((wm, a), b) in w.iter_mut().zip(r0).zip(r1) {
                *wm = a + g * (b - a);
            }
            let wsum = sum8(&w);
            let acc: f32 = if lo >= 0 && lo + taps as isize <= n {
                dot8(&x[lo as usize..lo as usize + taps], &w)
            } else {
                // near the edges, samples outside the signal count as zero
                let first = (-lo).max(0) as usize;
                let last = ((n - lo).max(0) as usize).min(taps);
                (first..last).map(|m| x[(lo + m as isize) as usize] * w[m]).sum()
            };
            if wsum.abs() > 1e-9 {
                acc / wsum
            } else {
                0.0
            }
        })
        .collect()
}

/// Dot product with eight independent accumulators, so the compiler can vectorize it.
#[inline(always)]
fn dot8(a: &[f32], b: &[f32]) -> f32 {
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

#[inline(always)]
fn sum8(a: &[f32]) -> f32 {
    let mut acc = [0.0f32; 8];
    let c = a.chunks_exact(8);
    let tail: f32 = c.remainder().iter().sum();
    for x in c {
        for (s, v) in acc.iter_mut().zip(x) {
            *s += v;
        }
    }
    acc.iter().sum::<f32>() + tail
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

    /// The polyphase bank matches evaluating the kernel at every tap to within -80 dB.
    #[test]
    fn polyphase_matches_direct_kernel() {
        let x: Vec<f32> = (0..4000u32).map(|i| ((i.wrapping_mul(2654435761) >> 8) as f32 / 8388608.0) - 1.0).collect();
        for factor in [0.7f64, 1.5, 0.31, 2.9] {
            let k = Kernel::new(16, 8.0);
            let fc = (1.0 / factor).min(1.0) * 0.97;
            let span = (k.half_width as f64 / fc).ceil() as isize;
            let n = x.len() as isize;
            let direct: Vec<f32> = (0..(x.len() as f64 / factor).ceil() as usize)
                .map(|j| {
                    let t = j as f64 * factor;
                    let base = t.floor() as isize;
                    let (mut acc, mut wsum) = (0.0f32, 0.0f32);
                    for i in (base - span + 1)..=(base + span) {
                        let w = k.at((t - i as f64) * fc);
                        wsum += w;
                        if i >= 0 && i < n {
                            acc += x[i as usize] * w;
                        }
                    }
                    acc / wsum
                })
                .collect();
            let fast = varispeed(&x, factor);
            assert_eq!(fast.len(), direct.len());
            let err = fast.iter().zip(&direct).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
            assert!(err < 1e-4, "factor {factor}: max error {err}");
        }
    }
}
