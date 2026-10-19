//! Progress on stderr: a bar on a TTY, NDJSON events with `--progress json`, or nothing.

use ferraus_core::ctx::Progress;
use serde_json::json;
use std::io::{IsTerminal, Write};
use std::sync::Mutex;
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    None,
    Bar,
    Json,
}

pub struct StderrProgress {
    mode: Mode,
    verbosity: i32,
    state: Mutex<State>,
}

struct State {
    started: Instant,
    last_draw: Option<Instant>,
    drew_bar: bool,
}

impl StderrProgress {
    pub fn new(mode: &str, verbosity: i32) -> StderrProgress {
        let mode = match mode {
            "none" => Mode::None,
            "json" => Mode::Json,
            "bar" => Mode::Bar,
            _ => {
                if std::io::stderr().is_terminal() && verbosity >= 0 {
                    Mode::Bar
                } else {
                    Mode::None
                }
            }
        };
        StderrProgress {
            mode,
            verbosity,
            state: Mutex::new(State { started: Instant::now(), last_draw: None, drew_bar: false }),
        }
    }

    fn emit(&self, v: serde_json::Value) {
        let mut e = std::io::stderr().lock();
        let _ = writeln!(e, "{v}");
    }

    fn clear_bar(&self, st: &mut State) {
        if st.drew_bar {
            let _ = write!(std::io::stderr(), "\r\x1b[2K");
            st.drew_bar = false;
        }
    }

    pub fn finish(&self) {
        let mut st = self.state.lock().unwrap();
        self.clear_bar(&mut st);
    }
}

impl Progress for StderrProgress {
    fn stage(&self, stage: &str, index: usize, of: usize) {
        match self.mode {
            Mode::Json => self.emit(json!({"event": "stage_start", "stage": stage, "index": index, "of": of})),
            Mode::Bar if self.verbosity > 0 => {
                let mut st = self.state.lock().unwrap();
                self.clear_bar(&mut st);
                eprintln!("[{index}/{of}] {stage}");
            }
            _ => {}
        }
    }

    fn progress(&self, stage: &str, fraction: f64) {
        match self.mode {
            Mode::Json => {
                let st = self.state.lock().unwrap();
                let el = st.started.elapsed().as_secs_f64();
                let eta = if fraction > 0.01 { Some(((el / fraction) - el).max(0.0).round() as u64) } else { None };
                drop(st);
                self.emit(json!({"event": "progress", "stage": stage, "percent": (fraction * 1000.0).round() / 10.0, "eta_s": eta}));
            }
            Mode::Bar => {
                let mut st = self.state.lock().unwrap();
                let now = Instant::now();
                if fraction < 1.0 && st.last_draw.map(|t| now.duration_since(t).as_millis() < 100).unwrap_or(false) {
                    return;
                }
                st.last_draw = Some(now);
                let width = 30;
                let filled = (fraction * width as f64).round() as usize;
                let _ = write!(
                    std::io::stderr(),
                    "\r\x1b[2K{stage} [{}{}] {:>5.1}%",
                    "#".repeat(filled),
                    "-".repeat(width - filled.min(width)),
                    fraction * 100.0
                );
                st.drew_bar = true;
            }
            Mode::None => {}
        }
    }

    fn output(&self, path: &str, role: Option<&str>) {
        if self.mode == Mode::Json {
            self.emit(json!({"event": "output", "path": path, "role": role}));
        }
    }

    fn warning(&self, code: &str, message: &str) {
        match self.mode {
            Mode::Json => self.emit(json!({"event": "warning", "code": code, "message": message})),
            _ if self.verbosity >= 0 => {
                let mut st = self.state.lock().unwrap();
                self.clear_bar(&mut st);
                eprintln!("warning: {message}");
            }
            _ => {}
        }
    }

    fn log(&self, message: &str) {
        if self.verbosity > 0 && self.mode != Mode::Json {
            let mut st = self.state.lock().unwrap();
            self.clear_bar(&mut st);
            eprintln!("{message}");
        }
    }
}
