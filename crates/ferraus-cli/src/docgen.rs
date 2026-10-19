//! Documentation generated from op specs: command reference pages, the error catalog,
//! `llms.txt`, and the mdBook site. Help text is written once, on the OpSpec.

use ferraus_core::error::ErrorKind;
use ferraus_core::op::{OpSpec, ParamKind};
use ferraus_core::Registry;
use std::collections::BTreeMap;
use std::fmt::Write;

/// Exit codes a command can return: every command can return 0, 1, 2, 3, 70; the rest
/// depend on what it touches.
pub fn exit_codes_for(op: &OpSpec) -> Vec<ErrorKind> {
    let mut v = vec![ErrorKind::Processing, ErrorKind::Usage, ErrorKind::InputMissing];
    let n = op.name;
    let audio = matches!(op.inputs.kind, ferraus_core::InputKind::Audio | ferraus_core::InputKind::Any);
    if audio {
        v.push(ErrorKind::Unsupported);
    }
    let model_cmds = [
        "separate",
        "denoise",
        "transcribe",
        "tune",
        "score",
        "models",
        "hum",
        "master",
        "analyze pitch",
        "accelerate",
    ];
    if model_cmds.iter().any(|m| n.starts_with(m)) {
        v.extend([ErrorKind::ModelNotCached, ErrorKind::ModelDownload, ErrorKind::Network]);
    }
    let plugin_cmds = ["plugin", "render", "hum", "vocoder", "sounds", "score perform"];
    if plugin_cmds.iter().any(|m| n.starts_with(m)) {
        v.push(ErrorKind::Plugin);
    }
    if n.starts_with("sounds") || n.starts_with("doctor") || n.starts_with("self-update") {
        v.push(ErrorKind::Network);
    }
    v.push(ErrorKind::Cancelled);
    if n.starts_with("verify") || n.starts_with("master verify") || n.starts_with("normalize") || n.starts_with("limit")
    {
        v.push(ErrorKind::Verification);
    }
    v.push(ErrorKind::Internal);
    v.sort_by_key(|k| k.exit_code());
    v.dedup();
    v
}

fn md_escape(s: &str) -> String {
    s.replace('|', "\\|")
}

