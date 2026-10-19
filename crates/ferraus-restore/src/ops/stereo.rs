//! Stereo tools: `phase-align`, `center`, and `channels mix`.

use crate::dsp::fft::{next_pow2, Fft, Stft, C};
use crate::dsp::util::{channel_index, shift_channel, smoothstep};
use ferraus_core::prelude::*;
use ferraus_core::units::Time;
use std::f64::consts::PI;

pub fn ops() -> Vec<OpSpec> {
    vec![
        OpSpec::transform(
            "phase-align",
            Category::Restoration,
            "Align the timing of channels (azimuth and mic-placement correction)",
            phase_align,
        )
        .about(
            "In 'auto' mode, cross-correlates the channels window by window, takes a \
             confidence-weighted median of the per-window delays (to sub-sample precision), \
             and advances the late channel so the channels line up. In 'shift' mode, applies \
             the delays you give per channel (positive delays, negative advances). The \
             length is unchanged. The metrics report the shift and the inter-channel \
             correlation before and after.",
        )
        .param(
            param("mode", ParamKind::Choice)
                .choices(&["auto", "shift"])
                .default("auto")
                .help("Detect the delay automatically, or apply --shift"),
        )
        .param(
            param("shift", ParamKind::Str)
                .default("L=0smp,R=0smp")
                .help("Per-channel delays for --mode shift (L=0smp,R=12smp; times like 0.25ms also work)"),
        )
        .param(param("window", ParamKind::Time).default("200ms").help("Analysis window length for auto mode (time)"))
        .param(
            param("max-shift", ParamKind::Time)
                .default("5ms")
                .advanced()
                .help("Largest delay searched in auto mode (time)"),
        )
        .example("ferraus phase-align tape.wav -o aligned.wav", "Fix tape azimuth error automatically")
        .example(
            "ferraus phase-align room.wav --mode shift --shift L=0smp,R=12smp -o aligned.wav",
            "Delay the right channel by 12 samples",
        ),
        OpSpec::transform(
            "center",
            Category::Restoration,
            "Extract or remove centre-panned material (vocals, dialogue)",
            center,
        )
        .about(
            "Compares the left and right spectra bin by bin: bins whose level balance \
             matches --pan within --width and whose inter-channel phase is within \
             --phase-tolerance are treated as centre material. 'extract' keeps them, \
             'remove' takes them out (the quick karaoke tool). Only --freq is affected. \
             `separate` is the higher-quality path.",
        )
        .param(
            param("mode", ParamKind::Choice)
                .choices(&["extract", "remove"])
                .default("extract")
                .help("Keep the centre (extract) or remove it"),
        )
        .param(
            param("pan", ParamKind::Float)
                .default("0")
                .range("-100..100")
                .help("Position of the material to target (-100 left .. 0 centre .. 100 right)"),
        )
        .param(
            param("phase-tolerance", ParamKind::Str)
                .default("5deg")
                .help("Inter-channel phase difference still counted as centre (degrees)"),
        )
        .param(
            param("width", ParamKind::Percent)
                .default("20%")
                .help("Pan tolerance around --pan, in percent of the full left-right range"),
        )
        .param(param("freq", ParamKind::Str).default("120Hz..8kHz").help("Frequency range to act on (low..high)"))
        .param(param("mix", ParamKind::Percent).default("100%").help("Dry/wet balance: 0% returns the input unchanged"))
        .example("ferraus center song.wav --mode remove -o karaoke.wav", "Quick karaoke: remove the centre vocal")
        .example(
            "ferraus center song.wav --mode extract --freq 200Hz..6kHz -o vocal.wav",
            "Pull out the centre vocal band",
        ),
        OpSpec::transform(
            "channels mix",
            Category::Panning,
            "Channel mixer: per-output weights and phase invert",
            channels_mix,
        )
        .about(
            "Builds each output channel from weighted input channels, like Audition's \
             Channel Mixer. Each --out is 'OUT=expression' where the expression sums terms \
             such as 0.5L, -R, 50%C, or -3dB*L (a dB weight's sign is its gain, not a phase \
             flip). Channels are named L, R, C, LFE, Ls, Rs, M, or by number (1, ch2). \
             Outputs not mentioned pass through; --invert flips the phase of output channels.",
        )
        .param(param("out", ParamKind::Str).repeatable().help("Output definition, e.g. \"L=0.5L+0.5R\" (repeatable)"))
        .param(param("invert", ParamKind::List).help("Output channels to phase-invert, e.g. R or L,R"))
        .example(
            "ferraus channels mix in.wav --out \"L=0.5L+0.5R\" --out \"R=0.5L+0.5R\" -o mono-in-stereo.wav",
            "Fold to mono in both channels",
        )
        .example(
            "ferraus channels mix in.wav --out \"L=R\" --out \"R=L\" --invert R -o swapped.wav",
            "Swap the channels and flip the new right channel's phase",
        ),
    ]
}

