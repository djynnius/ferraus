//! `channels`: mix down, up-mix, extract, swap, and mid/side encode/decode.

use crate::level::channel_index;
use ferraus_core::prelude::*;

pub fn ops() -> Vec<OpSpec> {
    vec![channels_spec()]
}

pub fn channels_spec() -> OpSpec {
    OpSpec::transform("channels", Category::Edit, "Mix down, up-mix, extract, swap, or mid/side-convert channels", channels)
        .about(
            "Does one channel operation per run. --to mono averages the channels; --to stereo \
             copies a mono signal to both sides (and folds 5.1 down with the ITU coefficients). \
             --extract keeps one channel as a mono file. --swap exchanges left and right. \
             --mid-side encode turns L/R into mid (L+R)/2 and side (L-R)/2; decode reverses it \
             exactly.",
        )
        .param(param("to", ParamKind::Choice).choices(&["mono", "stereo"]).help("Convert to mono or stereo"))
        .param(param("extract", ParamKind::Str).help("Keep one channel: L, R, C, or a channel number from 1"))
        .param(param("swap", ParamKind::Flag).help("Swap the left and right channels"))
        .param(
            param("mid-side", ParamKind::Choice)
                .choices(&["encode", "decode"])
                .help("encode: L/R to mid/side; decode: mid/side back to L/R"),
        )
        .no_mix()
        .example("ferraus channels stereo.wav --to mono -o mono.wav", "Average left and right into one channel")
        .example("ferraus channels interview.wav --extract R -o guest.wav", "Keep only the right channel (the guest's mic)")
        .example("ferraus channels mix.wav --mid-side encode -o ms.wav", "Get mid in channel 1 and side in channel 2 for M/S processing")
}

fn need_stereo(buf: &AudioBuffer, what: &str) -> Result<()> {
    if buf.channels() < 2 {
        return Err(Error::usage(format!("{what} needs a stereo input; this one is mono")));
    }
    Ok(())
}

fn channels(a: &Args, buf: AudioBuffer, ctx: &mut OpCtx) -> Result<AudioBuffer> {
    let actions: Vec<&str> = [
        ("to", a.has("to")),
        ("extract", a.has("extract")),
        ("swap", a.flag("swap")),
        ("mid-side", a.has("mid-side")),
    ]
    .into_iter()
    .filter(|(_, on)| *on)
    .map(|(n, _)| n)
    .collect();
    match actions.len() {
        0 => {
            return Err(Error::usage("channels needs one of --to, --extract, --swap, --mid-side")
                .with_hint("ferraus channels <input> --to mono"))
        }
        1 => {}
        _ => {
            return Err(Error::usage(format!(
                "channels does one operation per run; got {}",
                actions.iter().map(|n| format!("--{n}")).collect::<Vec<_>>().join(" and ")
            )))
        }
    }
    let sr = buf.sample_rate;
    let ch = buf.channels();
    let out = match actions[0] {
        "to" => match a.choice("to").as_str() {
            "mono" => buf.with_channels(1),
            _ => match ch {
                1 => buf.with_channels(2),
                2 => buf,
                6 => {
                    // ITU-R BS.775 fold-down: L R C LFE Ls Rs
                    let k = std::f32::consts::FRAC_1_SQRT_2;
                    let d = &buf.data;
                    let n = buf.frames();
                    let l = (0..n).map(|i| d[0][i] + k * d[2][i] + k * d[4][i]).collect();
                    let r = (0..n).map(|i| d[1][i] + k * d[2][i] + k * d[5][i]).collect();
                    ctx.note("downmix", "ITU-R BS.775 (LFE dropped)");
                    AudioBuffer::from_planar(sr, vec![l, r])
                }
                _ => {
                    ctx.warn(
                        "channels_dropped",
                        format!("no standard fold-down for {ch} channels; kept the first two"),
                    );
                    AudioBuffer::from_planar(sr, buf.data[..2].to_vec())
                }
            },
        },
        "extract" => {
            let name = a.str_or("extract", "L");
            let i = channel_index(&name, ch)?;
            ctx.note("extracted_channel", i + 1);
            AudioBuffer::from_mono(sr, buf.data[i].clone())
        }
        "swap" => {
            need_stereo(&buf, "--swap")?;
            let mut b = buf;
            b.data.swap(0, 1);
            b
        }
        _ => {
            need_stereo(&buf, "--mid-side")?;
            let mut b = buf;
            if ch > 2 {
                ctx.warn("channels_dropped", "mid/side uses the first two channels; the rest were dropped");
                b.data.truncate(2);
            }
            let (l, r) = b.data.split_at_mut(1);
            let (x, y) = (&mut l[0], &mut r[0]);
            let decode = a.choice("mid-side") == "decode";
            for (p, q) in x.iter_mut().zip(y.iter_mut()) {
                let (u, v) = (*p, *q);
                if decode {
                    *p = u + v;
                    *q = u - v;
                } else {
                    *p = 0.5 * (u + v);
                    *q = 0.5 * (u - v);
                }
            }
            b
        }
    };
    Ok(out)
}
