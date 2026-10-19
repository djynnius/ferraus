//! Operation specs: the single definition of every command. The CLI, `fx` stages,
//! pipeline stages, JSON Schemas, MCP tools, `describe --agent`, and the docs are all
//! generated from these, so parameter names and help text are written exactly once.

use crate::buffer::AudioBuffer;
use crate::ctx::OpCtx;
use crate::error::{Error, Result};
use crate::units::{self, Gain, Grid, SyncTime, Time, TimeRange};
use serde::Serialize;
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// The value type of a parameter. Every type is parsed from a string by the shared
/// unit parser, so CLI flags, fx strings, pipeline YAML, and MCP JSON accept the same text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ParamKind {
    /// A presence flag; `true`/`false`/`on`/`off` also accepted in fx and pipelines.
    Flag,
    /// `on`/`off`, `true`/`false`.
    Switch,
    Int,
    Float,
    Str,
    Path,
    /// A time point or duration (`1.5s`, `4b`, `01:23`).
    Time,
    /// `start..end`.
    Range,
    /// `-6dB`, `0.5x`, `-14LUFS`, `-1dBTP`.
    Gain,
    /// `120Hz`, `2.5kHz`, `C4`.
    Freq,
    /// `4:1`.
    Ratio,
    /// `80%` or `0.8`.
    Percent,
    /// `1/16`, `1/8T`.
    Grid,
    /// `80Hz..8kHz`, `C2..C6`.
    FreqRange,
    /// One of a fixed set of words.
    Choice,
    /// Cents or semitones.
    Pitch,
    /// Free-form list (comma separated).
    List,
}

impl ParamKind {
    pub fn type_name(self) -> &'static str {
        match self {
            ParamKind::Flag => "flag",
            ParamKind::Switch => "on|off",
            ParamKind::Int => "integer",
            ParamKind::Float => "number",
            ParamKind::Str => "text",
            ParamKind::Path => "path",
            ParamKind::Time => "time",
            ParamKind::Range => "range",
            ParamKind::Gain => "gain",
            ParamKind::Freq => "frequency",
            ParamKind::Ratio => "ratio",
            ParamKind::Percent => "percent",
            ParamKind::Grid => "grid",
            ParamKind::FreqRange => "frequency range",
            ParamKind::Choice => "choice",
            ParamKind::Pitch => "cents|semitones",
            ParamKind::List => "list",
        }
    }

    /// Validate a raw value without needing an input.
    pub fn validate(self, raw: &str, choices: &[&str]) -> Result<()> {
        // `auto` and `off` are accepted sentinels for measured kinds; ops resolve them
        // (the `opt_*` accessors return None for them).
        if matches!(raw, "auto" | "off")
            && matches!(self, ParamKind::Time | ParamKind::Gain | ParamKind::Freq | ParamKind::Percent | ParamKind::Ratio | ParamKind::Pitch | ParamKind::FreqRange | ParamKind::Range)
        {
            return Ok(());
        }
        match self {
            ParamKind::Flag | ParamKind::Switch => parse_bool(raw).map(|_| ()),
            ParamKind::Int => {
                raw.trim().parse::<i64>().map(|_| ()).map_err(|_| Error::usage(format!("'{raw}' is not an integer")))
            }
            ParamKind::Float => {
                raw.trim().parse::<f64>().map(|_| ()).map_err(|_| Error::usage(format!("'{raw}' is not a number")))
            }
            ParamKind::Str | ParamKind::Path | ParamKind::List => Ok(()),
            ParamKind::Time => {
                if raw.contains('/') {
                    Grid::parse(raw).map(|_| ())
                } else {
                    Time::parse(raw).map(|_| ())
                }
            }
            ParamKind::Range => TimeRange::parse(raw).map(|_| ()),
            ParamKind::Gain => Gain::parse(raw).map(|_| ()),
            ParamKind::Freq => units::parse_freq(raw).map(|_| ()),
            ParamKind::Ratio => units::parse_ratio(raw).map(|_| ()),
            ParamKind::Percent => units::parse_percent(raw).map(|_| ()),
            ParamKind::Grid => {
                if raw == "auto" || raw == "off" {
                    Ok(())
                } else {
                    Grid::parse(raw).map(|_| ())
                }
            }
            ParamKind::Pitch => units::parse_cents(raw).map(|_| ()),
            ParamKind::FreqRange => units::parse_num_range(raw, units::parse_freq).map(|_| ()),
            ParamKind::Choice => {
                if choices.is_empty() || choices.contains(&raw) {
                    Ok(())
                } else {
                    Err(Error::usage(format!("'{raw}' is not one of: {}", choices.join(", "))))
                }
            }
        }
    }
}

