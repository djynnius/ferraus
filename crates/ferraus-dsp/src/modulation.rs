//! LFOs and modulation effects: chorus, flanger, phaser, tremolo, and vibrato. Each
//! processor handles one channel; callers give stereo channels different LFO phases.

use crate::delayline::DelayLine;
use crate::util::flush;
use std::f64::consts::PI;

/// LFO waveform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    Sine,
    Triangle,
    /// Slightly rounded square (no clicks).
    Square,
    Saw,
}

impl Shape {
    pub fn parse(s: &str) -> Shape {
        match s.to_ascii_lowercase().as_str() {
            "triangle" | "tri" => Shape::Triangle,
            "square" | "sq" => Shape::Square,
            "saw" | "sawtooth" => Shape::Saw,
            _ => Shape::Sine,
        }
    }
}

/// Low-frequency oscillator with output in −1..1.
#[derive(Clone, Debug)]
pub struct Lfo {
    phase: f64,
    inc: f64,
    pub shape: Shape,
}

impl Lfo {
    /// `phase` in cycles (0..1).
    pub fn new(rate_hz: f64, sample_rate: f64, shape: Shape, phase: f64) -> Lfo {
        Lfo {
            phase: phase.rem_euclid(1.0),
            inc: rate_hz / sample_rate,
            shape,
        }
    }

    /// Value of a shape at a phase (cycles).
    #[inline]
    pub fn shape_at(shape: Shape, phase: f64) -> f64 {
        let p = phase.rem_euclid(1.0);
        match shape {
            Shape::Sine => (2.0 * PI * p).sin(),
            Shape::Triangle => {
                // starts at 0 rising, like sine
                let q = (p + 0.25).rem_euclid(1.0);
                1.0 - 4.0 * (q - 0.5).abs()
            }
            Shape::Square => {
                let s = (2.0 * PI * p).sin();
                (6.0 * s).tanh() / 6f64.tanh()
            }
            Shape::Saw => 2.0 * p - 1.0,
        }
    }

    #[inline]
    pub fn next(&mut self) -> f64 {
        let v = Self::shape_at(self.shape, self.phase);
        self.phase += self.inc;
        if self.phase >= 1.0 {
            self.phase -= 1.0;
        }
        v
    }
}

/// Multi-voice chorus (and, with `feedback` and a short base delay, a flanger).
#[derive(Clone, Debug)]
pub struct Chorus {
    line: DelayLine,
    lfos: Vec<Lfo>,
    base: f64,
    depth: f64,
    feedback: f32,
    fb_state: f32,
}

impl Chorus {
    /// `base_s` centre delay, `depth_s` sweep width (peak-to-peak), `phase` channel offset.
    pub fn new(
        sample_rate: f64,
        rate_hz: f64,
        base_s: f64,
        depth_s: f64,
        voices: usize,
        feedback: f32,
        phase: f64,
    ) -> Chorus {
        let voices = voices.clamp(1, 8);
        let max = ((base_s + depth_s) * sample_rate) as usize + 8;
        Chorus {
            line: DelayLine::new(max),
            lfos: (0..voices)
                .map(|v| {
                    // spread voices in phase and slightly in rate so they never lock
                    let r = rate_hz * (1.0 + 0.07 * v as f64);
                    Lfo::new(r, sample_rate, Shape::Sine, phase + v as f64 / voices as f64)
                })
                .collect(),
            base: base_s * sample_rate,
            depth: depth_s * sample_rate,
            feedback: feedback.clamp(-0.95, 0.95),
            fb_state: 0.0,
        }
    }

    /// Returns the wet signal.
    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        self.line.push(x + self.feedback * self.fb_state);
        let mut acc = 0.0f32;
        for l in &mut self.lfos {
            let d = self.base + 0.5 * self.depth * l.next();
            acc += self.line.read_cubic(d.max(2.0) as f32);
        }
        let wet = acc / self.lfos.len() as f32;
        self.fb_state = flush(wet);
        wet
    }
}

/// Flanger: a short swept delay with feedback.
pub fn flanger(sample_rate: f64, rate_hz: f64, depth_s: f64, feedback: f32, phase: f64) -> Chorus {
    // sweep from ~0.1 ms up to depth
    let base = 0.0001 + depth_s / 2.0;
    Chorus::new(sample_rate, rate_hz, base, depth_s, 1, feedback, phase)
}

/// Phaser: a chain of swept first-order all-passes with feedback.
#[derive(Clone, Debug)]
pub struct Phaser {
    stages: Vec<(f32, f32)>, // (x1, y1) state per stage
    lfo: Lfo,
    sample_rate: f64,
    center: f64,
    octaves: f64,
    feedback: f32,
    fb: f32,
    coeff: f32,
    counter: usize,
}

