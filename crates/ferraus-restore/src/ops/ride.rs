//! `ride`: an offline vocal rider. Turns a vocal up and down word by word (or phrase by
//! phrase) so its loudness keeps a steady offset to the music it sits in, and returns the
//! gain curve as `envelope` keyframes.

use crate::dsp::biquad::{butterworth, Sos, SosFilter};
use crate::dsp::loudness::{channel_weight, integrated, k_weighting};
use crate::dsp::util::{parse_keyframes, percentile, resolve_keyframes, Curve};
use ferraus_core::prelude::*;
use ferraus_core::resample::{resample, Quality};

/// Detector step: one gain value (and one possible keyframe) every 10 ms.
const HOP_S: f64 = 0.01;
/// Half-width, in steps, of the short-term detector that decides whether the vocal is
/// sounding at a step (5 steps = 50 ms).
const ACTIVE_HALF: usize = 2;
/// Pauses longer than this glide from one phrase's gain to the next; shorter ones hold.
const GLIDE_S: f64 = 2.0;
/// BS.1770 absolute gate: anything quieter is silence.
const ABS_GATE_LUFS: f64 = -70.0;
/// Music more than this far below its loudest window counts as silent (no reference).
const MUSIC_SILENT_DB: f64 = 40.0;
/// The music may be this much shorter than the vocal before `against_length` warns.
const AGAINST_SHORT_S: f64 = 0.05;
/// `range_limited` warns when more of the singing than this hits the range.
const RANGE_WARN_SHARE: f64 = 0.2;
/// `music_silent` warns when the music is silent under more of the singing than this.
const MUSIC_SILENT_WARN_SHARE: f64 = 0.1;
/// Keyframe simplification tolerance; with the values rounded to 0.1 dB the keyframes stay
/// within 0.25 dB of the smoothed gain curve.
const POINT_TOL_DB: f64 = 0.2;

pub fn ops() -> Vec<OpSpec> {
    vec![OpSpec::transform(
        "ride",
        Category::Dynamics,
        "Vocal rider: hold a vocal's level against the music, word by word",
        ride,
    )
    .about(
        "Rides the vocal's fader the way a mix engineer draws volume automation before the \
         compressor: each word or phrase is turned up or down so the vocal keeps a steady \
         level against the music. A compressor reacts to the vocal alone; the rider compares \
         the vocal with the music (--against: the instrumental, or every other stem summed, \
         starting with the vocal). Both are measured as K-weighted loudness (BS.1770, \
         channels summed) over --window, centred on every 10 ms step and counting only the \
         moments the vocal sounds; the file is processed offline, so the ride meets each word \
         with no lag. While the vocal sings (its 50 ms level within --floor of its loudest \
         window) the gain is (target - (vocal - music)) x --amount, limited to +/- --range. \
         Through breaths, bleed and pauses the gain holds, then moves to the next phrase's \
         value just before it starts (pauses longer than 2 s glide from one value to the \
         next); where the music is silent (40 dB under its loudest window) the gain holds \
         too. --smooth turns every change into a centred ramp. The automation comes back as \
         envelope keyframes in result.points: 'ferraus envelope <vocal> --points <points>' \
         reproduces the output exactly, so the ride can be kept and edited in a pipeline. \
         --target auto holds the take's typical offset (the mean of the middle half of the \
         singing windows): loud and quiet words move toward it by equal amounts, so the \
         vocal's typical level stays where the fader put it (its integrated loudness usually \
         drops a fraction of a LU as loud words come down: result.loudness_change_lu). \
         result.offset_before and offset_after are the vocal-minus-music offsets over the \
         singing, measured with the same detector. The ride evens out changes slower than \
         about the window; faster ones inside a word are the compressor's job, so ride before \
         compressing. Without --against the vocal rides toward its own typical level (a fast \
         leveler). Same length as the input, no delay.",
    )
    .param(param("against", ParamKind::Path).help(
        "The music the vocal sits in (the instrumental, or every other stem summed), starting at the \
         same time as the vocal; resampled to the vocal's rate. Without it the vocal rides toward its own \
         typical level",
    ))
    .param(param("target", ParamKind::Str).default("auto").help(
        "Vocal-minus-music loudness offset to hold while singing, in LU (-2LU, -2dB, or a bare number; \
         negative sits the vocal under the music): auto holds the take's typical offset, so the overall \
         balance stays where the fader put it. Without --against: the level relative to the vocal's own \
         typical level (auto = 0)",
    ))
    .param(
        param("range", ParamKind::Gain)
            .default("4dB")
            .help("Largest boost or cut, the same both ways (4dB rides between -4 and +4 dB)"),
    )
    .param(
        param("amount", ParamKind::Percent)
            .default("100%")
            .help("Share of each correction applied: 100% holds the target, 50% makes every move half as big"),
    )
    .param(
        param("window", ParamKind::Time)
            .default("400ms")
            .help("Detector length, centred on each moment: 400ms follows words, 1.5s phrases, 4s sections"),
    )
    .param(
        param("smooth", ParamKind::Time).default("120ms").help(
            "How fast the gain moves: every change becomes a centred ramp this long (0s = a new value every 10 ms)",
        ),
    )
    .param(param("floor", ParamKind::Gain).default("-30dB").help(
        "Singing detector: the vocal counts as singing where its 50 ms level is within this many dB of its \
         loudest window; elsewhere (breaths, bleed, pauses) the gain holds. Raise to -20dB to leave louder \
         breaths alone",
    ))
    .param(param("band", ParamKind::Str).advanced().help(
        "Band-limit both detectors to low..high (e.g. 200Hz..5kHz) so only that range drives the ride; \
         --target then refers to the levels in that band, not full-band loudness",
    ))
    .example(
        "ferraus ride vocal.wav --against instrumental.wav -o vocal.ride.wav",
        "Even the vocal out word by word against the instrumental, keeping its overall balance",
    )
    .example(
        "ferraus ride vocal.wav --against music.wav --target -2LU --range 6dB -o vocal.ride.wav",
        "Hold the vocal 2 LU under the music while singing, with up to 6 dB of boost or cut",
    )
    .example(
        "ferraus ride vocal.wav --against music.wav --window 1.5s --smooth 400ms --amount 70% -o vocal.ride.wav",
        "A gentler ride that follows phrases instead of words",
    )
    .example(
        "ferraus ride narration.wav --window 300ms -o narration.ride.wav",
        "No music: even a voice out word by word around its own typical level",
    )]
}

