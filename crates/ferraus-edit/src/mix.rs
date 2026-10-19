//! `mix`: sum several inputs with per-input gain, pan, and time offset.

use crate::util;
use ferraus_core::op::Inputs;
use ferraus_core::prelude::*;
use ferraus_core::resample::{resample, Quality};
use std::f64::consts::FRAC_PI_2;

pub fn ops() -> Vec<OpSpec> {
    vec![mix_spec()]
}

pub fn mix_spec() -> OpSpec {
    OpSpec::custom("mix", Category::Edit, "Sum several files with per-input gain, pan, and offset", mix)
        .inputs(Inputs::MANY_AUDIO)
        .about(
            "Adds the inputs together. --gains, --pans, and --offsets are comma lists with one \
             value per input, in input order (missing values mean 0 dB, unpanned, and no \
             offset). Pans run from -100 (hard left) to +100 (hard right) on a constant-power \
             -3 dB law: a mono source panned to the centre sits 3 dB down on each side; a stereo \
             source is balanced (its far side is turned down). Inputs are resampled to the \
             first file's rate. The result lasts as long as the longest input including its \
             offset. The sum is not normalized unless --normalize peak; a warning is raised \
             if it clips.",
        )
        .param(param("gains", ParamKind::List).help("Gain per input in dB or as a factor, e.g. 0dB,-3dB,0.5x"))
        .param(param("pans", ParamKind::List).help("Pan per input from -100 (left) to +100 (right), e.g. 0,-40,+40"))
        .param(param("offsets", ParamKind::List).help("Start time per input (duration), e.g. 0s,1.5s,4b"))
        .param(
            param("normalize", ParamKind::Choice)
                .choices(&["off", "peak"])
                .default("off")
                .help("peak: scale the sum so its sample peak sits at --ceiling"),
        )
        .param(param("ceiling", ParamKind::Gain).default("-1dB").help("Peak level (dBFS) used by --normalize peak"))
        .example("ferraus mix vocal.wav beat.wav --gains 0dB,-4dB -o demo.wav", "Lay a vocal over a beat, beat 4 dB down")
        .example(
            "ferraus mix vox.wav gtr.wav keys.wav --gains 0dB,-3dB,-6dB --pans 0,-40,+40 -o rough.wav",
            "Rough mix: vocal centred, guitar left, keys right",
        )
        .example(
            "ferraus mix music.wav sting.wav --offsets 0s,12.5s --normalize peak -o promo.wav",
            "Drop a sting in at 12.5 s and peak-normalize the sum",
        )
}

/// Constant-power -3 dB pan law: (left, right) gains for a pan in `[-100, 100]`.
pub fn pan_gains(pan: f64) -> (f32, f32) {
    let theta = (pan.clamp(-100.0, 100.0) + 100.0) / 200.0 * FRAC_PI_2;
    (theta.cos() as f32, theta.sin() as f32)
}

fn parse_pan(s: &str) -> Result<f64> {
    let t = s.trim();
    let lower = t.to_ascii_lowercase();
    let v = match lower.as_str() {
        "left" | "l" => -100.0,
        "right" | "r" => 100.0,
        "center" | "centre" | "c" | "0" => 0.0,
        _ => {
            if let Some(n) = lower.strip_prefix('l') {
                -n.parse::<f64>().map_err(|_| bad_pan(t))?
            } else if let Some(n) = lower.strip_prefix('r') {
                n.parse::<f64>().map_err(|_| bad_pan(t))?
            } else {
                lower.trim_start_matches('+').parse::<f64>().map_err(|_| bad_pan(t))?
            }
        }
    };
    if !(-100.0..=100.0).contains(&v) {
        return Err(bad_pan(t));
    }
    Ok(v)
}

fn bad_pan(s: &str) -> Error {
    Error::usage(format!("--pans: '{s}' is not a pan from -100 (left) to +100 (right)"))
}

fn per_input<T>(
    a: &Args,
    name: &str,
    count: usize,
    parse: &dyn Fn(&str) -> Result<T>,
) -> Result<Vec<Option<T>>> {
    let vals = a.list(name);
    if vals.len() > count {
        return Err(Error::usage(format!(
            "--{name} has {} values but there are only {count} inputs",
            vals.len()
        )));
    }
    let mut out: Vec<Option<T>> = Vec::with_capacity(count);
    for v in &vals {
        out.push(Some(parse(v)?));
    }
    out.resize_with(count, || None);
    Ok(out)
}

/// One input to [`mix_buffers`].
pub struct Track {
    pub buf: AudioBuffer,
    pub gain: f32,
    pub pan: Option<f64>,
    pub offset: usize,
}

