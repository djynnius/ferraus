//! Docs, completions, man page, doctor, licenses, self-update, and self-uninstall.

use ferraus_core::prelude::*;
use std::path::PathBuf;

pub fn ops() -> Vec<OpSpec> {
    vec![
        OpSpec::custom("docs", Category::Docs, "Full reference page for a command, offline", docs)
            .about("Prints the same page as the docs site: synopsis, what it does, examples, every option with its type and default, presets, exit codes, common mistakes, and the machine-oriented block. 'ferraus docs errors' prints the error catalog and 'ferraus docs troubleshooting' the troubleshooting guide.")
            .param(param("command", ParamKind::Str).help("Command name (e.g. separate, 'midi quantize'), 'errors', or 'troubleshooting'"))
            .positional("command")
            .example("ferraus docs separate", "Read the separate reference offline")
            .example("ferraus docs errors", "Every exit code with its cause and fix"),
        OpSpec::custom("docs build-site", Category::Docs, "Generate the mdBook docs site source from the command definitions", docs_build_site)
            .inputs(Inputs::NONE)
            .param(param("out", ParamKind::Path).default("target/docs-site").help("Directory to write book.toml and src/ into"))
            .param(param("guides", ParamKind::Path).default("docs/guides").help("Directory of hand-written guides copied into the site"))
            .param(param("check", ParamKind::Flag).help("Fail when any command is below the documentation depth bar"))
            .example("ferraus docs build-site --out target/docs-site", "Write the docs site source")
            .example("ferraus docs build-site --check", "Fail if any command lacks examples or help text"),
        OpSpec::custom("completions", Category::System, "Print a shell completion script", completions)
            .param(param("shell", ParamKind::Choice).choices(&["bash", "zsh", "fish", "powershell", "elvish"]).required().help("Shell to generate completions for"))
            .positional("shell")
            .example("ferraus completions zsh > ~/.zfunc/_ferraus", "Install zsh completions")
            .example("ferraus completions bash > ~/.local/share/bash-completion/completions/ferraus", "Install bash completions"),
        OpSpec::custom("man", Category::Docs, "Print a man page (roff) for ferraus", man_page)
            .inputs(Inputs::NONE)
            .example("ferraus man > ferraus.1", "Write the man page")
            .example("ferraus man | man -l -", "Read it now"),
        OpSpec::custom("doctor", Category::System, "Report what is installed: version, paths, FFmpeg, models, devices, plugins", doctor)
            .inputs(Inputs::NONE)
            .param(param("install-ffmpeg", ParamKind::Flag).help("Download a pinned, checksummed static FFmpeg build into the ferraus cache"))
            .param(param("install-audiveris", ParamKind::Flag).help("Download the Audiveris OMR engine (Java, AGPL) for 'score read --engine audiveris'"))
            .example("ferraus doctor", "Check the installation")
            .example("ferraus doctor --install-ffmpeg -y", "Install the managed FFmpeg for the bridge"),
        OpSpec::custom("licenses", Category::System, "Print every third-party license and attribution", licenses)
            .inputs(Inputs::NONE)
            .example("ferraus licenses > THIRD-PARTY-LICENSES.md", "Save the attributions")
            .example("ferraus licenses | grep -i mpl", "Find copyleft-at-file-level components"),
        OpSpec::custom("self-update", Category::System, "Update ferraus to the latest release (opt-in, never automatic)", self_update)
            .inputs(Inputs::NONE)
            .param(param("version", ParamKind::Str).help("Install this version instead of the latest, e.g. 1.2.0"))
            .example("ferraus self-update", "Install the newest signed release")
            .example("ferraus self-update --version 1.2.0", "Pin a version"),
        OpSpec::custom("self-uninstall", Category::System, "Remove everything the URL installer placed", self_uninstall)
            .inputs(Inputs::NONE)
            .param(param("purge", ParamKind::Flag).help("Also delete the config, cache (models), and data (sounds, jobs) directories"))
            .example("ferraus self-uninstall -y", "Remove the binary installed by the shell installer")
            .example("ferraus self-uninstall --purge -y", "Remove the binary, models, sounds, and config"),
    ]
}

fn docs(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let reg = ctx.registry.ok_or_else(|| Error::internal("no registry"))?;
    let what = a.str("command").unwrap_or_default();
    let text = match what.as_str() {
        "" => {
            let mut s = String::from(
                "ferraus documentation. Run 'ferraus docs <command>' for a command page.\n\nSpecial pages: errors, troubleshooting.\n\nCommands:\n",
            );
            for (_, l) in ferraus_core::builtin::agent_catalog(reg) {
                s.push_str("  ");
                s.push_str(&l);
                s.push('\n');
            }
            s
        }
        "errors" => crate::docgen::error_catalog(),
        "troubleshooting" => include_str!("../../../docs/guides/troubleshooting.md").to_string(),
        name => crate::docgen::command_page(reg.require(name)?),
    };
    Ok(json!({ "text": text }))
}

