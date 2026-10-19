//! Helpers shared by the editing commands: range resolution with clear errors, zero-crossing
//! snapping, crossfade curves, short anti-click fades, and output-extension choice.

use ferraus_core::prelude::*;
use ferraus_core::units::{Time, TimeRange};
use std::f64::consts::FRAC_PI_2;

/// Round seconds for JSON results (microsecond precision keeps sample positions readable).
pub fn round_s(x: f64) -> f64 {
    (x * 1e6).round() / 1e6
}

/// Round decibel values for JSON results.
pub fn round_db(x: f64) -> f64 {
    if !x.is_finite() {
        return if x < 0.0 { -240.0 } else { 240.0 };
    }
    (x * 100.0).round() / 100.0
}

/// Frame position of a time without clamping to the buffer (so callers can tell when a
/// time lies beyond the end).
pub fn unclamped_frames(t: &Time, sr: u32, total: usize, bpm: Option<f64>) -> Result<f64> {
    let sr_f = sr as f64;
    Ok(match t {
        Time::Start => 0.0,
        Time::End => total as f64,
        Time::Samples(n) => *n as f64,
        Time::FromEnd(inner) => total as f64 - inner.seconds(sr, bpm)? * sr_f,
        other => other.seconds(sr, bpm)? * sr_f,
    })
}

/// Resolve a `start..end` range against a buffer of `total` frames.
///
/// * start after end → usage error (exit code 2)
/// * start at or beyond the end of the input → usage error
/// * end beyond the input → clamped, with `clamped = true`
pub fn resolve_range(
    r: &TimeRange,
    sr: u32,
    total: usize,
    bpm: Option<f64>,
) -> Result<(usize, usize, bool)> {
    let a = unclamped_frames(&r.start, sr, total, bpm)?;
    let b = unclamped_frames(&r.end, sr, total, bpm)?;
    if a > b + 0.5 {
        return Err(Error::usage(format!(
            "range start ({:.3} s) is after its end ({:.3} s)",
            a / sr as f64,
            b / sr as f64
        )));
    }
    if total > 0 && a.round() >= total as f64 {
        return Err(Error::usage(format!(
            "range starts at {:.3} s, at or after the end of the input ({:.3} s)",
            a / sr as f64,
            total as f64 / sr as f64
        ))
        .with_hint("ferraus info <input> shows the duration"));
    }
    let clamped = b.round() > total as f64 || a < -0.5;
    let s = a.round().clamp(0.0, total as f64) as usize;
    let e = b.round().clamp(0.0, total as f64) as usize;
    Ok((s, e.max(s), clamped))
}

/// Duration parameter in frames (accepts beats via `--bpm`; rejects end-relative times).
pub fn duration_frames(a: &Args, name: &str, sr: u32, bpm: Option<f64>) -> Result<usize> {
    let t = a.time(name)?;
    match t {
        Time::End | Time::FromEnd(_) => Err(Error::usage(format!(
            "--{name} must be a duration such as 500ms or 2s, not an end-relative time"
        ))),
        other => other
            .duration_frames(sr, bpm)
            .map_err(|e| Error::usage(format!("--{name}: {}", e.message))),
    }
}

/// Parse one duration string to frames.
pub fn parse_duration_frames(s: &str, what: &str, sr: u32, bpm: Option<f64>) -> Result<usize> {
    let t = Time::parse(s).map_err(|e| Error::usage(format!("{what}: {}", e.message)))?;
    match t {
        Time::End | Time::FromEnd(_) => Err(Error::usage(format!(
            "{what}: '{s}' must be a duration such as 500ms or 2s"
        ))),
        other => other
            .duration_frames(sr, bpm)
            .map_err(|e| Error::usage(format!("{what}: {}", e.message))),
    }
}

