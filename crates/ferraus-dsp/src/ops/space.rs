//! Space commands: reverb (algorithmic and convolution) and delay.

use super::{apply_own_mix, own_mix, percent_in, secs, sr, sync_secs};
use crate::biquad::{Biquad, Coeffs, FilterType};
use crate::delayline::DelayLine;
use crate::modulation::{Lfo, Shape};
use crate::reverb::{self, ReverbParams, ReverbType};
use ferraus_core::prelude::*;

pub fn ops() -> Vec<OpSpec> {
    vec![
        OpSpec::transform("reverb", Category::Space, "Add reverb (plate, room, hall, chamber, or an impulse response)", reverb_op)
            .about("Algorithmic reverb: a Dattorro plate for 'plate', and an 8-line feedback delay network with Hadamard mixing for 'room', 'chamber', and 'hall'. --decay is the time for the tail to fall 60 dB. With --ir <file> the input is convolved with an impulse response instead (partitioned FFT convolution; the IR's sample rate is matched automatically, its silent tail below --trim-ir is cut, and pre-delay defaults to 0 ms). The output keeps the input's length unless --tail adds room for the reverb to ring out.")
            .param(param("type", ParamKind::Choice).choices(&["plate", "room", "hall", "chamber"]).default("plate").help("Algorithm"))
            .param(param("size", ParamKind::Percent).default("50%").help("Room size (scales the delay network)"))
            .param(param("decay", ParamKind::Time).default("1.8s").help("Time for the tail to decay by 60 dB (RT60)"))
            .param(param("damping", ParamKind::Percent).default("50%").help("How much faster high frequencies die away"))
            .param(param("predelay", ParamKind::Time).default("20ms").help("Gap before the reverb starts (0ms default with --ir)"))
            .param(param("lowcut", ParamKind::Freq).default("120Hz").help("High-pass on the reverb (keeps lows clean; 0Hz = off)"))
            .param(param("highcut", ParamKind::Freq).default("8kHz").help("Low-pass on the reverb (0Hz = off)"))
            .param(param("width", ParamKind::Percent).default("100%").help("Stereo width of the reverb (0% mono, 200% extra wide)"))
            .param(param("mix", ParamKind::Percent).default("25%").help("Dry/wet: 0% returns the input, 100% is reverb only"))
            .param(param("ir", ParamKind::Path).help("Impulse response file for convolution reverb"))
            .param(param("trim-ir", ParamKind::Gain).default("-60dB").help("Cut the IR where it falls below this level relative to its peak"))
            .param(param("tail", ParamKind::Time).default("0s").help("Extra length appended so the reverb can ring out (0s keeps the input length)"))
            .example("ferraus reverb vox.wav -o vox_verb.wav", "Default plate at 25%")
            .example("ferraus reverb snare.wav --type room --decay 0.8s --mix 20% -o snare_room.wav", "Short room on a snare")
            .example("ferraus reverb pad.wav --type hall --decay 4s --tail 4s --mix 35% -o pad_hall.wav", "Long hall that rings out past the end")
            .example("ferraus reverb vox.wav --ir church.wav --mix 30% --tail 3s -o vox_church.wav", "Convolution with an impulse response"),
        OpSpec::transform("delay", Category::Space, "Feedback delay (echo), tempo-synced", delay)
            .about("Feedback delay with a filter in the loop, so each repeat is darker than the last. --time accepts beat values (1/4, 1/8D, 1/8T) resolved against --bpm, or plain times; without a tempo, beat values assume 160 BPM so 1/4 = 375 ms. --pingpong bounces repeats between left and right; --analog adds tape-style saturation and wow in the loop.")
            .param(param("time", ParamKind::Time).default("1/4").help("Delay time: beat value (1/4, 1/8D) or time (375ms)"))
            .param(param("feedback", ParamKind::Percent).default("35%").help("How much of each repeat feeds the next (0..98%)"))
            .param(param("mix", ParamKind::Percent).default("25%").help("Dry/wet: 0% returns the input"))
            .param(param("pingpong", ParamKind::Flag).help("Alternate repeats between left and right (output is stereo)"))
            .param(param("filter", ParamKind::Str).default("lowpass:6kHz").help("Filter in the feedback loop: lowpass:<freq>, highpass:<freq>, bandpass:<freq>, or off"))
            .param(param("analog", ParamKind::Flag).help("Tape-style saturation and wow in the loop"))
            .param(param("tail", ParamKind::Time).default("0s").help("Extra length appended for the echoes to die away"))
            .example("ferraus delay vox.wav --bpm 120 -o vox_delay.wav", "Quarter-note delay at 120 BPM")
            .example("ferraus delay gtr.wav --time 1/8D --pingpong --feedback 45% --analog --bpm 96 -o gtr_pp.wav", "Dotted-eighth analog ping-pong")
            .example("ferraus delay clap.wav --time 250ms --filter highpass:800Hz --tail 2s -o clap_echo.wav", "Thin echoes that ring out"),
    ]
}

