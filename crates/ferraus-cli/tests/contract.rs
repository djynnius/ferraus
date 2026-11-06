//! Contract tests: schema snapshots, the agent catalog budget, the Audition coverage map,
//! and every `ferraus ...` example in the PRD and in op specs parsing against the real CLI.
//!
//! Update snapshots after an intentional contract change with
//! `UPDATE_SNAPSHOTS=1 cargo test -p ferraus-cli --test contract`.

use ferraus_core::op::Args;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn prd() -> String {
    let root = repo_root();
    let f = std::fs::read_dir(&root)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.file_name().unwrap().to_string_lossy().starts_with("PRD"))
        .expect("PRD file at repo root");
    std::fs::read_to_string(f).unwrap()
}

fn updating() -> bool {
    std::env::var("UPDATE_SNAPSHOTS").map(|v| v == "1").unwrap_or(false)
}

#[test]
fn schema_snapshots() {
    let reg = ferraus_cli::registry();
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots/schema");
    std::fs::create_dir_all(&dir).unwrap();
    let mut failures = Vec::new();
    let mut names = Vec::new();
    for op in reg.ops() {
        names.push(op.name.to_string());
        let text = serde_json::to_string_pretty(&ferraus_core::schema::command_schema(op)).unwrap() + "\n";
        let path = dir.join(format!("{}.json", op.id()));
        if updating() {
            std::fs::write(&path, &text).unwrap();
        } else {
            match std::fs::read_to_string(&path) {
                Ok(old) if old == text => {}
                Ok(_) => failures.push(format!("{} changed", op.name)),
                Err(_) => failures.push(format!("{} has no snapshot", op.name)),
            }
        }
    }
    names.sort();
    let index = names.join("\n") + "\n";
    let ipath = dir.join("_commands.txt");
    if updating() {
        std::fs::write(&ipath, &index).unwrap();
    } else if std::fs::read_to_string(&ipath).ok().as_deref() != Some(index.as_str()) {
        failures.push("the command list changed".into());
    }
    assert!(
        failures.is_empty(),
        "schema contract changed (re-run with UPDATE_SNAPSHOTS=1 if intended):\n{}",
        failures.join("\n")
    );
}

#[test]
fn agent_catalog_under_budget_and_in_sync() {
    let reg = ferraus_cli::registry();
    let catalog = ferraus_core::builtin::agent_catalog(&reg);
    let text: String = catalog.iter().map(|(_, l)| format!("{l}\n")).collect();
    // ~4 characters per token for English/code text
    let approx_tokens = text.len() / 4;
    assert!(approx_tokens < 4000, "describe --agent is ~{approx_tokens} tokens; keep summaries shorter");
    // docs/agents/catalog.md, AGENTS.md section, and the skill are generated from this
    let generated = render_markdown_catalog(&reg);
    let path = repo_root().join("docs/agents/catalog.md");
    if updating() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &generated).unwrap();
    } else {
        let on_disk = std::fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(
            on_disk, generated,
            "docs/agents/catalog.md is stale; run UPDATE_SNAPSHOTS=1 cargo test -p ferraus-cli --test contract"
        );
    }
}

fn render_markdown_catalog(reg: &ferraus_core::Registry) -> String {
    let io = ferraus_codec::CodecIo;
    let progress = ferraus_core::ctx::NoProgress;
    let mut ctx = ferraus_core::OpCtx::new("describe", vec![], Default::default(), &io, &progress);
    ctx.registry = Some(reg);
    let spec = reg.get("describe").unwrap();
    let args =
        Args::build(spec, vec![("agent".into(), "true".into()), ("format".into(), "markdown".into())], &|_, _| None)
            .unwrap();
    let f = match spec.runner {
        ferraus_core::Runner::Custom(f) => f,
        _ => unreachable!(),
    };
    f(&args, &mut ctx).unwrap()["text"].as_str().unwrap().to_string()
}

/// Resolve the longest command name at the start of `words`.
fn resolve_command<'a>(reg: &'a ferraus_core::Registry, words: &[&str]) -> Option<(&'a ferraus_core::OpSpec, usize)> {
    for n in (1..=words.len().min(3)).rev() {
        let name = words[..n].join(" ");
        // aliases (`self-update` for `update`) are real commands too
        if let Some(op) = reg.get(&name).filter(|o| o.name == name || o.aliases.contains(&name.as_str())) {
            return Some((op, n));
        }
    }
    None
}

