//! One-knob chains: `enhance-voice`, `enhance`, and `guitar`. They combine native DSP
//! from this crate with other ops (`compress`, `deess`, `eq`, `limit`, `reverb`) resolved
//! by name at run time, so this crate has no compile-time dependency on them. A stage
//! whose op is missing from the build (or rejects its arguments) is skipped with a
//! warning instead of failing the whole chain.

use crate::dsp::biquad::{butterworth, parse_filter_spec, Coefs, Sos};
use crate::dsp::fft::convolve;
use crate::dsp::loudness;
use crate::dsp::util::Follower;
use ferraus_core::prelude::*;
use ferraus_core::resample::{resample, resample_channel, Quality};

pub fn ops() -> Vec<OpSpec> {
    vec![
        OpSpec::transform(
            "enhance-voice",
            Category::Restoration,
            "Vocal enhancer: rumble filter, de-plosive, de-ess, compression, presence",
            enhance_voice,
        )
        .about(
            "A preset chain for a voice: a high-pass for rumble, a de-plosive that ducks \
             low-frequency bursts (p and b pops) only while they happen, then the `deess`, \
             `compress`, and presence `eq` commands with settings for the chosen voice type. \
             'music' is gentler and suits a sung vocal in a mix. The metrics list every stage \
             and its settings.",
        )
        .param(
            param("mode", ParamKind::Choice)
                .choices(&["male", "female", "music"])
                .default("male")
                .help("Voice type the settings are tuned for"),
        )
        .example("ferraus enhance-voice podcast.wav -o vox.wav", "Clean up a male speaking voice")
        .example("ferraus enhance-voice vocal.wav --mode music --mix 70% -o vox.wav", "Gentle polish for a sung vocal"),
        OpSpec::transform(
            "enhance",
            Category::Restoration,
            "Essential-Sound-style one-knob chain for dialogue, music, SFX, or ambience",
            enhance,
        )
        .about(
            "Runs a chain chosen by --preset (dialogue, music, sfx, ambience; dialogue when \
             omitted). Repair removes mains hum when it is clearly present, clicks, and (for \
             dialogue) hiss. Clarity applies the preset's tonal and dynamic shaping (for \
             dialogue: rumble filter, de-plosive, compression, de-essing, presence). --reverb \
             adds the `reverb` command at that wet level. Loudness then sets the integrated \
             loudness (BS.1770) to the preset's target (dialogue -16, music -14, sfx -18, \
             ambience -23 LUFS) or the value you give, with the `limit` command catching \
             peaks above -1 dBTP. The presets are ordinary presets: any flag overrides them.",
        )
        .preset("dialogue", "Speech: hum/click/hiss repair, voice clarity, -16 LUFS", &[("material", "dialogue")])
        .preset("music", "Music: click repair, gentle glue and air, -14 LUFS", &[("material", "music")])
        .preset("sfx", "Sound effects: click repair, light compression, -18 LUFS", &[("material", "sfx")])
        .preset("ambience", "Ambience and room tone: low cut, mid scoop, -23 LUFS", &[("material", "ambience")])
        .param(
            param("material", ParamKind::Choice)
                .choices(&["dialogue", "music", "sfx", "ambience"])
                .default("dialogue")
                .advanced()
                .help("Kind of material (normally set through --preset)"),
        )
        .param(
            param("loudness", ParamKind::Str)
                .default("auto")
                .help("Target integrated loudness (-16LUFS), 'auto' for the preset's target, or 'off'"),
        )
        .param(param("repair", ParamKind::Switch).default("on").help("Remove hum, clicks, and hiss (on|off)"))
        .param(param("clarity", ParamKind::Switch).default("on").help("Tonal and dynamic shaping (on|off)"))
        .param(
            param("reverb", ParamKind::Percent).default("0%").help("Creative reverb wet level in percent (0% = none)"),
        )
        .example("ferraus enhance interview.wav -o clean.wav", "Repair, clarify, and level dialogue to -16 LUFS")
        .example(
            "ferraus enhance song.wav --preset music --loudness -14LUFS --repair off -o master.wav",
            "Polish and level a music track",
        ),
        OpSpec::transform(
            "guitar",
            Category::Tone,
            "Guitar amp chain: compressor, tone filter, distortion, cabinet",
            guitar,
        )
        .about(
            "A guitar-rig chain: optional `compress` for sustain, a pre-emphasis filter, drive \
             into a 4x-oversampled waveshaper (clean, crunch, or lead voicing), a tone filter, \
             and a speaker cabinet: your impulse response file with --cab (FFT convolution) or \
             a built-in 4x12-style cabinet filter. The output level is matched to the input.",
        )
        .param(
            param("amp", ParamKind::Choice).choices(&["clean", "crunch", "lead"]).default("crunch").help("Amp voicing"),
        )
        .param(
            param("cab", ParamKind::Path).help("Cabinet impulse response (audio file); default is a built-in cabinet"),
        )
        .param(
            param("filter", ParamKind::List).default("auto").help(
                "Tone filter(s) after the amp, e.g. lowpass:5kHz,peak:800Hz:+2dB, or 'auto' for the amp's voicing",
            ),
        )
        .param(
            param("drive", ParamKind::Str)
                .default("auto")
                .help("Drive into the amp in dB, or 'auto' (clean 6dB, crunch 18dB, lead 30dB)"),
        )
        .param(param("compress", ParamKind::Switch).default("on").help("Compressor before the amp (on|off)"))
        .example("ferraus guitar di.wav -o amped.wav", "Crunch amp with the built-in cabinet")
        .example(
            "ferraus guitar di.wav --amp lead --cab 4x12.wav --drive 34dB --mix 80% -o lead.wav",
            "Lead tone through your own cabinet impulse response",
        ),
    ]
}

