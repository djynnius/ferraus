//! Page images for OMR: decoding PNG/JPEG/... with the `image` crate (alpha is composited on
//! white paper) and rasterizing PDF pages.
//!
//! PDF pages are rasterized by the first renderer available:
//!
//! 1. pdfium, when ferraus-score was built with the `pdfium` feature and the library loads;
//! 2. `pdftoppm` (poppler) on `PATH`;
//! 3. `mutool` (MuPDF) on `PATH`;
//! 4. macOS `sips`, which renders the first page only.
//!
//! Very large pages (some engravers write posters with 2100 pt wide pages) are rendered at
//! a reduced resolution so a page never exceeds about 12 megapixels; the recognizer works
//! at about 3.7 megapixels anyway.

use super::bitmap::Gray;
use super::page_selected;
use ferraus_core::{Error, ErrorKind, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// A rasterized page.
pub struct Page {
    /// 1-based page number.
    pub number: u32,
    pub image: Gray,
}

/// Largest page we rasterize, in pixels.
const MAX_PAGE_PIXELS: f64 = 12.0e6;

/// Decode an image file (PNG, JPEG, GIF, BMP, TIFF, WebP as compiled in) to grayscale with
/// transparent pixels composited on white.
pub fn decode_image(bytes: &[u8]) -> Result<Gray> {
    let img = image::load_from_memory(bytes).map_err(|e| Error::input(format!("cannot decode the image: {e}")))?;
    let rgba = img.to_rgba8();
    let (w, h) = (rgba.width() as usize, rgba.height() as usize);
    let mut px = Vec::with_capacity(w * h);
    for p in rgba.pixels() {
        let [r, g, b, a] = p.0;
        let luma = (r as u32 * 299 + g as u32 * 587 + b as u32 * 114) / 1000;
        // composite over white paper
        let v = (luma * a as u32 + 255 * (255 - a as u32)) / 255;
        px.push(v as u8);
    }
    Ok(Gray { w, h, px })
}

/// Rasterize the selected pages of a PDF at `dpi`.
pub fn rasterize_pdf(path: &Path, bytes: &[u8], ranges: &[(u32, u32)], dpi: u32) -> Result<(Vec<Page>, String)> {
    #[cfg(feature = "pdfium")]
    {
        if let Ok(p) = pdfium_pages(bytes, ranges, dpi) {
            return Ok((p, "pdfium".into()));
        }
    }
    let _ = bytes;
    let tmp = scratch_dir()?;
    let res = rasterize_with_tools(path, ranges, dpi, &tmp);
    let _ = std::fs::remove_dir_all(&tmp);
    res
}

fn scratch_dir() -> Result<PathBuf> {
    let d = std::env::temp_dir().join(format!(
        "ferraus-omr-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
    ));
    std::fs::create_dir_all(&d)?;
    Ok(d)
}

fn tool(name: &str) -> Option<PathBuf> {
    crate::engrave::find_in(std::env::var_os("PATH").as_deref(), name).or_else(|| {
        // Homebrew and MacPorts are often missing from PATH of GUI-launched agents
        ["/opt/homebrew/bin", "/usr/local/bin", "/opt/local/bin"]
            .iter()
            .map(|d| Path::new(d).join(name))
            .find(|p| p.is_file())
    })
}

/// Page sizes in points from `pdfinfo` (poppler), when available.
fn page_sizes(path: &Path) -> Vec<(f64, f64)> {
    let Some(exe) = tool("pdfinfo") else { return Vec::new() };
    let Ok(out) = Command::new(exe).args(["-f", "1", "-l", "100000"]).arg(path).output() else { return Vec::new() };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut sizes = Vec::new();
    for line in text.lines() {
        // "Page    1 size: 595.276 x 841.89 pts (A4)" or "Page size: ..."
        if let Some(rest) = line.split("size:").nth(1).filter(|_| line.starts_with("Page")) {
            let nums: Vec<f64> = rest.split_whitespace().filter_map(|t| t.parse().ok()).collect();
            if nums.len() >= 2 {
                sizes.push((nums[0], nums[1]));
            }
        }
    }
    sizes
}

/// Resolution that keeps a `w × h` pt page under [`MAX_PAGE_PIXELS`].
fn capped_dpi(dpi: u32, size_pt: Option<(f64, f64)>) -> u32 {
    match size_pt {
        Some((w, h)) if w > 0.0 && h > 0.0 => {
            let px = (w / 72.0 * dpi as f64) * (h / 72.0 * dpi as f64);
            if px > MAX_PAGE_PIXELS {
                ((dpi as f64) * (MAX_PAGE_PIXELS / px).sqrt()).floor().max(36.0) as u32
            } else {
                dpi
            }
        }
        _ => dpi,
    }
}

fn run(cmd: &mut Command, what: &str) -> Result<()> {
    let out = cmd.output().map_err(|e| Error::processing(format!("cannot run {what}: {e}")))?;
    if !out.status.success() {
        return Err(Error::processing(format!(
            "{what} failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(())
}

fn read_png(p: &Path) -> Result<Gray> {
    decode_image(&std::fs::read(p)?)
}

fn rasterize_with_tools(path: &Path, ranges: &[(u32, u32)], dpi: u32, tmp: &Path) -> Result<(Vec<Page>, String)> {
    let sizes = page_sizes(path);
    let count = if sizes.is_empty() { None } else { Some(sizes.len() as u32) };
    if let Some(exe) = tool("pdftoppm") {
        let mut pages = Vec::new();
        let last = count.unwrap_or(10_000);
        for n in 1..=last {
            if !page_selected(ranges, n) {
                continue;
            }
            let d = capped_dpi(dpi, sizes.get(n as usize - 1).copied());
            let prefix = tmp.join(format!("p{n}"));
            let mut cmd = Command::new(&exe);
            cmd.args([
                "-r",
                &d.to_string(),
                "-f",
                &n.to_string(),
                "-l",
                &n.to_string(),
                "-gray",
                "-png",
                "-singlefile",
            ])
            .arg(path)
            .arg(&prefix);
            match run(&mut cmd, "pdftoppm") {
                Ok(()) => pages.push(Page { number: n, image: read_png(&prefix.with_extension("png"))? }),
                // without pdfinfo the page count is unknown: stop at the first missing page
                Err(_) if count.is_none() && n > 1 => break,
                Err(e) => return Err(e),
            }
        }
        return Ok((pages, "pdftoppm".into()));
    }
    if let Some(exe) = tool("mutool") {
        let out = tmp.join("page-%d.png");
        let mut cmd = Command::new(&exe);
        cmd.args(["draw", "-r", &dpi.min(300).to_string(), "-c", "gray", "-o"]).arg(&out).arg(path);
        run(&mut cmd, "mutool draw")?;
        let mut pages = Vec::new();
        for n in 1..=10_000u32 {
            let p = tmp.join(format!("page-{n}.png"));
            if !p.is_file() {
                break;
            }
            if page_selected(ranges, n) {
                pages.push(Page { number: n, image: read_png(&p)? });
            }
        }
        return Ok((pages, "mutool".into()));
    }
    if cfg!(target_os = "macos") {
        if let Some(exe) = tool("sips").or_else(|| Some(PathBuf::from("/usr/bin/sips")).filter(|p| p.is_file())) {
            if !page_selected(ranges, 1) {
                return Err(pdf_tools_missing("macOS sips renders only the first page of a PDF"));
            }
            let out = tmp.join("page-1.png");
            let mut cmd = Command::new(&exe);
            cmd.args(["-s", "format", "png"]).arg(path).arg("--out").arg(&out);
            run(&mut cmd, "sips")?;
            return Ok((vec![Page { number: 1, image: read_png(&out)? }], "sips (first page only)".into()));
        }
    }
    Err(pdf_tools_missing("no PDF renderer found (pdftoppm, mutool)"))
}

fn pdf_tools_missing(why: &str) -> Error {
    Error::new(ErrorKind::Unsupported, "pdf_renderer_missing", format!("cannot rasterize the PDF: {why}")).with_hint(
        "install poppler (brew install poppler / apt install poppler-utils) for pdftoppm, pass page images (PNG/JPEG) instead, or use --engine audiveris",
    )
}

#[cfg(feature = "pdfium")]
fn pdfium_pages(bytes: &[u8], ranges: &[(u32, u32)], dpi: u32) -> Result<Vec<Page>> {
    use ferraus_core::paths;
    use pdfium_render::prelude::*;
    let local = paths::tools_dir().join("pdfium");
    let bindings = Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path(&local))
        .or_else(|_| Pdfium::bind_to_system_library())
        .map_err(|e| Error::unsupported(format!("pdfium is not available: {e}")))?;
    let pdfium = Pdfium::new(bindings);
    let doc =
        pdfium.load_pdf_from_byte_slice(bytes, None).map_err(|e| Error::input(format!("cannot open the PDF: {e}")))?;
    let mut out = Vec::new();
    for (i, page) in doc.pages().iter().enumerate() {
        let n = i as u32 + 1;
        if !page_selected(ranges, n) {
            continue;
        }
        let d = capped_dpi(dpi, Some((page.width().value as f64, page.height().value as f64)));
        let cfg = PdfRenderConfig::new().scale_page_by_factor(d as f32 / 72.0);
        let bmp =
            page.render_with_config(&cfg).map_err(|e| Error::processing(format!("cannot render page {n}: {e}")))?;
        let (w, h) = (bmp.width() as usize, bmp.height() as usize);
        let px = bmp
            .as_rgba_bytes()
            .chunks_exact(4)
            .map(|p| ((p[0] as u32 * 299 + p[1] as u32 * 587 + p[2] as u32 * 114) / 1000) as u8)
            .collect();
        out.push(Page { number: n, image: Gray { w, h, px } });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alpha_is_composited_on_white() {
        let mut img = image::RgbaImage::new(2, 1);
        img.put_pixel(0, 0, image::Rgba([0, 0, 0, 0]));
        img.put_pixel(1, 0, image::Rgba([0, 0, 0, 255]));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
            .unwrap();
        let g = decode_image(&bytes).unwrap();
        assert_eq!(g.px, vec![255, 0]);
        assert!(decode_image(b"not an image").is_err());
    }

    #[test]
    fn huge_pages_are_rendered_smaller() {
        assert_eq!(capped_dpi(300, Some((595.0, 842.0))), 300);
        let d = capped_dpi(300, Some((2100.0, 2970.0)));
        assert!(d < 150, "{d}");
        assert_eq!(capped_dpi(300, None), 300);
    }
}