#[test]
fn audition_coverage_map_commands_exist() {
    let reg = ferraus_cli::registry();
    let text = prd();
    let start = text.find("**Coverage map**").expect("coverage map");
    let end = start + text[start..].find("Sources: Adobe Audition").expect("end of map");
    let mut missing = Vec::new();
    for line in text[start..end].lines().filter(|l| l.starts_with('|')) {
        let cols: Vec<&str> = line.split('|').map(|c| c.trim()).collect();
        if cols.len() < 4 || cols[1] == "Audition effect" || cols[1].starts_with("---") {
            continue;
        }
        let cell = cols[2];
        if cell.contains("not in v1") {
            continue;
        }
        for span in cell.split('`').skip(1).step_by(2) {
            let words: Vec<&str> = span.split_whitespace().take_while(|w| !w.starts_with('-')).collect();
            if words.is_empty() {
                continue;
            }
            let group = words.join(" ") + " ";
            let is_group = reg.ops().iter().any(|o| o.name.starts_with(&group));
            if resolve_command(&reg, &words).is_none() && !is_group {
                missing.push(format!("{} (for {})", span, cols[1]));
            }
        }
    }
    assert!(missing.is_empty(), "coverage-map commands missing from the CLI:\n{}", missing.join("\n"));
}

/// Split a command line like a shell would (quotes, no expansion).
fn shell_split(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut any = false;
    for c in s.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => cur.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                any = true;
            }
            None if c.is_whitespace() => {
                if !cur.is_empty() || any {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            None => cur.push(c),
        }
    }
    if !cur.is_empty() || any {
        out.push(cur);
    }
    out
}

/// Check one example: it parses with clap and its op args validate.
fn check_example(reg: &ferraus_core::Registry, cmd: &str) -> Result<(), String> {
    let cmd = cmd.split(" #").next().unwrap().trim();
    let cmd = cmd.split(" > ").next().unwrap().split(" | ").next().unwrap().split(" && ").next().unwrap().trim();
    let argv = shell_split(cmd);
    if argv.first().map(|s| s.as_str()) != Some("ferraus") {
        return Ok(());
    }
    let words: Vec<&str> = argv[1..].iter().map(|s| s.as_str()).take_while(|w| !w.starts_with('-')).collect();
    let Some((op, nwords)) = resolve_command(reg, &words) else {
        return Err(format!("unknown command in: {cmd}"));
    };
    let m = ferraus_cli::cli::build_command(reg)
        .try_get_matches_from(&argv)
        .map_err(|e| format!("{cmd}\n  {}", e.to_string().lines().next().unwrap_or("")))?;
    let _ = m;
    // validate op values through Args::build (units, choices)
    let mut pairs = Vec::new();
    if let Some(pos) = op.positional {
        let rest: Vec<&str> = words[nwords..].to_vec();
        if !rest.is_empty() {
            pairs.push((pos.to_string(), rest.join(" ")));
        }
    }
    let mut i = 1;
    while i < argv.len() {
        let a = &argv[i];
        if a == "--" {
            break;
        }
        if let Some(flag) = a.strip_prefix("--") {
            let (name, inline) = match flag.split_once('=') {
                Some((n, v)) => (n.to_string(), Some(v.to_string())),
                None => (flag.to_string(), None),
            };
            if let Some(p) = op.effective_params().into_iter().find(|p| p.name == name) {
                if matches!(p.kind, ferraus_core::ParamKind::Flag) {
                    pairs.push((name, "true".to_string()));
                } else if let Some(v) = inline {
                    pairs.push((name, v));
                } else if let Some(v) = argv.get(i + 1) {
                    if p.kind == ferraus_core::ParamKind::Switch && v.starts_with('-') {
                        pairs.push((name, "on".into()));
                    } else {
                        pairs.push((name, v.clone()));
                        i += 1;
                    }
                }
            } else if !ferraus_cli::cli::GLOBAL_FLAGS.contains(&name.as_str()) {
                return Err(format!("{cmd}\n  unknown flag --{name}"));
            } else if !matches!(
                name.as_str(),
                "json"
                    | "dry-run"
                    | "overwrite"
                    | "in-place"
                    | "deterministic"
                    | "offline"
                    | "yes"
                    | "explain"
                    | "force"
                    | "quiet"
                    | "verbose"
                    | "async"
                    | "sidecar"
                    | "no-sidecar"
            ) {
                i += 1;
            }
        }
        i += 1;
    }
    let bare = argv.len() == 1 + nwords;
    match Args::build(op, pairs, &|_, _| None) {
        Ok(args) if matches!(op.runner, ferraus_core::Runner::Chain) && !bare => {
            ferraus_core::pipeline::validate_chain(&args, reg).map_err(|e| format!("{cmd}\n  {}", e.message))
        }
        Ok(_) => Ok(()),
        // a bare command name in prose ("ferraus jobs status") is not an invocation
        Err(e) if bare && e.message.contains("requires") => Ok(()),
        Err(e) => Err(format!("{cmd}\n  {}", e.message)),
    }
}

