//! ferraus-engrave: the engraving helper for ferraus.
//!
//! Verovio is LGPL-3.0, so it lives in this separate executable rather than in the main
//! `ferraus` binary (PRD open question #16). ferraus-score talks to it over a tiny
//! stdin/stdout protocol:
//!
//! ```text
//! ferraus-engrave --to svg|pdf|png [--page N] [--scale F] [--zoom PCT] < score.musicxml > out
//! ferraus-engrave --count < score.musicxml     # page count on stdout
//! ferraus-engrave --version                    # "ferraus-engrave <v> verovio <v>"
//! ```
//!
//! Input is MusicXML or MEI (auto-detected). `--to pdf` writes every page into one PDF;
//! SVG and PNG write one page (`--page`, default 1). Exit codes: 0 ok, 2 usage, 3 bad
//! input, 4 built without Verovio, 1 rendering failed.

use std::io::{Read, Write};

const USAGE: &str = "usage: ferraus-engrave --to svg|pdf|png [--page N] [--scale F] [--zoom PCT] < score.musicxml > out\n       ferraus-engrave --count < score.musicxml\n       ferraus-engrave --version";

struct Opts {
    to: Option<String>,
    page: u32,
    scale: f32,
    zoom: Option<u32>,
    count: bool,
}

fn parse(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts { to: None, page: 1, scale: 1.0, zoom: None, count: false };
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        let val = |i: usize| args.get(i + 1).cloned().ok_or_else(|| format!("{a} needs a value"));
        match a {
            "--to" => {
                let v = val(i)?;
                if !matches!(v.as_str(), "svg" | "pdf" | "png") {
                    return Err(format!("--to must be svg, pdf, or png, not '{v}'"));
                }
                o.to = Some(v);
                i += 1;
            }
            "--page" => {
                o.page = val(i)?.parse().map_err(|_| "--page must be a page number".to_string())?;
                i += 1;
            }
            "--scale" => {
                o.scale = val(i)?.parse().map_err(|_| "--scale must be a number".to_string())?;
                i += 1;
            }
            "--zoom" => {
                o.zoom = Some(val(i)?.parse().map_err(|_| "--zoom must be a percentage".to_string())?);
                i += 1;
            }
            "--count" => o.count = true,
            other => return Err(format!("unknown argument '{other}'")),
        }
        i += 1;
    }
    if o.to.is_none() && !o.count {
        return Err("give --to svg|pdf|png or --count".into());
    }
    if o.page == 0 {
        return Err("--page starts at 1".into());
    }
    Ok(o)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return;
    }
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("ferraus-engrave {} {}", env!("CARGO_PKG_VERSION"), engine_version());
        return;
    }
    let opts = match parse(&args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("ferraus-engrave: {e}\n{USAGE}");
            std::process::exit(2);
        }
    };
    let mut input = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut input) {
        eprintln!("ferraus-engrave: cannot read stdin: {e}");
        std::process::exit(3);
    }
    match engrave(&input, &opts) {
        Ok(bytes) => {
            let mut out = std::io::stdout().lock();
            if out.write_all(&bytes).and_then(|_| out.flush()).is_err() {
                std::process::exit(1);
            }
        }
        Err((code, msg)) => {
            eprintln!("ferraus-engrave: {msg}");
            std::process::exit(code);
        }
    }
}

#[cfg(feature = "verovio")]
fn engine_version() -> String {
    format!("verovio {}", verovio::Toolkit::new().version())
}

#[cfg(not(feature = "verovio"))]
fn engine_version() -> String {
    "(built without verovio)".into()
}

