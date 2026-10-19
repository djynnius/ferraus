//! Utilities with their own outputs: `generate tone|noise|silence` and `band-split`.

use crate::dsp::biquad::butterworth;
use ferraus_core::prelude::*;
use std::f64::consts::PI;
use std::path::PathBuf;

pub fn ops() -> Vec<OpSpec> {
    vec![
        OpSpec::custom("generate tone", Category::Edit, "Generate a test tone or sweep", gen_tone)
            .inputs(Inputs::NONE)
            .about(
                "Writes a sine, square, saw, or triangle tone (band-limited with polyBLEP, so \
                 square, saw, and triangle have no aliasing to speak of), optionally sweeping \
                 between two frequencies. The phase is accumulated in double precision, so the \
                 frequency is exact. The sample rate and channel count come from the global \
                 --sample-rate (default 48000) and --channels (default 1). Without -o the file \
                 is tone.wav in the current directory.",
            )
            .param(param("freq", ParamKind::Freq).default("440Hz").help("Frequency (Hz, kHz, or a note such as A4)"))
            .param(param("dur", ParamKind::Time).default("5s").help("Duration"))
            .param(
                param("shape", ParamKind::Choice)
                    .choices(&["sine", "square", "saw", "triangle"])
                    .default("sine")
                    .help("Waveform"),
            )
            .param(
                param("sweep", ParamKind::Str)
                    .help("Sweep from low to high over the duration, e.g. 20Hz..20kHz (overrides --freq)"),
            )
            .param(
                param("sweep-curve", ParamKind::Choice)
                    .choices(&["log", "linear"])
                    .default("log")
                    .help("Sweep spacing: logarithmic (equal time per octave) or linear"),
            )
            .param(param("amplitude", ParamKind::Gain).default("-3dB").help("Peak level in dBFS"))
            .example("ferraus generate tone -o a440.wav", "Five seconds of A440 at -3 dBFS")
            .example(
                "ferraus generate tone --sweep 20Hz..20kHz --dur 10s --sample-rate 96000 -o sweep.wav",
                "A ten-second logarithmic sweep at 96 kHz",
            )
            .example("ferraus generate tone --shape saw --freq C3 --dur 2s -o saw.wav", "A band-limited saw at C3"),
        OpSpec::custom("generate noise", Category::Edit, "Generate white, pink, or brown noise", gen_noise)
            .inputs(Inputs::NONE)
            .about(
                "Writes Gaussian noise: white (flat), pink (-3 dB per octave, Kellet filter), or \
                 brown (-6 dB per octave, leaky integrator), scaled to an exact RMS level. Each \
                 channel gets independent noise. --seed makes it reproducible. Without -o the \
                 file is noise.wav in the current directory.",
            )
            .param(
                param("color", ParamKind::Choice)
                    .choices(&["white", "pink", "brown"])
                    .default("white")
                    .help("Spectral colour"),
            )
            .param(param("dur", ParamKind::Time).default("5s").help("Duration"))
            .param(param("amplitude", ParamKind::Gain).default("-18dB").help("RMS level in dBFS"))
            .example("ferraus generate noise --color pink -o pink.wav", "Five seconds of pink noise at -18 dBFS RMS")
            .example(
                "ferraus generate noise --color brown --dur 30s --channels 2 --seed 7 -o rumble.wav",
                "Reproducible stereo brown noise",
            ),
        OpSpec::custom("generate silence", Category::Edit, "Generate digital silence", gen_silence)
            .inputs(Inputs::NONE)
            .about("Writes silence of the given duration (useful as padding or a fixture). Without -o the file is silence.wav in the current directory.")
            .param(param("dur", ParamKind::Time).default("2s").help("Duration"))
            .example("ferraus generate silence -o gap.wav", "Two seconds of silence")
            .example("ferraus generate silence --dur 500ms --channels 2 -o gap.wav", "Half a second of stereo silence"),
        OpSpec::custom(
            "band-split",
            Category::Tone,
            "Split audio into frequency bands that sum back to the input",
            band_split,
        )
        .inputs(Inputs::ONE_AUDIO)
        .about(
            "Writes one file per band (roles band1..bandN, lowest first). Each crossover is a \
             zero-phase Linkwitz-Riley (4th-order magnitude) split made by forward-backward \
             Butterworth filtering, and each higher band is what remains after removing the \
             lower ones, so the bands add back to the input exactly and have no phase shift.",
        )
        .param(
            param("crossovers", ParamKind::List)
                .default("120Hz,2kHz,8kHz")
                .help("Crossover frequencies, ascending (e.g. 120Hz,2kHz,8kHz gives four bands)"),
        )
        .example("ferraus band-split mix.wav", "Four bands: mix.band1.wav ... mix.band4.wav")
        .example(
            "ferraus band-split drums.wav --crossovers 200Hz -o drums.wav",
            "Two bands: drums.band1.wav (lows) and drums.band2.wav (highs)",
        ),
    ]
}

