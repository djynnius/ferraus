//! The editable mastering plan: `{"version":1, "target":..., "stages":[{"op":..,"args":{..}}]}`.
//! `master plan` writes one, `master --plan plan.json` validates every stage through the
//! registry and runs it.

use crate::targets::Target;
use ferraus_core::exec::run_transform_pairs;
use ferraus_core::op::json_to_pairs;
use ferraus_core::prelude::*;
use ferraus_core::{Args, Registry};
use serde::{Deserialize, Serialize};
use serde_json::Map;
use std::path::Path;

pub const PLAN_VERSION: u32 = 1;

/// One stage: a registered transform with its arguments.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Stage {
    /// Chain step this stage belongs to (`preflight`, `corrective_eq`, `dynamics`, `stereo`,
    /// `color`, `speech`, `loudness`). Informational.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stage: String,
    /// The command the stage runs (any `fx`-capable command).
    pub op: String,
    /// Arguments as JSON (strings; repeated parameters as arrays).
    #[serde(default)]
    pub args: Map<String, Value>,
    /// Why the value was chosen. Informational.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub why: String,
}

impl Stage {
    pub fn new(stage: &str, op: &str, args: Vec<(&str, String)>, why: impl Into<String>) -> Stage {
        let mut m = Map::new();
        for (k, v) in args {
            let key = k.replace('-', "_");
            match m.get_mut(&key) {
                Some(Value::Array(a)) => a.push(Value::String(v)),
                Some(old) => {
                    let prev = old.take();
                    *old = Value::Array(vec![prev, Value::String(v)]);
                }
                None => {
                    m.insert(key, Value::String(v));
                }
            }
        }
        Stage { stage: stage.into(), op: op.into(), args: m, why: why.into() }
    }

    pub fn pairs(&self) -> Result<Vec<(String, String)>> {
        json_to_pairs(&Value::Object(self.args.clone()))
    }

    /// The `fx` stage string for this stage (`compress:ratio=1.5:1,...`).
    pub fn fx_string(&self) -> String {
        let pairs = self.pairs().unwrap_or_default();
        if pairs.is_empty() {
            return self.op.clone();
        }
        let body: Vec<String> = pairs.iter().map(|(k, v)| format!("{}={v}", k.replace('_', "-"))).collect();
        format!("{}:{}", self.op, body.join(","))
    }
}

/// How the genre was chosen.
#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
pub struct GenreChoice {
    pub name: String,
    /// `explicit`, `auto`, or `reference` (a reference track replaces the curve).
    pub source: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
}

/// How the intensity was chosen.
#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
pub struct IntensityChoice {
    pub name: String,
    /// Upper bound on limiter gain reduction (dB).
    pub limiter_max_gain_reduction_db: f64,
    /// `explicit`, `default`, or `looks_mastered`.
    pub source: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub why: String,
}

/// The full plan.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Plan {
    pub version: u32,
    pub target: Target,
    #[serde(default)]
    pub genre: GenreChoice,
    #[serde(default)]
    pub intensity: IntensityChoice,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_amount: Option<f64>,
    pub stages: Vec<Stage>,
    /// Plain-language notes: skipped steps, guards that engaged, deviations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    /// What the planner measured when it simulated the chain on this input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected: Option<Value>,
}

impl Plan {
    /// Parse a plan file (a bare plan, or a `--json` envelope whose `result` is a plan).
    pub fn from_json_bytes(bytes: &[u8]) -> Result<Plan> {
        let v: Value =
            serde_json::from_slice(bytes).map_err(|e| Error::usage(format!("plan is not valid JSON: {e}")))?;
        let v = match v.get("result") {
            Some(r) if r.get("stages").is_some() => r.clone(),
            _ => match v.get("result").and_then(|r| r.get("plan")) {
                Some(p) if p.get("stages").is_some() => p.clone(),
                _ => match v.get("plan") {
                    Some(p) if p.get("stages").is_some() => p.clone(),
                    _ => v,
                },
            },
        };
        let plan: Plan = serde_json::from_value(v).map_err(|e| {
            Error::usage(format!("not a ferraus master plan: {e}"))
                .with_hint("ferraus master plan mix.wav --json > plan.json")
        })?;
        Ok(plan)
    }

