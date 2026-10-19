//! Scientific IIR filter design: analog prototypes (Butterworth, Chebyshev I and II,
//! Bessel, elliptic), frequency transformations, the bilinear transform with
//! prewarping, and pole/zero pairing into second-order sections.
//!
//! The prototypes follow the classical formulas (the same ones SciPy uses). The
//! elliptic design uses Jacobi elliptic functions computed with the arithmetic-geometric
//! mean, the complete elliptic integral via AGM, the degree equation solved with the
//! nome series, and the inverse `sc` function by descending Landen transformations.

use super::biquad::{Coefs, Sos};
use super::fft::C;
use std::f64::consts::PI;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Design {
    Butterworth,
    Chebyshev1,
    Chebyshev2,
    Bessel,
    Elliptic,
}

impl Design {
    pub fn parse(s: &str) -> Option<Design> {
        Some(match s {
            "butterworth" | "butter" => Design::Butterworth,
            "chebyshev1" | "cheby1" => Design::Chebyshev1,
            "chebyshev2" | "cheby2" => Design::Chebyshev2,
            "bessel" => Design::Bessel,
            "elliptic" | "ellip" | "cauer" => Design::Elliptic,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Band {
    Lowpass,
    Highpass,
    Bandpass,
    Bandstop,
}

/// Zeros, poles, gain.
#[derive(Clone, Debug)]
pub struct Zpk {
    pub z: Vec<C>,
    pub p: Vec<C>,
    pub k: f64,
}

fn prod_neg(v: &[C]) -> C {
    v.iter().fold(C::new(1.0, 0.0), |acc, r| acc * (-*r))
}

// ------------------------------------------------------------------------------
// Elliptic integrals and Jacobi functions
// ------------------------------------------------------------------------------

fn agm(mut a: f64, mut b: f64) -> f64 {
    for _ in 0..64 {
        let an = 0.5 * (a + b);
        let bn = (a * b).sqrt();
        a = an;
        b = bn;
        if (a - b).abs() <= 1e-16 * a {
            break;
        }
    }
    a
}

/// Complete elliptic integral of the first kind, parameter `m = k^2`.
pub fn ellipk(m: f64) -> f64 {
    if m >= 1.0 {
        return f64::INFINITY;
    }
    PI / (2.0 * agm(1.0, (1.0 - m).sqrt()))
}

/// `K(1 - m1)`, accurate for small `m1`.
pub fn ellipkm1(m1: f64) -> f64 {
    if m1 <= 0.0 {
        return f64::INFINITY;
    }
    PI / (2.0 * agm(1.0, m1.sqrt()))
}

/// Jacobi elliptic functions `(sn, cn, dn)` of argument `u` and parameter `m`.
pub fn ellipj(u: f64, m: f64) -> (f64, f64, f64) {
    if m < 1e-12 {
        return (u.sin(), u.cos(), 1.0);
    }
    if m > 1.0 - 1e-12 {
        let t = u.tanh();
        let s = 1.0 / u.cosh();
        return (t, s, s);
    }
    let mut a = vec![1.0f64];
    let mut c = vec![m.sqrt()];
    let mut b = (1.0 - m).sqrt();
    let mut i = 0;
    while c[i].abs() > 1e-16 && i < 30 {
        let ai = a[i];
        a.push(0.5 * (ai + b));
        c.push(0.5 * (ai - b));
        b = (ai * b).sqrt();
        i += 1;
    }
    let mut phi = 2f64.powi(i as i32) * a[i] * u;
    for j in (1..=i).rev() {
        phi = 0.5 * (phi + (c[j] * phi.sin() / a[j]).clamp(-1.0, 1.0).asin());
    }
    let sn = phi.sin();
    let cn = phi.cos();
    let dn = (1.0 - m * sn * sn).max(0.0).sqrt();
    (sn, cn, dn)
}

/// Solve the degree equation: the parameter `m` of the elliptic filter of order `n`
/// with discrimination parameter `m1`.
fn ellipdeg(n: usize, m1: f64) -> f64 {
    const MMAX: i32 = 7;
    let k1 = ellipk(m1);
    let k1p = ellipkm1(m1);
    let q1 = (-PI * k1p / k1).exp();
    let q = q1.powf(1.0 / n as f64);
    let num: f64 = (0..=MMAX).map(|m| q.powi(m * (m + 1))).sum();
    let den: f64 = 1.0 + 2.0 * (1..=MMAX + 1).map(|m| q.powi(m * m)).sum::<f64>();
    16.0 * q * (num / den).powi(4)
}

/// Imaginary part of the inverse Jacobi `sn` at a purely imaginary argument `i*w`,
/// i.e. the inverse of `sc` with the complementary parameter (Landen descent).
fn arc_jac_sc1(w: f64, m: f64) -> f64 {
    let complement = |kx: f64| ((1.0 - kx) * (1.0 + kx)).max(0.0).sqrt();
    let k = m.sqrt();
    if k >= 1.0 {
        return w.atan();
    }
    let mut ks = vec![k];
    let mut iter = 0;
    while *ks.last().unwrap() != 0.0 && iter < 20 {
        let kk = *ks.last().unwrap();
        let kp = complement(kk);
        ks.push((1.0 - kp) / (1.0 + kp));
        iter += 1;
    }
    let kcap: f64 = ks[1..].iter().map(|v| 1.0 + v).product::<f64>() * PI / 2.0;
    let mut y = w;
    for idx in 0..ks.len() - 1 {
        let kn = ks[idx];
        let knext = ks[idx + 1];
        y = 2.0 * y / ((1.0 + knext) * (1.0 + (1.0 + kn * kn * y * y).sqrt()));
    }
    kcap * 2.0 / PI * y.asinh()
}

// ------------------------------------------------------------------------------
// Analog prototypes (cutoff 1 rad/s)
// ------------------------------------------------------------------------------

pub fn buttap(n: usize) -> Zpk {
    let p = (0..n)
        .map(|i| {
            let m = -(n as f64) + 1.0 + 2.0 * i as f64;
            -C::polar(1.0, PI * m / (2.0 * n as f64))
        })
        .collect();
    Zpk { z: vec![], p, k: 1.0 }
}

pub fn cheb1ap(n: usize, rp: f64) -> Zpk {
    let eps = (10f64.powf(0.1 * rp) - 1.0).sqrt();
    let mu = (1.0 / eps).asinh() / n as f64;
    let p: Vec<C> = (0..n)
        .map(|i| {
            let m = -(n as f64) + 1.0 + 2.0 * i as f64;
            let theta = PI * m / (2.0 * n as f64);
            // -sinh(mu + j theta)
            C::new(-(mu.sinh() * theta.cos()), -(mu.cosh() * theta.sin()))
        })
        .collect();
    let mut k = prod_neg(&p).re;
    if n & 1 == 0 {
        k /= (1.0 + eps * eps).sqrt();
    }
    Zpk { z: vec![], p, k }
}

pub fn cheb2ap(n: usize, rs: f64) -> Zpk {
    let de = 1.0 / (10f64.powf(0.1 * rs) - 1.0).sqrt();
    let mu = (1.0 / de).asinh() / n as f64;
    let ms: Vec<f64> = if n % 2 == 1 {
        let mut v: Vec<f64> = (0..).map(|i| -(n as f64) + 1.0 + 2.0 * i as f64).take_while(|m| *m < 0.0).collect();
        v.extend((0..).map(|i| 2.0 + 2.0 * i as f64).take_while(|m| *m < n as f64));
        v
    } else {
        (0..n).map(|i| -(n as f64) + 1.0 + 2.0 * i as f64).collect()
    };
    let z: Vec<C> = ms
        .iter()
        .map(|m| {
            // -conj(j / sin(m pi / 2n))
            let s = (m * PI / (2.0 * n as f64)).sin();
            -C::new(0.0, 1.0 / s).conj()
        })
        .collect();
    let p: Vec<C> = (0..n)
        .map(|i| {
            let m = -(n as f64) + 1.0 + 2.0 * i as f64;
            let e = -C::polar(1.0, PI * m / (2.0 * n as f64));
            let q = C::new(mu.sinh() * e.re, mu.cosh() * e.im);
            q.inv()
        })
        .collect();
    let k = (prod_neg(&p) / prod_neg(&z)).re;
    Zpk { z, p, k }
}

/// Roots of a monic real polynomial `c[0] + c[1] x + ... + x^n` by Aberth iteration.
fn poly_roots(c: &[f64]) -> Vec<C> {
    let n = c.len() - 1;
    let eval = |x: C| -> (C, C) {
        let mut p = C::new(c[n], 0.0);
        let mut d = C::ZERO;
        for k in (0..n).rev() {
            d = d * x + p;
            p = p * x + C::new(c[k], 0.0);
        }
        (p, d)
    };
    let radius = c[0].abs().powf(1.0 / n as f64).max(1e-3);
    let mut z: Vec<C> = (0..n).map(|i| C::polar(radius, 2.0 * PI * (i as f64 + 0.25) / n as f64 + 0.4)).collect();
    for _ in 0..500 {
        let mut maxstep: f64 = 0.0;
        for i in 0..n {
            let (p, d) = eval(z[i]);
            if p.norm_sqr() == 0.0 {
                continue;
            }
            let ratio = p / d;
            let mut sum = C::ZERO;
            for j in 0..n {
                if j != i {
                    sum += (z[i] - z[j]).inv();
                }
            }
            let w = ratio / (C::new(1.0, 0.0) - ratio * sum);
            z[i] -= w;
            maxstep = maxstep.max(w.abs() / z[i].abs().max(1e-30));
        }
        if maxstep < 1e-15 {
            break;
        }
    }
    z
}

/// Bessel prototype normalised so the magnitude is -3 dB at 1 rad/s.
pub fn besselap(n: usize) -> Zpk {
    // reverse Bessel polynomial: a_k = (2n-k)! / (2^(n-k) k! (n-k)!)
    let fact = |m: usize| -> f64 { (1..=m).map(|v| v as f64).product::<f64>() };
    let coeffs: Vec<f64> =
        (0..=n).map(|k| fact(2 * n - k) / (2f64.powi((n - k) as i32) * fact(k) * fact(n - k))).collect();
    let mut p = if n == 1 { vec![C::new(-coeffs[0], 0.0)] } else { poly_roots(&coeffs) };
    // clean conjugate symmetry
    for r in p.iter_mut() {
        if r.im.abs() < 1e-9 * r.abs() {
            r.im = 0.0;
        }
    }
    let mag = |p: &[C], w: f64| -> f64 {
        let s = C::new(0.0, w);
        let num = prod_neg(p);
        let den = p.iter().fold(C::new(1.0, 0.0), |acc, r| acc * (s - *r));
        (num / den).abs()
    };
    let target = std::f64::consts::FRAC_1_SQRT_2;
    let (mut lo, mut hi) = (1e-3f64, 1e3f64);
    for _ in 0..200 {
        let mid = (lo * hi).sqrt();
        if mag(&p, mid) > target {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let w3 = (lo * hi).sqrt();
    for r in p.iter_mut() {
        *r = r.scale(1.0 / w3);
    }
    let k = prod_neg(&p).re;
    Zpk { z: vec![], p, k }
}

pub fn ellipap(n: usize, rp: f64, rs: f64) -> Zpk {
    if n == 1 {
        let p = -(1.0 / (10f64.powf(0.1 * rp) - 1.0)).sqrt();
        return Zpk { z: vec![], p: vec![C::new(p, 0.0)], k: -p };
    }
    let eps_sq = 10f64.powf(0.1 * rp) - 1.0;
    let eps = eps_sq.sqrt();
    let ck1_sq = eps_sq / (10f64.powf(0.1 * rs) - 1.0);
    let val0 = ellipk(ck1_sq);
    let m = ellipdeg(n, ck1_sq);
    let capk = ellipk(m);
    let js: Vec<f64> = ((1 - n % 2)..n).step_by(2).map(|j| j as f64).collect();
    let scd: Vec<(f64, f64, f64)> = js.iter().map(|j| ellipj(j * capk / n as f64, m)).collect();
    let mut z = Vec::new();
    for (s, _, _) in &scd {
        if s.abs() > 1e-12 {
            z.push(C::new(0.0, 1.0 / (m.sqrt() * s)));
        }
    }
    let zc: Vec<C> = z.iter().map(|v| v.conj()).collect();
    z.extend(zc);
    let r = arc_jac_sc1(1.0 / eps, ck1_sq);
    let v0 = capk * r / (n as f64 * val0);
    let (sv, cv, dv) = ellipj(v0, 1.0 - m);
    let mut p: Vec<C> = scd
        .iter()
        .map(|(s, c, d)| {
            let den = 1.0 - (d * sv).powi(2);
            C::new(-(c * d * sv * cv) / den, -(s * dv) / den)
        })
        .collect();
    if n % 2 == 1 {
        let newp: Vec<C> = p.iter().filter(|v| v.im.abs() > 1e-12 * v.abs()).map(|v| v.conj()).collect();
        for v in p.iter_mut() {
            if v.im.abs() <= 1e-12 * v.abs() {
                v.im = 0.0;
            }
        }
        p.extend(newp);
    } else {
        let pc: Vec<C> = p.iter().map(|v| v.conj()).collect();
        p.extend(pc);
    }
    let mut k = (prod_neg(&p) / prod_neg(&z)).re;
    if n & 1 == 0 {
        k /= (1.0 + eps_sq).sqrt();
    }
    Zpk { z, p, k }
}

// ------------------------------------------------------------------------------
// Transformations
// ------------------------------------------------------------------------------

fn lp2lp(f: Zpk, wo: f64) -> Zpk {
    let deg = f.p.len() as i32 - f.z.len() as i32;
    Zpk {
        z: f.z.iter().map(|v| v.scale(wo)).collect(),
        p: f.p.iter().map(|v| v.scale(wo)).collect(),
        k: f.k * wo.powi(deg),
    }
}

fn lp2hp(f: Zpk, wo: f64) -> Zpk {
    let deg = f.p.len() - f.z.len();
    let mut z: Vec<C> = f.z.iter().map(|v| C::new(wo, 0.0) / *v).collect();
    let p: Vec<C> = f.p.iter().map(|v| C::new(wo, 0.0) / *v).collect();
    z.extend(std::iter::repeat_n(C::ZERO, deg));
    let k = f.k * (prod_neg(&f.z) / prod_neg(&f.p)).re;
    Zpk { z, p, k }
}

fn lp2bp(f: Zpk, wo: f64, bw: f64) -> Zpk {
    let deg = f.p.len() - f.z.len();
    let tr = |v: &C| -> (C, C) {
        let l = v.scale(bw / 2.0);
        let s = (l * l - C::new(wo * wo, 0.0)).sqrt();
        (l + s, l - s)
    };
    let mut z = Vec::new();
    let mut z2 = Vec::new();
    for v in &f.z {
        let (a, b) = tr(v);
        z.push(a);
        z2.push(b);
    }
    z.extend(z2);
    let mut p = Vec::new();
    let mut p2 = Vec::new();
    for v in &f.p {
        let (a, b) = tr(v);
        p.push(a);
        p2.push(b);
    }
    p.extend(p2);
    z.extend(std::iter::repeat_n(C::ZERO, deg));
    Zpk { z, p, k: f.k * bw.powi(deg as i32) }
}

fn lp2bs(f: Zpk, wo: f64, bw: f64) -> Zpk {
    let deg = f.p.len() - f.z.len();
    let tr = |v: &C| -> (C, C) {
        let h = C::new(bw / 2.0, 0.0) / *v;
        let s = (h * h - C::new(wo * wo, 0.0)).sqrt();
        (h + s, h - s)
    };
    let mut z = Vec::new();
    let mut z2 = Vec::new();
    for v in &f.z {
        let (a, b) = tr(v);
        z.push(a);
        z2.push(b);
    }
    z.extend(z2);
    let mut p = Vec::new();
    let mut p2 = Vec::new();
    for v in &f.p {
        let (a, b) = tr(v);
        p.push(a);
        p2.push(b);
    }
    p.extend(p2);
    for _ in 0..deg {
        z.push(C::new(0.0, wo));
        z.push(C::new(0.0, -wo));
    }
    let k = f.k * (prod_neg(&f.z) / prod_neg(&f.p)).re;
    Zpk { z, p, k }
}

fn bilinear(f: Zpk, fs: f64) -> Zpk {
    let fs2 = C::new(2.0 * fs, 0.0);
    let deg = f.p.len() - f.z.len();
    let mut z: Vec<C> = f.z.iter().map(|v| (fs2 + *v) / (fs2 - *v)).collect();
    let p: Vec<C> = f.p.iter().map(|v| (fs2 + *v) / (fs2 - *v)).collect();
    z.extend(std::iter::repeat_n(C::new(-1.0, 0.0), deg));
    let num = f.z.iter().fold(C::new(1.0, 0.0), |a, v| a * (fs2 - *v));
    let den = f.p.iter().fold(C::new(1.0, 0.0), |a, v| a * (fs2 - *v));
    let k = f.k * (num / den).re;
    Zpk { z, p, k }
}

/// Group roots into conjugate pairs (`Some(im>0 root)`) and real singles.
fn split_roots(v: &[C]) -> (Vec<C>, Vec<f64>) {
    let mut cplx = Vec::new();
    let mut reals = Vec::new();
    for r in v {
        if r.im.abs() <= 1e-9 * r.abs().max(1.0) {
            reals.push(r.re);
        } else if r.im > 0.0 {
            cplx.push(*r);
        }
    }
    (cplx, reals)
}

#[derive(Clone, Copy, Debug)]
enum Group {
    Pair(C),
    Reals(f64, f64),
    Single(f64),
}

impl Group {
    fn poly(&self) -> (f64, f64, f64) {
        match *self {
            Group::Pair(r) => (1.0, -2.0 * r.re, r.norm_sqr()),
            Group::Reals(a, b) => (1.0, -(a + b), a * b),
            Group::Single(a) => (1.0, -a, 0.0),
        }
    }
    fn rep(&self) -> C {
        match *self {
            Group::Pair(r) => r,
            Group::Reals(a, b) => {
                if a.abs() > b.abs() {
                    C::new(a, 0.0)
                } else {
                    C::new(b, 0.0)
                }
            }
            Group::Single(a) => C::new(a, 0.0),
        }
    }
    fn order(&self) -> usize {
        match self {
            Group::Single(_) => 1,
            _ => 2,
        }
    }
}

fn groups(v: &[C], single_near: Option<f64>) -> Vec<Group> {
    let (cplx, mut reals) = split_roots(v);
    let mut g: Vec<Group> = cplx.into_iter().map(Group::Pair).collect();
    let mut single = None;
    if reals.len() % 2 == 1 {
        let target = single_near.unwrap_or(0.0);
        let (idx, _) = reals
            .iter()
            .enumerate()
            .min_by(|a, b| (a.1 - target).abs().partial_cmp(&(b.1 - target).abs()).unwrap())
            .unwrap();
        single = Some(reals.remove(idx));
    }
    reals.sort_by(|a, b| a.partial_cmp(b).unwrap());
    for ch in reals.chunks(2) {
        g.push(Group::Reals(ch[0], ch[1]));
    }
    if let Some(s) = single {
        g.push(Group::Single(s));
    }
    g
}

/// Pair poles with the nearest zeros and order the sections so the poles closest to
/// the unit circle come last.
pub fn zpk_to_sos(f: &Zpk) -> Sos {
    let pg = groups(&f.p, None);
    let single_pole = pg.iter().find_map(|g| match g {
        Group::Single(a) => Some(*a),
        _ => None,
    });
    let mut zg = groups(&f.z, single_pole);
    let mut pg = pg;
    // closest to the unit circle first for pairing
    pg.sort_by(|a, b| (1.0 - a.rep().abs()).abs().partial_cmp(&(1.0 - b.rep().abs()).abs()).unwrap());
    let mut secs: Vec<Coefs> = Vec::new();
    for g in &pg {
        let (_, a1, a2) = g.poly();
        let rep = g.rep();
        // prefer a zero group of the same order
        let pick = zg
            .iter()
            .enumerate()
            .filter(|(_, z)| z.order() == g.order())
            .min_by(|a, b| (a.1.rep() - rep).abs().partial_cmp(&(b.1.rep() - rep).abs()).unwrap())
            .map(|(i, _)| i)
            .or_else(|| {
                zg.iter()
                    .enumerate()
                    .min_by(|a, b| (a.1.rep() - rep).abs().partial_cmp(&(b.1.rep() - rep).abs()).unwrap())
                    .map(|(i, _)| i)
            });
        let (b0, b1, b2) = match pick {
            Some(i) => zg.remove(i).poly(),
            None => (1.0, 0.0, 0.0),
        };
        secs.push(Coefs { b0, b1, b2, a1, a2 });
    }
    secs.reverse();
    Sos { sections: secs, gain: f.k }
}

/// A complete digital design. `f1` is the cutoff (or lower edge), `f2` the upper edge for
/// band filters. `order` is the prototype order (band filters have twice as many poles).
#[allow(clippy::too_many_arguments)]
pub fn design(
    design: Design,
    band: Band,
    order: usize,
    f1: f64,
    f2: f64,
    ripple_db: f64,
    stop_db: f64,
    fs: f64,
) -> Sos {
    let order = order.clamp(1, 24);
    let proto = match design {
        Design::Butterworth => buttap(order),
        Design::Chebyshev1 => cheb1ap(order, ripple_db.max(1e-3)),
        Design::Chebyshev2 => cheb2ap(order, stop_db.max(1.0)),
        Design::Bessel => besselap(order.min(16)),
        Design::Elliptic => ellipap(order, ripple_db.max(1e-3), stop_db.max(ripple_db + 1.0)),
    };
    let warp = |f: f64| 2.0 * fs * (PI * (f / fs).clamp(1e-7, 0.4999)).tan();
    let analog = match band {
        Band::Lowpass => lp2lp(proto, warp(f1)),
        Band::Highpass => lp2hp(proto, warp(f1)),
        Band::Bandpass | Band::Bandstop => {
            let w1 = warp(f1.min(f2));
            let w2 = warp(f1.max(f2));
            let wo = (w1 * w2).sqrt();
            let bw = (w2 - w1).max(1e-6);
            if band == Band::Bandpass {
                lp2bp(proto, wo, bw)
            } else {
                lp2bs(proto, wo, bw)
            }
        }
    };
    zpk_to_sos(&bilinear(analog, fs))
}

#[cfg(test)]
mod tests {
    use super::*;
    const FS: f64 = 48000.0;

    #[test]
    fn jacobi_identities() {
        let (sn, cn, dn) = ellipj(0.7, 0.5);
        assert!((sn * sn + cn * cn - 1.0).abs() < 1e-12);
        assert!((dn * dn + 0.5 * sn * sn - 1.0).abs() < 1e-12);
        // sn(K, m) = 1
        let k = ellipk(0.3);
        let (s, _, _) = ellipj(k, 0.3);
        assert!((s - 1.0).abs() < 1e-10);
        // K(0) = pi/2; K(0.5) = 1.854074677301372
        assert!((ellipk(0.0) - PI / 2.0).abs() < 1e-14);
        assert!((ellipk(0.5) - 1.854_074_677_301_372).abs() < 1e-12);
    }

    #[test]
    fn butterworth_and_chebyshev_edges() {
        let s = design(Design::Butterworth, Band::Lowpass, 4, 1000.0, 0.0, 0.5, 60.0, FS);
        assert!((s.magnitude_db(1000.0, FS) + 3.0103).abs() < 0.01);
        assert!(s.magnitude_db(100.0, FS).abs() < 0.01);
        let c = design(Design::Chebyshev1, Band::Lowpass, 4, 1000.0, 0.0, 0.5, 60.0, FS);
        assert!((c.magnitude_db(1000.0, FS) + 0.5).abs() < 0.01);
        for f in [10.0, 200.0, 500.0, 800.0, 950.0] {
            let m = c.magnitude_db(f, FS);
            assert!((-0.51..=0.01).contains(&m), "{f}: {m}");
        }
        let c2 = design(Design::Chebyshev2, Band::Lowpass, 4, 1000.0, 0.0, 0.5, 60.0, FS);
        assert!((c2.magnitude_db(1000.0, FS) + 60.0).abs() < 0.05);
        for f in [1000.0, 1500.0, 3000.0, 10000.0] {
            assert!(c2.magnitude_db(f, FS) <= -59.9, "{f}");
        }
        assert!(c2.magnitude_db(50.0, FS).abs() < 0.1);
    }

    #[test]
    fn elliptic_meets_spec() {
        let e = design(Design::Elliptic, Band::Lowpass, 4, 1000.0, 0.0, 0.5, 60.0, FS);
        for i in 0..100 {
            let f = 10.0 + i as f64 * 9.9;
            let m = e.magnitude_db(f, FS);
            assert!((-0.51..=0.01).contains(&m), "passband {f}: {m}");
        }
        assert!((e.magnitude_db(1000.0, FS) + 0.5).abs() < 0.02);
        // the stopband begins where the response first reaches -60 dB, then stays below
        let mut edge = None;
        for i in 0..4000 {
            let f = 1000.0 + i as f64 * 2.0;
            if e.magnitude_db(f, FS) <= -59.9 {
                edge = Some(f);
                break;
            }
        }
        let edge = edge.expect("reaches stopband");
        assert!(edge < 4000.0, "order-4 elliptic transition too wide: {edge}");
        for i in 0..2000 {
            let f = edge + i as f64 * 10.0;
            if f >= FS / 2.0 {
                break;
            }
            assert!(e.magnitude_db(f, FS) <= -59.5, "stopband {f}");
        }
    }

    #[test]
    fn bessel_and_band_designs() {
        let b = design(Design::Bessel, Band::Lowpass, 4, 1000.0, 0.0, 0.5, 60.0, FS);
        assert!((b.magnitude_db(1000.0, FS) + 3.01).abs() < 0.05);
        // nearly flat group delay in the passband
        let g1 = b.group_delay(100.0, FS);
        let g2 = b.group_delay(500.0, FS);
        assert!((g1 - g2).abs() / g1 < 0.05, "{g1} {g2}");
        let bp = design(Design::Butterworth, Band::Bandpass, 2, 500.0, 2000.0, 0.5, 60.0, FS);
        assert!((bp.magnitude_db(500.0, FS) + 3.01).abs() < 0.05);
        assert!((bp.magnitude_db(2000.0, FS) + 3.01).abs() < 0.05);
        assert!(bp.magnitude_db(1000.0, FS).abs() < 0.05);
        let bs = design(Design::Butterworth, Band::Bandstop, 2, 500.0, 2000.0, 0.5, 60.0, FS);
        assert!(bs.magnitude_db(1000.0, FS) < -60.0);
        assert!(bs.magnitude_db(50.0, FS).abs() < 0.05);
        let hp = design(Design::Elliptic, Band::Highpass, 5, 1000.0, 0.0, 1.0, 50.0, FS);
        assert!(hp.magnitude_db(10000.0, FS).abs() < 1.01);
        assert!(hp.magnitude_db(100.0, FS) < -49.0);
    }
}
