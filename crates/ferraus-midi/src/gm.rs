//! General MIDI tables: program names, families, drum-key names, controller names.

/// The 128 General MIDI Level 1 program names, indexed by program number (0-based).
pub const PROGRAM_NAMES: [&str; 128] = [
    "Acoustic Grand Piano",
    "Bright Acoustic Piano",
    "Electric Grand Piano",
    "Honky-tonk Piano",
    "Electric Piano 1",
    "Electric Piano 2",
    "Harpsichord",
    "Clavinet",
    "Celesta",
    "Glockenspiel",
    "Music Box",
    "Vibraphone",
    "Marimba",
    "Xylophone",
    "Tubular Bells",
    "Dulcimer",
    "Drawbar Organ",
    "Percussive Organ",
    "Rock Organ",
    "Church Organ",
    "Reed Organ",
    "Accordion",
    "Harmonica",
    "Tango Accordion",
    "Acoustic Guitar (nylon)",
    "Acoustic Guitar (steel)",
    "Electric Guitar (jazz)",
    "Electric Guitar (clean)",
    "Electric Guitar (muted)",
    "Overdriven Guitar",
    "Distortion Guitar",
    "Guitar Harmonics",
    "Acoustic Bass",
    "Electric Bass (finger)",
    "Electric Bass (pick)",
    "Fretless Bass",
    "Slap Bass 1",
    "Slap Bass 2",
    "Synth Bass 1",
    "Synth Bass 2",
    "Violin",
    "Viola",
    "Cello",
    "Contrabass",
    "Tremolo Strings",
    "Pizzicato Strings",
    "Orchestral Harp",
    "Timpani",
    "String Ensemble 1",
    "String Ensemble 2",
    "Synth Strings 1",
    "Synth Strings 2",
    "Choir Aahs",
    "Voice Oohs",
    "Synth Choir",
    "Orchestra Hit",
    "Trumpet",
    "Trombone",
    "Tuba",
    "Muted Trumpet",
    "French Horn",
    "Brass Section",
    "Synth Brass 1",
    "Synth Brass 2",
    "Soprano Sax",
    "Alto Sax",
    "Tenor Sax",
    "Baritone Sax",
    "Oboe",
    "English Horn",
    "Bassoon",
    "Clarinet",
    "Piccolo",
    "Flute",
    "Recorder",
    "Pan Flute",
    "Blown Bottle",
    "Shakuhachi",
    "Whistle",
    "Ocarina",
    "Lead 1 (square)",
    "Lead 2 (sawtooth)",
    "Lead 3 (calliope)",
    "Lead 4 (chiff)",
    "Lead 5 (charang)",
    "Lead 6 (voice)",
    "Lead 7 (fifths)",
    "Lead 8 (bass + lead)",
    "Pad 1 (new age)",
    "Pad 2 (warm)",
    "Pad 3 (polysynth)",
    "Pad 4 (choir)",
    "Pad 5 (bowed)",
    "Pad 6 (metallic)",
    "Pad 7 (halo)",
    "Pad 8 (sweep)",
    "FX 1 (rain)",
    "FX 2 (soundtrack)",
    "FX 3 (crystal)",
    "FX 4 (atmosphere)",
    "FX 5 (brightness)",
    "FX 6 (goblins)",
    "FX 7 (echoes)",
    "FX 8 (sci-fi)",
    "Sitar",
    "Banjo",
    "Shamisen",
    "Koto",
    "Kalimba",
    "Bagpipe",
    "Fiddle",
    "Shanai",
    "Tinkle Bell",
    "Agogo",
    "Steel Drums",
    "Woodblock",
    "Taiko Drum",
    "Melodic Tom",
    "Synth Drum",
    "Reverse Cymbal",
    "Guitar Fret Noise",
    "Breath Noise",
    "Seashore",
    "Bird Tweet",
    "Telephone Ring",
    "Helicopter",
    "Applause",
    "Gunshot",
];

/// The 16 GM families (8 programs each).
pub const FAMILY_NAMES: [&str; 16] = [
    "piano",
    "chromatic-percussion",
    "organ",
    "guitar",
    "bass",
    "strings",
    "ensemble",
    "brass",
    "reed",
    "pipe",
    "synth-lead",
    "synth-pad",
    "synth-effects",
    "ethnic",
    "percussive",
    "sound-effects",
];

/// GM family index (0..16) of a program.
pub fn family_of(program: u8) -> usize {
    (program as usize / 8).min(15)
}

/// Family name of a program.
pub fn family_name(program: u8) -> &'static str {
    FAMILY_NAMES[family_of(program)]
}

