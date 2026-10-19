//! Reverbs: a Dattorro plate, an 8-line feedback delay network with Hadamard mixing (room,
//! chamber, hall), and partitioned FFT convolution with an impulse response.
//!
//! Decay times are exact at low frequencies: loop gains are derived from the requested
//! T60 and the loop lengths, and damping filters have unity DC gain (Jot's absorption
//! filters in the FDN), so `--decay` means "time to fall 60 dB".
//!
//! The algorithmic reverbs return a wet signal normalised to unit impulse-response energy,
//! so steady material comes back at roughly the input level whatever the decay or size.

use crate::biquad::Cascade;
use crate::delayline::{Allpass, DelayLine};
use crate::fir::PartitionedConvolver;
use crate::modulation::Lfo;
use crate::util::{flush, log_lerp};

/// Algorithm choice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReverbType {
    Plate,
    Room,
    Hall,
    Chamber,
}

impl ReverbType {
    pub fn parse(s: &str) -> ReverbType {
        match s.to_ascii_lowercase().as_str() {
            "room" => ReverbType::Room,
            "hall" => ReverbType::Hall,
            "chamber" => ReverbType::Chamber,
            _ => ReverbType::Plate,
        }
    }
}

/// Algorithmic reverb settings.
#[derive(Clone, Debug)]
pub struct ReverbParams {
    pub kind: ReverbType,
    /// 0..1 (0.5 = nominal room dimensions).
    pub size: f64,
    /// Time to decay by 60 dB, seconds.
    pub decay_s: f64,
    /// 0..1: how much faster high frequencies die away.
    pub damping: f64,
    pub predelay_s: f64,
    /// Wet high-pass corner (Hz, 0 = off).
    pub lowcut: f64,
    /// Wet low-pass corner (Hz, 0 = off).
    pub highcut: f64,
    /// Stereo width of the wet signal, 0..2 (1 = natural).
    pub width: f64,
}

impl Default for ReverbParams {
    fn default() -> Self {
        ReverbParams {
            kind: ReverbType::Plate,
            size: 0.5,
            decay_s: 1.8,
            damping: 0.5,
            predelay_s: 0.020,
            lowcut: 120.0,
            highcut: 8000.0,
            width: 1.0,
        }
    }
}

/// A stereo-output reverb tank fed with a mono input.
pub trait Tank {
    fn process(&mut self, x: f32) -> (f32, f32);
}

/// Dattorro's plate (J. Dattorro, "Effect Design Part 1", JAES 1997), scaled to the sample
/// rate and `size`, with the decay coefficient derived from the requested T60.
pub struct Plate {
    diffusers: [Allpass; 4],
    ap_l1: Allpass,
    d_l1: DelayLine,
    ap_l2: Allpass,
    d_l2: DelayLine,
    ap_r1: Allpass,
    d_r1: DelayLine,
    ap_r2: Allpass,
    d_r2: DelayLine,
    len: [usize; 4], // d_l1, d_l2, d_r1, d_r2
    damp_l: f32,
    damp_r: f32,
    damp: f32,
    decay: f32,
    lfo_l: Lfo,
    lfo_r: Lfo,
    excursion: f32,
    fb_l: f32,
    fb_r: f32,
    taps: [usize; 14],
}

