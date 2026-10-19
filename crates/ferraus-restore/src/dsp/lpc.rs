//! Autoregressive (LPC) models and least-squares AR interpolation of missing samples
//! (the Janssen/Veldhuis method used by declip and declick).

/// Prediction-error filter `a[0..=p]` with `a[0] == 1`: `e[n] = sum_k a[k] x[n-k]`.
pub type Ar = Vec<f64>;

/// Autocorrelation-method AR estimate (Levinson-Durbin) with a Hann window.
pub fn ar_autocorr(x: &[f64], order: usize) -> Ar {
    let n = x.len();
    let p = order.min(n.saturating_sub(1)).max(1);
    let w: Vec<f64> = (0..n)
        .map(|i| if n > 1 { 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (n - 1) as f64).cos() } else { 1.0 })
        .collect();
    let xw: Vec<f64> = x.iter().zip(&w).map(|(a, b)| a * b).collect();
    let mut r = vec![0.0; p + 1];
    for (lag, rv) in r.iter_mut().enumerate() {
        *rv = (lag..n).map(|i| xw[i] * xw[i - lag]).sum();
    }
    // tiny white-noise floor keeps Levinson stable on silence or pure tones
    r[0] *= 1.0 + 1e-9;
    r[0] += 1e-20;
    levinson(&r, p)
}

/// Levinson-Durbin recursion on autocorrelation `r[0..=p]`.
pub fn levinson(r: &[f64], p: usize) -> Ar {
    let mut a = vec![0.0; p + 1];
    a[0] = 1.0;
    let mut err = r[0];
    if err <= 0.0 {
        return a;
    }
    let mut tmp = vec![0.0; p + 1];
    for i in 1..=p {
        let mut acc = r[i];
        for j in 1..i {
            acc += a[j] * r[i - j];
        }
        let k = -acc / err;
        tmp[..=i].copy_from_slice(&a[..=i]);
        for j in 1..i {
            a[j] = tmp[j] + k * tmp[i - j];
        }
        a[i] = k;
        err *= 1.0 - k * k;
        if err <= 0.0 {
            break;
        }
    }
    a
}

/// Covariance-method AR estimate using only prediction rows whose samples are all
/// known (`known[i] == true`). Returns `None` when there are too few complete rows.
pub fn ar_covariance_known(x: &[f64], known: &[bool], order: usize) -> Option<Ar> {
    let n = x.len();
    let p = order;
    if p == 0 || n <= p {
        return None;
    }
    // rows n0 where known[n0-p..=n0]
    let mut run = 0usize;
    let mut rows = Vec::new();
    for i in 0..n {
        if known[i] {
            run += 1;
        } else {
            run = 0;
        }
        if run > p {
            rows.push(i);
        }
    }
    if rows.len() < 2 * p {
        return None;
    }
    // normal equations: R a' = -r, a' = a[1..=p]
    let mut rm = vec![0.0; p * p];
    let mut rv = vec![0.0; p];
    for &t in &rows {
        for i in 0..p {
            let xi = x[t - 1 - i];
            rv[i] += xi * x[t];
            for j in 0..=i {
                rm[i * p + j] += xi * x[t - 1 - j];
            }
        }
    }
    for i in 0..p {
        for j in 0..i {
            rm[j * p + i] = rm[i * p + j];
        }
    }
    let tr: f64 = (0..p).map(|i| rm[i * p + i]).sum::<f64>() / p as f64;
    for i in 0..p {
        rm[i * p + i] += tr * 1e-7 + 1e-18;
    }
    let sol = cholesky_solve(&mut rm, p, &rv.iter().map(|v| -v).collect::<Vec<_>>())?;
    let mut a = vec![1.0];
    a.extend(sol);
    Some(a)
}

