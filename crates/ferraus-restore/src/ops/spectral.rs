//! STFT-domain restoration: `dehiss`, `repair`, and `remove-sound`.

use crate::dsp::fft::{Stft, C};
use crate::dsp::util::{percentile, smoothstep};
use ferraus_core::prelude::*;
use std::f64::consts::PI;

pub fn ops() -> Vec<OpSpec> {
    vec![
        OpSpec::transform("dehiss", Category::Restoration, "Reduce broadband hiss", dehiss)
            .about(
                "Estimates a per-frequency noise floor from the quietest frames (or uses the \
                 level you give), then applies a soft spectral gate restricted to --range: \
                 bins near the floor are reduced by --reduction, bins well above it pass, with \
                 a --transition-wide soft knee in between. Gains open quickly and close slowly \
                 to avoid 'musical noise'. A --range ending at 20 kHz or above extends to Nyquist.",
            )
            .param(
                param("floor", ParamKind::Str)
                    .default("auto")
                    .help("Hiss level as dBFS RMS (-60dB) or 'auto' to measure it from the quietest frames"),
            )
            .param(
                param("reduction", ParamKind::Gain).default("12dB").help("How far hiss-only bins are lowered, in dB"),
            )
            .param(
                param("range", ParamKind::Str)
                    .default("2kHz..20kHz")
                    .help("Frequency range the gate acts on (low..high, e.g. 2kHz..20kHz)"),
            )
            .param(
                param("transition", ParamKind::Gain)
                    .default("6dB")
                    .help("Width in dB of the soft knee between full reduction and none"),
            )
            .example("ferraus dehiss tape.wav -o clean.wav", "Measure the hiss and reduce it by 12 dB")
            .example(
                "ferraus dehiss cassette.wav --reduction 18dB --range 3kHz..20kHz -o clean.wav",
                "Stronger reduction, above 3 kHz only",
            )
            .example("ferraus dehiss narration.wav --floor -66dB --reduction 8dB -o clean.wav", "Gentle reduction against a known hiss level of -66 dBFS")
            .example("ferraus dehiss vinyl.wav --range 5kHz..20kHz --transition 10dB -o clean.wav", "Only above 5 kHz, with a wide soft knee to keep cymbals natural"),
        OpSpec::transform(
            "repair",
            Category::Restoration,
            "Spectral repair of a time-frequency region from its neighbours",
            repair,
        )
        .about(
            "Re-synthesises the selected time range (optionally only a frequency band) from \
             the audio just before and after it: magnitudes are interpolated across the gap \
             and phases continue the instantaneous frequency of the left neighbour \
             ('interpolate'), or the region is only turned down to its neighbours' level \
             ('attenuate'). This is the marquee-and-heal workflow without a mouse. The \
             metrics list each region that was replaced.",
        )
        .param(
            param("range", ParamKind::Range)
                .required()
                .repeatable()
                .help("Time range to repair (start..end, e.g. 1:02.50..1:02.70); repeatable"),
        )
        .param(
            param("freq", ParamKind::Str)
                .default("all")
                .help("Frequency band to repair (low..high, e.g. 2kHz..6kHz) or 'all'"),
        )
        .param(
            param("method", ParamKind::Choice)
                .choices(&["interpolate", "attenuate"])
                .default("interpolate")
                .help("Re-synthesise from neighbours, or only attenuate to their level"),
        )
        .example(
            "ferraus repair take.wav --range 1:02.50..1:02.70 -o fixed.wav",
            "Heal a cough or click in a 200 ms region",
        )
        .example(
            "ferraus repair take.wav --range 12.1s..12.4s --freq 2kHz..6kHz --method attenuate -o fixed.wav",
            "Turn down a squeak in one band only",
        )
        .example("ferraus repair interview.wav --range 3:14.2..3:14.5 --range 7:02.0..7:02.3 -o fixed.wav", "Heal two separate phone pings in one pass")
        .example("ferraus repair guitar.wav --range 0:45.10..0:45.35 --freq 50Hz..300Hz -o fixed.wav", "Fill in a low thump without touching the strings above 300 Hz"),
        OpSpec::transform(
            "remove-sound",
            Category::Restoration,
            "Learn an unwanted sound and subtract it everywhere",
            remove_sound,
        )
        .best_effort()
        .about(
            "Learns spectral templates of an unwanted sound (a siren, a fridge, a phone \
             buzz) from a region where it plays alone (--learn) or from a saved model, then \
             subtracts the best-matching template from every frame. Best-effort: reports the \
             residual energy left in the learned region as a quality estimate. \
             --save-model writes the learned templates for reuse with --model.",
        )
        .param(param("learn", ParamKind::Range).help("Region containing only the unwanted sound (start..end)"))
        .param(
            param("model", ParamKind::Path).help("Load templates saved earlier with --save-model instead of --learn"),
        )
        .param(param("save-model", ParamKind::Path).help("Write the learned templates to this JSON file"))
        .param(
            param("complexity", ParamKind::Int)
                .default("20")
                .range("1..64")
                .help("Number of spectral templates learned (more follows a changing sound)"),
        )
        .param(param("passes", ParamKind::Int).default("2").range("1..8").help("Subtraction passes"))
        .param(param("strength", ParamKind::Percent).default("70%").help("How hard to subtract, in percent"))
        .param(
            param("protect-speech", ParamKind::Switch)
                .default("on")
                .help("Limit reduction in the 100 Hz-4 kHz speech band (on|off)"),
        )
        .example(
            "ferraus remove-sound street.wav --learn 0:10..0:14 -o clean.wav",
            "Learn the siren from 10-14 s and remove it",
        )
        .example(
            "ferraus remove-sound take2.wav --model buzz.json --strength 90% -o clean.wav",
            "Reuse a saved model of a buzz",
        )
        .example("ferraus remove-sound kitchen.wav --learn 1:20..1:26 --save-model fridge.json -o clean.wav", "Learn a fridge hum, remove it, and save the model for the next take")
        .example("ferraus remove-sound bed.wav --learn 0:02..0:05 --protect-speech off --complexity 32 --passes 3 -o clean.wav", "No dialogue here: subtract across the whole spectrum, with more templates and passes"),
    ]
}