impl Plate {
    pub fn new(sample_rate: f64, p: &ReverbParams) -> Plate {
        let scale = sample_rate / 29761.0;
        let size = 0.4 + 1.2 * p.size.clamp(0.0, 1.0);
        let s = |n: f64| ((n * scale * size).round() as usize).max(2);
        let si = |n: f64| ((n * scale).round() as usize).max(2);
        let len = [s(4453.0), s(3720.0), s(4217.0), s(3163.0)];
        let ap = [s(672.0), s(1800.0), s(908.0), s(2656.0)];
        let loop_samples: usize = len.iter().sum::<usize>() + ap.iter().sum::<usize>();
        let loop_s = loop_samples as f64 / sample_rate;
        let decay = 10f64.powf(-3.0 * loop_s / (4.0 * p.decay_s.max(0.05))).min(0.9999) as f32;
        // damping: one-pole low-pass in the loop, cutoff 16 kHz (0%) .. 1.2 kHz (100%)
        let fc = log_lerp(16000.0, 1200.0, p.damping);
        let damp = (-2.0 * std::f64::consts::PI * fc / sample_rate).exp() as f32;
        let t = |n: f64| ((n * scale * size).round() as usize).max(1);
        Plate {
            diffusers: [
                Allpass::new(si(142.0), 0.75),
                Allpass::new(si(107.0), 0.75),
                Allpass::new(si(379.0), 0.625),
                Allpass::new(si(277.0), 0.625),
            ],
            ap_l1: Allpass::new(ap[0] + 40, -0.7),
            d_l1: DelayLine::new(len[0]),
            ap_l2: Allpass::new(ap[1], 0.5),
            d_l2: DelayLine::new(len[1]),
            ap_r1: Allpass::new(ap[2] + 40, -0.7),
            d_r1: DelayLine::new(len[2]),
            ap_r2: Allpass::new(ap[3], 0.5),
            d_r2: DelayLine::new(len[3]),
            len,
            damp_l: 0.0,
            damp_r: 0.0,
            damp,
            decay,
            lfo_l: Lfo::new(0.5, sample_rate, crate::modulation::Shape::Sine, 0.0),
            lfo_r: Lfo::new(0.37, sample_rate, crate::modulation::Shape::Sine, 0.25),
            excursion: (8.0 * scale).min(30.0) as f32,
            fb_l: 0.0,
            fb_r: 0.0,
            taps: [
                t(266.0),
                t(2974.0),
                t(1913.0),
                t(1996.0),
                t(1990.0),
                t(187.0),
                t(1066.0),
                t(353.0),
                t(3627.0),
                t(1228.0),
                t(2673.0),
                t(2111.0),
                t(335.0),
                t(121.0),
            ],
        }
    }
}

impl Tank for Plate {
    #[inline]
    fn process(&mut self, x: f32) -> (f32, f32) {
        let mut v = x;
        for d in &mut self.diffusers {
            v = d.process(v);
        }
        // the modulated allpasses were built 40 samples long for headroom; centre on nominal
        let ml = self.excursion * self.lfo_l.next() as f32 - 40.0;
        let mr = self.excursion * self.lfo_r.next() as f32 - 40.0;
        // left half
        let a = self.ap_l1.process_mod(v + self.fb_r, ml);
        self.d_l1.push(a);
        let b = self.d_l1.read(self.len[0]);
        self.damp_l = flush(b + self.damp * (self.damp_l - b));
        let c = self.ap_l2.process(self.damp_l * self.decay);
        self.d_l2.push(c);
        let left_end = self.d_l2.read(self.len[1]) * self.decay;
        // right half
        let a = self.ap_r1.process_mod(v + self.fb_l, mr);
        self.d_r1.push(a);
        let b = self.d_r1.read(self.len[2]);
        self.damp_r = flush(b + self.damp * (self.damp_r - b));
        let c = self.ap_r2.process(self.damp_r * self.decay);
        self.d_r2.push(c);
        let right_end = self.d_r2.read(self.len[3]) * self.decay;
        self.fb_l = flush(left_end);
        self.fb_r = flush(right_end);
        let t = &self.taps;
        let l = self.d_r1.read(t[0]) + self.d_r1.read(t[1]) - self.ap_r2.tap(t[2])
            + self.d_r2.read(t[3])
            - self.d_l1.read(t[4])
            - self.ap_l2.tap(t[5])
            - self.d_l2.read(t[6]);
        let r = self.d_l1.read(t[7]) + self.d_l1.read(t[8]) - self.ap_l2.tap(t[9])
            + self.d_l2.read(t[10])
            - self.d_r1.read(t[11])
            - self.ap_r2.tap(t[12])
            - self.d_r2.read(t[13]);
        (l * 0.6, r * 0.6)
    }
}

