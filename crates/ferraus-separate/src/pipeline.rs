//! The separation pipeline building blocks (PRD steps 1–7), independent of argument parsing
//! so each is testable: model-rate conversion and stereo handling, windowed inference with
//! raised-cosine overlap-add, test-time augmentation, ensembles, quality estimates, and
//! residual (sum-to-mix) assignment on the output sample grid.

use crate::adapter::SeparationModel;
use crate::stft::{combine, stft};
use ferraus_core::{AudioBuffer, Error, Result};
use ferraus_ml::chunk::{from_model_rate, overlap_add, to_model_rate, Block, ChunkPlan};
use rand::{Rng, SeedableRng};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Segment {
    /// The model's trained window.
    Auto,
    /// One pass over the whole input (models with dynamic length only).
    Whole,
    Seconds(f64),
}

impl Segment {
    pub fn parse(s: &str) -> Result<Segment> {
        match s.trim() {
            "" | "auto" => Ok(Segment::Auto),
            "whole" | "none" | "off" => Ok(Segment::Whole),
            v => {
                let secs = ferraus_core::units::parse_duration_s(v)
                    .map_err(|e| Error::usage(format!("--segment: {}", e.message)))?;
                if secs <= 0.0 {
                    return Err(Error::usage("--segment must be positive"));
                }
                Ok(Segment::Seconds(secs))
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct RunOptions {
    /// Fraction of each window crossfaded with its neighbour.
    pub overlap: f64,
    /// Number of passes; pass 0 is unshifted, the others use random sub-second offsets.
    pub shifts: usize,
    pub segment: Segment,
    /// Also average channel-swapped passes (max tier).
    pub channel_swap: bool,
    pub seed: u64,
}

impl Default for RunOptions {
    fn default() -> Self {
        RunOptions { overlap: 0.25, shifts: 1, segment: Segment::Auto, channel_swap: false, seed: 0 }
    }
}

/// The window plan a model will use under `opts`, plus a note when the request was adjusted.
pub fn plan_for(model: &dyn SeparationModel, opts: &RunOptions) -> Result<(ChunkPlan, Option<String>)> {
    let sr = model.sample_rate() as f64;
    let mut note = None;
    let mut window = match opts.segment {
        Segment::Auto => model.segment(),
        Segment::Whole => 0,
        Segment::Seconds(s) => (s * sr).round() as usize,
    };
    if model.fixed_segment() && window != model.segment() {
        note = Some(format!(
            "{} only accepts {:.2} s windows; --segment ignored",
            model.name(),
            model.segment() as f64 / sr
        ));
        window = model.segment();
    }
    Ok((ChunkPlan::new(window, opts.overlap, model.context())?, note))
}

/// Offsets (samples) of the TTA passes: 0, then seeded random offsets in `1..max`.
pub fn shift_offsets(shifts: usize, sample_rate: u32, seed: u64) -> Vec<usize> {
    let max = (sample_rate as usize / 2).max(2);
    let mut rng = rand_chacha::ChaCha8Rng::seed_from_u64(seed ^ 0x5eed_5e9a);
    (0..shifts.max(1)).map(|i| if i == 0 { 0 } else { rng.gen_range(1..max) }).collect()
}

/// Run one model over a stereo signal `[2][N]` already at the model rate. Returns
/// `[S][2][N]`. `tick(fraction)` reports progress and may cancel.
pub fn run_model(
    model: &dyn SeparationModel,
    mix: &[Vec<f32>],
    opts: &RunOptions,
    tick: &mut dyn FnMut(f64) -> Result<()>,
) -> Result<Block> {
    let n = mix[0].len();
    let (plan, _) = plan_for(model, opts)?;
    // Demucs-style track normalization (scale only; restored afterwards)
    let scale = if model.normalize_track() {
        let m: Vec<f64> = (0..n).map(|i| (mix[0][i] as f64 + mix[1][i] as f64) / 2.0).collect();
        let mean = m.iter().sum::<f64>() / n.max(1) as f64;
        let var = m.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n.max(1) as f64;
        let sd = var.sqrt();
        if sd > 1e-8 {
            sd as f32
        } else {
            1.0
        }
    } else {
        1.0
    };
    let offsets = shift_offsets(opts.shifts, model.sample_rate(), opts.seed);
    let swaps: &[bool] = if opts.channel_swap { &[false, true] } else { &[false] };
    let passes = offsets.len() * swaps.len();
    let per_pass = plan.chunks(n + offsets.iter().max().copied().unwrap_or(0));
    let total = (passes * per_pass).max(1);
    let mut done = 0usize;
    let mut acc: Option<Block> = None;
    for &off in &offsets {
        for &swap in swaps {
            let input: Vec<Vec<f32>> = (0..2)
                .map(|c| {
                    let src = if swap { &mix[1 - c] } else { &mix[c] };
                    let mut v = vec![0f32; off];
                    v.extend(src.iter().map(|x| x / scale));
                    v
                })
                .collect();
            let mut f = |w: &[Vec<f32>], _k: usize| model.process(w);
            let mut t = |_d: usize, _t: usize| {
                done += 1;
                tick(done as f64 / total as f64)
            };
            let out = overlap_add(&input, &plan, &mut f, &mut t)?;
            let acc = acc.get_or_insert_with(|| vec![vec![vec![0f32; n]; 2]; out.len()]);
            if acc.len() != out.len() {
                return Err(Error::processing("stem count changed between passes"));
            }
            for (a, o) in acc.iter_mut().zip(&out) {
                if o.len() != 2 {
                    return Err(Error::processing(format!(
                        "{} returned {} channels per stem; separation models must be stereo",
                        model.name(),
                        o.len()
                    )));
                }
                for c in 0..2 {
                    let src = if swap { &o[1 - c] } else { &o[c] };
                    for i in 0..n {
                        a[c][i] += src[off + i];
                    }
                }
            }
        }
    }
    let mut acc = acc.unwrap_or_default();
    let k = (passes as f32) / scale;
    for s in acc.iter_mut() {
        for c in s.iter_mut() {
            for v in c.iter_mut() {
                *v /= k;
            }
        }
    }
    Ok(acc)
}

/// Separate an audio buffer at any rate/channel count: stereo conversion (mono duplicated),
/// resampling to the model rate, inference, resampling back, trimming to the input length,
/// and folding back to mono for mono inputs. Returns `(stem, buffer)` in model order.
pub fn separate_buffer(
    model: &dyn SeparationModel,
    input: &AudioBuffer,
    opts: &RunOptions,
    tick: &mut dyn FnMut(f64) -> Result<()>,
) -> Result<Vec<(String, AudioBuffer)>> {
    let frames = input.frames();
    let channels = input.channels();
    if frames == 0 {
        return Ok(model
            .stems()
            .iter()
            .map(|s| (s.clone(), AudioBuffer::new(input.sample_rate, channels, 0)))
            .collect());
    }
    let stereo = match channels {
        1 => input.with_channels(2),
        2 => input.clone(),
        n => {
            return Err(Error::unsupported(format!(
                "separation needs mono or stereo input, got {n} channels (downmix first with --channels 2)"
            )))
        }
    };
    let at_rate = to_model_rate(&stereo, model.sample_rate());
    let block = run_model(model, &at_rate.data, opts, tick)?;
    let mut out = Vec::new();
    for (name, stem) in model.stems().iter().zip(block) {
        let b = AudioBuffer { sample_rate: model.sample_rate(), data: stem };
        let mut back = from_model_rate(&b, input.sample_rate, frames);
        if channels == 1 {
            back = back.with_channels(1);
        }
        out.push((name.clone(), back));
    }
    Ok(out)
}

/// Combine several estimates of one stem (same rate/length/channels) with weights.
pub fn ensemble(estimates: &[&AudioBuffer], weights: &[f64], mode: &str) -> AudioBuffer {
    if estimates.len() == 1 {
        return estimates[0].clone();
    }
    let ch = estimates[0].channels();
    let data = (0..ch)
        .map(|c| {
            let views: Vec<&[f32]> = estimates.iter().map(|e| e.data[c].as_slice()).collect();
            combine(&views, weights, mode, 4096, 1024)
        })
        .collect();
    AudioBuffer { sample_rate: estimates[0].sample_rate, data }
}

fn energy(b: &AudioBuffer) -> f64 {
    b.data.iter().flatten().map(|v| (*v as f64).powi(2)).sum()
}

/// Agreement between ensemble members for one stem: mean over pairs of
/// `1 - |a-b|² / (|a|² + |b|²)`, in `[0, 1]` (1 = identical estimates).
pub fn agreement(estimates: &[&AudioBuffer]) -> Option<f64> {
    if estimates.len() < 2 {
        return None;
    }
    let mut sum = 0.0;
    let mut pairs = 0;
    for i in 0..estimates.len() {
        for j in i + 1..estimates.len() {
            let (a, b) = (estimates[i], estimates[j]);
            let mut d = 0.0;
            for (ca, cb) in a.data.iter().zip(&b.data) {
                for (x, y) in ca.iter().zip(cb) {
                    d += (*x as f64 - *y as f64).powi(2);
                }
            }
            let den = energy(a) + energy(b);
            let q = if den < 1e-12 { 1.0 } else { 1.0 - d / den };
            sum += q.clamp(0.0, 1.0);
            pairs += 1;
        }
    }
    Some(sum / pairs as f64)
}

/// Mask confidence of a single model: in the STFT domain each stem's implied soft mask is
/// `|S_s| / Σ|S|`; the score is the energy-weighted mean mask value of the stem (1 = the model
/// is certain wherever the stem has energy). Not yet calibrated against reference SDR.
pub fn mask_confidence(stems: &[&AudioBuffer]) -> Vec<f64> {
    let mono: Vec<Vec<f32>> = stems.iter().map(|s| s.mono_mix()).collect();
    if mono.is_empty() || mono[0].len() < 2048 {
        return vec![1.0; stems.len()];
    }
    let specs: Vec<_> = mono.iter().map(|m| stft(m, 2048, 512)).collect();
    let mut num = vec![0f64; stems.len()];
    let mut den = vec![0f64; stems.len()];
    for f in 0..specs[0].frames.len() {
        for b in 0..specs[0].frames[f].len() {
            let mags: Vec<f64> = specs.iter().map(|s| s.frames[f][b].norm() as f64).collect();
            let total: f64 = mags.iter().sum();
            if total < 1e-9 {
                continue;
            }
            for (s, m) in mags.iter().enumerate() {
                num[s] += m * (m / total);
                den[s] += m;
            }
        }
    }
    num.iter().zip(&den).map(|(n, d)| if *d < 1e-9 { 1.0 } else { n / d }).collect()
}

/// Report of a residual assignment.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Residual {
    /// Peak of the residual (mix minus the raw stem sum) before assignment.
    pub peak: f64,
    /// Samples of the target that left the representable range.
    pub clipped: usize,
}

/// Quantize to a grid of `1/scale` (no dither), clamped to the representable range.
pub fn quantize(x: f32, scale: f64) -> f32 {
    let k = (x as f64 * scale).round().clamp(-scale, scale - 1.0);
    (k / scale) as f32
}

/// Assign the residual (mix − Σ stems) to `target` so the stems add back to the mix.
///
/// With `grid = Some(2^(bits-1))` every non-target stem is first quantized to the output
/// integer grid and the target becomes `q(mix) − Σ q(stem)`, which lies on the same grid, so
/// the decoded files sum to the input within half an LSB (no dither may be applied).
pub fn assign_residual(
    stems: &mut [(String, AudioBuffer)],
    target: &str,
    mix: &AudioBuffer,
    grid: Option<f64>,
) -> Result<Residual> {
    let ti = stems
        .iter()
        .position(|(n, _)| n == target)
        .ok_or_else(|| Error::processing(format!("no '{target}' stem for the residual")))?;
    let frames = mix.frames();
    let ch = mix.channels();
    let mut rep = Residual::default();
    for c in 0..ch {
        for i in 0..frames {
            let raw: f64 = stems.iter().map(|(_, b)| b.data[c][i] as f64).sum();
            rep.peak = rep.peak.max((mix.data[c][i] as f64 - raw).abs());
        }
    }
    if let Some(scale) = grid {
        for (j, (_, b)) in stems.iter_mut().enumerate() {
            if j == ti {
                continue;
            }
            for chan in b.data.iter_mut() {
                for v in chan.iter_mut() {
                    *v = quantize(*v, scale);
                }
            }
        }
    }
    for c in 0..ch {
        for i in 0..frames {
            let others: f64 =
                stems.iter().enumerate().filter(|(j, _)| *j != ti).map(|(_, (_, b))| b.data[c][i] as f64).sum();
            let m = match grid {
                Some(scale) => quantize(mix.data[c][i], scale) as f64,
                None => mix.data[c][i] as f64,
            };
            let mut v = m - others;
            if let Some(scale) = grid {
                let lo = -1.0;
                let hi = (scale - 1.0) / scale;
                if v < lo || v > hi {
                    rep.clipped += 1;
                    v = v.clamp(lo, hi);
                }
            }
            stems[ti].1.data[c][i] = v as f32;
        }
    }
    Ok(rep)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferraus_core::testing;

    /// A pure-Rust test model: stem k = mix filtered by a short FIR.
    struct Fir {
        kernels: Vec<Vec<f32>>,
        stems: Vec<String>,
        segment: usize,
        context: usize,
    }

    impl SeparationModel for Fir {
        fn name(&self) -> &str {
            "fir"
        }
        fn stems(&self) -> &[String] {
            &self.stems
        }
        fn sample_rate(&self) -> u32 {
            44_100
        }
        fn segment(&self) -> usize {
            self.segment
        }
        fn context(&self) -> usize {
            self.context
        }
        fn process(&self, w: &[Vec<f32>]) -> Result<Block> {
            Ok(self
                .kernels
                .iter()
                .map(|k| {
                    w.iter()
                        .map(|x| {
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
                        })
                        .collect()
                })
                .collect())
        }
    }

    fn fir(segment: usize, context: usize) -> Fir {
        Fir {
            kernels: vec![vec![0.25, 0.5, 0.25], vec![-0.25, 0.5, -0.25]],
            stems: vec!["low".into(), "high".into()],
            segment,
            context,
        }
    }

    fn mix() -> AudioBuffer {
        let a = testing::sine(44_100, 220.0, 1.0, 0.4);
        let b = testing::noise(44_100, 1.0, 0.1, 3);
        let mut m = testing::add(&a, &b).with_channels(2);
        for v in m.data[1].iter_mut() {
            *v *= 0.7;
        }
        m
    }

    #[test]
    fn shifts_average_equals_unshifted_for_shift_invariant_model() {
        let m = mix();
        let model = fir(4096, 4);
        let base = separate_buffer(&model, &m, &RunOptions::default(), &mut |_| Ok(())).unwrap();
        let opts = RunOptions { shifts: 3, channel_swap: true, seed: 7, ..Default::default() };
        let tta = separate_buffer(&model, &m, &opts, &mut |_| Ok(())).unwrap();
        for ((_, a), (_, b)) in base.iter().zip(&tta) {
            assert!(testing::rms_diff(a, b) < 1e-6);
        }
        let offs = shift_offsets(3, 44_100, 7);
        assert_eq!(offs[0], 0);
        assert!(offs[1] > 0 && offs[1] < 22_050);
        assert_eq!(offs, shift_offsets(3, 44_100, 7), "seeded");
    }

    #[test]
    fn mono_in_mono_out_and_resampling() {
        let m = testing::sine(48_000, 440.0, 0.5, 0.5);
        let model = fir(8192, 4);
        let out = separate_buffer(&model, &m, &RunOptions::default(), &mut |_| Ok(())).unwrap();
        for (_, b) in &out {
            assert_eq!(b.channels(), 1);
            assert_eq!(b.sample_rate, 48_000);
            assert_eq!(b.frames(), m.frames());
        }
        // low + high = identity filter → sum ≈ input (resampling error only)
        let sum = testing::add(&out[0].1, &out[1].1);
        assert!(testing::rms_diff(&sum, &m) < 2e-3);
    }

    #[test]
    fn residual_on_24_bit_grid_is_exact() {
        let m = mix();
        let model = Fir {
            kernels: vec![vec![0.1, 0.3, 0.1], vec![0.0, 0.2, 0.0]],
            stems: vec!["vocals".into(), "other".into()],
            segment: 0,
            context: 0,
        };
        let mut stems = separate_buffer(&model, &m, &RunOptions::default(), &mut |_| Ok(())).unwrap();
        let scale = (1u64 << 23) as f64;
        let rep = assign_residual(&mut stems, "other", &m, Some(scale)).unwrap();
        assert!(rep.peak > 0.01, "model leaves a residual");
        assert_eq!(rep.clipped, 0);
        for c in 0..2 {
            for i in 0..m.frames() {
                let s: f64 = stems.iter().map(|(_, b)| b.data[c][i] as f64).sum();
                let err = (s - m.data[c][i] as f64).abs() * scale;
                assert!(err <= 0.5 + 1e-9, "{err} LSB");
            }
        }
    }

    #[test]
    fn agreement_and_confidence_ranges() {
        let a = testing::sine(44_100, 300.0, 0.5, 0.5).with_channels(2);
        let b = testing::noise(44_100, 0.5, 0.5, 9).with_channels(2);
        assert!((agreement(&[&a, &a]).unwrap() - 1.0).abs() < 1e-9);
        let q = agreement(&[&a, &b]).unwrap();
        assert!((0.0..0.8).contains(&q), "{q}");
        assert!(agreement(&[&a]).is_none());
        let conf = mask_confidence(&[&a, &b]);
        assert_eq!(conf.len(), 2);
        assert!(conf[0] > 0.8, "a tonal stem over noise dominates its bins: {conf:?}");
        let silent = AudioBuffer::new(44_100, 2, 22_050);
        let c2 = mask_confidence(&[&a, &silent]);
        assert!(c2[0] > 0.99);
    }

    #[test]
    fn segment_parsing() {
        assert_eq!(Segment::parse("auto").unwrap(), Segment::Auto);
        assert_eq!(Segment::parse("whole").unwrap(), Segment::Whole);
        assert_eq!(Segment::parse("7.8s").unwrap(), Segment::Seconds(7.8));
        assert!(Segment::parse("-1s").is_err());
    }
}
