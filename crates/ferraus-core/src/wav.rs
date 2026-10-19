//! A minimal, dependency-free WAV reader/writer (PCM 8/16/24/32 and float 32/64). It
//! backs `SimpleIo`, which every crate uses in tests; the real codec layer lives in
//! `ferraus-codec`.

use crate::buffer::AudioBuffer;
use crate::ctx::{AudioIo, EncodeOptions, SourceInfo};
use crate::error::{Error, Result};
use std::path::Path;

pub fn encode(buf: &AudioBuffer, bit_depth: u16, float: bool) -> Result<Vec<u8>> {
    let ch = buf.channels() as u16;
    let frames = buf.frames();
    let (fmt_tag, bits) = if float || bit_depth == 32 || bit_depth == 64 {
        (3u16, if bit_depth == 64 { 64u16 } else { 32u16 })
    } else if [8, 16, 24].contains(&bit_depth) {
        (1u16, bit_depth)
    } else {
        return Err(Error::unsupported(format!("WAV bit depth {bit_depth} is not supported (8, 16, 24, 32 float)")));
    };
    let bps = (bits / 8) as usize;
    let data_len = frames * ch as usize * bps;
    let mut out = Vec::with_capacity(44 + data_len);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&fmt_tag.to_le_bytes());
    out.extend_from_slice(&ch.to_le_bytes());
    out.extend_from_slice(&buf.sample_rate.to_le_bytes());
    out.extend_from_slice(&(buf.sample_rate * ch as u32 * bps as u32).to_le_bytes());
    out.extend_from_slice(&(ch * bps as u16).to_le_bytes());
    out.extend_from_slice(&bits.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data_len as u32).to_le_bytes());
    for f in 0..frames {
        for c in 0..ch as usize {
            let s = buf.data[c][f];
            match (fmt_tag, bits) {
                (3, 32) => out.extend_from_slice(&s.to_le_bytes()),
                (3, 64) => out.extend_from_slice(&(s as f64).to_le_bytes()),
                (1, 8) => out.push(((s as f64 * 128.0).round().clamp(-128.0, 127.0) as i16 + 128) as u8),
                (1, 16) => {
                    out.extend_from_slice(&((s as f64 * 32768.0).round().clamp(-32768.0, 32767.0) as i16).to_le_bytes())
                }
                (1, 24) => {
                    let v = (s as f64 * 8_388_608.0).round().clamp(-8_388_608.0, 8_388_607.0) as i32;
                    out.extend_from_slice(&v.to_le_bytes()[..3]);
                }
                _ => unreachable!(),
            }
        }
    }
    Ok(out)
}

pub fn decode(bytes: &[u8]) -> Result<(AudioBuffer, u16, bool)> {
    let bad = || Error::unsupported("not a valid WAV file");
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(bad());
    }
    let mut pos = 12;
    let mut fmt: Option<(u16, u16, u32, u16)> = None;
    let mut data: Option<&[u8]> = None;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let len = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let body_end = (pos + 8 + len).min(bytes.len());
        let body = &bytes[pos + 8..body_end];
        if id == b"fmt " && body.len() >= 16 {
            let mut tag = u16::from_le_bytes([body[0], body[1]]);
            let ch = u16::from_le_bytes([body[2], body[3]]);
            let sr = u32::from_le_bytes(body[4..8].try_into().unwrap());
            let bits = u16::from_le_bytes([body[14], body[15]]);
            if tag == 0xFFFE && body.len() >= 26 {
                tag = u16::from_le_bytes([body[24], body[25]]);
            }
            fmt = Some((tag, ch, sr, bits));
        } else if id == b"data" {
            data = Some(body);
        }
        pos += 8 + len + (len & 1);
    }
    let (tag, ch, sr, bits) = fmt.ok_or_else(bad)?;
    let data = data.ok_or_else(bad)?;
    if ch == 0 || sr == 0 {
        return Err(bad());
    }
    let bps = (bits as usize).div_ceil(8);
    let frame = bps * ch as usize;
    if frame == 0 {
        return Err(bad());
    }
    let frames = data.len() / frame;
    let mut out = vec![Vec::with_capacity(frames); ch as usize];
    for f in 0..frames {
        for (c, chan) in out.iter_mut().enumerate() {
            let o = f * frame + c * bps;
            let b = &data[o..o + bps];
            let v = match (tag, bits) {
                (3, 32) => f32::from_le_bytes(b.try_into().unwrap()),
                (3, 64) => f64::from_le_bytes(b.try_into().unwrap()) as f32,
                (1, 8) => (b[0] as f32 - 128.0) / 128.0,
                (1, 16) => i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0,
                (1, 24) => (i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8) as f32 / 8_388_608.0,
                (1, 32) => i32::from_le_bytes(b.try_into().unwrap()) as f32 / 2_147_483_648.0,
                _ => return Err(Error::unsupported(format!("WAV format tag {tag} with {bits} bits is not supported"))),
            };
            chan.push(v);
        }
    }
    Ok((AudioBuffer { sample_rate: sr, data: out }, bits, tag == 3))
}