/// One reference page per command: people first, then the machine block.
pub fn command_page(op: &OpSpec) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "# ferraus {}\n", op.name);
    let _ = writeln!(s, "{}\n", op.summary);
    if op.best_effort {
        let _ = writeln!(
            s,
            "> **Quality tier: best-effort.** Results are reported with a quality or confidence estimate.\n"
        );
    }
    let inputs = match (op.inputs.min, op.inputs.max) {
        (0, Some(0)) => String::new(),
        (_, Some(1)) => " <input>".to_string(),
        _ => " <inputs...>".to_string(),
    };
    let pos = op.positional.map(|p| format!(" <{p}>")).unwrap_or_default();
    let _ = writeln!(
        s,
        "## Synopsis\n\n```text\nferraus {}{}{} [options]{}\n```\n",
        op.name,
        pos,
        inputs,
        if inputs.is_empty() { "" } else { " [-o <output>]" }
    );
    let _ = writeln!(s, "## What it does\n");
    if op.about.is_empty() {
        let _ = writeln!(s, "{}\n", op.summary);
    } else {
        let _ = writeln!(s, "{}\n", op.about);
    }
    if op.is_transform() {
        let _ = writeln!(s, "This command is also an `fx` stage and a pipeline op: `ferraus fx in.wav -o out.wav --fx \"{}:key=value\"`.\n", op.name);
    }
    if !op.examples.is_empty() {
        let _ = writeln!(s, "## Examples\n");
        for e in &op.examples {
            let _ = writeln!(s, "{}\n\n```bash\n{}\n```\n", e.explain, e.cmd);
        }
    }
    let params = op.effective_params();
    if !params.is_empty() {
        let _ = writeln!(s, "## Options\n\n| Flag | Type | Default | Meaning |\n| --- | --- | --- | --- |");
        for p in &params {
            let flag =
                if op.positional == Some(p.name) { format!("`<{}>`", p.name) } else { format!("`--{}`", p.name) };
            let ty = match p.kind {
                ParamKind::Choice if !p.choices.is_empty() => p.choices.join(" \\| "),
                k => k.type_name().to_string(),
            };
            let mut meaning = md_escape(p.help);
            if let Some(r) = p.range {
                meaning.push_str(&format!(" Range: {}.", md_escape(r)));
            }
            if p.repeatable {
                meaning.push_str(" Repeatable.");
            }
            if p.required {
                meaning.push_str(" **Required.**");
            }
            let _ = writeln!(
                s,
                "| {} | {} | {} | {} |",
                flag,
                md_escape(&ty),
                p.default.map(|d| format!("`{}`", md_escape(d))).unwrap_or_else(|| "—".into()),
                meaning
            );
        }
        let _ = writeln!(s);
    }
    if !op.presets.is_empty() {
        let _ = writeln!(s, "## Presets\n");
        for p in &op.presets {
            let vals: Vec<String> = p.values.iter().map(|(k, v)| format!("{k}={v}")).collect();
            let _ = writeln!(s, "- `{}`: {} ({})", p.name, p.help, vals.join(", "));
        }
        let _ = writeln!(s);
    }
    let _ = writeln!(s, "## Global options\n\nEvery command also accepts the global options (`-o`, `--json`, `--dry-run`, `--overwrite`, `--progress`, ...). See [Global options](../concepts/global-options.md).\n");
    let _ = writeln!(s, "## Exit codes\n\n| Code | Meaning |\n| --- | --- |\n| 0 | Success |");
    for k in exit_codes_for(op) {
        let _ = writeln!(s, "| {} | {} |", k.exit_code(), k.describe());
    }
    let _ = writeln!(s);
    let _ = writeln!(s, "## Common mistakes\n");
    for m in common_mistakes(op) {
        let _ = writeln!(s, "- {m}");
    }
    let _ = writeln!(s);
    let _ = writeln!(s, "## For models\n");
    let _ = writeln!(s, "- MCP tool: `ferraus_{}`", op.id());
    let _ = writeln!(s, "- Schema: `ferraus schema {}`", op.name);
    let _ = writeln!(s, "- Pipeline: `{{ op: {}, args: {{ ... }} }}` (flag names with `_` instead of `-`)", op.name);
    let _ = writeln!(s, "- fx stage: {}", if op.is_transform() { "yes" } else { "no" });
    let defaults: Vec<String> =
        params.iter().filter_map(|p| p.default.map(|d| format!("\"{}\": \"{}\"", p.key(), d))).collect();
    let _ =
        writeln!(s, "\n```json\n{{\"command\": \"{}\", \"defaults\": {{{}}}}}\n```\n", op.name, defaults.join(", "));
    s
}

fn common_mistakes(op: &OpSpec) -> Vec<String> {
    let mut v = Vec::new();
    if op.inputs.max != Some(0) && op.positional.is_none() {
        v.push(
            "Forgetting that outputs are never overwritten: pass `--overwrite` or a new `-o` path when re-running."
                .to_string(),
        );
    }
    if op.params.iter().any(|p| p.kind == ParamKind::Time || p.kind == ParamKind::Range) {
        v.push("Beat-based times (`4b`, `1/4`) need a tempo: pass `--bpm` or write `4b@120`.".to_string());
    }
    if op.params.iter().any(|p| p.kind == ParamKind::Percent) {
        v.push("Percentages accept `80%` or the fraction `0.8`; a bare `80` means 80x, not 80%.".to_string());
    }
    if op.params.iter().any(|p| p.kind == ParamKind::Gain) {
        v.push("Gains need a unit when not in dB: `0.5x` is a factor, `-6` and `-6dB` are decibels.".to_string());
    }
    if op.best_effort {
        v.push("Treating best-effort output as final: check the reported quality/confidence in `--json`.".to_string());
    }
    if v.is_empty() {
        v.push("Expecting human-readable text when scripting: add `--json` for a stable machine result.".to_string());
    }
    v
}