// ---------------------------------------------------------------------------------
// phase-align
// ---------------------------------------------------------------------------------

/// Parse `L=0smp,R=12smp` into per-channel delays in samples.
fn parse_shifts(items: &[String], sr: u32, nch: usize) -> Result<Vec<f64>> {
    let mut d = vec![0.0; nch];
    for it in items {
        let (name, v) = it
            .split_once('=')
            .ok_or_else(|| Error::usage(format!("--shift item '{it}' should be CH=delay, e.g. R=12smp")))?;
        let idx = channel_index(name, nch)
            .filter(|i| *i < nch)
            .ok_or_else(|| Error::usage(format!("--shift: unknown channel '{name}'")))?;
        let v = v.trim();
        let (neg, body) = match v.strip_prefix('-') {
            Some(b) => (true, b),
            None => (false, v.strip_prefix('+').unwrap_or(v)),
        };
        let samples = match Time::parse(body)? {
            Time::Samples(s) => s as f64,
            t => t.seconds(sr, None)? * sr as f64,
        };
        d[idx] = if neg { -samples } else { samples };
    }
    Ok(d)
}

/// Delay of `y` relative to `x` (positive: y lags) in samples within +-max_lag, and the
/// normalised peak correlation.
fn xcorr_lag(x: &[f32], y: &[f32], max_lag: usize) -> (f64, f64) {
    let n = x.len().min(y.len());
    let nfft = next_pow2(2 * n);
    let fft = Fft::new(nfft);
    let mut a = vec![C::ZERO; nfft];
    let mut b = vec![C::ZERO; nfft];
    for i in 0..n {
        let w = 0.5 - 0.5 * (2.0 * PI * i as f64 / n as f64).cos();
        a[i].re = x[i] as f64 * w;
        b[i].re = y[i] as f64 * w;
    }
    let ea: f64 = a.iter().map(|c| c.re * c.re).sum();
    let eb: f64 = b.iter().map(|c| c.re * c.re).sum();
    fft.forward(&mut a);
    fft.forward(&mut b);
    // r[k] = sum x[i] y[i+k]  (peak at the lag of y)
    for (ai, bi) in a.iter_mut().zip(&b) {
        *ai = ai.conj() * *bi;
    }
    fft.inverse(&mut a);
    let at = |k: isize| -> f64 { a[k.rem_euclid(nfft as isize) as usize].re };
    let ml = max_lag.min(n.saturating_sub(1)) as isize;
    let mut best = (0isize, f64::MIN);
    for k in -ml..=ml {
        let v = at(k);
        if v > best.1 {
            best = (k, v);
        }
    }
    let (k, v) = best;
    let (l, r) = (at(k - 1), at(k + 1));
    let den = l - 2.0 * v + r;
    let frac = if den.abs() > 1e-20 && k > -ml && k < ml { (0.5 * (l - r) / den).clamp(-0.5, 0.5) } else { 0.0 };
    let norm = (ea * eb).sqrt().max(1e-30);
    (k as f64 + frac, v / norm)
}

fn zero_lag_corr(x: &[f32], y: &[f32]) -> f64 {
    let (mut xy, mut xx, mut yy) = (0.0, 0.0, 0.0);
    for (a, b) in x.iter().zip(y) {
        xy += *a as f64 * *b as f64;
        xx += (*a as f64).powi(2);
        yy += (*b as f64).powi(2);
    }
    xy / (xx * yy).sqrt().max(1e-30)
}

