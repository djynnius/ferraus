//! A minimal, dependency-free WAV reader/writer (PCM 8/16/24/32 and float 32/64). It
//! backs `SimpleIo`, which every crate uses in tests; the real codec layer lives in
//! `ferraus-codec`.

use crate::buffer::AudioBuffer;
use crate::ctx::{AudioIo, AudioReader, AudioWriter, EncodeOptions, SourceInfo};
use crate::error::{Error, Result};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

/// Format tag and bits per sample for a requested depth.
fn format_of(bit_depth: u16, float: bool) -> Result<(u16, u16)> {
    if float || bit_depth == 32 || bit_depth == 64 {
        Ok((3u16, if bit_depth == 64 { 64u16 } else { 32u16 }))
    } else if [8, 16, 24].contains(&bit_depth) {
        Ok((1u16, bit_depth))
    } else {
        Err(Error::unsupported(format!("WAV bit depth {bit_depth} is not supported (8, 16, 24, 32 float)")))
    }
}

fn header(out: &mut Vec<u8>, sample_rate: u32, ch: u16, frames: usize, fmt_tag: u16, bits: u16) {
    let bps = (bits / 8) as usize;
    let data_len = frames * ch as usize * bps;
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&fmt_tag.to_le_bytes());
    out.extend_from_slice(&ch.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&(sample_rate * ch as u32 * bps as u32).to_le_bytes());
    out.extend_from_slice(&(ch * bps as u16).to_le_bytes());
    out.extend_from_slice(&bits.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data_len as u32).to_le_bytes());
}

