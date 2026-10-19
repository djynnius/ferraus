//! `waveform` (PNG, SVG, JSON peaks) and `spectrogram` (PNG, JSON matrix).

use crate::util;
use ferraus_core::op::Inputs;
use ferraus_core::prelude::*;
use realfft::RealFftPlanner;
use std::path::PathBuf;

pub fn ops() -> Vec<OpSpec> {
    vec![waveform_spec(), spectrogram_spec()]
}

/// Pick the output path and image format (from `-o`'s extension, `--format`, or the default).
fn image_output(ctx: &OpCtx, allowed: &[&str], default: &str) -> Result<(PathBuf, String)> {
    let ext = match &ctx.globals.format {
        Some(f) if allowed.contains(&f.as_str()) => f.clone(),
        _ => default.to_string(),
    };
    let path = ctx.output_path(None, &ext)?;
    if path.as_os_str() == "-" {
        return Ok((path, ext));
    }
    let real = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if !allowed.contains(&real.as_str()) {
        return Err(Error::new(
            ErrorKind::Unsupported,
            "unsupported_output_format",
            format!("'{}' must end in {}", path.display(), allowed.iter().map(|e| format!(".{e}")).collect::<Vec<_>>().join(", ")),
        ));
    }
    Ok((path, real))
}

/// Parse `#rrggbb`, `#rrggbbaa`, or `transparent`.
pub fn parse_color(s: &str) -> Result<[u8; 4]> {
    let t = s.trim();
    if t.eq_ignore_ascii_case("transparent") || t.eq_ignore_ascii_case("none") {
        return Ok([0, 0, 0, 0]);
    }
    let h = t.trim_start_matches('#');
    let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16);
    let bad = || Error::usage(format!("'{s}' is not a colour such as #1f6feb or transparent"));
    match h.len() {
        6 => Ok([byte(0).map_err(|_| bad())?, byte(2).map_err(|_| bad())?, byte(4).map_err(|_| bad())?, 255]),
        8 => Ok([
            byte(0).map_err(|_| bad())?,
            byte(2).map_err(|_| bad())?,
            byte(4).map_err(|_| bad())?,
            byte(6).map_err(|_| bad())?,
        ]),
        _ => Err(bad()),
    }
}

fn hex(c: [u8; 4]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

/// Encode RGBA8 pixels as PNG.
pub fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, width, height);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc
            .write_header()
            .map_err(|e| Error::processing(format!("PNG encoding failed: {e}")))?;
        w.write_image_data(rgba)
            .map_err(|e| Error::processing(format!("PNG encoding failed: {e}")))?;
    }
    Ok(out)
}

fn dim(a: &Args, name: &str, max: usize) -> Result<usize> {
    let v = a.usize(name)?;
    if v == 0 || v > max {
        return Err(Error::usage(format!("--{name} must be between 1 and {max}")));
    }
    Ok(v)
}

// ---------------------------------------------------------------------------------------
// waveform
// ---------------------------------------------------------------------------------------

pub fn waveform_spec() -> OpSpec {
    OpSpec::custom("waveform", Category::Inspect, "Export a waveform image (PNG, SVG) or JSON peaks", waveform)
        .inputs(Inputs::ONE_AUDIO)
        .about(
            "Draws the waveform as a PNG or SVG image, or writes min/max peaks per time bucket \
             and channel as JSON for web players and agents. The format follows the output \
             extension (.png by default, .svg, .json). Images show the peak envelope with the \
             RMS level drawn darker inside it.",
        )
        .param(param("width", ParamKind::Int).default("1200").help("Image width in pixels"))
        .param(param("height", ParamKind::Int).default("200").help("Image height in pixels"))
        .param(param("peaks-per-second", ParamKind::Float).default("50").help("JSON only: min/max buckets per second of audio"))
        .param(param("split", ParamKind::Flag).help("Draw each channel in its own lane instead of one mixed waveform"))
        .param(param("color", ParamKind::Str).default("#1f6feb").help("Waveform colour, #rrggbb"))
        .param(param("background", ParamKind::Str).default("transparent").help("Background colour, #rrggbb or transparent"))
        .example("ferraus waveform song.wav -o song.png", "1200x200 PNG thumbnail")
        .example("ferraus waveform episode.mp3 -o peaks.json --peaks-per-second 20", "Peaks for a web player, 20 buckets per second")
        .example("ferraus waveform stereo.wav --split --width 800 --height 300 -o wave.svg", "SVG with one lane per channel")
}