/// Definition of one parameter.
#[derive(Clone, Debug, Serialize)]
pub struct ParamSpec {
    /// kebab-case name; the CLI flag is `--<name>`, pipeline keys use underscores.
    pub name: &'static str,
    pub kind: ParamKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<&'static str>,
    pub help: &'static str,
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    pub choices: &'static [&'static str],
    pub repeatable: bool,
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub short: Option<char>,
    /// Allowed numeric range for documentation, e.g. "8..64".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<&'static str>,
    /// Hidden from the compact agent catalog (still documented in full reference).
    #[serde(skip)]
    pub advanced: bool,
}

/// Start a parameter definition: `param("threshold", ParamKind::Gain).default("-18dB").help("...")`.
pub fn param(name: &'static str, kind: ParamKind) -> ParamSpec {
    ParamSpec {
        name,
        kind,
        default: None,
        help: "",
        choices: &[],
        repeatable: false,
        required: false,
        short: None,
        range: None,
        advanced: false,
    }
}

impl ParamSpec {
    pub fn default(mut self, d: &'static str) -> Self {
        self.default = Some(d);
        self
    }
    pub fn help(mut self, h: &'static str) -> Self {
        self.help = h;
        self
    }
    pub fn choices(mut self, c: &'static [&'static str]) -> Self {
        self.choices = c;
        self
    }
    pub fn repeatable(mut self) -> Self {
        self.repeatable = true;
        self
    }
    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }
    pub fn short(mut self, c: char) -> Self {
        self.short = Some(c);
        self
    }
    pub fn range(mut self, r: &'static str) -> Self {
        self.range = Some(r);
        self
    }
    pub fn advanced(mut self) -> Self {
        self.advanced = true;
        self
    }
    /// Pipeline/JSON key (`retune-speed` → `retune_speed`).
    pub fn key(&self) -> String {
        self.name.replace('-', "_")
    }
}

/// Command categories (also the groups in the appendix command reference).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Inspect,
    Edit,
    Dynamics,
    Tone,
    Space,
    Restoration,
    Panning,
    Clean,
    Separate,
    Pitch,
    Midi,
    Score,
    Chains,
    Plugins,
    Mastering,
    Media,
    Models,
    Agents,
    System,
    Docs,
}

impl Category {
    pub fn title(self) -> &'static str {
        match self {
            Category::Inspect => "Inspect",
            Category::Edit => "Edit",
            Category::Dynamics => "Dynamics",
            Category::Tone => "Tone",
            Category::Space => "Space and time",
            Category::Restoration => "Restoration",
            Category::Panning => "Panning",
            Category::Clean => "Clean",
            Category::Separate => "Separate",
            Category::Pitch => "Pitch",
            Category::Midi => "MIDI",
            Category::Score => "Scores",
            Category::Chains => "Chains",
            Category::Plugins => "Plugins and sounds",
            Category::Mastering => "Mastering",
            Category::Media => "Media bridge",
            Category::Models => "Models and system",
            Category::Agents => "Agents",
            Category::System => "System",
            Category::Docs => "Docs",
        }
    }
}

/// How many positional inputs a command takes and of what kind.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct Inputs {
    pub min: usize,
    /// `None` = unbounded.
    pub max: Option<usize>,
    pub kind: InputKind,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InputKind {
    Audio,
    Midi,
    Score,
    Any,
    None,
}

