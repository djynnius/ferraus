//! `mix balance`: measure every stem and propose the gains that put them on a genre balance
//! template, so an agent never does loudness arithmetic by hand.
//!
//! A stem's loudness is its BS.1770 gated integrated loudness ([`loudness::integrated_lufs`]).
//! The absolute (-70 LUFS) and relative (-10 LU) gates skip silence, so sparse stems (effects,
//! backing vocals) are measured on the part that plays. The anchor role lands at
//! `--anchor-lufs`; every other stem lands at its role's template offset from the anchor:
//! `target = anchor_lufs + offset(role) - offset(anchor_role)`, `gain = target - measured`.

use crate::loudness;
use crate::mix::{mix_buffers, Track};
use crate::util;
use ferraus_core::op::Inputs;
use ferraus_core::prelude::*;
use ferraus_core::resample::{resample, Quality};
use std::path::{Path, PathBuf};

pub fn ops() -> Vec<OpSpec> {
    vec![balance_spec()]
}

pub fn balance_spec() -> OpSpec {
    OpSpec::custom("mix balance", Category::Edit, "Propose stem gains that put a multitrack on a genre balance template", balance)
        .inputs(Inputs::MANY_AUDIO)
        .about(
            "Measures every stem (BS.1770 gated integrated loudness, so the silent stretches of a \
             sparse stem do not count) and proposes the gain that puts each one on a genre \
             balance template: the anchor stem lands at --anchor-lufs and every other stem sits \
             at its role's offset from the anchor (pop: kick 3 dB, bass 5 dB, backing vocals \
             7 dB, pads 12 dB under the lead vocal). A mono stem among stereo ones is measured as \
             it plays in the sum, copied to both channels (3 dB louder than the mono file reads \
             alone). Roles come from --roles (one per input, in input order; auto infers that \
             one), else from the file name (vocals, Lead Vox, BV, 808, Kick, Gtr, Pad...), else \
             other. The result lists each stem's loudness, target, and gain, the --gains list, \
             and a ready-to-run 'ferraus mix' command. --render also sums the stems with those \
             gains and reports the sum's loudness and true peak (headroom_ok when it peaks at or \
             below -3 dBTP); -o implies --render and writes the balanced sum. The offsets are \
             hand-designed starting points, not measurements: a static balance to refine, not a \
             finished mix.",
        )
        .param(param("roles", ParamKind::List).help(
            "Role per input, in input order: vocal, bvs, lead, kick, snare, hats, drums, perc, bass, sub, guitar, keys, piano, synth, pad, strings, brass, fx, other (synonyms such as vox, bv, 808, gtr work; auto or a missing value infers it from the file name)",
        ))
        .param(
            param("genre", ParamKind::Choice)
                .choices(GENRES)
                .default("pop")
                .help("Balance template: how far each role sits under the lead vocal"),
        )
        .param(param("anchor", ParamKind::Str).default("auto").help(
            "Role everything is relative to (it lands at --anchor-lufs); auto = vocal, else lead, kick, drums, else the loudest stem",
        ))
        .param(
            param("anchor-lufs", ParamKind::Gain)
                .default("-20LUFS")
                .help("Loudness of the anchor stem; -20LUFS leaves the sum of 6-10 stems near -15 LUFS with bus headroom"),
        )
        .param(param("render", ParamKind::Flag).help(
            "Also sum the stems with the proposed gains and measure the sum (loudness, true peak); -o implies it and writes the sum",
        ))
        .example(
            "ferraus mix balance vocals.wav drums.wav bass.wav other.wav",
            "Gains for the stems of 'ferraus separate' on the pop template (roles from the file names)",
        )
        .example(
            "ferraus mix balance vox.wav kick.wav snare.wav 808.wav keys.wav --genre hiphop --render",
            "Hip-hop balance, and report the loudness and true peak of the balanced sum",
        )
        .example(
            "ferraus mix balance take1.wav take2.wav strings.wav --roles vocal,bvs,strings --anchor-lufs -22LUFS -o balanced.wav",
            "Name the roles, leave 2 dB more headroom, and write the balanced sum",
        )
        .example(
            "ferraus mix balance gtr.wav bass.wav kick.wav snare.wav --genre rock --anchor kick",
            "Rock template anchored on the kick (no vocal recorded yet)",
        )
}

/// Genres of the balance template, in the column order of [`TEMPLATE`].
pub const GENRES: &[&str] = &["pop", "hiphop", "edm", "rock", "acoustic", "jazz"];