fn parse_band(s: &str, nyq: f64) -> Result<(f64, f64)> {
    if s == "all" || s.is_empty() {
        return Ok((0.0, nyq));
    }
    let (lo, hi) = units::parse_num_range(s, units::parse_freq)?;
    Ok((lo.min(hi), lo.max(hi).min(nyq)))
}

// ---------------------------------------------------------------------------------
// dehiss
// ---------------------------------------------------------------------------------

fn dehiss(a: &Args, mut buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let sr = buf.sample_rate;
    let nyq = sr as f64 / 2.0;
    let reduction = a.db("reduction")?.abs();
    let transition = a.db("transition")?.abs().max(0.5);
    let (mut lo, mut hi) = parse_band(&a.str_or("range", "2kHz..20kHz"), nyq)
        .map_err(|e| Error::usage(format!("--range: {}", e.message)))?;
    if hi >= 19_500.0f64.min(nyq * 0.95) {
        hi = nyq;
    }
    lo = lo.max(0.0);
    let floor_arg = a.str_or("floor", "auto");
    let explicit_floor = if floor_arg == "auto" {
        None
    } else {
        Some(units::parse_db(&floor_arg).map_err(|e| Error::usage(format!("--floor: {}", e.message)))?)
    };
    if buf.is_empty() || reduction == 0.0 {
        return Ok(buf);
    }
    let st = Stft::for_rate(sr, 43.0, 4);
    let bins = st.bins();
    // range weight per bin with a 1/3-octave cosine taper outside the range
    let taper = 2f64.powf(1.0 / 3.0);
    let weight: Vec<f64> = (0..bins)
        .map(|k| {
            let f = st.bin_hz(k, sr);
            if f >= lo && f <= hi {
                1.0
            } else if f < lo && f > lo / taper {
                smoothstep((f - lo / taper) / (lo - lo / taper))
            } else if f > hi && f < hi * taper {
                1.0 - smoothstep((f - hi) / (hi * taper - hi))
            } else {
                0.0
            }
        })
        .collect();
    let frames = st.frame_count(buf.frames());
    let release_frames = (0.06 * sr as f64 / st.hop as f64).max(1.0);
    let step = reduction / release_frames;
    let mut floors_db = Vec::new();
    let mut out_data = Vec::with_capacity(buf.channels());
    for ch in &buf.data {
        ctx.check_cancel()?;
        // per-bin noise floor
        let floor: Vec<f64> = match explicit_floor {
            Some(db) => {
                let sigma2 = db_to_lin(db).powi(2);
                vec![sigma2 * st.n as f64 / 2.0; bins]
            }
            None => {
                let energies: Vec<f64> = (0..frames)
                    .map(|fi| st.frame(ch, fi).iter().zip(&weight).map(|(c, w)| c.norm_sqr() * w).sum())
                    .collect();
                // skip frames that are mostly padding (first and last window)
                let inner: Vec<usize> =
                    (0..frames).filter(|&fi| fi * st.hop >= st.n && fi * st.hop <= ch.len()).collect();
                let pool: Vec<usize> = if inner.is_empty() { (0..frames).collect() } else { inner };
                let pe: Vec<f64> = pool.iter().map(|&fi| energies[fi]).collect();
                let cut = percentile(&pe, 0.1);
                let quiet: Vec<usize> = pool.iter().copied().filter(|&fi| energies[fi] <= cut).collect();
                let mut acc = vec![0.0; bins];
                for &fi in &quiet {
                    for (a, c) in acc.iter_mut().zip(st.frame(ch, fi)) {
                        *a += c.norm_sqr();
                    }
                }
                let cnt = quiet.len().max(1) as f64;
                let raw: Vec<f64> = acc.iter().map(|v| v / cnt).collect();
                // smooth across frequency (+-4 bins)
                (0..bins)
                    .map(|k| {
                        let a = k.saturating_sub(4);
                        let b = (k + 5).min(bins);
                        raw[a..b].iter().sum::<f64>() / (b - a) as f64
                    })
                    .collect()
            }
        };
        let in_range: Vec<f64> = floor.iter().zip(&weight).filter(|(_, w)| **w >= 1.0).map(|(f, _)| *f).collect();
        if !in_range.is_empty() {
            let mean = in_range.iter().sum::<f64>() / in_range.len() as f64;
            floors_db.push(10.0 * (mean / (st.n as f64 / 2.0)).max(1e-30).log10());
        }
        let mut smooth = vec![0.0f64; bins];
        let mut gains = vec![0.0f64; bins]; // dB
        let mut first = true;
        let processed = st.process(std::slice::from_ref(ch), |_, spec| {
            let x = &mut spec[0];
            let p: Vec<f64> = x.iter().map(|c| c.norm_sqr()).collect();
            for k in 0..bins {
                let a = k.saturating_sub(1);
                let b = (k + 2).min(bins);
                let pk = p[a..b].iter().sum::<f64>() / (b - a) as f64;
                smooth[k] = if first { pk } else { 0.5 * smooth[k] + 0.5 * pk };
            }
            first = false;
            for k in 0..bins {
                if weight[k] <= 0.0 {
                    continue;
                }
                let r_db = 10.0 * (smooth[k] / floor[k].max(1e-30)).max(1e-30).log10();
                let open = smoothstep((r_db - 3.0) / transition);
                let target = -reduction * (1.0 - open);
                gains[k] = if target > gains[k] { target } else { (gains[k] - step).max(target) };
                let g = db_to_lin(gains[k] * weight[k]);
                x[k] = x[k].scale(g);
            }
        });
        out_data.push(processed.into_iter().next().unwrap());
    }
    buf.data = out_data;
    if !floors_db.is_empty() {
        let f = floors_db.iter().sum::<f64>() / floors_db.len() as f64;
        ctx.note("floor", format!("{f:.1}dB"));
        ctx.metric("floor_dbfs", (f * 10.0).round() / 10.0);
    }
    Ok(buf)
}

