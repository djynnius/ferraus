//! HTDemucs with its STFT and iSTFT on the host.
//!
//! The registry's htdemucs ONNX exports compute the spectrogram branch's STFT as a 4096-tap
//! convolution with an fp16 DFT basis and the iSTFT as a transposed convolution followed by an
//! overlap-add unrolled into about 16 000 nodes. On a laptop CPU that is a quarter of every
//! window's run time (0.42 s of 1.55 s on an M1 Pro) and most of the session's 6.5 s load.
//! When a registry entry names the tensors at the edges of those subgraphs
//! (`io.params.host_stft`), the graph is cut there with [`ferraus_ml::onnx_edit`] and the same
//! transforms run here with an FFT:
//!
//! * input `spec` = `STFT(reflect_pad(mix, p, p + le·hop − T))` with a periodic Hann window,
//!   `center = True` (reflect), normalized by `1/sqrt(n_fft)`, laid out `[1, C, 2, F, frames]`
//!   (real and imaginary parts on axis 2), where `p = 3·hop/2` and `le = ceil(T / hop)`;
//! * outputs `zout` `[1, S, C, 2, F, frames]` (the masked spectrogram the graph's iSTFT
//!   would read) and `time` `[1, S, C, T]` (the time branch it would be added to);
//! * stem = `time + iSTFT(zout)[p .. p + T]`, the iSTFT being the normalized inverse with the
//!   squared-window envelope (accumulated in `f64`, clipped at 1e-11) of `torch.istft`.
//!
//! The graph's transforms use an fp16-rounded basis (relative error about 2e-4); the FFT is
//! exact, so stems differ from the uncut graph by about that much (SI-SDR on MUSDB18 is
//! unchanged to 0.01 dB) and are deterministic run to run.

use crate::adapter::SeparationModel;
use ferraus_core::{Error, ErrorKind, OpCtx, Result};
use ferraus_ml::{Block, ModelSession, ModelSpec, TensorF32};
use rayon::prelude::*;
use realfft::num_complex::Complex32;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::path::Path;
use std::sync::Arc;

/// Where the transforms are cut out (`io.params.host_stft` in the registry).
#[derive(Clone, Debug, PartialEq)]
pub struct HostStftCut {
    pub n_fft: usize,
    pub hop: usize,
    /// The graph's STFT output, `[1, C, 2, n_fft/2 + 1, frames]`.
    pub spec: String,
    /// The graph's iSTFT input, `[1, S, C, 2, n_fft/2 + 1, frames]`.
    pub zout: String,
    /// The time-branch output the iSTFT result is added to, `[1, S, C, T]`.
    pub time: String,
}

impl HostStftCut {
    pub fn from_spec(spec: &ModelSpec) -> Option<HostStftCut> {
        let v = spec.params.get("host_stft")?;
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(|x| x.to_string());
        let u = |k: &str| v.get(k).and_then(|x| x.as_u64()).map(|x| x as usize);
        let cut =
            HostStftCut { n_fft: u("n_fft")?, hop: u("hop")?, spec: s("spec")?, zout: s("zout")?, time: s("time")? };
        (cut.n_fft >= 4 && cut.n_fft.is_multiple_of(2) && cut.hop * 4 == cut.n_fft).then_some(cut)
    }
}

/// Window geometry for one segment length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    pub n_fft: usize,
    pub hop: usize,
    /// Segment length (samples).
    pub t: usize,
    /// `3·hop/2`, the reflect padding on the left (`_spec`) and the crop offset (`_ispec`).
    pub pad: usize,
    /// Padded length `le·hop + 2·pad`.
    pub lp: usize,
    pub bins: usize,
    pub frames: usize,
}

impl Geometry {
    pub fn new(n_fft: usize, hop: usize, t: usize) -> Geometry {
        let le = t.div_ceil(hop);
        let pad = hop / 2 * 3;
        let lp = le * hop + 2 * pad;
        Geometry { n_fft, hop, t, pad, lp, bins: n_fft / 2 + 1, frames: 1 + lp / hop }
    }
}

fn reflect(i: isize, n: usize) -> usize {
    let n = n as isize;
    if n <= 1 {
        return 0;
    }
    let period = 2 * (n - 1);
    let mut j = i.rem_euclid(period);
    if j >= n {
        j = period - j;
    }
    j as usize
}

/// Frames processed together (transposing `[bin][frame]` in cache-friendly blocks).
const FB: usize = 8;