/// Role offsets in dB relative to the lead vocal, one column per genre in [`GENRES`] order.
///
/// # Provenance (read before trusting these numbers)
///
/// Like the tonal-balance curves in `ferraus-master`'s `genres.rs`, these offsets are
/// **hand-designed starting points**, not measurements of a corpus of balanced sessions.
/// They encode widely shared static-balance rules of thumb: the lead vocal on top; kick and
/// bass a few dB under it (closer in hip-hop and EDM, further in acoustic and jazz); guitars
/// up front in rock, keys and piano in jazz and acoustic music; harmonic beds (pads,
/// strings) well under; hats, percussion, and effects as seasoning. An offset is the gap
/// between a stem's gated integrated loudness and the anchor's. With 6-10 stems on these
/// offsets the sum lands about 5 LU above the anchor (near -15 LUFS with the default
/// -20 LUFS anchor), which leaves room for bus processing and a true peak below -3 dBTP.
/// Replace the table with measured averages when balanced reference sessions exist.
pub const TEMPLATE: &[(&str, [f64; 6])] = &[
    ("vocal", [0.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
    ("bvs", [-7.0, -8.0, -6.0, -6.0, -6.0, -6.0]),
    ("lead", [-4.0, -5.0, -2.0, -3.0, -4.0, -3.0]),
    ("kick", [-3.0, -1.0, 0.0, -3.0, -7.0, -7.0]),
    ("snare", [-4.0, -3.0, -3.0, -2.0, -8.0, -7.0]),
    ("hats", [-12.0, -11.0, -10.0, -12.0, -15.0, -10.0]),
    ("drums", [-2.0, 0.0, 0.0, -1.0, -5.0, -3.0]),
    ("perc", [-12.0, -11.0, -10.0, -12.0, -12.0, -10.0]),
    ("bass", [-5.0, -2.0, -2.0, -5.0, -7.0, -4.0]),
    ("sub", [-6.0, -2.0, -2.0, -7.0, -10.0, -8.0]),
    ("guitar", [-7.0, -9.0, -8.0, -3.0, -4.0, -4.0]),
    ("keys", [-8.0, -9.0, -7.0, -8.0, -6.0, -4.0]),
    ("piano", [-7.0, -8.0, -7.0, -7.0, -4.0, -3.0]),
    ("synth", [-7.0, -8.0, -3.0, -9.0, -10.0, -8.0]),
    ("pad", [-12.0, -13.0, -9.0, -12.0, -12.0, -10.0]),
    ("strings", [-10.0, -11.0, -9.0, -10.0, -7.0, -6.0]),
    ("brass", [-8.0, -9.0, -7.0, -8.0, -8.0, -3.0]),
    ("fx", [-15.0, -14.0, -11.0, -15.0, -16.0, -16.0]),
    ("other", [-9.0, -9.0, -7.0, -8.0, -8.0, -7.0]),
];

/// Words accepted for a role in `--roles`, `--anchor`, and file names.
const SYNONYMS: &[(&str, &str)] = &[
    ("vocals", "vocal"),
    ("vox", "vocal"),
    ("voice", "vocal"),
    ("leadvox", "vocal"),
    ("bv", "bvs"),
    ("bgv", "bvs"),
    ("backing", "bvs"),
    ("harmony", "bvs"),
    ("choir", "bvs"),
    ("adlib", "bvs"),
    ("bd", "kick"),
    ("kik", "kick"),
    ("sd", "snare"),
    ("snr", "snare"),
    ("hh", "hats"),
    ("hihat", "hats"),
    ("hat", "hats"),
    ("cymbal", "hats"),
    ("ride", "hats"),
    ("kit", "drums"),
    ("overhead", "drums"),
    ("oh", "drums"),
    ("room", "drums"),
    ("shaker", "perc"),
    ("tamb", "perc"),
    ("conga", "perc"),
    ("bongo", "perc"),
    ("clap", "perc"),
    ("808", "sub"),
    ("gtr", "guitar"),
    ("guitars", "guitar"),
    ("rhodes", "keys"),
    ("organ", "keys"),
    ("wurli", "keys"),
    ("ep", "keys"),
    ("arp", "synth"),
    ("pluck", "synth"),
    ("violin", "strings"),
    ("viola", "strings"),
    ("cello", "strings"),
    ("orchestra", "strings"),
    ("horn", "brass"),
    ("horns", "brass"),
    ("trumpet", "brass"),
    ("sax", "brass"),
    ("trombone", "brass"),
    ("sfx", "fx"),
    ("riser", "fx"),
    ("sweep", "fx"),
    ("impact", "fx"),
    ("noise", "fx"),
];

/// When a file name mentions several roles, the first one in this order wins: effects
/// before what they decorate ("Vocal FX" is an effects stem), backing vocals before the lead
/// ("Backing Vocals"), the vocal before a lead line ("Lead Vox"), a lead line before its
/// instrument ("Synth Lead"), sub before bass ("808 Bass"), single drums before the kit, and
/// specific instruments before generic ones ("Synth Pad", "Gtr Arp").
const NAME_PRIORITY: &[&str] = &[
    "fx", "bvs", "vocal", "lead", "sub", "bass", "kick", "snare", "hats", "perc", "drums", "pad", "strings", "brass",
    "piano", "keys", "guitar", "synth", "other",
];

/// Roles `--anchor auto` looks for, in order; without any of them the loudest stem anchors.
const AUTO_ANCHORS: &[&str] = &["vocal", "lead", "kick", "drums"];

/// A rendered sum peaking above this (dBTP) gets a warning: mastering wants the headroom.
const RENDER_HEADROOM_DBTP: f64 = -3.0;
/// Highest true peak written to a fixed-point rough-mix file (`-o`); hotter sums are lowered.
const WRITE_CEILING_DBTP: f64 = -1.0;

/// A proposed boost above this (dB) usually means a near-silent or mislabelled stem.
const LARGE_GAIN_DB: f64 = 12.0;

/// Template offset of `role` for the genre in `column` of [`TEMPLATE`] (see [`GENRES`]).
pub fn role_offset(role: &str, column: usize) -> f64 {
    TEMPLATE.iter().find(|(r, _)| *r == role).map(|(_, o)| o[column]).unwrap_or(0.0)
}

fn lookup_role(word: &str) -> Option<&'static str> {
    TEMPLATE
        .iter()
        .map(|(r, _)| *r)
        .find(|r| *r == word)
        .or_else(|| SYNONYMS.iter().find(|(s, _)| *s == word).map(|(_, r)| *r))
}

/// The role a word names (a role, a synonym, or either in the singular or plural), if any:
/// `Vox` → vocal, `808` → sub, `kicks` → kick, `drum` → drums.
pub fn canonical_role(word: &str) -> Option<&'static str> {
    let w = word.trim().to_lowercase();
    if w.chars().count() < 2 {
        return None;
    }
    lookup_role(&w)
        .or_else(|| w.strip_suffix('s').filter(|s| s.len() >= 2).and_then(lookup_role))
        .or_else(|| lookup_role(&format!("{w}s")))
}

