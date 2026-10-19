//! Modulation and character commands: chorus, flanger, phaser, tremolo, vibrato,
//! saturate, bitcrush.

use super::{apply_own_mix, own_mix, per_channel, percent_in, rate_hz, sr};
use crate::modulation::{self, Chorus, Lfo, Phaser, Shape, Vibrato};
use crate::saturation::{SatType, Saturator};
use ferraus_core::prelude::*;

pub fn ops() -> Vec<OpSpec> {
    vec![
        OpSpec::transform("chorus", Category::Tone, "Chorus (or chorus/flanger blend)", chorus)
            .about("Several slowly modulated delay voices thicken the sound; stereo channels get offset LFO phases for width. --mode flanger blends in flanger character (shorter delays with feedback).")
            .param(param("rate", ParamKind::Freq).default("0.8Hz").help("LFO rate"))
            .param(param("depth", ParamKind::Time).default("3ms").help("Delay sweep width"))
            .param(param("voices", ParamKind::Int).default("2").range("1..8").help("Number of delay voices"))
            .param(param("mix", ParamKind::Percent).default("50%").help("Dry/wet: 0% returns the input"))
            .param(param("mode", ParamKind::Choice).choices(&["chorus", "flanger"]).default("chorus").help("chorus, or flanger for a chorus/flanger blend"))
            .example("ferraus chorus gtr.wav -o gtr_chorus.wav", "Classic two-voice chorus")
            .example("ferraus chorus synth.wav --voices 4 --depth 5ms --rate 0.4Hz --mix 40% -o lush.wav", "Lush four-voice chorus"),
        OpSpec::transform("flanger", Category::Tone, "Flanger", flanger)
            .about("A very short delay swept by an LFO and fed back, mixed with the dry signal for the jet-plane comb sweep.")
            .param(param("rate", ParamKind::Freq).default("0.3Hz").help("LFO rate"))
            .param(param("depth", ParamKind::Time).default("2ms").help("Delay sweep width"))
            .param(param("feedback", ParamKind::Percent).default("50%").help("Resonance (-95%..95%)"))
            .param(param("mix", ParamKind::Percent).default("50%").help("Dry/wet: 0% returns the input"))
            .example("ferraus flanger drums.wav -o drums_flange.wav", "Default flanger")
            .example("ferraus flanger gtr.wav --rate 0.1Hz --feedback 80% -o jet.wav", "Slow, resonant jet sweep"),
        OpSpec::transform("phaser", Category::Tone, "Phaser", phaser)
            .about("A chain of swept all-pass filters mixed with the dry signal creates moving notches.")
            .param(param("stages", ParamKind::Int).default("4").range("1..24").help("All-pass stages (2 per notch)"))
            .param(param("rate", ParamKind::Freq).default("0.5Hz").help("LFO rate"))
            .param(param("depth", ParamKind::Percent).default("70%").help("Sweep range"))
            .param(param("feedback", ParamKind::Percent).default("30%").help("Resonance (-95%..95%)"))
            .param(param("mix", ParamKind::Percent).default("50%").help("Dry/wet: 0% returns the input"))
            .example("ferraus phaser keys.wav -o keys_phase.wav", "Default phaser")
            .example("ferraus phaser gtr.wav --stages 8 --rate 0.2Hz --feedback 60% -o deep.wav", "Deep eight-stage sweep"),
        OpSpec::transform("tremolo", Category::Tone, "Tremolo (amplitude modulation)", tremolo)
            .about("Modulates the level with an LFO; the gain swings between full and (1 - depth). --rate also accepts beat values like 1/8 with --bpm.")
            .param(param("rate", ParamKind::Str).default("5Hz").help("LFO rate in Hz, or a beat value (1/8)"))
            .param(param("depth", ParamKind::Percent).default("50%").help("Modulation depth"))
            .param(param("shape", ParamKind::Choice).choices(&["sine", "triangle", "square"]).default("sine").help("LFO waveform"))
            .example("ferraus tremolo gtr.wav -o gtr_trem.wav", "Classic amp tremolo")
            .example("ferraus tremolo pad.wav --rate 1/8 --bpm 100 --shape square --depth 90% -o chop.wav", "Tempo-synced chop"),
        OpSpec::transform("vibrato", Category::Tone, "Vibrato (pitch modulation)", vibrato)
            .about("Modulates pitch with a sinusoidally swept delay; --depth is the peak deviation in cents. The constant delay is compensated so the output lines up with the input.")
            .param(param("rate", ParamKind::Freq).default("5Hz").help("LFO rate"))
            .param(param("depth", ParamKind::Pitch).default("20cents").help("Peak pitch deviation (cents or semitones)"))
            .example("ferraus vibrato vox.wav -o vox_vib.wav", "Subtle vocal vibrato")
            .example("ferraus vibrato synth.wav --rate 6.5Hz --depth 50cents -o wobble.wav", "Wider vibrato"),
        OpSpec::transform("saturate", Category::Tone, "Saturation (tape, tube, soft, hard)", saturate)
            .about("Drives the signal into a waveshaper with 2x oversampling. Small signals pass at unity gain; --drive pushes louder parts into the curve. tape is gentle with a slight top-end roll-off, tube is asymmetric (even harmonics), soft is tanh, hard clips. --tone tilts the result darker (negative) or brighter (positive), up to ±6 dB.")
            .param(param("drive", ParamKind::Gain).default("6dB").help("Gain into the saturator (removed afterwards)"))
            .param(param("type", ParamKind::Choice).choices(&["tape", "tube", "soft", "hard"]).default("tape").help("Saturation curve"))
            .param(param("tone", ParamKind::Float).default("0").range("-100..100").help("Post tilt: -100 darkest .. +100 brightest"))
            .example("ferraus saturate bass.wav -o bass_warm.wav", "Gentle tape warmth")
            .example("ferraus saturate drums.wav --type tube --drive 12dB --mix 50% -o drums_dirty.wav", "Parallel tube grit"),
        OpSpec::transform("bitcrush", Category::Tone, "Bit-depth and sample-rate reduction", bitcrush)
            .about("Quantizes to --bits and holds samples at --rate for lo-fi aliasing and grit.")
            .param(param("bits", ParamKind::Int).default("8").range("1..24").help("Bit depth"))
            .param(param("rate", ParamKind::Freq).default("11025Hz").help("Sample-and-hold rate"))
            .example("ferraus bitcrush drums.wav -o crushed.wav", "8-bit, 11 kHz")
            .example("ferraus bitcrush synth.wav --bits 4 --rate 4kHz --mix 30% -o grit.wav", "Heavy crush blended in"),
    ]
}

