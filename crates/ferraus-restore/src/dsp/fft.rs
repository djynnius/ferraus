//! A small radix-2 complex FFT, an STFT with exact overlap-add reconstruction, and
//! FFT convolution. Implemented here so the crate has no FFT dependency.

use std::f64::consts::PI;

/// A complex number in f64.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct C {
    pub re: f64,
    pub im: f64,
}

impl C {
    pub const ZERO: C = C { re: 0.0, im: 0.0 };
    #[inline]
    pub fn new(re: f64, im: f64) -> C {
        C { re, im }
    }
    #[inline]
    pub fn polar(mag: f64, phase: f64) -> C {
        C::new(mag * phase.cos(), mag * phase.sin())
    }
    #[inline]
    pub fn norm_sqr(self) -> f64 {
        self.re * self.re + self.im * self.im
    }
    #[inline]
    pub fn abs(self) -> f64 {
        self.norm_sqr().sqrt()
    }
    #[inline]
    pub fn arg(self) -> f64 {
        self.im.atan2(self.re)
    }
    #[inline]
    pub fn conj(self) -> C {
        C::new(self.re, -self.im)
    }
    #[inline]
    pub fn scale(self, s: f64) -> C {
        C::new(self.re * s, self.im * s)
    }
    #[inline]
    pub fn inv(self) -> C {
        let d = self.norm_sqr();
        C::new(self.re / d, -self.im / d)
    }
    pub fn sqrt(self) -> C {
        let r = self.abs();
        let re = ((r + self.re) * 0.5).max(0.0).sqrt();
        let im = ((r - self.re) * 0.5).max(0.0).sqrt();
        C::new(re, if self.im < 0.0 { -im } else { im })
    }
    pub fn exp(self) -> C {
        C::polar(self.re.exp(), self.im)
    }
}

impl std::ops::Add for C {
    type Output = C;
    #[inline]
    fn add(self, o: C) -> C {
        C::new(self.re + o.re, self.im + o.im)
    }
}
impl std::ops::Sub for C {
    type Output = C;
    #[inline]
    fn sub(self, o: C) -> C {
        C::new(self.re - o.re, self.im - o.im)
    }
}
impl std::ops::Mul for C {
    type Output = C;
    #[inline]
    fn mul(self, o: C) -> C {
        C::new(self.re * o.re - self.im * o.im, self.re * o.im + self.im * o.re)
    }
}
#[allow(clippy::suspicious_arithmetic_impl)]
impl std::ops::Div for C {
    type Output = C;
    #[inline]
    fn div(self, o: C) -> C {
        self * o.inv()
    }
}
impl std::ops::Neg for C {
    type Output = C;
    #[inline]
    fn neg(self) -> C {
        C::new(-self.re, -self.im)
    }
}
impl std::ops::AddAssign for C {
    #[inline]
    fn add_assign(&mut self, o: C) {
        self.re += o.re;
        self.im += o.im;
    }
}
impl std::ops::SubAssign for C {
    #[inline]
    fn sub_assign(&mut self, o: C) {
        self.re -= o.re;
        self.im -= o.im;
    }
}
impl std::ops::MulAssign for C {
    #[inline]
    fn mul_assign(&mut self, o: C) {
        *self = *self * o;
    }
}

/// Smallest power of two `>= n` (and at least 2).
pub fn next_pow2(n: usize) -> usize {
    n.max(2).next_power_of_two()
}

/// An in-place radix-2 FFT plan for one power-of-two size.
#[derive(Clone, Debug)]
pub struct Fft {
    n: usize,
    twiddles: Vec<C>,
    rev: Vec<u32>,
}

impl Fft {
    pub fn new(n: usize) -> Fft {
        assert!(n.is_power_of_two() && n >= 2, "FFT size must be a power of two");
        let bits = n.trailing_zeros();
        let rev = (0..n as u32).map(|i| i.reverse_bits() >> (32 - bits)).collect();
        let twiddles = (0..n / 2).map(|k| C::polar(1.0, -2.0 * PI * k as f64 / n as f64)).collect();
        Fft { n, twiddles, rev }
    }