/// Split a word at camelCase and letter/digit boundaries: `LeadVox` → `Lead`, `Vox`;
/// `Kick01` → `Kick`, `01`.
fn split_boundaries(word: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut prev: Option<char> = None;
    for (i, c) in word.char_indices() {
        if let Some(p) = prev {
            if (p.is_lowercase() && c.is_uppercase()) || p.is_alphabetic() != c.is_alphabetic() {
                parts.push(&word[start..i]);
                start = i;
            }
        }
        prev = Some(c);
    }
    parts.push(&word[start..]);
    parts
}

/// Lower-case tokens of a file name: words split on anything not alphanumeric, plus their
/// camelCase and letter/digit parts.
fn name_tokens(name: &str) -> Vec<String> {
    let mut out = Vec::new();
    for word in name.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()) {
        out.push(word.to_lowercase());
        let parts = split_boundaries(word);
        if parts.len() > 1 {
            out.extend(parts.into_iter().map(str::to_lowercase));
        }
    }
    out
}

/// The role a file name suggests (`08 Lead Vox.wav` → vocal, `BV_harm.wav` → bvs,
/// `808.wav` → sub, `song.vocals.wav` → vocal), or `None` when no token names a role.
pub fn role_from_name(path: &Path) -> Option<&'static str> {
    let stem = path.file_stem()?.to_string_lossy();
    let found: Vec<&str> = name_tokens(&stem).iter().filter_map(|t| canonical_role(t)).collect();
    NAME_PRIORITY.iter().find(|r| found.contains(r)).copied()
}

fn role_list() -> String {
    TEMPLATE.iter().map(|(r, _)| *r).collect::<Vec<_>>().join(", ")
}