fn phase_align(a: &Args, mut buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let sr = buf.sample_rate;
    let nch = buf.channels();
    let shifts: Vec<f64> = if a.choice("mode") == "shift" {
        parse_shifts(&a.list("shift"), sr, nch)?
    } else {
        if nch < 2 {
            ctx.warn("mono_input", "phase-align needs at least two channels; input returned unchanged");
            return Ok(buf);
        }
        let win = ((a.seconds("window")? * sr as f64) as usize).clamp(256, 1 << 20);
        let max_lag = (a.seconds("max-shift")? * sr as f64).round().max(1.0) as usize;
        let n = buf.frames();
        let mut d = vec![0.0; nch];
        for c in 1..nch {
            let mut ests: Vec<(f64, f64)> = Vec::new();
            let hop = win / 2;
            let mut start = 0;
            loop {
                let end = (start + win).min(n);
                if end <= start + 64 {
                    break;
                }
                let x = &buf.data[0][start..end];
                let y = &buf.data[c][start..end];
                let rms = (x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / x.len() as f64).sqrt();
                if rms > 1e-4 {
                    let (lag, peak) = xcorr_lag(x, y, max_lag);
                    if peak > 0.3 {
                        ests.push((lag, peak));
                    }
                }
                if end == n {
                    break;
                }
                start += hop;
            }
            if ests.is_empty() {
                ctx.warn("no_alignment", format!("channel {} has no usable correlation with channel 1", c + 1));
                continue;
            }
            // weighted median
            ests.sort_by(|p, q| p.0.partial_cmp(&q.0).unwrap());
            let total: f64 = ests.iter().map(|e| e.1).sum();
            let mut acc = 0.0;
            let mut lag = ests[0].0;
            for e in &ests {
                acc += e.1;
                if acc >= total / 2.0 {
                    lag = e.0;
                    break;
                }
            }
            // stereo: advance whichever channel is late; multichannel: align every
            // channel to the first one
            if nch == 2 && lag < 0.0 {
                d[0] = lag;
            } else {
                d[c] = -lag;
            }
        }
        d
    };
    let before = if nch >= 2 { Some(zero_lag_corr(&buf.data[0], &buf.data[1])) } else { None };
    for (c, ch) in buf.data.iter_mut().enumerate() {
        if shifts[c].abs() > 1e-9 {
            *ch = shift_channel(ch, shifts[c]);
        }
    }
    let names = ["L", "R", "C", "LFE", "Ls", "Rs"];
    let mut m = serde_json::Map::new();
    for (c, s) in shifts.iter().enumerate() {
        let name = names.get(c).map(|s| s.to_string()).unwrap_or(format!("ch{}", c + 1));
        m.insert(name, json!((s * 100.0).round() / 100.0));
    }
    ctx.metric("shift_samples", Value::Object(m));
    if let Some(b) = before {
        ctx.metric("correlation_before", (b * 1000.0).round() / 1000.0);
        let after = zero_lag_corr(&buf.data[0], &buf.data[1]);
        ctx.metric("correlation_after", (after * 1000.0).round() / 1000.0);
    }
    Ok(buf)
}

// ---------------------------------------------------------------------------------
// center
// ---------------------------------------------------------------------------------

