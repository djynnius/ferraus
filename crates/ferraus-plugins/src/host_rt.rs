//! The host-process side of the protocol: framing, shared-memory mapping, and dispatch to a
//! format backend (CLAP in `ferraus-plughost`, VST3 in `ferraus-plughost-vst3`).

use crate::protocol::*;
use base64::Engine;
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

/// An error a backend reports back to the client.
#[derive(Debug, Clone)]
pub struct HostError {
    /// `load_failed`, `unknown_param`, `bad_value`, `no_state`, `process_failed`,
    /// `unsupported`, `bad_request`.
    pub code: String,
    pub message: String,
}

impl HostError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        HostError { code: code.into(), message: message.into() }
    }
}

pub type HostResult<T> = Result<T, HostError>;

/// Result of activation.
#[derive(Debug, Clone, Copy)]
pub struct Activation {
    pub latency: u32,
    pub tail: Option<u32>,
}

/// One plugin format. Every method runs on the host's single thread.
pub trait Backend {
    /// Program name reported in `hello`.
    fn host_name(&self) -> &'static str;
    fn formats(&self) -> Vec<String>;
    fn probe(&mut self, path: &Path) -> HostResult<Vec<Descriptor>>;
    fn load(&mut self, path: &Path, plugin_id: Option<&str>) -> HostResult<PluginInfo>;
    fn get_params(&mut self) -> HostResult<Vec<ParamValue>>;
    fn set_params(&mut self, params: &[ParamAssign]) -> HostResult<Vec<ParamValue>>;
    fn list_presets(&mut self) -> HostResult<Vec<PresetInfo>>;
    fn load_preset(&mut self, preset: &str) -> HostResult<Vec<ParamValue>>;
    fn get_state(&mut self) -> HostResult<Vec<u8>>;
    fn set_state(&mut self, state: &[u8]) -> HostResult<()>;
    /// Activate and start processing. The main ports' channel counts come from
    /// [`PluginInfo::main_in_channels`]/[`PluginInfo::main_out_channels`].
    fn activate(&mut self, sample_rate: f64, block_size: u32) -> HostResult<Activation>;
    /// Process one block from the main input channels into the main output channels.
    fn process(
        &mut self,
        inputs: &[&[f32]],
        outputs: &mut [&mut [f32]],
        frames: u32,
        events: &[Event],
    ) -> HostResult<()>;
    fn deactivate(&mut self);
    /// Main channel counts, valid after `load`.
    fn main_channels(&self) -> (u32, u32);
}

/// Move the process's stdout out of reach of plugin code: the protocol gets a private
/// duplicate of fd 1 and fd 1 itself is pointed at stderr.
fn take_protocol_stdout() -> File {
    #[cfg(unix)]
    unsafe {
        use std::os::fd::FromRawFd;
        let fd = libc::dup(1);
        if fd >= 0 {
            libc::dup2(2, 1);
            return File::from_raw_fd(fd);
        }
        File::from_raw_fd(1)
    }
    #[cfg(windows)]
    unsafe {
        use std::os::windows::io::FromRawHandle;
        let fd = libc::dup(1);
        if fd >= 0 {
            libc::dup2(2, 1);
            let h = libc::get_osfhandle(fd);
            return File::from_raw_handle(h as _);
        }
        let h = libc::get_osfhandle(1);
        File::from_raw_handle(h as _)
    }
}

struct Shm {
    map: memmap2::MmapMut,
    layout: ShmLayout,
}

/// Run the request loop until `quit` or end of input. Returns the process exit code.
pub fn serve(backend: &mut dyn Backend) -> i32 {
    let mut out = take_protocol_stdout();
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    serve_on(backend, &mut input, &mut out)
}

/// The loop itself, on arbitrary streams (used by tests).
pub fn serve_on(backend: &mut dyn Backend, input: &mut dyn Read, out: &mut dyn Write) -> i32 {
    let mut shm: Option<Shm> = None;
    let mut block_size = 0u32;
    loop {
        let frame = match read_frame(input) {
            Ok(Some(f)) => f,
            Ok(None) => return 0,
            Err(e) => {
                eprintln!("ferraus-plughost: broken control pipe: {e}");
                return 3;
            }
        };
        let req: Request = match serde_json::from_slice(&frame) {
            Ok(r) => r,
            Err(e) => {
                let reply = Reply::Error { code: "bad_request".into(), message: format!("cannot parse request: {e}") };
                if write_frame(out, &reply).is_err() {
                    return 3;
                }
                continue;
            }
        };
        let quit = matches!(req, Request::Quit);
        let reply = handle(backend, req, &mut shm, &mut block_size);
        if write_frame(out, &reply).is_err() {
            return 3;
        }
        if quit {
            return 0;
        }
    }
}

