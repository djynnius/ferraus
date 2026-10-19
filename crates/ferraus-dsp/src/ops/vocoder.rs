//! `vocoder`: classic channel vocoder or spectral cross-synthesis.

use super::{secs, sr};
use crate::vocoder::{
    parse_midi_notes, pitch_track_to_samples, render_midi, render_osc, track_pitch, vocode_classic, vocode_spectral,
    MidiNote, VocoderParams, Waveform,
};
use ferraus_core::prelude::*;
use std::path::Path;

pub fn ops() -> Vec<OpSpec> {
    vec![OpSpec::transform("vocoder", Category::Tone, "Vocoder: make a carrier speak or sing with the input's voice", vocoder)
        .about("The input is the modulator (usually a voice). --carrier is an audio file, or a built-in oscillator (saw, square, pulse, noise) at --note, following the voice's detected pitch (--follow), or playing a MIDI file (--midi part.mid). Bands are log-spaced over --range. With --unvoiced noise, sibilants (detected by their high-band energy ratio) are re-synthesized from a noise carrier so consonants stay intelligible. --mode classic is a filter-bank channel vocoder; --mode spectral is STFT cross-synthesis (smoother, suits high band counts). The output level matches the modulator.")
        .param(param("carrier", ParamKind::Str).default("saw").help("Audio file, or saw, square, pulse, noise or plugin:<name> (an installed instrument plugin playing --midi, --follow, or --note)"))
        .param(param("note", ParamKind::Freq).default("C3").help("Oscillator pitch when not following or playing MIDI"))
        .param(param("follow", ParamKind::Flag).help("Oscillator follows the modulator's detected pitch"))
        .param(param("midi", ParamKind::Path).help("MIDI file whose notes play the oscillator carrier (polyphonic)"))
        .param(param("bands", ParamKind::Int).default("24").range("8..64").help("Number of bands"))
        .param(param("range", ParamKind::Str).default("80Hz..8kHz").help("Frequency range of the bands (log spaced)"))
        .param(param("attack", ParamKind::Time).default("5ms").help("Band envelope attack"))
        .param(param("release", ParamKind::Time).default("30ms").help("Band envelope release"))
        .param(param("formant-shift", ParamKind::Pitch).default("0").help("Shift the vocal formants in semitones (+ brighter/smaller, - darker/bigger)"))
        .param(param("unvoiced", ParamKind::Choice).choices(&["noise", "off"]).default("noise").help("Re-synthesize sibilants from noise"))
        .param(param("mode", ParamKind::Choice).choices(&["classic", "spectral"]).default("classic").help("Filter-bank or STFT vocoder"))
        .example("ferraus vocoder vox.wav --carrier saw --note C3 -o robot.wav", "Classic robot voice on a C3 saw")
        .example("ferraus vocoder vox.wav --carrier saw --follow -o sung.wav", "Saw that follows the singer's pitch")
        .example("ferraus vocoder vox.wav --carrier square --midi chords.mid --bands 32 --mode spectral -o choir.wav", "Vocoded chords from a MIDI part")
        .example("ferraus vocoder vox.wav --carrier synth_pad.wav --formant-shift +3 -o pad_talk.wav", "Make a pad talk, formants up 3 semitones")]
}