/// Role and its source (`given`, `filename`, or `default`) for every input.
fn resolve_roles(a: &Args, inputs: &[PathBuf]) -> Result<Vec<(&'static str, &'static str)>> {
    let given = a.list("roles");
    if given.len() > inputs.len() {
        return Err(Error::usage(format!(
            "--roles has {} values but there are only {} inputs",
            given.len(),
            inputs.len()
        )));
    }
    let mut roles = Vec::with_capacity(inputs.len());
    for (i, path) in inputs.iter().enumerate() {
        let role = match given.get(i).filter(|r| !r.eq_ignore_ascii_case("auto")) {
            Some(r) => (
                canonical_role(r).ok_or_else(|| {
                    Error::usage(format!("--roles: '{r}' is not a role"))
                        .with_hint(format!("use one of {}, or auto to infer it from the file name", role_list()))
                })?,
                "given",
            ),
            None => role_from_name(path).map(|r| (r, "filename")).unwrap_or(("other", "default")),
        };
        roles.push(role);
    }
    Ok(roles)
}

/// One measured stem.
struct Stem {
    path: String,
    role: &'static str,
    role_source: &'static str,
    channels: usize,
    /// Gated integrated loudness as the stem plays in the sum (see [`measure`]).
    lufs: f64,
    true_peak_dbtp: f64,
    /// The decoded audio, kept only when the sum is rendered.
    buf: Option<AudioBuffer>,
}

/// Channel count of the sum: that of the widest stem, read from the file headers so every
/// stem can be measured in the layout it will have in the sum.
fn sum_channels(ctx: &OpCtx) -> usize {
    ctx.inputs
        .iter()
        .filter(|p| p.as_os_str() != "-" && ctx.check_allowed(p).is_ok())
        .filter_map(|p| ctx.io.probe(p).ok())
        .map(|info| info.channels)
        .max()
        .unwrap_or(1)
}

/// Decode and measure every stem. A stem narrower than the sum is measured as it plays in
/// it: `ferraus mix` copies a mono stem to both channels of a stereo sum, which makes it
/// 3 dB louder than the mono file reads on its own.
fn measure(ctx: &mut OpCtx, roles: &[(&'static str, &'static str)], layout: usize, keep: bool) -> Result<Vec<Stem>> {
    let mut stems = Vec::with_capacity(roles.len());
    for (i, &(role, role_source)) in roles.iter().enumerate() {
        ctx.check_cancel()?;
        ctx.set_progress(i as f64 / roles.len() as f64);
        let buf = ctx.read_audio(i)?;
        let lufs = if buf.channels() < layout {
            loudness::integrated_lufs(&buf.with_channels(layout))?
        } else {
            loudness::integrated_lufs(&buf)?
        };
        stems.push(Stem {
            path: ctx.inputs[i].display().to_string(),
            role,
            role_source,
            channels: buf.channels(),
            lufs,
            true_peak_dbtp: loudness::true_peak_dbtp(&buf),
            buf: keep.then_some(buf),
        });
    }
    Ok(stems)
}

/// The `--anchor` role when one is given (checked against the inputs' roles before any audio
/// is decoded), `None` for auto.
fn given_anchor(a: &Args, roles: &[(&'static str, &'static str)]) -> Result<Option<&'static str>> {
    let raw = a.str_or("anchor", "auto");
    if raw.eq_ignore_ascii_case("auto") {
        return Ok(None);
    }
    let role = canonical_role(&raw).ok_or_else(|| {
        Error::usage(format!("--anchor: '{raw}' is not a role"))
            .with_hint(format!("use auto or one of {}", role_list()))
    })?;
    if !roles.iter().any(|(r, _)| *r == role) {
        let mut have: Vec<&str> = Vec::new();
        for (r, _) in roles {
            if !have.contains(r) {
                have.push(r);
            }
        }
        return Err(Error::usage(format!(
            "--anchor {role}: no input has that role (the inputs are: {})",
            have.join(", ")
        ))
        .with_hint("pass --anchor auto, or label the stems with --roles"));
    }
    Ok(Some(role))
}

/// `--anchor auto`: the first of [`AUTO_ANCHORS`] among the stems (`auto`), else the role of
/// the loudest stem (`loudest`).
fn auto_anchor(stems: &[Stem]) -> (&'static str, &'static str) {
    if let Some(role) = AUTO_ANCHORS.iter().find(|r| stems.iter().any(|s| s.role == **r)) {
        return (role, "auto");
    }
    let loudest = stems.iter().filter(|s| s.lufs.is_finite()).max_by(|x, y| x.lufs.total_cmp(&y.lufs));
    (loudest.unwrap_or(&stems[0]).role, "loudest")
}

