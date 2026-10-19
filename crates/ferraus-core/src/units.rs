//! The shared unit parser: time, gain, frequency, ratios, percentages, musical grids,
//! cents and semitones, byte sizes. Every command uses these so `1.5s`, `-6dB`, `C4`,
//! `1/8T` mean the same thing everywhere.

use crate::error::{Error, Result};
use serde::Serialize;

fn err(kind: &str, s: &str, expected: &str) -> Error {
    Error::usage(format!("cannot parse {kind} '{s}': expected {expected}"))
}

fn num(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    s.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// Split a trailing alphabetic unit (case-insensitive) from a number.
fn split_unit(s: &str) -> (&str, String) {
    let s = s.trim();
    let idx = s
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_ascii_alphabetic() || *c == '%' || *c == '/')
        .last()
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    (s[..idx].trim(), s[idx..].to_ascii_lowercase())
}

// ---------------------------------------------------------------------------------
// Time
// ---------------------------------------------------------------------------------

/// A point in time, resolved against a buffer later.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Time {
    Start,
    End,
    Seconds(f64),
    Samples(u64),
    Beats {
        beats: f64,
        bpm: Option<f64>,
    },
    /// Counted back from the end of the input.
    FromEnd(Box<Time>),
}

impl Time {
    /// Parse `1.5s`, `1500ms`, `01:23.5`, `1:02:03.250`, `44100smp`, `4b`, `4b@120`,
    /// `-10s`, `start`, `end`, or a bare number of seconds.
    pub fn parse(s: &str) -> Result<Time> {
        let t = s.trim();
        let lower = t.to_ascii_lowercase();
        match lower.as_str() {
            "start" | "begin" => return Ok(Time::Start),
            "end" => return Ok(Time::End),
            _ => {}
        }
        if let Some(rest) = t.strip_prefix('-') {
            let inner = Time::parse(rest)?;
            return match inner {
                Time::Start => Ok(Time::End),
                Time::End => Ok(Time::Start),
                Time::FromEnd(_) => Err(err("time", s, "a single leading '-'")),
                other => Ok(Time::FromEnd(Box::new(other))),
            };
        }
        let t = t.strip_prefix('+').unwrap_or(t);
        if t.contains(':') {
            return parse_clock(t).map(Time::Seconds).ok_or_else(|| err("time", s, "mm:ss(.fff) or hh:mm:ss(.fff)"));
        }
        // beats with optional @bpm
        let (body, bpm) = match t.split_once('@') {
            Some((b, bpm)) => (b, Some(num(bpm).ok_or_else(|| err("time", s, "a tempo after '@'"))?)),
            None => (t, None),
        };
        let (n, unit) = split_unit(body);
        let v = num(n).ok_or_else(|| err("time", s, "a number with s, ms, smp, or b"))?;
        let out = match unit.as_str() {
            "" | "s" | "sec" | "secs" => Time::Seconds(v),
            "ms" => Time::Seconds(v / 1000.0),
            "us" => Time::Seconds(v / 1e6),
            "min" | "m" => Time::Seconds(v * 60.0),
            "h" => Time::Seconds(v * 3600.0),
            "smp" | "samples" | "sample" | "spl" => {
                if v < 0.0 {
                    return Err(err("time", s, "a non-negative sample count"));
                }
                Time::Samples(v.round() as u64)
            }
            "b" | "beat" | "beats" => Time::Beats { beats: v, bpm },
            _ => return Err(err("time", s, "units s, ms, smp, b, or mm:ss")),
        };
        if bpm.is_some() && !matches!(out, Time::Beats { .. }) {
            return Err(err("time", s, "'@bpm' only after beats, e.g. 4b@120"));
        }
        if let Time::Seconds(x) = out {
            if x < 0.0 {
                return Err(err("time", s, "a non-negative time"));
            }
        }
        Ok(out)
    }

    /// Duration in seconds for times that do not need a buffer (`Start` is 0).
    pub fn seconds(&self, sample_rate: u32, bpm: Option<f64>) -> Result<f64> {
        match self {
            Time::Start => Ok(0.0),
            Time::Seconds(s) => Ok(*s),
            Time::Samples(n) => Ok(*n as f64 / sample_rate.max(1) as f64),
            Time::Beats { beats, bpm: b } => {
                let tempo = b
                    .or(bpm)
                    .ok_or_else(|| Error::usage("beat-based time needs a tempo: pass --bpm or use the form 4b@120"))?;
                Ok(beats * 60.0 / tempo)
            }
            Time::End | Time::FromEnd(_) => {
                Err(Error::usage("'end' and negative times need an input to resolve against"))
            }
        }
    }