/// The host transforms of one geometry: FFT plans, window, and the iSTFT envelope.
pub struct Transforms {
    pub g: Geometry,
    win: Vec<f32>,
    /// `win · sqrt(n_fft) / n_fft`: the inverse basis' window and normalization.
    iwin: Vec<f32>,
    /// Squared-window envelope at output samples `n_fft/2 + pad + [0, T)`, clipped, `f32`.
    env: Vec<f32>,
    fft: Arc<dyn RealToComplex<f32>>,
    ifft: Arc<dyn ComplexToReal<f32>>,
}

impl Transforms {
    pub fn new(g: Geometry) -> Transforms {
        let n = g.n_fft;
        let win64: Vec<f64> =
            (0..n).map(|i| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / n as f64).cos()).collect();
        let win: Vec<f32> = win64.iter().map(|w| *w as f32).collect();
        let iscale = (n as f64).sqrt() / n as f64;
        let iwin = win64.iter().map(|w| (w * iscale) as f32).collect();
        // torch.istft's window envelope, as the graph builds it: f64 sum over all frames
        let full = (g.frames - 1) * g.hop + n;
        let mut env = vec![0f64; full];
        for f in 0..g.frames {
            for (i, w) in win64.iter().enumerate() {
                env[f * g.hop + i] += w * w;
            }
        }
        let off = n / 2 + g.pad;
        let env = env[off..off + g.t].iter().map(|e| e.max(1e-11) as f32).collect();
        let mut planner = RealFftPlanner::<f32>::new();
        Transforms { g, win, iwin, env, fft: planner.plan_fft_forward(n), ifft: planner.plan_fft_inverse(n) }
    }

    /// `[C][T]` → `[C][2][bins][frames]`, flattened.
    #[allow(clippy::needless_range_loop)] // bin-major transposes of frame blocks
    pub fn stft(&self, x: &[Vec<f32>]) -> Vec<f32> {
        let g = self.g;
        let plane = g.bins * g.frames;
        let mut out = vec![0f32; x.len() * 2 * plane];
        let norm = 1.0 / (g.n_fft as f32).sqrt();
        out.par_chunks_mut(2 * plane).zip(x.par_iter()).for_each(|(dst, ch)| {
            // _spec's reflect pad, then the STFT's own centering (reflect by n_fft / 2)
            let right = g.lp - g.pad - ch.len();
            let mut xp = Vec::with_capacity(g.lp);
            xp.extend((0..g.lp).map(|i| ch[reflect(i as isize - g.pad as isize, ch.len())]));
            debug_assert_eq!(xp.len(), ch.len() + g.pad + right);
            let half = (g.n_fft / 2) as isize;
            let mut input = self.fft.make_input_vec();
            let mut scratch = self.fft.make_scratch_vec();
            let mut spec = vec![self.fft.make_output_vec(); FB];
            let (re, im) = dst.split_at_mut(plane);
            let mut f0 = 0;
            while f0 < g.frames {
                let nb = FB.min(g.frames - f0);
                for (j, s) in spec.iter_mut().enumerate().take(nb) {
                    let start = ((f0 + j) * g.hop) as isize - half;
                    for (i, v) in input.iter_mut().enumerate() {
                        *v = xp[reflect(start + i as isize, g.lp)] * self.win[i];
                    }
                    self.fft.process_with_scratch(&mut input, s, &mut scratch).expect("fft");
                }
                for k in 0..g.bins {
                    let row = k * g.frames + f0;
                    for j in 0..nb {
                        re[row + j] = spec[j][k].re * norm;
                        im[row + j] = spec[j][k].im * norm;
                    }
                }
                f0 += nb;
            }
        });
        out
    }

    /// One signal's inverse: `z` = `[2][bins][frames]` flattened → `[T]` added to `time`.
    #[allow(clippy::needless_range_loop)]
    fn istft_add(&self, z: &[f32], time: &[f32], out: &mut [f32]) {
        let g = self.g;
        let n = g.n_fft;
        let plane = g.bins * g.frames;
        let (re, im) = z.split_at(plane);
        let full = (g.frames - 1) * g.hop + n;
        let mut y = vec![0f32; full];
        let mut bufs = vec![self.ifft.make_input_vec(); FB];
        let mut frame = self.ifft.make_output_vec();
        let mut scratch = self.ifft.make_scratch_vec();
        let mut f0 = 0;
        while f0 < g.frames {
            let nb = FB.min(g.frames - f0);
            for k in 0..g.bins {
                let row = k * g.frames + f0;
                for j in 0..nb {
                    bufs[j][k] = Complex32::new(re[row + j], im[row + j]);
                }
            }
            for (j, buf) in bufs.iter_mut().enumerate().take(nb) {
                // the DC and Nyquist bins of a real signal carry no imaginary part
                buf[0].im = 0.0;
                buf[g.bins - 1].im = 0.0;
                self.ifft.process_with_scratch(buf, &mut frame, &mut scratch).expect("ifft");
                let off = (f0 + j) * g.hop;
                for ((acc, v), w) in y[off..off + n].iter_mut().zip(&frame).zip(&self.iwin) {
                    *acc += v * w;
                }
            }
            f0 += nb;
        }
        let off = n / 2 + g.pad;
        for (((o, yv), e), t) in out.iter_mut().zip(&y[off..off + g.t]).zip(&self.env).zip(time) {
            *o = t + yv / e;
        }
    }

    /// `zout` `[S][C][2][bins][frames]` and `time` `[S][C][T]` (flattened) → `[S][C][T]`.
    pub fn istft_stems(&self, zout: &[f32], time: &[f32], stems: usize, channels: usize) -> Block {
        let g = self.g;
        let plane = 2 * g.bins * g.frames;
        let mut flat = vec![0f32; stems * channels * g.t];
        flat.par_chunks_mut(g.t).enumerate().for_each(|(i, out)| {
            self.istft_add(&zout[i * plane..(i + 1) * plane], &time[i * g.t..(i + 1) * g.t], out);
        });
        let mut it = flat.chunks(g.t);
        (0..stems).map(|_| (0..channels).map(|_| it.next().expect("signal").to_vec()).collect()).collect()
    }
}

