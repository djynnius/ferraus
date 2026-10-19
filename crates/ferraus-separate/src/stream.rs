//! Bounded-memory separation: the same arithmetic as [`crate::pipeline`], done window by
//! window so peak memory is O(model + a few blocks) instead of O(song × stems).
//!
//! * [`Track`] holds one signal (the mix, a member's stem estimate, a final stem) either in
//!   memory or in a spill file under `std::env::temp_dir()` (removed on drop, so success,
//!   failure, and cancel all clean up).
//! * [`resample_range`] computes any range of `ferraus_core::resample::resample_channel`'s
//!   output from a slice of its input, bit-identical to resampling the whole signal (the
//!   resampler is a fixed polyphase FIR).
//! * [`run_member`] streams one model over a track: model-rate windows are resampled on the
//!   fly, every test-time-augmentation pass keeps its own overlap-add accumulator ([`Ola`]),
//!   finished samples are averaged, resampled back, and handed to a sink in input-rate blocks.
//!   The result equals [`crate::pipeline::separate_buffer`] sample for sample.
//! * [`combine_range`] and [`ConfidenceAcc`] are the block versions of the magnitude-domain
//!   ensemble and the mask-confidence estimate (identical frames, identical sums).

use crate::adapter::SeparationModel;
use crate::pipeline::{plan_for, shift_offsets, RunOptions};
use crate::stft::hann;
use ferraus_core::resample::{resample_channel, Quality};
use ferraus_core::{AudioBuffer, AudioReader, Error, Result};
use ferraus_ml::chunk::{Block, ChunkPlan};
use realfft::num_complex::Complex32;
use realfft::RealFftPlanner;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Prefix of spill directories (`<temp>/ferraus-separate-<pid>-<n>`).
pub const SPILL_PREFIX: &str = "ferraus-separate-";

struct DirGuard(PathBuf);

impl Drop for DirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A temporary directory for spill files; removed when it and every track in it are dropped.
#[derive(Clone)]
pub struct SpillDir {
    guard: Arc<DirGuard>,
    next: Arc<AtomicUsize>,
}

impl SpillDir {
    pub fn new() -> Result<SpillDir> {
        static N: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "{SPILL_PREFIX}{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path)
            .map_err(|e| Error::processing(format!("cannot create spill directory {}: {e}", path.display())))?;
        Ok(SpillDir { guard: Arc::new(DirGuard(path)), next: Arc::new(AtomicUsize::new(0)) })
    }

    pub fn path(&self) -> &Path {
        &self.guard.0
    }

    /// A new, empty spill file in this directory.
    fn new_file(&self) -> Result<(File, PathBuf)> {
        let path = self.path().join(format!("t{}.f32", self.next.fetch_add(1, Ordering::Relaxed)));
        let file = std::fs::OpenOptions::new().read(true).write(true).create_new(true).open(&path).map_err(io_err)?;
        Ok((file, path))
    }
}

enum Store {
    Mem(Vec<Vec<f32>>),
    Disk { file: File, path: PathBuf, _dir: Arc<DirGuard> },
}

/// One signal `[channel][frame]` of fixed length, in memory or spilled to a temp file
/// (interleaved little-endian `f32`).
pub struct Track {
    pub frames: usize,
    pub channels: usize,
    store: Store,
}