    /// Resolve to a frame index within a buffer of `total` frames, clamped to `[0, total]`.
    pub fn frames(&self, sample_rate: u32, total: usize, bpm: Option<f64>) -> Result<usize> {
        let sr = sample_rate as f64;
        let f = match self {
            Time::End => total as f64,
            Time::FromEnd(inner) => total as f64 - inner.seconds(sample_rate, bpm)? * sr,
            Time::Samples(n) => *n as f64,
            other => other.seconds(sample_rate, bpm)? * sr,
        };
        Ok(f.round().clamp(0.0, total as f64) as usize)
    }

    /// Resolve to frames without clamping to a total (for durations such as `--pad`).
    pub fn duration_frames(&self, sample_rate: u32, bpm: Option<f64>) -> Result<usize> {
        match self {
            Time::Samples(n) => Ok(*n as usize),
            other => Ok((other.seconds(sample_rate, bpm)? * sample_rate as f64).round().max(0.0) as usize),
        }
    }
}

fn parse_clock(s: &str) -> Option<f64> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() > 3 || parts.iter().any(|p| p.is_empty()) {
        return None;
    }
    let mut total = 0.0;
    for (i, p) in parts.iter().enumerate() {
        let v = num(p)?;
        if v < 0.0 {
            return None;
        }
        if i < parts.len() - 1 && p.contains('.') {
            return None;
        }
        if i > 0 && v >= 60.0 {
            return None;
        }
        total = total * 60.0 + v;
    }
    Some(total)
}

/// Parse a duration (`Time` that must be resolvable without an input).
pub fn parse_duration_s(s: &str) -> Result<f64> {
    match Time::parse(s)? {
        Time::Seconds(v) => Ok(v),
        Time::Start => Ok(0.0),
        _ => Err(err("duration", s, "a duration such as 2s or 500ms")),
    }
}

/// A `start..end` range.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TimeRange {
    pub start: Time,
    pub end: Time,
}

impl TimeRange {
    pub fn parse(s: &str) -> Result<TimeRange> {
        let (a, b) = s.split_once("..").ok_or_else(|| err("range", s, "start..end, e.g. 0s..2s or 1:00..end"))?;
        let mut start = if a.trim().is_empty() { Time::Start } else { Time::parse(a)? };
        let mut end = if b.trim().is_empty() { Time::End } else { Time::parse(b)? };
        // `0b..4b@120`: a tempo written on one end applies to both
        let tempo = |t: &Time| if let Time::Beats { bpm: Some(b), .. } = t { Some(*b) } else { None };
        if let Some(b) = tempo(&end).or(tempo(&start)) {
            for t in [&mut start, &mut end] {
                if let Time::Beats { bpm: bpm @ None, .. } = t {
                    *bpm = Some(b);
                }
            }
        }
        Ok(TimeRange { start, end })
    }

    /// Frames `[start, end)` within `total`. Errors when start > end.
    pub fn frames(&self, sample_rate: u32, total: usize, bpm: Option<f64>) -> Result<(usize, usize)> {
        let a = self.start.frames(sample_rate, total, bpm)?;
        let b = self.end.frames(sample_rate, total, bpm)?;
        if a > b {
            return Err(Error::usage(format!("range start ({a} frames) is after its end ({b} frames)")));
        }
        Ok((a, b))
    }
}

/// A numeric range such as `80Hz..8kHz` or `60..200`.
pub fn parse_num_range<T>(s: &str, f: impl Fn(&str) -> Result<T>) -> Result<(T, T)> {
    let (a, b) = s.split_once("..").ok_or_else(|| err("range", s, "low..high"))?;
    Ok((f(a)?, f(b)?))
}

// ---------------------------------------------------------------------------------
// Gain
// ---------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Gain {
    Db(f64),
    Linear(f64),
    Lufs(f64),
    DbTp(f64),
}

impl Gain {
    /// `-6dB`, `+3dB`, `0.5x`, `-14LUFS`, `-1dBTP`, `-6` (dB), `-23LKFS`, `-20dBFS`.
    pub fn parse(s: &str) -> Result<Gain> {
        let (n, unit) = split_unit(s);
        let v = num(n).ok_or_else(|| err("gain", s, "a number with dB, x, LUFS, or dBTP"))?;
        Ok(match unit.as_str() {
            "" | "db" | "dbfs" => Gain::Db(v),
            "x" => {
                if v < 0.0 {
                    return Err(err("gain", s, "a non-negative linear factor"));
                }
                Gain::Linear(v)
            }
            "lufs" | "lkfs" | "lu" => Gain::Lufs(v),
            "dbtp" => Gain::DbTp(v),
            _ => return Err(err("gain", s, "units dB, x, LUFS, or dBTP")),
        })
    }