/// Dense Cholesky solve of a symmetric positive-definite `p x p` system (row-major).
pub fn cholesky_solve(m: &mut [f64], p: usize, b: &[f64]) -> Option<Vec<f64>> {
    for j in 0..p {
        let mut d = m[j * p + j];
        for k in 0..j {
            d -= m[j * p + k] * m[j * p + k];
        }
        if d <= 0.0 || !d.is_finite() {
            return None;
        }
        let d = d.sqrt();
        m[j * p + j] = d;
        for i in j + 1..p {
            let mut s = m[i * p + j];
            for k in 0..j {
                s -= m[i * p + k] * m[j * p + k];
            }
            m[i * p + j] = s / d;
        }
    }
    let mut y = vec![0.0; p];
    for i in 0..p {
        let mut s = b[i];
        for k in 0..i {
            s -= m[i * p + k] * y[k];
        }
        y[i] = s / m[i * p + i];
    }
    for i in (0..p).rev() {
        let mut s = y[i];
        for k in i + 1..p {
            s -= m[k * p + i] * y[k];
        }
        y[i] = s / m[i * p + i];
    }
    Some(y)
}

/// Forward prediction error of `x` under `a` (first `p` samples are 0).
pub fn prediction_error(x: &[f64], a: &[f64]) -> Vec<f64> {
    let p = a.len() - 1;
    let mut e = vec![0.0; x.len()];
    for n in p..x.len() {
        let mut s = 0.0;
        for (k, ak) in a.iter().enumerate() {
            s += ak * x[n - k];
        }
        e[n] = s;
    }
    e
}

/// Least-squares interpolation of the samples where `known[i] == false`, minimising the
/// AR prediction error energy `||A x||^2` over the block. Solved with a banded Cholesky
/// factorisation (bandwidth `p`), so long gaps are cheap. Returns false if the system
/// was singular (the block is left untouched).
pub fn ar_interpolate(x: &mut [f64], known: &[bool], a: &[f64]) -> bool {
    let n = x.len();
    let p = a.len() - 1;
    let missing: Vec<usize> = (0..n).filter(|&i| !known[i]).collect();
    let m = missing.len();
    if m == 0 {
        return true;
    }
    if n <= p {
        return false;
    }
    // index of each sample in the compressed unknown vector
    let mut pos = vec![usize::MAX; n];
    for (ci, &i) in missing.iter().enumerate() {
        pos[i] = ci;
    }
    // error from known samples only
    let mut ek = vec![0.0; n];
    for t in p..n {
        let mut s = 0.0;
        for (k, ak) in a.iter().enumerate() {
            let i = t - k;
            if known[i] {
                s += ak * x[i];
            }
        }
        ek[t] = s;
    }
    // band storage: band[ci * (p+1) + d] = M[ci][ci-d] for d in 0..=p
    let bw = p;
    let mut band = vec![0.0; m * (bw + 1)];
    let mut rhs = vec![0.0; m];
    for (ci, &i) in missing.iter().enumerate() {
        // rhs_i = -sum_t a[t-i] ek[t], t in [max(i,p), min(i+p, n-1)]
        let t0 = i.max(p);
        let t1 = (i + p).min(n - 1);
        let mut s = 0.0;
        for t in t0..=t1 {
            s += a[t - i] * ek[t];
        }
        rhs[ci] = -s;
        // M[i][j] for missing j <= i within distance p
        for d in 0..=bw {
            if d > ci {
                break;
            }
            let cj = ci - d;
            let j = missing[cj];
            if i - j > p {
                continue;
            }
            let t0 = i.max(p);
            let t1 = (j + p).min(n - 1);
            let mut s = 0.0;
            if t0 <= t1 {
                for t in t0..=t1 {
                    s += a[t - i] * a[t - j];
                }
            }
            band[ci * (bw + 1) + d] = s;
        }
    }
    // regularise slightly
    let diag_mean = (0..m).map(|ci| band[ci * (bw + 1)]).sum::<f64>() / m as f64;
    for ci in 0..m {
        band[ci * (bw + 1)] += diag_mean * 1e-10 + 1e-30;
    }
    // banded Cholesky: L stored in the same layout
    for ci in 0..m {
        for d in (0..=bw.min(ci)).rev() {
            let cj = ci - d;
            // L[ci][cj] = (M[ci][cj] - sum_k L[ci][k] L[cj][k]) / L[cj][cj], k < cj, k >= ci - bw
            let mut s = band[ci * (bw + 1) + d];
            let kmin = ci.saturating_sub(bw);
            for k in kmin..cj {
                let lik = band[ci * (bw + 1) + (ci - k)];
                let ljk = if cj - k <= bw { band[cj * (bw + 1) + (cj - k)] } else { 0.0 };
                s -= lik * ljk;
            }
            if d == 0 {
                if s <= 0.0 || !s.is_finite() {
                    return false;
                }
                band[ci * (bw + 1)] = s.sqrt();
            } else {
                band[ci * (bw + 1) + d] = s / band[cj * (bw + 1)];
            }
        }
    }
    // forward substitution
    let mut y = vec![0.0; m];
    for ci in 0..m {
        let mut s = rhs[ci];
        for d in 1..=bw.min(ci) {
            s -= band[ci * (bw + 1) + d] * y[ci - d];
        }
        y[ci] = s / band[ci * (bw + 1)];
    }
    // back substitution with L^T
    for ci in (0..m).rev() {
        let mut s = y[ci];
        for d in 1..=bw {
            let ck = ci + d;
            if ck >= m {
                break;
            }
            s -= band[ck * (bw + 1) + d] * y[ck];
        }
        y[ci] = s / band[ci * (bw + 1)];
    }
    if y.iter().any(|v| !v.is_finite()) {
        return false;
    }
    for (ci, &i) in missing.iter().enumerate() {
        x[i] = y[ci];
    }
    let _ = pos;
    true
}

