//! DeepFilterNet 3 with the real model, compared with RNNoise on synthetic speech + noise.
//!
//! Runs when `denoise/dfn3` is in the model cache, or downloads it when
//! `FERRAUS_TEST_MODELS=1`; otherwise prints a note and passes.

#![cfg(feature = "dfn3")]

mod common;

use common::*;
use ferraus_core::testing::{self, si_sdr};
use ferraus_core::{AudioBuffer, OpSpec};

fn spec(name: &str) -> OpSpec {
    ferraus_denoise::ops().into_iter().find(|o| o.name == name).unwrap()
}

fn available() -> bool {
    let want = std::env::var("FERRAUS_TEST_MODELS").map(|v| v.trim() == "1").unwrap_or(false);
    if want {
        std::env::set_var("FERRAUS_AUTO_DOWNLOAD", "1");
        return true;
    }
    let cache = ferraus_models::Cache::default_root();
    let m = ferraus_models::active_registry(&cache).manifest;
    let cached = cache.cached(m.resolve(ferraus_denoise::dfn3::MODEL).unwrap()).is_some();
    if !cached {
        eprintln!("skipping: denoise/dfn3 is not cached (FERRAUS_TEST_MODELS=1 downloads it)");
    }
    cached
}

/// Two-pole resonator (Klatt): unity gain at DC.
struct Resonator {
    y1: f64,
    y2: f64,
}