pub fn error_catalog() -> String {
    let mut s = String::from("# Error catalog\n\nEvery error carries a stable `code`, a `message`, an `exit_code`, and when a fix exists, a `hint` that is a runnable command.\n\n| Exit code | Meaning | Typical codes | What to do |\n| --- | --- | --- | --- |\n");
    let rows: &[(ErrorKind, &str, &str)] = &[
        (
            ErrorKind::Processing,
            "`processing_failed`, `io_error`",
            "Read the message; re-run with `-v`; report with the sidecar if it persists.",
        ),
        (
            ErrorKind::Usage,
            "`invalid_argument`, `output_exists`, `path_not_allowed`",
            "Fix the flag named in the message; `ferraus docs <command>` lists every option.",
        ),
        (
            ErrorKind::InputMissing,
            "`input_not_found`, `not_found`, `permission_denied`",
            "Check the path and permissions.",
        ),
        (
            ErrorKind::Unsupported,
            "`unsupported_format`",
            "Convert first, or install FFmpeg: `ferraus doctor --install-ffmpeg`.",
        ),
        (
            ErrorKind::ModelNotCached,
            "`model_not_cached`",
            "Run the `ferraus models pull ...` command in the hint, or allow downloads with `-y`.",
        ),
        (
            ErrorKind::ModelDownload,
            "`download_failed`, `checksum_mismatch`",
            "Retry; the download resumes. A repeated mismatch means a corrupted mirror.",
        ),
        (
            ErrorKind::Plugin,
            "`plugin_crashed`, `plugin_timeout`, `plugin_not_found`",
            "Run `ferraus plugin scan`; try another preset; the main process is unaffected.",
        ),
        (ErrorKind::Cancelled, "`cancelled`, `timeout`", "Raise `--timeout`/`--max-memory` or re-run."),
        (
            ErrorKind::Verification,
            "`verification_failed`",
            "The output failed a check (hash, clipping, duration). See the message.",
        ),
        (ErrorKind::Network, "`network_error`", "Check connectivity, or use `--offline` with cached models."),
        (
            ErrorKind::Internal,
            "`internal_error`",
            "A bug: report it at https://github.com/djynnius/ferraus/issues with the command and sidecar.",
        ),
    ];
    for (k, codes, todo) in rows {
        let _ = writeln!(s, "| {} | {} | {} | {} |", k.exit_code(), k.describe(), codes, todo);
    }
    s
}

/// `llms.txt`: a site index for models.
pub fn llms_txt(reg: &Registry, base_url: &str) -> String {
    let mut s = String::from("# ferraus\n\n> A cross-platform music editing CLI for people and AI agents: editing, effects, restoration, noise reduction, stem separation, pitch correction, MIDI, notation, plugin hosting, and mastering. Every command has --json output, a JSON Schema, and an MCP tool.\n\n");
    let _ = writeln!(s, "## Start here\n\n- [Agent guide]({base_url}/agents.html): compact catalog of every command\n- [Getting started]({base_url}/getting-started.html)\n- [Error catalog]({base_url}/errors.html)\n- [Full reference as one file]({base_url}/llms-full.txt)\n");
    let mut by_cat: BTreeMap<_, Vec<&OpSpec>> = BTreeMap::new();
    for op in reg.ops() {
        by_cat.entry(op.category).or_default().push(op);
    }
    for (cat, ops) in by_cat {
        let _ = writeln!(s, "## {}\n", cat.title());
        for op in ops {
            let _ = writeln!(s, "- [ferraus {}]({}/commands/{}.html): {}", op.name, base_url, op.id(), op.summary);
        }
        let _ = writeln!(s);
    }
    s
}

pub fn llms_full(reg: &Registry) -> String {
    let mut s = String::from("# ferraus full reference\n\n");
    for op in reg.ops() {
        s.push_str(&command_page(op));
        s.push_str("\n---\n\n");
    }
    s.push_str(&error_catalog());
    s
}

