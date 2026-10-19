//! Optical music recognition engines for `score read`.
//!
//! * `builtin`: the oemer segmentation networks and the homr transformer exported to ONNX,
//!   served by the model registry (`ferraus models pull omr/oemer`). PDF pages are
//!   rasterized with pdfium (`pdfium` cargo feature; needs the pdfium shared library).
//! * `audiveris`: the optional Audiveris bridge (`ferraus doctor --install-audiveris`),
//!   which reads PDFs and images itself and exports `.mxl`.

use crate::model::Score;
use ferraus_core::{paths, Error, ErrorKind, Result};
use std::path::{Path, PathBuf};

/// Parse `1-4`, `2`, `1-3,6` into inclusive 1-based ranges.
pub fn parse_pages(s: &str) -> Result<Vec<(u32, u32)>> {
    let mut out = Vec::new();
    for part in s.split(',').map(|p| p.trim()).filter(|p| !p.is_empty()) {
        let (a, b) = match part.split_once('-') {
            Some((a, b)) => (a.trim(), b.trim()),
            None => (part, part),
        };
        let a: u32 = a.parse().map_err(|_| Error::usage(format!("--pages: '{part}' is not a page or range like 1-4")))?;
        let b: u32 = if b.is_empty() || b == "end" { u32::MAX } else { b.parse().map_err(|_| Error::usage(format!("--pages: '{part}' is not a page or range like 1-4")))? };
        if a == 0 || b < a {
            return Err(Error::usage(format!("--pages: '{part}' is not a valid range (pages start at 1)")));
        }
        out.push((a, b));
    }
    Ok(out)
}

pub fn page_selected(ranges: &[(u32, u32)], page: u32) -> bool {
    ranges.is_empty() || ranges.iter().any(|(a, b)| page >= *a && page <= *b)
}

/// Directory of the builtin OMR models in the shared model cache.
pub fn builtin_model_dir() -> PathBuf {
    paths::models_dir().join("omr").join("oemer")
}

fn builtin_models_present() -> bool {
    let d = builtin_model_dir();
    d.is_dir() && std::fs::read_dir(&d).map(|mut r| r.any(|e| e.map(|e| e.path().extension().map(|x| x == "onnx").unwrap_or(false)).unwrap_or(false))).unwrap_or(false)
}

/// A rasterized page: width, height, 8-bit grayscale pixels.
pub struct Page {
    pub number: u32,
    pub width: u32,
    pub height: u32,
    pub gray: Vec<u8>,
}

/// Rasterize selected PDF pages at `dpi`.
#[cfg(feature = "pdfium")]
pub fn rasterize_pdf(bytes: &[u8], ranges: &[(u32, u32)], dpi: u32) -> Result<Vec<Page>> {
    use pdfium_render::prelude::*;
    let local = paths::tools_dir().join("pdfium");
    let bindings = Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path(&local))
        .or_else(|_| Pdfium::bind_to_system_library())
        .map_err(|e| {
            Error::new(ErrorKind::Unsupported, "pdfium_missing", format!("PDF rasterization needs the pdfium library: {e}"))
                .with_hint(format!("put the pdfium shared library in {} or on the system library path, pass page images instead, or use --engine audiveris", local.display()))
        })?;
    let pdfium = Pdfium::new(bindings);
    let doc = pdfium
        .load_pdf_from_byte_slice(bytes, None)
        .map_err(|e| Error::input(format!("cannot open the PDF: {e}")))?;
    let mut out = Vec::new();
    for (i, page) in doc.pages().iter().enumerate() {
        let n = i as u32 + 1;
        if !page_selected(ranges, n) {
            continue;
        }
        let cfg = PdfRenderConfig::new().scale_page_by_factor(dpi as f32 / 72.0);
        let bmp = page.render_with_config(&cfg).map_err(|e| Error::processing(format!("cannot render page {n}: {e}")))?;
        let (w, h) = (bmp.width() as u32, bmp.height() as u32);
        let raw = bmp.as_rgba_bytes();
        let gray = raw.chunks_exact(4).map(|p| ((p[0] as u32 * 299 + p[1] as u32 * 587 + p[2] as u32 * 114) / 1000) as u8).collect();
        out.push(Page { number: n, width: w, height: h, gray });
    }
    Ok(out)
}

