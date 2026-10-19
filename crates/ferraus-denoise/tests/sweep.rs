//! Development sweep (ignored): prints SI-SDR gains for parameter combinations and an
//! oracle ideal-binary-mask bound. Run with
//! `cargo test -p ferraus-denoise --test sweep -- --ignored --nocapture`.

mod common;

use common::*;
use ferraus_core::testing::{self, si_sdr};
use ferraus_core::AudioBuffer;
use ferraus_denoise::stft::{fft_size_for, hop_for, Fft};
use ferraus_denoise::stft::Complex32;

fn oracle(clean: &[f32], nz: &[f32], gmin: f32) -> Vec<f32> {
    let n = fft_size_for(SR);
    let hop = hop_for(n);
    let mut fft = Fft::new(n);
    let bins = fft.bins();
    let pad = n - hop;
    let total = clean.len();
    let padv = |x: &[f32]| {
        let mut v = vec![0f32; pad];
        v.extend_from_slice(x);
        v.resize(pad + total + n, 0.0);
        v
    };
    let (c, z) = (padv(clean), padv(nz));
    let x: Vec<f32> = c.iter().zip(&z).map(|(a, b)| a + b).collect();
    let mut y = vec![0f32; x.len() + n];
    let (mut sc, mut sz, mut sx) = (
        vec![Complex32::default(); bins],
        vec![Complex32::default(); bins],
        vec![Complex32::default(); bins],
    );
    let mut s = 0;
    while s + n <= x.len() {
        fft.forward(&c[s..s + n], &mut sc);
        fft.forward(&z[s..s + n], &mut sz);
        fft.forward(&x[s..s + n], &mut sx);
        for k in 0..bins {
            if sc[k].norm_sqr() <= sz[k].norm_sqr() {
                sx[k] *= gmin;
            }
        }
        let t = fft.inverse_windowed(&mut sx);
        for (o, v) in y[s..s + n].iter_mut().zip(t) {
            *o += v;
        }
        s += hop;
    }
    y[pad..pad + total].to_vec()
}

#[test]
#[ignore]
fn sweep() {
    let combos: Vec<Vec<(&str, &str)>> = vec![
        vec![],
        vec![("smoothing", "0")],
        vec![("artifact-control", "0%")],
        vec![("preserve-transients", "off")],
        vec![("release", "40ms")],
        vec![("sensitivity", "3")],
        vec![("sensitivity", "10")],
        vec![("reduction", "18dB")],
        vec![("reduction", "24dB")],
    ];
    let lead = SR as usize;
    for signal in ["speech", "music"] {
        let clean = if signal == "speech" {
            speech_like(1.0, 2.0, 0.5)
        } else {
            music_like(1.0, 2.0, 0.5)
        };
        for kind in NoiseKind::ALL {
            let nz = make_noise(kind, 3.0, 17);
            let (noisy, scaled) = mix(&clean.buf, &nz, 10.0, lead);
            let c = &clean.buf.data[0];
            let full = std::env::var("FULL").is_ok();
            let lead = if full { 0 } else { lead };
            let before = si_sdr(&c[lead..], &noisy.data[0][lead..]);
            let o12 = oracle(c, &scaled, 10f32.powf(-12.0 / 20.0));
            let oinf = oracle(c, &scaled, 0.0);
            let mut line = format!(
                "{signal:6} {:7} oracle12 {:+5.2} oracleInf {:+5.2} |",
                format!("{kind:?}"),
                si_sdr(&c[lead..], &o12[lead..]) - before,
                si_sdr(&c[lead..], &oinf[lead..]) - before
            );
            for pairs in &combos {
                let mut p = pairs.clone();
                p.push(("noise-range", "0s..1s"));
                let spec = ferraus_denoise::ops().remove(0);
                let out: AudioBuffer = testing::run(&spec, &p, noisy.clone()).unwrap();
                let g = si_sdr(&c[lead..], &out.data[0][lead..]) - before;
                let r = retained_db(c, &out.data[0], &clean.voiced);
                line.push_str(&format!(" {g:+5.2}/{r:+4.1}"));
            }
            eprintln!("{line}");
        }
    }
}