fn chorus(a: &Args, buf: AudioBuffer, _ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let m = own_mix(a)?;
    if m <= 0.0 {
        return Ok(buf);
    }
    let rate = sr(&buf);
    let lfo = a.freq("rate")?;
    let depth = a.seconds("depth")?.clamp(0.0, 0.05);
    let voices = a.usize("voices")?.clamp(1, 8);
    let flanger_mode = a.choice("mode") == "flanger";
    let (base, fb) = if flanger_mode { (0.004 + depth / 2.0, 0.4) } else { (0.015 + depth / 2.0, 0.0) };
    let mut wet = buf.clone();
    per_channel(&mut wet, |c, ch| {
        let mut ch_fx = Chorus::new(rate, lfo, base, depth, voices, fb, c as f64 * 0.25);
        for v in ch.iter_mut() {
            *v = ch_fx.process(*v);
        }
    });
    Ok(apply_own_mix(m, buf, wet))
}

fn flanger(a: &Args, buf: AudioBuffer, _ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let m = own_mix(a)?;
    if m <= 0.0 {
        return Ok(buf);
    }
    let rate = sr(&buf);
    let lfo = a.freq("rate")?;
    let depth = a.seconds("depth")?.clamp(0.0, 0.02);
    let fb = percent_in(a, "feedback", -0.95, 0.95)? as f32;
    let mut wet = buf.clone();
    per_channel(&mut wet, |c, ch| {
        let mut f = modulation::flanger(rate, lfo, depth, fb, c as f64 * 0.25);
        for v in ch.iter_mut() {
            *v = f.process(*v);
        }
    });
    Ok(apply_own_mix(m, buf, wet))
}

fn phaser(a: &Args, buf: AudioBuffer, _ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let m = own_mix(a)?;
    if m <= 0.0 {
        return Ok(buf);
    }
    let rate = sr(&buf);
    let stages = a.usize("stages")?.clamp(1, 24);
    let lfo = a.freq("rate")?;
    let depth = percent_in(a, "depth", 0.0, 1.0)?;
    let fb = percent_in(a, "feedback", -0.95, 0.95)? as f32;
    let mut wet = buf.clone();
    per_channel(&mut wet, |c, ch| {
        let mut p = Phaser::new(rate, stages, lfo, depth, fb, c as f64 * 0.25);
        for v in ch.iter_mut() {
            *v = p.process(*v);
        }
    });
    Ok(apply_own_mix(m, buf, wet))
}

fn tremolo(a: &Args, mut buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let rate = sr(&buf);
    let f = rate_hz(a, "rate", ctx)?;
    let depth = percent_in(a, "depth", 0.0, 1.0)?;
    let shape = Shape::parse(&a.choice("shape"));
    let mut lfo = Lfo::new(f, rate, shape, 0.25); // start at full level
    let gains: Vec<f32> = (0..buf.frames()).map(|_| modulation::tremolo_gain(lfo.tick(), depth)).collect();
    per_channel(&mut buf, |_, ch| {
        for (v, g) in ch.iter_mut().zip(&gains) {
            *v *= g;
        }
    });
    Ok(buf)
}