/// 8-line feedback delay network with a Hadamard feedback matrix, Jot absorption filters,
/// input diffusion, and an early-reflection pattern.
pub struct Fdn {
    lines: Vec<DelayLine>,
    lens: [usize; 8],
    gains: [f32; 8],
    poles: [f32; 8],
    lp: [f32; 8],
    diffusers: Vec<Allpass>,
    er: DelayLine,
    er_l: Vec<(usize, f32)>,
    er_r: Vec<(usize, f32)>,
    out: [f32; 8],
}

fn is_prime(n: usize) -> bool {
    if n < 2 {
        return false;
    }
    let mut i = 2;
    while i * i <= n {
        if n % i == 0 {
            return false;
        }
        i += 1;
    }
    true
}

fn next_prime(mut n: usize) -> usize {
    while !is_prime(n) {
        n += 1;
    }
    n
}

/// In-place fast Walsh–Hadamard transform of 8 values, orthonormal (scaled by 1/√8).
#[inline(always)]
fn hadamard8(v: &mut [f32; 8]) {
    let mut h = 1;
    while h < 8 {
        let mut i = 0;
        while i < 8 {
            for j in i..i + h {
                let (a, b) = (v[j], v[j + h]);
                v[j] = a + b;
                v[j + h] = a - b;
            }
            i += 2 * h;
        }
        h *= 2;
    }
    let s = 1.0 / 8f32.sqrt();
    for x in v.iter_mut() {
        *x *= s;
    }
}

impl Fdn {
    pub fn new(sample_rate: f64, p: &ReverbParams) -> Fdn {
        let kind_scale = match p.kind {
            ReverbType::Room => 0.55,
            ReverbType::Chamber => 0.85,
            ReverbType::Hall => 1.6,
            ReverbType::Plate => 1.0,
        };
        let size = 0.5 + p.size.clamp(0.0, 1.0);
        let base_ms = [29.7, 37.1, 41.1, 43.7, 53.3, 59.9, 67.7, 73.1];
        let mut lens = [0usize; 8];
        let mut used = Vec::new();
        for (i, ms) in base_ms.iter().enumerate() {
            let mut n = next_prime((ms * 1e-3 * sample_rate * kind_scale * size).round().max(17.0) as usize);
            while used.contains(&n) {
                n = next_prime(n + 1);
            }
            used.push(n);
            lens[i] = n;
        }
        let t60 = p.decay_s.max(0.05);
        // Jot: T60 at Nyquist = alpha * T60 at DC
        let alpha = (1.0 - 0.85 * p.damping.clamp(0.0, 1.0)).max(0.05);
        let mut gains = [0f32; 8];
        let mut poles = [0f32; 8];
        for i in 0..8 {
            let g = 10f64.powf(-3.0 * lens[i] as f64 / (t60 * sample_rate));
            gains[i] = g as f32;
            let b = (10f64.ln() / 4.0) * g.log10() * (1.0 - 1.0 / (alpha * alpha));
            poles[i] = b.clamp(0.0, 0.99) as f32;
        }
        let dscale = kind_scale * size * 0.5;
        let diffusers = [4.7, 3.6, 12.7, 9.3]
            .iter()
            .map(|ms| Allpass::new(((ms * 1e-3 * sample_rate * dscale) as usize).max(3), 0.65))
            .collect();
        let er_ms_l = [7.0, 11.0, 17.0, 23.0, 31.0, 41.0];
        let er_ms_r = [9.0, 13.0, 19.0, 27.0, 34.0, 43.0];
        let mk = |ms: &[f64; 6]| -> Vec<(usize, f32)> {
            ms.iter()
                .enumerate()
                .map(|(k, m)| {
                    (
                        ((m * 1e-3 * sample_rate * kind_scale * size) as usize).max(1),
                        0.8f32.powi(k as i32) * if k % 2 == 0 { 1.0 } else { -1.0 },
                    )
                })
                .collect()
        };
        let er_l = mk(&er_ms_l);
        let er_r = mk(&er_ms_r);
        let er_max = er_l.iter().chain(&er_r).map(|t| t.0).max().unwrap_or(1);
        Fdn {
            lines: lens.iter().map(|n| DelayLine::new(*n)).collect(),
            lens,
            gains,
            poles,
            lp: [0.0; 8],
            diffusers,
            er: DelayLine::new(er_max + 1),
            er_l,
            er_r,
            out: [0.0; 8],
        }
    }
}

