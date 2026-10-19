//! Tempo-map conversions between ticks, beats (quarter notes), and seconds.

use crate::smf::Timing;

/// Default MIDI tempo: 120 BPM.
pub const DEFAULT_US_PER_QUARTER: u32 = 500_000;

/// A piecewise-constant tempo map.
#[derive(Clone, Debug, PartialEq)]
pub struct TempoMap {
    pub timing: Timing,
    /// `(tick, microseconds per quarter)`, sorted by tick; always starts at tick 0.
    segments: Vec<(u64, u32)>,
    /// Seconds at the start of each segment.
    starts_s: Vec<f64>,
}

impl TempoMap {
    /// Build from tempo changes `(tick, us_per_quarter)` in any order.
    pub fn new(timing: Timing, changes: &[(u64, u32)]) -> TempoMap {
        let mut ch: Vec<(u64, u32)> = changes.iter().copied().filter(|c| c.1 > 0).collect();
        ch.sort_by_key(|c| c.0);
        let mut segments: Vec<(u64, u32)> = Vec::new();
        for (t, us) in ch {
            if let Some(last) = segments.last_mut() {
                if last.0 == t {
                    last.1 = us;
                    continue;
                }
            }
            segments.push((t, us));
        }
        if segments.first().map(|s| s.0 != 0).unwrap_or(true) {
            let first_us = DEFAULT_US_PER_QUARTER;
            segments.insert(0, (0, first_us));
        }
        let mut m = TempoMap {
            timing,
            segments,
            starts_s: Vec::new(),
        };
        m.recompute();
        m
    }

    /// Constant tempo.
    pub fn constant(ppq: u16, bpm: f64) -> TempoMap {
        TempoMap::new(Timing::Ppq(ppq), &[(0, bpm_to_us(bpm))])
    }

    fn recompute(&mut self) {
        self.starts_s.clear();
        let mut acc = 0.0;
        for i in 0..self.segments.len() {
            self.starts_s.push(acc);
            if i + 1 < self.segments.len() {
                let dt = self.segments[i + 1].0 - self.segments[i].0;
                acc += self.ticks_to_s_at(dt as f64, self.segments[i].1);
            }
        }
    }

    pub fn ppq(&self) -> Option<u16> {
        match self.timing {
            Timing::Ppq(p) => Some(p),
            _ => None,
        }
    }

    fn ticks_to_s_at(&self, ticks: f64, us: u32) -> f64 {
        match self.timing {
            Timing::Ppq(p) => ticks * us as f64 / 1e6 / p as f64,
            Timing::Smpte { .. } => ticks / self.timing.smpte_ticks_per_second().unwrap().max(1e-9),
        }
    }

    /// Tempo changes as `(tick, us_per_quarter)`.
    pub fn segments(&self) -> &[(u64, u32)] {
        &self.segments
    }

    /// Seconds at an absolute tick (ticks may be fractional).
    pub fn tick_to_seconds(&self, tick: f64) -> f64 {
        if let Timing::Smpte { .. } = self.timing {
            return tick / self.timing.smpte_ticks_per_second().unwrap().max(1e-9);
        }
        let tick = tick.max(0.0);
        let i = match self
            .segments
            .binary_search_by(|s| (s.0 as f64).partial_cmp(&tick).unwrap())
        {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        };
        let (t0, us) = self.segments[i];
        self.starts_s[i] + self.ticks_to_s_at(tick - t0 as f64, us)
    }

    /// Absolute (fractional) tick at a time in seconds.
    pub fn seconds_to_tick(&self, s: f64) -> f64 {
        if let Timing::Smpte { .. } = self.timing {
            return s * self.timing.smpte_ticks_per_second().unwrap();
        }
        let s = s.max(0.0);
        let i = match self
            .starts_s
            .binary_search_by(|x| x.partial_cmp(&s).unwrap())
        {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        };
        let (t0, us) = self.segments[i];
        let ppq = self.ppq().unwrap_or(480) as f64;
        t0 as f64 + (s - self.starts_s[i]) * 1e6 * ppq / us as f64
    }

    /// Quarter-note beats at a tick (`None` for SMPTE timing).
    pub fn tick_to_beats(&self, tick: f64) -> Option<f64> {
        self.ppq().map(|p| tick / p as f64)
    }

    /// Tick at a beat position (SMPTE timing: beats at 120 BPM).
    pub fn beats_to_tick(&self, beats: f64) -> f64 {
        match self.ppq() {
            Some(p) => beats * p as f64,
            None => self.seconds_to_tick(beats * 0.5),
        }
    }

    /// Tempo in BPM at a tick.
    pub fn bpm_at(&self, tick: u64) -> f64 {
        let i = match self.segments.binary_search_by_key(&tick, |s| s.0) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        };
        us_to_bpm(self.segments[i].1)
    }

    /// Ticks spanned by `secs` seconds starting at `tick`.
    pub fn seconds_to_ticks_at(&self, tick: f64, secs: f64) -> f64 {
        let s0 = self.tick_to_seconds(tick);
        self.seconds_to_tick(s0 + secs) - tick
    }
}

pub fn bpm_to_us(bpm: f64) -> u32 {
    (60_000_000.0 / bpm.max(1e-3)).round().clamp(1.0, 16_777_215.0) as u32
}

pub fn us_to_bpm(us: u32) -> f64 {
    60_000_000.0 / us.max(1) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversions_with_tempo_change() {
        // 120 BPM for 4 beats, then 60 BPM
        let m = TempoMap::new(Timing::Ppq(480), &[(0, 500_000), (1920, 1_000_000)]);
        assert!((m.tick_to_seconds(1920.0) - 2.0).abs() < 1e-12);
        assert!((m.tick_to_seconds(2400.0) - 3.0).abs() < 1e-12);
        assert!((m.seconds_to_tick(3.0) - 2400.0).abs() < 1e-9);
        assert!((m.seconds_to_tick(1.0) - 960.0).abs() < 1e-9);
        assert_eq!(m.tick_to_beats(960.0), Some(2.0));
        assert_eq!(m.bpm_at(2000), 60.0);
        assert!((m.seconds_to_ticks_at(1920.0, 0.5) - 240.0).abs() < 1e-9);
    }

    #[test]
    fn default_tempo_and_bpm() {
        let m = TempoMap::new(Timing::Ppq(96), &[]);
        assert_eq!(m.bpm_at(0), 120.0);
        assert_eq!(bpm_to_us(120.0), 500_000);
        assert_eq!(us_to_bpm(bpm_to_us(93.0)).round(), 93.0);
        let s = TempoMap::new(
            Timing::Smpte {
                fps: 25,
                ticks_per_frame: 40,
            },
            &[],
        );
        assert_eq!(s.tick_to_seconds(1000.0), 1.0);
        assert_eq!(s.tick_to_beats(10.0), None);
    }
}