/// (min, max, rms) of `x[s..e]`.
fn bucket(x: &[f32], s: usize, e: usize) -> (f32, f32, f32) {
    let seg = &x[s.min(x.len())..e.min(x.len())];
    if seg.is_empty() {
        return (0.0, 0.0, 0.0);
    }
    let (mut lo, mut hi, mut sq) = (f32::MAX, f32::MIN, 0f64);
    for v in seg {
        lo = lo.min(*v);
        hi = hi.max(*v);
        sq += (*v as f64) * (*v as f64);
    }
    (lo, hi, (sq / seg.len() as f64).sqrt() as f32)
}

/// Per-column (min, max, rms) for `width` columns.
fn columns(x: &[f32], width: usize) -> Vec<(f32, f32, f32)> {
    let n = x.len();
    (0..width)
        .map(|c| {
            let s = c * n / width;
            let e = ((c + 1) * n / width).max(s + 1);
            bucket(x, s, e)
        })
        .collect()
}

fn lanes(buf: &AudioBuffer, split: bool) -> Vec<Vec<f32>> {
    if split {
        buf.data.clone()
    } else {
        vec![buf.mono_mix()]
    }
}

fn waveform(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let buf = ctx.read_audio(0)?;
    let (path, fmt) = image_output(ctx, &["png", "svg", "json"], "png")?;
    let sr = buf.sample_rate;
    let bytes = match fmt.as_str() {
        "json" => {
            let pps = a.f64("peaks-per-second")?;
            if !(pps > 0.0 && pps <= sr as f64) {
                return Err(Error::usage(format!("--peaks-per-second must be between 0 and {sr}")));
            }
            let bf = sr as f64 / pps;
            let buckets = ((buf.frames() as f64 / bf).ceil() as usize).max(usize::from(buf.frames() > 0));
            let r4 = |v: f32| (v as f64 * 1e4).round() / 1e4;
            let channels: Vec<Value> = buf
                .data
                .iter()
                .enumerate()
                .map(|(ci, c)| {
                    let (mut mins, mut maxs) = (Vec::with_capacity(buckets), Vec::with_capacity(buckets));
                    for b in 0..buckets {
                        let s = (b as f64 * bf).round() as usize;
                        let e = (((b + 1) as f64 * bf).round() as usize).max(s + 1);
                        let (lo, hi, _) = bucket(c, s, e);
                        mins.push(r4(lo));
                        maxs.push(r4(hi));
                    }
                    json!({"channel": ci + 1, "min": mins, "max": maxs})
                })
                .collect();
            ctx.metric("buckets", buckets);
            serde_json::to_vec(&json!({
                "sample_rate": sr,
                "channels": buf.channels(),
                "duration_s": util::round_s(buf.duration_s()),
                "peaks_per_second": pps,
                "frames_per_bucket": bf,
                "buckets": buckets,
                "peaks": channels,
            }))?
        }
        _ => {
            let w = dim(a, "width", 16384)?;
            let h = dim(a, "height", 16384)?;
            let color = parse_color(&a.str_or("color", "#1f6feb"))?;
            let bg = parse_color(&a.str_or("background", "transparent"))?;
            let ls = lanes(&buf, a.flag("split"));
            let lane_h = (h / ls.len()).max(1);
            let cols: Vec<Vec<(f32, f32, f32)>> = ls.iter().map(|x| columns(x, w)).collect();
            if fmt == "svg" {
                svg_waveform(w, h, lane_h, &cols, color, bg).into_bytes()
            } else {
                let dark = [
                    (color[0] as f32 * 0.6) as u8,
                    (color[1] as f32 * 0.6) as u8,
                    (color[2] as f32 * 0.6) as u8,
                    color[3],
                ];
                let mut px = vec![0u8; w * h * 4];
                for p in px.chunks_mut(4) {
                    p.copy_from_slice(&bg);
                }
                for (li, lane) in cols.iter().enumerate() {
                    let top = li * lane_h;
                    let y_of = |v: f32| -> usize {
                        let t = ((1.0 - v.clamp(-1.0, 1.0)) * 0.5) * (lane_h - 1) as f32;
                        top + t.round() as usize
                    };
                    for (x, &(lo, hi, rms)) in lane.iter().enumerate() {
                        let mut paint = |y0: usize, y1: usize, c: [u8; 4]| {
                            for y in y0.min(y1)..=y1.max(y0) {
                                if y < h {
                                    let i = (y * w + x) * 4;
                                    px[i..i + 4].copy_from_slice(&c);
                                }
                            }
                        };
                        paint(y_of(hi), y_of(lo), color);
                        let r = rms.min(hi.abs().max(lo.abs()));
                        paint(y_of(r), y_of(-r), dark);
                    }
                }
                encode_png(w as u32, h as u32, &px)?
            }
        }
    };
    ctx.write_file(&path, &bytes, None)?;
    Ok(json!({"path": path.display().to_string(), "format": fmt, "duration_s": util::round_s(buf.duration_s())}))
}