    pub fn len(&self) -> usize {
        self.n
    }

    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    fn run(&self, x: &mut [C], inverse: bool) {
        let n = self.n;
        assert_eq!(x.len(), n);
        for i in 0..n {
            let j = self.rev[i] as usize;
            if j > i {
                x.swap(i, j);
            }
        }
        let mut len = 2;
        while len <= n {
            let half = len / 2;
            let step = n / len;
            for start in (0..n).step_by(len) {
                for k in 0..half {
                    let mut w = self.twiddles[k * step];
                    if inverse {
                        w = w.conj();
                    }
                    let a = x[start + k];
                    let b = x[start + k + half] * w;
                    x[start + k] = a + b;
                    x[start + k + half] = a - b;
                }
            }
            len <<= 1;
        }
    }

    /// Forward transform (no scaling).
    pub fn forward(&self, x: &mut [C]) {
        self.run(x, false);
    }

    /// Inverse transform, scaled by 1/n so `inverse(forward(x)) == x`.
    pub fn inverse(&self, x: &mut [C]) {
        self.run(x, true);
        let s = 1.0 / self.n as f64;
        for v in x.iter_mut() {
            *v = v.scale(s);
        }
    }

    /// Forward transform of a real signal (zero-padded to n); returns bins `0..=n/2`.
    pub fn forward_real(&self, x: &[f64]) -> Vec<C> {
        let mut buf = vec![C::ZERO; self.n];
        for (b, v) in buf.iter_mut().zip(x) {
            b.re = *v;
        }
        self.forward(&mut buf);
        buf.truncate(self.n / 2 + 1);
        buf
    }

    /// Inverse of `forward_real`: takes bins `0..=n/2` (Hermitian symmetry assumed).
    pub fn inverse_real(&self, half: &[C]) -> Vec<f64> {
        let n = self.n;
        let mut buf = vec![C::ZERO; n];
        for k in 0..=n / 2 {
            if k < half.len() {
                buf[k] = half[k];
            }
        }
        buf[0].im = 0.0;
        buf[n / 2].im = 0.0;
        for k in 1..n / 2 {
            buf[n - k] = buf[k].conj();
        }
        self.inverse(&mut buf);
        buf.into_iter().map(|c| c.re).collect()
    }
}

/// Periodic Hann window.
pub fn hann(n: usize) -> Vec<f64> {
    (0..n).map(|i| 0.5 - 0.5 * (2.0 * PI * i as f64 / n as f64).cos()).collect()
}

/// Short-time Fourier transform with a sqrt-Hann analysis/synthesis window pair and
/// exact reconstruction (the overlap-added squared window is divided out per sample).
#[derive(Clone, Debug)]
pub struct Stft {
    pub n: usize,
    pub hop: usize,
    fft: Fft,
    window: Vec<f64>,
}

impl Stft {
    pub fn new(n: usize, hop: usize) -> Stft {
        let n = next_pow2(n);
        let hop = hop.clamp(1, n);
        let window = hann(n).into_iter().map(|w| w.sqrt()).collect();
        Stft { n, hop, fft: Fft::new(n), window }
    }

    /// STFT sized for a sample rate: `ms` window length rounded to a power of two, `overlap`
    /// frames per window (4 means 75% overlap).
    pub fn for_rate(sample_rate: u32, ms: f64, overlap: usize) -> Stft {
        let n = next_pow2((sample_rate as f64 * ms / 1000.0).round() as usize).max(64);
        Stft::new(n, n / overlap.max(1))
    }

    pub fn bins(&self) -> usize {
        self.n / 2 + 1
    }

    /// Centre frequency of bin `k`.
    pub fn bin_hz(&self, k: usize, sample_rate: u32) -> f64 {
        k as f64 * sample_rate as f64 / self.n as f64
    }

    /// Number of frames covering `len` samples (with the padding used by `process`).
    pub fn frame_count(&self, len: usize) -> usize {
        (len + self.n).div_ceil(self.hop) + 1
    }

    /// Spectrum of the frame `index` of `x` (same framing as `process`).
    pub fn frame(&self, x: &[f32], index: usize) -> Vec<C> {
        let start = index as isize * self.hop as isize - self.n as isize;
        let mut buf = vec![C::ZERO; self.n];
        for (i, b) in buf.iter_mut().enumerate() {
            let p = start + i as isize;
            if p >= 0 && (p as usize) < x.len() {
                b.re = x[p as usize] as f64 * self.window[i];
            }
        }
        self.fft.forward(&mut buf);
        buf.truncate(self.bins());
        buf
    }

