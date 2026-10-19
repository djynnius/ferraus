//! System commands owned by the CLI: background jobs, `doctor`, `licenses`, `docs`.

use ferraus_core::prelude::*;
use std::path::PathBuf;

pub fn ops() -> Vec<OpSpec> {
    vec![
        OpSpec::custom("jobs list", Category::Agents, "List background jobs started with --async", jobs_list)
            .inputs(Inputs::NONE)
            .example("ferraus jobs list", "Every background job with its state (running, done, failed, cancelled)")
            .example(
                "ferraus jobs list --json",
                "The job list as JSON: job_id, state, percent, and the original command line",
            )
            .example(
                "ferraus separate song.wav --async --json && ferraus jobs list",
                "Start a long separation in the background, then see it listed",
            ),
        OpSpec::custom("jobs status", Category::Agents, "Status of a background job", jobs_status)
            .param(param("id", ParamKind::Str).required().help("Job id returned by --async"))
            .positional("id")
            .example("ferraus jobs status 1760873452123-3f2a", "State and percent complete of one job")
            .example(
                "ferraus jobs status 1760873452123-3f2a --json",
                "Status as JSON; result is filled in once the job has finished",
            )
            .example(
                "ferraus denoise interview.wav --async --json",
                "Start the job whose id you then pass to jobs status",
            ),
        OpSpec::custom("jobs logs", Category::Agents, "Progress and log output of a background job", jobs_logs)
            .param(param("id", ParamKind::Str).required().help("Job id"))
            .positional("id")
            .example(
                "ferraus jobs logs 1760873452123-3f2a",
                "The progress events and log lines the job has written so far",
            )
            .example("ferraus jobs logs 1760873452123-3f2a --json", "Progress and logs as JSON")
            .example("ferraus jobs logs 1760873452123-3f2a | tail -n 5", "Only the latest progress lines"),
        OpSpec::custom(
            "jobs wait",
            Category::Agents,
            "Block until a background job finishes, then return its result",
            jobs_wait,
        )
        .param(param("id", ParamKind::Str).required().help("Job id"))
        .param(param("poll", ParamKind::Time).default("500ms").help("Polling interval"))
        .positional("id")
        .example("ferraus jobs wait 1760873452123-3f2a", "Block until the job finishes, then print its result")
        .example(
            "ferraus jobs wait 1760873452123-3f2a --poll 2s --json",
            "Check every two seconds; print the job's result envelope as JSON",
        )
        .example(
            "ferraus jobs wait 1760873452123-3f2a --timeout 10m",
            "Give up waiting after ten minutes (exit code 8)",
        ),
        OpSpec::custom("jobs cancel", Category::Agents, "Cancel a running background job", jobs_cancel)
            .param(param("id", ParamKind::Str).required().help("Job id"))
            .positional("id")
            .example("ferraus jobs cancel 1760873452123-3f2a", "Stop a running job; partial outputs are not kept")
            .example("ferraus jobs cancel 1760873452123-3f2a --json", "Report whether the job was cancelled, as JSON")
            .example("ferraus jobs cancel 1760873452123-3f2a --quiet", "Cancel silently, for scripts"),
        OpSpec::custom("jobs result", Category::Agents, "The JSON result envelope of a finished job", jobs_result)
            .param(param("id", ParamKind::Str).required().help("Job id"))
            .positional("id")
            .example(
                "ferraus jobs result 1760873452123-3f2a",
                "The finished job's result envelope, exactly as the command would have printed it",
            )
            .example(
                "ferraus jobs result 1760873452123-3f2a --json",
                "The envelope as JSON (outputs, metrics, warnings)",
            )
            .example("ferraus jobs result 1760873452123-3f2a --json | jq .outputs", "Just the files the job wrote"),
    ]
}

fn job_dir(id: &str) -> Result<PathBuf> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(Error::usage(format!("'{id}' is not a job id")));
    }
    let d = ferraus_core::paths::jobs_dir().join(id);
    if !d.exists() {
        return Err(
            Error::new(ErrorKind::Usage, "job_not_found", format!("no job '{id}'")).with_hint("ferraus jobs list")
        );
    }
    Ok(d)
}

fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    #[cfg(windows)]
    {
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
            .unwrap_or(false)
    }
}

fn job_info(id: &str) -> Result<Value> {
    let d = job_dir(id)?;
    let meta: Value = serde_json::from_slice(&std::fs::read(d.join("job.json"))?)?;
    let result_path = d.join("result.json");
    let read_result = || -> Option<Value> {
        std::fs::read(&result_path).ok().and_then(|b| {
            let s = String::from_utf8_lossy(&b).to_string();
            s.lines().rev().find(|l| l.trim_start().starts_with('{')).and_then(|l| serde_json::from_str(l).ok())
        })
    };
    let pid = meta["pid"].as_u64().unwrap_or(0) as u32;
    let mut result = read_result();
    // check liveness once: a job that exits between two checks must not look failed
    let alive = pid != 0 && pid_alive(pid);
    // a job that just exited may not have its result visible yet: look again briefly
    if result.is_none() && !alive {
        for _ in 0..10 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            result = read_result();
            if result.is_some() {
                break;
            }
        }
    }
    let cancelled = d.join("cancelled").exists();
    let state = if let Some(r) = &result {
        if r["ok"].as_bool() == Some(true) {
            "succeeded"
        } else {
            "failed"
        }
    } else if cancelled {
        "cancelled"
    } else if alive {
        "running"
    } else {
        "failed"
    };
    // last progress event
    let progress = std::fs::read_to_string(d.join("progress.ndjson"))
        .ok()
        .and_then(|s| {
            s.lines().rev().filter_map(|l| serde_json::from_str::<Value>(l).ok()).find(|v| v["event"] == "progress")
        })
        .and_then(|v| v["percent"].as_f64());
    Ok(
        json!({"job_id": id, "state": state, "pid": pid, "argv": meta["argv"], "started": meta["started"], "percent": progress, "result": result}),
    )
}