/// An htdemucs export run with host transforms (see the module docs).
pub struct HostStftDemucs {
    pub name: String,
    pub spec: ModelSpec,
    pub session: Arc<ModelSession>,
    cut: HostStftCut,
    stems: Vec<String>,
    tf: Transforms,
}

impl HostStftDemucs {
    /// Cut the graph and load it. Fails (and the caller runs the uncut graph) when the model
    /// does not have the expected tensors or shapes.
    pub fn load(
        ctx: &mut OpCtx,
        name: &str,
        spec: &ModelSpec,
        path: &Path,
        cut: HostStftCut,
        lean: bool,
    ) -> Result<HostStftDemucs> {
        let unsupported = |m: String| Error::new(ErrorKind::Unsupported, "host_stft_unsupported", m);
        let t = spec.chunk_len().ok_or_else(|| unsupported(format!("{name}: no segment length")))?;
        let s = spec.stems.len();
        let g = Geometry::new(cut.n_fft, cut.hop, t);
        let (c, bins, frames) = (2i64, g.bins as i64, g.frames as i64);
        let dims_spec = vec![1, c, 2, bins, frames];
        let dims_z = vec![1, s as i64, c, 2, bins, frames];
        let dims_t = vec![1, s as i64, c, t as i64];
        let make = |raw: &[u8]| -> Result<Vec<u8>> {
            ferraus_ml::onnx_edit::Model::parse(raw)?.cut(
                &[(cut.spec.as_str(), dims_spec.clone())],
                &[(cut.zout.as_str(), dims_z.clone()), (cut.time.as_str(), dims_t.clone())],
            )
        };
        let session =
            ferraus_ml::session::load_transformed_for(ctx, path, "host-stft", &spec.provider_hints, lean, &make)?;
        let declared = session.input_names.iter().zip(&session.input_shapes).find(|(n, _)| n.as_str() != cut.spec);
        if session.input_names.len() != 2
            || !session.input_names.contains(&cut.spec)
            || declared.and_then(|(_, sh)| sh.last().copied()) != Some(t as i64)
            || session.output_names != [cut.zout.clone(), cut.time.clone()]
        {
            return Err(unsupported(format!("{name}: the cut graph does not have the expected inputs and outputs")));
        }
        Ok(HostStftDemucs {
            name: name.to_string(),
            stems: spec.stems.clone(),
            spec: spec.clone(),
            session,
            cut,
            tf: Transforms::new(g),
        })
    }
}

