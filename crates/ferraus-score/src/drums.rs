//! General MIDI percussion: names and the standard drum-set staff map (percussion clef
//! read like treble: kick in the bottom space F4, snare in the third space C5, hi-hat
//! and cymbals as x noteheads on and above the top line).

use crate::model::Pitch;

/// How one GM drum is written.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DrumSpec {
    pub key: u8,
    pub name: &'static str,
    pub step: char,
    pub octave: i8,
    /// `None` = normal notehead.
    pub notehead: Option<&'static str>,
    /// Played by the feet (written in voice 2, stems down).
    pub foot: bool,
    /// Short code for text formats (`kick`, `snare`, `hh`).
    pub code: &'static str,
}

const fn d(
    key: u8,
    name: &'static str,
    step: char,
    octave: i8,
    notehead: Option<&'static str>,
    foot: bool,
    code: &'static str,
) -> DrumSpec {
    DrumSpec {
        key,
        name,
        step,
        octave,
        notehead,
        foot,
        code,
    }
}

/// The drum map. Keys not listed fall back to [`fallback`].
pub const DRUM_MAP: &[DrumSpec] = &[
    d(35, "Acoustic Bass Drum", 'F', 4, None, true, "kick"),
    d(36, "Bass Drum 1", 'F', 4, None, true, "kick"),
    d(37, "Side Stick", 'C', 5, Some("x"), false, "stick"),
    d(38, "Acoustic Snare", 'C', 5, None, false, "snare"),
    d(39, "Hand Clap", 'C', 5, Some("slash"), false, "clap"),
    d(40, "Electric Snare", 'C', 5, None, false, "snare"),
    d(41, "Low Floor Tom", 'G', 4, None, false, "tom-floor-low"),
    d(42, "Closed Hi-Hat", 'G', 5, Some("x"), false, "hh"),
    d(43, "High Floor Tom", 'A', 4, None, false, "tom-floor"),
    d(44, "Pedal Hi-Hat", 'D', 4, Some("x"), true, "hh-pedal"),
    d(45, "Low Tom", 'B', 4, None, false, "tom-low"),
    d(46, "Open Hi-Hat", 'G', 5, Some("circle-x"), false, "hh-open"),
    d(47, "Low-Mid Tom", 'D', 5, None, false, "tom-mid"),
    d(48, "Hi-Mid Tom", 'E', 5, None, false, "tom-high"),
    d(49, "Crash Cymbal 1", 'A', 5, Some("x"), false, "crash"),
    d(50, "High Tom", 'E', 5, None, false, "tom-high"),
    d(51, "Ride Cymbal 1", 'F', 5, Some("x"), false, "ride"),
    d(52, "Chinese Cymbal", 'B', 5, Some("x"), false, "china"),
    d(53, "Ride Bell", 'F', 5, Some("diamond"), false, "ride-bell"),
    d(54, "Tambourine", 'B', 5, Some("triangle"), false, "tambourine"),
    d(55, "Splash Cymbal", 'B', 5, Some("x"), false, "splash"),
    d(56, "Cowbell", 'E', 5, Some("triangle"), false, "cowbell"),
    d(57, "Crash Cymbal 2", 'B', 5, Some("x"), false, "crash2"),
    d(59, "Ride Cymbal 2", 'F', 5, Some("x"), false, "ride2"),
];

pub fn spec(key: u8) -> DrumSpec {
    DRUM_MAP
        .iter()
        .copied()
        .find(|s| s.key == key)
        .unwrap_or_else(|| fallback(key))
}

/// Unlisted percussion: x notehead on the third space with its GM name.
pub fn fallback(key: u8) -> DrumSpec {
    DrumSpec {
        key,
        name: gm_drum_name(key),
        step: 'C',
        octave: 5,
        notehead: Some("x"),
        foot: false,
        code: "perc",
    }
}