/// Sum tracks (all at the same sample rate) into `channels` output channels.
pub fn mix_buffers(tracks: &[Track], channels: usize, sample_rate: u32) -> AudioBuffer {
    let len = tracks.iter().map(|t| t.offset + t.buf.frames()).max().unwrap_or(0);
    let mut out = AudioBuffer::new(sample_rate, channels, len);
    for t in tracks {
        let src: AudioBuffer = match t.pan {
            Some(p) if channels >= 2 => {
                let (gl, gr) = pan_gains(p);
                if t.buf.channels() == 1 {
                    let x = &t.buf.data[0];
                    let mut d = vec![
                        x.iter().map(|v| v * gl).collect::<Vec<f32>>(),
                        x.iter().map(|v| v * gr).collect::<Vec<f32>>(),
                    ];
                    d.resize(channels, vec![0.0; x.len()]);
                    AudioBuffer::from_planar(sample_rate, d)
                } else {
                    // balance: centre leaves both sides at unity, moving away turns the far side down
                    let s2 = std::f32::consts::SQRT_2;
                    let (bl, br) = ((gl * s2).min(1.0), (gr * s2).min(1.0));
                    let mut b = t.buf.with_channels(channels);
                    for v in b.data[0].iter_mut() {
                        *v *= bl;
                    }
                    for v in b.data[1].iter_mut() {
                        *v *= br;
                    }
                    b
                }
            }
            _ => t.buf.with_channels(channels),
        };
        for (oc, sc) in out.data.iter_mut().zip(&src.data) {
            for (o, s) in oc[t.offset..t.offset + sc.len()].iter_mut().zip(sc) {
                *o += s * t.gain;
            }
        }
    }
    out
}

fn mix(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let count = ctx.inputs.len();
    if count == 0 {
        return Err(Error::usage("mix needs at least one input"));
    }
    let mut bufs = Vec::with_capacity(count);
    for i in 0..count {
        bufs.push(ctx.read_audio(i)?);
    }
    let sr = bufs[0].sample_rate;
    let bpm = ctx.bpm();
    let gains = per_input(a, "gains", count, &|s| {
        let g = units::Gain::parse(s).map_err(|e| Error::usage(format!("--gains: {}", e.message)))?;
        if matches!(g, units::Gain::Lufs(_) | units::Gain::DbTp(_)) {
            return Err(Error::usage(format!("--gains: '{s}' must be a gain such as -3dB or 0.5x")));
        }
        Ok(g.linear())
    })?;
    let pans = per_input(a, "pans", count, &parse_pan)?;
    let offsets = per_input(a, "offsets", count, &|s| util::parse_duration_frames(s, "--offsets", sr, bpm))?;
    let any_pan = pans.iter().any(|p| p.is_some());
    let max_ch = bufs.iter().map(|b| b.channels()).max().unwrap_or(1);
    let channels = if any_pan { max_ch.max(2) } else { max_ch };
    let mut tracks = Vec::with_capacity(count);
    let mut described = Vec::with_capacity(count);
    for (i, b) in bufs.into_iter().enumerate() {
        let b = if b.sample_rate != sr {
            ctx.note(&format!("resampled_input_{}", i + 1), json!({"from": b.sample_rate, "to": sr}));
            resample(&b, sr, Quality::High)
        } else {
            b
        };
        let gain = gains[i].unwrap_or(1.0);
        let offset = offsets[i].unwrap_or(0);
        described.push(json!({
            "path": ctx.inputs[i].display().to_string(),
            "gain_db": util::round_db(lin_to_db(gain)),
            "pan": pans[i],
            "offset_s": util::round_s(offset as f64 / sr as f64),
        }));
        tracks.push(Track { buf: b, gain: gain as f32, pan: pans[i], offset });
    }
    if any_pan {
        ctx.note("pan_law", "-3dB constant power");
    }
    let mut out = mix_buffers(&tracks, channels, sr);
    let peak = out.peak();
    let mut peak_db = lin_to_db(peak as f64);
    match a.choice("normalize").as_str() {
        "peak" if peak > 0.0 => {
            let c = a.db("ceiling")?;
            out.scale((db_to_lin(c) / peak as f64) as f32);
            ctx.metric("normalize_gain_db", util::round_db(c - peak_db));
            peak_db = c;
        }
        _ => {
            if peak > 1.0 {
                ctx.warn(
                    "clipping",
                    format!("the sum peaks at {peak_db:+.2} dBFS and will clip; lower --gains or use --normalize peak"),
                );
            }
        }
    }
    ctx.metric("peak_db", util::round_db(peak_db));
    ctx.metric("duration_s", util::round_s(out.duration_s()));
    let ext = util::audio_ext(ctx);
    let path = ctx.output_path(None, &ext)?;
    let out = util::conform(out, ctx);
    ctx.write_audio(&path, &out, None)?;
    Ok(json!({
        "duration_s": util::round_s(out.duration_s()),
        "channels": out.channels(),
        "sample_rate": out.sample_rate,
        "inputs": described,
    }))
}