/// Parameters after parsing and validation.
struct Settings {
    target: Option<f64>,
    range: f64,
    amount: f64,
    window_s: f64,
    smooth_s: f64,
    floor: f64,
    band: Band,
}

impl Settings {
    fn from_args(a: &Args, sr: f64) -> Result<Settings> {
        let amount = a.percent("amount")?;
        if !(0.0..=1.0).contains(&amount) {
            return Err(Error::usage(format!(
                "--amount must be between 0% and 100%, got {}%",
                (amount * 100.0).round()
            )));
        }
        let window_s = a.seconds("window")?;
        if window_s <= 0.0 {
            return Err(Error::usage("--window must be longer than 0 s (400ms follows words)"));
        }
        let smooth_s = a.seconds("smooth")?;
        if smooth_s < 0.0 {
            return Err(Error::usage("--smooth must not be negative"));
        }
        Ok(Settings {
            target: parse_target(&a.str_or("target", "auto"))?,
            range: a.db("range")?.abs(),
            amount,
            window_s,
            smooth_s,
            floor: -a.db("floor")?.abs(),
            band: Band::parse(a.str("band").as_deref(), sr)?,
        })
    }
}

/// `auto` is `None`; `-2LU`, `-2dB`, `+1`, and `-2` are offsets in LU.
fn parse_target(s: &str) -> Result<Option<f64>> {
    let t = s.trim().to_ascii_lowercase();
    if t == "auto" {
        return Ok(None);
    }
    let num = t.strip_suffix("lu").or_else(|| t.strip_suffix("db")).unwrap_or(&t).trim();
    num.parse::<f64>().ok().filter(|v| v.is_finite()).map(Some).ok_or_else(|| {
        Error::usage(format!(
            "--target '{s}' should be auto or a vocal-minus-music offset in LU, e.g. -2LU, -2dB, or +1"
        ))
    })
}

/// Optional band limits for the detectors.
#[derive(Default)]
struct Band {
    hp: Option<Sos>,
    lp: Option<Sos>,
}

impl Band {
    fn parse(band: Option<&str>, sr: f64) -> Result<Band> {
        let Some(b) = band else {
            return Ok(Band::default());
        };
        let (lo, hi) =
            units::parse_num_range(b, units::parse_freq).map_err(|e| Error::usage(format!("--band: {}", e.message)))?;
        let (lo, hi) = (lo.min(hi), lo.max(hi));
        if hi <= lo {
            return Err(Error::usage(format!("--band '{b}' is empty; give low..high, e.g. 200Hz..5kHz")));
        }
        let hp = if lo > 25.0 { Some(butterworth(2, lo, sr, true)) } else { None };
        let lp = if hi < sr * 0.45 && hi < 19_000.0 { Some(butterworth(2, hi, sr, false)) } else { None };
        Ok(Band { hp, lp })
    }

    fn filters(&self) -> (Option<SosFilter>, Option<SosFilter>) {
        (self.hp.as_ref().map(Sos::filter), self.lp.as_ref().map(Sos::filter))
    }
}

/// The 10 ms step grid: step `k` sits at frame `k * hop` and owns the block of frames
/// `[k*hop - hop/2, k*hop + hop/2)` (the last block runs to the end).
struct Grid {
    hop: usize,
    steps: usize,
    /// Frames in each block.
    len: Vec<f64>,
}

impl Grid {
    fn new(frames: usize, sr: u32) -> Grid {
        let hop = ((HOP_S * sr as f64).round() as usize).max(1);
        let steps = if frames == 0 { 0 } else { (frames - 1) / hop + 1 };
        let mut len = vec![0.0; steps];
        for i in 0..frames {
            len[Grid::block_of(hop, steps, i)] += 1.0;
        }
        Grid { hop, steps, len }
    }

    #[inline]
    fn block_of(hop: usize, steps: usize, frame: usize) -> usize {
        ((frame + hop / 2) / hop).min(steps - 1)
    }

    /// Sum of K-weighted, channel-weighted squares in every block (BS.1770 channel sum).
    fn power(&self, buf: &AudioBuffer, band: &Band) -> Vec<f64> {
        let mut sums = vec![0.0f64; self.steps];
        let kw = k_weighting(buf.sample_rate as f64);
        for (c, ch) in buf.data.iter().enumerate() {
            let w = channel_weight(c);
            if w == 0.0 {
                continue;
            }
            let mut f = kw.filter();
            let (mut hp, mut lp) = band.filters();
            for (i, x) in ch.iter().enumerate() {
                let mut y = f.tick(*x as f64);
                if let Some(h) = hp.as_mut() {
                    y = h.tick(y);
                }
                if let Some(l) = lp.as_mut() {
                    y = l.tick(y);
                }
                sums[Grid::block_of(self.hop, self.steps, i)] += w * y * y;
            }
        }
        sums
    }

    /// Steps `[k - half, k + half]`, clipped to the grid.
    fn span(&self, k: usize, half: usize) -> (usize, usize) {
        (k.saturating_sub(half), (k + half).min(self.steps - 1))
    }
}

/// Prefix sums for windowed means.
struct Prefix(Vec<f64>);

impl Prefix {
    fn new(v: impl Iterator<Item = f64>) -> Prefix {
        let mut s = vec![0.0];
        let mut acc = 0.0;
        for x in v {
            acc += x;
            s.push(acc);
        }
        Prefix(s)
    }
    /// Sum of entries `lo..=hi`.
    fn sum(&self, (lo, hi): (usize, usize)) -> f64 {
        self.0[hi + 1] - self.0[lo]
    }
}

#[inline]
fn lufs(mean_square: f64) -> f64 {
    -0.691 + 10.0 * mean_square.max(1e-20).log10()
}