impl Resonator {
    fn tick(&mut self, x: f64, f: f64, bw: f64) -> f64 {
        let sr = SR as f64;
        let c = -(-2.0 * std::f64::consts::PI * bw / sr).exp();
        let b = 2.0 * (-std::f64::consts::PI * bw / sr).exp() * (2.0 * std::f64::consts::PI * f / sr).cos();
        let a = 1.0 - b - c;
        let y = a * x + b * self.y1 + c * self.y2;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// A source-filter voice closer to real speech than `speech_like`: a jittered glottal pulse
/// train (−12 dB/octave) with aspiration noise and a declining phrase pitch, a cascade of
/// four formant resonators gliding between vowels, lip radiation, syllable envelopes, and
/// fricative noise bursts between words. `lead_s` seconds of silence first.
fn voice_like(lead_s: f64, secs: f64, amp: f32, seed: u64) -> AudioBuffer {
    let sr = SR as f64;
    let n_lead = (lead_s * sr) as usize;
    let n = (secs * sr) as usize;
    let vowels: [[(f64, f64); 4]; 5] = [
        [(730.0, 80.0), (1090.0, 90.0), (2440.0, 120.0), (3400.0, 200.0)],
        [(270.0, 60.0), (2290.0, 90.0), (3010.0, 120.0), (3700.0, 200.0)],
        [(300.0, 60.0), (870.0, 80.0), (2240.0, 110.0), (3300.0, 200.0)],
        [(530.0, 70.0), (1840.0, 90.0), (2480.0, 120.0), (3500.0, 200.0)],
        [(570.0, 70.0), (840.0, 80.0), (2410.0, 120.0), (3400.0, 200.0)],
    ];
    let mut rng = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut rnd = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        (rng >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
    };
    // syllables (start, duration, vowel) in words separated by pauses with fricatives
    let mut sched = Vec::new();
    let mut fric = Vec::new();
    let mut t = 0.1;
    let mut k = 0;
    while t < secs - 0.4 {
        let syl = 2 + k % 3;
        for _ in 0..syl {
            let d = 0.12 + 0.08 * (rnd() * 0.5 + 0.5);
            if t + d > secs - 0.1 {
                break;
            }
            sched.push((t, d, k % 5));
            k += 1;
            t += d + 0.03;
        }
        fric.push((t + 0.02, 0.07));
        t += 0.25;
    }
    let mut out = vec![0f32; n_lead + n];
    let (mut p, mut lp1, mut lp2, mut prev) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
    let mut res: Vec<Resonator> = (0..4).map(|_| Resonator { y1: 0.0, y2: 0.0 }).collect();
    let mut hp = Resonator { y1: 0.0, y2: 0.0 };
    let mut jitter = 0.0f64;
    let mut pulse_amp = 1.0;
    for i in 0..n {
        let t = i as f64 / sr;
        let phrase = (t % 2.5) / 2.5;
        let f0 = (175.0 - 60.0 * phrase) * (1.0 + jitter);
        // syllable envelope and formant targets (gliding over the first 40 ms)
        let cur = sched.iter().position(|s| t >= s.0 && t < s.0 + s.1);
        let (env, fm) = match cur {
            Some(si) => {
                let (s0, d, v) = sched[si];
                let ts = (t - s0) / d;
                let env = (std::f64::consts::PI * ts).sin().powf(0.5);
                let prev_v = if si > 0 { sched[si - 1].2 } else { v };
                let g = ((t - s0) / 0.04).min(1.0);
                let mut fm = [(0.0, 0.0); 4];
                for (j, f) in fm.iter_mut().enumerate() {
                    *f = (
                        vowels[prev_v][j].0 + (vowels[v][j].0 - vowels[prev_v][j].0) * g,
                        vowels[v][j].1,
                    );
                }
                (env, fm)
            }
            None => (0.0, vowels[0]),
        };
        p += f0 / sr;
        let mut impulse = 0.0;
        if p >= 1.0 {
            p -= 1.0;
            impulse = pulse_amp;
            jitter = (jitter + 0.004 * rnd()).clamp(-0.02, 0.02);
            pulse_amp = 1.0 + 0.05 * rnd();
        }
        // −12 dB/octave glottal spectrum: two one-pole low-passes at ~100 Hz
        let a = (-2.0 * std::f64::consts::PI * 100.0 / sr).exp();
        lp1 = a * lp1 + (1.0 - a) * impulse * 400.0;
        lp2 = a * lp2 + (1.0 - a) * lp1;
        let aspiration = 0.02 * rnd() * (0.5 + 0.5 * (2.0 * std::f64::consts::PI * p).cos());
        let mut s = (lp2 + aspiration) * env;
        for (r, (f, bw)) in res.iter_mut().zip(fm.iter()) {
            s = r.tick(s, *f, *bw);
        }
        // lip radiation (+6 dB/octave)
        let voiced = s - prev;
        prev = s;
        // fricatives: high-passed noise (resonance near 5 kHz)
        let fr = fric.iter().find(|f| t >= f.0 && t < f.0 + f.1).map(|f| (std::f64::consts::PI * (t - f.0) / f.1).sin());
        let fn_ = match fr {
            Some(e) => 0.25 * e * hp.tick(rnd(), 5000.0, 2500.0),
            None => {
                hp.tick(0.0, 5000.0, 2500.0);
                0.0
            }
        };
        out[n_lead + i] = (voiced * 40.0 + fn_) as f32;
    }
    let m = out.iter().fold(0f32, |a, v| a.max(v.abs()));
    for v in out.iter_mut() {
        *v *= amp / m.max(1e-12);
    }
    AudioBuffer::from_mono(SR, out)
}

fn run(model: &str, x: &AudioBuffer) -> AudioBuffer {
    testing::run(&spec("denoise"), &[("mode", "ml"), ("model", model)], x.clone()).unwrap()
}

/// SI-SDR improvement (dB) of dfn3 and rnnoise for every noise kind at 0, 5 and 10 dB SNR.
fn table(label: &str, clean: &AudioBuffer, lead: usize) -> Vec<(NoiseKind, f64, f64, f64)> {
    let c = &clean.data[0];
    let secs = c.len() as f64 / SR as f64 + 0.1;
    let mut rows = Vec::new();
    for kind in NoiseKind::ALL {
        for snr in [0.0, 5.0, 10.0] {
            let nz = make_noise(kind, secs, 21);
            let (noisy, _) = mix(clean, &nz, snr, lead);
            let before = si_sdr(c, &noisy.data[0]);
            let d = run("dfn3", &noisy);
            let r = run("rnnoise", &noisy);
            assert_eq!(d.frames(), noisy.frames());
            let (gd, gr) = (si_sdr(c, &d.data[0]) - before, si_sdr(c, &r.data[0]) - before);
            eprintln!("{label} {kind:?} {snr:>4} dB: input {before:6.2}  dfn3 {gd:+6.2}  rnnoise {gr:+6.2}");
            rows.push((kind, snr, gd, gr));
        }
    }
    let n = rows.len() as f64;
    let (md, mr) = (rows.iter().map(|r| r.2).sum::<f64>() / n, rows.iter().map(|r| r.3).sum::<f64>() / n);
    eprintln!("{label}: mean SI-SDR improvement dfn3 {md:+.2} dB, rnnoise {mr:+.2} dB");
    rows
}

#[test]
fn dfn3_beats_rnnoise_on_speech_in_noise() {
    if !available() {
        return;
    }
    // the source-filter voice: dfn3 wins every condition by a clear margin
    let voice = voice_like(1.0, 5.0, 0.5, 3);
    let rows = table("voice_like", &voice, SR as usize);
    for (kind, snr, gd, gr) in &rows {
        assert!(*gd > 3.0, "{kind:?} at {snr} dB: dfn3 improves SI-SDR by only {gd:.2} dB");
        assert!(gd > gr, "{kind:?} at {snr} dB: dfn3 {gd:+.2} dB vs rnnoise {gr:+.2} dB");
    }
    let n = rows.len() as f64;
    let (md, mr) = (rows.iter().map(|r| r.2).sum::<f64>() / n, rows.iter().map(|r| r.3).sum::<f64>() / n);
    assert!(md > mr + 3.0, "mean improvement dfn3 {md:.2} dB vs rnnoise {mr:.2} dB");

    // the plain harmonic `speech_like` signal (phase-locked sine partials, no jitter or
    // noise) is far from real speech: dfn3 still wins on broadband noise, but under a
    // stronger 60 Hz hum series it treats the synthetic voice as part of the tonal noise
    // (real speech with the same hum improves by 14-17 dB, see the ADR 0006 notes)
    let plain = speech_like(1.0, 4.0, 0.5);
    for (kind, snr, gd, gr) in table("speech_like", &plain.buf, SR as usize) {
        if kind != NoiseKind::Hum {
            assert!(gd > gr, "{kind:?} at {snr} dB: dfn3 {gd:+.2} dB vs rnnoise {gr:+.2} dB");
        }
    }
}

#[test]
fn dfn3_keeps_clean_speech_and_removes_noise_only() {
    if !available() {
        return;
    }
    let clean = voice_like(0.5, 3.0, 0.5, 5);
    let y = run("dfn3", &clean);
    let s = si_sdr(&clean.data[0], &y.data[0]);
    eprintln!("clean voice through dfn3: SI-SDR {s:.2} dB");
    assert!(s > 12.0, "clean voice SI-SDR {s:.2}");
    // aligned in time: the cross-correlation peaks at lag 0
    let c = &clean.data[0];
    let corr = |lag: isize| -> f64 {
        (2000..c.len() - 2000).map(|i| c[i] as f64 * y.data[0][(i as isize + lag) as usize] as f64).sum()
    };
    let best = (-200..=200).max_by(|a, b| corr(*a).partial_cmp(&corr(*b)).unwrap()).unwrap();
    assert_eq!(best, 0, "output is shifted by {best} samples");

    let nz: Vec<f32> = make_noise(NoiseKind::Hiss, 3.0, 4).iter().map(|v| v * 0.05).collect();
    let y = run("dfn3", &AudioBuffer::from_mono(SR, nz.clone()));
    let red = 20.0 * (rms(&y.data[0][SR as usize..]) / rms(&nz[SR as usize..])).log10();
    eprintln!("hiss only: reduced by {red:.1} dB");
    assert!(red < -15.0, "noise only reduced by {red:.1} dB");
}

#[test]
fn dfn3_long_input_is_segmented_without_seams() {
    if !available() {
        return;
    }
    // 70 s at 16 kHz stereo: resampling, two channels and three inference segments
    let sr = 16_000;
    let clean = speech_like(0.0, 70.0, 0.4);
    let nz = make_noise(NoiseKind::Fan, 70.0, 8);
    let (noisy, _) = mix(&clean.buf, &nz, 5.0, 0);
    let x16 = ferraus_core::resample::resample(&noisy, sr, ferraus_core::resample::Quality::High);
    let c16 = ferraus_core::resample::resample(&clean.buf, sr, ferraus_core::resample::Quality::High);
    let st = testing::stereo(x16.clone());
    let t0 = std::time::Instant::now();
    let y = run("dfn3", &st);
    let secs = t0.elapsed().as_secs_f64();
    eprintln!("70 s stereo in {secs:.1} s (real-time factor {:.3})", secs / 140.0);
    assert_eq!((y.channels(), y.frames()), (2, st.frames()));
    assert_eq!(y.data[0], y.data[1]);
    let before = si_sdr(&c16.data[0], &x16.data[0]);
    let after = si_sdr(&c16.data[0], &y.data[0]);
    eprintln!("long input: SI-SDR {before:.2} -> {after:.2}");
    assert!(after > before + 3.0);
    // around the segment boundaries (30 s, 60 s minus crossfade) the local SI-SDR is as good
    // as elsewhere
    let local = |t: f64| {
        let a = ((t - 0.5) * sr as f64) as usize;
        let b = ((t + 0.5) * sr as f64) as usize;
        si_sdr(&c16.data[0][a..b], &y.data[0][a..b])
    };
    for t in [29.5, 59.0] {
        let at = local(t);
        let near = (local(t - 3.0) + local(t + 3.0)) / 2.0;
        eprintln!("segment boundary {t} s: local SI-SDR {at:.2} vs {near:.2} nearby");
        assert!(at > near - 3.0, "boundary at {t} s: {at:.2} vs {near:.2}");
    }
}