/// Repair the masked samples of a whole channel in blocks: for every block that
/// contains missing samples, estimate an AR model of `order` (covariance method on
/// complete rows when possible, else Janssen iterations of the autocorrelation method)
/// and interpolate. `missing[i] == true` marks samples to replace.
pub fn repair_channel(x: &mut [f32], missing: &[bool], order: usize, block: usize) -> usize {
    let n = x.len();
    if n == 0 {
        return 0;
    }
    let order = order.max(2);
    let ctx = (order * 8).max(256);
    let block = block.max(order * 8);
    let mut repaired = 0;
    let mut start = 0;
    let orig: Vec<f64> = x.iter().map(|v| *v as f64).collect();
    while start < n {
        let end = (start + block).min(n);
        if !missing[start..end].iter().any(|m| *m) {
            start = end;
            continue;
        }
        let s0 = start.saturating_sub(ctx);
        let e0 = (end + ctx).min(n);
        let mut seg: Vec<f64> = orig[s0..e0].to_vec();
        // samples already repaired in earlier blocks count as known context
        for i in s0..start {
            seg[i - s0] = x[i] as f64;
        }
        // the model is estimated on genuinely known samples only; samples repaired in
        // the previous block act as known boundary values for the interpolation
        let est_known: Vec<bool> = (s0..e0).map(|i| !missing[i]).collect();
        let known: Vec<bool> = (s0..e0).map(|i| if i < start { true } else { !missing[i] }).collect();
        let limit = 8.0 * seg.iter().zip(&known).filter(|(_, k)| **k).map(|(v, _)| v.abs()).fold(1e-9, f64::max);
        let mut done = false;
        // covariance estimates on complete rows at several orders; high orders fitted to
        // very simple material can be ill-conditioned, so each candidate interpolation
        // is scored by how predictable the repaired block is under an independent
        // (minimum-phase, Levinson) model, and the most predictable one wins
        let first_missing = (start..end).find(|&i| missing[i]).unwrap_or(start) - s0;
        let last_missing = (start..end).rev().find(|&i| missing[i]).unwrap_or(end - 1) - s0;
        let lo = first_missing.saturating_sub(64);
        let hi = (last_missing + 64).min(seg.len());
        let mut best: Option<(f64, Vec<f64>)> = None;
        let mut p = order;
        while p >= 2 {
            if let Some(a) = ar_covariance_known(&seg, &est_known, p) {
                let mut trial = seg.clone();
                if ar_interpolate(&mut trial, &known, &a) && trial.iter().all(|v| v.abs() <= limit) {
                    let score = predictability(&trial[lo..hi], 12);
                    if best.as_ref().map(|(s, _)| score < *s).unwrap_or(true) {
                        best = Some((score, trial));
                    }
                }
            }
            p = if p > 3 { p / 2 } else { p - 1 };
        }
        if let Some((_, trial)) = best {
            seg = trial;
            done = true;
        }
        if !done {
            // Janssen: start from linear interpolation, alternate estimate/interpolate
            linear_fill(&mut seg, &known);
            for _ in 0..4 {
                let a = ar_autocorr(&seg, order);
                if !ar_interpolate(&mut seg, &known, &a) {
                    break;
                }
            }
        }
        for i in start..end {
            if missing[i] {
                let v = seg[i - s0];
                if v.is_finite() {
                    x[i] = v as f32;
                    repaired += 1;
                }
            }
        }
        start = end;
    }
    repaired
}