/// The vocal's level against the music (or its own typical level) at every step.
struct Analysis {
    /// The vocal is sounding at this step.
    active: Vec<bool>,
    /// Active, and the music is not silent: the gain follows the detector here.
    usable: Vec<bool>,
    /// Reference level per step: the music, or the vocal's typical level without music.
    reference: Vec<f64>,
    /// Vocal minus reference at usable steps (NaN elsewhere).
    offset: Vec<f64>,
    /// Long-window half-width in steps.
    half: usize,
    /// Share of active steps where the music is silent.
    music_silent_share: f64,
}

/// Level of the vocal over `half` steps around every step, counting only active blocks.
fn gated_levels(grid: &Grid, vocal: &[f64], active: &[bool], half: usize) -> Vec<f64> {
    let pv = Prefix::new(vocal.iter().zip(active).map(|(v, a)| if *a { *v } else { 0.0 }));
    let pl = Prefix::new(grid.len.iter().zip(active).map(|(l, a)| if *a { *l } else { 0.0 }));
    (0..grid.steps)
        .map(|k| {
            let s = grid.span(k, half);
            let l = pl.sum(s);
            if l > 0.0 {
                lufs(pv.sum(s) / l)
            } else {
                f64::NEG_INFINITY
            }
        })
        .collect()
}

fn analyze(grid: &Grid, vocal: &[f64], music: Option<&[f64]>, s: &Settings, sr: u32) -> Analysis {
    let steps = grid.steps;
    let half = ((s.window_s * sr as f64 / grid.hop as f64) / 2.0).round() as usize;
    let pl = Prefix::new(grid.len.iter().copied());
    let pv = Prefix::new(vocal.iter().copied());
    let level = |p: &Prefix, k: usize, h: usize| {
        let sp = grid.span(k, h);
        lufs(p.sum(sp) / pl.sum(sp).max(1.0))
    };
    let vmax = (0..steps).map(|k| level(&pv, k, half)).fold(f64::NEG_INFINITY, f64::max);
    let active: Vec<bool> = (0..steps)
        .map(|k| {
            let l = level(&pv, k, ACTIVE_HALF);
            vmax > ABS_GATE_LUFS && l > ABS_GATE_LUFS && l >= vmax + s.floor
        })
        .collect();
    let gv = gated_levels(grid, vocal, &active, half);
    let (usable, reference, music_silent_share) = match music {
        Some(m) => {
            let pm = Prefix::new(m.iter().copied());
            let mmax = (0..steps).map(|k| level(&pm, k, half)).fold(f64::NEG_INFINITY, f64::max);
            let silent = (mmax - MUSIC_SILENT_DB).max(ABS_GATE_LUFS);
            let gm = gated_levels(grid, m, &active, half);
            let usable: Vec<bool> = (0..steps).map(|k| active[k] && gm[k] > silent).collect();
            let n_active = active.iter().filter(|a| **a).count();
            let n_silent = (0..steps).filter(|k| active[*k] && !usable[*k]).count();
            (usable, gm, n_silent as f64 / n_active.max(1) as f64)
        }
        None => {
            let levels: Vec<f64> = (0..steps).filter(|k| active[*k]).map(|k| gv[k]).collect();
            let typical = interquartile_mean(&levels).unwrap_or(0.0);
            (active.clone(), vec![typical; steps], 0.0)
        }
    };
    let offset = (0..steps).map(|k| if usable[k] { gv[k] - reference[k] } else { f64::NAN }).collect();
    Analysis { active, usable, reference, offset, half, music_silent_share }
}

/// Mean of the middle half of the values (robust like a median, but it moves smoothly when
/// the values split into two groups, such as verse and chorus).
fn interquartile_mean(v: &[f64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = s.len();
    let (lo, hi) = if n < 4 { (0, n) } else { (n / 4, n - n / 4) };
    Some(s[lo..hi].iter().sum::<f64>() / (hi - lo) as f64)
}

/// Gain through steps where the detector does not apply (breaths, bleed, pauses, silent
/// music): hold the previous phrase's gain, then step to the next phrase's gain `lead` steps
/// before it starts, so a centred smoothing ramp ends as the phrase begins (in short pauses
/// the step falls in the middle). Pauses longer than `glide` steps glide linearly from one
/// gain to the next. Before the first and after the last phrase the nearest gain holds.
fn fill_gaps(g: &mut [f64], usable: &[bool], lead: usize, glide: usize) {
    let idx: Vec<usize> = (0..g.len()).filter(|k| usable[*k]).collect();
    let (Some(&first), Some(&last)) = (idx.first(), idx.last()) else {
        return;
    };
    let (gf, gl) = (g[first], g[last]);
    g[..first].fill(gf);
    g[last + 1..].fill(gl);
    for w in idx.windows(2) {
        let (a, b) = (w[0], w[1]);
        if b == a + 1 {
            continue;
        }
        let (ga, gb) = (g[a], g[b]);
        if b - a > glide {
            for k in a + 1..b {
                g[k] = ga + (gb - ga) * (k - a) as f64 / (b - a) as f64;
            }
        } else {
            let step = b.saturating_sub(lead).max(a + 1 + (b - a - 1) / 2);
            for k in a + 1..b {
                g[k] = if k < step { ga } else { gb };
            }
        }
    }
}

/// Centred moving average over `2 * half + 1` steps, holding the end values beyond the edges.
fn smooth(g: &[f64], half: usize) -> Vec<f64> {
    let n = g.len();
    if half == 0 || n < 2 {
        return g.to_vec();
    }
    let at = |i: isize| g[i.clamp(0, n as isize - 1) as usize];
    let h = half as isize;
    let w = (2 * half + 1) as f64;
    let mut sum: f64 = (-h..=h).map(at).sum();
    let mut out = Vec::with_capacity(n);
    for k in 0..n as isize {
        out.push(sum / w);
        sum += at(k + h + 1) - at(k - h);
    }
    out
}

/// Indices kept by Ramer-Douglas-Peucker with a vertical tolerance: straight lines through
/// the kept points stay within `tol` of every value. Both ends of every flat run (a held
/// gain) are kept, so holds stay exactly flat.
fn simplify(y: &[f64], tol: f64) -> Vec<usize> {
    let n = y.len();
    if n <= 2 {
        return (0..n).collect();
    }
    let mut keep = vec![false; n];
    keep[0] = true;
    keep[n - 1] = true;
    let flat = |i: usize| (y[i + 1] - y[i]).abs() < 1e-9;
    let mut i = 0;
    while i + 1 < n {
        if flat(i) {
            let start = i;
            while i + 1 < n && flat(i) {
                i += 1;
            }
            if i - start >= 2 {
                keep[start] = true;
                keep[i] = true;
            }
        } else {
            i += 1;
        }
    }
    let anchors: Vec<usize> = (0..n).filter(|i| keep[*i]).collect();
    let mut stack: Vec<(usize, usize)> = anchors.windows(2).map(|w| (w[0], w[1])).collect();
    while let Some((a, b)) = stack.pop() {
        if b <= a + 1 {
            continue;
        }
        let (ya, yb) = (y[a], y[b]);
        let mut worst = (0.0f64, a);
        for i in a + 1..b {
            let line = ya + (yb - ya) * (i - a) as f64 / (b - a) as f64;
            let d = (y[i] - line).abs();
            if d > worst.0 {
                worst = (d, i);
            }
        }
        if worst.0 > tol {
            keep[worst.1] = true;
            stack.push((a, worst.1));
            stack.push((worst.1, b));
        }
    }
    (0..n).filter(|i| keep[*i]).collect()
}

fn fmt_time(t: f64) -> String {
    let s = format!("{t:.3}");
    format!("{}s", s.trim_end_matches('0').trim_end_matches('.'))
}

fn fmt_db(v: f64) -> String {
    let r = (v * 10.0).round() / 10.0;
    if r == 0.0 {
        "0dB".to_string()
    } else {
        format!("{r:+.1}dB")
    }
}

/// Rounded to 0.01 (never -0).
fn r2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0 + 0.0
}