// ---------------------------------------------------------------------------------
// Stage helpers
// ---------------------------------------------------------------------------------

fn args_json(pairs: &[(&str, &str)]) -> Value {
    let mut m = serde_json::Map::new();
    for (k, v) in pairs {
        match m.get_mut(*k) {
            Some(Value::Array(a)) => a.push(json!(v)),
            Some(old) => {
                let prev = old.clone();
                *old = json!([prev, v]);
            }
            None => {
                m.insert(k.to_string(), json!(v));
            }
        }
    }
    Value::Object(m)
}

/// Run another op by name (resolved at run time). Missing ops or rejected arguments are
/// skipped with a warning.
pub(crate) fn stage(
    ctx: &mut OpCtx,
    log: &mut Vec<Value>,
    name: &str,
    pairs: &[(&str, &str)],
    buf: AudioBuffer,
) -> Result<AudioBuffer> {
    let available = ctx.registry.map(|r| r.get(name).is_some()).unwrap_or(false);
    if !available {
        ctx.warn("stage_unavailable", format!("'{name}' is not available in this build; that stage was skipped"));
        log.push(json!({"op": name, "skipped": true}));
        return Ok(buf);
    }
    let mut child = ctx.child(name, vec![]);
    let res = ferraus_core::exec::run_transform(&mut child, name, pairs, buf.clone());
    ctx.absorb(child);
    match res {
        Ok(out) => {
            log.push(json!({"op": name, "args": args_json(pairs)}));
            Ok(out)
        }
        Err(e) if e.kind == ErrorKind::Usage => {
            ctx.warn("stage_failed", format!("'{name}' rejected its settings ({}); that stage was skipped", e.message));
            log.push(json!({"op": name, "skipped": true}));
            Ok(buf)
        }
        Err(e) => Err(e),
    }
}

/// Run one of this crate's own ops directly (no registry needed).
fn own(
    ctx: &mut OpCtx,
    log: &mut Vec<Value>,
    name: &str,
    pairs: &[(&str, &str)],
    buf: AudioBuffer,
) -> Result<AudioBuffer> {
    let spec = crate::ops()
        .into_iter()
        .find(|o| o.name == name)
        .ok_or_else(|| Error::internal(format!("missing built-in stage {name}")))?;
    let args = Args::build(&spec, pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(), &|_, _| None)?;
    let mut child = ctx.child(name, vec![]);
    let out = ferraus_core::exec::apply_spec(&spec, &args, buf, &mut child)?;
    ctx.absorb(child);
    log.push(json!({"op": name, "args": args_json(pairs)}));
    Ok(out)
}

