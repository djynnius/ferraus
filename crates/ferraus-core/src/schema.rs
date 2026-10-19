//! JSON Schema (draft 2020-12) generated from op specs, for `ferraus schema`, MCP tool
//! definitions, and validation of agent-supplied arguments.

use crate::op::{InputKind, OpSpec, ParamKind, ParamSpec};
use serde_json::{json, Map, Value};

pub const DIALECT: &str = "https://json-schema.org/draft/2020-12/schema";

fn param_schema(p: &ParamSpec) -> Value {
    let mut s = Map::new();
    let desc = {
        let mut d = p.help.to_string();
        if let Some(r) = p.range {
            d.push_str(&format!(" Range: {r}."));
        }
        d.push_str(&format!(" Type: {}.", p.kind.type_name()));
        d
    };
    match p.kind {
        ParamKind::Flag | ParamKind::Switch => {
            s.insert("oneOf".into(), json!([{"type": "boolean"}, {"type": "string", "enum": ["on", "off", "true", "false"]}]));
        }
        ParamKind::Int => {
            s.insert(
                "oneOf".into(),
                json!([{"type": "integer"}, {"type": "string", "pattern": "^-?[0-9]+$"}]),
            );
        }
        ParamKind::Float => {
            s.insert(
                "oneOf".into(),
                json!([{"type": "number"}, {"type": "string"}]),
            );
        }
        ParamKind::Choice if !p.choices.is_empty() => {
            s.insert("type".into(), json!("string"));
            s.insert("enum".into(), json!(p.choices));
        }
        ParamKind::Percent | ParamKind::Ratio => {
            s.insert(
                "oneOf".into(),
                json!([{"type": "number"}, {"type": "string"}]),
            );
        }
        _ => {
            s.insert("type".into(), json!("string"));
        }
    }
    if let Some(ex) = kind_example(p.kind) {
        s.insert("examples".into(), json!([ex]));
    }
    s.insert("description".into(), json!(desc));
    if let Some(d) = p.default {
        s.insert("default".into(), json!(d));
    }
    s.insert("x-ferraus-kind".into(), json!(p.kind));
    if p.repeatable {
        let item = Value::Object(s.clone());
        let mut arr = Map::new();
        arr.insert("description".into(), json!(format!("{desc} Repeatable.")));
        arr.insert(
            "oneOf".into(),
            json!([item, {"type": "array", "items": item}]),
        );
        return Value::Object(arr);
    }
    Value::Object(s)
}

fn kind_example(k: ParamKind) -> Option<&'static str> {
    Some(match k {
        ParamKind::Time => "1.5s",
        ParamKind::Range => "0s..2s",
        ParamKind::Gain => "-6dB",
        ParamKind::Freq => "1kHz",
        ParamKind::Ratio => "4:1",
        ParamKind::Percent => "50%",
        ParamKind::Grid => "1/16",
        ParamKind::Pitch => "+200cents",
        _ => return None,
    })
}

/// Schema for an op's arguments (inputs, output, and params).
pub fn args_schema(spec: &OpSpec) -> Value {
    let mut props = Map::new();
    let mut required = Vec::new();
    if spec.inputs.kind != InputKind::None {
        let mut inp = json!({
            "type": "array",
            "items": {"type": "string"},
            "minItems": spec.inputs.min,
            "description": format!("Input file paths ({}); globs allowed; '-' reads stdin", match spec.inputs.kind {
                InputKind::Audio => "audio or video files",
                InputKind::Midi => "MIDI files",
                InputKind::Score => "notation files",
                _ => "files",
            }),
        });
        if let Some(m) = spec.inputs.max {
            if m > 1 {
                inp["maxItems"] = json!(m);
            }
        }
        props.insert("inputs".into(), inp);
        if spec.inputs.min > 0 {
            required.push("inputs".to_string());
        }
    }
    props.insert("output".into(), json!({"type": "string", "description": "Output path or template ({name}, {stem}, {index}, {ext}); '-' for stdout"}));
    for p in spec.effective_params() {
        props.insert(p.key(), param_schema(&p));
        if p.required {
            required.push(p.key());
        }
    }
    json!({
        "$schema": DIALECT,
        "$id": format!("https://djynnius.github.io/ferraus/schema/v1/{}.args.json", spec.id()),
        "title": format!("ferraus {} arguments", spec.name),
        "description": spec.summary,
        "type": "object",
        "properties": props,
        "required": required,
        "additionalProperties": false,
    })
}

/// Schema for the result envelope (shared by every command).
pub fn result_schema(spec: &OpSpec) -> Value {
    json!({
        "$schema": DIALECT,
        "$id": format!("https://djynnius.github.io/ferraus/schema/v1/{}.result.json", spec.id()),
        "title": format!("ferraus {} result", spec.name),
        "type": "object",
        "required": ["ok", "command", "ferraus_version", "schema_version", "inputs", "outputs", "params", "warnings", "timing_ms"],
        "properties": {
            "ok": {"type": "boolean"},
            "command": {"const": spec.name},
            "ferraus_version": {"type": "string"},
            "schema_version": {"const": crate::envelope::SCHEMA_VERSION},
            "inputs": {"type": "array", "items": {"type": "object", "required": ["path"], "properties": {
                "path": {"type": "string"}, "duration_s": {"type": "number"}, "sample_rate": {"type": "integer"}, "channels": {"type": "integer"}}}},
            "outputs": {"type": "array", "items": {"type": "object", "required": ["path"], "properties": {
                "path": {"type": "string"}, "role": {"type": "string"}, "duration_s": {"type": "number"}}}},
            "params": {"type": "object", "description": "Every parameter after defaults, presets, and run-time detection"},
            "models": {"type": "array", "items": {"type": "object", "properties": {"name": {"type": "string"}, "version": {"type": "string"}, "sha256": {"type": "string"}}}},
            "metrics": {"type": "object"},
            "warnings": {"type": "array", "items": {"type": "object", "required": ["code", "message"], "properties": {"code": {"type": "string"}, "message": {"type": "string"}}}},
            "result": {"description": "Command-specific result data"},
            "error": {"type": "object", "required": ["code", "message", "exit_code"], "properties": {
                "code": {"type": "string"}, "message": {"type": "string"}, "hint": {"type": "string"}, "exit_code": {"type": "integer"}}},
            "timing_ms": {"type": "integer"},
            "dry_run": {"type": "boolean"}
        }
    })
}

/// Combined schema document for `ferraus schema <command>`.
pub fn command_schema(spec: &OpSpec) -> Value {
    json!({
        "command": spec.name,
        "summary": spec.summary,
        "category": spec.category,
        "best_effort": spec.best_effort,
        "fx_stage": spec.is_transform(),
        "arguments": args_schema(spec),
        "result": result_schema(spec),
    })
}

/// Validate a JSON argument object against an op (names, types via the unit parser).
pub fn validate_args(spec: &OpSpec, args: &Value) -> crate::error::Result<()> {
    let pairs = crate::op::json_to_pairs(args)?;
    let filtered: Vec<(String, String)> = pairs
        .into_iter()
        .filter(|(k, _)| k != "inputs" && k != "output")
        .collect();
    crate::op::Args::build(spec, filtered, &|_, _| None).map(|_| ())
}