fn docs_build_site(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let reg = ctx.registry.ok_or_else(|| Error::internal("no registry"))?;
    let out = a.path("out").unwrap_or_else(|| PathBuf::from("target/docs-site"));
    let guides = a.path("guides").unwrap_or_else(|| PathBuf::from("docs/guides"));
    let problems = crate::docgen::coverage_problems(reg);
    if a.flag("check") && !problems.is_empty() {
        return Err(Error::verification(format!(
            "{} documentation problems:\n{}",
            problems.len(),
            problems.join("\n")
        )));
    }
    let n = crate::docgen::build_site(reg, &guides, &out)?;
    Ok(json!({"out": out.display().to_string(), "command_pages": n, "problems": problems}))
}

fn completions(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    use clap_complete::Shell;
    let reg = ctx.registry.ok_or_else(|| Error::internal("no registry"))?;
    let shell = match a.choice("shell").as_str() {
        "bash" => Shell::Bash,
        "zsh" => Shell::Zsh,
        "fish" => Shell::Fish,
        "powershell" => Shell::PowerShell,
        "elvish" => Shell::Elvish,
        other => return Err(Error::usage(format!("unknown shell '{other}'"))),
    };
    let mut cmd = crate::cli::build_command(reg);
    let mut buf = Vec::new();
    clap_complete::generate(shell, &mut cmd, "ferraus", &mut buf);
    Ok(json!({ "text": String::from_utf8_lossy(&buf) }))
}

fn roff(s: &str) -> String {
    s.replace('\\', "\\\\").replace('-', "\\-")
}

fn man_page(_a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let reg = ctx.registry.ok_or_else(|| Error::internal("no registry"))?;
    let mut s = String::new();
    s.push_str(&format!(".TH FERRAUS 1 \"\" \"ferraus {}\" \"User Commands\"\n", ferraus_core::FERRAUS_VERSION));
    s.push_str(".SH NAME\nferraus \\- music editing CLI for people and AI agents\n");
    s.push_str(
        ".SH SYNOPSIS\n.B ferraus\n\\fIcommand\\fR [\\fIinputs\\fR] [\\fIoptions\\fR] [\\fB\\-o\\fR \\fIoutput\\fR]\n",
    );
    s.push_str(".SH DESCRIPTION\nferraus edits, repairs, separates, tunes, transcribes, and masters audio. Every command accepts \\fB\\-\\-json\\fR for a structured result and writes a provenance sidecar beside each output. Run \\fBferraus docs\\fR \\fIcommand\\fR for the full page of any command.\n");
    s.push_str(".SH COMMANDS\n");
    for (_, line) in ferraus_core::builtin::agent_catalog(reg) {
        let (name, rest) = line.split_once(':').unwrap_or((&line, ""));
        s.push_str(&format!(".TP\n.B {}\n{}\n", roff(name), roff(rest.trim())));
    }
    s.push_str(".SH EXIT STATUS\n");
    for k in ErrorKind::all() {
        s.push_str(&format!(".TP\n.B {}\n{}\n", k.exit_code(), roff(k.describe())));
    }
    s.push_str(".SH FILES\n.TP\nconfig.toml\nPer-user configuration (see \\fBferraus doctor\\fR for its path).\n");
    s.push_str(".SH SEE ALSO\nhttps://djynnius.github.io/ferraus\n");
    Ok(json!({ "text": s }))
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let exe = if cfg!(windows) { format!("{name}.exe") } else { name.to_string() };
    std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join(&exe)).find(|c| c.is_file()))
}

fn dir_size(p: &std::path::Path) -> u64 {
    let mut total = 0;
    if let Ok(rd) = std::fs::read_dir(p) {
        for e in rd.flatten() {
            let path = e.path();
            if path.is_dir() {
                total += dir_size(&path);
            } else if let Ok(m) = e.metadata() {
                total += m.len();
            }
        }
    }
    total
}