/// Program name (0-based program number).
pub fn program_name(program: u8) -> &'static str {
    PROGRAM_NAMES[(program as usize).min(127)]
}

/// Common short instrument words mapped to a representative GM program.
pub const SHORT_NAMES: &[(&str, u8)] = &[
    ("piano", 0),
    ("grand", 0),
    ("grand piano", 0),
    ("keys", 0),
    ("epiano", 4),
    ("electric piano", 4),
    ("rhodes", 4),
    ("harpsichord", 6),
    ("clav", 7),
    ("bells", 14),
    ("mallets", 11),
    ("vibes", 11),
    ("marimba", 12),
    ("organ", 16),
    ("accordion", 21),
    ("harmonica", 22),
    ("guitar", 24),
    ("nylon guitar", 24),
    ("acoustic guitar", 25),
    ("steel guitar", 25),
    ("electric guitar", 27),
    ("pluck", 24),
    ("bass", 33),
    ("electric bass", 33),
    ("acoustic bass", 32),
    ("synth bass", 38),
    ("violin", 40),
    ("viola", 41),
    ("cello", 42),
    ("contrabass", 43),
    ("harp", 46),
    ("strings", 48),
    ("string ensemble", 48),
    ("ensemble", 48),
    ("orchestra", 48),
    ("choir", 52),
    ("voice", 53),
    ("vocal", 52),
    ("trumpet", 56),
    ("trombone", 57),
    ("tuba", 58),
    ("horn", 60),
    ("french horn", 60),
    ("brass", 61),
    ("sax", 65),
    ("saxophone", 65),
    ("oboe", 68),
    ("bassoon", 70),
    ("clarinet", 71),
    ("winds", 71),
    ("woodwinds", 71),
    ("flute", 73),
    ("lead", 81),
    ("synth", 81),
    ("synth lead", 81),
    ("square", 80),
    ("saw", 81),
    ("pad", 89),
    ("synth pad", 89),
    ("warm pad", 89),
    ("sitar", 104),
    ("banjo", 105),
    ("kalimba", 108),
];

/// Names of GM percussion keys (channel 10), key → name.
pub fn drum_name(key: u8) -> Option<&'static str> {
    Some(match key {
        35 => "Acoustic Bass Drum",
        36 => "Bass Drum 1",
        37 => "Side Stick",
        38 => "Acoustic Snare",
        39 => "Hand Clap",
        40 => "Electric Snare",
        41 => "Low Floor Tom",
        42 => "Closed Hi-Hat",
        43 => "High Floor Tom",
        44 => "Pedal Hi-Hat",
        45 => "Low Tom",
        46 => "Open Hi-Hat",
        47 => "Low-Mid Tom",
        48 => "Hi-Mid Tom",
        49 => "Crash Cymbal 1",
        50 => "High Tom",
        51 => "Ride Cymbal 1",
        52 => "Chinese Cymbal",
        53 => "Ride Bell",
        54 => "Tambourine",
        55 => "Splash Cymbal",
        56 => "Cowbell",
        57 => "Crash Cymbal 2",
        58 => "Vibraslap",
        59 => "Ride Cymbal 2",
        60 => "Hi Bongo",
        61 => "Low Bongo",
        62 => "Mute Hi Conga",
        63 => "Open Hi Conga",
        64 => "Low Conga",
        65 => "High Timbale",
        66 => "Low Timbale",
        67 => "High Agogo",
        68 => "Low Agogo",
        69 => "Cabasa",
        70 => "Maracas",
        71 => "Short Whistle",
        72 => "Long Whistle",
        73 => "Short Guiro",
        74 => "Long Guiro",
        75 => "Claves",
        76 => "Hi Wood Block",
        77 => "Low Wood Block",
        78 => "Mute Cuica",
        79 => "Open Cuica",
        80 => "Mute Triangle",
        81 => "Open Triangle",
        _ => return None,
    })
}

/// Names of common controllers.
pub fn controller_name(cc: u8) -> Option<&'static str> {
    Some(match cc {
        0 => "bank_select",
        1 => "modulation",
        2 => "breath",
        6 => "data_entry",
        7 => "volume",
        10 => "pan",
        11 => "expression",
        32 => "bank_select_lsb",
        64 => "sustain",
        65 => "portamento",
        66 => "sostenuto",
        67 => "soft_pedal",
        71 => "resonance",
        72 => "release_time",
        73 => "attack_time",
        74 => "brightness",
        91 => "reverb_send",
        93 => "chorus_send",
        98 => "nrpn_lsb",
        99 => "nrpn_msb",
        100 => "rpn_lsb",
        101 => "rpn_msb",
        120 => "all_sound_off",
        121 => "reset_all_controllers",
        123 => "all_notes_off",
        _ => return None,
    })
}

