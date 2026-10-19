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
fn resolve_external(name: &str) -> Result<Option<String>> {
    let paths = SOUND_PATHS.get().cloned().unwrap_or_default();
    Ok(ferraus_plugins::resolve::resolve_instrument(name, &paths)?.map(|r| r.sound.source()))
}

/// A plugin instrument as the vocoder's carrier.
fn vocoder_plugin_carrier(
    name: &str,
    notes: &[ferraus_dsp::vocoder::MidiNote],
    frames: usize,
    sample_rate: u32,
) -> Result<Vec<f32>> {
    let paths = SOUND_PATHS.get().cloned().unwrap_or_default();
    let plugin = ferraus_plugins::api::PluginRef::resolve(name, &paths)?;
    let sr = sample_rate as f64;
    let mut events = Vec::new();
    for n in notes {
        let on = (n.start_s * sr).round() as u64;
        let off = ((n.end_s * sr).round() as u64).max(on + 1);
        events.push(ferraus_plugins::api::MidiEvent::note_on(on, 0, n.key, n.velocity as f32 / 127.0));
        events.push(ferraus_plugins::api::MidiEvent::note_off(off, 0, n.key));
    }
    events.sort_by_key(|e| e.frame);
    let run = ferraus_plugins::api::render_instrument_with(
        &plugin,
        &events,
        sample_rate,
        frames as f64 / sr,
        &Default::default(),
    )?;
    let mut mono = run.audio.mono_mix();
    mono.resize(frames, 0.0);
    Ok(mono)
}

pub fn install() {
    ferraus_dsp::vocoder::set_plugin_carrier(vocoder_plugin_carrier);
    ferraus_synth::set_plugin_offline_renderer(render_plugin);
    ferraus_synth::set_external_resolver(resolve_external);
    install_transcription();
}

/// Basic Pitch (polyphonic transcription) through ferraus-ml's ONNX runtime.
struct BasicPitch {
    model: std::sync::Mutex<Option<(String, String, String)>>,
}

impl ferraus_midi::transcribe::poly::PolyTranscriber for BasicPitch {
    fn name(&self) -> &str {
        "basic-pitch"
    }

    fn model(&self) -> Option<(String, String, String)> {
        self.model.lock().ok().and_then(|m| m.clone())
    }

    fn activations(&self, samples: &[f32], sr: u32) -> Result<ferraus_midi::transcribe::poly::PolyActivations> {
        use ferraus_midi::transcribe::poly::{Activation, PolyActivations};
        let mono = ferraus_core::resample::resample_channel(
            samples,
            sr,
            ferraus_ml::basic_pitch::BASIC_PITCH_RATE,
            ferraus_core::resample::Quality::High,
        );
        // the download policy comes from the environment the CLI set up (-y, config, --offline)
        let g = ferraus_core::Globals {
            yes: std::env::var("FERRAUS_AUTO_DOWNLOAD").map(|v| v == "1").unwrap_or(false),
            offline: std::env::var("FERRAUS_OFFLINE").map(|v| v == "1").unwrap_or(false),
            ..Default::default()
        };
        let mut ctx = ferraus_core::OpCtx::new(
            "transcribe",
            vec![],
            g,
            &ferraus_core::testing::SIMPLE_IO,
            &ferraus_core::testing::NO_PROGRESS,
        );
        let (frames, onsets, contours) = ferraus_ml::basic_pitch_activations(&mut ctx, &mono)?;
        if let Some(m) = ctx.models.first() {
            if let Ok(mut slot) = self.model.lock() {
                *slot = Some((m.name.clone(), m.version.clone(), m.sha256.clone()));
            }
        }
        Ok(PolyActivations {
            notes: Activation::from_rows(&frames)?,
            onsets: Activation::from_rows(&onsets)?,
            contours: Some(Activation::from_rows(&contours)?),
            windowed: true,
        })
    }
}

fn basic_pitch_factory() -> Result<Box<dyn ferraus_midi::transcribe::poly::PolyTranscriber>> {
    Ok(Box::new(BasicPitch { model: std::sync::Mutex::new(None) }))
}

/// Register the model-backed transcriber (called from [`install`]).
pub fn install_transcription() {
    ferraus_midi::transcribe::poly::register_poly_transcriber(basic_pitch_factory);
}