fn native(log: &mut Vec<Value>, what: &str, detail: Value) {
    log.push(json!({"op": what, "native": true, "args": detail}));
}

fn highpass(buf: &mut AudioBuffer, freq: f64, order: usize) {
    let s = butterworth(order, freq, buf.sample_rate as f64, true);
    for ch in buf.data.iter_mut() {
        s.apply_in_place(ch);
    }
}

fn apply_sos(buf: &mut AudioBuffer, s: &Sos) {
    for ch in buf.data.iter_mut() {
        s.apply_in_place(ch);
    }
}

/// De-plosive: duck the band below 150 Hz while it bursts far above the rest of the
/// spectrum (complementary split, so with no plosives the signal is unchanged).
pub(crate) fn deplosive(buf: &mut AudioBuffer) -> usize {
    let sr = buf.sample_rate as f64;
    let lp = butterworth(2, 150.0, sr, false);
    let n = buf.frames();
    // zero-phase low band, so the complementary remainder carries almost none of it
    let pad = (sr * 0.05) as usize;
    let lows: Vec<Vec<f32>> =
        buf.data.iter().map(|c| lp.filtfilt(c, pad).into_iter().map(|v| v as f32).collect()).collect();
    // detection compares against a true high-pass (the complementary remainder still
    // carries part of the low band because of the low-pass phase shift)
    let hp = butterworth(2, 300.0, sr, true);
    let highs: Vec<Vec<f32>> = buf.data.iter().map(|c| hp.apply(c)).collect();
    let mut el = Follower::new(0.001, 0.06, sr);
    let mut eh = Follower::new(0.001, 0.06, sr);
    let mut gs = Follower::new(0.05, 0.001, sr); // gain recovers slowly, ducks fast
    gs.y = 1.0;
    let thr = db_to_lin(-40.0);
    let mut ducked = 0usize;
    let nch = buf.channels() as f64;
    let mut gains = vec![1.0f32; n];
    for (i, gv) in gains.iter_mut().enumerate() {
        let (mut l, mut h) = (0.0f64, 0.0f64);
        for c in 0..buf.data.len() {
            l += (lows[c][i] as f64).abs();
            h += (highs[c][i] as f64).abs();
        }
        let el_v = el.tick(l / nch);
        let eh_v = eh.tick(h / nch);
        // ratio of low-band to rest-of-band envelope; above 2 (6 dB) the low band is
        // ducked with a square law (continuous at the threshold)
        let target = if el_v > thr && el_v > 2.0 * eh_v { (2.0 * eh_v / el_v).powi(2).clamp(0.1, 1.0) } else { 1.0 };
        let g = gs.tick(target);
        if g < 0.9 {
            ducked += 1;
        }
        *gv = g as f32;
    }
    for (c, ch) in buf.data.iter_mut().enumerate() {
        for i in 0..n {
            let low = lows[c][i];
            ch[i] = ch[i] - low + gains[i] * low;
        }
    }
    ducked
}

// ---------------------------------------------------------------------------------
// enhance-voice
// ---------------------------------------------------------------------------------

struct VoiceSettings {
    hpf: f64,
    deess: [(&'static str, &'static str); 2],
    compress: [(&'static str, &'static str); 4],
    eq: &'static [&'static str],
}

fn voice_settings(mode: &str) -> VoiceSettings {
    match mode {
        "female" => VoiceSettings {
            hpf: 100.0,
            deess: [("center", "7kHz"), ("threshold", "-24dB")],
            compress: [("threshold", "-20dB"), ("ratio", "3:1"), ("attack", "5ms"), ("release", "80ms")],
            eq: &["peak:250Hz:-1dB:q=1", "peak:4kHz:+2dB:q=1", "highshelf:12kHz:+2dB"],
        },
        "music" => VoiceSettings {
            hpf: 40.0,
            deess: [("center", "7.5kHz"), ("threshold", "-20dB")],
            compress: [("threshold", "-18dB"), ("ratio", "2:1"), ("attack", "10ms"), ("release", "120ms")],
            eq: &["peak:5kHz:+1.5dB:q=0.8", "highshelf:12kHz:+1dB"],
        },
        _ => VoiceSettings {
            hpf: 70.0,
            deess: [("center", "5.5kHz"), ("threshold", "-24dB")],
            compress: [("threshold", "-20dB"), ("ratio", "3:1"), ("attack", "5ms"), ("release", "80ms")],
            eq: &["peak:250Hz:-1.5dB:q=1", "peak:3kHz:+2.5dB:q=1", "highshelf:10kHz:+1.5dB"],
        },
    }
}