fn anchor_lufs(a: &Args) -> Result<f64> {
    match a.gain("anchor-lufs")? {
        units::Gain::Lufs(v) | units::Gain::Db(v) => Ok(v),
        _ => Err(Error::usage("--anchor-lufs must be a loudness such as -20LUFS")),
    }
}

/// Round to 0.1 dB (the precision of the proposed gains).
fn round1(x: f64) -> f64 {
    let r = (x * 10.0).round() / 10.0;
    if r == 0.0 {
        0.0 // no "-0dB"
    } else {
        r
    }
}

/// A number with at most one decimal and no trailing `.0`: `-6`, `2.5`.
fn short_num(x: f64) -> String {
    let s = format!("{:.1}", round1(x));
    s.strip_suffix(".0").map(str::to_string).unwrap_or(s)
}

/// A gain as `mix --gains` takes it: `0dB`, `-6dB`, `+2.5dB`.
fn gain_str(db: f64) -> String {
    let db = round1(db);
    if db > 0.0 {
        format!("+{}dB", short_num(db))
    } else {
        format!("{}dB", short_num(db))
    }
}

/// Quote one argument as a POSIX shell word when it needs it.
fn shell_quote(a: &str) -> String {
    if !a.is_empty() && a.chars().all(|c| c.is_ascii_alphanumeric() || "-_./:=+,@%^".contains(c)) {
        a.to_string()
    } else {
        format!("'{}'", a.replace('\'', "'\\''"))
    }
}

/// The `ferraus mix` command that applies the gains. It writes `rough_mix.wav` in the current
/// directory: never into the stems folder, which should hold only the stems.
fn mix_command(inputs: &[PathBuf], gains: &[String]) -> String {
    let out = PathBuf::from("rough_mix.wav");
    let mut words = vec!["ferraus".to_string(), "mix".to_string()];
    words.extend(inputs.iter().map(|p| shell_quote(&p.display().to_string())));
    words.extend(["--gains".to_string(), gains.join(","), "-o".to_string(), shell_quote(&out.display().to_string())]);
    words.join(" ")
}

/// Sum the kept stems with `gains_db` exactly as `ferraus mix --gains` would (resampled to the
/// first stem's rate, the widest channel layout), measure the sum, and write it when `-o`
/// was given.
fn render(ctx: &mut OpCtx, stems: &mut [Stem], gains_db: &[f64], anchor_lufs: f64) -> Result<Value> {
    let rate = stems.first().and_then(|s| s.buf.as_ref()).map(|b| b.sample_rate).unwrap_or(48000);
    let channels = stems.iter().filter_map(|s| s.buf.as_ref()).map(|b| b.channels()).max().unwrap_or(1);
    let mut tracks = Vec::with_capacity(stems.len());
    for (i, (stem, &gain)) in stems.iter_mut().zip(gains_db).enumerate() {
        let Some(buf) = stem.buf.take() else { continue };
        let buf = if buf.sample_rate != rate {
            ctx.note(&format!("resampled_input_{}", i + 1), json!({"from": buf.sample_rate, "to": rate}));
            resample(&buf, rate, Quality::High)
        } else {
            buf
        };
        tracks.push(Track { buf, gain: db_to_lin(gain) as f32, pan: None, offset: 0 });
    }
    let sum = mix_buffers(&tracks, channels, rate);
    drop(tracks);
    let lufs = loudness::integrated_lufs(&sum)?;
    let true_peak = lin_to_db(loudness::true_peak_conservative(&sum)? as f64);
    let headroom_ok = true_peak <= RENDER_HEADROOM_DBTP;
    if !headroom_ok {
        let lower = (true_peak - RENDER_HEADROOM_DBTP).ceil().max(1.0);
        ctx.warn(
            "render_headroom",
            format!(
                "the balanced sum peaks at {true_peak:+.2} dBTP, above {RENDER_HEADROOM_DBTP} dBTP; lower --anchor-lufs by {} dB (--anchor-lufs {}LUFS)",
                short_num(lower),
                short_num(anchor_lufs - lower)
            ),
        );
    }
    let mut v = json!({
        "integrated_lufs": util::round_db(lufs),
        "true_peak_dbtp": util::round_db(true_peak),
        "headroom_ok": headroom_ok,
        "duration_s": util::round_s(sum.duration_s()),
        "channels": sum.channels(),
    });
    if ctx.globals.output.is_some() {
        let ext = util::audio_ext(ctx);
        let path = ctx.output_path(None, &ext)?;
        let mut out = util::conform(sum, ctx);
        // A fixed-point file cannot hold a sum above full scale: write it lower instead of
        // clipping it (the balance between the stems is unchanged; 32-bit float keeps it as is).
        if ctx.globals.bit_depth != Some(32) && true_peak > WRITE_CEILING_DBTP {
            let g = WRITE_CEILING_DBTP - true_peak;
            out.scale(db_to_lin(g) as f32);
            v["written_gain_db"] = json!(util::round_db(g));
            ctx.warn(
                "render_lowered",
                format!(
                    "the balanced sum peaks at {true_peak:+.2} dBTP, so {} was written {:.2} dB lower to avoid clipping; the balance between the stems is unchanged",
                    path.display(),
                    -g
                ),
            );
        }
        ctx.write_audio(&path, &out, None)?;
        v["path"] = json!(path.display().to_string());
    }
    Ok(v)
}

