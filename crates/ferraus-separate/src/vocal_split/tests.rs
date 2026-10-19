//! The built-in lead/backing split on synthetic two-voice signals.

use super::*;

const SR: u32 = 22_050;

/// A sung line: harmonics of a vibrato pitch (notes in Hz, `note_s` each) with a vowel-like
/// spectral envelope and soft note onsets.
fn voice(notes: &[f64], note_s: f64, amp: f32, vib_hz: f64, vib_phase: f64) -> Vec<f32> {
    let n = (notes.len() as f64 * note_s * SR as f64) as usize;
    let mut out = vec![0f32; n];
    let mut phase = 0f64;
    for (i, o) in out.iter_mut().enumerate() {
        let t = i as f64 / SR as f64;
        let k = ((t / note_s) as usize).min(notes.len() - 1);
        let cents = 25.0 * (2.0 * std::f64::consts::PI * vib_hz * t + vib_phase).sin();
        let f0 = notes[k] * 2f64.powf(cents / 1200.0);
        phase += 2.0 * std::f64::consts::PI * f0 / SR as f64;
        let local = t - k as f64 * note_s;
        let env = (local / 0.03).min(1.0) * ((note_s - local) / 0.03).clamp(0.0, 1.0);
        let mut s = 0.0;
        for h in 1..=24 {
            let fh = h as f64 * f0;
            if fh > 9000.0 {
                break;
            }
            // formants near 700 Hz and 1.2 kHz over a 1/h roll-off
            let formant =
                1.0 + 2.0 * (-((fh - 700.0) / 250.0).powi(2)).exp() + (-((fh - 1200.0) / 300.0).powi(2)).exp();
            s += formant / h as f64 * (h as f64 * phase).sin();
        }
        *o = (s * env) as f32 * amp;
    }
    out
}

fn lead_line() -> Vec<f32> {
    voice(&[220.0, 261.63, 329.63, 293.66], 0.5, 0.12, 5.5, 0.0)
}

/// A harmony a third to a fifth below the lead, 6 dB down, with its own vibrato.
fn backing_line() -> Vec<f32> {
    voice(&[174.61, 220.0, 261.63, 246.94], 0.5, 0.06, 4.7, 1.3)
}

fn si_sdr(est: &[f32], reference: &[f32]) -> f64 {
    let rr: f64 = reference.iter().map(|r| (*r as f64).powi(2)).sum();
    let er: f64 = est.iter().zip(reference).map(|(e, r)| *e as f64 * *r as f64).sum();
    let a = er / rr.max(1e-20);
    let (mut t, mut d) = (0.0, 0.0);
    for (e, r) in est.iter().zip(reference) {
        let s = a * *r as f64;
        t += s * s;
        d += (*e as f64 - s).powi(2);
    }
    10.0 * (t.max(1e-20) / d.max(1e-20)).log10()
}

fn flat(chans: &[Vec<f32>]) -> Vec<f32> {
    chans.concat()
}

/// Run both passes on planar channels; returns (lead, backing, stats).
fn split_planar(chans: Vec<Vec<f32>>, block: usize) -> (Vec<Vec<f32>>, Vec<Vec<f32>>, Stats) {
    let src = Track::from_buffer(ferraus_core::AudioBuffer::from_planar(SR, chans), None).unwrap();
    let opts = Options::default();
    let f0 = track_f0(&LeadPyin, &src, SR, &opts, &mut |_| Ok(())).unwrap();
    let mut lead = Track::zeros(src.frames, src.channels, None).unwrap();
    let mut back = Track::zeros(src.frames, src.channels, None).unwrap();
    let st = split(&src, SR, &f0, &opts, block, &mut lead, &mut back, &mut |_| Ok(())).unwrap();
    (lead.read(0, src.frames).unwrap(), back.read(0, src.frames).unwrap(), st)
}

fn assert_sums(lead: &[Vec<f32>], back: &[Vec<f32>], vocals: &[Vec<f32>]) {
    for ((l, b), v) in lead.iter().zip(back).zip(vocals) {
        for ((l, b), v) in l.iter().zip(b).zip(v) {
            assert!((l + b - v).abs() < 1e-6, "lead + backing != vocals: {l} + {b} vs {v}");
        }
    }
}

#[test]
fn mono_two_voices_lead_recovered() {
    let (l, b) = (lead_line(), backing_line());
    let v: Vec<f32> = l.iter().zip(&b).map(|(x, y)| x + y).collect();
    let (lead, back, st) = split_planar(vec![v.clone()], 1 << 20);
    assert_sums(&lead, &back, std::slice::from_ref(&v));
    let (base, got) = (si_sdr(&v, &l), si_sdr(&lead[0], &l));
    let (bbase, bgot) = (si_sdr(&v, &b), si_sdr(&back[0], &b));
    eprintln!("mono: lead {got:.2} dB (stem {base:.2}), backing {bgot:.2} dB (stem {bbase:.2})");
    assert!(got > base + 6.0, "lead SI-SDR {got:.2} dB vs {base:.2} dB for the whole stem");
    assert!(bgot > bbase + 10.0, "backing SI-SDR {bgot:.2} dB vs {bbase:.2} dB for the whole stem");
    assert!(st.voiced_fraction() > 0.8, "{st:?}");
    assert!(st.backing_ratio_db() > -14.0 && st.backing_ratio_db() < 0.0, "{st:?}");
    assert!(!st.stereo);
}

