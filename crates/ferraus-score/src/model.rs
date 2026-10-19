//! The agent-facing score model (`score to-json` / `score from-json`).
//!
//! Hierarchy: [`Score`] → [`Part`] → [`Staff`] → [`Measure`] → [`Voice`] → [`Event`].
//! Durations are integers in `divisions` per quarter note (MusicXML style), so every
//! duration is an exact rational number of quarter notes. Offsets of chord symbols,
//! tempo marks, and directions are in divisions from the start of the measure.

use serde::{Deserialize, Serialize};

/// Version of the score JSON schema.
pub const SCORE_JSON_VERSION: u32 = 1;

fn is_false(b: &bool) -> bool {
    !*b
}
fn is_zero_u8(n: &u8) -> bool {
    *n == 0
}
fn is_zero_i8(n: &i8) -> bool {
    *n == 0
}
fn default_true() -> bool {
    true
}
fn is_true(b: &bool) -> bool {
    *b
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Score {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub composer: Option<String>,
    /// Divisions per quarter note for every duration and offset in the score.
    pub divisions: u32,
    pub parts: Vec<Part>,
    /// Measures whose confidence is below the threshold, with reasons (computed).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub low_confidence: Vec<LowConfidence>,
}

fn default_version() -> u32 {
    SCORE_JSON_VERSION
}

impl Default for Score {
    fn default() -> Self {
        Score {
            version: SCORE_JSON_VERSION,
            title: None,
            composer: None,
            divisions: 48,
            parts: Vec::new(),
            low_confidence: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LowConfidence {
    pub part: String,
    pub measure: u32,
    pub confidence: f32,
    pub reasons: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Part {
    /// MusicXML part id (`P1`).
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abbreviation: Option<String>,
    /// General MIDI program 0..=127.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program: Option<u8>,
    /// MIDI channel 1..=16 (10 is percussion).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<u8>,
    /// Unpitched percussion part (drum score).
    #[serde(default, skip_serializing_if = "is_false")]
    pub percussion: bool,
    pub staves: Vec<Staff>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Staff {
    /// 1-based staff number within the part.
    pub number: u32,
    pub measures: Vec<Measure>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Measure {
    /// Displayed measure number (a pickup is measure 0).
    pub number: u32,
    /// Pickup (anacrusis) or other incomplete measure that is not counted.
    #[serde(default, skip_serializing_if = "is_false")]
    pub implicit: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<KeySig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<TimeSig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clef: Option<Clef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tempo: Vec<TempoMark>,
    /// Chord symbols.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub harmonies: Vec<Harmony>,
    /// Dynamics, words, and other directions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub directions: Vec<Direction>,
    pub voices: Vec<Voice>,
    /// Forward repeat at the start of the measure.
    #[serde(default, skip_serializing_if = "is_false")]
    pub repeat_start: bool,
    /// Backward repeat at the end of the measure; the value is the total number of times
    /// the section is played (2 for a plain repeat).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat_end: Option<u32>,
    /// Volta bracket covering this measure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ending: Option<Ending>,
    /// Right barline style when not a plain barline (`light-heavy`, `light-light`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub barline: Option<String>,
    /// Aggregate confidence 0..1 (transcriptions and OMR).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub confidence_reasons: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Major,
    Minor,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeySig {
    /// Sharps (positive) or flats (negative), -7..=7.
    pub fifths: i8,
    #[serde(default)]
    pub mode: Mode,
}

impl KeySig {
    pub fn major(fifths: i8) -> KeySig {
        KeySig { fifths, mode: Mode::Major }
    }
    pub fn minor(fifths: i8) -> KeySig {
        KeySig { fifths, mode: Mode::Minor }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeSig {
    pub beats: u32,
    pub beat_type: u32,
}

impl TimeSig {
    pub fn new(beats: u32, beat_type: u32) -> TimeSig {
        TimeSig { beats, beat_type }
    }
    /// Measure length in divisions.
    pub fn measure_divisions(&self, divisions: u32) -> u32 {
        self.beats * divisions * 4 / self.beat_type.max(1)
    }
    /// Compound meter (6/8, 9/8, 12/8).
    pub fn is_compound(&self) -> bool {
        self.beat_type >= 8 && self.beats % 3 == 0 && self.beats > 3
    }
    /// Length of one counted beat in divisions (dotted quarter in 6/8).
    pub fn beat_divisions(&self, divisions: u32) -> u32 {
        let unit = divisions * 4 / self.beat_type.max(1);
        if self.is_compound() {
            unit * 3
        } else {
            unit
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Clef {
    /// `G`, `F`, `C`, `percussion`, `TAB`.
    pub sign: String,
    pub line: u8,
    #[serde(default, skip_serializing_if = "is_zero_i8")]
    pub octave_change: i8,
}

impl Clef {
    pub fn treble() -> Clef {
        Clef { sign: "G".into(), line: 2, octave_change: 0 }
    }
    pub fn bass() -> Clef {
        Clef { sign: "F".into(), line: 4, octave_change: 0 }
    }
    pub fn percussion() -> Clef {
        Clef { sign: "percussion".into(), line: 3, octave_change: 0 }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TempoMark {
    #[serde(default)]
    pub offset: u32,
    /// Quarter notes per minute.
    pub bpm: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// A pitch class spelled with a letter and alteration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Spelled {
    /// `C`..`B`.
    pub step: char,
    #[serde(default, skip_serializing_if = "is_zero_i8")]
    pub alter: i8,
}

impl Spelled {
    pub fn pc(&self) -> i32 {
        (crate::pitch::step_pc(self.step) + self.alter as i32).rem_euclid(12)
    }
    pub fn name(&self) -> String {
        let acc = match self.alter {
            -2 => "bb",
            -1 => "b",
            1 => "#",
            2 => "##",
            _ => "",
        };
        format!("{}{}", self.step, acc)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Harmony {
    #[serde(default)]
    pub offset: u32,
    pub root: Spelled,
    /// `maj`, `min`, `7`, `maj7`, `m7`, `dim`, `dim7`, `m7b5`, `aug`, `sus2`, `sus4`,
    /// `add9`, `6`, `m6`, `9`.
    pub quality: String,
    /// Bass note of a slash chord (`G/B`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bass: Option<Spelled>,
    /// Display text (`Am7`, `G/B`).
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame: Option<GuitarFrame>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
}

/// A guitar fretboard diagram (MusicXML `<frame>`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuitarFrame {
    pub strings: u8,
    pub frets: u8,
    /// Lowest fret shown (1 for open position).
    pub first_fret: u8,
    /// Fret per string from low E (string 6) to high E (string 1); `None` = muted.
    pub fret_by_string: Vec<Option<u8>>,
    /// Shape name when played with a capo (`G shape`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capo: Option<u8>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Direction {
    #[serde(default)]
    pub offset: u32,
    /// `dynamic` (value `p`, `mf`, `ff`...), `words`, `wedge` (`crescendo`, `diminuendo`, `stop`).
    pub kind: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placement: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Ending {
    /// `1`, `2`, `1, 2`.
    pub number: String,
    /// Position of this measure in the bracket: `start`, `continue`, `stop` (closed end),
    /// `discontinue` (open end), or `start-stop` / `start-discontinue` for a one-measure
    /// ending.
    #[serde(rename = "type")]
    pub kind: String,
}

impl Ending {
    pub fn numbers(&self) -> Vec<u32> {
        self.number.split([',', ' ']).filter_map(|s| s.trim().parse().ok()).collect()
    }
    pub fn starts(&self) -> bool {
        self.kind.starts_with("start")
    }
    pub fn stops(&self) -> bool {
        self.kind.ends_with("stop") || self.kind.ends_with("discontinue")
    }
    /// The MusicXML type for the right barline (`stop` or `discontinue`).
    pub fn stop_type(&self) -> &'static str {
        if self.kind.ends_with("discontinue") {
            "discontinue"
        } else {
            "stop"
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Voice {
    /// 1-based voice number within the staff.
    pub number: u32,
    pub events: Vec<Event>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tuplet {
    /// Notes played (3 in a triplet).
    pub actual: u32,
    /// In the time of (2 in a triplet).
    pub normal: u32,
    #[serde(default, skip_serializing_if = "is_false")]
    pub start: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub stop: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Lyric {
    pub text: String,
    /// `single`, `begin`, `middle`, `end`.
    #[serde(default = "single")]
    pub syllabic: String,
    #[serde(default = "one")]
    pub verse: u32,
}

fn single() -> String {
    "single".into()
}
fn one() -> u32 {
    1
}

/// A rest, a single note, or a chord (several notes sharing one duration).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// Duration in divisions (0 for grace notes).
    pub duration: u32,
    #[serde(default, skip_serializing_if = "is_false")]
    pub rest: bool,
    /// One note, or several for a chord. Empty for a rest.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<Note>,
    /// Written type (`quarter`, `eighth`...); derived from the duration when absent.
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "type")]
    pub note_type: Option<String>,
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub dots: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tuplet: Option<Tuplet>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub grace: bool,
    /// Whole-measure rest.
    #[serde(default, skip_serializing_if = "is_false")]
    pub measure_rest: bool,
    /// Printed (false for invisible spacer rests).
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub visible: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lyrics: Vec<Lyric>,
    /// Tonic sol-fa syllable (`d`, `s,`, `m'`, `fe`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub solfa: Option<String>,
    /// `staccato`, `tenuto`, `accent`, `marcato`, `staccatissimo`, `legato`, `fermata`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub articulations: Vec<String>,
    /// Dynamic marking attached to this event (`p`, `mf`, `ff`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dynamic: Option<String>,
    /// Slur `start` / `stop`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub slurs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence_reason: Option<String>,
}

impl Event {
    pub fn rest(duration: u32) -> Event {
        Event { duration, rest: true, ..Event::empty() }
    }
    pub fn notes(duration: u32, notes: Vec<Note>) -> Event {
        Event { duration, notes, ..Event::empty() }
    }
    pub fn empty() -> Event {
        Event {
            duration: 0,
            rest: false,
            notes: Vec::new(),
            note_type: None,
            dots: 0,
            tuplet: None,
            grace: false,
            measure_rest: false,
            visible: true,
            lyrics: Vec::new(),
            solfa: None,
            articulations: Vec::new(),
            dynamic: None,
            slurs: Vec::new(),
            confidence: None,
            confidence_reason: None,
        }
    }
    pub fn is_rest(&self) -> bool {
        self.rest || self.notes.is_empty()
    }
}

/// A written pitch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pitch {
    pub step: char,
    #[serde(default, skip_serializing_if = "is_zero_i8")]
    pub alter: i8,
    pub octave: i8,
}

impl Pitch {
    pub fn midi(&self) -> i32 {
        (self.octave as i32 + 1) * 12 + crate::pitch::step_pc(self.step) + self.alter as i32
    }
    pub fn name(&self) -> String {
        format!("{}{}", Spelled { step: self.step, alter: self.alter }.name(), self.octave)
    }
    pub fn parse(s: &str) -> Option<Pitch> {
        let s = s.trim();
        let mut chars = s.chars();
        let step = chars.next()?.to_ascii_uppercase();
        if !('A'..='G').contains(&step) {
            return None;
        }
        let rest: String = chars.collect();
        let split = rest.char_indices().find(|(_, c)| c.is_ascii_digit() || *c == '-').map(|(i, _)| i)?;
        let (acc, oct) = rest.split_at(split);
        let alter = match acc {
            "" | "n" => 0,
            "#" | "s" => 1,
            "##" | "x" => 2,
            "b" | "f" => -1,
            "bb" => -2,
            _ => return None,
        };
        Some(Pitch { step, alter, octave: oct.parse().ok()? })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Note {
    /// Written pitch; for unpitched percussion this is the display position on the staff.
    pub pitch: Pitch,
    /// Sounding MIDI number (the GM drum key for percussion).
    pub midi: i32,
    #[serde(default, skip_serializing_if = "is_false")]
    pub tie_start: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub tie_stop: bool,
    /// `x`, `circle-x`, `diamond`, `triangle`, `slash`; normal when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notehead: Option<String>,
    /// Unpitched percussion note.
    #[serde(default, skip_serializing_if = "is_false")]
    pub unpitched: bool,
    /// Percussion instrument name (`Bass Drum 1`, `Closed Hi-Hat`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instrument: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub velocity: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
}

impl Note {
    pub fn new(pitch: Pitch) -> Note {
        Note {
            midi: pitch.midi(),
            pitch,
            tie_start: false,
            tie_stop: false,
            notehead: None,
            unpitched: false,
            instrument: None,
            velocity: None,
            confidence: None,
        }
    }
}

impl Score {
    /// Iterate (part index, staff index, measure index, &Measure).
    pub fn measure_count(&self) -> usize {
        self.parts.iter().flat_map(|p| p.staves.iter().map(|s| s.measures.len())).max().unwrap_or(0)
    }

    /// The effective time signature at measure `mi` of a staff.
    pub fn time_at(staff: &Staff, mi: usize) -> TimeSig {
        staff.measures[..=mi.min(staff.measures.len().saturating_sub(1))]
            .iter()
            .rev()
            .find_map(|m| m.time)
            .unwrap_or(TimeSig::new(4, 4))
    }

    /// The effective key at measure `mi` of a staff.
    pub fn key_at(staff: &Staff, mi: usize) -> KeySig {
        staff.measures[..=mi.min(staff.measures.len().saturating_sub(1))]
            .iter()
            .rev()
            .find_map(|m| m.key)
            .unwrap_or(KeySig::major(0))
    }

    /// Recompute `low_confidence` from measure confidences.
    pub fn refresh_low_confidence(&mut self, threshold: f32) {
        let mut out = Vec::new();
        for p in &self.parts {
            if let Some(st) = p.staves.first() {
                for (mi, m) in st.measures.iter().enumerate() {
                    // aggregate across staves
                    let mut conf: Option<f32> = m.confidence;
                    let mut reasons = m.confidence_reasons.clone();
                    for other in p.staves.iter().skip(1) {
                        if let Some(om) = other.measures.get(mi) {
                            if let Some(c) = om.confidence {
                                conf = Some(conf.map(|x| x.min(c)).unwrap_or(c));
                            }
                            for r in &om.confidence_reasons {
                                if !reasons.contains(r) {
                                    reasons.push(r.clone());
                                }
                            }
                        }
                    }
                    if let Some(c) = conf {
                        if c < threshold {
                            out.push(LowConfidence { part: p.id.clone(), measure: m.number, confidence: c, reasons });
                        }
                    }
                }
            }
        }
        self.low_confidence = out;
    }

    /// Rescale every duration and offset to a new divisions value (must be a multiple
    /// of the old one, or an exact divisor of every duration).
    pub fn rescale(&mut self, new_div: u32) {
        let old = self.divisions;
        if old == new_div || old == 0 {
            return;
        }
        let f = |x: u32| -> u32 { ((x as u64 * new_div as u64) / old as u64) as u32 };
        for p in &mut self.parts {
            for s in &mut p.staves {
                for m in &mut s.measures {
                    for t in &mut m.tempo {
                        t.offset = f(t.offset);
                    }
                    for h in &mut m.harmonies {
                        h.offset = f(h.offset);
                    }
                    for d in &mut m.directions {
                        d.offset = f(d.offset);
                    }
                    for v in &mut m.voices {
                        for e in &mut v.events {
                            e.duration = f(e.duration);
                        }
                    }
                }
            }
        }
        self.divisions = new_div;
    }
}