impl Phaser {
    pub fn new(sample_rate: f64, stages: usize, rate_hz: f64, depth: f64, feedback: f32, phase: f64) -> Phaser {
        Phaser {
            stages: vec![(0.0, 0.0); stages.clamp(1, 24)],
            lfo: Lfo::new(rate_hz, sample_rate, Shape::Sine, phase),
            sample_rate,
            center: 900.0,
            octaves: 2.5 * depth.clamp(0.0, 1.0),
            feedback: feedback.clamp(-0.95, 0.95),
            fb: 0.0,
            coeff: 0.0,
            counter: 0,
        }
    }

    /// Returns the wet (all-passed) signal.
    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        if self.counter == 0 {
            let f = (self.center * 2f64.powf(self.octaves * self.lfo.next()))
                .clamp(20.0, self.sample_rate * 0.45);
            let t = (PI * f / self.sample_rate).tan();
            self.coeff = ((t - 1.0) / (t + 1.0)) as f32;
            // advance the LFO by the skipped samples
            for _ in 0..15 {
                self.lfo.next();
            }
        }
        self.counter = (self.counter + 1) % 16;
        let a = self.coeff;
        let mut v = x + self.feedback * self.fb;
        for (x1, y1) in &mut self.stages {
            let y = a * v + *x1 - a * *y1;
            *x1 = v;
            *y1 = flush(y);
            v = y;
        }
        self.fb = flush(v);
        v
    }
}

/// Vibrato: a sinusoidally modulated delay producing ±`depth_cents` of pitch deviation.
/// The constant part of the delay is reported by [`Vibrato::latency`] for compensation.
#[derive(Clone, Debug)]
pub struct Vibrato {
    line: DelayLine,
    lfo: Lfo,
    center: f64,
    amp: f64,
}

impl Vibrato {
    pub fn new(sample_rate: f64, rate_hz: f64, depth_cents: f64, shape: Shape, phase: f64) -> Vibrato {
        let rate = rate_hz.max(0.01);
        // for d(t) = A sin(2π f t): pitch ratio = 1 - 2π f A cos(...), so A = (2^(c/1200)-1)/(2π f)
        let ratio = 2f64.powf(depth_cents.abs() / 1200.0) - 1.0;
        let amp = ratio / (2.0 * PI * rate) * sample_rate;
        let center = amp + 3.0;
        Vibrato {
            line: DelayLine::new((center + amp) as usize + 8),
            lfo: Lfo::new(rate, sample_rate, shape, phase),
            center,
            amp,
        }
    }

    /// Constant delay in samples (rounded).
    pub fn latency(&self) -> usize {
        self.center.round() as usize
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        self.line.push(x);
        let d = self.center + self.amp * self.lfo.next();
        self.line.read_cubic(d as f32)
    }
}

/// Tremolo gain for an LFO value `v` (−1..1): swings between 1 and `1 - depth`.
#[inline]
pub fn tremolo_gain(v: f64, depth: f64) -> f32 {
    (1.0 - depth.clamp(0.0, 1.0) * (0.5 - 0.5 * v)) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lfo_shapes_in_range() {
        for s in [Shape::Sine, Shape::Triangle, Shape::Square, Shape::Saw] {
            let mut l = Lfo::new(3.0, 1000.0, s, 0.0);
            for _ in 0..1000 {
                let v = l.next();
                assert!((-1.0001..=1.0001).contains(&v));
            }
        }
    }

    #[test]
    fn vibrato_pitch_deviation() {
        // instantaneous frequency of a vibrato'd 1 kHz sine stays within ±depth
        let sr = 48000.0;
        let mut v = Vibrato::new(sr, 5.0, 50.0, Shape::Sine, 0.0);
        let x: Vec<f32> = (0..48000).map(|i| (2.0 * PI * 1000.0 * i as f64 / sr).sin() as f32).collect();
        let y: Vec<f32> = x.iter().map(|s| v.process(*s)).collect();
        // count zero crossings in 50 ms windows
        let w = 2400;
        let mut maxf: f64 = 0.0;
        let mut minf: f64 = 1e9;
        for c in y[4800..].chunks(w) {
            if c.len() < w {
                break;
            }
            let zc = c.windows(2).filter(|p| p[0] <= 0.0 && p[1] > 0.0).count() as f64;
            let f = zc / (w as f64 / sr);
            maxf = maxf.max(f);
            minf = minf.min(f);
        }
        let cents_hi = 1200.0 * (maxf / 1000.0).log2();
        let cents_lo = 1200.0 * (minf / 1000.0).log2();
        assert!(cents_hi > 20.0 && cents_hi < 80.0, "{cents_hi}");
        assert!(cents_lo < -20.0 && cents_lo > -80.0, "{cents_lo}");
    }
}
