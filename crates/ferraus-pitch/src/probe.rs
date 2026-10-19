use crate::fixtures::*;
use crate::pyin::{track, PyinOptions};
#[test]
fn probe() {
    let sr = 16000u32;
    for &f0 in &[100.0, 110.0, 115.0, 120.0, 125.0] {
        let x = vocal(sr, &[(f0, 1.0)], Vowel::A, 5.5, 50.0);
        let fr = track(&x, sr, &PyinOptions::default());
        let r = 2f64.powf(4.0 / 12.0);
        let y = crate::psola::shift_with_track(&[x.clone()], &x, sr, &fr, &vec![r; fr.len()]);
        let mut pp = crate::vocoder::PvParams::high(sr);
        pp.lifter = (0.6 * sr as f64 / f0) as usize;
        let z = crate::vocoder::pitch_shift(&x, r, &pp);
        for p in [12usize, 14, 16] {
            let a = lpc_formants(&x[2000..x.len() - 2000], sr, p);
            let b = lpc_formants(&y[0][2000..x.len() - 2000], sr, p);
            let c = lpc_formants(&z[2000..x.len() - 2000], sr, p);
            let dev = |v: &[f64]| -> Vec<String> {
                (0..3).map(|i| format!("{:+.1}%", 100.0 * (v.get(i).copied().unwrap_or(0.0) / a[i] - 1.0))).collect()
            };
            eprintln!("f0 {f0} p {p}: psola {:?} pv {:?}", dev(&b), dev(&c));
        }
    }
}
