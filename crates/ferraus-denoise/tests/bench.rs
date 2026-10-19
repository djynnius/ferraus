//! Speed benchmark for the profile path (ignored; run in release):
//! `cargo test -p ferraus-denoise --release --test bench -- --ignored --nocapture`.
//! PRD: at least 20x real time single-threaded. The engine runs on one thread.

use ferraus_core::testing;
use ferraus_core::AudioBuffer;
use ferraus_denoise::engine::{Engine, GateConfig, NoiseModel};
use ferraus_denoise::profile;
use std::time::Instant;

fn bench(sr: u32, channels: usize, secs: f64, model: &str) -> f64 {
    let tone = testing::sine(sr, 440.0, secs, 0.3);
    let nz = testing::noise(sr, secs, 0.02, 1);
    let mono = testing::add(&tone, &nz);
    let buf = if channels == 2 {
        AudioBuffer::from_planar(sr, vec![mono.data[0].clone(), mono.data[0].clone()])
    } else {
        mono
    };
    let noise = match model {
        "profile" => {
            let n = testing::noise(sr, 1.0, 0.02, 2);
            let n = if channels == 2 { testing::stereo(n) } else { n };
            NoiseModel::Profile(Box::new(profile::learn(&n, 0, n.frames(), Default::default()).unwrap()))
        }
        _ => NoiseModel::Adaptive { noisiness: 0.3, floor_db: 0.0 },
    };
    let mut e = Engine::new(GateConfig::default(), &noise, sr, channels);
    let t = Instant::now();
    let out = e.process_buffer(&buf, &mut |_| Ok(())).unwrap();
    let el = t.elapsed().as_secs_f64();
    assert_eq!(out.frames(), buf.frames());
    let x = secs / el;
    eprintln!("{model:8} {sr} Hz x{channels}: {secs:.0} s of audio in {el:.3} s = {x:.0}x real time");
    x
}

#[test]
#[ignore]
fn profile_path_is_at_least_20x_real_time() {
    let mono = bench(48_000, 1, 120.0, "profile");
    let stereo = bench(48_000, 2, 120.0, "profile");
    let _ = bench(44_100, 2, 120.0, "profile");
    let _ = bench(48_000, 2, 120.0, "adaptive");
    let _ = bench(96_000, 2, 60.0, "profile");
    assert!(mono >= 20.0, "{mono:.1}x");
    assert!(stereo >= 20.0, "{stereo:.1}x");
}
