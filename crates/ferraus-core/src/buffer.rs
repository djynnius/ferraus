//! Planar `f32` audio buffers and the `MediaBuffer` enum reserved for v2 video.

use serde::Serialize;

/// Planar `f32` audio: one `Vec<f32>` per channel, all the same length.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioBuffer {
    pub sample_rate: u32,
    pub data: Vec<Vec<f32>>,
}

impl AudioBuffer {
    /// A silent buffer.
    pub fn new(sample_rate: u32, channels: usize, frames: usize) -> Self {
        AudioBuffer {
            sample_rate,
            data: vec![vec![0.0; frames]; channels.max(1)],
        }
    }

    /// Build from planar channels. Shorter channels are zero-padded to the longest.
    pub fn from_planar(sample_rate: u32, mut data: Vec<Vec<f32>>) -> Self {
        if data.is_empty() {
            data.push(Vec::new());
        }
        let n = data.iter().map(|c| c.len()).max().unwrap_or(0);
        for c in &mut data {
            c.resize(n, 0.0);
        }
        AudioBuffer { sample_rate, data }
    }

    pub fn from_mono(sample_rate: u32, samples: Vec<f32>) -> Self {
        AudioBuffer {
            sample_rate,
            data: vec![samples],
        }
    }

    pub fn from_interleaved(sample_rate: u32, channels: usize, samples: &[f32]) -> Self {
        let channels = channels.max(1);
        let frames = samples.len() / channels;
        let mut data = vec![Vec::with_capacity(frames); channels];
        for f in 0..frames {
            for (c, ch) in data.iter_mut().enumerate() {
                ch.push(samples[f * channels + c]);
            }
        }
        AudioBuffer { sample_rate, data }
    }

    pub fn to_interleaved(&self) -> Vec<f32> {
        let ch = self.channels();
        let n = self.frames();
        let mut out = Vec::with_capacity(ch * n);
        for f in 0..n {
            for c in 0..ch {
                out.push(self.data[c][f]);
            }
        }
        out
    }

    pub fn channels(&self) -> usize {
        self.data.len()
    }

    pub fn frames(&self) -> usize {
        self.data.first().map(|c| c.len()).unwrap_or(0)
    }

    pub fn duration_s(&self) -> f64 {
        if self.sample_rate == 0 {
            0.0
        } else {
            self.frames() as f64 / self.sample_rate as f64
        }
    }

    pub fn is_empty(&self) -> bool {
        self.frames() == 0
    }

    /// Average of all channels.
    pub fn mono_mix(&self) -> Vec<f32> {
        let n = self.frames();
        let ch = self.channels() as f32;
        let mut out = vec![0.0f32; n];
        for c in &self.data {
            for (o, s) in out.iter_mut().zip(c) {
                *o += *s;
            }
        }
        if ch > 1.0 {
            for o in &mut out {
                *o /= ch;
            }
        }
        out
    }

    /// Frames `[start, end)`, clamped to the buffer.
    pub fn slice(&self, start: usize, end: usize) -> AudioBuffer {
        let n = self.frames();
        let s = start.min(n);
        let e = end.clamp(s, n);
        AudioBuffer {
            sample_rate: self.sample_rate,
            data: self.data.iter().map(|c| c[s..e].to_vec()).collect(),
        }
    }

    /// Append another buffer with the same channel count (extra channels are dropped,
    /// missing ones are filled from the last channel).
    pub fn append(&mut self, other: &AudioBuffer) {
        let ch = self.channels();
        for c in 0..ch {
            let src = other.data.get(c).or(other.data.last());
            if let Some(src) = src {
                self.data[c].extend_from_slice(src);
            }
        }
    }

    pub fn peak(&self) -> f32 {
        self.data
            .iter()
            .flat_map(|c| c.iter())
            .fold(0.0f32, |m, s| m.max(s.abs()))
    }

    pub fn rms(&self) -> f32 {
        let n = (self.frames() * self.channels()).max(1);
        let sum: f64 = self
            .data
            .iter()
            .flat_map(|c| c.iter())
            .map(|s| (*s as f64) * (*s as f64))
            .sum();
        (sum / n as f64).sqrt() as f32
    }

    /// Multiply every sample by `g`.
    pub fn scale(&mut self, g: f32) {
        for c in &mut self.data {
            for s in c.iter_mut() {
                *s *= g;
            }
        }
    }

    /// Change the channel count: mono→N duplicates, N→mono averages, otherwise
    /// truncates or repeats the last channel.
    pub fn with_channels(&self, n: usize) -> AudioBuffer {
        let n = n.max(1);
        let cur = self.channels();
        if n == cur {
            return self.clone();
        }
        let data = if n == 1 {
            vec![self.mono_mix()]
        } else if cur == 1 {
            vec![self.data[0].clone(); n]
        } else {
            (0..n).map(|i| self.data[i.min(cur - 1)].clone()).collect()
        };
        AudioBuffer {
            sample_rate: self.sample_rate,
            data,
        }
    }

    /// Resize every channel to `frames`, padding with zeros.
    pub fn resize(&mut self, frames: usize) {
        for c in &mut self.data {
            c.resize(frames, 0.0);
        }
    }

    pub fn info(&self) -> BufferInfo {
        BufferInfo {
            sample_rate: self.sample_rate,
            channels: self.channels(),
            frames: self.frames(),
            duration_s: self.duration_s(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct BufferInfo {
    pub sample_rate: u32,
    pub channels: usize,
    pub frames: usize,
    pub duration_s: f64,
}

/// The unit of data that flows through a pipeline. v1 only has `Audio`; `Video` and
/// `Subtitle` are additive in v2 (see the PRD's v2 video path).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum MediaBuffer {
    Audio(AudioBuffer),
}

impl MediaBuffer {
    pub fn as_audio(&self) -> Option<&AudioBuffer> {
        match self {
            MediaBuffer::Audio(a) => Some(a),
        }
    }

    pub fn into_audio(self) -> Option<AudioBuffer> {
        match self {
            MediaBuffer::Audio(a) => Some(a),
        }
    }
}

/// Convert decibels to linear gain.
#[inline]
pub fn db_to_lin(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

/// Convert linear gain to decibels (floored at -240 dB).
#[inline]
pub fn lin_to_db(lin: f64) -> f64 {
    if lin <= 1e-12 {
        -240.0
    } else {
        20.0 * lin.log10()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interleave_roundtrip() {
        let b = AudioBuffer::from_planar(48000, vec![vec![1.0, 2.0, 3.0], vec![4.0, 5.0, 6.0]]);
        let i = b.to_interleaved();
        assert_eq!(i, vec![1.0, 4.0, 2.0, 5.0, 3.0, 6.0]);
        assert_eq!(AudioBuffer::from_interleaved(48000, 2, &i), b);
    }

    #[test]
    fn channels_conversion() {
        let b = AudioBuffer::from_planar(48000, vec![vec![1.0, 0.0], vec![0.0, 1.0]]);
        assert_eq!(b.with_channels(1).data[0], vec![0.5, 0.5]);
        assert_eq!(b.with_channels(1).with_channels(2).channels(), 2);
    }

    #[test]
    fn db_conversions() {
        assert!((db_to_lin(-6.0206) - 0.5).abs() < 1e-4);
        assert!((lin_to_db(0.5) + 6.0206).abs() < 1e-3);
    }
}