fn vibrato(a: &Args, mut buf: AudioBuffer, _ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let rate = sr(&buf);
    let f = a.freq("rate")?;
    let cents = a.cents("depth")?.clamp(-1200.0, 1200.0);
    if cents == 0.0 {
        return Ok(buf);
    }
    let n = buf.frames();
    per_channel(&mut buf, |_, ch| {
        let mut v = Vibrato::new(rate, f, cents, Shape::Sine, 0.0);
        let lat = v.latency();
        let mut out = Vec::with_capacity(n);
        for i in 0..n + lat {
            let y = v.process(ch.get(i).copied().unwrap_or(0.0));
            if i >= lat {
                out.push(y);
            }
        }
        *ch = out;
    });
    Ok(buf)
}

fn saturate(a: &Args, mut buf: AudioBuffer, _ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let rate = sr(&buf);
    let drive = a.db("drive")?.clamp(-24.0, 48.0) as f32;
    let kind = SatType::parse(&a.choice("type"));
    let tone = a.f64("tone")?;
    if !(-100.0..=100.0).contains(&tone) {
        return Err(Error::usage("--tone must be between -100 and 100"));
    }
    per_channel(&mut buf, |_, ch| Saturator::new(kind, drive, (tone / 100.0) as f32, rate).process(ch));
    Ok(buf)
}

fn bitcrush(a: &Args, mut buf: AudioBuffer, _ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let rate = sr(&buf);
    let bits = a.int("bits")?;
    if !(1..=24).contains(&bits) {
        return Err(Error::usage("--bits must be between 1 and 24"));
    }
    let hold_rate = a.freq("rate")?.clamp(1.0, rate);
    let q = 2f32.powi(bits as i32 - 1);
    let inc = hold_rate / rate;
    per_channel(&mut buf, |_, ch| {
        let mut phase = 1.0f64;
        let mut held = 0.0f32;
        for v in ch.iter_mut() {
            if phase >= 1.0 {
                phase -= 1.0;
                held = ((*v * q).round() / q).clamp(-1.0, 1.0);
            }
            phase += inc;
            *v = held;
        }
    });
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferraus_core::testing::{noise, run, sine, stereo};

    fn spec(name: &str) -> OpSpec {
        ops().into_iter().find(|o| o.name == name).unwrap()
    }

    #[test]
    fn modulation_effects_change_the_signal_but_keep_level() {
        let x = stereo(noise(48000, 0.5, 0.3, 3));
        for name in ["chorus", "flanger", "phaser", "tremolo", "vibrato", "saturate", "bitcrush"] {
            let y = run(&spec(name), &[], x.clone()).unwrap();
            assert_eq!(y.frames(), x.frames(), "{name}");
            assert_eq!(y.channels(), 2, "{name}");
            assert_ne!(y, x, "{name}");
            let db = 20.0 * (y.rms() / x.rms()).log10();
            assert!(db.abs() < 6.0, "{name}: {db} dB");
        }
    }

    #[test]
    fn tremolo_depth() {
        let x = AudioBuffer::from_mono(1000, vec![1.0; 2000]);
        let y = run(&spec("tremolo"), &[("rate", "5Hz"), ("depth", "50%")], x).unwrap();
        let min = y.data[0].iter().cloned().fold(1.0, f32::min);
        let max = y.data[0].iter().cloned().fold(0.0, f32::max);
        assert!((min - 0.5).abs() < 0.01 && (max - 1.0).abs() < 0.01, "{min} {max}");
    }

    #[test]
    fn bitcrush_quantizes() {
        let y = run(&spec("bitcrush"), &[("bits", "2"), ("rate", "48kHz")], sine(48000, 100.0, 0.1, 0.9)).unwrap();
        for v in &y.data[0] {
            assert!([-1.0f32, -0.5, 0.0, 0.5, 1.0].iter().any(|q| (v - q).abs() < 1e-6), "{v}");
        }
    }

    #[test]
    fn vibrato_is_aligned() {
        // with zero depth the input is returned; with depth, a low tone stays time-aligned
        let x = sine(48000, 50.0, 0.5, 0.5);
        assert_eq!(run(&spec("vibrato"), &[("depth", "0cents")], x.clone()).unwrap(), x);
        let y = run(&spec("vibrato"), &[("depth", "5cents")], x.clone()).unwrap();
        let err = ferraus_core::testing::rms_diff(&x, &y);
        assert!(err < 0.05, "{err}");
    }
}