fn center(a: &Args, buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let mix = a.percent("mix")?.clamp(0.0, 1.0);
    if mix <= 0.0 {
        return Ok(buf);
    }
    let remove = a.choice("mode") == "remove";
    let sr = buf.sample_rate;
    if buf.channels() < 2 {
        ctx.warn("mono_input", "center needs a stereo input; a mono input is all centre");
        let mut out = buf.clone();
        if remove {
            out.scale(1.0 - mix as f32);
        }
        return Ok(out);
    }
    let pan = (a.f64("pan")? / 100.0).clamp(-1.0, 1.0);
    let tol = units::parse_with_unit(&a.str_or("phase-tolerance", "5deg"), &["deg", "°"])
        .map_err(|e| Error::usage(format!("--phase-tolerance: {}", e.message)))?
        .abs()
        .max(0.1)
        .to_radians();
    let width = a.percent("width")?.clamp(0.005, 2.0);
    let nyq = sr as f64 / 2.0;
    let (lo, hi) = units::parse_num_range(&a.str_or("freq", "120Hz..8kHz"), units::parse_freq)
        .map_err(|e| Error::usage(format!("--freq: {}", e.message)))?;
    let (lo, hi) = (lo.min(hi), lo.max(hi).min(nyq));
    let st = Stft::for_rate(sr, 85.0, 4);
    let bins = st.bins();
    let edge = 2f64.powf(1.0 / 6.0);
    let fmask: Vec<f64> = (0..bins)
        .map(|k| {
            let f = st.bin_hz(k, sr);
            if f >= lo && f <= hi {
                1.0
            } else if f < lo && f > lo / edge {
                smoothstep((f - lo / edge) / (lo - lo / edge))
            } else if f > hi && f < hi * edge {
                1.0 - smoothstep((f - hi) / (hi * edge - hi))
            } else {
                0.0
            }
        })
        .collect();
    // constant-power pan law: balance measure q = (|R|-|L|)/(|R|+|L|) = tan(pan*pi/4)
    let q0 = (pan * PI / 4.0).tan();
    let chans = vec![buf.data[0].clone(), buf.data[1].clone()];
    let processed = st.process(&chans, |_, spec| {
        let (l, r) = spec.split_at_mut(1);
        let (l, r) = (&mut l[0], &mut r[0]);
        for k in 0..bins {
            let fm = fmask[k];
            let (al, ar) = (l[k].abs(), r[k].abs());
            let m = if fm <= 0.0 || al + ar < 1e-12 {
                0.0
            } else {
                let q = (ar - al) / (ar + al);
                let mp = 1.0 - smoothstep(((q - q0).abs() - width) / width);
                let dphi = (l[k] * r[k].conj()).arg().abs();
                let mph = 1.0 - smoothstep((dphi - tol) / (3.0 * tol));
                mp * mph * fm
            };
            let g = if remove { 1.0 - m } else { m };
            l[k] = l[k].scale(g);
            r[k] = r[k].scale(g);
        }
    });
    let mut out = buf.clone();
    for (c, p) in processed.into_iter().enumerate() {
        out.data[c] = p;
    }
    // channels beyond the first two pass through (remove) or are dropped (extract)
    for c in 2..out.channels() {
        if !remove {
            for v in out.data[c].iter_mut() {
                *v = 0.0;
            }
        }
    }
    if mix < 1.0 {
        out = ferraus_core::exec::blend(&buf, &out, mix as f32);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------
// channels mix
// ---------------------------------------------------------------------------------

/// Parse a weight expression such as `0.5L+0.5R`, `-R`, `50%C`, `-3dB*L`.
fn parse_expr(expr: &str, nch: usize) -> Result<Vec<(usize, f64)>> {
    let s: Vec<char> = expr.chars().filter(|c| !c.is_whitespace()).collect();
    let mut terms = Vec::new();
    let mut i = 0;
    let bad = || Error::usage(format!("cannot parse channel expression '{expr}' (e.g. 0.5L+0.5R)"));
    while i < s.len() {
        let mut sign = 1.0;
        while i < s.len() && (s[i] == '+' || s[i] == '-') {
            if s[i] == '-' {
                sign = -sign;
            }
            i += 1;
        }
        let start = i;
        while i < s.len() && (s[i].is_ascii_digit() || s[i] == '.') {
            i += 1;
        }
        let num: Option<f64> =
            if i > start { Some(s[start..i].iter().collect::<String>().parse().map_err(|_| bad())?) } else { None };
        let mut w = match num {
            Some(v) => {
                if i < s.len() && s[i] == '%' {
                    i += 1;
                    sign * v / 100.0
                } else if i + 1 < s.len() && s[i].eq_ignore_ascii_case(&'d') && s[i + 1].eq_ignore_ascii_case(&'b') {
                    i += 2;
                    db_to_lin(sign * v)
                } else {
                    sign * v
                }
            }
            None => sign,
        };
        if i < s.len() && s[i] == '*' {
            i += 1;
        }
        let ns = i;
        while i < s.len() && s[i] != '+' && s[i] != '-' {
            i += 1;
        }
        let name: String = s[ns..i].iter().collect();
        if name.is_empty() {
            return Err(bad());
        }
        let idx = channel_index(&name, nch).ok_or_else(|| {
            Error::usage(format!("unknown channel '{name}' in '{expr}' (use L, R, C, LFE, Ls, Rs, M, or 1..N)"))
        })?;
        if idx >= nch {
            return Err(Error::usage(format!("'{expr}' refers to channel {name} but the input has {nch} channel(s)")));
        }
        if !w.is_finite() {
            w = 0.0;
        }
        terms.push((idx, w));
    }
    if terms.is_empty() {
        return Err(bad());
    }
    Ok(terms)
}

fn channels_mix(a: &Args, buf: AudioBuffer, _ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let nin = buf.channels();
    let mut defs: Vec<(usize, Vec<(usize, f64)>)> = Vec::new();
    let mut nout = nin;
    for o in a.all("out") {
        let (lhs, rhs) = o
            .split_once('=')
            .ok_or_else(|| Error::usage(format!("--out '{o}' should be OUT=expression, e.g. L=0.5L+0.5R")))?;
        let oi = channel_index(lhs, nin.max(2))
            .ok_or_else(|| Error::usage(format!("--out: unknown output channel '{lhs}'")))?;
        if oi >= 32 {
            return Err(Error::usage("at most 32 output channels"));
        }
        nout = nout.max(oi + 1);
        defs.push((oi, parse_expr(rhs, nin)?));
    }
    let frames = buf.frames();
    let mut out: Vec<Vec<f32>> =
        (0..nout).map(|c| if c < nin { buf.data[c].clone() } else { vec![0.0; frames] }).collect();
    for (oi, terms) in &defs {
        let mut ch = vec![0.0f64; frames];
        for (ii, w) in terms {
            for (o, v) in ch.iter_mut().zip(&buf.data[*ii]) {
                *o += *v as f64 * w;
            }
        }
        out[*oi] = ch.into_iter().map(|v| v as f32).collect();
    }
    for name in a.list("invert") {
        let idx = channel_index(&name, nout)
            .filter(|i| *i < nout)
            .ok_or_else(|| Error::usage(format!("--invert: unknown output channel '{name}'")))?;
        for v in out[idx].iter_mut() {
            *v = -*v;
        }
    }
    Ok(AudioBuffer::from_planar(buf.sample_rate, out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use ferraus_core::testing::{self, run};

    fn spec(name: &str) -> OpSpec {
        ops().into_iter().find(|o| o.name == name).unwrap()
    }

    fn apply(name: &str, pairs: &[(&str, &str)], x: AudioBuffer) -> (AudioBuffer, OpCtx<'static>) {
        let sp = spec(name);
        let mut c = testing::ctx();
        let out = ferraus_core::exec::apply_spec(&sp, &testing::args(&sp, pairs), x, &mut c).unwrap();
        (out, c)
    }

    fn broadband(sr: u32, secs: f64) -> Vec<f32> {
        let m = music(sr, secs, 0.5);
        let n = testing::noise(sr, secs, 0.2, 17);
        m.data[0].iter().zip(&n.data[0]).map(|(a, b)| a + b).collect()
    }

    #[test]
    fn phase_align_recovers_12_sample_delay() {
        let sr = 48000;
        let x = broadband(sr, 1.0);
        let r = shift_channel(&x, 12.0);
        let st = AudioBuffer::from_planar(sr, vec![x.clone(), r]);
        let (out, c) = apply("phase-align", &[], st);
        let shift_r = c.metrics["shift_samples"]["R"].as_f64().unwrap();
        assert!((shift_r + 12.0).abs() <= 1.0, "R shifted by {shift_r}");
        assert!(c.metrics["correlation_after"].as_f64().unwrap() > 0.99);
        // left untouched, right now aligned
        assert_eq!(out.data[0], x);
        let d: Vec<f32> = (100..40000).map(|i| out.data[1][i] - x[i]).collect();
        assert!(testing::rms_db(&d) < testing::rms_db(&x) - 30.0);
        // the other direction: left late
        let l = shift_channel(&x, 7.0);
        let (_, c) = apply("phase-align", &[], AudioBuffer::from_planar(sr, vec![l, x.clone()]));
        assert!((c.metrics["shift_samples"]["L"].as_f64().unwrap() + 7.0).abs() <= 1.0);
    }

    #[test]
    fn phase_align_shift_mode_null_and_mono() {
        let sr = 48000;
        let x = broadband(sr, 0.2);
        let st = AudioBuffer::from_planar(sr, vec![x.clone(), x.clone()]);
        let (out, _) = apply("phase-align", &[("mode", "shift"), ("shift", "L=0smp,R=12smp")], st.clone());
        assert_eq!(out.data[1][12..], x[..x.len() - 12]);
        assert_eq!(out.data[0], x);
        assert_eq!(run(&spec("phase-align"), &[("mix", "0%")], st.clone()).unwrap(), st);
        let mono = AudioBuffer::from_mono(sr, x);
        assert_eq!(run(&spec("phase-align"), &[], mono.clone()).unwrap(), mono);
    }

    #[test]
    fn center_extract_reaches_6db_sdr() {
        let sr = 48000;
        let secs = 1.5;
        let n = (sr as f64 * secs) as usize;
        // centre "vocal": harmonic tone with vibrato
        let vocal: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f64 / sr as f64;
                let ph = 2.0 * PI * 220.0 * t + 0.8 * (2.0 * PI * 5.0 * t).sin();
                (1..=12).map(|h| (h as f64 * ph).sin() / h as f64).sum::<f64>() as f32 * 0.25
            })
            .collect();
        // wide backing: a hard-left harmonic tone, hard-right filtered noise, and an
        // 80/20 panned pad
        let left_tone = testing::sine(sr, 311.0, secs, 0.2).data[0].clone();
        let lp = crate::dsp::biquad::butterworth(2, 3000.0, sr as f64, false);
        let right_noise = lp.apply(&testing::noise(sr, secs, 0.15, 3).data[0]);
        let pad = testing::sine(sr, 523.0, secs, 0.15).data[0].clone();
        let l: Vec<f32> = (0..n).map(|i| vocal[i] + left_tone[i] + 0.8 * pad[i]).collect();
        let r: Vec<f32> = (0..n).map(|i| vocal[i] + right_noise[i] + 0.2 * pad[i]).collect();
        let x = AudioBuffer::from_planar(sr, vec![l.clone(), r]);
        let (out, _) = apply("center", &[("mode", "extract")], x.clone());
        let s = 8000..n - 8000;
        let sdr = testing::si_sdr(&vocal[s.clone()], &out.data[0][s.clone()]);
        let sdr_in = testing::si_sdr(&vocal[s.clone()], &l[s.clone()]);
        eprintln!("center extract SDR {sdr:.2} dB (input {sdr_in:.2})");
        assert!(sdr >= 6.0, "extract SDR {sdr:.1} dB (input {sdr_in:.1} dB)");
        // remove mode lowers the vocal
        let (rem, _) = apply("center", &[("mode", "remove")], x.clone());
        let v_in = testing::tone_level(&l[s.clone()], sr, 440.0);
        let v_out = testing::tone_level(&rem.data[0][s.clone()], sr, 440.0);
        assert!(v_out < v_in * 0.25, "vocal harmonic {v_in} -> {v_out}");
        assert_eq!(run(&spec("center"), &[("mix", "0%")], x.clone()).unwrap(), x);
    }

    #[test]
    fn channels_mix_weights_and_invert() {
        let sr = 48000;
        let st = AudioBuffer::from_planar(sr, vec![vec![1.0; 10], vec![0.5; 10]]);
        let (out, _) = apply("channels mix", &[("out", "L=0.5L+0.5R")], st.clone());
        assert_eq!(out.data[0][0], 0.75);
        assert_eq!(out.data[1][0], 0.5);
        let sp = spec("channels mix");
        let a = Args::build(
            &sp,
            vec![("out".into(), "L=R".into()), ("out".into(), "R=L - 50%R".into()), ("invert".into(), "R".into())],
            &|_, _| None,
        )
        .unwrap();
        let mut c = testing::ctx();
        let out = channels_mix(&a, st.clone(), &mut c).unwrap();
        assert_eq!(out.data[0][3], 0.5);
        assert!((out.data[1][3] + 0.75).abs() < 1e-6);
        let (out, _) = apply("channels mix", &[("out", "C=-6dB*L+-6dB*R")], st.clone());
        assert_eq!(out.channels(), 3);
        assert!((out.data[2][0] - 1.5 * 0.501_187).abs() < 1e-4);
        assert_eq!(run(&sp, &[("out", "L=R"), ("mix", "0%")], st.clone()).unwrap(), st);
        assert!(run(&sp, &[("out", "L=0.5X")], st.clone()).is_err());
        assert!(run(&sp, &[("out", "L=ch5")], st).is_err());
    }
}
