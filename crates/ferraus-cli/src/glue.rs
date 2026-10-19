//! Wiring between crates that must not depend on each other: plugin instruments for
//! `render` (ferraus-synth ↔ ferraus-plugins).

use ferraus_core::Result;
use ferraus_synth::{InstrumentSource, Scheduled};
use std::path::PathBuf;
use std::sync::OnceLock;

static SOUND_PATHS: OnceLock<Vec<PathBuf>> = OnceLock::new();

/// Remember the sound folders of this invocation (from --sound-path, env, config).
pub fn set_sound_paths(paths: Vec<PathBuf>) {
    let _ = SOUND_PATHS.set(paths);
}

fn render_plugin(
    src: &InstrumentSource,
    events: &[Scheduled],
    frames: usize,
    sample_rate: u32,
) -> Result<(Vec<f32>, Vec<f32>)> {
    let InstrumentSource::Plugin { name, preset, state } = src else {
        return Err(ferraus_core::Error::internal("render_plugin called for a non-plugin source"));
    };
    let paths = SOUND_PATHS.get().cloned().unwrap_or_default();
    let plugin = ferraus_plugins::api::PluginRef::resolve(name, &paths)?;
    let midi: Vec<ferraus_plugins::api::MidiEvent> = events
        .iter()
        .map(|e| match e.status {
            0x9 if e.data2 > 0 => {
                ferraus_plugins::api::MidiEvent::note_on(e.sample, e.channel, e.data1, e.data2 as f32 / 127.0)
            }
            0x8 | 0x9 => ferraus_plugins::api::MidiEvent::note_off(e.sample, e.channel, e.data1),
            st => ferraus_plugins::api::MidiEvent {
                frame: e.sample,
                kind: ferraus_plugins::api::MidiEventKind::Raw([(st << 4) | (e.channel & 0x0f), e.data1, e.data2]),
            },
        })
        .collect();
    let opts = ferraus_plugins::api::PluginOptions {
        preset: preset.clone(),
        state: match state {
            Some(p) => Some(ferraus_plugins::api::read_state_file(p)?),
            None => None,
        },
        ..Default::default()
    };
    let duration = frames as f64 / sample_rate as f64;
    let run = ferraus_plugins::api::render_instrument_with(&plugin, &midi, sample_rate, duration, &opts)?;
    let audio = run.audio.with_channels(2);
    let mut l = audio.data[0].clone();
    let mut r = audio.data[1].clone();
    l.resize(frames, 0.0);
    r.resize(frames, 0.0);
    Ok((l, r))
}

/// Register cross-crate hooks once per process.
pub fn install() {
    ferraus_synth::set_plugin_offline_renderer(render_plugin);
}
