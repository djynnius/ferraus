//! Running oemer's two segmentation networks over a page.
//!
//! ferraus-score has no ONNX runtime of its own: the CLI registers a [`SegBackend`] that
//! runs the networks through ferraus-ml (`register_seg_backend`), the same way it wires
//! Basic Pitch into ferraus-midi. Tests can register a fake backend.
//!
//! Like oemer's `inference.py`, a page is scaled to about 3.0–4.35 megapixels, cut into
//! square tiles (256 px for `omr/oemer`, 288 px for `omr/oemer-seg`) every half tile, the
//! softmax outputs are averaged where tiles overlap, and each pixel takes the most
//! probable class. Tiles without ink are skipped (they are background).

use super::bitmap::Gray;
use ferraus_core::{Error, OpCtx, Result};
use std::sync::OnceLock;

/// Staff-line and symbol network (`unet_big`): classes background, staff, symbols.
pub const STAFF_MODEL: &str = "omr/oemer";
/// Symbol-type network (`seg_net`): classes background, stems/rests, noteheads, clefs/keys.
pub const SYMBOL_MODEL: &str = "omr/oemer-seg";
pub const STAFF_WINDOW: usize = 256;
pub const SYMBOL_WINDOW: usize = 288;

/// Runs the OMR networks for `score read --engine builtin`.
#[derive(Clone, Copy)]
pub struct SegBackend {
    /// Make sure the models are available (download policy of `ctx`), recording them in
    /// the envelope.
    pub prepare: fn(&mut OpCtx, &[&str]) -> Result<()>,
    /// Run `model` on `batch` NHWC `u8` RGB tiles of `window`² pixels; returns NHWC `f32`
    /// class probabilities.
    pub run: fn(&mut OpCtx, &str, usize, usize, Vec<u8>) -> Result<Vec<f32>>,
}

static BACKEND: OnceLock<SegBackend> = OnceLock::new();

/// Register the network runner (the CLI does this once at start-up).
pub fn register_seg_backend(b: SegBackend) {
    let _ = BACKEND.set(b);
}

pub fn backend() -> Result<SegBackend> {
    BACKEND.get().copied().ok_or_else(|| {
        Error::new(
            ferraus_core::ErrorKind::Unsupported,
            "omr_runtime_missing",
            "this build has no ONNX runtime wired into score read (the ferraus CLI registers one)",
        )
        .with_hint("use --engine audiveris")
    })
}

/// Target size of a page for the networks (oemer's `resize_image`).
pub fn model_size(w: usize, h: usize) -> (usize, usize) {
    let pixels = (w * h) as f64;
    if (3.0e6..=4.35e6).contains(&pixels) {
        return (w, h);
    }
    let lb = 3.0e6 / pixels;
    let ub = 4.35e6 / pixels;
    let ratio = ((lb + ub) / 2.0).sqrt();
    ((ratio * w as f64).round() as usize, (ratio * h as f64).round() as usize)
}

/// Per-pixel class map (`w × h`, values are class indices).
pub struct ClassMap {
    pub w: usize,
    pub h: usize,
    pub classes: usize,
    pub class: Vec<u8>,
    /// Probability of the winning class.
    pub prob: Vec<f32>,
}

impl ClassMap {
    pub fn mask(&self, class: u8) -> super::bitmap::Mask {
        super::bitmap::Mask { w: self.w, h: self.h, px: self.class.iter().map(|&c| (c == class) as u8).collect() }
    }
}

fn tile_starts(len: usize, win: usize, step: usize) -> Vec<usize> {
    if len <= win {
        return vec![0];
    }
    let mut v: Vec<usize> = (0..len).step_by(step).map(|s| s.min(len - win)).collect();
    v.dedup();
    v
}

/// Run one network over a page (already at model size). `progress` gets the fraction done.
pub fn segment(
    ctx: &mut OpCtx,
    b: &SegBackend,
    model: &str,
    page: &Gray,
    win: usize,
    classes: usize,
    progress: &mut dyn FnMut(&OpCtx, f64),
) -> Result<ClassMap> {
    // pad pages smaller than a tile with paper
    let (pw, ph) = (page.w.max(win), page.h.max(win));
    let at = |x: usize, y: usize| if x < page.w && y < page.h { page.at(x, y) } else { 255 };
    let step = win / 2;
    let xs = tile_starts(pw, win, step);
    let ys = tile_starts(ph, win, step);
    let mut tiles: Vec<(usize, usize)> = Vec::new();
    for &y in &ys {
        for &x in &xs {
            // skip blank paper
            let mut ink = false;
            'scan: for yy in (y..y + win).step_by(2) {
                for xx in (x..x + win).step_by(2) {
                    if at(xx, yy) < 160 {
                        ink = true;
                        break 'scan;
                    }
                }
            }
            if ink {
                tiles.push((x, y));
            }
        }
    }
    let mut acc = vec![0f32; pw * ph * classes];
    let mut cnt = vec![0u16; pw * ph];
    const BATCH: usize = 8;
    let total = tiles.len().max(1);
    for (bi, chunk) in tiles.chunks(BATCH).enumerate() {
        ctx.check_cancel()?;
        let mut data = Vec::with_capacity(chunk.len() * win * win * 3);
        for &(x, y) in chunk {
            for yy in y..y + win {
                for xx in x..x + win {
                    let v = at(xx, yy);
                    data.extend_from_slice(&[v, v, v]);
                }
            }
        }
        let out = (b.run)(ctx, model, chunk.len(), win, data)?;
        if out.len() != chunk.len() * win * win * classes {
            return Err(Error::processing(format!(
                "{model} returned {} values for {} tiles of {win}² × {classes} classes",
                out.len(),
                chunk.len()
            )));
        }
        for (ti, &(x, y)) in chunk.iter().enumerate() {
            let base = ti * win * win * classes;
            for yy in 0..win {
                for xx in 0..win {
                    let src = base + (yy * win + xx) * classes;
                    let p = (y + yy) * pw + x + xx;
                    cnt[p] += 1;
                    let dst = p * classes;
                    for c in 0..classes {
                        acc[dst + c] += out[src + c];
                    }
                }
            }
        }
        progress(ctx, ((bi + 1) * BATCH).min(total) as f64 / total as f64);
    }
    let mut class = vec![0u8; page.w * page.h];
    let mut prob = vec![1f32; page.w * page.h];
    for y in 0..page.h {
        for x in 0..page.w {
            let p = y * pw + x;
            if cnt[p] == 0 {
                continue;
            }
            let v = &acc[p * classes..(p + 1) * classes];
            let (mut best, mut bv) = (0usize, f32::MIN);
            for (c, &s) in v.iter().enumerate() {
                if s > bv {
                    bv = s;
                    best = c;
                }
            }
            class[y * page.w + x] = best as u8;
            prob[y * page.w + x] = bv / cnt[p] as f32;
        }
    }
    Ok(ClassMap { w: page.w, h: page.h, classes, class, prob })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_size_matches_oemer() {
        let (w, h) = model_size(2100, 2970);
        let px = (w * h) as f64;
        assert!((3.0e6..=4.35e6).contains(&px), "{w}x{h}");
        assert_eq!(model_size(1700, 2200), (1700, 2200));
        assert_eq!(tile_starts(600, 256, 128), vec![0, 128, 256, 344]);
        assert_eq!(tile_starts(100, 256, 128), vec![0]);
    }
}