    /// Decibel value (LUFS and dBTP are returned as their numeric value).
    pub fn db(&self) -> f64 {
        match *self {
            Gain::Db(d) | Gain::Lufs(d) | Gain::DbTp(d) => d,
            Gain::Linear(l) => crate::buffer::lin_to_db(l),
        }
    }

    pub fn linear(&self) -> f64 {
        match *self {
            Gain::Linear(l) => l,
            other => crate::buffer::db_to_lin(other.db()),
        }
    }
}

/// Parse a decibel value (accepts `dB`, `x`, bare numbers).
pub fn parse_db(s: &str) -> Result<f64> {
    Ok(Gain::parse(s)?.db())
}

// ---------------------------------------------------------------------------------
// Frequency and pitch
// ---------------------------------------------------------------------------------

/// `120Hz`, `2.5kHz`, `C4`, `A#3`, `Bb2`, `midi:60`, or a bare number in Hz.
pub fn parse_freq(s: &str) -> Result<f64> {
    let t = s.trim();
    if let Some(m) = t.strip_prefix("midi:") {
        let n = num(m).ok_or_else(|| err("frequency", s, "midi:<number>"))?;
        return Ok(midi_to_hz(n));
    }
    if let Some(n) = parse_note_name(t) {
        return Ok(midi_to_hz(n as f64));
    }
    let (n, unit) = split_unit(t);
    let v = num(n).ok_or_else(|| err("frequency", s, "Hz, kHz, a note name like C4, or midi:60"))?;
    let hz = match unit.as_str() {
        "" | "hz" => v,
        "khz" | "k" => v * 1000.0,
        _ => return Err(err("frequency", s, "units Hz or kHz")),
    };
    if hz < 0.0 {
        return Err(err("frequency", s, "a non-negative frequency"));
    }
    Ok(hz)
}

pub fn midi_to_hz(n: f64) -> f64 {
    440.0 * 2f64.powf((n - 69.0) / 12.0)
}

pub fn hz_to_midi(hz: f64) -> f64 {
    69.0 + 12.0 * (hz.max(1e-9) / 440.0).log2()
}

const NOTE_NAMES_SHARP: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

/// Note name for a MIDI number, e.g. 60 → `C4`.
pub fn midi_to_name(n: i32) -> String {
    let pc = n.rem_euclid(12) as usize;
    let oct = n.div_euclid(12) - 1;
    format!("{}{}", NOTE_NAMES_SHARP[pc], oct)
}

/// Pitch class of a note letter with accidentals (`C`, `F#`, `Bb`), 0..12.
pub fn parse_pitch_class(s: &str) -> Option<i32> {
    let mut chars = s.trim().chars();
    let letter = chars.next()?.to_ascii_uppercase();
    let base = match letter {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return None,
    };
    let mut pc: i32 = base;
    for c in chars {
        match c {
            '#' | '♯' => pc += 1,
            'b' | '♭' => pc -= 1,
            _ => return None,
        }
    }
    Some(pc.rem_euclid(12))
}

/// MIDI number for a note name with octave (`C4` = 60, `A#3`, `Bb-1`).
pub fn parse_note_name(s: &str) -> Option<i32> {
    let s = s.trim();
    let first = s.chars().next()?;
    if !matches!(first.to_ascii_uppercase(), 'A'..='G') {
        return None;
    }
    let split = s.char_indices().find(|(i, c)| *i > 0 && (c.is_ascii_digit() || *c == '-')).map(|(i, _)| i)?;
    let pc = parse_pitch_class(&s[..split])?;
    let oct: i32 = s[split..].parse().ok()?;
    // Accidentals can cross octave boundaries (B#3 = C4, Cb4 = B3): compute from base letter.
    let letter_pc = parse_pitch_class(&s[..1])?;
    let acc = {
        let raw = pc - letter_pc;
        if raw > 6 {
            raw - 12
        } else if raw < -6 {
            raw + 12
        } else {
            raw
        }
    };
    Some((oct + 1) * 12 + letter_pc + acc)
}