fn voice_chain(ctx: &mut OpCtx, log: &mut Vec<Value>, mode: &str, mut buf: AudioBuffer) -> Result<AudioBuffer> {
    let s = voice_settings(mode);
    highpass(&mut buf, s.hpf, 4);
    native(log, "highpass", json!({"freq_hz": s.hpf, "order": 4}));
    let ducked = deplosive(&mut buf);
    native(
        log,
        "de-plosive",
        json!({"below_hz": 150, "ducked_ms": (ducked as f64 * 1000.0 / buf.sample_rate as f64).round()}),
    );
    buf = stage(ctx, log, "deess", &s.deess, buf)?;
    buf = stage(ctx, log, "compress", &s.compress, buf)?;
    let bands: Vec<(&str, &str)> = s.eq.iter().map(|b| ("band", *b)).collect();
    buf = stage(ctx, log, "eq", &bands, buf)?;
    Ok(buf)
}

fn enhance_voice(a: &Args, buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let mut log = Vec::new();
    let out = voice_chain(ctx, &mut log, &a.choice("mode"), buf)?;
    ctx.metric("stages", Value::Array(log));
    Ok(out)
}

// ---------------------------------------------------------------------------------
// enhance
// ---------------------------------------------------------------------------------

fn enhance(a: &Args, mut buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let preset = a.choice("material");
    let mut log = Vec::new();
    let sr = buf.sample_rate;
    let lufs_in = loudness::integrated(&buf.data, sr);
    if a.switch("repair", true) && !buf.is_empty() {
        if let Some(f) = crate::ops::hum::detect_hum(&buf) {
            let fs = format!("{f}Hz");
            buf = own(ctx, &mut log, "dehum", &[("freq", &fs)], buf)?;
        }
        if preset != "ambience" {
            buf = own(ctx, &mut log, "declick", &[("sensitivity", "20%"), ("passes", "2")], buf)?;
        }
        if preset == "dialogue" {
            buf = own(ctx, &mut log, "dehiss", &[("reduction", "8dB")], buf)?;
        }
    }
    if a.switch("clarity", true) {
        match preset.as_str() {
            "dialogue" => {
                buf = voice_chain(ctx, &mut log, "male", buf)?;
            }
            "music" => {
                buf = stage(
                    ctx,
                    &mut log,
                    "compress",
                    &[("threshold", "-18dB"), ("ratio", "1.5:1"), ("attack", "30ms"), ("release", "200ms")],
                    buf,
                )?;
                buf = stage(
                    ctx,
                    &mut log,
                    "eq",
                    &[("band", "lowshelf:100Hz:+1dB"), ("band", "highshelf:10kHz:+1dB")],
                    buf,
                )?;
            }
            "sfx" => {
                highpass(&mut buf, 30.0, 2);
                native(&mut log, "highpass", json!({"freq_hz": 30, "order": 2}));
                buf = stage(
                    ctx,
                    &mut log,
                    "compress",
                    &[("threshold", "-20dB"), ("ratio", "2:1"), ("attack", "10ms"), ("release", "100ms")],
                    buf,
                )?;
            }
            _ => {
                highpass(&mut buf, 40.0, 2);
                let scoop = Sos::new(vec![Coefs::peak(400.0, 0.8, -2.0, sr as f64)]);
                apply_sos(&mut buf, &scoop);
                native(&mut log, "eq", json!({"highpass_hz": 40, "peak": "400Hz -2dB"}));
            }
        }
    }
    let wet = a.percent("reverb")?.clamp(0.0, 1.0);
    if wet > 0.0 {
        let ty = match preset.as_str() {
            "music" => "plate",
            "ambience" => "hall",
            _ => "room",
        };
        let mix = format!("{:.0}%", wet * 100.0);
        buf = stage(ctx, &mut log, "reverb", &[("type", ty), ("mix", &mix)], buf)?;
    }
    let loud = a.str_or("loudness", "auto");
    if loud != "off" {
        let target = if loud == "auto" {
            match preset.as_str() {
                "music" => -14.0,
                "sfx" => -18.0,
                "ambience" => -23.0,
                _ => -16.0,
            }
        } else {
            units::parse_db(&loud).map_err(|e| Error::usage(format!("--loudness: {}", e.message)))?
        };
        if let Some(now) = loudness::integrated(&buf.data, sr) {
            let g = (target - now).clamp(-40.0, 40.0);
            buf.scale(db_to_lin(g) as f32);
            native(&mut log, "loudness", json!({"target_lufs": target, "gain_db": (g * 100.0).round() / 100.0}));
            if buf.peak() as f64 > db_to_lin(-1.0) {
                buf = stage(ctx, &mut log, "limit", &[("ceiling", "-1dBTP")], buf)?;
            }
            ctx.note("loudness", format!("{target}LUFS"));
        }
    }
    if let Some(l) = lufs_in {
        ctx.metric("loudness_in_lufs", (l * 10.0).round() / 10.0);
    }
    if let Some(l) = loudness::integrated(&buf.data, sr) {
        ctx.metric("loudness_out_lufs", (l * 10.0).round() / 10.0);
    }
    ctx.metric("stages", Value::Array(log));
    Ok(buf)
}