impl Inputs {
    pub const NONE: Inputs = Inputs { min: 0, max: Some(0), kind: InputKind::None };
    pub const ONE_AUDIO: Inputs = Inputs { min: 1, max: Some(1), kind: InputKind::Audio };
    pub const MANY_AUDIO: Inputs = Inputs { min: 1, max: None, kind: InputKind::Audio };
    pub const ONE_MIDI: Inputs = Inputs { min: 1, max: Some(1), kind: InputKind::Midi };
    pub const MANY_MIDI: Inputs = Inputs { min: 1, max: None, kind: InputKind::Midi };
    pub const ONE_ANY: Inputs = Inputs { min: 1, max: Some(1), kind: InputKind::Any };
    pub const MANY_ANY: Inputs = Inputs { min: 1, max: None, kind: InputKind::Any };
    pub const OPTIONAL_ANY: Inputs = Inputs { min: 0, max: None, kind: InputKind::Any };
    pub fn exactly(n: usize, kind: InputKind) -> Inputs {
        Inputs { min: n, max: Some(n), kind }
    }
}

/// A worked example, used for docs and doc-tests.
#[derive(Clone, Debug, Serialize)]
pub struct Example {
    pub cmd: &'static str,
    pub explain: &'static str,
}

/// Single-input, single-output audio transform (usable as an `fx` stage).
pub type TransformFn = fn(&Args, AudioBuffer, &mut OpCtx) -> Result<AudioBuffer>;
/// General command: reads its own inputs via the context and records outputs.
pub type CustomFn = fn(&Args, &mut OpCtx) -> Result<Value>;

#[derive(Clone, Copy)]
pub enum Runner {
    /// One audio buffer in, one out. The executor handles decoding, `--mix`, `--range`
    /// limiting is per-op, encoding and output naming. Usable inside `fx` and pipelines.
    Transform(TransformFn),
    /// Full control: the command reads inputs and writes outputs through `OpCtx`.
    /// The returned JSON value becomes `result` in the envelope.
    Custom(CustomFn),
    /// The `fx` chain op, implemented by the executor itself.
    Chain,
}

impl std::fmt::Debug for Runner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Runner::Transform(_) => write!(f, "Transform"),
            Runner::Custom(_) => write!(f, "Custom"),
            Runner::Chain => write!(f, "Chain"),
        }
    }
}

/// A named preset: a saved set of parameter values.
#[derive(Clone, Debug, Serialize)]
pub struct Preset {
    pub name: &'static str,
    pub help: &'static str,
    pub values: &'static [(&'static str, &'static str)],
}

/// The definition of one command.
#[derive(Clone, Debug, Serialize)]
pub struct OpSpec {
    /// Space-separated command path, e.g. `"denoise profile"`, `"midi quantize"`.
    pub name: &'static str,
    pub category: Category,
    /// One line: what it does.
    pub summary: &'static str,
    /// Longer plain-language explanation for people.
    pub about: &'static str,
    pub params: Vec<ParamSpec>,
    pub inputs: Inputs,
    pub examples: Vec<Example>,
    pub presets: Vec<Preset>,
    /// The executor adds a generic `--mix` dry/wet when the op is a Transform that keeps
    /// the length and channel count, unless the op declares its own `mix` param.
    pub auto_mix: bool,
    /// Best-effort quality tier (labelled in docs and results).
    pub best_effort: bool,
    /// Other names the command answers to.
    pub aliases: Vec<&'static str>,
    /// When set, positional words on the command line fill this parameter instead of
    /// being input files (`ferraus schema separate`, `ferraus models pull <name>`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub positional: Option<&'static str>,
    #[serde(skip)]
    pub runner: Runner,
}