/// Write the mdBook source tree for the docs site into `out` (src/ + book.toml).
pub fn build_site(reg: &Registry, guides_dir: &std::path::Path, out: &std::path::Path) -> std::io::Result<usize> {
    let src = out.join("src");
    std::fs::create_dir_all(src.join("commands"))?;
    std::fs::create_dir_all(src.join("concepts"))?;
    let base = "https://djynnius.github.io/ferraus";
    let mut summary = String::from("# Summary\n\n[Introduction](README.md)\n\n");
    // hand-written guides (copied verbatim); each top-level md file becomes a chapter
    let mut guides: Vec<std::path::PathBuf> =
        std::fs::read_dir(guides_dir).map(|rd| rd.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    guides.sort();
    let mut intro_written = false;
    for g in &guides {
        if g.is_dir() {
            let section = g.file_name().unwrap().to_string_lossy().to_string();
            std::fs::create_dir_all(src.join(&section))?;
            let mut files: Vec<_> = std::fs::read_dir(g)?
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().map(|e| e == "md").unwrap_or(false))
                .collect();
            files.sort();
            let title = section.replace('-', " ");
            let _ = writeln!(summary, "\n# {}\n", title[..1].to_uppercase() + &title[1..]);
            for f in files {
                let name = f.file_name().unwrap().to_string_lossy().to_string();
                std::fs::copy(&f, src.join(&section).join(&name))?;
                let t = first_heading(&f).unwrap_or_else(|| name.trim_end_matches(".md").to_string());
                let _ = writeln!(summary, "- [{}]({}/{})", t, section, name);
            }
        } else if g.extension().map(|e| e == "md").unwrap_or(false) {
            let name = g.file_name().unwrap().to_string_lossy().to_string();
            std::fs::copy(g, src.join(&name))?;
            if name == "README.md" {
                intro_written = true;
                continue;
            }
            let t = first_heading(g).unwrap_or_else(|| name.trim_end_matches(".md").to_string());
            let _ = writeln!(summary, "- [{}]({})", t, name);
        }
    }
    if !intro_written {
        std::fs::write(
            src.join("README.md"),
            "# ferraus\n\nA cross-platform music editing CLI for people and AI agents.\n",
        )?;
    }
    // agent guide
    let catalog = ferraus_core::builtin::agent_catalog(reg);
    let mut agents = String::from("# Agent guide\n\nThe whole command surface, one line per command. The same text is printed by `ferraus describe --agent`.\n\n```text\n");
    for (_, l) in &catalog {
        agents.push_str(l);
        agents.push('\n');
    }
    agents.push_str("```\n");
    std::fs::write(src.join("agents.md"), agents)?;
    let _ = writeln!(summary, "- [Agent guide](agents.md)");
    std::fs::write(src.join("errors.md"), error_catalog())?;
    let _ = writeln!(summary, "- [Error catalog](errors.md)");
    let _ = writeln!(summary, "\n# Command reference\n");
    let mut by_cat: BTreeMap<_, Vec<&OpSpec>> = BTreeMap::new();
    for op in reg.ops() {
        by_cat.entry(op.category).or_default().push(op);
    }
    let mut n = 0;
    for (cat, ops) in by_cat {
        let _ = writeln!(summary, "- [{}]()", cat.title());
        for op in ops {
            std::fs::write(src.join("commands").join(format!("{}.md", op.id())), command_page(op))?;
            let _ = writeln!(summary, "  - [{}](commands/{}.md)", op.name, op.id());
            n += 1;
        }
    }
    std::fs::write(src.join("SUMMARY.md"), summary)?;
    std::fs::write(src.join("llms.txt"), llms_txt(reg, base))?;
    std::fs::write(src.join("llms-full.txt"), llms_full(reg))?;
    std::fs::write(
        out.join("book.toml"),
        "[book]\ntitle = \"ferraus\"\nauthors = [\"Sunday Ikpe\"]\nlanguage = \"en\"\nsrc = \"src\"\n\n[output.html]\ngit-repository-url = \"https://github.com/djynnius/ferraus\"\nsite-url = \"/ferraus/\"\n",
    )?;
    Ok(n)
}

fn first_heading(p: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(p).ok()?;
    text.lines().find(|l| l.starts_with("# ")).map(|l| l[2..].trim().to_string())
}

/// Every command page needs at least this many runnable examples (the PRD's depth bar).
pub const MIN_EXAMPLES: usize = 3;

/// Docs coverage check: commands below the depth bar (missing help, examples, defaults).
pub fn coverage_problems(reg: &Registry) -> Vec<String> {
    let mut v = Vec::new();
    for op in reg.ops() {
        if op.examples.len() < MIN_EXAMPLES {
            v.push(format!("{}: {} runnable example(s), the depth bar is {MIN_EXAMPLES}", op.name, op.examples.len()));
        }
        for p in &op.params {
            if p.help.is_empty() {
                v.push(format!("{} --{}: no help text", op.name, p.name));
            }
        }
    }
    v
}