/// Normalize a name for matching: lowercase, alphanumerics separated by single spaces.
pub fn normalize(s: &str) -> String {
    let mut out = String::new();
    let mut space = false;
    for c in s.chars() {
        if c.is_alphanumeric() {
            if space && !out.is_empty() {
                out.push(' ');
            }
            space = false;
            out.extend(c.to_lowercase());
        } else {
            space = true;
        }
    }
    out
}

/// Look up a GM program by exact program name (case- and punctuation-insensitive), short
/// name (`piano`, `strings`), `gm:<n>`, or a 1-based-free number (`48`). Returns the
/// program, or the candidate programs when a partial name matches several.
pub fn lookup_program(name: &str) -> std::result::Result<u8, Vec<u8>> {
    let t = name.trim();
    let num = t.strip_prefix("gm:").unwrap_or(t).trim();
    if let Ok(n) = num.parse::<u32>() {
        return if n < 128 { Ok(n as u8) } else { Err(vec![]) };
    }
    let n = normalize(num);
    if n.is_empty() {
        return Err(vec![]);
    }
    if let Some(p) = PROGRAM_NAMES.iter().position(|p| normalize(p) == n) {
        return Ok(p as u8);
    }
    if let Some((_, p)) = SHORT_NAMES.iter().find(|(s, _)| *s == n) {
        return Ok(*p);
    }
    let cands: Vec<u8> = PROGRAM_NAMES
        .iter()
        .enumerate()
        .filter(|(_, p)| {
            let pn = normalize(p);
            n.split(' ').all(|w| pn.split(' ').any(|x| x == w))
        })
        .map(|(i, _)| i as u8)
        .collect();
    if cands.len() == 1 {
        Ok(cands[0])
    } else {
        Err(cands)
    }
}

/// Key-signature name for `sharps` (-7..7) and mode.
pub fn key_name(sharps: i8, minor: bool) -> String {
    const MAJ: [&str; 15] = ["Cb", "Gb", "Db", "Ab", "Eb", "Bb", "F", "C", "G", "D", "A", "E", "B", "F#", "C#"];
    const MIN: [&str; 15] = ["Ab", "Eb", "Bb", "F", "C", "G", "D", "A", "E", "B", "F#", "C#", "G#", "D#", "A#"];
    let i = (sharps.clamp(-7, 7) + 7) as usize;
    if minor {
        format!("{} minor", MIN[i])
    } else {
        format!("{} major", MAJ[i])
    }
}

/// Parse `Eb major`, `F# minor`, `Am`, `C` into (sharps, minor).
pub fn parse_key_name(s: &str) -> Option<(i8, bool)> {
    let t = s.trim();
    let lower = t.to_ascii_lowercase();
    let (root, minor) = if let Some(r) = lower.strip_suffix(" minor") {
        (r.trim().to_string(), true)
    } else if let Some(r) = lower.strip_suffix(" major") {
        (r.trim().to_string(), false)
    } else if let Some(r) = lower.strip_suffix('m') {
        (r.trim().to_string(), true)
    } else {
        (lower.clone(), false)
    };
    for sh in -7i8..=7 {
        let name = key_name(sh, minor);
        let r = name.split(' ').next().unwrap().to_ascii_lowercase();
        if r == root {
            return Some((sh, minor));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookups() {
        assert_eq!(lookup_program("String Ensemble 1"), Ok(48));
        assert_eq!(lookup_program("string-ensemble 1"), Ok(48));
        assert_eq!(lookup_program("gm:48"), Ok(48));
        assert_eq!(lookup_program("strings"), Ok(48));
        assert_eq!(lookup_program("piano"), Ok(0));
        assert_eq!(lookup_program("Trumpet"), Ok(56));
        assert!(lookup_program("gm:200").is_err());
        // partial name matching several programs
        match lookup_program("electric") {
            Err(c) => assert!(c.len() > 3),
            Ok(_) => panic!("ambiguous"),
        }
        assert_eq!(family_name(48), "ensemble");
        assert_eq!(drum_name(38), Some("Acoustic Snare"));
    }

    #[test]
    fn keys() {
        assert_eq!(key_name(-3, false), "Eb major");
        assert_eq!(key_name(0, true), "A minor");
        assert_eq!(parse_key_name("Eb major"), Some((-3, false)));
        assert_eq!(parse_key_name("F# minor"), Some((3, true)));
        assert_eq!(parse_key_name("Am"), Some((0, true)));
        assert_eq!(parse_key_name("C"), Some((0, false)));
    }
}