impl OpSpec {
    pub fn new(name: &'static str, category: Category, summary: &'static str, runner: Runner) -> OpSpec {
        let inputs = match runner {
            Runner::Transform(_) | Runner::Chain => Inputs::ONE_AUDIO,
            Runner::Custom(_) => Inputs::ONE_ANY,
        };
        OpSpec {
            name,
            category,
            summary,
            about: "",
            params: Vec::new(),
            inputs,
            examples: Vec::new(),
            presets: Vec::new(),
            auto_mix: matches!(runner, Runner::Transform(_)),
            best_effort: false,
            aliases: Vec::new(),
            positional: None,
            runner,
        }
    }

    pub fn transform(name: &'static str, category: Category, summary: &'static str, f: TransformFn) -> OpSpec {
        OpSpec::new(name, category, summary, Runner::Transform(f))
    }

    pub fn custom(name: &'static str, category: Category, summary: &'static str, f: CustomFn) -> OpSpec {
        OpSpec::new(name, category, summary, Runner::Custom(f))
    }

    pub fn about(mut self, a: &'static str) -> Self {
        self.about = a;
        self
    }
    pub fn param(mut self, p: ParamSpec) -> Self {
        self.params.push(p);
        self
    }
    pub fn params(mut self, ps: impl IntoIterator<Item = ParamSpec>) -> Self {
        self.params.extend(ps);
        self
    }
    pub fn inputs(mut self, i: Inputs) -> Self {
        self.inputs = i;
        self
    }
    pub fn example(mut self, cmd: &'static str, explain: &'static str) -> Self {
        self.examples.push(Example { cmd, explain });
        self
    }
    pub fn preset(
        mut self,
        name: &'static str,
        help: &'static str,
        values: &'static [(&'static str, &'static str)],
    ) -> Self {
        self.presets.push(Preset { name, help, values });
        self
    }
    pub fn no_mix(mut self) -> Self {
        self.auto_mix = false;
        self
    }
    pub fn best_effort(mut self) -> Self {
        self.best_effort = true;
        self
    }
    /// Positional words fill parameter `name` (implies no input files).
    pub fn positional(mut self, name: &'static str) -> Self {
        self.positional = Some(name);
        self.inputs = Inputs::NONE;
        self
    }
    pub fn alias(mut self, a: &'static str) -> Self {
        self.aliases.push(a);
        self
    }

    /// Identifier used for MCP tool names and schema files: `denoise profile` → `denoise_profile`.
    pub fn id(&self) -> String {
        self.name.replace([' ', '-'], "_")
    }

    pub fn is_transform(&self) -> bool {
        matches!(self.runner, Runner::Transform(_))
    }

    pub fn find_param(&self, name: &str) -> Option<&ParamSpec> {
        let norm = name.replace('_', "-");
        self.params.iter().find(|p| p.name == norm)
    }

    /// All params including the implicit ones the executor adds (`mix`, `preset`).
    pub fn effective_params(&self) -> Vec<ParamSpec> {
        let mut ps = self.params.clone();
        if self.auto_mix && self.find_param("mix").is_none() {
            ps.push(
                param("mix", ParamKind::Percent)
                    .default("100%")
                    .help("Dry/wet balance: 0% returns the input unchanged, 100% is fully processed"),
            );
        }
        if !self.presets.is_empty() && self.find_param("preset").is_none() {
            let _names: Vec<&str> = self.presets.iter().map(|p| p.name).collect();
            ps.push(
                param("preset", ParamKind::Str)
                    .help("Load a named preset (built-in or user); explicit flags override its values"),
            );
        }
        ps
    }
}

/// Parse booleans the way every surface accepts them.
pub fn parse_bool(s: &str) -> Result<bool> {
    match s.trim().to_ascii_lowercase().as_str() {
        "" | "on" | "true" | "yes" | "1" => Ok(true),
        "off" | "false" | "no" | "0" => Ok(false),
        other => Err(Error::usage(format!("'{other}' is not on/off or true/false"))),
    }
}

