//! Utilities: reverse, speed (varispeed), dc-remove, invert-phase.

use super::{per_channel, sr};
use crate::biquad::Cascade;
use crate::varispeed::varispeed;
use ferraus_core::prelude::*;

pub fn ops() -> Vec<OpSpec> {
    vec![
        OpSpec::transform("reverse", Category::Edit, "Play the audio backwards", reverse)
            .about("Reverses every channel. Combine with reverb for the classic reverse-reverb swell: reverse, reverb with --tail, reverse again.")
            .example("ferraus reverse cymbal.wav -o swell.wav", "Reverse a cymbal")
            .example("ferraus fx vox.wav -o pre.wav --fx reverse --fx \"reverb:mix=100%,tail=2s\" --fx reverse", "Reverse-reverb pre-swell"),
        OpSpec::transform("speed", Category::Edit, "Change speed and pitch together (varispeed)", speed)
            .about("Tape-style speed change: --factor 2.0 plays twice as fast, an octave higher, in half the time. Uses windowed-sinc resampling with automatic anti-aliasing. To change only duration or only pitch, use stretch or pitch.")
            .param(param("factor", ParamKind::Float).default("1.0").range("0.05..20").help("Playback speed multiplier (0.5 = half speed, an octave down)"))
            .no_mix()
            .example("ferraus speed loop.wav --factor 1.5 -o fast.wav", "Faster and higher")
            .example("ferraus speed vox.wav --factor 0.5 -o slow.wav", "Half speed, an octave down"),
        OpSpec::transform("dc-remove", Category::Restoration, "Remove DC offset", dc_remove)
            .about("Removes DC offset with a gentle high-pass (default 5 Hz, inaudible), which also follows a drifting offset. --freq 0Hz subtracts each channel's mean instead (Audition's DC bias adjust).")
            .param(param("freq", ParamKind::Freq).default("5Hz").help("High-pass corner; 0Hz subtracts the mean"))
            .example("ferraus dc-remove take.wav -o take_dc.wav", "Remove DC offset")
            .example("ferraus dc-remove old.wav --freq 0Hz -o old_dc.wav", "Subtract the static mean only"),
        OpSpec::transform("invert-phase", Category::Edit, "Invert polarity", invert_phase)
            .about("Multiplies the signal by -1 (all channels, or only --channel left/right). Fixes a mic wired out of polarity or tests phase cancellation.")
            .param(param("channel", ParamKind::Choice).choices(&["all", "left", "right"]).default("all").help("Which channel(s) to invert"))
            .example("ferraus invert-phase snare_bottom.wav -o flipped.wav", "Flip the polarity of a bottom-snare mic")
            .example("ferraus invert-phase stereo.wav --channel right -o r_flipped.wav", "Invert only the right channel"),
    ]
}

fn reverse(_a: &Args, mut buf: AudioBuffer, _ctx: &mut OpCtx) -> Result<AudioBuffer> {
    per_channel(&mut buf, |_, ch| ch.reverse());
    Ok(buf)
}

fn speed(a: &Args, mut buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let f = a.f64("factor")?;
    if !(0.05..=20.0).contains(&f) {
        return Err(Error::usage("--factor must be between 0.05 and 20"));
    }
    if f == 1.0 {
        return Ok(buf);
    }
    per_channel(&mut buf, |_, ch| *ch = varispeed(ch, f));
    ctx.metric("pitch_shift_semitones", (12.0 * f.log2() * 100.0).round() / 100.0);
    Ok(buf)
}

fn dc_remove(a: &Args, mut buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let f = a.freq("freq")?;
    let rate = sr(&buf);
    let offsets: Vec<f64> =
        buf.data.iter().map(|c| c.iter().map(|v| *v as f64).sum::<f64>() / c.len().max(1) as f64).collect();
    if f <= 0.0 {
        per_channel(&mut buf, |c, ch| {
            let m = offsets[c] as f32;
            ch.iter_mut().for_each(|v| *v -= m);
        });
    } else {
        // subtract the mean first so the filter does not have to settle from a step
        per_channel(&mut buf, |c, ch| {
            let m = offsets[c] as f32;
            ch.iter_mut().for_each(|v| *v -= m);
            Cascade::butterworth_highpass(2, f, rate).process_block(ch);
        });
    }
    ctx.metric("dc_offset", offsets.iter().map(|o| (o * 1e6).round() / 1e6).collect::<Vec<_>>());
    Ok(buf)
}

fn invert_phase(a: &Args, mut buf: AudioBuffer, _ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let which = a.choice("channel");
    per_channel(&mut buf, |c, ch| {
        let flip = match which.as_str() {
            "left" => c == 0,
            "right" => c == 1,
            _ => true,
        };
        if flip {
            ch.iter_mut().for_each(|v| *v = -*v);
        }
    });
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferraus_core::testing::{run, sine};

    fn spec(name: &str) -> OpSpec {
        ops().into_iter().find(|o| o.name == name).unwrap()
    }

    #[test]
    fn reverse_and_invert() {
        let x = AudioBuffer::from_mono(10, vec![1.0, 2.0, 3.0]);
        assert_eq!(run(&spec("reverse"), &[], x.clone()).unwrap().data[0], vec![3.0, 2.0, 1.0]);
        assert_eq!(run(&spec("invert-phase"), &[], x).unwrap().data[0], vec![-1.0, -2.0, -3.0]);
    }

    #[test]
    fn speed_changes_length() {
        let x = sine(48000, 440.0, 1.0, 0.5);
        let y = run(&spec("speed"), &[("factor", "2.0")], x.clone()).unwrap();
        assert_eq!(y.frames(), 24000);
        assert_eq!(run(&spec("speed"), &[], x.clone()).unwrap(), x);
    }

    #[test]
    fn dc_is_removed() {
        let mut x = sine(48000, 200.0, 1.0, 0.3);
        x.data[0].iter_mut().for_each(|v| *v += 0.2);
        for f in ["5Hz", "0Hz"] {
            let y = run(&spec("dc-remove"), &[("freq", f)], x.clone()).unwrap();
            let mean: f32 = y.data[0][24000..].iter().sum::<f32>() / 24000.0;
            assert!(mean.abs() < 1e-3, "{f}: {mean}");
        }
    }
}