// ---------------------------------------------------------------------------------
// repair
// ---------------------------------------------------------------------------------

fn wrap(p: f64) -> f64 {
    let mut x = (p + PI) % (2.0 * PI);
    if x < 0.0 {
        x += 2.0 * PI;
    }
    x - PI
}

fn repair(a: &Args, mut buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let sr = buf.sample_rate;
    let nyq = sr as f64 / 2.0;
    let (flo, fhi) =
        parse_band(&a.str_or("freq", "all"), nyq).map_err(|e| Error::usage(format!("--freq: {}", e.message)))?;
    let attenuate = a.choice("method") == "attenuate";
    let ranges = a.ranges("range")?;
    if ranges.is_empty() {
        return Err(Error::usage("repair needs --range start..end"));
    }
    let st = Stft::new(
        crate::dsp::fft::next_pow2((sr as f64 * 0.043) as usize).max(256),
        crate::dsp::fft::next_pow2((sr as f64 * 0.043) as usize).max(256) / 8,
    );
    let n = st.n;
    let hop = st.hop;
    let bins = st.bins();
    let total = buf.frames();
    let mut report = Vec::new();
    for r in &ranges {
        let (s, e) = r.frames(sr, total, ctx.bpm())?;
        if e <= s {
            continue;
        }
        let seg_start = s.saturating_sub(4 * n);
        let seg_end = (e + 4 * n).min(total);
        let rs = s - seg_start; // region in segment coordinates
        let re = e - seg_start;
        // band weights with 2-bin soft edges
        let bw: Vec<f64> = (0..bins)
            .map(|k| {
                let f = st.bin_hz(k, sr);
                let d = st.bin_hz(1, sr) * 2.0;
                if f >= flo && f <= fhi {
                    1.0
                } else if f < flo {
                    (1.0 - (flo - f) / d).max(0.0)
                } else {
                    (1.0 - (f - fhi) / d).max(0.0)
                }
            })
            .collect();
        // frame fi covers segment samples [fi*hop - n, fi*hop)
        let overlap = |fi: usize| -> f64 {
            let a0 = fi as isize * hop as isize - n as isize;
            let a1 = fi as isize * hop as isize;
            let lo = a0.max(rs as isize);
            let hi = a1.min(re as isize);
            ((hi - lo).max(0) as f64 / n as f64 * 2.0).min(1.0)
        };
        // reference frames: last fully before, first fully after the region
        let left = rs / hop; // fi*hop <= rs
        let right = (re + n).div_ceil(hop); // fi*hop - n >= re
        let mut changed_frames = 0usize;
        let mut removed = 0.0f64;
        let mut before = 0.0f64;
        let mut new_data = Vec::with_capacity(buf.channels());
        for ch in &buf.data {
            let seg: Vec<f32> = ch[seg_start..seg_end].to_vec();
            let has_left = left >= 1 && left * hop >= n;
            let has_right = (right * hop).saturating_sub(n) < seg.len();
            let lref = st.frame(&seg, left);
            let lprev = st.frame(&seg, left.saturating_sub(1));
            let rref = st.frame(&seg, right);
            let (lref, rref) = match (has_left, has_right) {
                (true, true) => (lref, rref),
                (true, false) => (lref.clone(), lref),
                (false, true) => (rref.clone(), rref),
                (false, false) => (vec![C::ZERO; bins], vec![C::ZERO; bins]),
            };
            // instantaneous frequency (radians per hop) from the left neighbour
            let omega: Vec<f64> = (0..bins)
                .map(|k| {
                    let expect = 2.0 * PI * k as f64 * hop as f64 / n as f64;
                    if has_left {
                        expect + wrap(lref[k].arg() - lprev[k].arg() - expect)
                    } else {
                        expect
                    }
                })
                .collect();
            let span = (right - left).max(1) as f64;
            let out = st.process(std::slice::from_ref(&seg), |fi, spec| {
                if fi <= left || fi >= right {
                    return;
                }
                let w = overlap(fi);
                if w <= 0.0 {
                    return;
                }
                changed_frames += 1;
                let t = (fi - left) as f64 / span;
                let x = &mut spec[0];
                for k in 0..bins {
                    let bwk = bw[k] * w;
                    if bwk <= 0.0 {
                        continue;
                    }
                    let ml = lref[k].abs().max(1e-12);
                    let mr = rref[k].abs().max(1e-12);
                    let m = (ml.ln() * (1.0 - t) + mr.ln() * t).exp();
                    let orig = x[k];
                    let synth = if attenuate {
                        let g = (m / orig.abs().max(1e-12)).min(1.0);
                        orig.scale(g)
                    } else {
                        let ph = lref[k].arg() + omega[k] * (fi - left) as f64;
                        C::polar(m, ph)
                    };
                    let y = orig.scale(1.0 - bwk) + synth.scale(bwk);
                    before += orig.norm_sqr();
                    removed += orig.norm_sqr() - y.norm_sqr();
                    x[k] = y;
                }
            });
            let mut full = ch.clone();
            full[seg_start..seg_end].copy_from_slice(&out[0]);
            new_data.push(full);
        }
        buf.data = new_data;
        let change_db = if before > 0.0 { 10.0 * ((before - removed).max(1e-30) / before).log10() } else { 0.0 };
        report.push(json!({
            "start_s": s as f64 / sr as f64,
            "end_s": e as f64 / sr as f64,
            "freq_low_hz": flo,
            "freq_high_hz": fhi,
            "method": if attenuate { "attenuate" } else { "interpolate" },
            "frames": changed_frames / buf.channels().max(1),
            "level_change_db": (change_db * 10.0).round() / 10.0,
        }));
    }
    ctx.metric("replaced", Value::Array(report));
    Ok(buf)
}