fn svg_waveform(
    w: usize,
    h: usize,
    lane_h: usize,
    cols: &[Vec<(f32, f32, f32)>],
    color: [u8; 4],
    bg: [u8; 4],
) -> String {
    let mut s = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}\" height=\"{h}\" viewBox=\"0 0 {w} {h}\">\n"
    );
    if bg[3] > 0 {
        s.push_str(&format!("<rect width=\"{w}\" height=\"{h}\" fill=\"{}\"/>\n", hex(bg)));
    }
    let opacity = color[3] as f32 / 255.0;
    for (li, lane) in cols.iter().enumerate() {
        let top = (li * lane_h) as f32;
        let half = lane_h as f32 / 2.0;
        let y = |v: f32| top + half - v.clamp(-1.0, 1.0) * half;
        let mut d = String::new();
        for (x, &(_, hi, _)) in lane.iter().enumerate() {
            d.push_str(&format!("{}{} {:.2} ", if x == 0 { "M" } else { "L" }, x as f32 + 0.5, y(hi)));
        }
        for (x, &(lo, _, _)) in lane.iter().enumerate().rev() {
            d.push_str(&format!("L{} {:.2} ", x as f32 + 0.5, y(lo)));
        }
        d.push('Z');
        s.push_str(&format!(
            "<path d=\"{d}\" fill=\"{}\" fill-opacity=\"{opacity:.3}\" stroke=\"{}\" stroke-width=\"1\"/>\n",
            hex(color),
            hex(color)
        ));
    }
    s.push_str("</svg>\n");
    s
}

// ---------------------------------------------------------------------------------------
// spectrogram
// ---------------------------------------------------------------------------------------

pub fn spectrogram_spec() -> OpSpec {
    OpSpec::custom("spectrogram", Category::Inspect, "Export a spectrogram as a PNG image or JSON matrix", spectrogram)
        .inputs(Inputs::ONE_AUDIO)
        .about(
            "Computes a short-time Fourier transform of the mono mix (Hann window) and writes it \
             as a PNG (time left to right, low frequencies at the bottom) or as JSON (dB values \
             per frame and bin, 0 dB = loudest point, floored at -db-range). --mel 128 groups \
             the bins into 128 mel bands, which matches hearing and is what ML models expect; \
             --mel 0 keeps linear FFT bins.",
        )
        .param(param("fft", ParamKind::Int).default("2048").help("FFT size in samples (frequency resolution)"))
        .param(param("hop", ParamKind::Int).default("512").help("Hop between frames in samples (time resolution)"))
        .param(param("mel", ParamKind::Int).default("128").help("Number of mel bands; 0 for linear frequency bins"))
        .param(param("db-range", ParamKind::Gain).default("80dB").help("Dynamic range shown, in dB below the loudest point"))
        .param(param("fmin", ParamKind::Freq).default("0Hz").advanced().help("Lowest mel band edge (Hz)"))
        .param(param("fmax", ParamKind::Freq).advanced().help("Highest mel band edge (Hz); defaults to half the sample rate"))
        .param(param("width", ParamKind::Int).default("0").help("PNG width in pixels; 0 = one column per frame"))
        .param(param("height", ParamKind::Int).default("0").help("PNG height in pixels; 0 = one row per band"))
        .example("ferraus spectrogram song.wav -o song_spec.png", "128-band mel spectrogram image")
        .example(
            "ferraus spectrogram voice.wav --mel 0 --fft 4096 --hop 256 -o voice_spec.json",
            "Linear-frequency matrix in JSON for analysis",
        )
        .example("ferraus spectrogram drums.wav --width 1200 --height 300 --db-range 60 -o drums.png", "Fixed-size image with 60 dB range")
}