/// Examples written into op specs (and so into --help and the docs) must all be valid.
#[test]
fn op_spec_examples_parse() {
    let reg = ferraus_cli::registry();
    let mut bad = Vec::new();
    for op in reg.ops() {
        for e in &op.examples {
            if e.cmd.contains('<') || e.cmd.contains("...") {
                continue;
            }
            if let Err(msg) = check_example(&reg, e.cmd) {
                bad.push(msg);
            }
        }
    }
    assert!(bad.is_empty(), "{} invalid op-spec examples:\n{}", bad.len(), bad.join("\n"));
}

/// Every concrete `ferraus ...` invocation in the PRD validates against the CLI.
#[test]
fn prd_examples_parse() {
    let reg = ferraus_cli::registry();
    let text = prd();
    let mut candidates: Vec<String> = Vec::new();
    // inline code spans
    for span in text.split('`').skip(1).step_by(2) {
        if span.starts_with("ferraus ") {
            candidates.push(span.replace("\\|", "|"));
        }
    }
    // fenced bash blocks
    let mut in_block = false;
    for line in text.lines() {
        if line.starts_with("```") {
            in_block = !in_block;
            continue;
        }
        if in_block && line.trim_start().starts_with("ferraus ") {
            candidates.push(line.trim().to_string());
        }
    }
    let mut bad = Vec::new();
    let mut checked = 0;
    for c in candidates {
        // placeholders, alternations, and prose fragments are not concrete invocations
        if c.contains('<') || c.contains("...") || c.contains('…') || c.contains("\\|") || c.contains("|ferraus") {
            continue;
        }
        if c.split_whitespace().any(|w| w.contains('|')) {
            continue;
        }
        // `ferraus --version`, and the `video` namespace reserved for v2
        let second = c.split_whitespace().nth(1).unwrap_or("");
        if second.starts_with('-') || second == "video" {
            continue;
        }
        checked += 1;
        if let Err(msg) = check_example(&reg, &c) {
            bad.push(msg);
        }
    }
    assert!(checked > 20, "only {checked} PRD examples found");
    assert!(bad.is_empty(), "{} PRD examples do not parse:\n{}", bad.len(), bad.join("\n"));
}

/// Every concrete `ferraus ...` command in a Markdown text: lines of fenced code blocks
/// (shell continuations joined, block quotes included) and inline code spans.
fn md_command_candidates(text: &str) -> Vec<String> {
    let mut candidates: Vec<String> = Vec::new();
    let mut in_block = false;
    let mut pending = String::new();
    for line in text.lines() {
        // code inside block quotes (test-file recipes) counts too
        let line = line.trim_start().trim_start_matches('>').trim_start();
        if line.starts_with("```") {
            in_block = !in_block;
            continue;
        }
        if in_block {
            // join shell line continuations
            if !pending.is_empty() {
                pending.push(' ');
                pending.push_str(line.trim_end_matches('\\').trim());
                if !line.ends_with('\\') {
                    candidates.push(std::mem::take(&mut pending));
                }
            } else if line.starts_with("ferraus ") {
                if line.ends_with('\\') {
                    pending = line.trim_end_matches('\\').trim().to_string();
                } else {
                    candidates.push(line.to_string());
                }
            }
        } else {
            for span in line.split('`').skip(1).step_by(2) {
                if span.starts_with("ferraus ") {
                    candidates.push(span.to_string());
                }
            }
        }
    }
    candidates
}