fn doctor(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    use ferraus_core::paths;
    let mut report = serde_json::Map::new();
    if a.flag("install-ffmpeg") {
        let p = ferraus_media::install_ffmpeg(ctx.globals.yes, ctx.globals.offline)?;
        report.insert("installed_ffmpeg".into(), json!(p.display().to_string()));
    }
    if a.flag("install-audiveris") {
        let p = ferraus_score::install_audiveris(ctx.globals.yes, ctx.globals.offline)?;
        report.insert("installed_audiveris".into(), json!(p.display().to_string()));
    }
    report.insert("ferraus_version".into(), json!(ferraus_core::FERRAUS_VERSION));
    report.insert("platform".into(), json!({"os": std::env::consts::OS, "arch": std::env::consts::ARCH}));
    report.insert(
        "paths".into(),
        json!({
            "config": paths::config_file().display().to_string(),
            "cache": paths::cache_dir().display().to_string(),
            "data": paths::data_dir().display().to_string(),
            "models": paths::models_dir().display().to_string(),
            "sounds": paths::sounds_dir().display().to_string(),
        }),
    );
    report.insert("ffmpeg".into(), ferraus_media::ffmpeg_status());
    report.insert("models".into(), json!({"cache_bytes": dir_size(&paths::models_dir())}));
    report.insert("compute".into(), ferraus_ml::device_report());
    let plughost = std::env::current_exe()
        .ok()
        .and_then(|e| {
            e.parent().map(|d| d.join(if cfg!(windows) { "ferraus-plughost.exe" } else { "ferraus-plughost" }))
        })
        .filter(|p| p.exists())
        .map(|p| p.display().to_string());
    report.insert("plughost".into(), json!({ "clap_host": plughost }));
    report.insert("java".into(), json!(find_on_path("java").map(|p| p.display().to_string())));
    let mut hints = Vec::new();
    if report["ffmpeg"]["found"] != json!(true) {
        hints.push("ferraus doctor --install-ffmpeg  # enables AAC/ALAC encoding, exotic containers, media replace-audio/remux");
    }
    report.insert("suggestions".into(), json!(hints));
    Ok(Value::Object(report))
}

fn licenses(_a: &Args, _ctx: &mut OpCtx) -> Result<Value> {
    Ok(json!({ "text": include_str!("../THIRD-PARTY-LICENSES.md") }))
}

fn self_update(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    if ctx.globals.offline {
        return Err(Error::new(ErrorKind::Network, "offline", "self-update needs the network and --offline is set"));
    }
    let base = "https://github.com/djynnius/ferraus/releases";
    let (cmd, args): (&str, Vec<String>) = if cfg!(windows) {
        let url = match a.str("version") {
            Some(v) => format!("{base}/download/v{v}/ferraus-installer.ps1"),
            None => format!("{base}/latest/download/ferraus-installer.ps1"),
        };
        ("powershell", vec!["-ExecutionPolicy".into(), "ByPass".into(), "-c".into(), format!("irm {url} | iex")])
    } else {
        let url = match a.str("version") {
            Some(v) => format!("{base}/download/v{v}/ferraus-installer.sh"),
            None => format!("{base}/latest/download/ferraus-installer.sh"),
        };
        ("sh", vec!["-c".into(), format!("curl --proto '=https' --tlsv1.2 -fsSL {url} | sh")])
    };
    if ctx.globals.dry_run {
        return Ok(json!({"would_run": [cmd, args]}));
    }
    let status = std::process::Command::new(cmd)
        .args(&args)
        .status()
        .map_err(|e| Error::new(ErrorKind::Network, "update_failed", e.to_string()))?;
    if !status.success() {
        return Err(Error::new(ErrorKind::Network, "update_failed", format!("installer exited with {status}")));
    }
    Ok(json!({"updated": true}))
}

fn self_uninstall(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let install_dir = std::env::var_os("FERRAUS_INSTALL_DIR").map(PathBuf::from).unwrap_or_else(|| {
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or_default()
            .join(".ferraus")
    });
    let mut targets = vec![install_dir];
    if a.flag("purge") {
        targets.push(ferraus_core::paths::cache_dir());
        targets.push(ferraus_core::paths::data_dir());
        targets.push(ferraus_core::paths::config_dir());
    }
    let existing: Vec<String> = targets.iter().filter(|t| t.exists()).map(|t| t.display().to_string()).collect();
    if ctx.globals.dry_run {
        return Ok(json!({"would_remove": existing}));
    }
    if !ctx.globals.yes {
        return Err(Error::usage(format!("self-uninstall removes {}; re-run with -y to confirm", existing.join(", "))));
    }
    for t in &targets {
        if t.exists() {
            std::fs::remove_dir_all(t)?;
        }
    }
    Ok(json!({
        "removed": existing,
        "note": "Remove the ~/.ferraus/bin line from your shell profile if the installer added one. Package-manager installs (brew, apt, dnf, winget) must be removed with that package manager."
    }))
}
