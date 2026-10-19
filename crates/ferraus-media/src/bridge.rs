//! Process helpers for the FFmpeg bridge. Every FFmpeg/ffprobe invocation in ferraus goes
//! through `ferraus_codec::ffmpeg_bridge` (binary discovery, spawning); this module adds the
//! pass-through runner that streams stderr live while keeping its tail for error messages.

use ferraus_codec::ffmpeg_bridge;
use ferraus_core::{Error, ErrorKind, Result};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;

/// Result of a bridge run.
pub struct RunOutput {
    pub status: Option<i32>,
    pub success: bool,
    pub stdout: Vec<u8>,
    /// Last lines of stderr.
    pub stderr_tail: Vec<String>,
}

/// Quote one argument for display as a POSIX shell word.
pub fn shell_quote(a: &str) -> String {
    if !a.is_empty() && a.chars().all(|c| c.is_ascii_alphanumeric() || "-_./:=+,@%^".contains(c)) {
        a.to_string()
    } else {
        format!("'{}'", a.replace('\'', "'\\''"))
    }
}

/// The exact command line, quoted for display and provenance.
pub fn command_line(bin: &Path, args: &[String]) -> String {
    std::iter::once(bin.display().to_string())
        .chain(args.iter().cloned())
        .map(|a| shell_quote(&a))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Run a bridge binary, forwarding stderr to our stderr line by line (when `echo`), keeping
/// the last 20 lines, and capturing stdout.
pub fn run_streaming(bin: &Path, args: &[String], echo: bool) -> Result<RunOutput> {
    let mut child = ffmpeg_bridge::spawn(bin, args)?;
    let stderr = child.stderr.take().expect("piped stderr");
    let mut stdout = child.stdout.take().expect("piped stdout");
    let err_thread = std::thread::spawn(move || {
        let mut tail: std::collections::VecDeque<String> = std::collections::VecDeque::new();
        let reader = BufReader::new(stderr);
        let mut err = std::io::stderr();
        // ffmpeg updates progress with '\r'; split on both line endings
        for chunk in reader.split(b'\n') {
            let Ok(chunk) = chunk else { break };
            if echo {
                let _ = err.write_all(&chunk);
                let _ = err.write_all(b"\n");
            }
            let line = String::from_utf8_lossy(&chunk);
            for part in line.split('\r') {
                let p = part.trim();
                if !p.is_empty() {
                    tail.push_back(p.to_string());
                    if tail.len() > 20 {
                        tail.pop_front();
                    }
                }
            }
        }
        tail.into_iter().collect::<Vec<_>>()
    });
    let mut out = Vec::new();
    stdout.read_to_end(&mut out).map_err(|e| Error::processing(format!("reading ffmpeg output: {e}")))?;
    let status = child.wait().map_err(|e| Error::processing(format!("waiting for ffmpeg: {e}")))?;
    let tail = err_thread.join().unwrap_or_default();
    Ok(RunOutput { status: status.code(), success: status.success(), stdout: out, stderr_tail: tail })
}

/// The error for a failed FFmpeg run: exit status plus the last stderr lines.
pub fn failed(what: &str, cmdline: &str, out: &RunOutput) -> Error {
    let n = out.stderr_tail.len();
    let tail = out.stderr_tail[n.saturating_sub(8)..].join("\n");
    Error::new(
        ErrorKind::Processing,
        "ffmpeg_failed",
        format!(
            "{what} failed (exit status {}): {cmdline}\n{tail}",
            out.status.map(|s| s.to_string()).unwrap_or_else(|| "signal".into())
        ),
    )
}

/// Run ffmpeg quietly for a container job; errors carry the stderr tail.
pub fn run_job(what: &str, args: Vec<String>) -> Result<(String, RunOutput)> {
    let bin = ffmpeg_bridge::require(what)?;
    let mut full = vec!["-hide_banner".to_string(), "-nostdin".into(), "-loglevel".into(), "error".into()];
    full.extend(args);
    let cmd = command_line(&bin, &full);
    let out = run_streaming(&bin, &full, false)?;
    if !out.success {
        return Err(failed(what, &cmd, &out));
    }
    Ok((cmd, out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting() {
        assert_eq!(shell_quote("-i"), "-i");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(shell_quote(""), "''");
        assert_eq!(
            command_line(Path::new("/usr/bin/ffmpeg"), &["-af".into(), "volume=0.5,aformat=s16".into()]),
            "/usr/bin/ffmpeg -af volume=0.5,aformat=s16"
        );
    }
}