fn err(e: HostError) -> Reply {
    Reply::Error { code: e.code, message: e.message }
}

fn handle(backend: &mut dyn Backend, req: Request, shm: &mut Option<Shm>, block_size: &mut u32) -> Reply {
    let b64 = base64::engine::general_purpose::STANDARD;
    match req {
        Request::Hello => Reply::Hello {
            host: backend.host_name().into(),
            version: env!("CARGO_PKG_VERSION").into(),
            protocol: PROTOCOL_VERSION,
            formats: backend.formats(),
        },
        Request::Probe { path } => match backend.probe(Path::new(&path)) {
            Ok(plugins) => Reply::Probe { plugins },
            Err(e) => err(e),
        },
        Request::Load { path, plugin_id } => match backend.load(Path::new(&path), plugin_id.as_deref()) {
            Ok(info) => Reply::Loaded { info },
            Err(e) => err(e),
        },
        Request::SetParams { params } => match backend.set_params(&params) {
            Ok(params) => Reply::Params { params },
            Err(e) => err(e),
        },
        Request::GetParams => match backend.get_params() {
            Ok(params) => Reply::Params { params },
            Err(e) => err(e),
        },
        Request::ListPresets => match backend.list_presets() {
            Ok(presets) => Reply::Presets { presets },
            Err(e) => err(e),
        },
        Request::LoadPreset { preset } => match backend.load_preset(&preset) {
            Ok(params) => Reply::Params { params },
            Err(e) => err(e),
        },
        Request::GetState => match backend.get_state() {
            Ok(s) => Reply::State { state_b64: b64.encode(s) },
            Err(e) => err(e),
        },
        Request::SetState { state_b64 } => match b64.decode(state_b64.trim()) {
            Ok(bytes) => match backend.set_state(&bytes) {
                Ok(()) => Reply::Ok,
                Err(e) => err(e),
            },
            Err(e) => err(HostError::new("bad_request", format!("state is not base64: {e}"))),
        },
        Request::Activate { sample_rate, block_size: bs, shm_path } => {
            let map = match File::options()
                .read(true)
                .write(true)
                .open(&shm_path)
                .and_then(|f| unsafe { memmap2::MmapMut::map_mut(&f) })
            {
                Ok(m) => m,
                Err(e) => {
                    return err(HostError::new("bad_request", format!("cannot map shared memory {shm_path}: {e}")))
                }
            };
            let layout = match ShmLayout::read_header(&map) {
                Ok(l) => l,
                Err(e) => return err(HostError::new("bad_request", e)),
            };
            if layout.block_size != bs {
                return err(HostError::new("bad_request", "block size differs from the shared memory header"));
            }
            match backend.activate(sample_rate, bs) {
                Ok(a) => {
                    let (ci, co) = backend.main_channels();
                    *shm = Some(Shm { map, layout });
                    *block_size = bs;
                    Reply::Activated { latency: a.latency, tail: a.tail, channels_in: ci, channels_out: co }
                }
                Err(e) => err(e),
            }
        }
        Request::Process { slot, frames, events } => {
            let Some(s) = shm.as_mut() else {
                return err(HostError::new("bad_request", "process before activate"));
            };
            let l = s.layout;
            if slot >= l.slots || frames > *block_size || frames > l.block_size {
                return err(HostError::new("bad_request", "slot or frame count out of range"));
            }
            std::sync::atomic::fence(std::sync::atomic::Ordering::SeqCst);
            let base = s.map.as_mut_ptr();
            let n = frames as usize;
            // SAFETY: offsets come from the validated layout, input and output ranges are
            // disjoint, and the client does not touch this slot until we reply.
            let ins: Vec<&[f32]> =
                (0..l.channels_in).map(|c| unsafe { &*f32_slice_mut(base, l.input_offset(slot, c), n) }).collect();
            let mut outs: Vec<&mut [f32]> =
                (0..l.channels_out).map(|c| unsafe { f32_slice_mut(base, l.output_offset(slot, c), n) }).collect();
            for o in outs.iter_mut() {
                o.fill(0.0);
            }
            let r = backend.process(&ins, &mut outs, frames, &events);
            std::sync::atomic::fence(std::sync::atomic::Ordering::SeqCst);
            match r {
                Ok(()) => Reply::Processed { slot },
                Err(e) => err(e),
            }
        }
        Request::Deactivate => {
            backend.deactivate();
            *shm = None;
            Reply::Ok
        }
        Request::Quit => {
            backend.deactivate();
            *shm = None;
            Reply::Ok
        }
    }
}