const SIGN_L: [f32; 8] = [1.0, -1.0, 1.0, -1.0, 1.0, -1.0, 1.0, -1.0];
const SIGN_R: [f32; 8] = [1.0, 1.0, -1.0, -1.0, 1.0, 1.0, -1.0, -1.0];
const SIGN_IN: [f32; 8] = [1.0, -1.0, -1.0, 1.0, 1.0, 1.0, -1.0, -1.0];

impl Tank for Fdn {
    #[inline]
    fn process(&mut self, x: f32) -> (f32, f32) {
        self.er.push(x);
        let mut erl = 0.0;
        let mut err = 0.0;
        for (d, g) in &self.er_l {
            erl += self.er.read(*d) * g;
        }
        for (d, g) in &self.er_r {
            err += self.er.read(*d) * g;
        }
        let mut v = x + 0.3 * (erl + err);
        for d in &mut self.diffusers {
            v = d.process(v);
        }
        // read line outputs through absorption filters
        for i in 0..8 {
            let y = self.lines[i].read(self.lens[i]);
            let g = self.gains[i];
            let b = self.poles[i];
            self.lp[i] = flush(g * (1.0 - b) * y + b * self.lp[i]);
            self.out[i] = self.lp[i];
        }
        let mut mix = self.out;
        hadamard8(&mut mix);
        let inj = v * 0.5;
        for i in 0..8 {
            self.lines[i].push(mix[i] + inj * SIGN_IN[i]);
        }
        let mut l = 0.0;
        let mut r = 0.0;
        for i in 0..8 {
            l += self.out[i] * SIGN_L[i];
            r += self.out[i] * SIGN_R[i];
        }
        (l * 0.5 + 0.4 * erl, r * 0.5 + 0.4 * err)
    }
}

fn make_tank(sample_rate: f64, p: &ReverbParams) -> Box<dyn Tank> {
    match p.kind {
        ReverbType::Plate => Box::new(Plate::new(sample_rate, p)),
        _ => Box::new(Fdn::new(sample_rate, p)),
    }
}

/// Wet stereo signal of length `out_len` for a mono input, normalised to unit
/// impulse-response energy, with pre-delay, wet filters, and width applied.
pub fn reverb_wet(input: &[f32], out_len: usize, sample_rate: f64, p: &ReverbParams) -> [Vec<f32>; 2] {
    let predelay = (p.predelay_s.max(0.0) * sample_rate).round() as usize;
    // normalisation probe: impulse-response energy of the tank alone
    let probe_len = ((p.decay_s.max(0.05) * 1.5 + 0.1) * sample_rate) as usize;
    let mut probe = make_tank(sample_rate, p);
    let mut energy = 0.0f64;
    for i in 0..probe_len {
        let (l, r) = probe.process(if i == 0 { 1.0 } else { 0.0 });
        energy += 0.5 * ((l as f64).powi(2) + (r as f64).powi(2));
    }
    let norm = if energy > 1e-12 { (1.0 / energy.sqrt()) as f32 } else { 1.0 };

    let mut tank = make_tank(sample_rate, p);
    let mut wl = Vec::with_capacity(out_len);
    let mut wr = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let x = if i >= predelay {
            input.get(i - predelay).copied().unwrap_or(0.0)
        } else {
            0.0
        };
        let (l, r) = tank.process(x);
        wl.push(l * norm);
        wr.push(r * norm);
    }
    post_filter(&mut wl, &mut wr, sample_rate, p.lowcut, p.highcut, p.width);
    [wl, wr]
}