/// Append frames `[channel][frame]` as interleaved samples.
fn samples(out: &mut Vec<u8>, data: &[&[f32]], fmt_tag: u16, bits: u16) {
    let frames = data.first().map(|c| c.len()).unwrap_or(0);
    for f in 0..frames {
        for c in data {
            let s = c[f];
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
}

pub fn encode(buf: &AudioBuffer, bit_depth: u16, float: bool) -> Result<Vec<u8>> {
    let (fmt_tag, bits) = format_of(bit_depth, float)?;
    let mut out = Vec::with_capacity(44 + buf.frames() * buf.channels() * (bits / 8) as usize);
    header(&mut out, buf.sample_rate, buf.channels() as u16, buf.frames(), fmt_tag, bits);
    let planes: Vec<&[f32]> = buf.data.iter().map(|c| c.as_slice()).collect();
    samples(&mut out, &planes, fmt_tag, bits);
    Ok(out)
}

/// Streaming writer: the header (sizes known up front), then each block as it comes.
struct WavWriter {
    file: std::io::BufWriter<std::fs::File>,
    fmt_tag: u16,
    bits: u16,
    buf: Vec<u8>,
}

impl AudioWriter for WavWriter {
    fn write(&mut self, planes: &[&[f32]]) -> Result<()> {
        self.buf.clear();
        samples(&mut self.buf, planes, self.fmt_tag, self.bits);
        self.file.write_all(&self.buf)?;
        Ok(())
    }
    fn finish(mut self: Box<Self>) -> Result<()> {
        self.file.flush()?;
        Ok(())
    }
}

/// Streaming reader over the `data` chunk of a PCM/float WAV file.
struct WavReader {
    file: std::fs::File,
    info: SourceInfo,
    tag: u16,
    bits: u16,
    left: u64,
    frames: u64,
}

impl WavReader {
    fn open(path: &Path) -> Result<WavReader> {
        let bad = || Error::unsupported("not a valid WAV file");
        let mut file = std::fs::File::open(path)?;
        let mut head = [0u8; 12];
        file.read_exact(&mut head).map_err(|_| bad())?;
        if &head[0..4] != b"RIFF" || &head[8..12] != b"WAVE" {
            return Err(bad());
        }
        let mut fmt: Option<(u16, u16, u32, u16)> = None;
        loop {
            let mut ck = [0u8; 8];
            file.read_exact(&mut ck).map_err(|_| bad())?;
            let len = u32::from_le_bytes(ck[4..8].try_into().unwrap()) as u64;
            if &ck[0..4] == b"data" {
                let (tag, ch, sr, bits) = fmt.ok_or_else(bad)?;
                let frame = (bits as u64).div_ceil(8) * ch as u64;
                if ch == 0 || sr == 0 || frame == 0 {
                    return Err(bad());
                }
                let avail = file.metadata()?.len().saturating_sub(file.stream_position()?);
                let frames = len.min(avail) / frame;
                let info = SourceInfo {
                    path: path.display().to_string(),
                    container: Some("wav".into()),
                    codec: Some("pcm".into()),
                    duration_s: frames as f64 / sr as f64,
                    sample_rate: sr,
                    channels: ch as usize,
                    bit_depth: Some(bits),
                    ..Default::default()
                };
                return Ok(WavReader { file, info, tag, bits, left: frames * frame, frames });
            }
            let mut body = vec![0u8; len as usize + (len & 1) as usize];
            if &ck[0..4] == b"fmt " && len >= 16 {
                file.read_exact(&mut body).map_err(|_| bad())?;
                let mut tag = u16::from_le_bytes([body[0], body[1]]);
                if tag == 0xFFFE && body.len() >= 26 {
                    tag = u16::from_le_bytes([body[24], body[25]]);
                }
                let sr = u32::from_le_bytes(body[4..8].try_into().unwrap());
                fmt = Some((tag, u16::from_le_bytes([body[2], body[3]]), sr, u16::from_le_bytes([body[14], body[15]])));
            } else {
                file.seek(SeekFrom::Current(body.len() as i64))?;
            }
        }
    }
}

impl AudioReader for WavReader {
    fn info(&self) -> &SourceInfo {
        &self.info
    }
    fn frames_hint(&self) -> Option<u64> {
        Some(self.frames)
    }
    fn next_chunk(&mut self) -> Result<Option<AudioBuffer>> {
        let ch = self.info.channels;
        let frame = (self.bits as usize).div_ceil(8) * ch;
        let n = (self.left as usize).min(frame * 65_536);
        if n == 0 {
            return Ok(None);
        }
        let mut bytes = vec![0u8; n];
        self.file.read_exact(&mut bytes)?;
        self.left -= n as u64;
        // reuse the whole-file decoder on a minimal header + this chunk's data
        let mut wav = Vec::with_capacity(44 + n);
        header(&mut wav, self.info.sample_rate, ch as u16, n / frame, self.tag, self.bits);
        wav.extend_from_slice(&bytes);
        decode(&wav).map(|(b, _, _)| Some(b))
    }
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
    fn open_reader(&self, path: &Path) -> Result<Option<Box<dyn AudioReader>>> {
        Ok(Some(Box::new(WavReader::open(path)?)))
    }
    fn open_writer(
        &self,
        path: &Path,
        sample_rate: u32,
        channels: usize,
        frames: u64,
        opts: &EncodeOptions,
    ) -> Result<Option<Box<dyn AudioWriter>>> {
        let bits = opts.bit_depth.unwrap_or(32);
        let (fmt_tag, bits) = format_of(bits, opts.float || bits == 32)?;
        let mut head = Vec::with_capacity(44);
        header(&mut head, sample_rate, channels as u16, frames as usize, fmt_tag, bits);
        let mut file = std::io::BufWriter::new(std::fs::File::create(path)?);
        file.write_all(&head)?;
        Ok(Some(Box::new(WavWriter { file, fmt_tag, bits, buf: Vec::new() })))
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
    fn streaming_reader_and_writer_match_whole_buffer_io() {
        let dir = std::env::temp_dir().join(format!("ferraus-wav-stream-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let n = 150_001;
        let b = AudioBuffer::from_planar(
            48_000,
            vec![
                (0..n).map(|i| ((i as f32) * 0.01).sin() * 0.8).collect(),
                (0..n).map(|i| (i % 97) as f32 / 97.0 - 0.5).collect(),
            ],
        );
        for (bits, float) in [(16u16, false), (24, false), (32, true)] {
            let opts = EncodeOptions { bit_depth: Some(bits), float, ..Default::default() };
            let whole = dir.join(format!("whole{bits}.wav"));
            SimpleIo.write(&whole, &b, &opts).unwrap();
            let streamed = dir.join(format!("streamed{bits}.wav"));
            let mut w = SimpleIo.open_writer(&streamed, b.sample_rate, 2, n as u64, &opts).unwrap().unwrap();
            let mut a = 0;
            while a < n {
                let e = (a + 7_777).min(n);
                w.write(&[&b.data[0][a..e], &b.data[1][a..e]]).unwrap();
                a = e;
            }
            w.finish().unwrap();
            assert_eq!(std::fs::read(&whole).unwrap(), std::fs::read(&streamed).unwrap(), "{bits} bits");
            let mut r = SimpleIo.open_reader(&streamed).unwrap().unwrap();
            assert_eq!(r.frames_hint(), Some(n as u64));
            let mut got = vec![Vec::new(), Vec::new()];
            while let Some(c) = r.next_chunk().unwrap() {
                for (g, s) in got.iter_mut().zip(&c.data) {
                    g.extend_from_slice(s);
                }
            }
            assert_eq!(got, SimpleIo.read(&streamed).unwrap().0.data);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_garbage() {
        assert!(decode(b"hello world, not a wav").is_err());
        assert!(decode(&[]).is_err());
    }
}
