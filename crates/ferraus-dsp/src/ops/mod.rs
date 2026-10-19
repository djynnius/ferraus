//! The effect commands. Each submodule defines `ops()` for one family; [`all`] collects
//! them. Helpers here handle the parameter forms several commands share.

use ferraus_core::exec::blend;
use ferraus_core::prelude::*;

mod dynamics;
mod eq;
mod fade;
mod modfx;
mod space;
mod stereo;
mod utility;
mod vocoder;

#[cfg(test)]
mod tests;

/// Every op in this crate.
pub fn all() -> Vec<OpSpec> {
    let mut v = Vec::new();
    v.extend(fade::ops());
    v.extend(dynamics::ops());
    v.extend(eq::ops());
    v.extend(space::ops());
    v.extend(modfx::ops());
    v.extend(stereo::ops());
    v.extend(utility::ops());
    v.extend(vocoder::ops());
    v
}

/// Sample rate as f64.
pub(crate) fn sr(b: &AudioBuffer) -> f64 {
    b.sample_rate as f64
}

/// For ops that declare their own `mix`: the fraction, validated to 0..1.
pub(crate) fn own_mix(a: &Args) -> Result<f64> {
    let m = a.percent("mix")?;
    if !(0.0..=1.0).contains(&m) {
        return Err(Error::usage("--mix must be between 0% and 100%"));
    }
    Ok(m)
}

/// Blend for ops with their own `mix` (0 → the dry input itself, bit-exact).
pub(crate) fn apply_own_mix(m: f64, dry: AudioBuffer, wet: AudioBuffer) -> AudioBuffer {
    if m <= 0.0 {
        dry
    } else if m >= 1.0 {
        wet
    } else {
        blend(&dry, &wet, m as f32)
    }
}

/// `auto` → None, otherwise a gain in dB.
pub(crate) fn gain_or_auto(a: &Args, name: &str) -> Result<Option<f64>> {
    match a.raw(name) {
        None => Ok(None),
        Some(v) if v.eq_ignore_ascii_case("auto") => Ok(None),
        Some(v) => {
            units::parse_db(v).map(Some).map_err(|e| Error::usage(format!("--{name}: {} (or 'auto')", e.message)))
        }
    }
}

/// An optional frequency: absent, `off`, or 0 Hz → None.
pub(crate) fn freq_or_off(a: &Args, name: &str) -> Result<Option<f64>> {
    Ok(a.opt_freq(name)?.filter(|f| *f > 0.0))
}

/// A rate given in Hz (`0.5Hz`) or as a beat length (`1/2` = one cycle per half note).
/// Beat values resolve against `--bpm`, falling back to 120 BPM with a warning.
pub(crate) fn rate_hz(a: &Args, name: &str, ctx: &mut OpCtx) -> Result<f64> {
    let raw = a.str_or(name, "1Hz");
    if raw.contains('/') {
        let g = units::Grid::parse(&raw).map_err(|e| Error::usage(format!("--{name}: {}", e.message)))?;
        let bpm = match ctx.bpm() {
            Some(b) => b,
            None => {
                ctx.warn(
                    "no_tempo",
                    format!("--{name} {raw} is in beats but no tempo is known; assuming 120 BPM (pass --bpm)"),
                );
                120.0
            }
        };
        Ok(1.0 / g.seconds(bpm))
    } else {
        units::parse_freq(&raw).map_err(|e| Error::usage(format!("--{name}: {}", e.message)))
    }
}

/// Angle in degrees from `90deg`, `90`, or `0.25` cycles is not accepted (degrees only).
pub(crate) fn degrees(a: &Args, name: &str) -> Result<f64> {
    let raw = a.str_or(name, "0deg");
    units::parse_with_unit(&raw, &["deg", "°"]).map_err(|e| Error::usage(format!("--{name}: {}", e.message)))
}

/// Fraction parameter with a range check.
pub(crate) fn percent_in(a: &Args, name: &str, lo: f64, hi: f64) -> Result<f64> {
    let v = a.percent(name)?;
    if v < lo || v > hi {
        return Err(Error::usage(format!("--{name} must be between {}% and {}%", lo * 100.0, hi * 100.0)));
    }
    Ok(v)
}

/// Positive duration in seconds.
pub(crate) fn secs(a: &Args, name: &str) -> Result<f64> {
    let v = a.seconds(name)?;
    if v < 0.0 {
        return Err(Error::usage(format!("--{name} must not be negative")));
    }
    Ok(v)
}

/// Sync time (`1/4` or `375ms`) in seconds; beats use `--bpm`, else `fallback_bpm`.
pub(crate) fn sync_secs(a: &Args, name: &str, sample_rate: u32, ctx: &mut OpCtx, fallback_bpm: f64) -> Result<f64> {
    let st = a.sync_time(name)?;
    if matches!(st, units::SyncTime::Grid(_)) && ctx.bpm().is_none() {
        ctx.warn(
            "no_tempo",
            format!(
                "--{name} {} is a beat value but no tempo is known; using {fallback_bpm} BPM (pass --bpm)",
                a.str_or(name, "")
            ),
        );
    }
    st.seconds(sample_rate, ctx.bpm(), fallback_bpm).map_err(|e| Error::usage(format!("--{name}: {}", e.message)))
}

/// Run `f` on each channel in place (parallel-safe signature, sequential execution keeps
/// results deterministic and single-threaded speed figures honest).
pub(crate) fn per_channel(buf: &mut AudioBuffer, mut f: impl FnMut(usize, &mut Vec<f32>)) {
    for (c, ch) in buf.data.iter_mut().enumerate() {
        f(c, ch);
    }
}

/// Parse a comma list where each entry uses `parse`; shorter lists repeat their last value.
pub(crate) fn per_band<T: Clone>(
    a: &Args,
    name: &str,
    bands: usize,
    parse: impl Fn(&str) -> Result<T>,
) -> Result<Vec<T>> {
    let items = a.list(name);
    if items.is_empty() {
        return Err(Error::usage(format!("--{name} needs a value")));
    }
    let parsed: Vec<T> = items
        .iter()
        .map(|s| parse(s).map_err(|e| Error::usage(format!("--{name}: {}", e.message))))
        .collect::<Result<_>>()?;
    if parsed.len() > bands {
        return Err(Error::usage(format!("--{name} has {} values but there are only {bands} bands", parsed.len())));
    }
    Ok((0..bands).map(|i| parsed[i.min(parsed.len() - 1)].clone()).collect())
}