/// Start the same command line (minus --async) as a detached background process.
pub fn spawn_async(argv: &[String], json_out: bool) -> i32 {
    let id = format!(
        "{}-{:04x}",
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0),
        std::process::id() & 0xffff
    );
    let dir = ferraus_core::paths::jobs_dir().join(&id);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("error: cannot create job directory: {e}");
        return 1;
    }
    let mut args: Vec<String> = argv.iter().skip(1).filter(|a| *a != "--async").cloned().collect();
    args.retain(|a| a != "--json");
    args.push("--json".into());
    args.push("--progress".into());
    args.push("json".into());
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("ferraus"));
    let stdout = std::fs::File::create(dir.join("result.json"));
    let stderr = std::fs::File::create(dir.join("progress.ndjson"));
    let (Ok(stdout), Ok(stderr)) = (stdout, stderr) else {
        eprintln!("error: cannot create job files");
        return 1;
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.args(&args).stdin(std::process::Stdio::null()).stdout(stdout).stderr(stderr);
    if let Ok(cwd) = std::env::current_dir() {
        cmd.current_dir(cwd);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x00000008 | 0x00000200); // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP
    }
    match cmd.spawn() {
        Ok(child) => {
            let meta = json!({"job_id": id, "pid": child.id(), "argv": args, "started": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)});
            let _ = std::fs::write(dir.join("job.json"), serde_json::to_vec_pretty(&meta).unwrap());
            if json_out {
                println!("{}", json!({"ok": true, "job_id": id}));
            } else {
                println!("{id}");
            }
            0
        }
        Err(e) => {
            eprintln!("error: cannot start background job: {e}");
            1
        }
    }
}

fn jobs_list(_: &Args, _: &mut OpCtx) -> Result<Value> {
    let mut jobs = Vec::new();
    if let Ok(rd) = std::fs::read_dir(ferraus_core::paths::jobs_dir()) {
        let mut ids: Vec<String> = rd.flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
        ids.sort();
        for id in ids {
            if let Ok(mut info) = job_info(&id) {
                info.as_object_mut().map(|o| o.remove("result"));
                jobs.push(info);
            }
        }
    }
    Ok(json!({ "jobs": jobs }))
}

fn jobs_status(a: &Args, _: &mut OpCtx) -> Result<Value> {
    let mut info = job_info(&a.str_or("id", ""))?;
    info.as_object_mut().map(|o| o.remove("result"));
    Ok(info)
}

fn jobs_logs(a: &Args, _: &mut OpCtx) -> Result<Value> {
    let d = job_dir(&a.str_or("id", ""))?;
    let text = std::fs::read_to_string(d.join("progress.ndjson")).unwrap_or_default();
    Ok(json!({ "text": text }))
}

fn jobs_wait(a: &Args, ctx: &mut OpCtx) -> Result<Value> {
    let id = a.str_or("id", "");
    let poll = a.seconds("poll")?.max(0.05);
    loop {
        ctx.check_cancel()?;
        let info = job_info(&id)?;
        if info["state"] != "running" {
            return Ok(info);
        }
        std::thread::sleep(std::time::Duration::from_secs_f64(poll));
    }
}

fn jobs_cancel(a: &Args, _: &mut OpCtx) -> Result<Value> {
    let id = a.str_or("id", "");
    let info = job_info(&id)?;
    if info["state"] != "running" {
        return Ok(json!({"job_id": id, "cancelled": false, "state": info["state"]}));
    }
    let pid = info["pid"].as_u64().unwrap_or(0);
    #[cfg(unix)]
    let ok = std::process::Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    #[cfg(windows)]
    let ok = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    let _ = std::fs::write(job_dir(&id)?.join("cancelled"), b"");
    Ok(json!({"job_id": id, "cancelled": ok}))
}

fn jobs_result(a: &Args, _: &mut OpCtx) -> Result<Value> {
    let id = a.str_or("id", "");
    let info = job_info(&id)?;
    match &info["result"] {
        Value::Null => Err(Error::new(
            ErrorKind::Usage,
            "job_not_finished",
            format!("job {id} is {}", info["state"].as_str().unwrap_or("unknown")),
        )
        .with_hint(format!("ferraus jobs wait {id}"))),
        r => Ok(r.clone()),
    }
}