fn duration_frames(a: &Args, sr: u32) -> Result<usize> {
    let secs = a.seconds("dur")?;
    if !(secs > 0.0) || secs > 24.0 * 3600.0 {
        return Err(Error::usage("--dur must be between 0 and 24 hours"));
    }
    Ok((secs * sr as f64).round() as usize)
}

/// Output path for a generator: `-o` (templates allowed) or `<outdir|.>/<default>.<ext>`.
fn gen_output(ctx: &OpCtx, default: &str) -> Result<PathBuf> {
    let ext = ctx.globals.format.clone().unwrap_or_else(|| "wav".into());
    if ctx.globals.output.is_some() {
        return ctx.output_path(None, &ext);
    }
    let file = format!("{default}.{ext}");
    Ok(match &ctx.globals.outdir {
        Some(d) => d.join(file),
        None => PathBuf::from(file),
    })
}

fn check_memory(ctx: &OpCtx, frames: usize, channels: usize) -> Result<()> {
    let bytes = frames as u64 * channels as u64 * 4;
    if bytes > ctx.globals.max_memory {
        return Err(Error::usage(format!(
            "the generated audio would need {} MB, above --max-memory",
            bytes / (1024 * 1024)
        )));
    }
    Ok(())
}

#[inline]
fn poly_blep(t: f64, dt: f64) -> f64 {
    if dt <= 0.0 {
        0.0
    } else if t < dt {
        let x = t / dt;
        x + x - x * x - 1.0
    } else if t > 1.0 - dt {
        let x = (t - 1.0) / dt;
        x * x + x + x + 1.0
    } else {
        0.0
    }
}

#[inline]
fn poly_blamp(t: f64, dt: f64) -> f64 {
    if dt <= 0.0 {
        0.0
    } else if t < dt {
        let x = t / dt - 1.0;
        -x * x * x / 3.0
    } else if t > 1.0 - dt {
        let x = (t - 1.0) / dt + 1.0;
        x * x * x / 3.0
    } else {
        0.0
    }
}

/// One band-limited oscillator sample at phase `t` (cycles, 0..1) and increment `dt`.
pub(crate) fn osc(shape: &str, t: f64, dt: f64) -> f64 {
    match shape {
        "square" => {
            let mut v = if t < 0.5 { 1.0 } else { -1.0 };
            v += poly_blep(t, dt);
            v -= poly_blep((t + 0.5) % 1.0, dt);
            v
        }
        "saw" => 2.0 * t - 1.0 - poly_blep(t, dt),
        "triangle" => {
            // peak at t = 0.25, trough at t = 0.75 (same phase as the sine)
            let u = (t + 0.25) % 1.0;
            let mut v = 1.0 - 4.0 * (u - 0.5).abs();
            // the slope jumps by +8 per cycle at the trough (u = 0) and by -8 at the peak
            // (u = 0.5); polyBLAMP is scaled for a jump of 2 per sample
            v += 4.0 * dt * poly_blamp(u, dt);
            v -= 4.0 * dt * poly_blamp((u + 0.5) % 1.0, dt);
            v
        }
        _ => (2.0 * PI * t).sin(),
    }
}

