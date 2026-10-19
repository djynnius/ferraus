//! Integer PCM conversion and dither.
//!
//! Scaling is symmetric power-of-two (`x / 2^(n-1)` on decode, `round(x * 2^(n-1))` on
//! encode, clamped), so decoding integer PCM to `f32` and re-encoding at the same depth is
//! bit-exact for every depth up to 24 bits.

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

/// Dither applied when quantizing to integer PCM.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dither {
    /// No dither: plain rounding.
    None,
    /// TPDF (triangular) dither of +-1 LSB.
    Triangular,
    /// TPDF dither with first-order error-feedback noise shaping (pushes noise up in frequency).
    Shaped,
}

impl Dither {
    /// Parse `triangular` (default for `None`), `shaped`, or `none`.
    pub fn parse(s: Option<&str>) -> Dither {
        match s.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
            Some("none") | Some("off") => Dither::None,
            Some("shaped") | Some("noise-shaped") => Dither::Shaped,
            _ => Dither::Triangular,
        }
    }
}

/// Scale factor `2^(bits-1)` for a bit depth.
pub fn scale(bits: u16) -> f64 {
    (1u64 << (bits.clamp(1, 32) - 1)) as f64
}

/// Integer sample to `f32` (exact for `bits <= 24`).
#[inline]
pub fn int_to_f32(v: i32, bits: u16) -> f32 {
    (v as f64 / scale(bits)) as f32
}

/// True when every sample is exactly representable at `bits` (so quantizing needs no dither).
pub fn exactly_representable(data: &[&[f32]], bits: u16) -> bool {
    let s = scale(bits);
    let (lo, hi) = (-s, s - 1.0);
    data.iter().flat_map(|c| c.iter()).all(|&x| {
        let v = x as f64 * s;
        v.fract() == 0.0 && v >= lo && v <= hi
    })
}

/// Quantizes planar `f32` audio to interleaved integers at a bit depth, with optional
/// seeded dither. The state (RNG, noise-shaping error) carries across chunks so streaming
/// output is identical to whole-buffer output.
pub struct Quantizer {
    bits: u16,
    dither: Dither,
    rng: ChaCha8Rng,
    err: Vec<f64>,
    /// Number of samples that clipped.
    pub clipped: u64,
}

impl Quantizer {
    pub fn new(bits: u16, dither: Dither, seed: u64) -> Quantizer {
        Quantizer {
            bits,
            dither,
            rng: ChaCha8Rng::seed_from_u64(seed ^ 0x6665_7272_6175_7300),
            err: Vec::new(),
            clipped: 0,
        }
    }

    pub fn bits(&self) -> u16 {
        self.bits
    }

    /// Quantize one chunk to interleaved integers. Chunks that are already exactly
    /// representable at the target depth are converted without dither (bit-exact).
    pub fn quantize(&mut self, data: &[&[f32]], out: &mut Vec<i32>) {
        let ch = data.len();
        let frames = data.first().map(|c| c.len()).unwrap_or(0);
        out.clear();
        out.reserve(ch * frames);
        if self.err.len() != ch {
            self.err = vec![0.0; ch];
        }
        let s = scale(self.bits);
        let (lo, hi) = (-s, s - 1.0);
        let dither = if self.dither != Dither::None && exactly_representable(data, self.bits) {
            Dither::None
        } else {
            self.dither
        };
        for f in 0..frames {
            for (c, chan) in data.iter().enumerate() {
                let x = chan[f] as f64 * s;
                let v = match dither {
                    Dither::None => x.round(),
                    Dither::Triangular => (x + self.tpdf()).round(),
                    Dither::Shaped => {
                        let target = x - self.err[c];
                        let q = (target + self.tpdf()).round();
                        // first-order error feedback (highpass-shaped requantization noise)
                        self.err[c] = (q - target).clamp(-4.0, 4.0);
                        q
                    }
                };
                let v = if v < lo {
                    if x < lo {
                        self.clipped += 1;
                    }
                    lo
                } else if v > hi {
                    // +1.0 full scale maps to the top code without counting as a clip
                    if x > s {
                        self.clipped += 1;
                    }
                    hi
                } else {
                    v
                };
                out.push(v as i32);
            }
        }
    }

    fn tpdf(&mut self) -> f64 {
        let a: f64 = self.rng.gen();
        let b: f64 = self.rng.gen();
        a - b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn int_roundtrip_exact() {
        for bits in [8u16, 16, 24] {
            let s = scale(bits) as i32;
            let vals: Vec<i32> = vec![-s, -s + 1, -1, 0, 1, s / 3, s - 1];
            let f: Vec<f32> = vals.iter().map(|&v| int_to_f32(v, bits)).collect();
            let mut q = Quantizer::new(bits, Dither::Triangular, 1);
            let mut out = Vec::new();
            q.quantize(&[&f[..]], &mut out);
            assert_eq!(out, vals, "bits {bits}");
        }
    }

    #[test]
    fn dither_is_seeded() {
        let ch: Vec<f32> = (0..1000).map(|i| (i as f32 * 0.001).sin() * 0.3).collect();
        let data = [&ch[..]];
        let run = |seed| {
            let mut q = Quantizer::new(16, Dither::Triangular, seed);
            let mut o = Vec::new();
            q.quantize(&data, &mut o);
            o
        };
        assert_eq!(run(7), run(7));
        assert_ne!(run(7), run(8));
        let mut q = Quantizer::new(16, Dither::None, 7);
        let mut plain = Vec::new();
        q.quantize(&data, &mut plain);
        let diff: i64 = plain.iter().zip(run(7)).map(|(a, b)| (*a as i64 - b as i64).abs()).max().unwrap();
        assert!(diff <= 1, "TPDF stays within 1 LSB of rounding");
    }

    #[test]
    fn clipping_counts() {
        let mut q = Quantizer::new(16, Dither::None, 0);
        let mut o = Vec::new();
        q.quantize(&[&[1.5, -1.5, 1.0, -1.0][..]], &mut o);
        assert_eq!(o, vec![32767, -32768, 32767, -32768]);
        assert_eq!(q.clipped, 2);
    }
}