pub fn display_pitch(key: u8) -> Pitch {
    let s = spec(key);
    Pitch {
        step: s.step,
        alter: 0,
        octave: s.octave,
    }
}

/// Best GM key for a display position and notehead (when a file has no instrument ids).
pub fn key_for_display(p: Pitch, notehead: Option<&str>) -> u8 {
    DRUM_MAP
        .iter()
        .find(|s| s.step == p.step && s.octave == p.octave && s.notehead == notehead)
        .or_else(|| {
            DRUM_MAP
                .iter()
                .find(|s| s.step == p.step && s.octave == p.octave)
        })
        .map(|s| s.key)
        .unwrap_or(38)
}

/// Key for a drum name or code (`kick`, `snare`, `hh`, `Closed Hi-Hat`).
pub fn key_for_name(name: &str) -> Option<u8> {
    let n = name.trim().to_ascii_lowercase();
    DRUM_MAP
        .iter()
        .find(|s| s.code == n || s.name.to_ascii_lowercase() == n)
        .map(|s| s.key)
}

pub fn gm_drum_name(key: u8) -> &'static str {
    const NAMES: [&str; 47] = [
        "Acoustic Bass Drum",
        "Bass Drum 1",
        "Side Stick",
        "Acoustic Snare",
        "Hand Clap",
        "Electric Snare",
        "Low Floor Tom",
        "Closed Hi-Hat",
        "High Floor Tom",
        "Pedal Hi-Hat",
        "Low Tom",
        "Open Hi-Hat",
        "Low-Mid Tom",
        "Hi-Mid Tom",
        "Crash Cymbal 1",
        "High Tom",
        "Ride Cymbal 1",
        "Chinese Cymbal",
        "Ride Bell",
        "Tambourine",
        "Splash Cymbal",
        "Cowbell",
        "Crash Cymbal 2",
        "Vibraslap",
        "Ride Cymbal 2",
        "Hi Bongo",
        "Low Bongo",
        "Mute Hi Conga",
        "Open Hi Conga",
        "Low Conga",
        "High Timbale",
        "Low Timbale",
        "High Agogo",
        "Low Agogo",
        "Cabasa",
        "Maracas",
        "Short Whistle",
        "Long Whistle",
        "Short Guiro",
        "Long Guiro",
        "Claves",
        "Hi Wood Block",
        "Low Wood Block",
        "Mute Cuica",
        "Open Cuica",
        "Mute Triangle",
        "Open Triangle",
    ];
    if (35..=81).contains(&key) {
        NAMES[(key - 35) as usize]
    } else {
        "Percussion"
    }
}