fn carrier_signal(a: &Args, modulator: &[f32], rate: u32, ctx: &mut OpCtx) -> Result<Vec<f32>> {
    let spec = a.str_or("carrier", "saw");
    let n = modulator.len();
    let fs = rate as f64;
    if spec.trim().to_ascii_lowercase().starts_with("plugin:") {
        let name = spec.trim()[7..].trim().trim_matches('"').to_string();
        let Some(render) = crate::vocoder::plugin_carrier() else {
            return Err(Error::unsupported("plugin carriers need the plugin host, which this build does not include")
                .with_hint("use --carrier saw|square|pulse|noise or an audio file"));
        };
        // the notes the instrument plays: a MIDI file, the modulator's own melody, or one held note
        let notes: Vec<MidiNote> = if let Some(midi) = a.path("midi") {
            ctx.check_allowed(&midi)?;
            let bytes = std::fs::read(&midi)
                .map_err(|e| Error::input(format!("cannot read MIDI file {}: {e}", midi.display())))?;
            ctx.note_input(&midi);
            parse_midi_notes(&bytes).map_err(Error::input)?
        } else if a.flag("follow") {
            let hop = 256;
            let frames = track_pitch(modulator, fs, hop, 60.0, 1000.0);
            let mut notes: Vec<MidiNote> = Vec::new();
            for (k, f) in frames.iter().enumerate() {
                let t0 = k as f64 * hop as f64 / fs;
                let t1 = (k + 1) as f64 * hop as f64 / fs;
                match f {
                    Some(hz) => {
                        let key = (69.0 + 12.0 * (hz / 440.0).log2()).round().clamp(0.0, 127.0) as u8;
                        match notes.last_mut() {
                            Some(last) if last.key == key && (last.end_s - t0).abs() < 1e-6 => last.end_s = t1,
                            _ => notes.push(MidiNote { start_s: t0, end_s: t1, key, velocity: 100 }),
                        }
                    }
                    None => {}
                }
            }
            notes.retain(|n| n.end_s - n.start_s >= 0.03);
            notes
        } else {
            let hz = a.freq("note")?;
            let key = (69.0 + 12.0 * (hz / 440.0).log2()).round().clamp(0.0, 127.0) as u8;
            vec![MidiNote { start_s: 0.0, end_s: n as f64 / fs, key, velocity: 100 }]
        };
        ctx.note("carrier", format!("plugin:{name}"));
        ctx.note("carrier_notes", notes.len());
        ctx.deterministic = Some(false);
        let mut out = render(&name, &notes, n, rate)?;
        out.resize(n, 0.0);
        return Ok(out);
    }
    let as_osc = Waveform::parse(&spec).filter(|_| !Path::new(&spec).exists());
    match as_osc {
        Some(wave) => {
            if let Some(midi) = a.path("midi") {
                ctx.check_allowed(&midi)?;
                let bytes = std::fs::read(&midi)
                    .map_err(|e| Error::input(format!("cannot read MIDI file {}: {e}", midi.display())))?;
                ctx.note_input(&midi);
                let notes = parse_midi_notes(&bytes).map_err(Error::input)?;
                if notes.is_empty() {
                    ctx.warn("midi_empty", "the MIDI file has no notes; the carrier is silent");
                }
                ctx.note("midi_notes", notes.len());
                Ok(render_midi(wave, &notes, n, fs, ctx.seed()))
            } else if a.flag("follow") {
                let hop = 256;
                let frames = track_pitch(modulator, fs, hop, 60.0, 1000.0);
                let voiced = frames.iter().filter(|f| f.is_some()).count();
                ctx.metric("voiced_fraction", (voiced as f64 / frames.len().max(1) as f64 * 100.0).round() / 100.0);
                let track = pitch_track_to_samples(&frames, hop, n, a.freq("note")?);
                Ok(render_osc(wave, &track, fs, ctx.seed()))
            } else {
                let f = a.freq("note")?;
                Ok(render_osc(wave, &vec![f; n], fs, ctx.seed()))
            }
        }
        None => {
            if a.has("midi") {
                ctx.warn(
                    "midi_ignored",
                    "--midi only applies to oscillator carriers; it is ignored for an audio carrier",
                );
            }
            let mut c = ctx.read_audio_path(Path::new(&spec))?;
            if c.sample_rate != rate {
                c = ferraus_core::resample::resample(&c, rate, ferraus_core::resample::Quality::High);
            }
            let m = c.mono_mix();
            if m.is_empty() {
                return Err(Error::input("the carrier file is empty"));
            }
            if m.len() < n {
                ctx.warn("carrier_looped", "the carrier is shorter than the input and is looped");
            }
            Ok((0..n).map(|i| m[i % m.len()]).collect())
        }
    }
}