impl Drop for Track {
    fn drop(&mut self) {
        if let Store::Disk { path, .. } = &self.store {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn io_err(e: std::io::Error) -> Error {
    Error::processing(format!("spill file: {e}"))
}

impl Track {
    /// A silent track; on disk when `spill` is given.
    pub fn zeros(frames: usize, channels: usize, spill: Option<&SpillDir>) -> Result<Track> {
        match spill {
            None => Ok(Track { frames, channels, store: Store::Mem(vec![vec![0f32; frames]; channels]) }),
            Some(dir) => {
                let (file, path) = dir.new_file()?;
                // sparse: unwritten ranges read back as zeros
                file.set_len((frames * channels * 4) as u64).map_err(io_err)?;
                Ok(Track { frames, channels, store: Store::Disk { file, path, _dir: dir.guard.clone() } })
            }
        }
    }

    /// Take ownership of a buffer (in memory) or copy it to a spill file.
    pub fn from_buffer(buf: AudioBuffer, spill: Option<&SpillDir>) -> Result<Track> {
        let (frames, channels) = (buf.frames(), buf.channels());
        match spill {
            None => Ok(Track { frames, channels, store: Store::Mem(buf.data) }),
            Some(_) => {
                let mut t = Track::zeros(frames, channels, spill)?;
                let step = 1 << 18;
                let mut a = 0;
                while a < frames {
                    let b = (a + step).min(frames);
                    let part: Vec<Vec<f32>> = buf.data.iter().map(|c| c[a..b].to_vec()).collect();
                    t.write(a, &part)?;
                    a = b;
                }
                Ok(t)
            }
        }
    }

    /// Decode a stream into a track: appended to a spill file chunk by chunk, or collected in
    /// memory (`hint` = expected frames, for the allocation). Returns the track and the
    /// stream's sample rate. `tick` runs once per chunk (cancellation).
    pub fn from_reader(
        r: &mut dyn AudioReader,
        hint: usize,
        spill: Option<&SpillDir>,
        tick: &mut dyn FnMut() -> Result<()>,
    ) -> Result<(Track, u32)> {
        let mut rate = r.info().sample_rate;
        let mut channels = 0usize;
        let track = match spill {
            None => {
                let mut data: Vec<Vec<f32>> = Vec::new();
                while let Some(b) = r.next_chunk()? {
                    tick()?;
                    rate = b.sample_rate;
                    if data.is_empty() {
                        channels = b.channels();
                        data = (0..channels).map(|_| Vec::with_capacity(hint.min(1 << 28))).collect();
                    }
                    for (d, s) in data.iter_mut().zip(&b.data) {
                        d.extend_from_slice(s);
                    }
                }
                if data.is_empty() {
                    channels = r.info().channels.max(1);
                    data = vec![Vec::new(); channels];
                }
                for d in data.iter_mut() {
                    d.shrink_to_fit();
                }
                Track { frames: data[0].len(), channels, store: Store::Mem(data) }
            }
            Some(dir) => {
                let (mut file, path) = dir.new_file()?;
                let mut frames = 0usize;
                let mut bytes = Vec::new();
                while let Some(b) = r.next_chunk()? {
                    tick()?;
                    rate = b.sample_rate;
                    if channels == 0 {
                        channels = b.channels();
                    }
                    bytes.clear();
                    for i in 0..b.frames() {
                        for c in &b.data {
                            bytes.extend_from_slice(&c[i].to_le_bytes());
                        }
                    }
                    file.write_all(&bytes).map_err(io_err)?;
                    frames += b.frames();
                }
                if channels == 0 {
                    channels = r.info().channels.max(1);
                }
                Track { frames, channels, store: Store::Disk { file, path, _dir: dir.guard.clone() } }
            }
        };
        Ok((track, rate.max(1)))
    }

    pub fn is_spilled(&self) -> bool {
        matches!(self.store, Store::Disk { .. })
    }

    /// Frames `[a, b)` (clamped to the track), planar.
    pub fn read(&self, a: usize, b: usize) -> Result<Vec<Vec<f32>>> {
        let b = b.min(self.frames);
        let a = a.min(b);
        match &self.store {
            Store::Mem(d) => Ok(d.iter().map(|c| c[a..b].to_vec()).collect()),
            Store::Disk { file, .. } => {
                // read in pieces so a whole-track read costs one planar copy, not two
                let ch = self.channels;
                let mut out = vec![Vec::with_capacity(b - a); ch];
                let mut f: &File = file;
                f.seek(SeekFrom::Start((a * ch * 4) as u64)).map_err(io_err)?;
                let step = 1 << 16;
                let mut bytes = vec![0u8; step.min(b - a) * ch * 4];
                let mut pos = a;
                while pos < b {
                    let len = step.min(b - pos);
                    let buf = &mut bytes[..len * ch * 4];
                    f.read_exact(buf).map_err(io_err)?;
                    for (i, s) in buf.chunks_exact(4).enumerate() {
                        out[i % ch].push(f32::from_le_bytes([s[0], s[1], s[2], s[3]]));
                    }
                    pos += len;
                }
                Ok(out)
            }
        }
    }

    /// Overwrite frames starting at `a` with `data` (`[channel][len]`).
    pub fn write(&mut self, a: usize, data: &[Vec<f32>]) -> Result<()> {
        let len = data.first().map(|c| c.len()).unwrap_or(0);
        if data.len() != self.channels || a + len > self.frames {
            return Err(Error::internal("track write out of range"));
        }
        match &mut self.store {
            Store::Mem(d) => {
                for (dst, src) in d.iter_mut().zip(data) {
                    dst[a..a + len].copy_from_slice(src);
                }
            }
            Store::Disk { file, .. } => {
                let ch = self.channels;
                let mut bytes = Vec::with_capacity(len * ch * 4);
                for i in 0..len {
                    for c in data {
                        bytes.extend_from_slice(&c[i].to_le_bytes());
                    }
                }
                file.seek(SeekFrom::Start((a * ch * 4) as u64)).map_err(io_err)?;
                file.write_all(&bytes).map_err(io_err)?;
            }
        }
        Ok(())
    }

    /// The whole track as a buffer (for encoding).
    pub fn to_buffer(&self, sample_rate: u32) -> Result<AudioBuffer> {
        Ok(AudioBuffer { sample_rate, data: self.read(0, self.frames)? })
    }
}

/// Polyphase geometry of `resample_channel(from → to, Quality::High)`.
#[derive(Clone, Copy, Debug)]
pub struct Rs {
    pub from: u32,
    pub to: u32,
    up: usize,
    down: usize,
    width: usize,
    /// Source length and the full output length `ceil(n_src · to / from)`.
    pub n_src: usize,
    pub n_out: usize,
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

impl Rs {
    pub fn new(from: u32, to: u32, n_src: usize) -> Rs {
        let g = gcd(from as u64, to as u64).max(1);
        let cutoff = (to as f64 / from as f64).min(1.0) * 0.97;
        // Quality::High uses 64 zero crossings; the bank spans ceil(64 / cutoff) taps each side
        let width = (64f64 / cutoff).ceil() as usize;
        let n_out = if from == to { n_src } else { (n_src as u64 * to as u64).div_ceil(from as u64) as usize };
        Rs { from, to, up: (to as u64 / g) as usize, down: (from as u64 / g) as usize, width, n_src, n_out }
    }

    fn base(&self, i: usize) -> usize {
        i * self.down / self.up
    }

    /// Source range `[s, e)` needed for outputs `[a, b)`; `s` is a multiple of `down` so the
    /// local filter phases line up with the global ones.
    pub fn src_range(&self, a: usize, b: usize) -> (usize, usize) {
        if self.from == self.to {
            return (a.min(self.n_src), b.min(self.n_src));
        }
        let lo = self.base(a).saturating_sub(self.width + 1);
        let s = (lo / self.down) * self.down;
        let e = if b == 0 { s } else { (self.base(b - 1) + self.width + 2).min(self.n_src) };
        (s.min(self.n_src), e.max(s.min(self.n_src)))
    }

    /// Outputs `[a, b)` from `local` = source samples `[s, s + local.len())` as returned by
    /// [`Rs::src_range`]. Outputs past the full output length are zero.
    pub fn compute(&self, local: &[f32], s: usize, a: usize, b: usize) -> Vec<f32> {
        if self.from == self.to {
            let mut v: Vec<f32> =
                (a..b).map(|i| if i >= s && i - s < local.len() { local[i - s] } else { 0.0 }).collect();
            v.truncate(b - a);
            return v;
        }
        let mut out = vec![0f32; b.saturating_sub(a)];
        let real_end = b.min(self.n_out);
        if a >= real_end || local.is_empty() {
            return out;
        }
        let o0 = s / self.down * self.up;
        let y = resample_channel(local, self.from, self.to, Quality::High);
        for i in a..real_end {
            out[i - a] = y.get(i - o0).copied().unwrap_or(0.0);
        }
        out
    }
}

/// Output samples `[a, b)` of `resample_channel(x, from, to, Quality::High)` where `x` has
/// length `n` and `get(lo, hi)` returns `x[lo..hi]`.
pub fn resample_range(
    get: &mut dyn FnMut(usize, usize) -> Result<Vec<f32>>,
    n: usize,
    from: u32,
    to: u32,
    a: usize,
    b: usize,
) -> Result<Vec<f32>> {
    let rs = Rs::new(from, to, n);
    let (s, e) = rs.src_range(a, b);
    let local = get(s, e)?;
    Ok(rs.compute(&local, s, a, b))
}

/// Streaming overlap-add of one pass (same windows and weights as
/// [`ferraus_ml::chunk::overlap_add`]).
pub struct Ola {
    plan: ChunkPlan,
    n: usize,
    k: usize,
    total: usize,
    base: usize,
    weights: Vec<f32>,
    acc: Block,
    wsum: Vec<f32>,
}

impl Ola {
    pub fn new(plan: ChunkPlan, n: usize, stems: usize) -> Ola {
        let weights = if plan.window == 0 { vec![] } else { (0..plan.valid()).map(|i| plan.weight(i)).collect() };
        Ola {
            plan,
            n,
            k: 0,
            total: if n == 0 { 0 } else { plan.chunks(n) },
            base: 0,
            weights,
            acc: vec![vec![Vec::new(); 2]; stems],
            wsum: Vec::new(),
        }
    }

    pub fn windows(&self) -> usize {
        self.total
    }

    /// Samples `< finalized()` receive no further contributions.
    pub fn finalized(&self) -> usize {
        if self.k >= self.total {
            return self.n;
        }
        if self.plan.window == 0 {
            return 0;
        }
        ((self.k * self.plan.hop()) as isize - self.plan.overlap as isize).clamp(0, self.n as isize) as usize
    }

    fn grow(&mut self, upto: usize) {
        let len = upto.saturating_sub(self.base);
        if self.wsum.len() < len {
            self.wsum.resize(len, 0.0);
            for s in self.acc.iter_mut() {
                for c in s.iter_mut() {
                    c.resize(len, 0.0);
                }
            }
        }
    }

    /// Run the next window: `input(start, len)` returns the (zero-padded) input window.
    pub fn advance(
        &mut self,
        model: &dyn SeparationModel,
        input: &mut dyn FnMut(isize, usize) -> Result<Vec<Vec<f32>>>,
    ) -> Result<()> {
        let whole = self.plan.window == 0;
        let (start, len) = if whole { (0isize, self.n) } else { (self.plan.window_start(self.k), self.plan.window) };
        let w = input(start, len)?;
        let out = model.process(&w)?;
        if out.len() != self.acc.len() {
            return Err(Error::processing("model output shape changed between windows"));
        }
        for o in &out {
            if o.len() != 2 {
                return Err(Error::processing(format!(
                    "{} returned {} channels per stem; separation models must be stereo",
                    model.name(),
                    o.len()
                )));
            }
            for c in o {
                if c.len() != len {
                    return Err(Error::processing(format!("model returned {} samples for a window of {len}", c.len())));
                }
            }
        }
        if whole {
            // one pass over everything: the output is used as is (no weighting)
            self.grow(self.n);
            for (a, o) in self.acc.iter_mut().zip(&out) {
                for (ac, oc) in a.iter_mut().zip(o) {
                    ac.copy_from_slice(&oc[..]);
                }
            }
            self.wsum.iter_mut().for_each(|w| *w = 1.0);
            self.k = self.total;
            return Ok(());
        }
        let ctx = self.plan.context;
        let vstart = start + ctx as isize;
        let hi = (vstart + self.weights.len() as isize).clamp(0, self.n as isize) as usize;
        self.grow(hi);
        for (i, w) in self.weights.iter().enumerate() {
            let idx = vstart + i as isize;
            if idx < 0 || idx as usize >= self.n {
                continue;
            }
            let j = idx as usize - self.base;
            self.wsum[j] += *w;
            for (a, o) in self.acc.iter_mut().zip(&out) {
                for (ac, oc) in a.iter_mut().zip(o) {
                    ac[j] += oc[ctx + i] * *w;
                }
            }
        }
        self.k += 1;
        Ok(())
    }

    /// Normalized output `[base, b)` (must be finalized); drops it from the accumulator.
    pub fn take(&mut self, b: usize) -> Block {
        let len = b.saturating_sub(self.base);
        self.grow(b);
        let w: Vec<f32> = self.wsum.drain(..len).collect();
        let out = self
            .acc
            .iter_mut()
            .map(|s| {
                s.iter_mut()
                    .map(|c| {
                        let mut v: Vec<f32> = c.drain(..len).collect();
                        for (x, ws) in v.iter_mut().zip(&w) {
                            if *ws > 0.0 {
                                *x /= *ws;
                            }
                        }
                        v
                    })
                    .collect()
            })
            .collect();
        self.base = b;
        out
    }
}

/// Reads the stereo model-rate view of an input track (mono duplicated, resampled on the fly).
pub struct ModelInput<'a> {
    src: &'a Track,
    rs: Rs,
}

impl<'a> ModelInput<'a> {
    pub fn new(src: &'a Track, sr_in: u32, sr_model: u32) -> ModelInput<'a> {
        ModelInput { src, rs: Rs::new(sr_in, sr_model, src.frames) }
    }

    /// Model-rate length.
    pub fn len(&self) -> usize {
        self.rs.n_out
    }

    pub fn is_empty(&self) -> bool {
        self.rs.n_out == 0
    }

    /// Model-rate samples `[lo, lo + len)`, zero outside `[0, len())`.
    pub fn read(&self, lo: isize, len: usize) -> Result<Vec<Vec<f32>>> {
        let n = self.len() as isize;
        let a = lo.clamp(0, n) as usize;
        let b = (lo + len as isize).clamp(0, n) as usize;
        if a >= b {
            return Ok(vec![vec![0f32; len]; 2]);
        }
        let (s, e) = self.rs.src_range(a, b);
        let local = self.src.read(s, e)?;
        let off = (a as isize - lo) as usize;
        let mut chans: Vec<Vec<f32>> = local
            .iter()
            .take(2)
            .map(|ch| {
                let got = self.rs.compute(ch, s, a, b);
                let mut v = vec![0f32; len];
                v[off..off + got.len()].copy_from_slice(&got);
                v
            })
            .collect();
        if chans.len() == 1 {
            // mono: both model channels carry the same samples
            chans.push(chans[0].clone());
        }
        Ok(chans)
    }
}

/// Demucs-style track scale (standard deviation of the model-rate mono mix), computed in a
/// streaming pass with the same sums as [`crate::pipeline::run_model`].
pub fn track_scale(input: &ModelInput, block: usize) -> Result<f32> {
    let n = input.len();
    let mut sum = 0f64;
    let mut a = 0;
    while a < n {
        let b = (a + block).min(n);
        let w = input.read(a as isize, b - a)?;
        for (l, r) in w[0].iter().zip(&w[1]) {
            sum += (*l as f64 + *r as f64) / 2.0;
        }
        a = b;
    }
    let mean = sum / n.max(1) as f64;
    let mut var = 0f64;
    let mut a = 0;
    while a < n {
        let b = (a + block).min(n);
        let w = input.read(a as isize, b - a)?;
        for (l, r) in w[0].iter().zip(&w[1]) {
            var += ((*l as f64 + *r as f64) / 2.0 - mean).powi(2);
        }
        a = b;
    }
    let sd = (var / n.max(1) as f64).sqrt();
    Ok(if sd > 1e-8 { sd as f32 } else { 1.0 })
}

/// Separate `src` with one model, streaming. `sink(a, block)` receives input-rate blocks
/// `[stem][channel][len]` (the input's channel count) in order, covering `[0, src.frames)`.
/// `chunk` is the block length in model-rate samples. Equals
/// [`crate::pipeline::separate_buffer`] sample for sample.
pub fn run_member(
    model: &dyn SeparationModel,
    src: &Track,
    sr_in: u32,
    opts: &RunOptions,
    chunk: usize,
    tick: &mut dyn FnMut(f64) -> Result<()>,
    sink: &mut dyn FnMut(usize, Block) -> Result<()>,
) -> Result<()> {
    let n_in = src.frames;
    let ch_in = src.channels;
    if !(1..=2).contains(&ch_in) {
        return Err(Error::unsupported(format!(
            "separation needs mono or stereo input, got {ch_in} channels (downmix first with --channels 2)"
        )));
    }
    if n_in == 0 {
        return Ok(());
    }
    let sr_m = model.sample_rate();
    let input = ModelInput::new(src, sr_in, sr_m);
    let n_m = input.len();
    let (plan, _) = plan_for(model, opts)?;
    let chunk = chunk.max(1024);
    let scale = if model.normalize_track() { track_scale(&input, chunk)? } else { 1.0 };
    let offsets = shift_offsets(opts.shifts, sr_m, opts.seed);
    let swaps: &[bool] = if opts.channel_swap { &[false, true] } else { &[false] };
    let n_stems = model.stems().len();
    let mut passes: Vec<(usize, bool, Ola)> = Vec::new();
    for &off in &offsets {
        for &swap in swaps {
            passes.push((off, swap, Ola::new(plan, n_m + off, n_stems)));
        }
    }
    let total = passes.iter().map(|p| p.2.windows()).sum::<usize>().max(1);
    let k = (passes.len() as f32) / scale;
    let mut done = 0usize;
    // back to the input rate
    let back = Rs::new(sr_m, sr_in, n_m);
    let mut hist: Block = vec![vec![Vec::new(); 2]; n_stems];
    let mut hist_start = 0usize;
    let mut out_pos = 0usize;
    let mut e = 0usize;
    while out_pos < n_in {
        let t = (e + chunk).min(n_m);
        let mut acc: Block = vec![vec![vec![0f32; t - e]; 2]; n_stems];
        for (off, swap, ola) in passes.iter_mut() {
            let (off, swap) = (*off, *swap);
            while ola.finalized() < t + off {
                let mut read = |start: isize, len: usize| -> Result<Vec<Vec<f32>>> {
                    let mut w = input.read(start - off as isize, len)?;
                    if swap {
                        w.swap(0, 1);
                    }
                    for c in w.iter_mut() {
                        for v in c.iter_mut() {
                            *v /= scale;
                        }
                    }
                    Ok(w)
                };
                ola.advance(model, &mut read)?;
                done += 1;
                tick(done as f64 / total as f64)?;
            }
            // discard the shifted lead-in, then take [e, t) of this pass
            if ola.base < e + off {
                ola.take(e + off);
            }
            let part = ola.take(t + off);
            for (a, o) in acc.iter_mut().zip(&part) {
                for c in 0..2 {
                    let src = if swap { &o[1 - c] } else { &o[c] };
                    for (x, y) in a[c].iter_mut().zip(src) {
                        *x += *y;
                    }
                }
            }
        }
        for s in acc.iter_mut() {
            for c in s.iter_mut() {
                for v in c.iter_mut() {
                    *v /= k;
                }
            }
        }
        // model-rate [e, t) is final: resample what can be finished at the input rate
        let ready = if t >= n_m {
            n_in
        } else if sr_m == sr_in {
            t.min(n_in)
        } else {
            back.ready_outputs(t).min(n_in)
        };
        let block: Block = if sr_m == sr_in {
            acc
        } else {
            for (h, a) in hist.iter_mut().zip(acc) {
                for (hc, ac) in h.iter_mut().zip(a) {
                    hc.extend_from_slice(&ac);
                }
            }
            let mut blk: Block = Vec::with_capacity(n_stems);
            if ready > out_pos {
                let (s, end) = back.src_range(out_pos, ready);
                for h in &hist {
                    let mut chans = Vec::with_capacity(2);
                    for hc in h {
                        let local = &hc[s - hist_start..(end - hist_start).min(hc.len())];
                        chans.push(back.compute(local, s, out_pos, ready));
                    }
                    blk.push(chans);
                }
                let (keep, _) = back.src_range(ready, ready + 1);
                let keep = keep.max(hist_start);
                for h in hist.iter_mut() {
                    for hc in h.iter_mut() {
                        hc.drain(..keep - hist_start);
                    }
                }
                hist_start = keep;
            }
            blk
        };
        if ready > out_pos {
            let mut block = block;
            for s in block.iter_mut() {
                for c in s.iter_mut() {
                    c.resize(ready - out_pos, 0.0);
                }
                if ch_in == 1 {
                    let mono: Vec<f32> = s[0].iter().zip(&s[1]).map(|(l, r)| (0.0 + l + r) / 2.0).collect();
                    *s = vec![mono];
                }
            }
            sink(out_pos, block)?;
            out_pos = ready;
        }
        e = t;
        if t >= n_m && out_pos < n_in {
            // the model-rate signal ended (can only happen through rounding): pad with silence
            let pad: Block = vec![vec![vec![0f32; n_in - out_pos]; ch_in]; n_stems];
            sink(out_pos, pad)?;
            out_pos = n_in;
        }
    }
    Ok(())
}

impl Rs {
    /// Number of leading outputs computable from the first `avail` source samples.
    fn ready_outputs(&self, avail: usize) -> usize {
        if avail >= self.n_src {
            return self.n_out;
        }
        let need = self.width + 2;
        if avail < need {
            return 0;
        }
        (avail - need) * self.up / self.down + 1
    }
}

fn reflect_index(i: isize, n: usize) -> usize {
    let n = n as isize;
    if n == 1 {
        return 0;
    }
    let period = 2 * (n - 1);
    let mut j = i.rem_euclid(period);
    if j >= n {
        j = period - j;
    }
    j as usize
}

/// Block version of [`crate::stft::combine`]: `estimates` cover `[ra, ra + len)` of signals of
/// global length `n`; returns the combined samples `[a, b)`. The range must include
/// `n_fft + hop` samples of margin on each side of `[a, b)` (or reach the signal's ends).
#[allow(clippy::too_many_arguments)]
pub fn combine_range(
    estimates: &[&[f32]],
    weights: &[f64],
    mode: &str,
    n_fft: usize,
    hop: usize,
    ra: usize,
    n: usize,
    a: usize,
    b: usize,
) -> Vec<f32> {
    if b <= a || n == 0 {
        return vec![];
    }
    let win = hann(n_fft);
    let pad = n_fft / 2;
    let n_frames = 1 + n / hop;
    // frames whose span [f·hop − pad, f·hop − pad + n_fft) meets [a, b)
    let f_lo = (a + pad + 1).saturating_sub(n_fft).div_ceil(hop);
    let f_hi = ((b - 1 + pad) / hop).min(n_frames - 1);
    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(n_fft);
    let ifft = planner.plan_fft_inverse(n_fft);
    let mut input = fft.make_input_vec();
    let mut specs: Vec<Vec<Complex32>> = vec![fft.make_output_vec(); estimates.len()];
    let mut out_buf = ifft.make_output_vec();
    let wsum_total: f64 = weights.iter().sum::<f64>().max(1e-12);
    // local padded coordinates: position p ↔ signal p − pad; span f_lo·hop .. f_hi·hop + n_fft
    let p0 = f_lo * hop;
    let span = (f_hi.max(f_lo) - f_lo) * hop + n_fft;
    let mut y = vec![0f32; span];
    let mut wsum = vec![0f32; span];
    for f in f_lo..=f_hi.max(f_lo) {
        if f > f_hi {
            break;
        }
        let start = (f * hop) as isize - pad as isize;
        for (e, spec) in estimates.iter().zip(specs.iter_mut()) {
            for (i, v) in input.iter_mut().enumerate() {
                let j = reflect_index(start + i as isize, n);
                *v = e.get(j.wrapping_sub(ra)).copied().unwrap_or(0.0) * win[i];
            }
            fft.process(&mut input, spec).expect("fft");
        }
        let mut frame = specs[0].clone();
        for (bin, v) in frame.iter_mut().enumerate() {
            let vals: Vec<Complex32> = specs.iter().map(|s| s[bin]).collect();
            *v = match mode {
                "max" => *vals.iter().max_by(|a, b| a.norm_sqr().total_cmp(&b.norm_sqr())).unwrap(),
                "min" => *vals.iter().min_by(|a, b| a.norm_sqr().total_cmp(&b.norm_sqr())).unwrap(),
                _ => {
                    let mut c = Complex32::new(0.0, 0.0);
                    let mut mag = 0f64;
                    for (v, w) in vals.iter().zip(weights) {
                        c += v * (*w as f32);
                        mag += v.norm() as f64 * w;
                    }
                    let mag = (mag / wsum_total) as f32;
                    let nrm = c.norm();
                    if nrm > 1e-20 {
                        c * (mag / nrm)
                    } else {
                        Complex32::new(0.0, 0.0)
                    }
                }
            };
        }
        frame[0].im = 0.0;
        if let Some(last) = frame.last_mut() {
            last.im = 0.0;
        }
        ifft.process(&mut frame, &mut out_buf).expect("ifft");
        let off = f * hop - p0;
        for i in 0..n_fft {
            y[off + i] += out_buf[i] / n_fft as f32 * win[i];
            wsum[off + i] += win[i] * win[i];
        }
    }
    (a..b)
        .map(|i| {
            let p = i + pad - p0;
            let w = wsum[p];
            if w > 1e-8 {
                y[p] / w
            } else {
                0.0
            }
        })
        .collect()
}

/// Streaming [`crate::pipeline::mask_confidence`]: feed every block's stems (with 1024
/// samples of margin) and each STFT frame is counted exactly once, by the block holding its
/// center.
pub struct ConfidenceAcc {
    n: usize,
    num: Vec<f64>,
    den: Vec<f64>,
    win: Vec<f32>,
    fft: Arc<dyn realfft::RealToComplex<f32>>,
}

impl ConfidenceAcc {
    pub const N_FFT: usize = 2048;
    pub const HOP: usize = 512;

    pub fn new(stems: usize, n: usize) -> ConfidenceAcc {
        let mut planner = RealFftPlanner::<f32>::new();
        ConfidenceAcc {
            n,
            num: vec![0.0; stems],
            den: vec![0.0; stems],
            win: hann(Self::N_FFT),
            fft: planner.plan_fft_forward(Self::N_FFT),
        }
    }

    /// `mono[s]` holds samples `[ra, ra + len)` of stem `s`'s mono mix; frames whose center lies
    /// in `[a, b)` (or at the very end when `b == n`) are counted.
    pub fn feed(&mut self, mono: &[Vec<f32>], ra: usize, a: usize, b: usize) {
        if self.n < Self::N_FFT || mono.is_empty() {
            return;
        }
        let pad = (Self::N_FFT / 2) as isize;
        let last_center = self.n / Self::HOP * Self::HOP;
        let f_lo = a.div_ceil(Self::HOP);
        let mut f_hi_excl = b.div_ceil(Self::HOP);
        if b >= self.n {
            f_hi_excl = last_center / Self::HOP + 1;
        }
        let mut input = self.fft.make_input_vec();
        let mut specs: Vec<Vec<Complex32>> = vec![self.fft.make_output_vec(); mono.len()];
        for f in f_lo..f_hi_excl {
            let start = (f * Self::HOP) as isize - pad;
            for (m, spec) in mono.iter().zip(specs.iter_mut()) {
                for (i, v) in input.iter_mut().enumerate() {
                    let j = reflect_index(start + i as isize, self.n);
                    *v = m.get(j.wrapping_sub(ra)).copied().unwrap_or(0.0) * self.win[i];
                }
                self.fft.process(&mut input, spec).expect("fft");
            }
            for bin in 0..specs[0].len() {
                let mags: Vec<f64> = specs.iter().map(|s| s[bin].norm() as f64).collect();
                let total: f64 = mags.iter().sum();
                if total < 1e-9 {
                    continue;
                }
                for (s, m) in mags.iter().enumerate() {
                    self.num[s] += m * (m / total);
                    self.den[s] += m;
                }
            }
        }
    }

    pub fn scores(&self) -> Vec<f64> {
        if self.n < Self::N_FFT {
            return vec![1.0; self.num.len()];
        }
        self.num.iter().zip(&self.den).map(|(n, d)| if *d < 1e-9 { 1.0 } else { n / d }).collect()
    }
}

/// Mono mix exactly as [`AudioBuffer::mono_mix`].
pub fn mono_of(chans: &[Vec<f32>]) -> Vec<f32> {
    if chans.len() == 1 {
        return chans[0].clone();
    }
    let n = chans.first().map(|c| c.len()).unwrap_or(0);
    let mut out = vec![0f32; n];
    for c in chans {
        for (o, s) in out.iter_mut().zip(c) {
            *o += *s;
        }
    }
    let k = chans.len() as f32;
    out.iter_mut().for_each(|o| *o /= k);
    out
}

/// Ensemble agreement accumulated over blocks (see [`crate::pipeline::agreement`]).
#[derive(Clone, Debug, Default)]
pub struct AgreementAcc {
    /// `[i][j]` squared differences (i < j) and per-estimate energies.
    diff: Vec<Vec<f64>>,
    energy: Vec<f64>,
}

impl AgreementAcc {
    pub fn new(members: usize) -> AgreementAcc {
        AgreementAcc { diff: vec![vec![0.0; members]; members], energy: vec![0.0; members] }
    }

    /// `est[m]` is `[channel][len]` of member `m` over the same frames.
    pub fn feed(&mut self, est: &[&[Vec<f32>]]) {
        for (i, e) in est.iter().enumerate() {
            self.energy[i] += e.iter().flatten().map(|v| (*v as f64).powi(2)).sum::<f64>();
            for (j, other) in est.iter().enumerate().skip(i + 1) {
                let mut d = 0.0;
                for (ca, cb) in e.iter().zip(other.iter()) {
                    for (x, y) in ca.iter().zip(cb) {
                        d += (*x as f64 - *y as f64).powi(2);
                    }
                }
                self.diff[i][j] += d;
            }
        }
    }

    pub fn members(&self) -> usize {
        self.energy.len()
    }

    pub fn score(&self) -> Option<f64> {
        let m = self.energy.len();
        if m < 2 {
            return None;
        }
        let mut sum = 0.0;
        let mut pairs = 0;
        for i in 0..m {
            for j in i + 1..m {
                let den = self.energy[i] + self.energy[j];
                let q = if den < 1e-12 { 1.0 } else { 1.0 - self.diff[i][j] / den };
                sum += q.clamp(0.0, 1.0);
                pairs += 1;
            }
        }
        Some(sum / pairs as f64)
    }
}

/// Bytes of one `f32` signal.
pub fn signal_bytes(frames: usize, channels: usize) -> u64 {
    (frames * channels * 4) as u64
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::pipeline::{separate_buffer, Segment};
    use ferraus_core::testing;

    /// Tests that create spill directories run one at a time, so one can check none are left.
    pub(crate) fn spill_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Stem k = FIR k of each channel (zero-padded inside the window), stem 2 also mixes the
    /// channels so a wrong channel swap shows up.
    struct Toy {
        stems: Vec<String>,
        rate: u32,
        normalize: bool,
    }

    fn fir(x: &[f32], k: &[f32]) -> Vec<f32> {
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

    impl SeparationModel for Toy {
        fn name(&self) -> &str {
            "toy"
        }
        fn stems(&self) -> &[String] {
            &self.stems
        }
        fn sample_rate(&self) -> u32 {
            self.rate
        }
        fn segment(&self) -> usize {
            3000
        }
        fn context(&self) -> usize {
            8
        }
        fn normalize_track(&self) -> bool {
            self.normalize
        }
        fn process(&self, w: &[Vec<f32>]) -> Result<Block> {
            let k0 = [0.25f32, 0.5, 0.25];
            let k1 = [-0.1f32, 0.3, 0.6, 0.3, -0.1];
            let s0: Vec<Vec<f32>> = w.iter().map(|c| fir(c, &k0)).collect();
            let s1: Vec<Vec<f32>> = w.iter().map(|c| fir(c, &k1)).collect();
            let s2: Vec<Vec<f32>> = vec![
                w[0].iter().zip(&w[1]).map(|(l, r)| 0.3 * l - 0.1 * r).collect(),
                w[0].iter().zip(&w[1]).map(|(l, r)| 0.05 * l + 0.2 * r).collect(),
            ];
            Ok(vec![s0, s1, s2])
        }
    }

    fn toy(rate: u32, normalize: bool) -> Toy {
        Toy { stems: vec!["a".into(), "b".into(), "c".into()], rate, normalize }
    }

    fn signal(sr: u32, secs: f64, channels: usize) -> AudioBuffer {
        let a = testing::sine(sr, 220.0, secs, 0.3);
        let b = testing::noise(sr, secs, 0.1, 4);
        let mut m = testing::add(&a, &b);
        if channels == 2 {
            m = m.with_channels(2);
            for (i, v) in m.data[1].iter_mut().enumerate() {
                *v = *v * 0.6 + ((i as f32) * 0.003).sin() * 0.2;
            }
        }
        m
    }

    fn streamed(model: &dyn SeparationModel, buf: &AudioBuffer, opts: &RunOptions, chunk: usize, disk: bool) -> Block {
        let dir = SpillDir::new().unwrap();
        let t = Track::from_buffer(buf.clone(), disk.then_some(&dir)).unwrap();
        assert_eq!(t.is_spilled(), disk);
        let n = buf.frames();
        let mut out: Block = vec![vec![vec![0f32; n]; buf.channels()]; model.stems().len()];
        let mut next = 0;
        run_member(model, &t, buf.sample_rate, opts, chunk, &mut |_| Ok(()), &mut |a, blk| {
            assert_eq!(a, next, "blocks arrive in order");
            for (o, s) in out.iter_mut().zip(&blk) {
                for (oc, sc) in o.iter_mut().zip(s) {
                    oc[a..a + sc.len()].copy_from_slice(sc);
                }
            }
            next = a + blk[0][0].len();
            Ok(())
        })
        .unwrap();
        assert_eq!(next, n, "the sink saw every frame");
        out
    }

    #[test]
    fn streaming_equals_whole_buffer_pipeline_exactly() {
        let _g = spill_lock();
        let cases = [
            (44_100u32, 44_100u32, 2usize, false, Segment::Auto),
            (48_000, 44_100, 1, true, Segment::Auto),
            (32_000, 44_100, 2, true, Segment::Auto),
            (44_100, 44_100, 2, true, Segment::Whole),
            (22_050, 44_100, 2, false, Segment::Seconds(0.05)),
        ];
        for (sr, rate, ch, normalize, segment) in cases {
            let buf = signal(sr, 0.37, ch);
            let model = toy(rate, normalize);
            for (shifts, swap) in [(1usize, false), (3, true)] {
                let opts = RunOptions { overlap: 0.25, shifts, segment, channel_swap: swap, seed: 5 };
                let whole = separate_buffer(&model, &buf, &opts, &mut |_| Ok(())).unwrap();
                for (chunk, disk) in [(1500usize, false), (4096, true), (1 << 20, false)] {
                    let got = streamed(&model, &buf, &opts, chunk, disk);
                    for ((_, w), g) in whole.iter().zip(&got) {
                        assert_eq!(w.channels(), g.len());
                        for (wc, gc) in w.data.iter().zip(g) {
                            let d = wc.iter().zip(gc).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
                            assert!(
                                d == 0.0,
                                "sr {sr} → {rate}, ch {ch}, shifts {shifts}, chunk {chunk}, disk {disk}: max diff {d}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn resample_range_matches_whole_signal() {
        let x: Vec<f32> = (0..5003).map(|i| ((i as f32) * 0.037).sin() + ((i * 7 % 11) as f32 - 5.0) * 0.01).collect();
        for (from, to) in
            [(44_100u32, 48_000u32), (48_000, 44_100), (22_050, 44_100), (44_100, 16_000), (8_000, 44_100)]
        {
            let whole = resample_channel(&x, from, to, Quality::High);
            let n_out = whole.len();
            assert_eq!(n_out, Rs::new(from, to, x.len()).n_out);
            for (a, b) in [(0usize, 10usize), (0, n_out), (17, 900), (n_out / 2, n_out), (n_out - 3, n_out + 5)] {
                let got = resample_range(&mut |lo, hi| Ok(x[lo..hi].to_vec()), x.len(), from, to, a, b).unwrap();
                for (i, v) in got.iter().enumerate() {
                    let want = whole.get(a + i).copied().unwrap_or(0.0);
                    assert!(*v == want, "{from}→{to} [{a},{b}) at {}: {v} vs {want}", a + i);
                }
            }
        }
    }

    #[test]
    fn block_combine_and_confidence_match_whole_signal() {
        let n = 10_007;
        let a: Vec<f32> = (0..n).map(|i| ((i as f32) * 0.021).sin() * 0.5).collect();
        let b: Vec<f32> = testing::noise(44_100, 0.25, 0.3, 2).data[0].iter().cycle().take(n).copied().collect();
        let c: Vec<f32> = a.iter().zip(&b).map(|(x, y)| 0.7 * x - 0.2 * y).collect();
        let (n_fft, hop) = (512usize, 128usize);
        for mode in ["avg", "max", "min"] {
            let whole = crate::stft::combine(&[&a, &b, &c], &[0.5, 0.3, 0.2], mode, n_fft, hop);
            let mut got = Vec::new();
            let mut s = 0;
            while s < n {
                let e = (s + 1000).min(n);
                let (ra, rb) = (s.saturating_sub(n_fft + hop), (e + n_fft + hop).min(n));
                let v = [&a[ra..rb], &b[ra..rb], &c[ra..rb]];
                got.extend(combine_range(&v, &[0.5, 0.3, 0.2], mode, n_fft, hop, ra, n, s, e));
                s = e;
            }
            assert_eq!(got.len(), n);
            assert!(got.iter().zip(&whole).all(|(x, y)| x == y), "{mode}: block combine differs");
        }
        // mask confidence, fed block by block with 1024 samples of margin
        let bufs: Vec<AudioBuffer> = [&a, &b, &c].iter().map(|v| AudioBuffer::from_mono(44_100, v.to_vec())).collect();
        let refs: Vec<&AudioBuffer> = bufs.iter().collect();
        let whole = crate::pipeline::mask_confidence(&refs);
        let mut acc = ConfidenceAcc::new(3, n);
        let mut s = 0;
        while s < n {
            let e = (s + 1500).min(n);
            let (ca, cb) = (s.saturating_sub(1024), (e + 1024).min(n));
            let monos = vec![a[ca..cb].to_vec(), b[ca..cb].to_vec(), c[ca..cb].to_vec()];
            acc.feed(&monos, ca, s, e);
            s = e;
        }
        assert_eq!(acc.scores(), whole);
    }

    #[test]
    fn spill_files_are_removed_on_drop() {
        let _g = spill_lock();
        let dir = SpillDir::new().unwrap();
        let root = dir.path().to_path_buf();
        let mut t = Track::zeros(1000, 2, Some(&dir)).unwrap();
        t.write(10, &[vec![0.5; 5], vec![-0.25; 5]]).unwrap();
        let r = t.read(8, 16).unwrap();
        assert_eq!(r[0], vec![0.0, 0.0, 0.5, 0.5, 0.5, 0.5, 0.5, 0.0]);
        assert_eq!(r[1][2], -0.25);
        assert!(root.exists());
        drop(dir);
        assert!(root.exists(), "a live track keeps the directory");
        drop(t);
        assert!(!root.exists(), "the last track removes the directory");
    }
}