    pub fn load(path: &Path) -> Result<Plan> {
        let bytes = std::fs::read(path).map_err(|e| {
            Error::new(ErrorKind::InputMissing, "input_not_found", format!("cannot read plan {}: {e}", path.display()))
        })?;
        Plan::from_json_bytes(&bytes)
    }

    /// Check the version, and that every stage names a registered `fx`-capable command
    /// whose arguments parse.
    pub fn validate(&self, registry: &Registry) -> Result<()> {
        if self.version != PLAN_VERSION {
            return Err(Error::usage(format!(
                "plan version {} is not supported (this ferraus reads version {PLAN_VERSION})",
                self.version
            )));
        }
        if self.stages.is_empty() {
            return Err(Error::usage("the plan has no stages"));
        }
        for (i, s) in self.stages.iter().enumerate() {
            let spec = registry.require(&s.op).map_err(|e| {
                let mut e2 = Error::usage(format!("plan stage {} ('{}'): {}", i + 1, s.op, e.message));
                e2.hint = e.hint;
                e2
            })?;
            if !spec.is_transform() {
                return Err(Error::usage(format!(
                    "plan stage {} ('{}') is not a single-input audio effect and cannot run in a plan",
                    i + 1,
                    s.op
                )));
            }
            let pairs = s.pairs()?;
            Args::build(spec, pairs, &|_, _| None).map_err(|e| {
                Error::usage(format!("plan stage {} ('{}'): {}", i + 1, s.op, e.message))
                    .with_hint(format!("ferraus schema {}", spec.name))
            })?;
        }
        if !(-40.0..=0.0).contains(&self.target.lufs) || !(-12.0..=0.0).contains(&self.target.true_peak_dbtp) {
            return Err(Error::usage("plan target loudness or true-peak ceiling is out of range"));
        }
        Ok(())
    }
}

/// What one stage did when it ran.
#[derive(Clone, Debug, Serialize)]
pub struct StageTrace {
    pub op: String,
    #[serde(skip_serializing_if = "Map::is_empty")]
    pub metrics: Map<String, Value>,
}

/// Run one stage on a buffer in a child context; returns the output and the stage's metrics.
/// Warnings whose code passes `keep` are merged into `ctx` (`None` drops all of them).
pub fn run_stage_filtered(
    ctx: &mut OpCtx,
    stage: &Stage,
    buf: AudioBuffer,
    keep: Option<&dyn Fn(&str) -> bool>,
) -> Result<(AudioBuffer, Map<String, Value>)> {
    let registry = ctx.registry.ok_or_else(|| Error::internal("no registry in context"))?;
    let pairs = stage.pairs()?;
    let mut child = ctx.child(&stage.op, ctx.inputs.clone());
    let out = run_transform_pairs(registry, &|_, _| None, &stage.op, pairs, buf, &mut child)?;
    let metrics = std::mem::take(&mut child.metrics);
    for (k, v) in std::mem::take(&mut child.notes) {
        ctx.notes.entry(format!("{}.{k}", stage.op.replace(' ', "_"))).or_insert(v);
    }
    match keep {
        Some(f) => child.warnings.retain(|w| f(&w.code)),
        None => child.warnings.clear(),
    }
    for w in std::mem::take(&mut child.warnings) {
        if !ctx.warnings.iter().any(|x| x.code == w.code && x.message == w.message) {
            ctx.warnings.push(w);
        }
    }
    ctx.absorb(child);
    Ok((out, metrics))
}

/// [`run_stage_filtered`] keeping every warning.
pub fn run_stage(ctx: &mut OpCtx, stage: &Stage, buf: AudioBuffer) -> Result<(AudioBuffer, Map<String, Value>)> {
    run_stage_filtered(ctx, stage, buf, Some(&|_| true))
}