/// Cents from `20cents`, `+200cents`, `-50c`, `2st`, `2semitones`, or a bare number (cents).
pub fn parse_cents(s: &str) -> Result<f64> {
    let (n, unit) = split_unit(s);
    let v = num(n).ok_or_else(|| err("pitch offset", s, "cents or semitones"))?;
    Ok(match unit.as_str() {
        "" | "c" | "cent" | "cents" | "ct" => v,
        "st" | "semi" | "semitone" | "semitones" => v * 100.0,
        _ => return Err(err("pitch offset", s, "cents or semitones")),
    })
}

/// Semitones from `2`, `-3st`, `+200cents`.
pub fn parse_semitones(s: &str) -> Result<f64> {
    let (n, unit) = split_unit(s);
    let v = num(n).ok_or_else(|| err("semitones", s, "a number of semitones"))?;
    Ok(match unit.as_str() {
        "" | "st" | "semi" | "semitone" | "semitones" => v,
        "c" | "cent" | "cents" => v / 100.0,
        _ => return Err(err("semitones", s, "semitones or cents")),
    })
}

// ---------------------------------------------------------------------------------
// Ratios, percentages, misc
// ---------------------------------------------------------------------------------

/// `4:1` or `4` → 4.0.
pub fn parse_ratio(s: &str) -> Result<f64> {
    let t = s.trim();
    let v = if let Some((a, b)) = t.split_once(':') {
        let a = num(a).ok_or_else(|| err("ratio", s, "N:1 or N"))?;
        let b = num(b).ok_or_else(|| err("ratio", s, "N:1 or N"))?;
        if b == 0.0 {
            return Err(err("ratio", s, "a non-zero denominator"));
        }
        a / b
    } else {
        num(t).ok_or_else(|| err("ratio", s, "N:1 or N"))?
    };
    if v <= 0.0 {
        return Err(err("ratio", s, "a positive ratio"));
    }
    Ok(v)
}

/// `80%` → 0.8, `0.8` → 0.8, `130%` → 1.3.
pub fn parse_percent(s: &str) -> Result<f64> {
    let t = s.trim();
    if let Some(p) = t.strip_suffix('%') {
        return num(p).map(|v| v / 100.0).ok_or_else(|| err("percentage", s, "N% or a fraction"));
    }
    num(t).ok_or_else(|| err("percentage", s, "N% or a fraction such as 0.8"))
}

/// Byte sizes: `2GB`, `512MB`, `300M`, raw bytes.
pub fn parse_bytes(s: &str) -> Result<u64> {
    let (n, unit) = split_unit(s);
    let v = num(n).ok_or_else(|| err("size", s, "a size such as 2GB"))?;
    let mul = match unit.as_str() {
        "" | "b" => 1.0,
        "k" | "kb" | "kib" => 1024.0,
        "m" | "mb" | "mib" => 1024.0 * 1024.0,
        "g" | "gb" | "gib" => 1024.0 * 1024.0 * 1024.0,
        "t" | "tb" | "tib" => 1024f64.powi(4),
        _ => return Err(err("size", s, "B, KB, MB, GB")),
    };
    Ok((v * mul) as u64)
}

/// Bitrates: `192k`, `320kbps`, `128000`.
pub fn parse_bitrate(s: &str) -> Result<u32> {
    let (n, unit) = split_unit(s);
    let v = num(n).ok_or_else(|| err("bitrate", s, "a bitrate such as 192k"))?;
    let bps = match unit.as_str() {
        "" | "bps" => v,
        "k" | "kbps" | "kb" => v * 1000.0,
        "m" | "mbps" => v * 1e6,
        _ => return Err(err("bitrate", s, "a bitrate such as 192k")),
    };
    Ok(bps as u32)
}

/// Generic number with an optional expected unit suffix (e.g. `deg`, `m/s`, `oct`, `cents`).
pub fn parse_with_unit(s: &str, units: &[&str]) -> Result<f64> {
    let t = s.trim();
    let lower = t.to_ascii_lowercase();
    for u in units {
        if let Some(n) = lower.strip_suffix(&u.to_ascii_lowercase()) {
            if let Some(v) = num(n) {
                return Ok(v);
            }
        }
    }
    num(t).ok_or_else(|| err("value", s, &format!("a number optionally followed by {}", units.join(", "))))
}

// ---------------------------------------------------------------------------------
// Musical grid
// ---------------------------------------------------------------------------------

/// `1/16`, `1/8T` (triplet), `1/8D` (dotted), `1/4`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Grid {
    pub num: u32,
    pub den: u32,
    pub triplet: bool,
    pub dotted: bool,
}