// ---------------------------------------------------------------------------------
// remove-sound
// ---------------------------------------------------------------------------------

fn kmeans(data: &[Vec<f64>], k: usize, iters: usize) -> Vec<Vec<f64>> {
    let k = k.clamp(1, data.len().max(1));
    if data.is_empty() {
        return Vec::new();
    }
    let dim = data[0].len();
    // deterministic farthest-point initialisation starting from the first frame
    let mut cents: Vec<Vec<f64>> = vec![data[0].clone()];
    while cents.len() < k {
        let (idx, _) = data
            .iter()
            .enumerate()
            .map(|(i, d)| {
                let best = cents
                    .iter()
                    .map(|c| d.iter().zip(c).map(|(a, b)| (a - b).powi(2)).sum::<f64>())
                    .fold(f64::INFINITY, f64::min);
                (i, best)
            })
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
            .unwrap();
        cents.push(data[idx].clone());
    }
    for _ in 0..iters {
        let mut sums = vec![vec![0.0; dim]; k];
        let mut counts = vec![0usize; k];
        for d in data {
            let (j, _) = cents
                .iter()
                .enumerate()
                .map(|(j, c)| (j, d.iter().zip(c).map(|(a, b)| (a - b).powi(2)).sum::<f64>()))
                .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
                .unwrap();
            counts[j] += 1;
            for (s, v) in sums[j].iter_mut().zip(d) {
                *s += v;
            }
        }
        for j in 0..k {
            if counts[j] > 0 {
                cents[j] = sums[j].iter().map(|s| s / counts[j] as f64).collect();
            }
        }
    }
    cents
}