/// Rasterize selected PDF pages at `dpi` (this build has no pdfium support).
#[cfg(not(feature = "pdfium"))]
pub fn rasterize_pdf(_bytes: &[u8], _ranges: &[(u32, u32)], _dpi: u32) -> Result<Vec<Page>> {
    Err(Error::new(ErrorKind::Unsupported, "pdfium_missing", "this build was compiled without PDF rasterization (pdfium)")
        .with_hint("pass page images (PNG/JPEG) instead, use --engine audiveris (it reads PDFs itself), or build ferraus-score with --features pdfium"))
}

/// What a recognition run produced.
pub struct Recognized {
    pub score: Score,
    pub unsupported: Vec<String>,
    pub engine: String,
    pub movements: usize,
}

/// Recognize sheet music from a PDF or image.
pub fn recognize(input: &Path, engine: &str, pages: Option<&str>, dpi: u32) -> Result<Recognized> {
    let ranges = match pages {
        Some(p) => parse_pages(p)?,
        None => Vec::new(),
    };
    match engine {
        "audiveris" => {
            let docs = crate::audiveris::transcribe(input, pages)?;
            let movements = docs.len();
            let mut iter = docs.into_iter();
            let (mut score, mut unsupported) = crate::musicxml::read_bytes(&iter.next().unwrap())?;
            for d in iter {
                // append further movements' measures to matching parts
                let (s2, u2) = crate::musicxml::read_bytes(&d)?;
                unsupported.extend(u2);
                append_movement(&mut score, s2);
            }
            Ok(Recognized { score, unsupported, engine: "audiveris".into(), movements })
        }
        _ => {
            if !builtin_models_present() {
                return Err(Error::new(
                    ErrorKind::ModelNotCached,
                    "model_not_cached",
                    "the builtin OMR models (oemer segmentation + homr) are not in the model cache",
                )
                .with_hint("ferraus models pull omr/oemer"));
            }
            let bytes = std::fs::read(input)?;
            if bytes.starts_with(b"%PDF") {
                let pages = rasterize_pdf(&bytes, &ranges, dpi)?;
                if pages.is_empty() {
                    return Err(Error::input("no pages selected"));
                }
            }
            Err(Error::new(ErrorKind::Unsupported, "omr_runtime_missing", "the builtin OMR models are cached, but this build has no ONNX runtime wired into ferraus-score")
                .with_hint("use --engine audiveris (ferraus doctor --install-audiveris --yes) until the ferraus-ml OMR runner lands"))
        }
    }
}

fn append_movement(score: &mut Score, other: Score) {
    if other.divisions != score.divisions {
        let l = crate::pitch::lcm(score.divisions as u64, other.divisions as u64) as u32;
        score.rescale(l);
        let mut o = other;
        o.rescale(l);
        return append_movement(score, o);
    }
    for (pi, p) in other.parts.into_iter().enumerate() {
        if let Some(dst) = score.parts.get_mut(pi) {
            for (si, st) in p.staves.into_iter().enumerate() {
                if let Some(dst_st) = dst.staves.get_mut(si) {
                    let base = dst_st.measures.last().map(|m| m.number).unwrap_or(0);
                    for mut m in st.measures {
                        m.number += base;
                        dst_st.measures.push(m);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages() {
        assert_eq!(parse_pages("1-4").unwrap(), vec![(1, 4)]);
        assert_eq!(parse_pages("2, 5-6").unwrap(), vec![(2, 2), (5, 6)]);
        assert!(parse_pages("0-2").is_err());
        assert!(parse_pages("x").is_err());
        let r = parse_pages("1-2,4").unwrap();
        assert!(page_selected(&r, 4) && !page_selected(&r, 3));
    }
}
