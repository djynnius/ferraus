# Writing ferraus ops (for contributors and building agents)

Every command is one `OpSpec` (see `crates/ferraus-core/src/op.rs`). The CLI flags, `fx`
stages, pipeline stages, JSON Schemas, MCP tools, `describe --agent`, and the docs site are
all generated from it, so names and help text are written exactly once.

## Shape of a crate

```rust
use ferraus_core::prelude::*;

pub fn ops() -> Vec<OpSpec> {
    vec![
        OpSpec::transform("gain", Category::Edit, "Apply a fixed gain", gain)
            .about("Longer plain-language explanation for people.")
            .param(param("by", ParamKind::Gain).default("0dB").help("Gain in dB (-6dB) or as a factor (0.5x)"))
            .example("ferraus gain in.wav --by -6dB -o quieter.wav", "Halve the amplitude"),
    ]
}

fn gain(a: &Args, mut buf: AudioBuffer, _ctx: &mut OpCtx) -> Result<AudioBuffer> {
    buf.scale(a.gain("by")?.linear() as f32);
    Ok(buf)
}
```

The CLI already calls `ops()` of every crate (`crates/ferraus-cli/src/lib.rs`). Never edit
the CLI to add a command; add it to your crate's `ops()`.

## Two kinds of op

* `OpSpec::transform(name, category, summary, fn(&Args, AudioBuffer, &mut OpCtx) -> Result<AudioBuffer>)`
  — one audio input, one audio output. The executor decodes the input, applies `--mix`
  (dry/wet, added automatically unless the op declares its own `mix` param or calls
  `.no_mix()`), converts `--sample-rate`/`--channels`, encodes, names the output, and writes
  the sidecar. Transforms are automatically `fx` stages and pipeline stages. Use `.no_mix()`
  for ops that change length or channel count (reverse is fine to mix; speed/stretch/trim are not).
* `OpSpec::custom(name, category, summary, fn(&Args, &mut OpCtx) -> Result<Value>)` — full
  control: read inputs with `ctx.read_audio(i)` / `ctx.input(i)`, choose output paths with
  `ctx.output_path(role, ext)`, write with `ctx.write_audio(path, &buf, role)` or
  `ctx.write_file(path, bytes, role)`. Return a JSON value; it becomes `result` in the
  envelope. Honour `ctx.globals.dry_run` (resolve and return the plan without writing).

Multi-word commands are just names with spaces: `"denoise profile"`, `"midi quantize"`,
`"analyze tempo"`. Commands whose first positional is a name, not a file (`models pull
<name>`), use `.positional("name")` with a matching param.

## Parameters

`param(name, kind)` with `.default("...")`, `.help("...")`, `.choices(&[..])` (with
`ParamKind::Choice`), `.repeatable()`, `.required()`, `.range("8..64")`, `.advanced()`.
Names are kebab-case; pipeline/JSON keys use underscores automatically. Every value is a
string parsed by the shared unit parser, so read them with the typed accessors:

| Kind | Example values | Accessor |
| --- | --- | --- |
| `Time` | `1.5s`, `1500ms`, `01:23.5`, `4b`, `-10s`, `end` | `a.time(n)?` / `a.seconds(n)?` (durations) / `a.sync_time(n)?` (`1/4` or `375ms`) |
| `Range` | `0s..2s`, `1:00..end` | `a.range(n)?` → `Option<TimeRange>`, `a.ranges(n)?` for repeatable; `.frames(sr, total, ctx.bpm())` |
| `Gain` | `-6dB`, `0.5x`, `-14LUFS`, `-1dBTP` | `a.db(n)?`, `a.gain(n)?.linear()` |
| `Freq` | `120Hz`, `2.5kHz`, `C4`, `midi:60` | `a.freq(n)?`, `a.freq_range(n)?` for `80Hz..8kHz` |
| `Ratio` | `4:1`, `4` | `a.ratio(n)?` |
| `Percent` | `80%`, `0.8` | `a.percent(n)?` → fraction |
| `Grid` | `1/16`, `1/8T`, `1/8D`, `auto` | `a.grid(n)?` → `Option<Grid>` |
| `Pitch` | `20cents`, `+2st` | `a.cents(n)?`, `a.semitones(n)?` |
| `Flag` | presence | `a.flag(n)` |
| `Switch` | `on`/`off` | `a.switch(n, default)` |
| `Int`/`Float` | `24`, `0.5` | `a.int(n)?`, `a.usize(n)?`, `a.f64(n)?`, `a.opt_f64(n)?` (`auto` → None) |
| `Choice` | one of `choices` | `a.choice(n)` |
| `Str`/`Path`/`List` | free text | `a.str(n)`, `a.path(n)`, `a.list(n)` (comma lists and repeats) |

Defaults must make the op sound right with zero parameters. Parameter names must never
collide with global flags (`output`, `format`, `channels`, `sample-rate`, `bit-depth`,
`bitrate`, `threads`, `seed`, `bpm`, `force`, ...) unless you mean the same thing — a param
named like a global receives the global's value. Prefer `ctx.bpm()` for tempo.

## Context helpers (`OpCtx`)

* `ctx.warn(code, msg)`, `ctx.metric(key, value)`, `ctx.note(key, value)` (run-time resolved
  params such as a detected key — shown in `params` and the sidecar), `ctx.model_used(..)`.
* `ctx.set_progress(fraction)`, `ctx.check_cancel()?` in long loops.
* `ctx.seed()` for stochastic steps; `ctx.threads()`; `ctx.globals.deterministic`.
* Composite ops can run other ops by name at run time without a compile-time dependency:
  `ferraus_core::exec::run_transform(ctx, "compress", &[("ratio", "1.5:1")], buf)?` and
  `ferraus_core::exec::run_nested(ctx, "transcribe", &[..], inputs, Some(out_path))?`.

## Errors

Return `ferraus_core::Error` with the right kind: `Error::usage` (bad parameter, exit 2),
`Error::input` (3), `Error::unsupported` (4), `Error::processing` (1),
`Error::new(ErrorKind::ModelNotCached, "model_not_cached", msg).with_hint("ferraus models pull x")`
(5), `ErrorKind::Plugin` (7), `Error::verification` (9). Add `.with_hint(cmd)` whenever a
runnable fix exists.

## Tests

Use `ferraus_core::testing`: `sine`, `noise`, `chirp`, `clicks`, `add`, `rms_diff`, `rms_db`,
`tone_level` (Goertzel), `si_sdr`, `run(spec, pairs, buf)`, `args(spec, pairs)`, `ctx()`,
`TempDir`, `write_wav`/`read_wav`. Fixtures are generated by code; never commit binary audio.
Every transform needs: the null test (`mix=0%` returns the input bit-exactly — automatic for
auto-mix ops, write it anyway), a golden/analytic test of what it does, and an edge case
(silence, mono vs stereo, very short input). Keep debug-build tests fast (< a few seconds each).