/// General MIDI program names (0-based), used for part names and `--instrument-map`.
pub const GM_PROGRAMS: [&str; 128] = [
    "Acoustic Grand Piano", "Bright Acoustic Piano", "Electric Grand Piano", "Honky-tonk Piano",
    "Electric Piano 1", "Electric Piano 2", "Harpsichord", "Clavinet", "Celesta", "Glockenspiel",
    "Music Box", "Vibraphone", "Marimba", "Xylophone", "Tubular Bells", "Dulcimer", "Drawbar Organ",
    "Percussive Organ", "Rock Organ", "Church Organ", "Reed Organ", "Accordion", "Harmonica",
    "Tango Accordion", "Acoustic Guitar (nylon)", "Acoustic Guitar (steel)", "Electric Guitar (jazz)",
    "Electric Guitar (clean)", "Electric Guitar (muted)", "Overdriven Guitar", "Distortion Guitar",
    "Guitar Harmonics", "Acoustic Bass", "Electric Bass (finger)", "Electric Bass (pick)",
    "Fretless Bass", "Slap Bass 1", "Slap Bass 2", "Synth Bass 1", "Synth Bass 2", "Violin", "Viola",
    "Cello", "Contrabass", "Tremolo Strings", "Pizzicato Strings", "Orchestral Harp", "Timpani",
    "String Ensemble 1", "String Ensemble 2", "Synth Strings 1", "Synth Strings 2", "Choir Aahs",
    "Voice Oohs", "Synth Voice", "Orchestra Hit", "Trumpet", "Trombone", "Tuba", "Muted Trumpet",
    "French Horn", "Brass Section", "Synth Brass 1", "Synth Brass 2", "Soprano Sax", "Alto Sax",
    "Tenor Sax", "Baritone Sax", "Oboe", "English Horn", "Bassoon", "Clarinet", "Piccolo", "Flute",
    "Recorder", "Pan Flute", "Blown Bottle", "Shakuhachi", "Whistle", "Ocarina", "Lead 1 (square)",
    "Lead 2 (sawtooth)", "Lead 3 (calliope)", "Lead 4 (chiff)", "Lead 5 (charang)", "Lead 6 (voice)",
    "Lead 7 (fifths)", "Lead 8 (bass + lead)", "Pad 1 (new age)", "Pad 2 (warm)", "Pad 3 (polysynth)",
    "Pad 4 (choir)", "Pad 5 (bowed)", "Pad 6 (metallic)", "Pad 7 (halo)", "Pad 8 (sweep)",
    "FX 1 (rain)", "FX 2 (soundtrack)", "FX 3 (crystal)", "FX 4 (atmosphere)", "FX 5 (brightness)",
    "FX 6 (goblins)", "FX 7 (echoes)", "FX 8 (sci-fi)", "Sitar", "Banjo", "Shamisen", "Koto",
    "Kalimba", "Bagpipe", "Fiddle", "Shanai", "Tinkle Bell", "Agogo", "Steel Drums", "Woodblock",
    "Taiko Drum", "Melodic Tom", "Synth Drum", "Reverse Cymbal", "Guitar Fret Noise", "Breath Noise",
    "Seashore", "Bird Tweet", "Telephone Ring", "Helicopter", "Applause", "Gunshot",
];

/// Program for a common instrument word (`piano`, `strings`, `guitar`, `bass`, `flute`) or a
/// number `0..=127`.
pub fn program_for(name: &str) -> Option<u8> {
    let n = name.trim().to_ascii_lowercase();
    if let Ok(p) = n.parse::<u8>() {
        return (p < 128).then_some(p);
    }
    let alias = match n.as_str() {
        "piano" | "grand" | "grand-piano" | "keys" => 0,
        "epiano" | "electric-piano" | "rhodes" => 4,
        "harpsichord" => 6,
        "vibes" | "vibraphone" => 11,
        "marimba" => 12,
        "organ" => 19,
        "accordion" => 21,
        "harmonica" => 22,
        "guitar" | "nylon-guitar" | "classical-guitar" => 24,
        "steel-guitar" | "acoustic-guitar" => 25,
        "electric-guitar" | "clean-guitar" => 27,
        "distortion-guitar" => 30,
        "bass" | "electric-bass" => 33,
        "acoustic-bass" | "upright-bass" => 32,
        "violin" => 40,
        "viola" => 41,
        "cello" => 42,
        "contrabass" | "double-bass" => 43,
        "harp" => 46,
        "timpani" => 47,
        "strings" | "string-ensemble" => 48,
        "choir" | "voice" | "vocals" => 52,
        "trumpet" => 56,
        "trombone" => 57,
        "tuba" => 58,
        "horn" | "french-horn" => 60,
        "brass" => 61,
        "soprano-sax" => 64,
        "sax" | "alto-sax" => 65,
        "tenor-sax" => 66,
        "oboe" => 68,
        "bassoon" => 70,
        "clarinet" => 71,
        "piccolo" => 72,
        "flute" => 73,
        "recorder" => 74,
        "synth" | "lead" => 80,
        "pad" => 88,
        _ => {
            return GM_PROGRAMS
                .iter()
                .position(|p| p.to_ascii_lowercase() == n)
                .map(|p| p as u8)
        }
    };
    Some(alias)
}