fn gen_tone(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let sr = ctx.globals.sample_rate.unwrap_or(48000);
    let ch = ctx.globals.channels.unwrap_or(1).clamp(1, 32);
    let n = duration_frames(a, sr)?;
    check_memory(ctx, n, ch)?;
    let shape = a.choice("shape");
    let amp = a.gain("amplitude")?.linear();
    let nyq = sr as f64 / 2.0;
    let (f0, f1) = match a.str("sweep") {
        Some(s) => units::parse_num_range(&s, units::parse_freq)
            .map_err(|e| Error::usage(format!("--sweep: {}", e.message)))?,
        None => {
            let f = a.freq("freq")?;
            (f, f)
        }
    };
    if f0 <= 0.0 || f1 <= 0.0 || f0 >= nyq || f1 >= nyq {
        return Err(Error::usage(format!(
            "frequencies must be between 0 and the Nyquist frequency ({nyq} Hz) at {sr} Hz"
        ))
        .with_hint("raise --sample-rate or lower the frequency"));
    }
    let log = a.choice("sweep-curve") != "linear";
    let total = n as f64 / sr as f64;
    let mut phase = 0.0f64;
    let mut v = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f64 / sr as f64;
        let f = if f0 == f1 {
            f0
        } else if log {
            f0 * (f1 / f0).powf(t / total)
        } else {
            f0 + (f1 - f0) * t / total
        };
        let dt = f / sr as f64;
        v.push((amp * osc(&shape, phase, dt)) as f32);
        phase += dt;
        if phase >= 1.0 {
            phase -= phase.floor();
        }
    }
    let buf = AudioBuffer::from_planar(sr, vec![v; ch]);
    let path = gen_output(ctx, "tone")?;
    ctx.write_audio(&path, &buf, None)?;
    Ok(json!({
        "path": path.display().to_string(),
        "shape": shape,
        "freq_hz": if f0 == f1 { json!(f0) } else { json!([f0, f1]) },
        "duration_s": buf.duration_s(),
        "sample_rate": sr,
        "channels": ch,
        "peak_dbfs": lin_to_db(amp),
    }))
}