/// Resolved arguments for one invocation of one op: raw strings keyed by kebab-case
/// parameter name, with defaults filled in. Typed accessors use the shared unit parser.
#[derive(Clone, Debug, Default)]
pub struct Args {
    values: BTreeMap<String, Vec<String>>,
    /// Names that were given explicitly (not defaults or presets).
    explicit: Vec<String>,
}

impl Args {
    pub fn new() -> Args {
        Args::default()
    }

    /// Build args for `spec` from raw key/values (keys may use `_` or `-`), applying
    /// a preset (if `preset` is set) and then defaults. Validates names and values.
    pub fn build(
        spec: &OpSpec,
        raw: Vec<(String, String)>,
        user_presets: &dyn Fn(&str, &str) -> Option<Vec<(String, String)>>,
    ) -> Result<Args> {
        let params = spec.effective_params();
        let mut args = Args::new();
        let mut explicit: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (k, v) in raw {
            let name = k.replace('_', "-");
            let p = params.iter().find(|p| p.name == name).ok_or_else(|| {
                let known: Vec<&str> = params.iter().map(|p| p.name).collect();
                Error::usage(format!("'{}' does not accept '{}'. Accepted: {}", spec.name, name, known.join(", ")))
                    .with_hint(format!("ferraus schema {}", spec.name))
            })?;
            p.kind.validate(&v, p.choices).map_err(|e| Error::usage(format!("--{}: {}", p.name, e.message)))?;
            let entry = explicit.entry(name.clone()).or_default();
            if p.repeatable {
                entry.push(v);
            } else {
                *entry = vec![v];
            }
        }
        // presets: explicit flags override preset values. An op that declares its own
        // `preset` parameter (e.g. a plugin preset name) handles it itself.
        let own_preset = spec.params.iter().any(|p| p.name == "preset");
        if let Some(pname) = explicit.get("preset").and_then(|v| v.first()).cloned().filter(|_| !own_preset) {
            let values: Vec<(String, String)> = if let Some(p) = spec.presets.iter().find(|p| p.name == pname) {
                p.values.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
            } else if let Some(u) = user_presets(spec.name, &pname) {
                u
            } else {
                let names: Vec<&str> = spec.presets.iter().map(|p| p.name).collect();
                return Err(Error::usage(format!(
                    "unknown preset '{pname}' for '{}'; built-in presets: {}",
                    spec.name,
                    names.join(", ")
                ))
                .with_hint(format!("ferraus presets list {}", spec.name)));
            };
            for (k, v) in values {
                let name = k.replace('_', "-");
                if !explicit.contains_key(&name) {
                    if let Some(p) = params.iter().find(|p| p.name == name) {
                        p.kind.validate(&v, p.choices)?;
                        args.values.insert(name, vec![v]);
                    }
                }
            }
        }
        for (k, v) in explicit {
            args.explicit.push(k.clone());
            args.values.insert(k, v);
        }
        for p in &params {
            if !args.values.contains_key(p.name) {
                if let Some(d) = p.default {
                    args.values.insert(p.name.to_string(), vec![d.to_string()]);
                } else if p.required {
                    return Err(Error::usage(format!("'{}' requires --{}", spec.name, p.name))
                        .with_hint(format!("ferraus docs {}", spec.name)));
                }
            }
        }
        Ok(args)
    }

    /// Set a raw value directly (used by ops composing other ops and by tests).
    pub fn set(&mut self, name: &str, value: impl Into<String>) -> &mut Self {
        self.values.insert(name.replace('_', "-"), vec![value.into()]);
        self
    }

    pub fn with(mut self, name: &str, value: impl Into<String>) -> Self {
        self.set(name, value);
        self
    }

    pub fn remove(&mut self, name: &str) {
        self.values.remove(&name.replace('_', "-"));
    }

    pub fn is_explicit(&self, name: &str) -> bool {
        self.explicit.iter().any(|n| n == name)
    }

    pub fn has(&self, name: &str) -> bool {
        self.values.contains_key(name)
    }

    pub fn raw(&self, name: &str) -> Option<&str> {
        self.values.get(name).and_then(|v| v.last()).map(|s| s.as_str())
    }