fn vocoder(a: &Args, buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let rate = sr(&buf);
    let bands = a.usize("bands")?;
    if !(8..=64).contains(&bands) {
        return Err(Error::usage("--bands must be between 8 and 64"));
    }
    let (lo, hi) = a.freq_range("range")?;
    if !(lo > 0.0 && hi > lo * 2.0) {
        return Err(Error::usage("--range must be low..high with high at least an octave above low, e.g. 80Hz..8kHz"));
    }
    let p = VocoderParams {
        bands,
        lo,
        hi: hi.min(rate * 0.45),
        attack_s: secs(a, "attack")?.max(1e-4),
        release_s: secs(a, "release")?.max(1e-4),
        formant_shift: a.semitones("formant-shift")?.clamp(-24.0, 24.0),
        unvoiced: a.choice("unvoiced") != "off",
        seed: ctx.seed(),
    };
    let modulator = buf.mono_mix();
    let carrier = carrier_signal(a, &modulator, buf.sample_rate, ctx)?;
    let out = if a.choice("mode") == "spectral" {
        vocode_spectral(&modulator, &carrier, rate, &p)
    } else {
        vocode_classic(&modulator, &carrier, rate, &p)
    };
    Ok(AudioBuffer::from_planar(buf.sample_rate, vec![out; buf.channels()]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferraus_core::testing::{noise, run, sine};

    fn spec() -> OpSpec {
        ops().remove(0)
    }

    fn high_band_energy(x: &[f32], rate: f64) -> f64 {
        let mut y = x.to_vec();
        crate::biquad::Cascade::butterworth_highpass(4, 6000.0, rate).process_block(&mut y);
        y.iter().map(|v| (*v as f64).powi(2)).sum()
    }

    /// A vowel-like buzz followed by an "s"-like noise burst.
    fn speechish(rate: u32) -> AudioBuffer {
        let vowel = sine(rate, 150.0, 0.3, 0.3);
        let mut hiss = noise(rate, 0.3, 0.3, 7);
        crate::biquad::Cascade::butterworth_highpass(4, 4000.0, rate as f64).process_block(&mut hiss.data[0]);
        let mut v = vowel.data[0].clone();
        v.extend_from_slice(&hiss.data[0]);
        AudioBuffer::from_mono(rate, v)
    }

    #[test]
    fn unvoiced_noise_raises_sibilant_highs() {
        let rate = 32000;
        let x = speechish(rate);
        for mode in ["classic", "spectral"] {
            let on = run(&spec(), &[("mode", mode)], x.clone()).unwrap();
            let off = run(&spec(), &[("mode", mode), ("unvoiced", "off")], x.clone()).unwrap();
            let s = rate as usize * 3 / 10;
            let e_on = high_band_energy(&on.data[0][s..], rate as f64);
            let e_off = high_band_energy(&off.data[0][s..], rate as f64);
            assert!(e_on > e_off * 1.5, "{mode}: {e_on} vs {e_off}");
        }
    }

    #[test]
    fn plugin_carrier_without_a_host_is_unsupported() {
        if crate::vocoder::plugin_carrier().is_some() {
            return;
        }
        let e = run(&spec(), &[("carrier", "plugin:Surge XT")], sine(16000, 200.0, 0.1, 0.3)).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Unsupported);
        assert!(e.message.contains("plugin host"));
    }

    #[test]
    fn follow_and_midi_carriers() {
        let x = sine(16000, 220.0, 0.5, 0.3);
        let y = run(&spec(), &[("follow", "true"), ("bands", "16")], x.clone()).unwrap();
        assert_eq!(y.frames(), x.frames());
        assert!(y.rms() > 0.05);
        // a tiny MIDI file with one A3 note
        let dir = ferraus_core::testing::TempDir::new("vocoder-midi");
        let path = dir.path("a.mid");
        std::fs::write(&path, one_note_midi(57)).unwrap();
        let notes = parse_midi_notes(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].key, 57);
        assert!((notes[0].end_s - 0.5).abs() < 1e-6);
        let y = run(&spec(), &[("midi", path.to_str().unwrap())], x).unwrap();
        assert!(y.rms() > 0.05);
    }

    fn one_note_midi(key: u8) -> Vec<u8> {
        use midly::{num::*, Format, Header, MetaMessage, MidiMessage, Smf, Timing, TrackEvent, TrackEventKind};
        let mut smf = Smf::new(Header::new(Format::SingleTrack, Timing::Metrical(u15::new(480))));
        smf.tracks.push(vec![
            TrackEvent { delta: u28::new(0), kind: TrackEventKind::Meta(MetaMessage::Tempo(u24::new(500_000))) },
            TrackEvent {
                delta: u28::new(0),
                kind: TrackEventKind::Midi {
                    channel: u4::new(0),
                    message: MidiMessage::NoteOn { key: u7::new(key), vel: u7::new(100) },
                },
            },
            TrackEvent {
                delta: u28::new(480),
                kind: TrackEventKind::Midi {
                    channel: u4::new(0),
                    message: MidiMessage::NoteOff { key: u7::new(key), vel: u7::new(0) },
                },
            },
            TrackEvent { delta: u28::new(0), kind: TrackEventKind::Meta(MetaMessage::EndOfTrack) },
        ]);
        let mut out = Vec::new();
        smf.write_std(&mut out).unwrap();
        out
    }
}