fn reverb_params(a: &Args) -> Result<ReverbParams> {
    let decay = secs(a, "decay")?;
    if !(0.05..=60.0).contains(&decay) {
        return Err(Error::usage("--decay must be between 50ms and 60s"));
    }
    Ok(ReverbParams {
        kind: ReverbType::parse(&a.choice("type")),
        size: percent_in(a, "size", 0.0, 1.0)?,
        decay_s: decay,
        damping: percent_in(a, "damping", 0.0, 1.0)?,
        predelay_s: secs(a, "predelay")?,
        lowcut: a.freq("lowcut")?,
        highcut: a.freq("highcut")?,
        width: percent_in(a, "width", 0.0, 2.0)?,
    })
}

fn reverb_op(a: &Args, buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let m = own_mix(a)?;
    if m <= 0.0 {
        return Ok(buf);
    }
    let rate = sr(&buf);
    let n = buf.frames();
    let tail = a.time("tail")?.duration_frames(buf.sample_rate, ctx.bpm())?;
    let out_len = n + tail;
    let mut p = reverb_params(a)?;
    let wet: Vec<Vec<f32>> = if let Some(path) = a.path("ir") {
        let mut ir = ctx.read_audio_path(&path)?;
        if ir.sample_rate != buf.sample_rate {
            ir = ferraus_core::resample::resample(&ir, buf.sample_rate, ferraus_core::resample::Quality::High);
        }
        let mut chans = reverb::trim_ir(&ir.data, a.db("trim-ir")?);
        if !a.is_explicit("predelay") {
            p.predelay_s = 0.0;
        }
        let pd = (p.predelay_s * rate).round() as usize;
        if pd > 0 {
            for c in &mut chans {
                c.splice(0..0, std::iter::repeat_n(0.0, pd));
            }
        }
        ctx.note("ir_length_s", (chans[0].len() as f64 / rate * 1000.0).round() / 1000.0);
        let mut w = reverb::convolve_ir(&buf.data, &chans, out_len);
        let lowcut = if a.is_explicit("lowcut") { p.lowcut } else { 0.0 };
        let highcut = if a.is_explicit("highcut") { p.highcut } else { 0.0 };
        if w.len() == 2 {
            let (l, r) = w.split_at_mut(1);
            reverb::post_filter(&mut l[0], &mut r[0], rate, lowcut, highcut, p.width);
        } else if lowcut > 0.0 || highcut > 0.0 {
            for c in &mut w {
                let mut dummy = c.clone();
                reverb::post_filter(c, &mut dummy, rate, lowcut, highcut, 1.0);
            }
        }
        w
    } else {
        let mono = buf.mono_mix();
        let [l, r] = reverb::reverb_wet(&mono, out_len, rate, &p);
        match buf.channels() {
            1 => vec![l.iter().zip(&r).map(|(a, b)| (a + b) * std::f32::consts::FRAC_1_SQRT_2).collect()],
            c => (0..c).map(|i| if i % 2 == 0 { l.clone() } else { r.clone() }).collect(),
        }
    };
    let wet = AudioBuffer::from_planar(buf.sample_rate, wet);
    Ok(apply_own_mix(m, buf, wet))
}

enum LoopFilter {
    None,
    Biquad(Biquad),
}

