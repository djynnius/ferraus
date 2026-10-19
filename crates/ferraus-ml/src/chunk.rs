//! Generic chunked inference with raised-cosine overlap-add.
//!
//! A model sees fixed windows of `window` samples. The outer `context` samples on each side
//! of a window are receptive-field padding and are discarded; the remaining `valid` samples
//! are crossfaded with their neighbours over `overlap` samples using complementary
//! raised-cosine ramps (the two ramps sum to exactly one), and the result is normalized by the
//! accumulated weight. The signal is padded with zeros so the first and last real samples
//! fall in the flat part of a window. For any model whose receptive field fits inside
//! `context`, the chunked result therefore equals processing the whole file in one pass
//! (zero-padded at its ends) up to float rounding, with no seams at window boundaries.

use ferraus_core::resample::{resample, Quality};
use ferraus_core::{AudioBuffer, Error, Result};

/// Planar multi-output block: `[output][channel][sample]`.
pub type Block = Vec<Vec<Vec<f32>>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChunkPlan {
    /// Model input length in samples (`0` = the whole signal in one call).
    pub window: usize,
    /// Crossfade length inside the valid region.
    pub overlap: usize,
    /// Samples discarded at each window edge.
    pub context: usize,
}

impl ChunkPlan {
    /// `overlap` is a fraction of the valid region (`--overlap 25%` → 0.25).
    pub fn new(window: usize, overlap: f64, context: usize) -> Result<ChunkPlan> {
        if window == 0 {
            return Ok(ChunkPlan {
                window: 0,
                overlap: 0,
                context: 0,
            });
        }
        if window <= 2 * context + 1 {
            return Err(Error::usage(format!(
                "window of {window} samples is too short for {context} samples of context"
            )));
        }
        if !(0.0..0.95).contains(&overlap) {
            return Err(Error::usage("--overlap must be between 0% and 95%"));
        }
        let valid = window - 2 * context;
        let ov = ((valid as f64) * overlap).round() as usize;
        Ok(ChunkPlan {
            window,
            overlap: ov.min(valid - 1),
            context,
        })
    }

    pub fn whole() -> ChunkPlan {
        ChunkPlan {
            window: 0,
            overlap: 0,
            context: 0,
        }
    }

    pub fn valid(&self) -> usize {
        self.window - 2 * self.context
    }

    pub fn hop(&self) -> usize {
        self.valid() - self.overlap
    }

    /// Number of model calls for `n` samples.
    pub fn chunks(&self, n: usize) -> usize {
        if self.window == 0 {
            return 1;
        }
        // valid regions start at -overlap and must reach n + overlap
        let need = n + 2 * self.overlap;
        if need <= self.valid() {
            1
        } else {
            1 + (need - self.valid()).div_ceil(self.hop())
        }
    }

    /// Start of window `k` (including context) in signal coordinates.
    pub fn window_start(&self, k: usize) -> isize {
        (k * self.hop()) as isize - self.overlap as isize - self.context as isize
    }

    /// Windows whose valid region starts at a sample index (for seam tests).
    pub fn boundaries(&self, n: usize) -> Vec<usize> {
        (1..self.chunks(n))
            .map(|k| (k * self.hop()) as isize - self.overlap as isize)
            .filter(|b| *b > 0 && (*b as usize) < n)
            .map(|b| b as usize)
            .collect()
    }

    /// Weight of valid-region sample `i`.
    pub fn weight(&self, i: usize) -> f32 {
        let l = self.overlap;
        let v = self.valid();
        if l == 0 {
            return 1.0;
        }
        let ramp = |j: usize| {
            (0.5 - 0.5 * (std::f64::consts::PI * (j as f64 + 0.5) / l as f64).cos()) as f32
        };
        if i < l {
            ramp(i)
        } else if i >= v - l {
            ramp(v - 1 - i)
        } else {
            1.0
        }
    }
}