fn warn_about_stems(ctx: &mut OpCtx, stems: &[Stem], gains_db: &[f64]) {
    for (s, &g) in stems.iter().zip(gains_db) {
        if !s.lufs.is_finite() {
            ctx.warn(
                "silent_stem",
                format!("{} is silent or entirely below the -70 LUFS gate; its gain stays at 0 dB", s.path),
            );
        } else if g > LARGE_GAIN_DB {
            ctx.warn(
                "large_gain",
                format!(
                    "{} needs {} to reach its target; it is probably near-silent, mostly empty, or mislabelled (check --roles)",
                    s.path,
                    gain_str(g)
                ),
            );
        }
    }
    let defaulted: Vec<&str> = stems.iter().filter(|s| s.role_source == "default").map(|s| s.path.as_str()).collect();
    if !defaulted.is_empty() {
        ctx.warn(
            "role_default",
            format!(
                "no role found in the file name of {}; treated as other (label them with --roles)",
                defaulted.join(", ")
            ),
        );
    }
}

fn balance(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    if ctx.inputs.is_empty() {
        return Err(Error::usage("mix balance needs at least one stem"));
    }
    let genre = a.choice("genre");
    let column = GENRES.iter().position(|g| *g == genre).unwrap_or(0);
    let anchor_level = anchor_lufs(a)?;
    let roles = resolve_roles(a, &ctx.inputs)?;
    let given = given_anchor(a, &roles)?;
    let rendering = a.flag("render") || ctx.globals.output.is_some();
    let layout = sum_channels(ctx);
    let mut stems = measure(ctx, &roles, layout, rendering)?;
    let (anchor, anchor_source) = match given {
        Some(role) => (role, "given"),
        None => auto_anchor(&stems),
    };
    let anchor_offset = role_offset(anchor, column);

    let mut gains_db = Vec::with_capacity(stems.len());
    let mut described = Vec::with_capacity(stems.len());
    for s in &stems {
        let target = anchor_level + role_offset(s.role, column) - anchor_offset;
        let gain = if s.lufs.is_finite() { round1(target - s.lufs) } else { 0.0 };
        gains_db.push(gain);
        described.push(json!({
            "path": s.path,
            "role": s.role,
            "role_source": s.role_source,
            "channels": s.channels,
            "integrated_lufs": util::round_db(s.lufs),
            "true_peak_dbtp": util::round_db(s.true_peak_dbtp),
            "target_lufs": util::round_db(target),
            "gain_db": gain,
        }));
    }
    warn_about_stems(ctx, &stems, &gains_db);
    let gains: Vec<String> = gains_db.iter().map(|&g| gain_str(g)).collect();
    ctx.note("anchor", anchor);
    ctx.note("roles", roles.iter().map(|(r, _)| *r).collect::<Vec<_>>().join(","));
    ctx.metric("max_gain_db", gains_db.iter().copied().fold(f64::NEG_INFINITY, f64::max));
    ctx.metric("min_gain_db", gains_db.iter().copied().fold(f64::INFINITY, f64::min));

    let mut result = json!({
        "anchor": anchor,
        "anchor_source": anchor_source,
        "anchor_lufs": anchor_level,
        "genre": genre,
        "channels": layout,
        "inputs": described,
        "gains": gains,
        "mix_command": mix_command(&ctx.inputs, &gains),
    });
    if rendering {
        ctx.set_progress(0.9);
        result["render"] = render(ctx, &mut stems, &gains_db, anchor_level)?;
    }
    Ok(result)
}