#[cfg(feature = "verovio")]
fn engrave(input: &str, o: &Opts) -> Result<Vec<u8>, (i32, String)> {
    let mut tk = verovio::Toolkit::new();
    // A4 portrait, breaks chosen by Verovio, page height kept fixed for print
    tk.set_options(r#"{"pageWidth": 2100, "pageHeight": 2970, "breaks": "auto", "footer": "none", "header": "auto", "adjustPageHeight": false}"#)
        .map_err(|e| (1, format!("Verovio rejected the layout options: {e:?}")))?;
    if let Some(z) = o.zoom {
        tk.set_zoom(z).map_err(|e| (2, format!("bad --zoom: {e:?}")))?;
    }
    tk.load_data(input).map_err(|_| (3, "Verovio could not read the input (expected MusicXML or MEI)".to_string()))?;
    let pages = tk.page_count();
    if o.count {
        return Ok(format!("{pages}\n").into_bytes());
    }
    if pages == 0 {
        return Err((1, "the score has no pages".into()));
    }
    let svg = |tk: &mut verovio::Toolkit, p: u32| {
        tk.render_to_svg(p).map_err(|e| (1, format!("SVG rendering failed: {e:?}")))
    };
    match o.to.as_deref() {
        Some("pdf") => {
            let mut svgs = Vec::new();
            for p in 1..=pages {
                svgs.push(rasterizable(&svg(&mut tk, p)?));
            }
            verovio::raster::svgs_to_pdf(&svgs).map_err(|e| (1, format!("PDF rendering failed: {e:?}")))
        }
        Some(to) => {
            if o.page > pages {
                return Err((2, format!("page {} requested; the score has {pages}", o.page)));
            }
            let page = svg(&mut tk, o.page)?;
            if to == "png" {
                verovio::raster::svg_to_png(&rasterizable(&page), o.scale)
                    .map_err(|e| (1, format!("PNG rendering failed: {e:?}")))
            } else {
                Ok(page.into_bytes())
            }
        }
        None => Err((2, "nothing to do".into())),
    }
}

/// The rasterizer (resvg) cannot use the WOFF2 music text font Verovio embeds in its SVG,
/// so SMuFL characters inside text (metronome marks, dynamics in text) are swapped for
/// Unicode equivalents in a system font before rendering PNG and PDF. Browsers get the
/// untouched SVG.
#[cfg(feature = "verovio")]
fn rasterizable(svg: &str) -> String {
    let map = |c: char| -> Option<&'static str> {
        Some(match c as u32 {
            0xECA0 | 0xECA2 => "\u{1D15D}",
            0xECA3 | 0xECA4 => "\u{1D15E}",
            0xECA5 | 0xECA6 => "\u{2669}",
            0xECA7 | 0xECA8 => "\u{266A}",
            0xECA9 | 0xECAA => "\u{1D161}",
            0xECB7 => ".",
            0xE520 => "p",
            0xE521 => "m",
            0xE522 => "f",
            0xE523 => "r",
            0xE524 => "s",
            0xE525 => "z",
            0xE526 => "n",
            0xE000..=0xF8FF => "",
            _ => return None,
        })
    };
    const OPEN: &str = "font-family=\"Leipzig\"";
    let mut out = String::with_capacity(svg.len());
    let mut rest = svg;
    while let Some(i) = rest.find(OPEN) {
        out.push_str(&rest[..i]);
        out.push_str("font-family=\"Apple Symbols, Segoe UI Symbol, Noto Music, DejaVu Sans, serif\"");
        rest = &rest[i + OPEN.len()..];
        let Some(gt) = rest.find('>') else { break };
        // the music font is drawn much larger than text: scale it down
        let attrs = &rest[..gt];
        let attrs = match attrs.find("font-size=\"") {
            Some(fs) => {
                let start = fs + "font-size=\"".len();
                let end = attrs[start..].find('"').map(|e| start + e).unwrap_or(start);
                let px: f64 = attrs[start..end].trim_end_matches("px").parse().unwrap_or(0.0);
                format!("{}{}px{}", &attrs[..start], (px * 0.6).round(), &attrs[end..])
            }
            None => attrs.to_string(),
        };
        out.push_str(&attrs);
        out.push('>');
        rest = &rest[gt + 1..];
        let lt = rest.find('<').unwrap_or(rest.len());
        for c in rest[..lt].chars() {
            match map(c) {
                Some(r) => out.push_str(r),
                None => out.push(c),
            }
        }
        rest = &rest[lt..];
    }
    out.push_str(rest);
    out
}

#[cfg(not(feature = "verovio"))]
fn engrave(_input: &str, _o: &Opts) -> Result<Vec<u8>, (i32, String)> {
    Err((
        4,
        "this ferraus-engrave was built without Verovio (cargo build -p ferraus-engrave --features verovio)".into(),
    ))
}