/// `AudioIo` that only handles WAV, for tests and minimal builds.
pub struct SimpleIo;

impl AudioIo for SimpleIo {
    fn read(&self, path: &Path) -> Result<(AudioBuffer, SourceInfo)> {
        let bytes = std::fs::read(path)?;
        let (buf, bits, _) = decode(&bytes)?;
        let info = SourceInfo {
            path: path.display().to_string(),
            container: Some("wav".into()),
            codec: Some("pcm".into()),
            duration_s: buf.duration_s(),
            sample_rate: buf.sample_rate,
            channels: buf.channels(),
            bit_depth: Some(bits),
            ..Default::default()
        };
        Ok((buf, info))
    }
    fn read_bytes(&self, bytes: &[u8], _hint: Option<&str>) -> Result<(AudioBuffer, SourceInfo)> {
        let (buf, bits, _) = decode(bytes)?;
        let info = SourceInfo {
            path: "-".into(),
            duration_s: buf.duration_s(),
            sample_rate: buf.sample_rate,
            channels: buf.channels(),
            bit_depth: Some(bits),
            ..Default::default()
        };
        Ok((buf, info))
    }
    fn probe(&self, path: &Path) -> Result<SourceInfo> {
        self.read(path).map(|(_, i)| i)
    }
    fn write(&self, path: &Path, buf: &AudioBuffer, opts: &EncodeOptions) -> Result<()> {
        let bytes = self.write_bytes(buf, opts)?;
        std::fs::write(path, bytes)?;
        Ok(())
    }
    fn write_bytes(&self, buf: &AudioBuffer, opts: &EncodeOptions) -> Result<Vec<u8>> {
        let bits = opts.bit_depth.unwrap_or(32);
        encode(buf, bits, opts.float || bits == 32)
    }
    fn can_encode(&self, ext: &str) -> bool {
        ext == "wav"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_float_and_pcm() {
        let b = AudioBuffer::from_planar(44100, vec![vec![0.0, 0.5, -0.5, 0.25], vec![0.1, -0.1, 0.9, -0.9]]);
        let (d, bits, float) = decode(&encode(&b, 32, true).unwrap()).unwrap();
        assert_eq!(d, b);
        assert_eq!((bits, float), (32, true));
        let (d, _, _) = decode(&encode(&b, 24, false).unwrap()).unwrap();
        for (x, y) in d.data.iter().flatten().zip(b.data.iter().flatten()) {
            assert!((x - y).abs() < 1e-6);
        }
        let (d, _, _) = decode(&encode(&b, 16, false).unwrap()).unwrap();
        for (x, y) in d.data.iter().flatten().zip(b.data.iter().flatten()) {
            assert!((x - y).abs() < 1e-4);
        }
    }

    #[test]
    fn rejects_garbage() {
        assert!(decode(b"hello world, not a wav").is_err());
        assert!(decode(&[]).is_err());
    }
}