/// Rounded to 0.001 (never -0).
fn r3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0 + 0.0
}

/// Median, 10th and 90th percentile, and their spread.
fn offset_stats(v: &[f64]) -> Value {
    if v.is_empty() {
        return Value::Null;
    }
    let (p10, p90) = (percentile(v, 0.1), percentile(v, 0.9));
    json!({
        "median_lu": r2(percentile(v, 0.5)),
        "p10_lu": r2(p10),
        "p90_lu": r2(p90),
        "spread_lu": r2(p90 - p10),
    })
}

/// The `--against` music: decoded, resampled to the vocal's rate, and padded with silence
/// or truncated to the vocal's frame count.
fn against_audio(a: &Args, input: &AudioBuffer, ctx: &mut OpCtx) -> Result<Option<(String, AudioBuffer)>> {
    let Some(path) = a.path("against") else {
        return Ok(None);
    };
    let mut music = ctx.read_audio_path(&path)?;
    if music.sample_rate != input.sample_rate {
        music = resample(&music, input.sample_rate, Quality::High);
    }
    let short_s = (input.frames() as f64 - music.frames() as f64) / input.sample_rate.max(1) as f64;
    if short_s > AGAINST_SHORT_S {
        ctx.warn(
            "against_length",
            format!(
                "the music {} is {short_s:.2} s shorter than the vocal; the gain holds over the last {short_s:.2} s",
                path.display()
            ),
        );
    }
    music.resize(input.frames());
    let shown = path.display().to_string();
    ctx.note("against", &shown);
    Ok(Some((shown, music)))
}

/// Apply `points` exactly as `ferraus envelope --points` does.
fn apply_points(buf: &mut AudioBuffer, points: &str) -> Result<Curve> {
    let kf = parse_keyframes(&units::split_list(points), units::parse_db)?;
    let curve = Curve::new(resolve_keyframes(&kf, buf.sample_rate, buf.frames(), None)?, false);
    let gains: Vec<f32> = (0..buf.frames()).map(|i| db_to_lin(curve.eval(i as f64)) as f32).collect();
    for ch in buf.data.iter_mut() {
        for (v, g) in ch.iter_mut().zip(&gains) {
            *v *= *g;
        }
    }
    Ok(curve)
}

/// Result for an input with nothing to ride: the output is the input.
fn unchanged(ctx: &mut OpCtx, against: Option<String>, singing_share: f64, s: &Settings) {
    ctx.metric("spread_before_lu", Value::Null);
    ctx.metric("spread_after_lu", Value::Null);
    ctx.metric("gain_min_db", 0.0);
    ctx.metric("gain_max_db", 0.0);
    ctx.set_result(json!({
        "against": against,
        "target_lu": s.target.map(r2),
        "target_source": if s.target.is_some() { "given" } else { "auto" },
        "offset_before": Value::Null,
        "offset_after": Value::Null,
        "gain_db": {"min": 0.0, "max": 0.0, "mean_abs": 0.0},
        "singing_share": r3(singing_share),
        "at_range_share": 0.0,
        "loudness_change_lu": if singing_share > 0.0 { Some(0.0) } else { None },
        "points": "0s=0dB",
        "points_count": 1,
    }));
}