// ---------------------------------------------------------------------------------
// guitar
// ---------------------------------------------------------------------------------

fn shape(amp: &str, x: f64) -> f64 {
    match amp {
        "clean" => x.tanh(),
        // asymmetric: even harmonics, like a single-ended stage
        "crunch" => {
            if x >= 0.0 {
                x.tanh()
            } else {
                (1.4 * x).tanh() / 1.4 * 1.1
            }
        }
        _ => {
            let y = (1.5 * x).tanh();
            y.clamp(-0.85, 0.85) / 0.85
        }
    }
}

fn guitar(a: &Args, mut buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let amp = a.choice("amp");
    let sr = buf.sample_rate;
    let srf = sr as f64;
    let mut log = Vec::new();
    let in_rms = buf.rms() as f64;
    if buf.is_empty() || in_rms == 0.0 {
        return Ok(buf);
    }
    if a.switch("compress", true) {
        buf = stage(
            ctx,
            &mut log,
            "compress",
            &[("threshold", "-24dB"), ("ratio", "4:1"), ("attack", "5ms"), ("release", "100ms")],
            buf,
        )?;
    }
    // pre-emphasis
    let mut pre = vec![Coefs::highpass(80.0, 0.707, srf)];
    match amp.as_str() {
        "crunch" => pre.push(Coefs::peak(800.0, 0.8, 3.0, srf)),
        "lead" => pre.push(Coefs::peak(1000.0, 0.8, 5.0, srf)),
        _ => {}
    }
    apply_sos(&mut buf, &Sos::new(pre));
    let drive_db = match a.str_or("drive", "auto").as_str() {
        "auto" => match amp.as_str() {
            "clean" => 6.0,
            "lead" => 30.0,
            _ => 18.0,
        },
        s => units::parse_db(s).map_err(|e| Error::usage(format!("--drive: {}", e.message)))?,
    };
    // normalise the input to a reference level so the drive setting means the same
    // thing for quiet and hot DI tracks
    let pre_rms = buf.rms() as f64;
    let g = db_to_lin(drive_db) * 0.1 / pre_rms.max(1e-9);
    let os = 4u32;
    for ch in buf.data.iter_mut() {
        ctx.check_cancel()?;
        let up = resample_channel(ch, sr, sr * os, Quality::High);
        let shaped: Vec<f32> = up.iter().map(|v| shape(&amp, *v as f64 * g) as f32).collect();
        let mut down = resample_channel(&shaped, sr * os, sr, Quality::High);
        down.resize(ch.len(), 0.0);
        *ch = down;
    }
    native(&mut log, "amp", json!({"voicing": amp, "drive_db": drive_db, "oversample": os}));
    // tone filter
    let filters = a.list("filter");
    let tone: Vec<Coefs> = if filters.len() == 1 && filters[0] == "auto" {
        match amp.as_str() {
            "clean" => vec![Coefs::highshelf(5000.0, 0.707, 1.0, srf)],
            "lead" => vec![Coefs::lowpass(6000.0, 0.707, srf)],
            _ => vec![Coefs::lowpass(7000.0, 0.707, srf)],
        }
    } else {
        let mut v = Vec::new();
        for f in &filters {
            if let Some(c) = parse_filter_spec(f, srf).map_err(|e| Error::usage(format!("--filter: {}", e.message)))? {
                v.push(c);
            }
        }
        v
    };
    if !tone.is_empty() {
        apply_sos(&mut buf, &Sos::new(tone));
    }
    // cabinet
    if let Some(path) = a.path("cab") {
        let ir = ctx.read_audio_path(&path)?;
        let ir = if ir.sample_rate != sr { resample(&ir, sr, Quality::High) } else { ir };
        let mut h: Vec<f64> = ir.mono_mix().iter().map(|v| *v as f64).collect();
        h.truncate((srf * 2.0) as usize);
        let e = h.iter().map(|v| v * v).sum::<f64>().sqrt();
        if e <= 1e-12 {
            return Err(Error::input(format!("cabinet impulse response {} is silent", path.display())));
        }
        for v in h.iter_mut() {
            *v /= e;
        }
        for ch in buf.data.iter_mut() {
            let y = convolve(ch, &h);
            *ch = y[..ch.len()].iter().map(|v| *v as f32).collect();
        }
        native(&mut log, "cabinet", json!({"ir": path.display().to_string(), "taps": h.len()}));
    } else {
        let cab = Sos::new(vec![
            Coefs::highpass(90.0, 0.707, srf),
            Coefs::peak(2500.0, 1.2, 3.0, srf),
            Coefs::lowpass(5000.0, 0.54, srf),
            Coefs::lowpass(5000.0, 1.31, srf),
        ]);
        apply_sos(&mut buf, &cab);
        native(&mut log, "cabinet", json!({"built_in": "4x12"}));
    }
    // match the output level to the input
    let out_rms = buf.rms() as f64;
    if out_rms > 1e-12 {
        buf.scale((in_rms / out_rms) as f32);
    }
    ctx.metric("stages", Value::Array(log));
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::music;
    use ferraus_core::testing::{self, tone_level};
    use std::cell::RefCell;

    thread_local! {
        static CALLS: RefCell<Vec<(String, Vec<(String, String)>)>> = const { RefCell::new(Vec::new()) };
    }

    fn record(name: &str, a: &Args) {
        CALLS.with(|c| c.borrow_mut().push((name.to_string(), a.pairs())));
    }

    fn compress_standin(a: &Args, mut b: AudioBuffer, _: &mut OpCtx) -> Result<AudioBuffer> {
        record("compress", a);
        b.scale(0.5);
        Ok(b)
    }
    fn deess_standin(a: &Args, b: AudioBuffer, _: &mut OpCtx) -> Result<AudioBuffer> {
        record("deess", a);
        Ok(b)
    }
    fn eq_standin(a: &Args, b: AudioBuffer, _: &mut OpCtx) -> Result<AudioBuffer> {
        record("eq", a);
        Ok(b)
    }
    fn limit_standin(a: &Args, mut b: AudioBuffer, _: &mut OpCtx) -> Result<AudioBuffer> {
        record("limit", a);
        let c = db_to_lin(a.db("ceiling")?) as f32;
        for ch in b.data.iter_mut() {
            for v in ch.iter_mut() {
                *v = v.clamp(-c, c);
            }
        }
        Ok(b)
    }
    fn reverb_standin(a: &Args, b: AudioBuffer, _: &mut OpCtx) -> Result<AudioBuffer> {
        record("reverb", a);
        Ok(b)
    }

    fn registry() -> &'static ferraus_core::Registry {
        use ferraus_core::param;
        let mut v = vec![
            OpSpec::transform("compress", Category::Dynamics, "stand-in", compress_standin).params([
                param("threshold", ParamKind::Gain).default("-18dB"),
                param("ratio", ParamKind::Ratio).default("3:1"),
                param("attack", ParamKind::Time).default("10ms"),
                param("release", ParamKind::Time).default("100ms"),
            ]),
            OpSpec::transform("deess", Category::Dynamics, "stand-in", deess_standin).params([
                param("center", ParamKind::Freq).default("6.5kHz"),
                param("threshold", ParamKind::Gain).default("-20dB"),
            ]),
            OpSpec::transform("eq", Category::Tone, "stand-in", eq_standin)
                .param(param("band", ParamKind::Str).repeatable()),
            OpSpec::transform("limit", Category::Dynamics, "stand-in", limit_standin)
                .param(param("ceiling", ParamKind::Gain).default("-1dBTP")),
            OpSpec::transform("reverb", Category::Space, "stand-in", reverb_standin)
                .param(param("type", ParamKind::Str).default("plate"))
                .param(param("mix", ParamKind::Percent).default("25%")),
        ];
        v.extend(crate::ops());
        testing::leak_registry(v)
    }

    fn calls() -> Vec<(String, Vec<(String, String)>)> {
        CALLS.with(|c| c.borrow_mut().drain(..).collect())
    }

    fn apply(
        name: &str,
        pairs: &[(&str, &str)],
        x: AudioBuffer,
        reg: Option<&'static ferraus_core::Registry>,
    ) -> (AudioBuffer, OpCtx<'static>) {
        let sp = crate::ops().into_iter().find(|o| o.name == name).unwrap();
        let mut c = match reg {
            Some(r) => testing::ctx_with(r),
            None => testing::ctx(),
        };
        let out = ferraus_core::exec::apply_spec(&sp, &testing::args(&sp, pairs), x, &mut c).unwrap();
        (out, c)
    }

    #[test]
    fn enhance_voice_wires_runtime_stages_in_order() {
        let reg = registry();
        calls();
        let sr = 48000;
        let x = testing::add(&testing::sine(sr, 30.0, 1.0, 0.3), &testing::sine(sr, 1000.0, 1.0, 0.3));
        let (out, c) = apply("enhance-voice", &[("mode", "female")], x.clone(), Some(reg));
        let log = calls();
        let names: Vec<&str> = log.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["deess", "compress", "eq"]);
        assert!(log[0].1.contains(&("center".to_string(), "7kHz".to_string())));
        assert!(log[1].1.contains(&("ratio".to_string(), "3:1".to_string())));
        assert_eq!(log[2].1.iter().filter(|(k, _)| k == "band").count(), 3);
        // native rumble filter removed 30 Hz; the stand-in compressor halved the level
        let r = &out.data[0][24000..];
        assert!(tone_level(r, sr, 30.0) < 0.3 * 0.5 * 0.1);
        assert!((tone_level(r, sr, 1000.0) - 0.15).abs() < 0.01);
        assert_eq!(c.metrics["stages"].as_array().unwrap().len(), 5);
        // null test
        let (n, _) = apply("enhance-voice", &[("mix", "0%")], x.clone(), Some(reg));
        assert_eq!(n, x);
    }

    #[test]
    fn chains_degrade_gracefully_without_runtime_ops() {
        let sr = 48000;
        let x = testing::sine(sr, 1000.0, 0.5, 0.3);
        let (out, c) = apply("enhance-voice", &[], x, None);
        assert_eq!(out.frames(), 24000);
        assert!(c.warnings.iter().filter(|w| w.code == "stage_unavailable").count() == 3);
    }

    #[test]
    fn deplosive_ducks_low_bursts_only() {
        let sr = 48000;
        let voice = testing::sine(sr, 1200.0, 1.0, 0.2);
        let mut x = voice.clone();
        // a 60 Hz thump between 0.4 s and 0.5 s
        for i in 19200..24000 {
            x.data[0][i] += 0.8 * (2.0 * std::f64::consts::PI * 60.0 * i as f64 / sr as f64).sin() as f32;
        }
        let mut y = x.clone();
        let ducked = deplosive(&mut y);
        assert!(ducked > 0);
        let thump_in = tone_level(&x.data[0][19200..24000], sr, 60.0);
        let thump_out = tone_level(&y.data[0][19200..24000], sr, 60.0);
        assert!(thump_out < thump_in * 0.5, "{thump_in} -> {thump_out}");
        // no plosive, no change
        let mut v = voice.clone();
        assert_eq!(deplosive(&mut v), 0);
        assert!(testing::rms_diff(&v, &voice) < 1e-4);
    }

    #[test]
    fn enhance_presets_reach_loudness_and_log_stages() {
        let reg = registry();
        calls();
        let sr = 48000;
        let x = testing::stereo(music(sr, 1.5, 0.3));
        let (out, c) = apply("enhance", &[("preset", "music")], x.clone(), Some(reg));
        let l = loudness::integrated(&out.data, sr).unwrap();
        assert!((l + 14.0).abs() < 0.6 || out.peak() <= db_to_lin(-1.0) as f32 + 1e-6, "loudness {l}");
        let names: Vec<String> = calls().into_iter().map(|(n, _)| n).collect();
        assert!(names.contains(&"compress".to_string()));
        assert!(c.metrics["stages"].as_array().unwrap().len() >= 3);
        // dialogue with hum: hum is detected and removed by the native dehum stage
        let mut hummy = music(sr, 1.5, 0.3);
        let hum = testing::sine(sr, 60.0, 1.5, 0.05);
        for (v, h) in hummy.data[0].iter_mut().zip(&hum.data[0]) {
            *v += *h;
        }
        let (_, c) = apply("enhance", &[("loudness", "-20LUFS"), ("reverb", "20%")], hummy, Some(reg));
        let stages = c.metrics["stages"].to_string();
        assert!(stages.contains("dehum") && stages.contains("reverb"), "{stages}");
        assert!((c.metrics["loudness_out_lufs"].as_f64().unwrap() + 20.0).abs() < 1.0);
        // everything off is the identity
        let (o, _) =
            apply("enhance", &[("repair", "off"), ("clarity", "off"), ("loudness", "off")], x.clone(), Some(reg));
        assert_eq!(o, x);
        let (n, _) = apply("enhance", &[("mix", "0%")], x.clone(), Some(reg));
        assert_eq!(n, x);
    }

    #[test]
    fn guitar_adds_harmonics_and_uses_cab_ir() {
        let reg = registry();
        let sr = 48000;
        let di = testing::sine(sr, 220.0, 1.0, 0.2);
        let (clean, _) = apply("guitar", &[("amp", "clean"), ("compress", "off")], di.clone(), Some(reg));
        let (lead, c) = apply("guitar", &[("amp", "lead")], di.clone(), Some(reg));
        let h =
            |b: &AudioBuffer| tone_level(&b.data[0][12000..], sr, 660.0) / tone_level(&b.data[0][12000..], sr, 220.0);
        assert!(h(&lead) > h(&clean) * 2.0, "lead {} clean {}", h(&lead), h(&clean));
        assert!((lead.rms() - di.rms()).abs() / di.rms() < 0.01, "level matched");
        assert!(c.metrics["stages"].to_string().contains("compress"));
        // a cabinet IR that is a pure delay of 10 samples delays the output
        let dir = testing::TempDir::new("cab");
        let mut ir = vec![0.0f32; 64];
        ir[10] = 1.0;
        let ir_path = dir.path("ir.wav");
        testing::write_wav(&ir_path, &AudioBuffer::from_mono(sr, ir));
        let (with_ir, _) = apply(
            "guitar",
            &[("amp", "clean"), ("compress", "off"), ("cab", ir_path.to_str().unwrap()), ("filter", "none")],
            di.clone(),
            Some(reg),
        );
        assert_eq!(with_ir.frames(), di.frames());
        assert!(with_ir.data[0][..10].iter().all(|v| v.abs() < 1e-9));
        let (n, _) = apply("guitar", &[("mix", "0%")], di.clone(), Some(reg));
        assert_eq!(n, di);
    }
}