/// Recursively list the Markdown files under `dir`.
fn md_files_under(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            md_files_under(&p, out);
        } else if p.extension().map(|x| x == "md").unwrap_or(false) {
            out.push(p);
        }
    }
}

/// Every concrete `ferraus ...` command in the companion skills (skills/*/SKILL.md and
/// their references) validates against the CLI, including the stages of `--fx` chains.
/// Skills are what agents copy from; a stale flag there fails a whole unsupervised job.
#[test]
fn skill_examples_parse() {
    let reg = ferraus_cli::registry();
    let mut files = Vec::new();
    md_files_under(&repo_root().join("skills"), &mut files);
    files.sort();
    let mut bad = Vec::new();
    let mut checked = 0;
    for f in &files {
        for c in md_command_candidates(&std::fs::read_to_string(f).unwrap()) {
            if c.contains('<') || c.contains("...") || c.contains('…') {
                continue;
            }
            let second = c.split_whitespace().nth(1).unwrap_or("");
            if second.starts_with('-') {
                continue;
            }
            checked += 1;
            if let Err(msg) = check_example(&reg, &c) {
                bad.push(format!("{}: {msg}", f.strip_prefix(repo_root()).unwrap_or(f).display()));
            }
        }
    }
    assert!(checked > 50, "only {checked} skill examples found");
    assert!(bad.is_empty(), "{} skill examples do not parse:\n{}", bad.len(), bad.join("\n"));
}

/// Every YAML block in the skills (and every .yaml file under skills/) is a valid pipeline,
/// a list of pipeline stages, or a list of fx chain items: ops exist, arguments parse,
/// `$refs` and `in:` resolve, chains validate item by item.
#[test]
fn skill_pipelines_validate() {
    use serde_json::{json, Value};
    let reg = ferraus_cli::registry();
    let mut docs: Vec<(String, String)> = Vec::new();
    let mut files = Vec::new();
    md_files_under(&repo_root().join("skills"), &mut files);
    for f in &files {
        let text = std::fs::read_to_string(f).unwrap();
        let mut block: Option<String> = None;
        for line in text.lines() {
            let tl = line.trim_start();
            match &mut block {
                None if tl.starts_with("```yaml") => block = Some(String::new()),
                Some(b) if tl.starts_with("```") => {
                    docs.push((f.display().to_string(), std::mem::take(b)));
                    block = None;
                }
                Some(b) => {
                    b.push_str(line);
                    b.push('\n');
                }
                None => {}
            }
        }
    }
    fn yaml_files(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                yaml_files(&p, out);
            } else if p.extension().map(|x| x == "yaml" || x == "yml").unwrap_or(false) {
                out.push(p);
            }
        }
    }
    let mut yfiles = Vec::new();
    yaml_files(&repo_root().join("skills"), &mut yfiles);
    for f in &yfiles {
        docs.push((f.display().to_string(), std::fs::read_to_string(f).unwrap()));
    }
    let mut bad = Vec::new();
    let mut checked = 0;
    for (src, text) in &docs {
        // dedent (blocks inside lists are indented)
        let indent =
            text.lines().filter(|l| !l.trim().is_empty()).map(|l| l.len() - l.trim_start().len()).min().unwrap_or(0);
        let text: String = text.lines().map(|l| l.get(indent..).unwrap_or("").to_string() + "\n").collect();
        let v: Value = match serde_yaml::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                bad.push(format!("{src}: YAML does not parse: {e}\n{text}"));
                continue;
            }
        };
        let pipeline_text = match &v {
            Value::Object(o) if o.contains_key("version") => text.clone(),
            Value::Array(items) if items.iter().all(|i| i.get("id").is_some()) => {
                // stages: every `in` and `$ref` that is not a stage becomes an input
                let ids: Vec<String> = items.iter().filter_map(|i| i["id"].as_str().map(String::from)).collect();
                let mut inputs = serde_json::Map::new();
                let mut refs: Vec<String> = Vec::new();
                for it in items {
                    match &it["in"] {
                        Value::String(s) => refs.push(s.clone()),
                        Value::Array(a) => refs.extend(a.iter().filter_map(|x| x.as_str().map(String::from))),
                        _ => {}
                    }
                    fn walk(v: &Value, out: &mut Vec<String>) {
                        match v {
                            Value::String(s) => {
                                if let Some(r) = ferraus_core::pipeline::arg_ref(s) {
                                    out.push(r.to_string());
                                }
                            }
                            Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
                            Value::Object(o) => o.values().for_each(|x| walk(x, out)),
                            _ => {}
                        }
                    }
                    walk(&it["args"], &mut refs);
                }
                for r in refs {
                    let base = r.split('.').next().unwrap_or(&r).to_string();
                    if !ids.contains(&base) {
                        inputs.insert(base.clone(), json!(format!("{base}.wav")));
                    }
                }
                serde_json::to_string(&json!({"version": 1, "inputs": inputs, "stages": items})).unwrap()
            }
            Value::Array(items) if items.iter().all(|i| i.get("op").is_some()) => serde_json::to_string(&json!({
                "version": 1,
                "inputs": {"src": "src.wav"},
                "stages": [{"id": "chain", "op": "fx", "in": "src", "args": {"chain": items}}],
            }))
            .unwrap(),
            _ => continue,
        };
        checked += 1;
        let r = ferraus_core::pipeline::parse(&pipeline_text, &[])
            .and_then(|p| ferraus_core::pipeline::validate(&p, &reg).map(|_| ()));
        if let Err(e) = r {
            bad.push(format!("{src}: {}\n{text}", e.message));
        }
    }
    assert!(checked >= 15, "only {checked} skill YAML blocks found");
    assert!(bad.is_empty(), "{} skill YAML blocks do not validate:\n{}", bad.len(), bad.join("\n---\n"));
}