fn remove_sound(a: &Args, mut buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let sr = buf.sample_rate;
    let st = Stft::for_rate(sr, 43.0, 4);
    let bins = st.bins();
    let strength = a.percent("strength")?.clamp(0.0, 2.0);
    let passes = a.usize("passes")?.clamp(1, 8);
    let protect = a.switch("protect-speech", true);
    let learn = a.range("learn")?;
    let total = buf.frames();
    // templates: normalised magnitude spectra
    let templates: Vec<Vec<f64>> = if let Some(r) = &learn {
        let (s, e) = r.frames(sr, total, ctx.bpm())?;
        if e <= s + st.n {
            return Err(Error::usage(format!(
                "--learn region must be longer than {:.0} ms",
                st.n as f64 * 1000.0 / sr as f64
            )));
        }
        let mono = buf.mono_mix();
        let mut spectra = Vec::new();
        let mut fi = (s + st.n).div_ceil(st.hop);
        while fi * st.hop <= e {
            let m: Vec<f64> = st.frame(&mono, fi).iter().map(|c| c.abs()).collect();
            let norm = m.iter().map(|v| v * v).sum::<f64>().sqrt();
            if norm > 1e-9 {
                spectra.push(m.iter().map(|v| v / norm).collect());
            }
            fi += 1;
        }
        if spectra.is_empty() {
            return Err(Error::input("the --learn region is silent; nothing to learn"));
        }
        let k = a.usize("complexity")?.clamp(1, 64);
        kmeans(&spectra, k, 12)
            .into_iter()
            .map(|c| {
                let n = c.iter().map(|v| v * v).sum::<f64>().sqrt().max(1e-12);
                c.into_iter().map(|v| v / n).collect()
            })
            .collect()
    } else if let Some(path) = a.path("model") {
        ctx.check_allowed(&path)?;
        let text = std::fs::read_to_string(&path)
            .map_err(|e| Error::input(format!("cannot read model {}: {e}", path.display())))?;
        ctx.note_input(&path);
        let v: Value = serde_json::from_str(&text)?;
        let fft = v["fft"].as_u64().unwrap_or(0) as usize;
        let msr = v["sample_rate"].as_u64().unwrap_or(0) as u32;
        let t: Vec<Vec<f64>> = v["templates"]
            .as_array()
            .ok_or_else(|| Error::input("model has no 'templates'"))?
            .iter()
            .map(|row| row.as_array().map(|r| r.iter().filter_map(|x| x.as_f64()).collect()).unwrap_or_default())
            .collect();
        if fft != st.n || msr != sr || t.iter().any(|r: &Vec<f64>| r.len() != bins) {
            return Err(Error::input(format!(
                "model was learned at {msr} Hz / FFT {fft}; this input needs {sr} Hz / FFT {}",
                st.n
            ))
            .with_hint("resample the input or re-learn with --learn"));
        }
        t
    } else {
        return Err(Error::usage("remove-sound needs --learn start..end or --model file.json")
            .with_hint("ferraus remove-sound in.wav --learn 0:10..0:14 -o out.wav"));
    };
    if let Some(path) = a.path("save-model") {
        let model = json!({
            "ferraus_sound_model": 1,
            "sample_rate": sr,
            "fft": st.n,
            "templates": templates,
        });
        ctx.write_file(&path, serde_json::to_string(&model)?.as_bytes(), Some("model"))?;
    }
    let g_min = db_to_lin(-40.0 * strength.min(1.0));
    let g_speech = g_min.max(db_to_lin(-12.0));
    let over = 1.0 + 1.5 * strength;
    let in_learn = learn.as_ref().map(|r| r.frames(sr, total, ctx.bpm())).transpose()?;
    let energy_in = |b: &AudioBuffer| -> f64 {
        match in_learn {
            Some((s, e)) => b.data.iter().map(|c| c[s..e].iter().map(|v| (*v as f64).powi(2)).sum::<f64>()).sum(),
            None => 0.0,
        }
    };
    let before = energy_in(&buf);
    let speech: Vec<bool> = (0..bins)
        .map(|k| {
            let f = st.bin_hz(k, sr);
            protect && (100.0..=4000.0).contains(&f)
        })
        .collect();
    let mut out = Vec::with_capacity(buf.channels());
    for ch in &buf.data {
        ctx.check_cancel()?;
        let y = st.process(std::slice::from_ref(ch), |_, spec| {
            let x = &mut spec[0];
            let orig: Vec<f64> = x.iter().map(|c| c.abs()).collect();
            let mut mag = orig.clone();
            for _ in 0..passes {
                // best template by robust (median ratio) presence estimate
                let mut best = (0usize, 0.0f64);
                for (j, t) in templates.iter().enumerate() {
                    let tmax = t.iter().cloned().fold(0.0, f64::max);
                    let ratios: Vec<f64> =
                        t.iter().zip(&mag).filter(|(tv, _)| **tv > 0.1 * tmax).map(|(tv, m)| m / tv).collect();
                    let alpha = percentile(&ratios, 0.5);
                    if alpha > best.1 {
                        best = (j, alpha);
                    }
                }
                if best.1 <= 0.0 {
                    break;
                }
                let t = &templates[best.0];
                for k in 0..bins {
                    let est = over * best.1 * t[k];
                    let floor = if speech[k] { g_speech } else { g_min };
                    let g = (1.0 - est / mag[k].max(1e-12)).max(floor);
                    mag[k] *= g;
                }
            }
            for k in 0..bins {
                let g = if orig[k] > 1e-12 { mag[k] / orig[k] } else { 1.0 };
                x[k] = x[k].scale(g.min(1.0));
            }
        });
        out.push(y.into_iter().next().unwrap());
    }
    buf.data = out;
    if in_learn.is_some() && before > 0.0 {
        let after = energy_in(&buf);
        ctx.metric("residual_energy_db", (10.0 * (after.max(1e-30) / before).log10() * 10.0).round() / 10.0);
    }
    ctx.metric("templates", templates.len());
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::biquad::butterworth;
    use crate::testutil::*;
    use ferraus_core::testing::{self, run, tone_level};

    fn spec(name: &str) -> OpSpec {
        ops().into_iter().find(|o| o.name == name).unwrap()
    }

    fn hiss(sr: u32, secs: f64, amp: f32, seed: u64) -> AudioBuffer {
        let n = testing::noise(sr, secs, amp, seed);
        let hp = butterworth(4, 2500.0, sr as f64, true);
        AudioBuffer::from_mono(sr, hp.apply(&n.data[0]))
    }

    #[test]
    fn dehiss_reduces_hiss_by_10db() {
        // PESQ is not available: residual-hiss energy and SI-SDR improvement are the proxies
        let sr = 48000;
        let clean = music(sr, 1.5, 0.5);
        let h = hiss(sr, 1.5, 0.03, 5);
        let dirty = testing::add(&clean, &h);
        let out = run(&spec("dehiss"), &[], dirty.clone()).unwrap();
        let res_in: Vec<f32> = dirty.data[0].iter().zip(&clean.data[0]).map(|(a, b)| a - b).collect();
        let res_out: Vec<f32> = out.data[0].iter().zip(&clean.data[0]).map(|(a, b)| a - b).collect();
        let s = 4800..clean.frames() - 4800;
        let improvement = testing::rms_db(&res_in[s.clone()]) - testing::rms_db(&res_out[s.clone()]);
        eprintln!("dehiss improvement {improvement:.2} dB");
        assert!(improvement >= 10.0, "hiss reduced by only {improvement:.1} dB");
        let sdr_in = testing::si_sdr(&clean.data[0][s.clone()], &dirty.data[0][s.clone()]);
        let sdr_out = testing::si_sdr(&clean.data[0][s.clone()], &out.data[0][s]);
        assert!(sdr_out > sdr_in + 6.0, "SI-SDR {sdr_in:.1} -> {sdr_out:.1}");
        // the music (below 2 kHz) is untouched
        let t = 20.0 * (tone_level(&out.data[0], sr, 220.0) / tone_level(&dirty.data[0], sr, 220.0)).log10();
        assert!(t.abs() < 0.2, "{t}");
    }

    #[test]
    fn dehiss_noise_only_approaches_reduction() {
        let sr = 48000;
        let h = hiss(sr, 1.5, 0.03, 5);
        let s = 4800..h.frames() - 4800;
        for (red, min) in [("12dB", 11.0), ("6dB", 5.5)] {
            let out = run(&spec("dehiss"), &[("reduction", red)], h.clone()).unwrap();
            let r = testing::rms_db(&h.data[0][s.clone()]) - testing::rms_db(&out.data[0][s.clone()]);
            assert!(r >= min && r <= min + 1.2, "{red}: {r:.2}");
        }
    }

    #[test]
    fn dehiss_explicit_floor_null_and_silence() {
        let sr = 48000;
        let x = testing::add(&music(sr, 0.5, 0.5), &hiss(sr, 0.5, 0.02, 3));
        assert_eq!(run(&spec("dehiss"), &[("mix", "0%")], x.clone()).unwrap(), x);
        let out = run(&spec("dehiss"), &[("floor", "-45dB")], x.clone()).unwrap();
        assert_eq!(out.frames(), x.frames());
        let s = AudioBuffer::new(sr, 2, 3000);
        let out = run(&spec("dehiss"), &[], s.clone()).unwrap();
        assert!(out.peak() < 1e-6);
    }

    #[test]
    fn repair_heals_a_burst() {
        let sr = 48000;
        let clean = testing::sine(sr, 440.0, 1.0, 0.5);
        let mut dirty = clean.clone();
        let burst = testing::noise(sr, 0.05, 0.5, 9);
        for i in 0..burst.frames() {
            dirty.data[0][24000 + i] += burst.data[0][i];
        }
        let sp = spec("repair");
        let mut c = testing::ctx();
        let out = ferraus_core::exec::apply_spec(
            &sp,
            &testing::args(&sp, &[("range", "0.5s..0.55s")]),
            dirty.clone(),
            &mut c,
        )
        .unwrap();
        let region = 24000..26400;
        let err_in = testing::rms_db(&region.clone().map(|i| dirty.data[0][i] - clean.data[0][i]).collect::<Vec<_>>());
        let err_out = testing::rms_db(&region.map(|i| out.data[0][i] - clean.data[0][i]).collect::<Vec<_>>());
        assert!(err_out < err_in - 15.0, "{err_in:.1} -> {err_out:.1}");
        // outside the region nothing changes
        let far: Vec<f32> = (0..12000).map(|i| out.data[0][i] - dirty.data[0][i]).collect();
        assert!(testing::rms_db(&far) < -100.0);
        let rep = c.metrics["replaced"].as_array().unwrap();
        assert_eq!(rep.len(), 1);
        assert_eq!(rep[0]["method"], json!("interpolate"));
        // attenuate within a band, null
        let out =
            run(&sp, &[("range", "0.5s..0.55s"), ("freq", "2kHz..20kHz"), ("method", "attenuate")], dirty.clone())
                .unwrap();
        assert!(tone_level(&out.data[0], sr, 440.0) > 0.49);
        assert_eq!(run(&sp, &[("range", "0.5s..0.55s"), ("mix", "0%")], dirty.clone()).unwrap(), dirty);
    }

    #[test]
    fn remove_sound_subtracts_learned_buzz() {
        let sr = 48000;
        let n = sr as usize * 2;
        // unwanted: a band of noise 3-5 kHz all the time; wanted: a 440 Hz tone from 0.5 s
        let bp_hi = butterworth(4, 3000.0, sr as f64, true);
        let bp_lo = butterworth(4, 5000.0, sr as f64, false);
        let noise = testing::noise(sr, 2.0, 0.2, 4);
        let buzz = bp_lo.apply(&bp_hi.apply(&noise.data[0]));
        let mut tone = testing::sine(sr, 440.0, 2.0, 0.3).data[0].clone();
        for v in tone.iter_mut().take(24000) {
            *v = 0.0;
        }
        let mix: Vec<f32> = (0..n).map(|i| buzz[i] + tone[i]).collect();
        let x = AudioBuffer::from_mono(sr, mix);
        let dir = testing::TempDir::new("rmsound");
        let model = dir.path("buzz.json");
        let sp = spec("remove-sound");
        let mut c = testing::ctx();
        let out = ferraus_core::exec::apply_spec(
            &sp,
            &testing::args(&sp, &[("learn", "0s..0.45s"), ("save-model", model.to_str().unwrap())]),
            x.clone(),
            &mut c,
        )
        .unwrap();
        assert!(c.metrics["residual_energy_db"].as_f64().unwrap() < -10.0);
        // buzz band energy drops, the tone stays
        let band = |b: &[f32]| -> f64 {
            let y = bp_lo.apply(&bp_hi.apply(&b[30000..90000]));
            testing::rms_db(&y)
        };
        let drop = band(&x.data[0]) - band(&out.data[0]);
        eprintln!("remove-sound drop {drop:.2} dB");
        assert!(drop > 8.0, "buzz reduced by {drop:.1} dB");
        let t = 20.0
            * (tone_level(&out.data[0][30000..90000], sr, 440.0) / tone_level(&x.data[0][30000..90000], sr, 440.0))
                .log10();
        assert!(t.abs() < 1.0, "tone changed {t:.2} dB");
        // the saved model reproduces the result
        let out2 = run(&sp, &[("model", model.to_str().unwrap())], x.clone()).unwrap();
        assert!(testing::rms_diff(&out, &out2) < 1e-6);
        assert_eq!(run(&sp, &[("learn", "0s..0.45s"), ("mix", "0%")], x.clone()).unwrap(), x);
        assert!(run(&sp, &[], x).is_err());
    }
}