/// Run every stage of a validated plan.
pub fn run_plan(ctx: &mut OpCtx, plan: &Plan, mut buf: AudioBuffer) -> Result<(AudioBuffer, Vec<StageTrace>)> {
    let mut trace = Vec::new();
    let n = plan.stages.len();
    for (i, s) in plan.stages.iter().enumerate() {
        ctx.check_cancel()?;
        ctx.progress.stage(&s.op, i + 1, n);
        // a gain stage feeding the limiter may exceed 0 dBFS in float; that is not clipping
        let feeds_limiter = s.op == "gain" && plan.stages.get(i + 1).map(|n| n.op == "limit").unwrap_or(false);
        let keep = move |code: &str| !(feeds_limiter && code == "clipping");
        let (out, metrics) = run_stage_filtered(ctx, s, buf, Some(&keep))?;
        buf = out;
        trace.push(StageTrace { op: s.op.clone(), metrics });
        ctx.set_progress((i + 1) as f64 / n as f64);
    }
    Ok((buf, trace))
}

/// Run a plan, holding the plan target's true-peak ceiling as measured by the analysis
/// meter: when the plan ends in `limit` and the result still peaks over the target's
/// ceiling (meters differ slightly in interpolation), the final limiter is re-run with its
/// ceiling lowered by the excess. Returns a note when the guard engaged.
pub fn run_plan_guarded(
    ctx: &mut OpCtx,
    plan: &Plan,
    buf: AudioBuffer,
) -> Result<(AudioBuffer, Vec<StageTrace>, Option<String>)> {
    let ends_in_limit = plan.stages.last().map(|s| s.op == "limit").unwrap_or(false);
    if !ends_in_limit {
        let (out, trace) = run_plan(ctx, plan, buf)?;
        return Ok((out, trace, None));
    }
    let n = plan.stages.len();
    let head = Plan { stages: plan.stages[..n - 1].to_vec(), ..plan.clone() };
    let (pre, mut trace) = if head.stages.is_empty() { (buf, Vec::new()) } else { run_plan(ctx, &head, buf)? };
    let mut stage = plan.stages[n - 1].clone();
    let want = plan.target.true_peak_dbtp;
    let mut note = None;
    for attempt in 0..4 {
        let (out, metrics) = run_stage(ctx, &stage, pre.clone())?;
        let tp = ferraus_analysis::loudness::true_peak_db(&out);
        let ceiling =
            stage.args.get("ceiling").and_then(|v| v.as_str()).and_then(|s| units::parse_db(s).ok()).unwrap_or(want);
        if tp <= want || attempt == 3 {
            trace.push(StageTrace { op: stage.op.clone(), metrics });
            return Ok((out, trace, note));
        }
        let lowered = ceiling - (tp - want + 0.02);
        note = Some(format!(
            "true-peak guard: the limiter at {ceiling:.2} dBTP left a {tp:.2} dBTP peak on the 4x meter, so it re-ran at {lowered:.2} dBTP"
        ));
        stage.args.insert(
            "ceiling".into(),
            Value::String(format!("{}TP", crate::util::fmt_db(crate::util::round_to(lowered, 2)))),
        );
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_args_round_trip() {
        let s = Stage::new(
            "corrective_eq",
            "eq",
            vec![
                ("band", "peak:1kHz:+1dB:q=4.3".into()),
                ("band", "peak:2kHz:-1dB".into()),
                ("linear-phase", "true".into()),
            ],
            "",
        );
        let p = s.pairs().unwrap();
        assert_eq!(p.iter().filter(|(k, _)| k == "band").count(), 2);
        assert!(s.fx_string().starts_with("eq:band=peak:1kHz"));
        assert!(s.args.contains_key("linear_phase"));
    }

    #[test]
    fn parses_envelope_or_bare_plan() {
        let plan = json!({"version": 1, "target": {"name": "streaming", "lufs": -14.0, "true_peak_dbtp": -1.0},
                          "stages": [{"op": "dc-remove", "args": {"freq": "5Hz"}}]});
        let p = Plan::from_json_bytes(plan.to_string().as_bytes()).unwrap();
        assert_eq!(p.stages[0].op, "dc-remove");
        let env = json!({"ok": true, "result": plan});
        let p2 = Plan::from_json_bytes(env.to_string().as_bytes()).unwrap();
        assert_eq!(p, p2);
        assert!(Plan::from_json_bytes(b"{\"nope\":1}").is_err());
    }
}