    pub fn all(&self, name: &str) -> Vec<String> {
        self.values.get(name).cloned().unwrap_or_default()
    }

    /// Every value of a repeatable param, also splitting comma lists.
    pub fn list(&self, name: &str) -> Vec<String> {
        self.all(name).iter().flat_map(|v| units::split_list(v)).collect()
    }

    pub fn str(&self, name: &str) -> Option<String> {
        self.raw(name).map(|s| s.to_string())
    }

    pub fn str_or(&self, name: &str, d: &str) -> String {
        self.raw(name).unwrap_or(d).to_string()
    }

    fn need<'a>(&'a self, name: &str) -> Result<&'a str> {
        self.raw(name).ok_or_else(|| Error::usage(format!("missing --{name}")))
    }

    fn ctx<T>(&self, name: &str, r: Result<T>) -> Result<T> {
        r.map_err(|e| Error::usage(format!("--{name}: {}", e.message)))
    }

    pub fn flag(&self, name: &str) -> bool {
        self.raw(name).map(|v| parse_bool(v).unwrap_or(false)).unwrap_or(false)
    }

    /// `on`/`off` switch; missing means `default`.
    pub fn switch(&self, name: &str, default: bool) -> bool {
        self.raw(name).map(|v| parse_bool(v).unwrap_or(default)).unwrap_or(default)
    }

    pub fn f64(&self, name: &str) -> Result<f64> {
        let v = self.need(name)?;
        self.ctx(name, v.trim().parse::<f64>().map_err(|_| Error::usage(format!("'{v}' is not a number"))))
    }

    pub fn opt_f64(&self, name: &str) -> Result<Option<f64>> {
        match self.raw(name) {
            None | Some("auto") => Ok(None),
            Some(_) => self.f64(name).map(Some),
        }
    }

    pub fn int(&self, name: &str) -> Result<i64> {
        let v = self.need(name)?;
        self.ctx(name, v.trim().parse::<i64>().map_err(|_| Error::usage(format!("'{v}' is not an integer"))))
    }

    pub fn usize(&self, name: &str) -> Result<usize> {
        let v = self.int(name)?;
        if v < 0 {
            return Err(Error::usage(format!("--{name} must not be negative")));
        }
        Ok(v as usize)
    }

    pub fn time(&self, name: &str) -> Result<Time> {
        let v = self.need(name)?;
        self.ctx(name, Time::parse(v))
    }

    pub fn opt_time(&self, name: &str) -> Result<Option<Time>> {
        match self.raw(name) {
            None => Ok(None),
            Some(_) => self.time(name).map(Some),
        }
    }

    /// Duration in seconds (no `end`-relative values).
    pub fn seconds(&self, name: &str) -> Result<f64> {
        let v = self.need(name)?;
        self.ctx(name, units::parse_duration_s(v))
    }

    pub fn sync_time(&self, name: &str) -> Result<SyncTime> {
        let v = self.need(name)?;
        self.ctx(name, SyncTime::parse(v))
    }

    pub fn range(&self, name: &str) -> Result<Option<TimeRange>> {
        match self.raw(name) {
            None => Ok(None),
            Some(v) => self.ctx(name, TimeRange::parse(v)).map(Some),
        }
    }

    pub fn ranges(&self, name: &str) -> Result<Vec<TimeRange>> {
        self.all(name).iter().map(|v| self.ctx(name, TimeRange::parse(v))).collect()
    }

    pub fn gain(&self, name: &str) -> Result<Gain> {
        let v = self.need(name)?;
        self.ctx(name, Gain::parse(v))
    }

    /// Decibels.
    pub fn db(&self, name: &str) -> Result<f64> {
        Ok(self.gain(name)?.db())
    }

    pub fn opt_db(&self, name: &str) -> Result<Option<f64>> {
        match self.raw(name) {
            None | Some("auto") => Ok(None),
            Some(_) => self.db(name).map(Some),
        }
    }

    pub fn freq(&self, name: &str) -> Result<f64> {
        let v = self.need(name)?;
        self.ctx(name, units::parse_freq(v))
    }

    pub fn opt_freq(&self, name: &str) -> Result<Option<f64>> {
        match self.raw(name) {
            None | Some("auto") | Some("off") => Ok(None),
            Some(_) => self.freq(name).map(Some),
        }
    }

    pub fn freq_range(&self, name: &str) -> Result<(f64, f64)> {
        let v = self.need(name)?;
        self.ctx(name, units::parse_num_range(v, units::parse_freq))
    }

    pub fn ratio(&self, name: &str) -> Result<f64> {
        let v = self.need(name)?;
        self.ctx(name, units::parse_ratio(v))
    }

    /// Fraction (80% → 0.8).
    pub fn percent(&self, name: &str) -> Result<f64> {
        let v = self.need(name)?;
        self.ctx(name, units::parse_percent(v))
    }

    pub fn grid(&self, name: &str) -> Result<Option<Grid>> {
        match self.raw(name) {
            None | Some("auto") | Some("off") => Ok(None),
            Some(v) => self.ctx(name, Grid::parse(v)).map(Some),
        }
    }

    pub fn cents(&self, name: &str) -> Result<f64> {
        let v = self.need(name)?;
        self.ctx(name, units::parse_cents(v))
    }

    pub fn semitones(&self, name: &str) -> Result<f64> {
        let v = self.need(name)?;
        self.ctx(name, units::parse_semitones(v))
    }

    pub fn path(&self, name: &str) -> Option<PathBuf> {
        self.raw(name).map(PathBuf::from)
    }

    pub fn choice(&self, name: &str) -> String {
        self.raw(name).unwrap_or("").to_string()
    }

    /// Resolved parameters as JSON (for the envelope and sidecar).
    pub fn to_json(&self) -> Map<String, Value> {
        let mut m = Map::new();
        for (k, v) in &self.values {
            let key = k.replace('-', "_");
            if v.len() == 1 {
                m.insert(key, Value::String(v[0].clone()));
            } else {
                m.insert(key, Value::Array(v.iter().cloned().map(Value::String).collect()));
            }
        }
        m
    }

    /// Raw key/value pairs (for re-building, replay, and pipelines).
    pub fn pairs(&self) -> Vec<(String, String)> {
        self.values.iter().flat_map(|(k, vs)| vs.iter().map(move |v| (k.clone(), v.clone()))).collect()
    }
}