/// Move a cut point to the nearest zero crossing within `window` frames.
///
/// The crossing is located on the loudest channel in the window; among the two samples
/// either side of the crossing, the one with the smaller worst-case magnitude across all
/// channels is chosen. When no crossing exists (silence, DC), the quietest frame in the
/// window is used.
pub fn snap_to_zero(buf: &AudioBuffer, idx: usize, window: usize) -> usize {
    let n = buf.frames();
    if n == 0 || window == 0 {
        return idx.min(n);
    }
    let idx = idx.min(n);
    let lo = idx.saturating_sub(window);
    let hi = (idx + window).min(n);
    let cost = |i: usize| -> f32 {
        if i >= n {
            0.0 // the end of the buffer is a perfect cut point
        } else {
            buf.data.iter().fold(0.0f32, |m, c| m.max(c[i].abs()))
        }
    };
    // reference channel: most energy in the window
    let reference = buf
        .data
        .iter()
        .max_by(|a, b| {
            let ea: f32 = a[lo..hi].iter().map(|v| v * v).sum();
            let eb: f32 = b[lo..hi].iter().map(|v| v * v).sum();
            ea.partial_cmp(&eb).unwrap_or(std::cmp::Ordering::Equal)
        })
        .unwrap();
    if cost(idx) == 0.0 {
        return idx;
    }
    let mut best: Option<(usize, usize)> = None; // (distance, index)
    for i in lo.max(1)..hi {
        let (p, q) = (reference[i - 1], reference[i]);
        if (p <= 0.0 && q >= 0.0) || (p >= 0.0 && q <= 0.0) {
            let cand = if cost(i - 1) < cost(i) { i - 1 } else { i };
            let d = cand.abs_diff(idx);
            if best.map(|(bd, _)| d < bd).unwrap_or(true) {
                best = Some((d, cand));
            }
        }
    }
    if let Some((_, i)) = best {
        // only accept the crossing when it really is quiet on every channel
        let quietest = (lo..hi)
            .min_by(|a, b| cost(*a).partial_cmp(&cost(*b)).unwrap())
            .unwrap_or(idx);
        if cost(i) <= cost(quietest) * 4.0 + 1e-6 {
            return i;
        }
        return quietest;
    }
    (lo..hi)
        .min_by(|a, b| {
            cost(*a)
                .partial_cmp(&cost(*b))
                .unwrap()
                .then(a.abs_diff(idx).cmp(&b.abs_diff(idx)))
        })
        .unwrap_or(idx)
}

/// Crossfade curve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Curve {
    EqualPower,
    Linear,
}

impl Curve {
    pub fn parse(s: &str) -> Curve {
        match s {
            "linear" => Curve::Linear,
            _ => Curve::EqualPower,
        }
    }
    /// (fade-out gain, fade-in gain) at position `t` in `[0, 1]`.
    pub fn gains(self, t: f64) -> (f32, f32) {
        match self {
            Curve::EqualPower => ((t * FRAC_PI_2).cos() as f32, (t * FRAC_PI_2).sin() as f32),
            Curve::Linear => ((1.0 - t) as f32, t as f32),
        }
    }
}

/// Apply a raised-cosine fade-in over the first `len` frames.
pub fn fade_in(buf: &mut AudioBuffer, len: usize) {
    let len = len.min(buf.frames());
    for c in &mut buf.data {
        for (i, s) in c.iter_mut().take(len).enumerate() {
            let t = (i as f64 + 0.5) / len as f64;
            *s *= (0.5 - 0.5 * (std::f64::consts::PI * t).cos()) as f32;
        }
    }
}

/// Apply a raised-cosine fade-out over the last `len` frames.
pub fn fade_out(buf: &mut AudioBuffer, len: usize) {
    let n = buf.frames();
    let len = len.min(n);
    for c in &mut buf.data {
        for i in 0..len {
            let t = (i as f64 + 0.5) / len as f64;
            c[n - 1 - i] *= (0.5 - 0.5 * (std::f64::consts::PI * t).cos()) as f32;
        }
    }
}

/// Largest absolute sample at frame `i` across channels.
pub fn frame_mag(buf: &AudioBuffer, i: usize) -> f32 {
    buf.data
        .iter()
        .fold(0.0f32, |m, c| m.max(c.get(i).copied().unwrap_or(0.0).abs()))
}

/// Default extension for an audio output: `--format`, else the input's extension when this
/// build can encode it, else `wav` (mirrors the executor's choice for transforms).
pub fn audio_ext(ctx: &OpCtx) -> String {
    if let Some(f) = &ctx.globals.format {
        return f.clone();
    }
    let ext = ctx
        .inputs
        .first()
        .and_then(|p| p.extension())
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_else(|| "wav".into());
    if ctx.io.can_encode(&ext) {
        ext
    } else {
        "wav".into()
    }
}

/// Apply `--sample-rate`/`--channels` global overrides before writing from a custom op.
pub fn conform(buf: AudioBuffer, ctx: &OpCtx) -> AudioBuffer {
    ferraus_core::exec::conform_output(buf, &ctx.globals)
}