fn hz_to_mel(f: f64) -> f64 {
    2595.0 * (1.0 + f / 700.0).log10()
}

fn mel_to_hz(m: f64) -> f64 {
    700.0 * (10f64.powf(m / 2595.0) - 1.0)
}

/// Triangular mel filters as sparse (bin, weight) lists, and their centre frequencies.
pub fn mel_filterbank(
    n_mels: usize,
    fft: usize,
    sr: u32,
    fmin: f64,
    fmax: f64,
) -> (Vec<Vec<(usize, f32)>>, Vec<f64>) {
    let bins = fft / 2 + 1;
    let (m0, m1) = (hz_to_mel(fmin), hz_to_mel(fmax));
    let pts: Vec<f64> = (0..n_mels + 2)
        .map(|i| mel_to_hz(m0 + (m1 - m0) * i as f64 / (n_mels + 1) as f64))
        .collect();
    let bin_hz = sr as f64 / fft as f64;
    let mut bank = Vec::with_capacity(n_mels);
    for m in 0..n_mels {
        let (l, c, r) = (pts[m], pts[m + 1], pts[m + 2]);
        let mut row = Vec::new();
        for k in 0..bins {
            let f = k as f64 * bin_hz;
            let w = if f > l && f <= c {
                (f - l) / (c - l)
            } else if f > c && f < r {
                (r - f) / (r - c)
            } else {
                0.0
            };
            if w > 0.0 {
                row.push((k, w as f32));
            }
        }
        if row.is_empty() {
            // band narrower than one bin: take the nearest bin
            row.push((((c / bin_hz).round() as usize).min(bins - 1), 1.0));
        }
        bank.push(row);
    }
    (bank, pts[1..=n_mels].to_vec())
}

/// Power spectrogram of a mono signal: frames × bins, centred frames (zero padded).
pub fn stft_power(x: &[f32], fft: usize, hop: usize) -> Vec<Vec<f32>> {
    let n = x.len();
    let frames = 1 + n / hop;
    let mut planner = RealFftPlanner::<f32>::new();
    let plan = planner.plan_fft_forward(fft);
    let window: Vec<f32> = (0..fft)
        .map(|i| (0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / fft as f64).cos()) as f32)
        .collect();
    let mut input = plan.make_input_vec();
    let mut spec = plan.make_output_vec();
    let mut out = Vec::with_capacity(frames);
    for j in 0..frames {
        let start = (j * hop) as isize - (fft / 2) as isize;
        for (i, v) in input.iter_mut().enumerate() {
            let idx = start + i as isize;
            *v = if idx >= 0 && (idx as usize) < n { x[idx as usize] * window[i] } else { 0.0 };
        }
        plan.process(&mut input, &mut spec).expect("fft sizes match");
        out.push(spec.iter().map(|c| c.norm_sqr()).collect());
    }
    out
}

fn magma(t: f32) -> [u8; 4] {
    const STOPS: [[f32; 3]; 6] = [
        [0.0, 0.0, 4.0],
        [59.0, 15.0, 112.0],
        [140.0, 41.0, 129.0],
        [222.0, 73.0, 104.0],
        [254.0, 159.0, 109.0],
        [252.0, 253.0, 191.0],
    ];
    let t = t.clamp(0.0, 1.0) * (STOPS.len() - 1) as f32;
    let i = (t.floor() as usize).min(STOPS.len() - 2);
    let f = t - i as f32;
    let c = |k: usize| (STOPS[i][k] + (STOPS[i + 1][k] - STOPS[i][k]) * f).round() as u8;
    [c(0), c(1), c(2), 255]
}