#[test]
fn stereo_lead_centred_backing_panned() {
    let (l, b) = (lead_line(), backing_line());
    let (gl, gr) = (0.95f32, 0.3f32);
    let chans: Vec<Vec<f32>> =
        vec![l.iter().zip(&b).map(|(x, y)| x + gl * y).collect(), l.iter().zip(&b).map(|(x, y)| x + gr * y).collect()];
    let lead_ref = vec![l.clone(), l.clone()];
    let back_ref: Vec<Vec<f32>> = vec![b.iter().map(|y| gl * y).collect(), b.iter().map(|y| gr * y).collect()];
    let (lead, back, st) = split_planar(chans.clone(), 1 << 20);
    assert!(st.stereo);
    assert_sums(&lead, &back, &chans);
    let (base, got) = (si_sdr(&flat(&chans), &flat(&lead_ref)), si_sdr(&flat(&lead), &flat(&lead_ref)));
    assert!(got > base + 6.0, "lead SI-SDR {got:.2} dB vs {base:.2} dB");
    let (bbase, bgot) = (si_sdr(&flat(&chans), &flat(&back_ref)), si_sdr(&flat(&back), &flat(&back_ref)));
    eprintln!("stereo: lead {got:.2} dB (stem {base:.2}), backing {bgot:.2} dB (stem {bbase:.2})");
    assert!(bgot > bbase + 10.0, "backing SI-SDR {bgot:.2} dB vs {bbase:.2} dB");
}

#[test]
fn lead_alone_stays_lead() {
    let l = lead_line();
    let (lead, back, st) = split_planar(vec![l.clone()], 1 << 20);
    assert!(si_sdr(&lead[0], &l) > 12.0, "{}", si_sdr(&lead[0], &l));
    assert!(st.backing_ratio_db() < -12.0, "{st:?}");
    assert_sums(&lead, &back, std::slice::from_ref(&l));
}

#[test]
fn blocks_match_a_single_pass() {
    let (l, b) = (lead_line(), backing_line());
    let chans = vec![l.iter().zip(&b).map(|(x, y)| x + y).collect::<Vec<f32>>(), l.clone()];
    let (lead_a, _, _) = split_planar(chans.clone(), 1 << 20);
    let (lead_b, back_b, _) = split_planar(chans.clone(), 3000);
    for (x, y) in flat(&lead_a).iter().zip(&flat(&lead_b)) {
        assert!((x - y).abs() < 1e-5, "{x} vs {y}");
    }
    assert_sums(&lead_b, &back_b, &chans);
}

#[test]
fn silent_and_empty_input() {
    let (lead, back, st) = split_planar(vec![vec![0.0; SR as usize]; 2], 1 << 20);
    assert!(lead.iter().chain(&back).flatten().all(|v| *v == 0.0));
    assert_eq!((st.vocals_energy, st.voiced_frames), (0.0, 0));
    assert!(st.voiced_fraction() == 0.0 && st.backing_ratio_db().is_finite());
    let (lead, back, _) = split_planar(vec![Vec::new()], 1 << 20);
    assert!(lead[0].is_empty() && back[0].is_empty());
}

#[test]
fn f0_track_interpolates_and_stitches_chunks() {
    let t = F0Track { hop_s: 0.01, f0: vec![0.0, 200.0, 400.0, 0.0], conf: vec![0.0; 4], rms: vec![0.0; 4] };
    assert!((t.f0_at(0.01) - 200.0).abs() < 1e-6);
    assert!((t.f0_at(0.015) - 200.0 * 2f64.sqrt()).abs() < 1e-6, "geometric between voiced points");
    assert_eq!((t.f0_at(0.024), t.f0_at(0.027)), (400.0, 0.0), "nearest point next to an unvoiced one");
    assert_eq!(t.f0_at(-1.0), 0.0);
    // 70 s of a steady tone crosses two chunk boundaries: one grid point per 10 ms, all voiced
    let n = 70 * 8000;
    let x: Vec<f32> = (0..n).map(|i| (2.0 * std::f32::consts::PI * 196.0 * i as f32 / 8000.0).sin() * 0.3).collect();
    let src = Track::from_buffer(ferraus_core::AudioBuffer::from_mono(8000, x), None).unwrap();
    let f0 = track_f0(&LeadPyin, &src, 8000, &Options::default(), &mut |_| Ok(())).unwrap();
    assert_eq!(f0.f0.len(), 7001);
    let voiced = f0.f0[10..6990].iter().filter(|f| (**f - 196.0).abs() < 2.0).count();
    assert_eq!(voiced, 6980, "every frame away from the ends tracks 196 Hz");
}