fn loop_filter(spec: &str, rate: f64) -> Result<LoopFilter> {
    let s = spec.trim().to_ascii_lowercase();
    if s.is_empty() || s == "off" || s == "none" {
        return Ok(LoopFilter::None);
    }
    let (ty, f) = s.split_once(':').ok_or_else(|| {
        Error::usage(format!("--filter '{spec}': expected lowpass:6kHz, highpass:800Hz, bandpass:1kHz, or off"))
    })?;
    let ty = match ty {
        "lowpass" | "lp" => FilterType::Lowpass,
        "highpass" | "hp" => FilterType::Highpass,
        "bandpass" | "bp" => FilterType::Bandpass,
        other => return Err(Error::usage(format!("--filter: unknown type '{other}' (lowpass, highpass, bandpass)"))),
    };
    let freq = units::parse_freq(f).map_err(|e| Error::usage(format!("--filter: {}", e.message)))?;
    Ok(LoopFilter::Biquad(Biquad::new(Coeffs::design(ty, freq, 0.707, 0.0, rate))))
}

impl LoopFilter {
    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        match self {
            LoopFilter::None => x,
            LoopFilter::Biquad(b) => b.process(x),
        }
    }
}

/// One delay channel: line + loop filter + optional analog colour.
struct Echo {
    line: DelayLine,
    filt: LoopFilter,
    wow: Option<(Lfo, Lfo)>,
}

impl Echo {
    fn new(max: usize, filter: &str, rate: f64, analog: bool, phase: f64) -> Result<Echo> {
        Ok(Echo {
            line: DelayLine::new(max),
            filt: loop_filter(filter, rate)?,
            wow: analog
                .then(|| (Lfo::new(0.6, rate, Shape::Sine, phase), Lfo::new(5.3, rate, Shape::Sine, phase + 0.3))),
        })
    }

    /// Read the delayed, filtered signal.
    #[inline]
    fn read(&mut self, d: f64) -> f32 {
        let d = match &mut self.wow {
            Some((w, f)) => d * (1.0 + 0.003 * w.tick() + 0.0007 * f.tick()),
            None => d,
        };
        let y = self.line.read_cubic(d.max(2.0) as f32);
        let y = self.filt.process(y);
        if self.wow.is_some() {
            (1.3 * y).tanh() / 1.3
        } else {
            y
        }
    }
}