fn spectrogram(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let buf = ctx.read_audio(0)?;
    let (path, fmt) = image_output(ctx, &["png", "json"], "png")?;
    let sr = buf.sample_rate;
    let fft = a.usize("fft")?;
    if !(16..=65536).contains(&fft) {
        return Err(Error::usage("--fft must be between 16 and 65536"));
    }
    let hop = a.usize("hop")?;
    if hop == 0 {
        return Err(Error::usage("--hop must be at least 1"));
    }
    let n_mels = a.usize("mel")?;
    let range = a.db("db-range")?.abs().max(1.0);
    let x = buf.mono_mix();
    let power = stft_power(&x, fft, hop);
    let fmax = a.opt_freq("fmax")?.unwrap_or(sr as f64 / 2.0).min(sr as f64 / 2.0);
    let fmin = a.freq("fmin")?.min(fmax);
    let (rows, freqs): (Vec<Vec<f32>>, Vec<f64>) = if n_mels > 0 {
        if n_mels > fft / 2 {
            ctx.warn("mel_bands_exceed_bins", format!("--mel {n_mels} is more than the {} FFT bins; bands repeat", fft / 2));
        }
        let (bank, centres) = mel_filterbank(n_mels, fft, sr, fmin, fmax);
        let rows = power
            .iter()
            .map(|p| bank.iter().map(|f| f.iter().map(|&(k, w)| p[k] * w).sum()).collect())
            .collect();
        (rows, centres)
    } else {
        let freqs = (0..fft / 2 + 1).map(|k| k as f64 * sr as f64 / fft as f64).collect();
        (power, freqs)
    };
    // dB relative to the loudest cell
    let mut db: Vec<Vec<f32>> = rows
        .iter()
        .map(|r| r.iter().map(|p| (10.0 * (*p as f64 + 1e-20).log10()) as f32).collect())
        .collect();
    let max = db.iter().flatten().fold(f32::MIN, |m, v| m.max(*v));
    for r in &mut db {
        for v in r.iter_mut() {
            *v = (*v - max).max(-range as f32);
        }
    }
    let frames = db.len();
    let bins = freqs.len();
    ctx.metric("frames", frames);
    ctx.metric("bins", bins);
    let bytes = if fmt == "json" {
        let r1 = |v: f32| (v as f64 * 10.0).round() / 10.0;
        serde_json::to_vec(&json!({
            "sample_rate": sr,
            "fft": fft,
            "hop": hop,
            "mel": n_mels,
            "db_range": range,
            "frames": frames,
            "bins": bins,
            "hop_s": hop as f64 / sr as f64,
            "times_s": (0..frames).map(|j| util::round_s(j as f64 * hop as f64 / sr as f64)).collect::<Vec<_>>(),
            "freqs_hz": freqs.iter().map(|f| (f * 10.0).round() / 10.0).collect::<Vec<_>>(),
            "data": db.iter().map(|r| r.iter().map(|v| r1(*v)).collect::<Vec<_>>()).collect::<Vec<_>>(),
        }))?
    } else {
        let w = match a.usize("width")? {
            0 => frames.min(16384),
            v => v.min(16384),
        };
        let h = match a.usize("height")? {
            0 => bins.min(16384),
            v => v.min(16384),
        };
        let mut px = vec![0u8; w * h * 4];
        for x in 0..w {
            let f0 = x * frames / w;
            let f1 = ((x + 1) * frames / w).max(f0 + 1).min(frames);
            for y in 0..h {
                let row = h - 1 - y; // low frequencies at the bottom
                let b0 = row * bins / h;
                let b1 = ((row + 1) * bins / h).max(b0 + 1).min(bins);
                let mut v = f32::MIN;
                for fr in &db[f0..f1] {
                    for val in &fr[b0..b1] {
                        v = v.max(*val);
                    }
                }
                let i = (y * w + x) * 4;
                px[i..i + 4].copy_from_slice(&magma((v + range as f32) / range as f32));
            }
        }
        encode_png(w as u32, h as u32, &px)?
    };
    ctx.write_file(&path, &bytes, None)?;
    Ok(json!({"path": path.display().to_string(), "format": fmt, "frames": frames, "bins": bins}))
}