    /// Analyse several equally long channels frame by frame, let `f` modify the spectra
    /// (`f(frame_index, &mut spectra_per_channel)`), and resynthesise. With an identity
    /// `f` the output equals the input to rounding error.
    pub fn process<F>(&self, chans: &[Vec<f32>], mut f: F) -> Vec<Vec<f32>>
    where
        F: FnMut(usize, &mut [Vec<C>]),
    {
        let nch = chans.len();
        let len = chans.first().map(|c| c.len()).unwrap_or(0);
        if len == 0 {
            return chans.to_vec();
        }
        let n = self.n;
        let total = len + 2 * n;
        let mut out = vec![vec![0.0f64; total]; nch];
        let mut norm = vec![0.0f64; total];
        let frames = self.frame_count(len);
        let mut spectra: Vec<Vec<C>> = vec![Vec::new(); nch];
        let mut buf = vec![C::ZERO; n];
        for fi in 0..frames {
            // frame fi starts at sample fi*hop - n of the input (position fi*hop in `out`)
            let start = fi * self.hop;
            if start >= total {
                break;
            }
            for (c, ch) in chans.iter().enumerate() {
                for (i, b) in buf.iter_mut().enumerate() {
                    let p = start as isize + i as isize - n as isize;
                    *b = if p >= 0 && (p as usize) < len {
                        C::new(ch[p as usize] as f64 * self.window[i], 0.0)
                    } else {
                        C::ZERO
                    };
                }
                self.fft.forward(&mut buf);
                spectra[c].clear();
                spectra[c].extend_from_slice(&buf[..n / 2 + 1]);
            }
            f(fi, &mut spectra);
            for (c, spec) in spectra.iter().enumerate() {
                let time = self.fft.inverse_real(spec);
                for i in 0..n {
                    let p = start + i;
                    if p < total {
                        out[c][p] += time[i] * self.window[i];
                    }
                }
            }
            for i in 0..n {
                let p = start + i;
                if p < total {
                    norm[p] += self.window[i] * self.window[i];
                }
            }
        }
        out.into_iter()
            .map(|o| {
                (0..len)
                    .map(|i| {
                        let p = i + n;
                        let w = norm[p];
                        if w > 1e-9 {
                            (o[p] / w) as f32
                        } else {
                            0.0
                        }
                    })
                    .collect()
            })
            .collect()
    }
}

/// Linear convolution of `x` with FIR `h` by FFT overlap-add. Output length is
/// `x.len() + h.len() - 1`.
pub fn convolve(x: &[f32], h: &[f64]) -> Vec<f64> {
    if x.is_empty() || h.is_empty() {
        return Vec::new();
    }
    let out_len = x.len() + h.len() - 1;
    if h.len() <= 64 || x.len() <= 64 {
        let mut y = vec![0.0f64; out_len];
        for (i, xv) in x.iter().enumerate() {
            let xv = *xv as f64;
            if xv == 0.0 {
                continue;
            }
            for (j, hv) in h.iter().enumerate() {
                y[i + j] += xv * hv;
            }
        }
        return y;
    }
    let nfft = next_pow2(2 * h.len()).max(1024);
    let block = nfft - h.len() + 1;
    let fft = Fft::new(nfft);
    let mut hf = vec![C::ZERO; nfft];
    for (b, v) in hf.iter_mut().zip(h) {
        b.re = *v;
    }
    fft.forward(&mut hf);
    let mut y = vec![0.0f64; out_len];
    let mut buf = vec![C::ZERO; nfft];
    let mut start = 0;
    while start < x.len() {
        let end = (start + block).min(x.len());
        for b in buf.iter_mut() {
            *b = C::ZERO;
        }
        for (i, v) in x[start..end].iter().enumerate() {
            buf[i].re = *v as f64;
        }
        fft.forward(&mut buf);
        for (b, hv) in buf.iter_mut().zip(&hf) {
            *b *= *hv;
        }
        fft.inverse(&mut buf);
        for (i, b) in buf.iter().enumerate() {
            let p = start + i;
            if p < out_len {
                y[p] += b.re;
            }
        }
        start = end;
    }
    y
}

