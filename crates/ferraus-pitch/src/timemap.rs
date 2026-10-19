//! Piecewise-linear time maps shared by the phase vocoder and WSOLA.
//!
//! A map relates input sample positions to output sample positions. Transient anchoring
//! inserts slope-1 ("protected") windows around each onset so the attack is copied
//! verbatim and lands exactly at `factor × onset` in the output.

/// Piecewise-linear, strictly increasing map between input and output positions.
#[derive(Clone, Debug)]
pub struct TimeMap {
    /// (input, output) breakpoints, strictly increasing in both coordinates.
    pts: Vec<(f64, f64)>,
    /// Output ranges copied at slope 1 around transients.
    protected: Vec<(f64, f64)>,
    pub in_len: usize,
    pub out_len: usize,
}

impl TimeMap {
    /// Uniform stretch: output length `round(in_len × factor)`.
    pub fn uniform(in_len: usize, factor: f64) -> TimeMap {
        let out_len = (in_len as f64 * factor).round() as usize;
        TimeMap { pts: vec![(0.0, 0.0), (in_len as f64, out_len as f64)], protected: Vec::new(), in_len, out_len }
    }

    /// Uniform stretch with slope-1 windows `[onset − pre, onset + post]` around each
    /// onset (input samples). Onsets too close to each other or to the ends to fit their
    /// window are not protected. Each protected onset maps to exactly `onset × factor`.
    pub fn anchored(in_len: usize, factor: f64, onsets: &[usize], pre: usize, post: usize) -> TimeMap {
        let out_len = (in_len as f64 * factor).round() as usize;
        let mut pts = vec![(0.0, 0.0)];
        let mut protected = Vec::new();
        let margin = 16.0;
        let (pre, post) = (pre as f64, post as f64);
        for &p in onsets {
            let p = p as f64;
            let o = p * factor;
            let (li, lo) = *pts.last().unwrap();
            // an onset at the very start (e.g. a click at sample 0) is protected from 0
            if pts.len() == 1 && p < pre && p * (factor - 1.0).abs() < 1.0 {
                let b = (p + post, p + post);
                if b.0 < in_len as f64 - margin && b.1 < out_len as f64 - margin {
                    pts.push(b);
                    protected.push((0.0, b.1));
                }
                continue;
            }
            let a = (p - pre, o - pre);
            let b = (p + post, o + post);
            if a.0 > li + margin && a.1 > lo + margin && b.0 < in_len as f64 - margin && b.1 < out_len as f64 - margin {
                pts.push(a);
                pts.push(b);
                protected.push((a.1, b.1));
            }
        }
        pts.push((in_len as f64, out_len as f64));
        TimeMap { pts, protected, in_len, out_len }
    }

    /// Input position for an output position (linear extrapolation outside the map).
    pub fn in_at(&self, out: f64) -> f64 {
        let pts = &self.pts;
        let k = match pts.iter().position(|p| p.1 > out) {
            Some(0) => 1,
            Some(k) => k,
            None => pts.len() - 1,
        };
        let (i0, o0) = pts[k - 1];
        let (i1, o1) = pts[k];
        if (o1 - o0).abs() < 1e-12 {
            return i0;
        }
        i0 + (out - o0) * (i1 - i0) / (o1 - o0)
    }

    /// Is this output position inside a protected (slope-1) window?
    pub fn is_protected(&self, out: f64) -> bool {
        self.protected.iter().any(|(a, b)| out >= *a && out <= *b)
    }

    /// Protected output windows.
    pub fn protected(&self) -> &[(f64, f64)] {
        &self.protected
    }

    /// Breakpoints (input, output).
    pub fn points(&self) -> &[(f64, f64)] {
        &self.pts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uniform_and_anchored() {
        let m = TimeMap::uniform(1000, 2.0);
        assert_eq!(m.out_len, 2000);
        assert!((m.in_at(1000.0) - 500.0).abs() < 1e-9);
        let m = TimeMap::anchored(10000, 2.0, &[2000, 2100, 6000], 100, 100);
        // 2100 is too close to 2000 and is skipped
        assert_eq!(m.protected().len(), 2);
        assert!((m.in_at(4000.0) - 2000.0).abs() < 1e-9);
        assert!((m.in_at(4050.0) - 2050.0).abs() < 1e-9);
        assert!(m.is_protected(4000.0));
        assert!(!m.is_protected(5000.0));
        assert!((m.in_at(20000.0) - 10000.0).abs() < 1e-9);
    }
}