impl Grid {
    pub fn parse(s: &str) -> Result<Grid> {
        let t = s.trim();
        let (body, triplet, dotted) = if let Some(b) = t.strip_suffix(['T', 't']) {
            (b, true, false)
        } else if let Some(b) = t.strip_suffix(['D', 'd', '.']) {
            (b, false, true)
        } else {
            (t, false, false)
        };
        let (n, d) = body.split_once('/').ok_or_else(|| err("grid", s, "1/16, 1/8T, or 1/8D"))?;
        let num: u32 = n.trim().parse().map_err(|_| err("grid", s, "1/16, 1/8T, or 1/8D"))?;
        let den: u32 = d.trim().parse().map_err(|_| err("grid", s, "1/16, 1/8T, or 1/8D"))?;
        if num == 0 || den == 0 {
            return Err(err("grid", s, "non-zero numerator and denominator"));
        }
        Ok(Grid { num, den, triplet, dotted })
    }

    /// Length in quarter-note beats (`1/4` = 1.0, `1/16` = 0.25, `1/8T` = 1/3).
    pub fn beats(&self) -> f64 {
        let base = 4.0 * self.num as f64 / self.den as f64;
        if self.triplet {
            base * 2.0 / 3.0
        } else if self.dotted {
            base * 1.5
        } else {
            base
        }
    }

    pub fn seconds(&self, bpm: f64) -> f64 {
        self.beats() * 60.0 / bpm
    }

    /// Ticks per grid step at a given PPQ.
    pub fn ticks(&self, ppq: u32) -> f64 {
        self.beats() * ppq as f64
    }
}

impl std::fmt::Display for Grid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}/{}{}",
            self.num,
            self.den,
            if self.triplet {
                "T"
            } else if self.dotted {
                "D"
            } else {
                ""
            }
        )
    }
}

/// A delay time that may be a beat fraction (`1/4`) or a plain time (`375ms`).
#[derive(Clone, Debug, PartialEq)]
pub enum SyncTime {
    Grid(Grid),
    Time(Time),
}

impl SyncTime {
    pub fn parse(s: &str) -> Result<SyncTime> {
        if s.contains('/') {
            Grid::parse(s).map(SyncTime::Grid)
        } else {
            Time::parse(s).map(SyncTime::Time)
        }
    }

    pub fn seconds(&self, sample_rate: u32, bpm: Option<f64>, fallback_bpm: f64) -> Result<f64> {
        match self {
            SyncTime::Grid(g) => Ok(g.seconds(bpm.unwrap_or(fallback_bpm))),
            SyncTime::Time(t) => t.seconds(sample_rate, bpm),
        }
    }
}

