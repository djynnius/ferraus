//! SoundFont (SF2) rendering through the pure-Rust `rustysynth` synthesizer (MIT).
//!
//! `oxisynth` (named in the PRD) is LGPL-2.1, which conflicts with the project's
//! permissive-only dependency policy, so `rustysynth` is used instead. Its internal block
//! size is set to 8 frames so events land within 8 samples (< 0.2 ms at 48 kHz) of their
//! scheduled time; reverb and chorus are off so the render stays dry (use `--fx`).

use crate::render::Engine;
use ferraus_core::{Error, ErrorKind, Result};
use rustysynth::{SoundFont, Synthesizer, SynthesizerSettings};
use std::path::Path;
use std::sync::Arc;

pub struct Sf2Engine {
    synth: Synthesizer,
}

/// Preset names and numbers in a SoundFont: `(bank, program, name)`.
pub fn list_presets(path: &Path) -> Result<Vec<(u16, u8, String)>> {
    let sf = load(path)?;
    let mut v: Vec<(u16, u8, String)> = sf
        .get_presets()
        .iter()
        .map(|p| (p.get_bank_number() as u16, p.get_patch_number() as u8, p.get_name().to_string()))
        .collect();
    v.sort();
    Ok(v)
}

fn load(path: &Path) -> Result<SoundFont> {
    let mut f = std::fs::File::open(path).map_err(|e| {
        Error::new(ErrorKind::InputMissing, "sound_not_found", format!("cannot open SoundFont {}: {e}", path.display()))
            .with_hint("ferraus sounds list")
    })?;
    SoundFont::new(&mut f)
        .map_err(|e| Error::unsupported(format!("{} is not a usable SoundFont: {e:?}", path.display())))
}

impl Sf2Engine {
    pub fn open(path: &Path, sample_rate: u32) -> Result<Sf2Engine> {
        let sf = Arc::new(load(path)?);
        let mut settings = SynthesizerSettings::new(sample_rate as i32);
        settings.block_size = 8;
        settings.maximum_polyphony = 256;
        settings.enable_reverb_and_chorus = false;
        let synth = Synthesizer::new(&sf, &settings)
            .map_err(|e| Error::unsupported(format!("cannot start the SoundFont synth: {e:?}")))?;
        Ok(Sf2Engine { synth })
    }
}

impl Engine for Sf2Engine {
    fn message(&mut self, status: u8, channel: u8, data1: u8, data2: u8) {
        self.synth.process_midi_message(channel as i32, (status as i32) << 4, data1 as i32, data2 as i32);
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        self.synth.render(left, right);
    }
}
