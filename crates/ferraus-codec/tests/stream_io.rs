//! `CodecIo`'s streaming reader and writer (used by `separate` for long inputs) give exactly
//! what the whole-buffer `read` and `write` give: the same samples, the same file bytes.

use ferraus_codec::CodecIo;
use ferraus_core::ctx::{AudioIo, EncodeOptions};
use ferraus_core::testing::TempDir;
use ferraus_core::AudioBuffer;

fn signal(sr: u32, frames: usize) -> AudioBuffer {
    let l: Vec<f32> =
        (0..frames).map(|i| ((i as f32) * 0.013).sin() * 0.7 + ((i * 31 % 101) as f32 - 50.0) * 1e-3).collect();
    let r: Vec<f32> =
        (0..frames).map(|i| ((i as f32) * 0.0021).cos() * 0.5 - ((i * 17 % 89) as f32 - 44.0) * 2e-3).collect();
    AudioBuffer::from_planar(sr, vec![l, r])
}

fn stream_write(path: &std::path::Path, b: &AudioBuffer, opts: &EncodeOptions, block: usize) {
    let mut w =
        CodecIo.open_writer(path, b.sample_rate, b.channels(), b.frames() as u64, opts).unwrap().expect("streams");
    let mut a = 0;
    while a < b.frames() {
        let e = (a + block).min(b.frames());
        let planes: Vec<&[f32]> = b.data.iter().map(|c| &c[a..e]).collect();
        w.write(&planes).unwrap();
        a = e;
    }
    w.finish().unwrap();
}

#[test]
fn streamed_wav_and_flac_are_bit_identical_to_whole_buffer_writes() {
    let td = TempDir::new("stream-io");
    let b = signal(44_100, 200_003);
    let cases: Vec<(&str, EncodeOptions)> = vec![
        ("wav", EncodeOptions { format: Some("wav".into()), bit_depth: Some(16), ..Default::default() }),
        ("wav", EncodeOptions { format: Some("wav".into()), bit_depth: Some(24), ..Default::default() }),
        (
            "wav",
            EncodeOptions {
                format: Some("wav".into()),
                bit_depth: Some(24),
                dither: Some("none".into()),
                ..Default::default()
            },
        ),
        (
            "wav",
            EncodeOptions {
                format: Some("wav".into()),
                bit_depth: Some(16),
                dither: Some("shaped".into()),
                seed: 7,
                ..Default::default()
            },
        ),
        ("wav", EncodeOptions { format: Some("wav".into()), bit_depth: Some(32), float: true, ..Default::default() }),
        ("flac", EncodeOptions { format: Some("flac".into()), bit_depth: Some(16), ..Default::default() }),
        (
            "flac",
            EncodeOptions {
                format: Some("flac".into()),
                bit_depth: Some(24),
                dither: Some("none".into()),
                ..Default::default()
            },
        ),
        ("aiff", EncodeOptions { format: Some("aiff".into()), bit_depth: Some(24), ..Default::default() }),
    ];
    for (i, (ext, opts)) in cases.iter().enumerate() {
        let whole = td.path(&format!("w{i}.{ext}"));
        CodecIo.write(&whole, &b, opts).unwrap();
        let want = std::fs::read(&whole).unwrap();
        for block in [1_000usize, 4_096, 65_536, 1 << 20] {
            let p = td.path(&format!("s{i}-{block}.{ext}"));
            stream_write(&p, &b, opts, block);
            assert!(
                std::fs::read(&p).unwrap() == want,
                "{ext} {opts:?}: block {block} differs from the whole-buffer file"
            );
        }
    }
}

#[test]
fn streamed_reads_concatenate_to_the_whole_file() {
    let td = TempDir::new("stream-read");
    let b = signal(48_000, 123_457);
    for ext in ["wav", "flac"] {
        let p = td.path(&format!("in.{ext}"));
        CodecIo
            .write(&p, &b, &EncodeOptions { format: Some(ext.into()), bit_depth: Some(24), ..Default::default() })
            .unwrap();
        let (whole, info) = CodecIo.read(&p).unwrap();
        let mut r = CodecIo.open_reader(&p).unwrap().expect("streams");
        assert_eq!(r.info().sample_rate, info.sample_rate);
        assert_eq!(r.frames_hint(), Some(whole.frames() as u64));
        let mut got = vec![Vec::new(); whole.channels()];
        while let Some(c) = r.next_chunk().unwrap() {
            for (g, s) in got.iter_mut().zip(&c.data) {
                g.extend_from_slice(s);
            }
        }
        assert!(got == whole.data, "{ext}: streamed samples differ");
    }
}

#[test]
fn formats_that_need_the_whole_buffer_do_not_stream() {
    let td = TempDir::new("stream-none");
    // Opus encodes at 48 kHz: a 44.1 kHz signal must be resampled whole
    let opts = EncodeOptions { format: Some("opus".into()), ..Default::default() };
    let r = CodecIo.open_writer(&td.path("x.opus"), 44_100, 2, 1000, &opts);
    assert!(matches!(r, Ok(None) | Err(_)));
    assert!(!td.path("x.opus").exists(), "nothing is created when the writer does not stream");
}