/// Split a comma list, trimming entries and dropping empties.
pub fn split_list(s: &str) -> Vec<String> {
    s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times() {
        assert_eq!(Time::parse("1.5s").unwrap(), Time::Seconds(1.5));
        assert_eq!(Time::parse("1500ms").unwrap(), Time::Seconds(1.5));
        assert_eq!(Time::parse("01:23.5").unwrap(), Time::Seconds(83.5));
        assert_eq!(Time::parse("1:02:03.250").unwrap(), Time::Seconds(3723.25));
        assert_eq!(Time::parse("44100smp").unwrap(), Time::Samples(44100));
        assert_eq!(Time::parse("4b").unwrap(), Time::Beats { beats: 4.0, bpm: None });
        assert_eq!(Time::parse("4b@120").unwrap(), Time::Beats { beats: 4.0, bpm: Some(120.0) });
        assert_eq!(Time::parse("-10s").unwrap(), Time::FromEnd(Box::new(Time::Seconds(10.0))));
        assert_eq!(Time::parse("end").unwrap(), Time::End);
        assert_eq!(Time::parse("2").unwrap(), Time::Seconds(2.0));
        assert!(Time::parse("1:75").is_err());
        assert!(Time::parse("abc").is_err());
        assert!(Time::parse("1s@120").is_err());
    }

    #[test]
    fn time_resolution() {
        let t = Time::parse("4b@120").unwrap();
        assert_eq!(t.frames(48000, 1_000_000, None).unwrap(), 96000);
        let t = Time::parse("-1s").unwrap();
        assert_eq!(t.frames(1000, 5000, None).unwrap(), 4000);
        assert!(Time::parse("4b").unwrap().seconds(48000, None).is_err());
        assert_eq!(Time::parse("4b").unwrap().seconds(48000, Some(60.0)).unwrap(), 4.0);
    }

    #[test]
    fn ranges() {
        let r = TimeRange::parse("0s..2s").unwrap();
        assert_eq!(r.frames(100, 1000, None).unwrap(), (0, 200));
        let r = TimeRange::parse("1:00..end").unwrap();
        assert_eq!(r.frames(10, 1000, None).unwrap(), (600, 1000));
        let r = TimeRange::parse("0b..4b@120").unwrap();
        assert_eq!(r.frames(48000, 10_000_000, None).unwrap(), (0, 96000));
        let r = TimeRange::parse("3s..1s").unwrap();
        assert!(r.frames(10, 1000, None).is_err());
    }

    #[test]
    fn gains() {
        assert_eq!(Gain::parse("-6dB").unwrap(), Gain::Db(-6.0));
        assert_eq!(Gain::parse("+3dB").unwrap(), Gain::Db(3.0));
        assert_eq!(Gain::parse("0.5x").unwrap(), Gain::Linear(0.5));
        assert_eq!(Gain::parse("-14LUFS").unwrap(), Gain::Lufs(-14.0));
        assert_eq!(Gain::parse("-1dBTP").unwrap(), Gain::DbTp(-1.0));
        assert!((Gain::parse("-6.0206dB").unwrap().linear() - 0.5).abs() < 1e-4);
        assert!(Gain::parse("loud").is_err());
    }

    #[test]
    fn freqs() {
        assert_eq!(parse_freq("120Hz").unwrap(), 120.0);
        assert_eq!(parse_freq("2.5kHz").unwrap(), 2500.0);
        assert!((parse_freq("A4").unwrap() - 440.0).abs() < 1e-9);
        assert!((parse_freq("C4").unwrap() - 261.6256).abs() < 1e-3);
        assert!((parse_freq("midi:69").unwrap() - 440.0).abs() < 1e-9);
        assert_eq!(parse_note_name("A#3"), Some(58));
        assert_eq!(parse_note_name("Bb3"), Some(58));
        assert_eq!(parse_note_name("B#3"), Some(60));
        assert_eq!(parse_note_name("Cb4"), Some(59));
        assert_eq!(parse_note_name("C-1"), Some(0));
        assert_eq!(midi_to_name(60), "C4");
    }

    #[test]
    fn ratios_percent_grid() {
        assert_eq!(parse_ratio("4:1").unwrap(), 4.0);
        assert_eq!(parse_ratio("4").unwrap(), 4.0);
        assert_eq!(parse_percent("80%").unwrap(), 0.8);
        assert_eq!(parse_percent("0.8").unwrap(), 0.8);
        assert_eq!(Grid::parse("1/16").unwrap().beats(), 0.25);
        assert!((Grid::parse("1/8T").unwrap().beats() - 1.0 / 3.0).abs() < 1e-12);
        assert_eq!(Grid::parse("1/8D").unwrap().beats(), 0.75);
        assert_eq!(parse_cents("+200cents").unwrap(), 200.0);
        assert_eq!(parse_cents("2st").unwrap(), 200.0);
        assert_eq!(parse_bytes("2GB").unwrap(), 2 * 1024 * 1024 * 1024);
        assert_eq!(parse_bitrate("192k").unwrap(), 192_000);
    }

    proptest::proptest! {
        #[test]
        fn seconds_roundtrip(v in 0.0f64..100000.0) {
            let s = format!("{v}s");
            let t = Time::parse(&s).unwrap();
            proptest::prop_assert!(matches!(t, Time::Seconds(x) if (x - v).abs() < 1e-9));
        }

        #[test]
        fn ms_equals_seconds(v in 0u32..10_000_000) {
            let a = Time::parse(&format!("{v}ms")).unwrap().seconds(48000, None).unwrap();
            proptest::prop_assert!((a - v as f64 / 1000.0).abs() < 1e-9);
        }

        #[test]
        fn parser_never_panics(s in ".{0,20}") {
            let _ = Time::parse(&s);
            let _ = Gain::parse(&s);
            let _ = parse_freq(&s);
            let _ = Grid::parse(&s);
            let _ = TimeRange::parse(&s);
            let _ = parse_percent(&s);
            let _ = parse_ratio(&s);
        }

        #[test]
        fn db_linear_roundtrip(db in -120.0f64..24.0) {
            let g = Gain::Db(db);
            proptest::prop_assert!((Gain::Linear(g.linear()).db() - db).abs() < 1e-6);
        }
    }
}