fn delay(a: &Args, buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let m = own_mix(a)?;
    if m <= 0.0 {
        return Ok(buf);
    }
    let rate = sr(&buf);
    let t = sync_secs(a, "time", buf.sample_rate, ctx, 160.0)?;
    if !(0.001..=10.0).contains(&t) {
        return Err(Error::usage("--time must be between 1ms and 10s"));
    }
    ctx.note("time_resolved", format!("{:.1}ms", t * 1000.0));
    let fb = percent_in(a, "feedback", 0.0, 0.98)? as f32;
    let analog = a.flag("analog");
    let filter = a.str_or("filter", "lowpass:6kHz");
    let d = t * rate;
    let max = (d * 1.01) as usize + 16;
    let n = buf.frames();
    let tail = a.time("tail")?.duration_frames(buf.sample_rate, ctx.bpm())?;
    let out_len = n + tail;
    let wet: Vec<Vec<f32>> = if a.flag("pingpong") {
        let mono = buf.mono_mix();
        let mut l = Echo::new(max, &filter, rate, analog, 0.0)?;
        let mut r = Echo::new(max, &filter, rate, analog, 0.5)?;
        let mut wl = Vec::with_capacity(out_len);
        let mut wr = Vec::with_capacity(out_len);
        for i in 0..out_len {
            let x = mono.get(i).copied().unwrap_or(0.0);
            let yl = l.read(d);
            let yr = r.read(d);
            l.line.push(x + fb * yr);
            r.line.push(fb * yl);
            wl.push(yl);
            wr.push(yr);
        }
        vec![wl, wr]
    } else {
        buf.data
            .iter()
            .enumerate()
            .map(|(c, ch)| -> Result<Vec<f32>> {
                let mut e = Echo::new(max, &filter, rate, analog, c as f64 * 0.25)?;
                let mut w = Vec::with_capacity(out_len);
                for i in 0..out_len {
                    let x = ch.get(i).copied().unwrap_or(0.0);
                    let y = e.read(d);
                    e.line.push(x + fb * y);
                    w.push(y);
                }
                Ok(w)
            })
            .collect::<Result<_>>()?
    };
    let wet = AudioBuffer::from_planar(buf.sample_rate, wet);
    Ok(apply_own_mix(m, buf, wet))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferraus_core::testing::{noise, run};

    fn spec(name: &str) -> OpSpec {
        ops().into_iter().find(|o| o.name == name).unwrap()
    }

    fn impulse(rate: u32, secs: f64) -> AudioBuffer {
        let mut v = vec![0.0f32; (rate as f64 * secs) as usize];
        v[0] = 1.0;
        AudioBuffer::from_mono(rate, v)
    }

    #[test]
    fn reverb_tail_reaches_minus_60_within_decay() {
        let s = spec("reverb");
        for (ty, decay) in [("plate", 1.8), ("room", 0.8), ("hall", 2.5), ("chamber", 1.2)] {
            let d = format!("{decay}s");
            let tail = format!("{}s", decay * 2.5);
            let rate = 24000;
            let y = run(
                &s,
                &[
                    ("type", ty),
                    ("decay", &d),
                    ("mix", "100%"),
                    ("tail", &tail),
                    ("predelay", "0ms"),
                    ("lowcut", "0Hz"),
                    ("highcut", "0Hz"),
                ],
                impulse(rate, 0.01),
            )
            .unwrap();
            // RT60 in the 1 kHz octave band (ISO 3382 practice: damping shortens the highs)
            let mut band = y.data[0].clone();
            crate::biquad::Cascade::butterworth_highpass(4, 707.0, rate as f64).process_block(&mut band);
            crate::biquad::Cascade::butterworth_lowpass(4, 1414.0, rate as f64).process_block(&mut band);
            let t = reverb::edc_decay_time(&band, rate as f64, 60.0).unwrap();
            assert!((t - decay).abs() <= 0.1 * decay, "{ty}: measured {t:.3}s for {decay}s (1 kHz band)");
            // broadband, without damping
            let y = run(
                &s,
                &[
                    ("type", ty),
                    ("decay", &d),
                    ("mix", "100%"),
                    ("tail", &tail),
                    ("predelay", "0ms"),
                    ("lowcut", "0Hz"),
                    ("highcut", "0Hz"),
                    ("damping", "0%"),
                ],
                impulse(rate, 0.01),
            )
            .unwrap();
            let t = reverb::edc_decay_time(&y.data[0], rate as f64, 60.0).unwrap();
            assert!((t - decay).abs() <= 0.1 * decay, "{ty}: measured {t:.3}s for {decay}s (broadband, no damping)");
        }
    }

    #[test]
    fn reverb_keeps_length_and_tail_extends() {
        let s = spec("reverb");
        let x = noise(48000, 0.2, 0.3, 1);
        assert_eq!(run(&s, &[], x.clone()).unwrap().frames(), x.frames());
        assert_eq!(run(&s, &[("tail", "1s")], x.clone()).unwrap().frames(), x.frames() + 48000);
        assert_eq!(run(&s, &[("mix", "0%")], x.clone()).unwrap(), x);
    }

    #[test]
    fn reverb_ir_convolution() {
        let dir = ferraus_core::testing::TempDir::new("reverb-ir");
        // an IR that is a single delayed impulse at 10 ms: output wet = delayed input
        let mut ir = vec![0.0f32; 24000];
        ir[441] = 1.0;
        let path = dir.path("ir.wav");
        ferraus_core::testing::write_wav(&path, &AudioBuffer::from_mono(44100, ir));
        let x = noise(44100, 0.2, 0.3, 2);
        let y = run(&spec("reverb"), &[("ir", path.to_str().unwrap()), ("mix", "100%")], x.clone()).unwrap();
        // the IR's leading silence is trimmed, so the wet path is the input itself
        let err: f32 = x.data[0].iter().zip(&y.data[0]).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
        assert!(err < 1e-3, "{err}");
    }

    #[test]
    fn delay_echoes_at_the_tempo() {
        let s = spec("delay");
        let x = impulse(48000, 1.5);
        let y = run(&s, &[("mix", "100%"), ("filter", "off")], x.clone()).unwrap();
        // no --bpm: 1/4 at 160 BPM = 375 ms
        let peak =
            y.data[0].iter().enumerate().fold((0, 0.0f32), |m, (i, v)| if v.abs() > m.1 { (i, v.abs()) } else { m });
        assert!((peak.0 as i64 - 18000).abs() <= 2, "{}", peak.0);
        // second echo is feedback-scaled
        let second = y.data[0][36000 - 3..36000 + 3].iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!((second / peak.1 - 0.35).abs() < 0.03);
        let pp = run(&s, &[("pingpong", "true"), ("mix", "100%")], x).unwrap();
        assert_eq!(pp.channels(), 2);
    }
}