/// Run `f` over `input` (`[channel][sample]`) in windows and overlap-add the outputs.
///
/// `f(window, index)` receives `[channel][plan.window]` (or the whole signal when
/// `plan.window == 0`) and returns `[output][channel][same length]`. `tick(done, total)` is
/// called after each window (progress and cancellation).
pub fn overlap_add(
    input: &[Vec<f32>],
    plan: &ChunkPlan,
    f: &mut dyn FnMut(&[Vec<f32>], usize) -> Result<Block>,
    tick: &mut dyn FnMut(usize, usize) -> Result<()>,
) -> Result<Block> {
    let n = input.first().map(|c| c.len()).unwrap_or(0);
    if plan.window == 0 {
        let out = f(input, 0)?;
        check_block(&out, n)?;
        tick(1, 1)?;
        return Ok(out);
    }
    let total = plan.chunks(n);
    let valid = plan.valid();
    let weights: Vec<f32> = (0..valid).map(|i| plan.weight(i)).collect();
    let mut acc: Option<Block> = None;
    let mut wsum = vec![0f32; n];
    let mut window: Vec<Vec<f32>> = vec![vec![0f32; plan.window]; input.len()];
    for k in 0..total {
        let start = plan.window_start(k);
        for (c, ch) in input.iter().enumerate() {
            for (i, w) in window[c].iter_mut().enumerate() {
                let idx = start + i as isize;
                *w = if idx >= 0 && (idx as usize) < n {
                    ch[idx as usize]
                } else {
                    0.0
                };
            }
        }
        let out = f(&window, k)?;
        check_block(&out, plan.window)?;
        let acc = acc.get_or_insert_with(|| {
            out.iter()
                .map(|o| vec![vec![0f32; n]; o.len()])
                .collect()
        });
        if acc.len() != out.len() || acc.iter().zip(&out).any(|(a, o)| a.len() != o.len()) {
            return Err(Error::processing("model output shape changed between windows"));
        }
        let vstart = start + plan.context as isize;
        for (i, w) in weights.iter().enumerate() {
            let idx = vstart + i as isize;
            if idx < 0 || idx as usize >= n {
                continue;
            }
            let idx = idx as usize;
            wsum[idx] += *w;
            for (a, o) in acc.iter_mut().zip(&out) {
                for (ac, oc) in a.iter_mut().zip(o) {
                    ac[idx] += oc[plan.context + i] * *w;
                }
            }
        }
        tick(k + 1, total)?;
    }
    let mut acc = acc.unwrap_or_default();
    for a in acc.iter_mut() {
        for c in a.iter_mut() {
            for (v, w) in c.iter_mut().zip(&wsum) {
                if *w > 0.0 {
                    *v /= *w;
                }
            }
        }
    }
    Ok(acc)
}

fn check_block(b: &Block, len: usize) -> Result<()> {
    for o in b {
        for c in o {
            if c.len() != len {
                return Err(Error::processing(format!(
                    "model returned {} samples for a window of {len}",
                    c.len()
                )));
            }
        }
    }
    Ok(())
}

/// Resample to the model rate (no-op when equal).
pub fn to_model_rate(buf: &AudioBuffer, rate: u32) -> AudioBuffer {
    resample(buf, rate, Quality::High)
}

/// Resample back to `rate` and trim/pad to exactly `frames`.
pub fn from_model_rate(buf: &AudioBuffer, rate: u32, frames: usize) -> AudioBuffer {
    let mut out = resample(buf, rate, Quality::High);
    out.resize(frames);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conv(x: &[f32], k: &[f32]) -> Vec<f32> {
        let h = k.len() as isize / 2;
        (0..x.len() as isize)
            .map(|i| {
                k.iter()
                    .enumerate()
                    .map(|(j, kv)| {
                        let idx = i + j as isize - h;
                        if idx >= 0 && (idx as usize) < x.len() {
                            x[idx as usize] * kv
                        } else {
                            0.0
                        }
                    })
                    .sum()
            })
            .collect()
    }

    #[test]
    fn weights_are_complementary() {
        let p = ChunkPlan::new(1000, 0.25, 10).unwrap();
        let l = p.overlap;
        for j in 0..l {
            let s = p.weight(p.valid() - l + j) + p.weight(j);
            assert!((s - 1.0).abs() < 1e-6, "{s}");
        }
    }

    #[test]
    fn chunked_equals_whole_for_receptive_field_within_context() {
        let n = 10_007;
        let x: Vec<f32> = (0..n).map(|i| ((i as f32) * 0.013).sin() + ((i * 7 % 13) as f32 - 6.0) * 0.01).collect();
        let k: Vec<f32> = (0..9).map(|i| 1.0 / (1.0 + i as f32)).collect();
        let whole = conv(&x, &k);
        for (window, ov, ctx) in [(1000usize, 0.25, 8usize), (777, 0.5, 4), (512, 0.0, 6), (20_000, 0.25, 8)] {
            let p = ChunkPlan::new(window, ov, ctx).unwrap();
            let mut f = |w: &[Vec<f32>], _k: usize| -> Result<Block> { Ok(vec![vec![conv(&w[0], &k)]]) };
            let out = overlap_add(&[x.clone()], &p, &mut f, &mut |_, _| Ok(())).unwrap();
            let err = out[0][0]
                .iter()
                .zip(&whole)
                .map(|(a, b)| (a - b).abs())
                .fold(0f32, f32::max);
            assert!(err < 1e-5, "window {window}: max err {err}");
        }
    }

    #[test]
    fn short_and_empty_inputs() {
        let p = ChunkPlan::new(64, 0.25, 4).unwrap();
        let mut f = |w: &[Vec<f32>], _k: usize| -> Result<Block> { Ok(vec![w.to_vec()]) };
        let out = overlap_add(&[vec![0.5; 3]], &p, &mut f, &mut |_, _| Ok(())).unwrap();
        assert_eq!(out[0][0], vec![0.5; 3]);
        let out = overlap_add(&[vec![]], &p, &mut f, &mut |_, _| Ok(())).unwrap();
        assert!(out.is_empty() || out[0][0].is_empty());
    }

    #[test]
    fn chunk_count_covers_signal() {
        let p = ChunkPlan::new(100, 0.25, 0).unwrap();
        for n in [1usize, 50, 100, 101, 1000] {
            let k = p.chunks(n);
            let last_end = p.window_start(k - 1) + p.window as isize;
            assert!(last_end >= (n + p.overlap) as isize, "n={n}");
        }
    }
}