fn ride(a: &Args, mut buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let sr = buf.sample_rate;
    let s = Settings::from_args(a, sr as f64)?;
    let against = against_audio(a, &buf, ctx)?;
    if against.is_none() {
        ctx.warn(
            "no_against",
            "no --against music: the vocal rides toward its own typical level (a fast leveler); pass the \
             instrumental (or the other stems summed) with --against to hold the vocal against the music",
        );
    }
    let against_path = against.as_ref().map(|(p, _)| p.clone());
    let n = buf.frames();
    if n == 0 {
        ctx.warn("silent_input", "the vocal is empty: nothing to ride");
        unchanged(ctx, against_path, 0.0, &s);
        return Ok(buf);
    }
    let grid = Grid::new(n, sr);
    let vocal = grid.power(&buf, &s.band);
    let music = against.as_ref().map(|(_, m)| grid.power(m, &s.band));
    ctx.check_cancel()?;
    let an = analyze(&grid, &vocal, music.as_deref(), &s, sr);
    let steps = grid.steps;
    let n_active = an.active.iter().filter(|x| **x).count();
    let singing_share = n_active as f64 / steps as f64;
    if n_active == 0 {
        ctx.warn("silent_input", "no singing found: the vocal never rises above -70 LUFS; the output is unchanged");
        unchanged(ctx, against_path, 0.0, &s);
        return Ok(buf);
    }
    if an.music_silent_share > MUSIC_SILENT_WARN_SHARE {
        ctx.warn(
            "music_silent",
            format!(
                "the music is silent under {:.0}% of the singing (an a cappella part, or the wrong file?); the gain \
                 holds there",
                an.music_silent_share * 100.0
            ),
        );
    }
    let usable: Vec<usize> = (0..steps).filter(|k| an.usable[*k]).collect();
    if usable.is_empty() {
        unchanged(ctx, against_path, singing_share, &s);
        return Ok(buf);
    }
    let before: Vec<f64> = usable.iter().map(|k| an.offset[*k]).collect();
    let target = match s.target {
        Some(t) => t,
        None if music.is_some() => interquartile_mean(&before).unwrap_or(0.0),
        None => 0.0,
    };
    ctx.note("target_resolved", format!("{target:+.2}LU"));

    // the correction while singing, limited to the range
    let mut g = vec![0.0f64; steps];
    let mut hits = 0usize;
    for &k in &usable {
        let c = (target - an.offset[k]) * s.amount;
        if c.abs() > s.range {
            hits += 1;
        }
        g[k] = c.clamp(-s.range, s.range);
    }
    let at_range_share = hits as f64 / usable.len() as f64;
    let hop_s = grid.hop as f64 / sr as f64;
    let smooth_half = (s.smooth_s / hop_s / 2.0).round() as usize;
    fill_gaps(&mut g, &an.usable, smooth_half, (GLIDE_S / hop_s).round() as usize);
    let g = smooth(&g, smooth_half);

    // keyframes, then the gain exactly as `envelope` applies them
    let keep = simplify(&g, POINT_TOL_DB);
    let points: Vec<String> =
        keep.iter().map(|k| format!("{}={}", fmt_time((k * grid.hop) as f64 / sr as f64), fmt_db(g[*k]))).collect();
    let points = points.join(",");
    let loudness_before = integrated(&buf.data, sr);
    let curve = apply_points(&mut buf, &points)?;
    ctx.check_cancel()?;
    let loudness_change = match (loudness_before, integrated(&buf.data, sr)) {
        (Some(b), Some(a)) => Some(r2(a - b)),
        _ => None,
    };

    // measure what the ride did, with the same detector
    let out_power = grid.power(&buf, &s.band);
    let gv_after = gated_levels(&grid, &out_power, &an.active, an.half);
    let after: Vec<f64> = usable.iter().map(|k| gv_after[*k] - an.reference[*k]).collect();
    let applied: Vec<f64> = (0..steps).map(|k| curve.eval((k * grid.hop) as f64)).collect();
    let gmin = applied.iter().copied().fold(f64::INFINITY, f64::min);
    let gmax = applied.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let mean_abs = applied.iter().map(|v| v.abs()).sum::<f64>() / steps as f64;
    let (stats_before, stats_after) = (offset_stats(&before), offset_stats(&after));
    let spread = |v: &Value| v.get("spread_lu").cloned().unwrap_or(Value::Null);
    ctx.metric("spread_before_lu", spread(&stats_before));
    ctx.metric("spread_after_lu", spread(&stats_after));
    ctx.metric("gain_min_db", r2(gmin));
    ctx.metric("gain_max_db", r2(gmax));
    if at_range_share > RANGE_WARN_SHARE {
        ctx.warn(
            "range_limited",
            format!(
                "{:.0}% of the singing needs more than the {} dB range: the vocal is often more than {} dB from \
                 target; move the fader, or the arrangement fights the vocal (or allow more with --range)",
                at_range_share * 100.0,
                s.range,
                s.range
            ),
        );
    }
    ctx.set_result(json!({
        "against": against_path,
        "target_lu": r2(target),
        "target_source": if s.target.is_some() { "given" } else { "auto" },
        "offset_before": stats_before,
        "offset_after": stats_after,
        "gain_db": {"min": r2(gmin), "max": r2(gmax), "mean_abs": r2(mean_abs)},
        "singing_share": r3(singing_share),
        "at_range_share": r3(at_range_share),
        "loudness_change_lu": loudness_change,
        "points": points,
        "points_count": keep.len(),
    }));
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::loudness::integrated;
    use crate::testutil::Rng;
    use ferraus_core::testing::{self, run, run_ctx, write_wav, TempDir};
    use std::f64::consts::PI;

    const SR: u32 = 48000;

    fn spec() -> OpSpec {
        ops().into_iter().find(|o| o.name == "ride").unwrap()
    }

    fn envelope_spec() -> OpSpec {
        crate::ops::automation::ops().into_iter().find(|o| o.name == "envelope").unwrap()
    }

    /// Sung-like words: a harmonic tone (a different pitch per word) with 10 ms fades, one
    /// word per level, `lead_s` of silence first.
    fn words(levels_db: &[f64], word_s: f64, gap_s: f64, lead_s: f64) -> AudioBuffer {
        let per = word_s + gap_s;
        let total = lead_s + per * levels_db.len() as f64 + 0.5;
        let n = (total * SR as f64) as usize;
        let mut v = vec![0.0f32; n];
        let wn = (word_s * SR as f64) as usize;
        let fade = (0.01 * SR as f64) as usize;
        for (w, db) in levels_db.iter().enumerate() {
            let start = ((lead_s + per * w as f64) * SR as f64) as usize;
            let f0 = [196.0, 220.0, 247.0, 262.0, 294.0][w % 5];
            let amp = 0.1 * db_to_lin(*db);
            for i in 0..wn {
                let t = i as f64 / SR as f64;
                let env = (i.min(wn - 1 - i) as f64 / fade as f64).min(1.0);
                let mut s = 0.0;
                for h in 1..=6 {
                    s += (2.0 * PI * f0 * h as f64 * t).sin() / h as f64;
                }
                v[start + i] = (amp * env * s) as f32;
            }
        }
        AudioBuffer::from_mono(SR, v)
    }

    /// Steady pink-ish noise (white plus a low-passed copy) at roughly `amp` RMS.
    fn music(secs: f64, amp: f32, seed: u64) -> AudioBuffer {
        let w = testing::noise(SR, secs, 1.0, seed);
        let mut lp = 0.0f64;
        let v: Vec<f32> = w.data[0]
            .iter()
            .map(|x| {
                lp = 0.97 * lp + 0.03 * *x as f64;
                (amp as f64 * (0.5 * *x as f64 + 4.0 * lp)) as f32
            })
            .collect();
        AudioBuffer::from_mono(SR, v)
    }

    fn write(dir: &TempDir, name: &str, b: &AudioBuffer) -> String {
        let p = dir.path(name);
        write_wav(&p, b);
        p.display().to_string()
    }

    fn result(c: &OpCtx) -> Value {
        c.result.clone().expect("ride sets a result")
    }

    fn f(v: &Value, path: &str) -> f64 {
        path.split('.').fold(v, |v, k| &v[k]).as_f64().unwrap_or_else(|| panic!("{path} in {v}"))
    }

    fn has_warning(c: &OpCtx, code: &str) -> bool {
        c.warnings.iter().any(|w| w.code == code)
    }

    /// The applied gain curve (dB) from `points`, per frame.
    fn curve_of(points: &str, frames: usize) -> Curve {
        let kf = parse_keyframes(&units::split_list(points), units::parse_db).unwrap();
        Curve::new(resolve_keyframes(&kf, SR, frames, None).unwrap(), false)
    }

    fn uneven_levels(count: usize, seed: u64) -> Vec<f64> {
        let mut r = Rng::new(seed);
        (0..count).map(|_| r.range(-6.0, 6.0)).collect()
    }

    #[test]
    fn evens_uneven_words_against_steady_music() {
        let dir = TempDir::new("ride-even");
        let x = words(&uneven_levels(24, 7), 0.3, 0.1, 0.5);
        let m = write(&dir, "music.wav", &music(x.duration_s(), 0.05, 3));
        // words up to 6 dB either side of their typical level: a 6 dB range reaches them all
        let (y, c) = run_ctx(&spec(), &[("against", m.as_str()), ("range", "6dB")], x.clone()).unwrap();
        let r = result(&c);
        let (before, after) = (f(&r, "offset_before.spread_lu"), f(&r, "offset_after.spread_lu"));
        assert!(before > 6.0 && after < 1.5, "spread {before} -> {after} LU: {r}");
        assert!(f(&r, "gain_db.min") >= -6.0 && f(&r, "gain_db.max") <= 6.0, "{r}");
        assert_eq!(r["target_source"], "auto");
        assert_eq!(r["against"], json!(m));
        // auto target: the typical level stays put; the integrated loudness comes down a
        // little because loud words come down as far as quiet ones go up (reported)
        assert!((f(&r, "offset_after.median_lu") - f(&r, "target_lu")).abs() < 0.5, "{r}");
        assert!((f(&r, "target_lu") - f(&r, "offset_before.median_lu")).abs() < 0.5, "{r}");
        let shift = integrated(&y.data, SR).unwrap() - integrated(&x.data, SR).unwrap();
        assert!(shift.abs() < 1.5, "overall level moved {shift:.2} LU");
        assert!((f(&r, "loudness_change_lu") - shift).abs() < 0.01, "{r}");
        assert_eq!(y.frames(), x.frames());
        assert_eq!(c.metrics["spread_after_lu"], r["offset_after"]["spread_lu"]);
        assert_eq!(c.notes["against"], json!(m));
        assert!(c.warnings.is_empty(), "{:?}", c.warnings);
    }

    #[test]
    fn louder_chorus_lifts_the_vocal_and_the_verse_comes_down() {
        let dir = TempDir::new("ride-chorus");
        // a steady vocal of 40 words; the music is 4 dB louder in the second half
        let x = words(&[0.0; 40], 0.3, 0.1, 0.0);
        let n = x.frames();
        let mut bed = music(x.duration_s(), 0.05, 5);
        let half = 20.0 * 0.4;
        for (i, v) in bed.data[0].iter_mut().enumerate() {
            if i as f64 / SR as f64 >= half {
                *v *= db_to_lin(4.0) as f32;
            }
        }
        let m = write(&dir, "music.wav", &bed);
        let (_, c) = run_ctx(&spec(), &[("against", m.as_str())], x).unwrap();
        let r = result(&c);
        let curve = curve_of(r["points"].as_str().unwrap(), n);
        let at = |t: f64| curve.eval(t * SR as f64);
        for t in [1.0, 3.0, 5.0, 7.0] {
            assert!((at(t) + 2.0).abs() < 0.6, "verse at {t}s: {:.2} dB ({r})", at(t));
            assert!((at(t + half) - 2.0).abs() < 0.6, "chorus at {}s: {:.2} dB", t + half, at(t + half));
        }
        assert!(f(&r, "gain_db.min") >= -4.0 && f(&r, "gain_db.max") <= 4.0);
        assert!(f(&r, "offset_after.spread_lu") < 1.0, "{r}");
    }

    #[test]
    fn the_gain_holds_through_pauses_and_glides_through_long_ones() {
        let dir = TempDir::new("ride-gaps");
        // phrase A loud (0..2 s), a 1.1 s pause, phrase B quiet (3..5 s), a 4.1 s pause,
        // phrase C in between (9..11 s); low bleed under everything
        let mut levels = vec![3.0; 5];
        levels.extend([-3.0; 5]);
        levels.extend([0.0; 5]);
        let w = words(&levels, 0.3, 0.1, 0.0).data[0].clone();
        let cut = |s: f64| (s * SR as f64) as usize;
        let mut v = w[..cut(2.0)].to_vec();
        v.extend(vec![0.0f32; cut(1.0)]);
        v.extend_from_slice(&w[cut(2.0)..cut(4.0)]);
        v.extend(vec![0.0f32; cut(4.0)]);
        v.extend_from_slice(&w[cut(4.0)..]);
        let bleed = testing::noise(SR, v.len() as f64 / SR as f64, 0.0001, 9);
        for (s, b) in v.iter_mut().zip(&bleed.data[0]) {
            *s += *b;
        }
        let x = AudioBuffer::from_mono(SR, v);
        let m = write(&dir, "music.wav", &music(x.duration_s(), 0.05, 4));
        let (y, c) = run_ctx(&spec(), &[("against", m.as_str())], x.clone()).unwrap();
        let r = result(&c);
        let curve = curve_of(r["points"].as_str().unwrap(), x.frames());
        let at = |t: f64| curve.eval(t * SR as f64);
        // A is turned down, B up, C stays near 0
        assert!(
            at(1.0) < -2.0 && at(3.5) > 2.0 && at(10.0).abs() < 1.0,
            "A {:.2}, B {:.2}, C {:.2}",
            at(1.0),
            at(3.5),
            at(10.0)
        );
        // the 1.1 s pause (1.9..3.0 s): A's gain holds, flat, until just before B
        let held = at(2.0);
        assert!((held - at(1.85)).abs() < 0.3, "held {held:.2} vs A's end {:.2}", at(1.85));
        for i in 0..=16 {
            let t = 2.0 + i as f64 * 0.05;
            assert!((at(t) - held).abs() < 1e-9, "pause at {t:.2}s: {:.3} vs {held:.3}", at(t));
        }
        // the bleed in the pause gets exactly the held gain (it is not pumped)
        let (a, b) = (cut(2.0), cut(2.8));
        let moved = testing::rms_db(&y.data[0][a..b]) - testing::rms_db(&x.data[0][a..b]);
        assert!((moved - held).abs() < 0.01, "bleed moved {moved:.2} dB, held {held:.2}");
        // the gain reaches B's value as B starts
        assert!((at(3.0) - at(3.5)).abs() < 0.6, "B starts at {:.2}, settles at {:.2}", at(3.0), at(3.5));
        // the 4.1 s pause (4.9..9.0 s) glides from B's gain to C's
        let mid = at(7.0);
        assert!((mid - 0.5 * (at(4.9) + at(9.1))).abs() < 0.6, "glide midpoint {mid:.2}");
        assert!(at(6.0) > at(7.0) + 0.3 && at(7.0) > at(8.0) + 0.3, "{:.2} {:.2} {:.2}", at(6.0), at(7.0), at(8.0));
    }

    #[test]
    fn points_reproduce_the_ride_through_envelope() {
        let dir = TempDir::new("ride-points");
        let x = testing::stereo(words(&uneven_levels(16, 11), 0.3, 0.1, 0.3));
        let m = write(&dir, "music.wav", &testing::stereo(music(x.duration_s(), 0.05, 6)));
        let (y, c) = run_ctx(&spec(), &[("against", m.as_str())], x.clone()).unwrap();
        let r = result(&c);
        let points = r["points"].as_str().unwrap();
        assert_eq!(points.split(',').count() as u64, r["points_count"].as_u64().unwrap());
        for p in points.split(',') {
            let (t, v) = p.split_once('=').unwrap();
            assert!(t.ends_with('s') && v.ends_with("dB"), "{p}");
            assert!(v == "0dB" || v.starts_with('+') || v.starts_with('-'), "explicit sign: {p}");
            assert!(t.trim_end_matches('s').split('.').nth(1).map(|d| d.len() <= 3).unwrap_or(true), "{p}");
        }
        assert!(points.starts_with("0s="), "{points}");
        // the envelope command applies the same automation, sample for sample
        let z = run(&envelope_spec(), &[("points", points)], x.clone()).unwrap();
        assert_eq!(y, z);
        // far fewer keyframes than 10 ms steps
        assert!(r["points_count"].as_u64().unwrap() < (x.duration_s() * 100.0 / 4.0) as u64, "{points}");
    }

    #[test]
    fn simplified_keyframes_stay_within_tolerance() {
        let mut rng = Rng::new(5);
        let mut y = vec![0.0f64; 3000];
        let mut v = 0.0;
        for s in y.iter_mut() {
            v = (v + rng.range(-0.3, 0.3)).clamp(-4.0, 4.0);
            *s = v;
        }
        let y = smooth(&y, 6);
        let keep = simplify(&y, POINT_TOL_DB);
        assert!(keep.len() < y.len() / 3, "{} of {}", keep.len(), y.len());
        let c = Curve::new(keep.iter().map(|k| (*k as f64, y[*k])).collect(), false);
        for (i, v) in y.iter().enumerate() {
            assert!((c.eval(i as f64) - v).abs() <= POINT_TOL_DB + 1e-9, "{i}");
        }
        assert_eq!(simplify(&[1.0, 2.0], 0.1), vec![0, 1]);
        assert_eq!(simplify(&[0.0, 0.0, 0.0, 0.0], 0.1), vec![0, 3]);
    }

    #[test]
    fn target_range_and_amount() {
        let dir = TempDir::new("ride-params");
        let x = words(&uneven_levels(24, 21), 0.3, 0.1, 0.5);
        let m = write(&dir, "music.wav", &music(x.duration_s(), 0.05, 8));
        let sp = spec();
        let (_, c) = run_ctx(&sp, &[("against", m.as_str())], x.clone()).unwrap();
        let auto = result(&c);
        let typical = f(&auto, "target_lu");

        // an explicit target 3 LU under the take's typical offset lands there
        let t = format!("{:.2}LU", typical - 3.0);
        let (_, c) =
            run_ctx(&sp, &[("against", m.as_str()), ("target", t.as_str()), ("range", "12dB")], x.clone()).unwrap();
        let r = result(&c);
        assert_eq!(r["target_source"], "given");
        assert!((f(&r, "offset_after.median_lu") - (typical - 3.0)).abs() < 0.5, "{r}");

        // --range clamps the ride, and a big target warns about it
        let (_, c) = run_ctx(&sp, &[("against", m.as_str()), ("range", "2dB")], x.clone()).unwrap();
        let r = result(&c);
        assert!(f(&r, "gain_db.min") >= -2.0 && f(&r, "gain_db.max") <= 2.0, "{r}");
        assert!(f(&r, "at_range_share") > 0.2 && has_warning(&c, "range_limited"), "{r}");
        assert!(f(&r, "offset_after.spread_lu") > f(&auto, "offset_after.spread_lu"));

        // --amount 50% halves every move
        let (_, c) = run_ctx(&sp, &[("against", m.as_str()), ("amount", "50%"), ("range", "12dB")], x.clone()).unwrap();
        let half = result(&c);
        let (_, c) = run_ctx(&sp, &[("against", m.as_str()), ("range", "12dB")], x.clone()).unwrap();
        let full = result(&c);
        let ch = curve_of(half["points"].as_str().unwrap(), x.frames());
        let cf = curve_of(full["points"].as_str().unwrap(), x.frames());
        let mut checked = 0;
        for i in (0..x.frames()).step_by(4800) {
            let (h, g) = (ch.eval(i as f64), cf.eval(i as f64));
            assert!((h - 0.5 * g).abs() < 0.4, "at {i}: {h:.2} vs {g:.2}");
            checked += 1;
        }
        assert!(checked > 50);
        let ratio = f(&half, "offset_after.spread_lu") / f(&half, "offset_before.spread_lu");
        assert!((0.35..0.65).contains(&ratio), "half the correction leaves about half the spread: {ratio:.2}");
    }

    #[test]
    fn without_music_the_vocal_rides_against_itself() {
        let x = words(&uneven_levels(24, 13), 0.3, 0.1, 0.5);
        let (y, c) = run_ctx(&spec(), &[("range", "6dB")], x.clone()).unwrap();
        let r = result(&c);
        assert!(has_warning(&c, "no_against"));
        assert_eq!(r["against"], Value::Null);
        assert_eq!(f(&r, "target_lu"), 0.0);
        assert!(f(&r, "offset_before.spread_lu") > 6.0 && f(&r, "offset_after.spread_lu") < 1.5, "{r}");
        assert!(f(&r, "offset_after.median_lu").abs() < 0.5, "{r}");
        // a take within 3 dB of its typical level: evened out, loudness nearly unchanged
        let gentle: Vec<f64> = uneven_levels(24, 17).iter().map(|v| v / 2.0).collect();
        let xg = words(&gentle, 0.3, 0.1, 0.5);
        let (yg, cg) = run_ctx(&spec(), &[], xg.clone()).unwrap();
        let rg = result(&cg);
        assert!(f(&rg, "offset_after.spread_lu") < f(&rg, "offset_before.spread_lu") / 3.0, "{rg}");
        let shift = integrated(&yg.data, SR).unwrap() - integrated(&xg.data, SR).unwrap();
        assert!(shift.abs() < 0.5, "{shift}");
        // a stereo copy rides the same (BS.1770 channel sum, auto target)
        let (ys, cs) = run_ctx(&spec(), &[("range", "6dB")], testing::stereo(x.clone())).unwrap();
        assert_eq!(result(&cs)["points"], r["points"]);
        assert_eq!(ys.data[1], y.data[0]);
    }

    #[test]
    fn edge_cases_and_warnings() {
        let dir = TempDir::new("ride-edges");
        let sp = spec();
        // silence: unchanged, with a warning
        let s = AudioBuffer::new(SR, 2, 48000);
        let (y, c) = run_ctx(&sp, &[], s.clone()).unwrap();
        assert_eq!(y, s);
        assert!(has_warning(&c, "silent_input"));
        assert_eq!(result(&c)["points"], "0s=0dB");
        // music shorter than the vocal (and at another rate): warned, still rides
        let x = words(&uneven_levels(12, 3), 0.3, 0.1, 0.2);
        let short = music(x.duration_s() - 1.0, 0.05, 2);
        let short44 = resample(&short, 44100, Quality::High);
        let m = write(&dir, "short.wav", &short44);
        let (y, c) = run_ctx(&sp, &[("against", m.as_str())], x.clone()).unwrap();
        assert!(has_warning(&c, "against_length"), "{:?}", c.warnings);
        assert_eq!(y.frames(), x.frames());
        assert!(f(&result(&c), "offset_after.spread_lu") < f(&result(&c), "offset_before.spread_lu"));
        // an a cappella vocal against silent music holds and says so
        let m = write(&dir, "silent.wav", &AudioBuffer::new(SR, 1, x.frames()));
        let (y, c) = run_ctx(&sp, &[("against", m.as_str())], x.clone()).unwrap();
        assert!(has_warning(&c, "music_silent"));
        assert_eq!(y, x);
        // mix 0% is a null; bad values are usage errors
        assert_eq!(run(&sp, &[("mix", "0%")], x.clone()).unwrap(), x);
        for (k, v) in [("target", "loud"), ("target", "-14LUFS"), ("amount", "150%"), ("window", "0s"), ("band", "x")] {
            let e = run(&sp, &[(k, v)], x.clone()).unwrap_err();
            assert!(matches!(e.kind, ErrorKind::Usage), "--{k} {v}: {e:?}");
        }
        assert!(run(&sp, &[("against", "/nonexistent/music.wav")], x.clone()).is_err());
        // a band-limited detector still rides
        let (_, c) = run_ctx(&sp, &[("band", "200Hz..4kHz")], x).unwrap();
        assert!(f(&result(&c), "offset_after.spread_lu") < f(&result(&c), "offset_before.spread_lu"));
    }

    #[test]
    fn target_parsing() {
        assert_eq!(parse_target("auto").unwrap(), None);
        assert_eq!(parse_target("AUTO").unwrap(), None);
        assert_eq!(parse_target("-2LU").unwrap(), Some(-2.0));
        assert_eq!(parse_target("-2dB").unwrap(), Some(-2.0));
        assert_eq!(parse_target("+1").unwrap(), Some(1.0));
        assert_eq!(parse_target(" -1.5 lu ").unwrap(), Some(-1.5));
        assert!(parse_target("-14LUFS").is_err());
        assert!(parse_target("").is_err());
        assert_eq!(fmt_time(0.0), "0s");
        assert_eq!(fmt_time(1.25), "1.25s");
        assert_eq!(fmt_time(10.0), "10s");
        assert_eq!(fmt_time(2.0004), "2s");
        assert_eq!(fmt_db(-0.01), "0dB");
        assert_eq!(fmt_db(1.46), "+1.5dB");
        assert_eq!(fmt_db(-2.0), "-2.0dB");
        assert_eq!(interquartile_mean(&[0.0, 0.0, 4.0, 4.0]), Some(2.0));
        assert_eq!(interquartile_mean(&[1.0, 2.0, 3.0, 100.0, -100.0]), Some(2.0));
    }
}