/// Convert a JSON object (MCP, pipeline YAML) into raw key/value pairs.
pub fn json_to_pairs(v: &Value) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    let obj = match v {
        Value::Object(o) => o,
        Value::Null => return Ok(out),
        _ => return Err(Error::usage("arguments must be a JSON object")),
    };
    for (k, val) in obj {
        match val {
            Value::Null => {}
            Value::Array(items) => {
                for it in items {
                    out.push((k.clone(), scalar_to_string(it)?));
                }
            }
            other => out.push((k.clone(), scalar_to_string(other)?)),
        }
    }
    Ok(out)
}

fn scalar_to_string(v: &Value) -> Result<String> {
    Ok(match v {
        Value::String(s) => s.clone(),
        Value::Bool(b) => {
            if *b {
                "true".into()
            } else {
                "false".into()
            }
        }
        Value::Number(n) => n.to_string(),
        Value::Null => String::new(),
        Value::Array(a) => a.iter().map(scalar_to_string).collect::<Result<Vec<_>>>()?.join(","),
        Value::Object(_) => serde_json::to_string(v)?,
    })
}

/// Parse an fx stage string: `denoise:auto,reduction=12dB` or `fade:in=1s,out=2s`.
pub fn parse_stage_string(s: &str) -> Result<(String, Vec<(String, String)>)> {
    let (name, rest) = match s.split_once(':') {
        Some((n, r)) => (n.trim(), r),
        None => (s.trim(), ""),
    };
    if name.is_empty() {
        return Err(Error::usage(format!("empty effect name in '{s}'")));
    }
    let mut pairs = Vec::new();
    // split on commas that start a new key (key=...), so values may contain commas
    let mut current: Option<(String, String)> = None;
    for part in rest.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let looks_like_key = part
            .split_once('=')
            .map(|(k, _)| k.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
            .unwrap_or(false)
            || (!part.contains('=')
                && part.starts_with(|c: char| c.is_ascii_alphabetic())
                && part.chars().all(|c| c.is_ascii_alphabetic() || c == '-' || c == '_'));
        if looks_like_key {
            if let Some(kv) = current.take() {
                pairs.push(kv);
            }
            match part.split_once('=') {
                Some((k, v)) => current = Some((k.trim().to_string(), v.trim().to_string())),
                None => current = Some((part.to_string(), "true".to_string())),
            }
        } else if let Some((_, v)) = current.as_mut() {
            v.push(',');
            v.push_str(part);
        } else {
            return Err(Error::usage(format!("cannot parse effect parameters '{rest}'")));
        }
    }
    if let Some(kv) = current.take() {
        pairs.push(kv);
    }
    Ok((name.to_string(), pairs))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy(_: &Args, b: AudioBuffer, _: &mut OpCtx) -> Result<AudioBuffer> {
        Ok(b)
    }

    fn spec() -> OpSpec {
        OpSpec::transform("compress", Category::Dynamics, "Compressor", dummy)
            .param(param("threshold", ParamKind::Gain).default("-18dB"))
            .param(param("ratio", ParamKind::Ratio).default("3:1"))
            .param(param("detector", ParamKind::Choice).choices(&["rms", "peak"]).default("rms"))
            .preset("vocal", "", &[("threshold", "-20dB"), ("ratio", "4:1")])
    }

    #[test]
    fn defaults_and_explicit() {
        let s = spec();
        let a = Args::build(&s, vec![("ratio".into(), "4:1".into())], &|_, _| None).unwrap();
        assert_eq!(a.ratio("ratio").unwrap(), 4.0);
        assert_eq!(a.db("threshold").unwrap(), -18.0);
        assert_eq!(a.percent("mix").unwrap(), 1.0);
        assert!(a.is_explicit("ratio"));
        assert!(!a.is_explicit("threshold"));
    }

    #[test]
    fn preset_then_override() {
        let s = spec();
        let a = Args::build(&s, vec![("preset".into(), "vocal".into()), ("ratio".into(), "2:1".into())], &|_, _| None)
            .unwrap();
        assert_eq!(a.db("threshold").unwrap(), -20.0);
        assert_eq!(a.ratio("ratio").unwrap(), 2.0);
    }

    #[test]
    fn rejects_unknown_and_bad_values() {
        let s = spec();
        assert!(Args::build(&s, vec![("nope".into(), "1".into())], &|_, _| None).is_err());
        assert!(Args::build(&s, vec![("detector".into(), "lol".into())], &|_, _| None).is_err());
        assert!(Args::build(&s, vec![("threshold".into(), "loud".into())], &|_, _| None).is_err());
    }

    #[test]
    fn stage_strings() {
        let (n, p) = parse_stage_string("denoise:auto").unwrap();
        assert_eq!(n, "denoise");
        assert_eq!(p, vec![("auto".to_string(), "true".to_string())]);
        let (n, p) = parse_stage_string("fade:in=1s,out=2s").unwrap();
        assert_eq!(n, "fade");
        assert_eq!(p.len(), 2);
        let (_, p) = parse_stage_string("eq:band=peak:1kHz:+3dB,band=highpass:80Hz:0dB").unwrap();
        assert_eq!(p.len(), 2);
        let (_, p) = parse_stage_string("mix:gains=0dB,-3dB").unwrap();
        assert_eq!(p, vec![("gains".to_string(), "0dB,-3dB".to_string())]);
        let (n, p) = parse_stage_string("reverse").unwrap();
        assert_eq!(n, "reverse");
        assert!(p.is_empty());
    }
}