/// Remove ranges `[s, e)` (sorted, non-overlapping, within the buffer) and join the remaining
/// pieces with centred crossfades of up to `xfade` frames. Ranges touching the start or end
/// of the buffer are simply dropped. The output length is exactly the input length minus the
/// removed frames. Returns the buffer and the crossfade length actually used at each join.
pub fn remove_ranges(
    buf: &AudioBuffer,
    ranges: &[(usize, usize)],
    xfade: usize,
    curve: Curve,
) -> (AudioBuffer, Vec<usize>) {
    let n = buf.frames();
    // kept segments
    let mut kept: Vec<(usize, usize)> = Vec::new();
    let mut pos = 0;
    for &(s, e) in ranges {
        if s > pos {
            kept.push((pos, s));
        }
        pos = pos.max(e);
    }
    if pos < n {
        kept.push((pos, n));
    }
    let total: usize = kept.iter().map(|(s, e)| e - s).sum();
    let mut out: Vec<Vec<f32>> = vec![Vec::with_capacity(total); buf.channels()];
    if kept.is_empty() {
        return (AudioBuffer::from_planar(buf.sample_rate, out), Vec::new());
    }
    // half crossfade length per join
    let half_target = xfade / 2;
    let mut halves = Vec::with_capacity(kept.len().saturating_sub(1));
    for w in kept.windows(2) {
        let (s0, e0) = w[0];
        let (s1, e1) = w[1];
        let h = half_target
            .min((e0 - s0) / 2)
            .min((e1 - s1) / 2)
            .min(n - e0)
            .min(s1);
        halves.push(h);
    }
    for (i, &(s, e)) in kept.iter().enumerate() {
        let h_prev = if i > 0 { halves[i - 1] } else { 0 };
        let h_next = halves.get(i).copied().unwrap_or(0);
        for (c, o) in out.iter_mut().enumerate() {
            o.extend_from_slice(&buf.data[c][s + h_prev..e - h_next]);
        }
        if h_next > 0 {
            let (s1, _) = kept[i + 1];
            let len = 2 * h_next;
            for (c, o) in out.iter_mut().enumerate() {
                let src = &buf.data[c];
                for k in 0..len {
                    let t = (k as f64 + 0.5) / len as f64;
                    let (go, gi) = curve.gains(t);
                    o.push(src[e - h_next + k] * go + src[s1 - h_next + k] * gi);
                }
            }
        }
    }
    (
        AudioBuffer::from_planar(buf.sample_rate, out),
        halves.iter().map(|h| h * 2).collect(),
    )
}

/// Sort and merge overlapping or touching ranges.
pub fn merge_ranges(mut r: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    r.retain(|(s, e)| e > s);
    r.sort();
    let mut out: Vec<(usize, usize)> = Vec::new();
    for (s, e) in r {
        if let Some(last) = out.last_mut() {
            if s <= last.1 {
                last.1 = last.1.max(e);
                continue;
            }
        }
        out.push((s, e));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferraus_core::testing::sine;

    #[test]
    fn snap_finds_crossing() {
        let b = sine(48000, 1000.0, 0.1, 0.8);
        // a peak of the sine is at 12 samples (quarter period of 48)
        let i = snap_to_zero(&b, 12, 240);
        assert!(b.data[0][i].abs() < 0.06, "value {}", b.data[0][i]);
        assert!(i.abs_diff(12) <= 24);
    }

    #[test]
    fn remove_ranges_length() {
        let b = sine(48000, 440.0, 1.0, 0.5);
        let (o, x) = remove_ranges(&b, &[(1000, 5000), (20000, 30000)], 480, Curve::EqualPower);
        assert_eq!(o.frames(), 48000 - 4000 - 10000);
        assert_eq!(x, vec![480, 480]);
        let (o, x) = remove_ranges(&b, &[(0, 5000)], 480, Curve::EqualPower);
        assert_eq!(o.frames(), 43000);
        assert!(x.is_empty());
        assert_eq!(o.data[0][0], b.data[0][5000]);
    }

    #[test]
    fn merge() {
        assert_eq!(
            merge_ranges(vec![(5, 10), (0, 3), (8, 12), (3, 4), (7, 7)]),
            vec![(0, 4), (5, 12)]
        );
    }

    #[test]
    fn range_errors() {
        let r = TimeRange::parse("2s..1s").unwrap();
        let e = resolve_range(&r, 1000, 5000, None).unwrap_err();
        assert_eq!(e.exit_code, 2);
        let r = TimeRange::parse("6s..7s").unwrap();
        assert_eq!(resolve_range(&r, 1000, 5000, None).unwrap_err().exit_code, 2);
        let r = TimeRange::parse("1s..9s").unwrap();
        assert_eq!(resolve_range(&r, 1000, 5000, None).unwrap(), (1000, 5000, true));
        let r = TimeRange::parse("-2s..end").unwrap();
        assert_eq!(resolve_range(&r, 1000, 5000, None).unwrap(), (3000, 5000, false));
    }
}