/// Every concrete `ferraus ...` command in the hand-written guides (tutorials, cookbook,
/// concepts, getting started, troubleshooting) validates against the CLI.
#[test]
fn guide_examples_parse() {
    fn md_files(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                md_files(&p, out);
            } else if p.extension().map(|x| x == "md").unwrap_or(false) {
                out.push(p);
            }
        }
    }
    let reg = ferraus_cli::registry();
    let mut files = Vec::new();
    md_files(&repo_root().join("docs/guides"), &mut files);
    files.sort();
    let mut bad = Vec::new();
    let mut checked = 0;
    for f in &files {
        let text = std::fs::read_to_string(f).unwrap();
        let candidates = md_command_candidates(&text);
        for c in candidates {
            if c.contains('<') || c.contains("...") || c.contains('…') {
                continue;
            }
            let second = c.split_whitespace().nth(1).unwrap_or("");
            if second.starts_with('-') || second == "video" {
                continue;
            }
            checked += 1;
            if let Err(msg) = check_example(&reg, &c) {
                bad.push(format!("{}: {msg}", f.strip_prefix(repo_root()).unwrap_or(f).display()));
            }
        }
    }
    assert!(checked > 100, "only {checked} guide examples found");
    assert!(bad.is_empty(), "{} guide examples do not parse:\n{}", bad.len(), bad.join("\n"));
}

#[test]
fn embedded_troubleshooting_page_matches_docs() {
    // `ferraus docs troubleshooting` embeds crates/ferraus-cli/guides/troubleshooting.md, because
    // the published crate cannot reach docs/; it must stay a copy of the docs page
    let docs = repo_root().join("docs/guides/troubleshooting.md");
    let copy = Path::new(env!("CARGO_MANIFEST_DIR")).join("guides/troubleshooting.md");
    if updating() {
        std::fs::copy(&docs, &copy).unwrap();
    } else {
        assert_eq!(
            std::fs::read_to_string(&copy).unwrap(),
            std::fs::read_to_string(&docs).unwrap(),
            "crates/ferraus-cli/guides/troubleshooting.md is stale; run UPDATE_SNAPSHOTS=1 cargo test -p ferraus --test contract"
        );
    }
}