impl SeparationModel for HostStftDemucs {
    fn name(&self) -> &str {
        &self.name
    }
    fn stems(&self) -> &[String] {
        &self.stems
    }
    fn sample_rate(&self) -> u32 {
        self.spec.sample_rate
    }
    fn segment(&self) -> usize {
        self.tf.g.t
    }
    fn fixed_segment(&self) -> bool {
        true
    }
    fn context(&self) -> usize {
        self.spec.context_samples
    }
    fn normalize_track(&self) -> bool {
        self.spec.params.get("normalize").and_then(|v| v.as_str()) == Some("track")
    }
    fn process(&self, window: &[Vec<f32>]) -> Result<Block> {
        let g = self.tf.g;
        if window.len() != 2 || window.iter().any(|c| c.len() != g.t) {
            return Err(Error::processing(format!("{} expects stereo windows of {} samples", self.name, g.t)));
        }
        let spec = self.tf.stft(window);
        let mut mix = Vec::with_capacity(2 * g.t);
        for c in window {
            mix.extend_from_slice(c);
        }
        let mix_name = self.session.input_names.iter().find(|n| **n != self.cut.spec).cloned().unwrap_or_default();
        let out = self.session.run(vec![
            TensorF32::new(&mix_name, vec![1, 2, g.t as i64], mix),
            TensorF32::new(&self.cut.spec, vec![1, 2, 2, g.bins as i64, g.frames as i64], spec),
        ])?;
        let s = self.stems.len();
        let (z, time) = (&out[0], &out[1]);
        if z.data.len() != s * 2 * 2 * g.bins * g.frames || time.data.len() != s * 2 * g.t {
            return Err(Error::processing(format!(
                "{}: unexpected output shapes {:?} and {:?}",
                self.name, z.shape, time.shape
            )));
        }
        Ok(self.tf.istft_stems(&z.data, &time.data, s, 2))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The htdemucs window: 343 980 samples → 340 frames of 2049 bins, crop 1536.
    #[test]
    fn geometry_matches_the_htdemucs_graph() {
        let g = Geometry::new(4096, 1024, 343_980);
        assert_eq!((g.pad, g.lp, g.bins, g.frames), (1536, 347_136, 2049, 340));
    }

    /// Reference transforms written the slow, obvious way (explicit DFT sums in f64).
    fn dft_stft(x: &[f32], g: Geometry) -> Vec<f64> {
        let n = g.n_fft;
        let xp: Vec<f64> = (0..g.lp).map(|i| x[reflect(i as isize - g.pad as isize, x.len())] as f64).collect();
        let win: Vec<f64> =
            (0..n).map(|i| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / n as f64).cos()).collect();
        let mut out = vec![0f64; 2 * g.bins * g.frames];
        for f in 0..g.frames {
            for k in 0..g.bins {
                let (mut re, mut im) = (0f64, 0f64);
                for i in 0..n {
                    let v = xp[reflect((f * g.hop + i) as isize - (n / 2) as isize, g.lp)] * win[i];
                    let a = 2.0 * std::f64::consts::PI * (k * i) as f64 / n as f64;
                    re += v * a.cos();
                    im -= v * a.sin();
                }
                out[k * g.frames + f] = re / (n as f64).sqrt();
                out[g.bins * g.frames + k * g.frames + f] = im / (n as f64).sqrt();
            }
        }
        out
    }

    #[test]
    fn stft_matches_a_direct_dft_and_istft_inverts_it() {
        // small geometry with the same structure (hop = n_fft / 4)
        let g = Geometry::new(64, 16, 300);
        let tf = Transforms::new(g);
        let x: Vec<Vec<f32>> = (0..2)
            .map(|c| {
                (0..g.t)
                    .map(|i| ((i as f32) * (0.07 + 0.03 * c as f32)).sin() * 0.6 + ((i * 7 % 13) as f32 - 6.0) * 0.02)
                    .collect()
            })
            .collect();
        let spec = tf.stft(&x);
        let plane = 2 * g.bins * g.frames;
        for (c, ch) in x.iter().enumerate() {
            let want = dft_stft(ch, g);
            let got = &spec[c * plane..(c + 1) * plane];
            let err = want.iter().zip(got).map(|(a, b)| (a - *b as f64).abs()).fold(0f64, f64::max);
            assert!(err < 1e-5, "channel {c}: {err}");
        }
        // zout = the STFT itself (one stem), time = 0: the iSTFT gives the input back
        let time = vec![0f32; 2 * g.t];
        let stems = tf.istft_stems(&spec, &time, 1, 2);
        for (c, ch) in x.iter().enumerate() {
            let err = ch.iter().zip(&stems[0][c]).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
            assert!(err < 1e-5, "channel {c}: reconstruction error {err}");
        }
        // the time branch is added sample for sample
        let time: Vec<f32> = (0..2 * g.t).map(|i| i as f32 * 1e-3).collect();
        let with = tf.istft_stems(&spec, &time, 1, 2);
        assert!((with[0][1][5] - stems[0][1][5] - time[g.t + 5]).abs() < 1e-6);
    }
}