/// Apply a linear-phase FIR of odd length (delay `(len-1)/2`) without shifting the signal:
/// the output is aligned with and as long as the input.
pub fn fir_zero_delay(x: &[f32], h: &[f64]) -> Vec<f32> {
    let d = (h.len().saturating_sub(1)) / 2;
    let y = convolve(x, h);
    (0..x.len()).map(|i| y.get(i + d).copied().unwrap_or(0.0) as f32).collect()
}

/// Design a linear-phase FIR of odd length `taps` whose magnitude follows
/// `mag(freq_hz)` (linear gain), by frequency sampling and a Blackman window.
pub fn design_fir(taps: usize, sample_rate: u32, mag: impl Fn(f64) -> f64) -> Vec<f64> {
    let taps = if taps & 1 == 0 { taps + 1 } else { taps }.max(3);
    let m = next_pow2(taps * 4).max(1024);
    let fft = Fft::new(m);
    let half: Vec<C> = (0..=m / 2)
        .map(|k| {
            let f = k as f64 * sample_rate as f64 / m as f64;
            C::new(mag(f), 0.0)
        })
        .collect();
    let imp = fft.inverse_real(&half); // zero-phase, centred at 0 (circular)
    let mid = (taps - 1) / 2;
    (0..taps)
        .map(|i| {
            let k = i as isize - mid as isize;
            let v = imp[k.rem_euclid(m as isize) as usize];
            let w = 0.42 - 0.5 * (2.0 * PI * i as f64 / (taps - 1) as f64).cos()
                + 0.08 * (4.0 * PI * i as f64 / (taps - 1) as f64).cos();
            v * w
        })
        .collect()
}

/// Magnitude response of an FIR at `freq`.
pub fn fir_response(h: &[f64], freq: f64, sample_rate: u32) -> f64 {
    let w = 2.0 * PI * freq / sample_rate as f64;
    let mut acc = C::ZERO;
    for (i, v) in h.iter().enumerate() {
        acc += C::polar(*v, -w * i as f64);
    }
    acc.abs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fft_roundtrip_and_tone() {
        let n = 256;
        let f = Fft::new(n);
        let x: Vec<f64> = (0..n).map(|i| (2.0 * PI * 8.0 * i as f64 / n as f64).cos()).collect();
        let spec = f.forward_real(&x);
        assert!((spec[8].abs() - n as f64 / 2.0).abs() < 1e-9);
        assert!(spec[7].abs() < 1e-9);
        let back = f.inverse_real(&spec);
        for (a, b) in x.iter().zip(&back) {
            assert!((a - b).abs() < 1e-12);
        }
    }

    #[test]
    fn stft_identity() {
        let x: Vec<f32> = (0..5000).map(|i| ((i * 7919) % 101) as f32 / 50.0 - 1.0).collect();
        let st = Stft::new(512, 128);
        let y = st.process(std::slice::from_ref(&x), |_, _| {});
        for (a, b) in x.iter().zip(&y[0]) {
            assert!((a - b).abs() < 1e-5);
        }
    }

    #[test]
    fn convolution_matches_direct() {
        let x: Vec<f32> = (0..3000).map(|i| ((i * 31) % 17) as f32 - 8.0).collect();
        let h: Vec<f64> = (0..300).map(|i| ((i * 13) % 7) as f64 - 3.0).collect();
        let y = convolve(&x, &h);
        for i in [0usize, 10, 299, 1500, 3298] {
            let mut d = 0.0;
            for (j, hv) in h.iter().enumerate() {
                if i >= j && i - j < x.len() {
                    d += x[i - j] as f64 * hv;
                }
            }
            assert!((y[i] - d).abs() < 1e-6 * d.abs().max(1.0), "{i}");
        }
    }

    #[test]
    fn fir_design_follows_curve() {
        let h = design_fir(1023, 48000, |f| if f < 4000.0 { 1.0 } else { 0.5 });
        assert!((fir_response(&h, 1000.0, 48000) - 1.0).abs() < 0.01);
        assert!((fir_response(&h, 10000.0, 48000) - 0.5).abs() < 0.01);
    }
}