/// Normalised prediction-error energy of `x` under its own order-`p` Levinson model
/// (lower is more predictable).
fn predictability(x: &[f64], p: usize) -> f64 {
    if x.len() <= 2 * p {
        return 1.0;
    }
    let a = ar_autocorr(x, p);
    let e = prediction_error(x, &a);
    let ee: f64 = e[p..].iter().map(|v| v * v).sum();
    let xx: f64 = x[p..].iter().map(|v| v * v).sum::<f64>().max(1e-30);
    ee / xx
}

/// Fill unknown samples by linear interpolation between known neighbours.
pub fn linear_fill(x: &mut [f64], known: &[bool]) {
    let n = x.len();
    let mut i = 0;
    while i < n {
        if known[i] {
            i += 1;
            continue;
        }
        let a = i;
        while i < n && !known[i] {
            i += 1;
        }
        let left = if a > 0 { Some(x[a - 1]) } else { None };
        let right = if i < n { Some(x[i]) } else { None };
        let (l, r) = match (left, right) {
            (Some(l), Some(r)) => (l, r),
            (Some(l), None) => (l, l),
            (None, Some(r)) => (r, r),
            (None, None) => (0.0, 0.0),
        };
        let len = (i - a + 1) as f64;
        for (k, v) in x[a..i].iter_mut().enumerate() {
            let t = (k + 1) as f64 / len;
            *v = l + (r - l) * t;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolates_sine_gap_exactly() {
        let x: Vec<f64> = (0..400).map(|i| (2.0 * std::f64::consts::PI * 0.013 * i as f64).sin()).collect();
        let mut known = vec![true; 400];
        for k in known.iter_mut().take(240).skip(200) {
            *k = false;
        }
        let a = ar_covariance_known(&x, &known, 4).unwrap();
        let mut y = x.clone();
        for i in 200..240 {
            y[i] = 0.0;
        }
        assert!(ar_interpolate(&mut y, &known, &a));
        for i in 200..240 {
            assert!((y[i] - x[i]).abs() < 1e-4, "{i}: {} vs {}", y[i], x[i]);
        }
    }

    #[test]
    fn repairs_heavily_clipped_f32_sine_in_blocks() {
        // 2/3 of the samples are missing; f32 rounding makes high orders ill-conditioned
        let x: Vec<f64> = (0..7000)
            .map(|i| 2.0 * (2.0 * std::f64::consts::PI * 1000.0 * i as f64 / 48000.0).sin())
            .map(|v| v as f32 as f64)
            .collect();
        let missing: Vec<bool> = x.iter().map(|v| v.abs() >= 1.0).collect();
        let mut y: Vec<f32> = x.iter().map(|v| v.clamp(-1.0, 1.0) as f32).collect();
        repair_channel(&mut y, &missing, 32, 2048);
        let err = x.iter().zip(&y).map(|(a, b)| (a - *b as f64).abs()).fold(0.0, f64::max);
        assert!(err < 1e-4, "max error {err}");
    }

    #[test]
    fn levinson_recovers_ar2() {
        // x[n] = 1.6 x[n-1] - 0.8 x[n-2] + noise
        let mut x = vec![0.0f64; 4000];
        let mut s = 12345u64;
        for n in 2..4000 {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            let e = (s >> 11) as f64 / (1u64 << 53) as f64 - 0.5;
            x[n] = 1.6 * x[n - 1] - 0.8 * x[n - 2] + e;
        }
        let a = ar_autocorr(&x, 2);
        assert!((a[1] + 1.6).abs() < 0.05, "{a:?}");
        assert!((a[2] - 0.8).abs() < 0.05, "{a:?}");
    }
}