struct Gauss(u64);
impl Gauss {
    fn uniform(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        ((self.0 >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    }
    fn next(&mut self) -> f64 {
        let u1 = self.uniform();
        let u2 = self.uniform();
        (-2.0 * u1.ln()).sqrt() * (2.0 * PI * u2).cos()
    }
}

fn noise_channel(color: &str, n: usize, seed: u64) -> Vec<f64> {
    let mut g = Gauss(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(0xD1B5_4A32_D192_ED03) | 1);
    let mut b = [0.0f64; 7];
    let mut brown = 0.0f64;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let w = g.next();
        let v = match color {
            "pink" => {
                b[0] = 0.99886 * b[0] + w * 0.0555179;
                b[1] = 0.99332 * b[1] + w * 0.0750759;
                b[2] = 0.96900 * b[2] + w * 0.1538520;
                b[3] = 0.86650 * b[3] + w * 0.3104856;
                b[4] = 0.55000 * b[4] + w * 0.5329522;
                b[5] = -0.7616 * b[5] - w * 0.0168980;
                let p = b.iter().sum::<f64>() + w * 0.5362;
                b[6] = w * 0.115926;
                p
            }
            "brown" => {
                brown = 0.9995 * brown + w * 0.05;
                brown
            }
            _ => w,
        };
        out.push(v);
    }
    if color == "brown" {
        // remove the slow DC wander so the RMS reflects the audible part
        let hp = butterworth(1, 5.0, 48000.0, true);
        let mut f = hp.filter();
        for v in out.iter_mut() {
            *v = f.tick(*v);
        }
    }
    out
}

fn gen_noise(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let sr = ctx.globals.sample_rate.unwrap_or(48000);
    let ch = ctx.globals.channels.unwrap_or(1).clamp(1, 32);
    let n = duration_frames(a, sr)?;
    check_memory(ctx, n, ch)?;
    let color = a.choice("color");
    let rms_target = a.gain("amplitude")?.linear();
    let data: Vec<Vec<f32>> = (0..ch)
        .map(|c| {
            let v = noise_channel(&color, n, ctx.seed().wrapping_add(c as u64 * 7919 + 1));
            let rms = (v.iter().map(|x| x * x).sum::<f64>() / n.max(1) as f64).sqrt().max(1e-20);
            let g = rms_target / rms;
            v.into_iter().map(|x| (x * g) as f32).collect()
        })
        .collect();
    let buf = AudioBuffer::from_planar(sr, data);
    let path = gen_output(ctx, "noise")?;
    if buf.peak() > 1.0 {
        ctx.warn("noise_peaks_clip", "noise peaks exceed full scale; lower --amplitude to avoid clipping in integer formats");
    }
    ctx.write_audio(&path, &buf, None)?;
    Ok(json!({
        "path": path.display().to_string(),
        "color": color,
        "duration_s": buf.duration_s(),
        "sample_rate": sr,
        "channels": ch,
        "rms_dbfs": lin_to_db(rms_target),
        "seed": ctx.seed(),
    }))
}

fn gen_silence(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let sr = ctx.globals.sample_rate.unwrap_or(48000);
    let ch = ctx.globals.channels.unwrap_or(1).clamp(1, 32);
    let n = duration_frames(a, sr)?;
    check_memory(ctx, n, ch)?;
    let buf = AudioBuffer::new(sr, ch, n);
    let path = gen_output(ctx, "silence")?;
    ctx.write_audio(&path, &buf, None)?;
    Ok(json!({
        "path": path.display().to_string(),
        "duration_s": buf.duration_s(),
        "sample_rate": sr,
        "channels": ch,
    }))
}

/// Split into zero-phase bands that sum exactly to the input.
pub(crate) fn split_bands(buf: &AudioBuffer, crossovers: &[f64]) -> Vec<AudioBuffer> {
    let sr = buf.sample_rate as f64;
    let mut bands: Vec<Vec<Vec<f32>>> = vec![Vec::new(); crossovers.len() + 1];
    for ch in &buf.data {
        let mut rest: Vec<f64> = ch.iter().map(|v| *v as f64).collect();
        for (bi, fc) in crossovers.iter().enumerate() {
            let lp = butterworth(2, *fc, sr, false);
            let pad = ((4.0 * sr / fc) as usize).min(ch.len().saturating_sub(1));
            let rest32: Vec<f32> = rest.iter().map(|v| *v as f32).collect();
            let low = lp.filtfilt(&rest32, pad);
            // keep the exact remainder in f64 so the bands add back to the input
            let low32: Vec<f32> = low.iter().map(|v| *v as f32).collect();
            for (r, l) in rest.iter_mut().zip(&low32) {
                *r -= *l as f64;
            }
            bands[bi].push(low32);
        }
        bands[crossovers.len()].push(rest.into_iter().map(|v| v as f32).collect());
    }
    bands
        .into_iter()
        .map(|d| AudioBuffer::from_planar(buf.sample_rate, d))
        .collect()
}

fn band_split(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let buf = ctx.read_audio(0)?;
    let nyq = buf.sample_rate as f64 / 2.0;
    let mut xs: Vec<f64> = a
        .list("crossovers")
        .iter()
        .map(|s| units::parse_freq(s))
        .collect::<Result<_>>()
        .map_err(|e| Error::usage(format!("--crossovers: {}", e.message)))?;
    if xs.is_empty() || xs.len() > 15 {
        return Err(Error::usage("--crossovers needs 1 to 15 frequencies"));
    }
    if xs.windows(2).any(|w| w[1] <= w[0]) {
        xs.sort_by(|p, q| p.partial_cmp(q).unwrap());
        if xs.windows(2).any(|w| w[1] <= w[0]) {
            return Err(Error::usage("--crossovers must be distinct frequencies"));
        }
    }
    if xs.iter().any(|f| *f <= 0.0 || *f >= nyq * 0.95) {
        return Err(Error::usage(format!("crossovers must be between 0 and {:.0} Hz", nyq * 0.95)));
    }
    let bands = split_bands(&buf, &xs);
    // reconstruction check
    let mut err = 0.0f64;
    let mut sig = 0.0f64;
    for c in 0..buf.channels() {
        for i in 0..buf.frames() {
            let s: f64 = bands.iter().map(|b| b.data[c][i] as f64).sum();
            err += (s - buf.data[c][i] as f64).powi(2);
            sig += (buf.data[c][i] as f64).powi(2);
        }
    }
    let ext = ctx.globals.format.clone().unwrap_or_else(|| {
        ctx.inputs
            .first()
            .and_then(|p| p.extension())
            .map(|e| e.to_string_lossy().to_lowercase())
            .filter(|e| ctx.io.can_encode(e))
            .unwrap_or_else(|| "wav".into())
    });
    let mut list = Vec::new();
    for (i, b) in bands.iter().enumerate() {
        let role = format!("band{}", i + 1);
        let path = ctx.output_path(Some(&role), &ext)?;
        ctx.write_audio(&path, b, Some(&role))?;
        let lo = if i == 0 { 0.0 } else { xs[i - 1] };
        let hi = if i < xs.len() { xs[i] } else { nyq };
        list.push(json!({"role": role, "path": path.display().to_string(), "low_hz": lo, "high_hz": hi}));
    }
    let err_db = if sig > 0.0 { 10.0 * (err.max(1e-30) / sig).log10() } else { -300.0 };
    ctx.metric("reconstruction_error_db", (err_db * 10.0).round() / 10.0);
    Ok(json!({ "bands": list }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{dominant_freq, music};
    use ferraus_core::testing::{self, tone_level};
    use ferraus_core::Globals;

    fn spec(name: &str) -> OpSpec {
        ops().into_iter().find(|o| o.name == name).unwrap()
    }

    fn gen(name: &str, pairs: &[(&str, &str)], g: impl FnOnce(&mut Globals)) -> (AudioBuffer, Value, OpCtx<'static>) {
        let dir = testing::TempDir::new("gen");
        let path = dir.path("out.wav");
        let mut c = testing::ctx();
        c.globals.output = Some(path.display().to_string());
        g(&mut c.globals);
        let sp = spec(name);
        let f = match sp.runner {
            Runner::Custom(f) => f,
            _ => unreachable!(),
        };
        let v = f(&testing::args(&sp, pairs), &mut c).unwrap();
        let b = testing::read_wav(&path);
        (b, v, c)
    }

    use ferraus_core::Runner;

    #[test]
    fn tone_frequency_accurate_to_001_percent() {
        for f in [440.0, 1234.5, 97.0] {
            let fs = format!("{f}Hz");
            let (b, v, _) = gen("generate tone", &[("freq", &fs), ("dur", "2s")], |_| {});
            assert_eq!(b.frames(), 96000);
            assert_eq!(v["sample_rate"], json!(48000));
            let est = dominant_freq(&b.data[0], 48000, f * 0.99, f * 1.01);
            assert!(((est - f) / f).abs() < 1e-4, "{f}: {est}");
            assert!((b.peak() as f64 - db_to_lin(-3.0)).abs() < 1e-3);
        }
        // zero-crossing count over 1 s is exactly the frequency
        let (b, _, _) = gen("generate tone", &[("freq", "1000Hz"), ("dur", "1s")], |g| g.sample_rate = Some(44100));
        assert_eq!(b.sample_rate, 44100);
        let ups = b.data[0].windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        assert!((999..=1000).contains(&ups), "{ups}");
    }

    #[test]
    fn band_limited_shapes_match_additive_reference() {
        let sr = 48000;
        let f = 1000.0;
        for shape in ["square", "saw", "triangle"] {
            let (b, _, _) = gen("generate tone", &[("freq", "1kHz"), ("dur", "0.25s"), ("shape", shape), ("amplitude", "0dB")], |_| {});
            // Fourier-series harmonic amplitudes of the ideal waveforms
            for h in [1usize, 2, 3, 5, 7] {
                let kf = h as f64;
                let want = match shape {
                    "square" if h % 2 == 1 => 4.0 / (PI * kf),
                    "saw" => 2.0 / (PI * kf),
                    "triangle" if h % 2 == 1 => 8.0 / (PI * PI * kf * kf),
                    _ => 0.0,
                };
                let got = tone_level(&b.data[0], sr, f * kf);
                // two-point polyBLEP rolls the upper harmonics off slightly (-0.6 dB at 7 kHz)
                let tol = (0.01 + 0.015 * kf) * want.max(0.02);
                assert!((got - want).abs() < tol, "{shape} h{h}: {got} vs {want}");
            }
        }
        // aliasing: at 1234.5 Hz the 13th harmonic (16048.5 Hz) is real, the 23rd folds to
        // 19606.5 Hz and the 25th to 17137.5 Hz; compare with the naive waveform
        let f = 1234.5;
        for shape in ["square", "saw", "triangle"] {
            let (b, _, _) = gen("generate tone", &[("freq", "1234.5Hz"), ("dur", "0.5s"), ("shape", shape), ("amplitude", "0dB")], |_| {});
            let naive: Vec<f32> = (0..b.frames())
                .map(|i| {
                    let t = (i as f64 * f / sr as f64).fract();
                    (match shape {
                        "square" => if t < 0.5 { 1.0 } else { -1.0 },
                        "saw" => 2.0 * t - 1.0,
                        _ => 1.0 - 4.0 * (((t + 0.25) % 1.0) - 0.5).abs(),
                    }) as f32
                })
                .collect();
            for alias in [19606.5, 17137.5] {
                let got = tone_level(&b.data[0], sr, alias);
                let raw = tone_level(&naive, sr, alias);
                assert!(got < raw * 0.5, "{shape} alias at {alias}: {got} vs naive {raw}");
            }
        }
    }

    #[test]
    fn sweep_noise_silence_and_default_names() {
        let (b, _, _) = gen("generate tone", &[("sweep", "100Hz..1kHz"), ("dur", "1s")], |_| {});
        let early = dominant_freq(&b.data[0][0..4800], 48000, 80.0, 200.0);
        let late = dominant_freq(&b.data[0][43200..], 48000, 700.0, 1100.0);
        assert!(early < 130.0 && late > 850.0, "{early} {late}");
        let (b, v, _) = gen("generate noise", &[("color", "pink"), ("dur", "2s")], |g| g.channels = Some(2));
        assert_eq!(b.channels(), 2);
        assert!((testing::rms_db(&b.data[0]) + 18.0).abs() < 0.05);
        assert_ne!(b.data[0], b.data[1]);
        assert_eq!(v["color"], json!("pink"));
        // pink: equal energy per octave; white: +3 dB per octave
        let band = |x: &[f32], lo: f64| -> f64 {
            let hp = butterworth(4, lo, 48000.0, true);
            let lp = butterworth(4, lo * 2.0, 48000.0, false);
            testing::rms_db(&lp.apply(&hp.apply(x)))
        };
        let d = band(&b.data[0], 1600.0) - band(&b.data[0], 200.0);
        assert!(d.abs() < 1.5, "pink octave slope {d}");
        let (w, _, _) = gen("generate noise", &[("dur", "2s")], |_| {});
        let d = band(&w.data[0], 1600.0) - band(&w.data[0], 200.0);
        assert!((d - 9.0).abs() < 1.5, "white over 3 octaves {d}");
        let (br, _, _) = gen("generate noise", &[("color", "brown"), ("dur", "2s")], |_| {});
        let d = band(&br.data[0], 1600.0) - band(&br.data[0], 200.0);
        assert!((d + 9.0).abs() < 2.0, "brown over 3 octaves {d}");
        let (s, _, _) = gen("generate silence", &[], |_| {});
        assert_eq!(s.frames(), 96000);
        assert_eq!(s.peak(), 0.0);
        // default output name without -o
        let dir = testing::TempDir::new("gendef");
        let mut c = testing::ctx();
        c.globals.outdir = Some(dir.0.clone());
        let sp = spec("generate tone");
        if let Runner::Custom(f) = sp.runner {
            let v = f(&testing::args(&sp, &[("dur", "0.1s")]), &mut c).unwrap();
            assert!(v["path"].as_str().unwrap().ends_with("tone.wav"));
            assert!(dir.path("tone.wav").exists());
        }
        assert!(matches!(spec("generate tone").inputs.max, Some(0)));
    }

    #[test]
    fn band_split_sums_back_to_input() {
        let sr = 48000;
        let x = testing::add(&music(sr, 1.0, 0.5), &testing::noise(sr, 1.0, 0.1, 2));
        let x = testing::stereo(x);
        let dir = testing::TempDir::new("bands");
        let input = dir.path("mix.wav");
        testing::write_wav(&input, &x);
        let mut c = testing::ctx();
        c.inputs = vec![input.clone()];
        let sp = spec("band-split");
        let v = band_split(&testing::args(&sp, &[]), &mut c).unwrap();
        let bands = v["bands"].as_array().unwrap();
        assert_eq!(bands.len(), 4);
        let bufs: Vec<AudioBuffer> = (1..=4).map(|i| testing::read_wav(&dir.path(&format!("mix.band{i}.wav")))).collect();
        let mut sum = bufs[0].clone();
        for b in &bufs[1..] {
            sum = testing::add(&sum, b);
        }
        let resid: Vec<f32> = (0..x.frames()).map(|i| sum.data[1][i] - x.data[1][i]).collect();
        let r = testing::rms_db(&resid) - testing::rms_db(&x.data[1]);
        assert!(r < -60.0, "reconstruction residual {r:.1} dB");
        assert!(c.metrics["reconstruction_error_db"].as_f64().unwrap() < -60.0);
        // band shapes: a 500 Hz tone lands in band 2, 30 Hz in band 1
        let t = testing::add(&testing::sine(sr, 30.0, 1.0, 0.5), &testing::sine(sr, 500.0, 1.0, 0.5));
        let parts = split_bands(&t, &[120.0, 2000.0, 8000.0]);
        assert!(tone_level(&parts[0].data[0][12000..36000], sr, 30.0) > 0.49);
        assert!(tone_level(&parts[1].data[0][12000..36000], sr, 500.0) > 0.49);
        assert!(tone_level(&parts[2].data[0][12000..36000], sr, 500.0) < 0.01);
        // -6 dB at the crossover (Linkwitz-Riley)
        let xo = split_bands(&testing::sine(sr, 2000.0, 1.0, 0.5), &[2000.0]);
        assert!((tone_level(&xo[0].data[0][12000..36000], sr, 2000.0) - 0.25).abs() < 0.01);
        let mut bad = testing::ctx();
        bad.inputs = vec![input];
        assert!(band_split(&testing::args(&sp, &[("crossovers", "30kHz")]), &mut bad).is_err());
    }
}