/// Wet high/low cut and mid/side width.
pub fn post_filter(l: &mut [f32], r: &mut [f32], sample_rate: f64, lowcut: f64, highcut: f64, width: f64) {
    for ch in [&mut *l, &mut *r] {
        if lowcut > 0.0 {
            Cascade::butterworth_highpass(2, lowcut, sample_rate).process_block(ch);
        }
        if highcut > 0.0 && highcut < sample_rate * 0.49 {
            Cascade::butterworth_lowpass(2, highcut, sample_rate).process_block(ch);
        }
    }
    if (width - 1.0).abs() > 1e-6 {
        let w = width.max(0.0) as f32;
        for (a, b) in l.iter_mut().zip(r.iter_mut()) {
            let m = 0.5 * (*a + *b);
            let s = 0.5 * (*a - *b) * w;
            *a = m + s;
            *b = m - s;
        }
    }
}

/// Remove the IR's tail after it falls below `threshold_db` relative to its peak, plus any
/// leading silence before it rises above that level. Returns the trimmed channels.
pub fn trim_ir(ir: &[Vec<f32>], threshold_db: f64) -> Vec<Vec<f32>> {
    let peak = ir
        .iter()
        .flat_map(|c| c.iter())
        .fold(0.0f32, |m, v| m.max(v.abs()));
    if peak <= 0.0 {
        return ir.to_vec();
    }
    let thr = peak * 10f32.powf(threshold_db as f32 / 20.0);
    let n = ir.first().map(|c| c.len()).unwrap_or(0);
    let first = (0..n)
        .find(|&i| ir.iter().any(|c| c[i].abs() >= thr))
        .unwrap_or(0);
    let last = (0..n)
        .rev()
        .find(|&i| ir.iter().any(|c| c[i].abs() >= thr))
        .unwrap_or(n.saturating_sub(1));
    ir.iter().map(|c| c[first..=last].to_vec()).collect()
}

/// Convolve `input` channels with IR channels: a mono IR is applied to every channel; a
/// stereo IR maps channel-to-channel (a mono input is fed to both IR channels). The IR is
/// normalised to unit energy per channel. Output has `max(input, ir)` channels.
pub fn convolve_ir(input: &[Vec<f32>], ir: &[Vec<f32>], out_len: usize) -> Vec<Vec<f32>> {
    let out_ch = input.len().max(ir.len());
    (0..out_ch)
        .map(|c| {
            let x = &input[c.min(input.len() - 1)];
            let h = &ir[c.min(ir.len() - 1)];
            let e: f64 = h.iter().map(|v| (*v as f64).powi(2)).sum();
            let g = if e > 1e-20 { (1.0 / e.sqrt()) as f32 } else { 1.0 };
            let hn: Vec<f32> = h.iter().map(|v| v * g).collect();
            PartitionedConvolver::convolve(&hn, x, out_len)
        })
        .collect()
}

/// Time (seconds) for the Schroeder energy decay curve of `ir` to fall by `drop_db`.
pub fn edc_decay_time(ir: &[f32], sample_rate: f64, drop_db: f64) -> Option<f64> {
    let mut edc = vec![0.0f64; ir.len()];
    let mut acc = 0.0f64;
    for i in (0..ir.len()).rev() {
        acc += (ir[i] as f64).powi(2);
        edc[i] = acc;
    }
    let start = ir.iter().position(|v| v.abs() > 0.0)?;
    let e0 = edc[start];
    let thr = e0 * 10f64.powf(-drop_db / 10.0);
    let end = (start..ir.len()).find(|&i| edc[i] <= thr)?;
    Some((end - start) as f64 / sample_rate)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hadamard_is_orthonormal() {
        let mut v = [1.0f32, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        hadamard8(&mut v);
        let e: f32 = v.iter().map(|x| x * x).sum();
        assert!((e - 1.0).abs() < 1e-6);
    }

    #[test]
    fn trims_quiet_tail() {
        let mut ir = vec![0.0f32; 1000];
        ir[10] = 1.0;
        ir[500] = 0.01;
        ir[900] = 1e-5;
        let t = trim_ir(&[ir], -60.0);
        assert_eq!(t[0].len(), 491);
    }
}
