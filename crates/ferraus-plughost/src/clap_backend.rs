//! CLAP backend over the raw C ABI (`clap-sys`). Single-threaded: the host's one thread is
//! the CLAP "main thread" for everything except `process`, during which it is the audio
//! thread (reported through the `thread-check` extension).

use clap_sys::entry::clap_plugin_entry;
use clap_sys::events::*;
use clap_sys::ext::audio_ports::*;
use clap_sys::ext::latency::*;
use clap_sys::ext::log::*;
use clap_sys::ext::note_ports::*;
use clap_sys::ext::params::*;
use clap_sys::ext::preset_load::*;
use clap_sys::ext::state::*;
use clap_sys::ext::tail::*;
use clap_sys::ext::thread_check::*;
use clap_sys::factory::plugin_factory::*;
use clap_sys::factory::preset_discovery::*;
use clap_sys::fixedpoint::{CLAP_BEATTIME_FACTOR, CLAP_SECTIME_FACTOR};
use clap_sys::host::clap_host;
use clap_sys::id::clap_id;
use clap_sys::plugin::*;
use clap_sys::process::*;
use clap_sys::stream::*;
use clap_sys::universal_plugin_id::clap_universal_plugin_id;
use clap_sys::version::{clap_version_is_compatible, CLAP_VERSION};
use ferraus_plugins::host_rt::{Activation, Backend, HostError, HostResult};
use ferraus_plugins::protocol::*;
use std::ffi::{c_char, c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, Ordering};

unsafe fn cs(p: *const c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        CStr::from_ptr(p).to_string_lossy().into_owned()
    }
}

fn cstring(s: &str) -> CString {
    CString::new(s.replace('\0', "")).unwrap_or_default()
}

fn load_err(msg: impl Into<String>) -> HostError {
    HostError::new("load_failed", msg)
}

/// The executable inside a CLAP bundle: on macOS `Foo.clap/Contents/MacOS/<exe>`, elsewhere
/// the `.clap` file itself.
pub fn clap_binary(bundle: &Path) -> HostResult<PathBuf> {
    if bundle.is_file() {
        return Ok(bundle.to_path_buf());
    }
    if !bundle.is_dir() {
        return Err(load_err(format!("{} does not exist", bundle.display())));
    }
    let macos = bundle.join("Contents").join("MacOS");
    let plist = bundle.join("Contents").join("Info.plist");
    if let Ok(text) = std::fs::read_to_string(&plist) {
        if let Some(exe) = plist_string(&text, "CFBundleExecutable") {
            let p = macos.join(exe);
            if p.is_file() {
                return Ok(p);
            }
        }
    }
    if let Some(stem) = bundle.file_stem() {
        let p = macos.join(stem);
        if p.is_file() {
            return Ok(p);
        }
    }
    if let Ok(rd) = std::fs::read_dir(&macos) {
        for e in rd.flatten() {
            if e.path().is_file() {
                return Ok(e.path());
            }
        }
    }
    Err(load_err(format!("{} is a directory but not a CLAP bundle (no Contents/MacOS executable)", bundle.display())))
}

fn plist_string(text: &str, key: &str) -> Option<String> {
    let k = format!("<key>{key}</key>");
    let rest = &text[text.find(&k)? + k.len()..];
    let start = rest.find("<string>")? + "<string>".len();
    let end = rest[start..].find("</string>")?;
    Some(rest[start..start + end].trim().to_string())
}

// ---------------------------------------------------------------------------------------
// module (the loaded shared library)

struct Module {
    entry: *const clap_plugin_entry,
    path: PathBuf,
    // Never unloaded: unloading plugin code that may have registered thread-local
    // destructors or atexit handlers is a classic source of crashes, and a host process
    // lives for one plugin anyway.
    _lib: std::mem::ManuallyDrop<libloading::Library>,
}

impl Module {
    fn open(bundle: &Path) -> HostResult<Module> {
        let bin = clap_binary(bundle)?;
        // SAFETY: loading a plugin runs its initializers; that is the point of the host
        // process, which is isolated from ferraus.
        let lib = unsafe { libloading::Library::new(&bin) }
            .map_err(|e| load_err(format!("cannot load {}: {e}", bin.display())))?;
        let entry: *const clap_plugin_entry = unsafe {
            let sym = lib
                .get::<*const clap_plugin_entry>(b"clap_entry\0")
                .map_err(|e| load_err(format!("{} has no clap_entry: {e}", bin.display())))?;
            *sym
        };
        if entry.is_null() {
            return Err(load_err("clap_entry is null"));
        }
        let e = unsafe { &*entry };
        if !clap_version_is_compatible(e.clap_version) {
            return Err(load_err(format!(
                "incompatible CLAP version {}.{}.{}",
                e.clap_version.major, e.clap_version.minor, e.clap_version.revision
            )));
        }
        let cpath = cstring(&bundle.to_string_lossy());
        if let Some(init) = e.init {
            if !unsafe { init(cpath.as_ptr()) } {
                return Err(load_err("the plugin's clap_entry.init() failed"));
            }
        }
        Ok(Module { entry, path: bundle.to_path_buf(), _lib: std::mem::ManuallyDrop::new(lib) })
    }

    fn factory(&self, id: &CStr) -> *const c_void {
        let e = unsafe { &*self.entry };
        match e.get_factory {
            Some(f) => unsafe { f(id.as_ptr()) },
            None => null(),
        }
    }

    fn plugin_factory(&self) -> HostResult<&clap_plugin_factory> {
        let f = self.factory(CLAP_PLUGIN_FACTORY_ID) as *const clap_plugin_factory;
        if f.is_null() {
            return Err(load_err("the bundle exposes no CLAP plugin factory"));
        }
        Ok(unsafe { &*f })
    }

    fn descriptors(&self) -> HostResult<Vec<(Descriptor, *const clap_plugin_descriptor)>> {
        let f = self.plugin_factory()?;
        let count = f.get_plugin_count.map(|c| unsafe { c(f) }).unwrap_or(0);
        let mut out = Vec::new();
        for i in 0..count {
            let d = match f.get_plugin_descriptor {
                Some(g) => unsafe { g(f, i) },
                None => null(),
            };
            if d.is_null() {
                continue;
            }
            out.push((unsafe { descriptor(&*d) }, d));
        }
        Ok(out)
    }
}

impl Drop for Module {
    fn drop(&mut self) {
        let e = unsafe { &*self.entry };
        if let Some(deinit) = e.deinit {
            unsafe { deinit() };
        }
    }
}

unsafe fn descriptor(d: &clap_plugin_descriptor) -> Descriptor {
    let mut features = Vec::new();
    if !d.features.is_null() {
        let mut i = 0;
        loop {
            let f = *d.features.add(i);
            if f.is_null() || i > 64 {
                break;
            }
            features.push(cs(f).to_ascii_lowercase());
            i += 1;
        }
    }
    let kind = Descriptor::classify(&features);
    Descriptor {
        id: cs(d.id),
        name: cs(d.name),
        vendor: cs(d.vendor),
        version: cs(d.version),
        description: cs(d.description),
        url: cs(d.url),
        features,
        kind,
    }
}

// ---------------------------------------------------------------------------------------
// host callbacks

#[repr(C)]
struct HostShared {
    clap: clap_host,
    callback_requested: AtomicBool,
    restart_requested: AtomicBool,
    in_process: AtomicBool,
}

unsafe fn shared<'a>(h: *const clap_host) -> &'a HostShared {
    &*((*h).host_data as *const HostShared)
}

unsafe extern "C" fn host_get_extension(_h: *const clap_host, id: *const c_char) -> *const c_void {
    if id.is_null() {
        return null();
    }
    let id = CStr::from_ptr(id);
    if id == CLAP_EXT_LOG {
        &HOST_LOG as *const _ as *const c_void
    } else if id == CLAP_EXT_THREAD_CHECK {
        &HOST_THREAD_CHECK as *const _ as *const c_void
    } else if id == CLAP_EXT_PARAMS {
        &HOST_PARAMS as *const _ as *const c_void
    } else if id == CLAP_EXT_STATE {
        &HOST_STATE as *const _ as *const c_void
    } else if id == CLAP_EXT_LATENCY {
        &HOST_LATENCY as *const _ as *const c_void
    } else if id == CLAP_EXT_TAIL {
        &HOST_TAIL as *const _ as *const c_void
    } else if id == CLAP_EXT_AUDIO_PORTS {
        &HOST_AUDIO_PORTS as *const _ as *const c_void
    } else if id == CLAP_EXT_NOTE_PORTS {
        &HOST_NOTE_PORTS as *const _ as *const c_void
    } else {
        null()
    }
}

unsafe extern "C" fn host_request_restart(h: *const clap_host) {
    shared(h).restart_requested.store(true, Ordering::SeqCst);
}
unsafe extern "C" fn host_request_process(_h: *const clap_host) {}
unsafe extern "C" fn host_request_callback(h: *const clap_host) {
    shared(h).callback_requested.store(true, Ordering::SeqCst);
}

static HOST_LOG: clap_host_log = clap_host_log { log: Some(host_log) };
unsafe extern "C" fn host_log(_h: *const clap_host, severity: clap_log_severity, msg: *const c_char) {
    let level = match severity {
        CLAP_LOG_DEBUG => "debug",
        CLAP_LOG_INFO => "info",
        CLAP_LOG_WARNING => "warning",
        CLAP_LOG_ERROR | CLAP_LOG_FATAL => "error",
        _ => "misbehaving",
    };
    eprintln!("[plugin {level}] {}", cs(msg));
}

static HOST_THREAD_CHECK: clap_host_thread_check =
    clap_host_thread_check { is_main_thread: Some(host_is_main), is_audio_thread: Some(host_is_audio) };
unsafe extern "C" fn host_is_main(h: *const clap_host) -> bool {
    !shared(h).in_process.load(Ordering::SeqCst)
}
unsafe extern "C" fn host_is_audio(h: *const clap_host) -> bool {
    shared(h).in_process.load(Ordering::SeqCst)
}

static HOST_PARAMS: clap_host_params = clap_host_params {
    rescan: Some(host_params_rescan),
    clear: Some(host_params_clear),
    request_flush: Some(host_noop),
};
unsafe extern "C" fn host_params_rescan(_h: *const clap_host, _f: clap_param_rescan_flags) {}
unsafe extern "C" fn host_params_clear(_h: *const clap_host, _id: clap_id, _f: clap_param_clear_flags) {}
unsafe extern "C" fn host_noop(_h: *const clap_host) {}

static HOST_STATE: clap_host_state = clap_host_state { mark_dirty: Some(host_noop) };
static HOST_LATENCY: clap_host_latency = clap_host_latency { changed: Some(host_noop) };
static HOST_TAIL: clap_host_tail = clap_host_tail { changed: Some(host_noop) };
static HOST_AUDIO_PORTS: clap_host_audio_ports =
    clap_host_audio_ports { is_rescan_flag_supported: Some(host_ap_flag), rescan: Some(host_ap_rescan) };
unsafe extern "C" fn host_ap_flag(_h: *const clap_host, _f: u32) -> bool {
    false
}
unsafe extern "C" fn host_ap_rescan(_h: *const clap_host, _f: u32) {}
static HOST_NOTE_PORTS: clap_host_note_ports =
    clap_host_note_ports { supported_dialects: Some(host_np_dialects), rescan: Some(host_ap_rescan) };
unsafe extern "C" fn host_np_dialects(_h: *const clap_host) -> clap_note_dialect {
    CLAP_NOTE_DIALECT_CLAP | CLAP_NOTE_DIALECT_MIDI
}

fn new_host() -> Box<HostShared> {
    let mut b = Box::new(HostShared {
        clap: clap_host {
            clap_version: CLAP_VERSION,
            host_data: null_mut(),
            name: c"ferraus".as_ptr(),
            vendor: c"ferraus".as_ptr(),
            url: c"https://github.com/djynnius/ferraus".as_ptr(),
            version: c"0.1.0".as_ptr(),
            get_extension: Some(host_get_extension),
            request_restart: Some(host_request_restart),
            request_process: Some(host_request_process),
            request_callback: Some(host_request_callback),
        },
        callback_requested: AtomicBool::new(false),
        restart_requested: AtomicBool::new(false),
        in_process: AtomicBool::new(false),
    });
    let p = &mut *b as *mut HostShared as *mut c_void;
    b.clap.host_data = p;
    b
}

// ---------------------------------------------------------------------------------------
// event lists and streams

#[repr(C)]
#[derive(Clone, Copy)]
union EventStore {
    header: clap_event_header,
    note: clap_event_note,
    midi: clap_event_midi,
    param: clap_event_param_value,
}

#[derive(Default)]
struct EventList {
    events: Vec<EventStore>,
}

fn header(size: usize, time: u32, type_: clap_event_type) -> clap_event_header {
    clap_event_header { size: size as u32, time, space_id: CLAP_CORE_EVENT_SPACE_ID, type_, flags: 0 }
}

impl EventList {
    fn param(&mut self, time: u32, id: clap_id, cookie: *mut c_void, value: f64) {
        self.events.push(EventStore {
            param: clap_event_param_value {
                header: header(std::mem::size_of::<clap_event_param_value>(), time, CLAP_EVENT_PARAM_VALUE),
                param_id: id,
                cookie,
                note_id: -1,
                port_index: -1,
                channel: -1,
                key: -1,
                value,
            },
        });
    }

    fn note(&mut self, time: u32, on: bool, channel: u8, key: u8, velocity: f32) {
        self.events.push(EventStore {
            note: clap_event_note {
                header: header(
                    std::mem::size_of::<clap_event_note>(),
                    time,
                    if on { CLAP_EVENT_NOTE_ON } else { CLAP_EVENT_NOTE_OFF },
                ),
                note_id: -1,
                port_index: 0,
                channel: channel as i16,
                key: key as i16,
                velocity: velocity as f64,
            },
        });
    }

    fn midi(&mut self, time: u32, data: [u8; 3]) {
        self.events.push(EventStore {
            midi: clap_event_midi {
                header: header(std::mem::size_of::<clap_event_midi>(), time, CLAP_EVENT_MIDI),
                port_index: 0,
                data,
            },
        });
    }

    fn input(&self) -> clap_input_events {
        clap_input_events {
            ctx: self as *const EventList as *mut c_void,
            size: Some(events_size),
            get: Some(events_get),
        }
    }
}

unsafe extern "C" fn events_size(list: *const clap_input_events) -> u32 {
    (*((*list).ctx as *const EventList)).events.len() as u32
}

unsafe extern "C" fn events_get(list: *const clap_input_events, i: u32) -> *const clap_event_header {
    let l = &*((*list).ctx as *const EventList);
    match l.events.get(i as usize) {
        Some(e) => e as *const EventStore as *const clap_event_header,
        None => null(),
    }
}

unsafe extern "C" fn events_push(_l: *const clap_output_events, _e: *const clap_event_header) -> bool {
    true
}

static OUT_EVENTS: clap_output_events = clap_output_events { ctx: null_mut(), try_push: Some(events_push) };

unsafe extern "C" fn ostream_write(s: *const clap_ostream, buf: *const c_void, size: u64) -> i64 {
    let v = &mut *((*s).ctx as *mut Vec<u8>);
    if size > 0 && !buf.is_null() {
        v.extend_from_slice(std::slice::from_raw_parts(buf as *const u8, size as usize));
    }
    size as i64
}

struct IStream<'a> {
    data: &'a [u8],
    pos: usize,
}

unsafe extern "C" fn istream_read(s: *const clap_istream, buf: *mut c_void, size: u64) -> i64 {
    let st = &mut *((*s).ctx as *mut IStream);
    let n = (size as usize).min(st.data.len() - st.pos);
    if n > 0 {
        std::ptr::copy_nonoverlapping(st.data.as_ptr().add(st.pos), buf as *mut u8, n);
        st.pos += n;
    }
    n as i64
}

// ---------------------------------------------------------------------------------------
// preset discovery

#[derive(Default)]
struct IndexerData {
    extensions: Vec<String>,
    locations: Vec<(u32, Option<String>)>,
}

unsafe extern "C" fn ix_filetype(
    ix: *const clap_preset_discovery_indexer,
    ft: *const clap_preset_discovery_filetype,
) -> bool {
    let d = &mut *((*ix).indexer_data as *mut IndexerData);
    if !ft.is_null() {
        let e = cs((*ft).file_extension);
        if !e.is_empty() {
            d.extensions.push(e.trim_start_matches('.').to_ascii_lowercase());
        }
    }
    true
}

unsafe extern "C" fn ix_location(
    ix: *const clap_preset_discovery_indexer,
    loc: *const clap_preset_discovery_location,
) -> bool {
    let d = &mut *((*ix).indexer_data as *mut IndexerData);
    if !loc.is_null() {
        let l = &*loc;
        let path = if l.location.is_null() { None } else { Some(cs(l.location)) };
        d.locations.push((l.kind, path));
    }
    true
}

unsafe extern "C" fn ix_soundpack(
    _ix: *const clap_preset_discovery_indexer,
    _sp: *const clap_preset_discovery_soundpack,
) -> bool {
    true
}

unsafe extern "C" fn ix_extension(_ix: *const clap_preset_discovery_indexer, _id: *const c_char) -> *const c_void {
    null()
}

struct ReceiverData {
    presets: Vec<(PresetInfo, Vec<String>)>,
    kind: u32,
    location: Option<String>,
    provider: String,
}

unsafe fn rd<'a>(r: *const clap_preset_discovery_metadata_receiver) -> &'a mut ReceiverData {
    &mut *((*r).receiver_data as *mut ReceiverData)
}

unsafe extern "C" fn rcv_error(_r: *const clap_preset_discovery_metadata_receiver, _e: i32, msg: *const c_char) {
    eprintln!("[preset discovery] {}", cs(msg));
}

unsafe extern "C" fn rcv_begin(
    r: *const clap_preset_discovery_metadata_receiver,
    name: *const c_char,
    load_key: *const c_char,
) -> bool {
    let d = rd(r);
    let mut name = cs(name);
    if name.is_empty() {
        name = d
            .location
            .as_deref()
            .and_then(|l| Path::new(l).file_stem())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "preset".into());
    }
    let info = PresetInfo {
        name,
        load_key: if load_key.is_null() { None } else { Some(cs(load_key)) },
        location_kind: if d.kind == CLAP_PRESET_DISCOVERY_LOCATION_PLUGIN { "plugin".into() } else { "file".into() },
        location: d.location.clone(),
        creators: Vec::new(),
        description: String::new(),
        provider: d.provider.clone(),
    };
    d.presets.push((info, Vec::new()));
    d.presets.len() < 20000
}

unsafe extern "C" fn rcv_plugin_id(
    r: *const clap_preset_discovery_metadata_receiver,
    id: *const clap_universal_plugin_id,
) {
    let d = rd(r);
    if let (Some(last), false) = (d.presets.last_mut(), id.is_null()) {
        if cs((*id).abi) == "clap" {
            last.1.push(cs((*id).id));
        }
    }
}

unsafe extern "C" fn rcv_str(_r: *const clap_preset_discovery_metadata_receiver, _s: *const c_char) {}
unsafe extern "C" fn rcv_flags(_r: *const clap_preset_discovery_metadata_receiver, _f: u32) {}
unsafe extern "C" fn rcv_creator(r: *const clap_preset_discovery_metadata_receiver, s: *const c_char) {
    if let Some(last) = rd(r).presets.last_mut() {
        last.0.creators.push(cs(s));
    }
}
unsafe extern "C" fn rcv_description(r: *const clap_preset_discovery_metadata_receiver, s: *const c_char) {
    if let Some(last) = rd(r).presets.last_mut() {
        last.0.description = cs(s);
    }
}
unsafe extern "C" fn rcv_times(_r: *const clap_preset_discovery_metadata_receiver, _a: u64, _b: u64) {}
unsafe extern "C" fn rcv_extra(
    _r: *const clap_preset_discovery_metadata_receiver,
    _k: *const c_char,
    _v: *const c_char,
) {
}

fn walk_files(dir: &Path, exts: &[String], depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 8 || out.len() > 5000 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            walk_files(&p, exts, depth + 1, out);
        } else {
            let ext = p.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
            if exts.is_empty() || exts.contains(&ext) {
                out.push(p);
            }
        }
    }
}

fn discover_presets(module: &Module, plugin_id: &str) -> Vec<PresetInfo> {
    let mut f = module.factory(CLAP_PRESET_DISCOVERY_FACTORY_ID) as *const clap_preset_discovery_factory;
    if f.is_null() {
        f = module.factory(CLAP_PRESET_DISCOVERY_FACTORY_ID_COMPAT) as *const clap_preset_discovery_factory;
    }
    if f.is_null() {
        return Vec::new();
    }
    let f = unsafe { &*f };
    let mut all: Vec<PresetInfo> = Vec::new();
    let count = f.count.map(|c| unsafe { c(f) }).unwrap_or(0);
    for i in 0..count {
        let (Some(get_desc), Some(create)) = (f.get_descriptor, f.create) else {
            break;
        };
        let desc = unsafe { get_desc(f, i) };
        if desc.is_null() {
            continue;
        }
        let provider_name = unsafe { cs((*desc).name) };
        let mut data = Box::new(IndexerData::default());
        let indexer = Box::new(clap_preset_discovery_indexer {
            clap_version: CLAP_VERSION,
            name: c"ferraus".as_ptr(),
            vendor: c"ferraus".as_ptr(),
            url: c"https://github.com/djynnius/ferraus".as_ptr(),
            version: c"0.1.0".as_ptr(),
            indexer_data: &mut *data as *mut IndexerData as *mut c_void,
            declare_filetype: Some(ix_filetype),
            declare_location: Some(ix_location),
            declare_soundpack: Some(ix_soundpack),
            get_extension: Some(ix_extension),
        });
        let provider = unsafe { create(f, &*indexer, (*desc).id) };
        if provider.is_null() {
            continue;
        }
        let p = unsafe { &*provider };
        let ok = p.init.map(|init| unsafe { init(provider) }).unwrap_or(false);
        if ok {
            let mut rdata = Box::new(ReceiverData {
                presets: Vec::new(),
                kind: 0,
                location: None,
                provider: provider_name.clone(),
            });
            let receiver = clap_preset_discovery_metadata_receiver {
                receiver_data: &mut *rdata as *mut ReceiverData as *mut c_void,
                on_error: Some(rcv_error),
                begin_preset: Some(rcv_begin),
                add_plugin_id: Some(rcv_plugin_id),
                set_soundpack_id: Some(rcv_str),
                set_flags: Some(rcv_flags),
                add_creator: Some(rcv_creator),
                set_description: Some(rcv_description),
                set_timestamps: Some(rcv_times),
                add_feature: Some(rcv_str),
                add_extra_info: Some(rcv_extra),
            };
            let locations = data.locations.clone();
            let exts = data.extensions.clone();
            if let Some(get_metadata) = p.get_metadata {
                for (kind, loc) in locations {
                    let targets: Vec<Option<String>> = match (&loc, kind) {
                        (Some(l), CLAP_PRESET_DISCOVERY_LOCATION_FILE) if Path::new(l).is_dir() => {
                            let mut files = Vec::new();
                            walk_files(Path::new(l), &exts, 0, &mut files);
                            files.into_iter().map(|f| Some(f.to_string_lossy().into_owned())).collect()
                        }
                        _ => vec![loc.clone()],
                    };
                    for t in targets {
                        rdata.kind = kind;
                        rdata.location = t.clone();
                        let c = t.as_deref().map(cstring);
                        let ptr = c.as_ref().map(|c| c.as_ptr()).unwrap_or(null());
                        unsafe { get_metadata(provider, kind, ptr, &receiver) };
                    }
                }
            }
            for (info, ids) in rdata.presets.drain(..) {
                if ids.is_empty() || ids.iter().any(|i| i == plugin_id) {
                    all.push(info);
                }
            }
        }
        if let Some(destroy) = p.destroy {
            unsafe { destroy(provider) };
        }
        drop(indexer);
        drop(data);
    }
    all
}

// ---------------------------------------------------------------------------------------
// the backend

struct Exts {
    params: *const clap_plugin_params,
    state: *const clap_plugin_state,
    latency: *const clap_plugin_latency,
    tail: *const clap_plugin_tail,
    audio_ports: *const clap_plugin_audio_ports,
    note_ports: *const clap_plugin_note_ports,
    preset_load: *const clap_plugin_preset_load,
}

impl Default for Exts {
    fn default() -> Self {
        Exts {
            params: null(),
            state: null(),
            latency: null(),
            tail: null(),
            audio_ports: null(),
            note_ports: null(),
            preset_load: null(),
        }
    }
}

pub struct ClapBackend {
    module: Option<Module>,
    host: Box<HostShared>,
    plugin: *const clap_plugin,
    plugin_id: String,
    ext: Exts,
    params: Vec<ParamInfo>,
    cookies: Vec<*mut c_void>,
    pending: Vec<(clap_id, *mut c_void, f64)>,
    in_ports: Vec<u32>,
    out_ports: Vec<u32>,
    main_in: Option<usize>,
    main_out: Option<usize>,
    note_dialects: u32,
    active: bool,
    processing: bool,
    sample_rate: f64,
    in_bufs: Vec<Vec<Vec<f32>>>,
    out_bufs: Vec<Vec<Vec<f32>>>,
    steady: i64,
    presets: Option<Vec<PresetInfo>>,
}

impl Default for ClapBackend {
    fn default() -> Self {
        Self::new()
    }
}

struct ClapText {
    plugin: *const clap_plugin,
    ext: *const clap_plugin_params,
}

impl ParamText for ClapText {
    fn text_to_value(&mut self, id: u32, text: &str) -> Option<f64> {
        let ext = unsafe { self.ext.as_ref()? };
        let f = ext.text_to_value?;
        let c = CString::new(text).ok()?;
        let mut v = 0.0f64;
        if unsafe { f(self.plugin, id, c.as_ptr(), &mut v) } {
            Some(v)
        } else {
            None
        }
    }

    fn value_to_text(&mut self, id: u32, value: f64) -> Option<String> {
        let ext = unsafe { self.ext.as_ref()? };
        let f = ext.value_to_text?;
        let mut buf = [0 as c_char; 256];
        if unsafe { f(self.plugin, id, value, buf.as_mut_ptr(), buf.len() as u32) } {
            buf[255] = 0;
            Some(unsafe { cs(buf.as_ptr()) })
        } else {
            None
        }
    }
}

impl ClapBackend {
    pub fn new() -> Self {
        ClapBackend {
            module: None,
            host: new_host(),
            plugin: null(),
            plugin_id: String::new(),
            ext: Exts::default(),
            params: Vec::new(),
            cookies: Vec::new(),
            pending: Vec::new(),
            in_ports: Vec::new(),
            out_ports: Vec::new(),
            main_in: None,
            main_out: None,
            note_dialects: 0,
            active: false,
            processing: false,
            sample_rate: 48000.0,
            in_bufs: Vec::new(),
            out_bufs: Vec::new(),
            steady: 0,
            presets: None,
        }
    }

    fn plugin(&self) -> HostResult<&clap_plugin> {
        if self.plugin.is_null() {
            Err(HostError::new("bad_request", "no plugin loaded"))
        } else {
            Ok(unsafe { &*self.plugin })
        }
    }

    fn module_for(&mut self, path: &Path) -> HostResult<&Module> {
        let same = self.module.as_ref().map(|m| m.path == path).unwrap_or(false);
        if !same {
            if !self.plugin.is_null() {
                return Err(HostError::new("bad_request", "a different plugin is already loaded in this host"));
            }
            self.module = None;
            self.module = Some(Module::open(path)?);
        }
        Ok(self.module.as_ref().unwrap())
    }

    fn ext<T>(&self, id: &CStr) -> *const T {
        let p = unsafe { &*self.plugin };
        match p.get_extension {
            Some(g) => unsafe { g(self.plugin, id.as_ptr()) as *const T },
            None => null(),
        }
    }

    /// Run `on_main_thread` when the plugin asked for a callback.
    fn pump(&mut self) {
        if !self.plugin.is_null() && self.host.callback_requested.swap(false, Ordering::SeqCst) {
            let p = unsafe { &*self.plugin };
            if let Some(f) = p.on_main_thread {
                unsafe { f(self.plugin) };
            }
        }
    }

    fn text(&self) -> ClapText {
        ClapText { plugin: self.plugin, ext: self.ext.params }
    }

    fn read_params(&mut self) {
        let Some(ext) = (unsafe { self.ext.params.as_ref() }) else {
            return;
        };
        let mut conv = self.text();
        for p in self.params.iter_mut() {
            if let Some(get) = ext.get_value {
                let mut v = p.value;
                if unsafe { get(self.plugin, p.id, &mut v) } {
                    p.value = v;
                }
            }
            p.value_text = conv.value_to_text(p.id, p.value).unwrap_or_default();
        }
    }

    fn scan_params(&mut self) {
        self.params.clear();
        self.cookies.clear();
        let Some(ext) = (unsafe { self.ext.params.as_ref() }) else {
            return;
        };
        let count = ext.count.map(|c| unsafe { c(self.plugin) }).unwrap_or(0);
        let mut conv = self.text();
        for i in 0..count {
            let mut info: clap_param_info = unsafe { std::mem::zeroed() };
            let ok = ext.get_info.map(|g| unsafe { g(self.plugin, i, &mut info) }).unwrap_or(false);
            if !ok {
                continue;
            }
            let name = unsafe { cs(info.name.as_ptr()) };
            let module = unsafe { cs(info.module.as_ptr()) };
            let default_text = conv.value_to_text(info.id, info.default_value).unwrap_or_default();
            let f = info.flags;
            self.params.push(ParamInfo {
                id: info.id,
                name,
                module,
                min: info.min_value,
                max: info.max_value,
                default: info.default_value,
                value: info.default_value,
                unit: infer_unit(&default_text),
                default_text,
                value_text: String::new(),
                stepped: f & CLAP_PARAM_IS_STEPPED != 0,
                hidden: f & CLAP_PARAM_IS_HIDDEN != 0,
                readonly: f & CLAP_PARAM_IS_READONLY != 0,
                bypass: f & CLAP_PARAM_IS_BYPASS != 0,
                automatable: f & CLAP_PARAM_IS_AUTOMATABLE != 0,
            });
            self.cookies.push(info.cookie);
        }
        self.read_params();
    }

    fn scan_ports(&mut self, is_input: bool) -> Vec<PortInfo> {
        let mut out = Vec::new();
        if let Some(ext) = unsafe { self.ext.audio_ports.as_ref() } {
            let n = ext.count.map(|c| unsafe { c(self.plugin, is_input) }).unwrap_or(0);
            for i in 0..n {
                let mut info: clap_audio_port_info = unsafe { std::mem::zeroed() };
                if ext.get.map(|g| unsafe { g(self.plugin, i, is_input, &mut info) }).unwrap_or(false) {
                    out.push(PortInfo {
                        name: unsafe { cs(info.name.as_ptr()) },
                        channels: info.channel_count,
                        main: info.flags & CLAP_AUDIO_PORT_IS_MAIN != 0,
                    });
                }
            }
        }
        out
    }

    fn scan_note_ports(&mut self, is_input: bool) -> Vec<NotePortInfo> {
        let mut out = Vec::new();
        if let Some(ext) = unsafe { self.ext.note_ports.as_ref() } {
            let n = ext.count.map(|c| unsafe { c(self.plugin, is_input) }).unwrap_or(0);
            for i in 0..n {
                let mut info: clap_note_port_info = unsafe { std::mem::zeroed() };
                if ext.get.map(|g| unsafe { g(self.plugin, i, is_input, &mut info) }).unwrap_or(false) {
                    let d = info.supported_dialects;
                    if is_input && i == 0 {
                        self.note_dialects = d;
                    }
                    let mut dialects = Vec::new();
                    for (bit, name) in [
                        (CLAP_NOTE_DIALECT_CLAP, "clap"),
                        (CLAP_NOTE_DIALECT_MIDI, "midi"),
                        (CLAP_NOTE_DIALECT_MIDI_MPE, "midi-mpe"),
                        (CLAP_NOTE_DIALECT_MIDI2, "midi2"),
                    ] {
                        if d & bit != 0 {
                            dialects.push(name.to_string());
                        }
                    }
                    out.push(NotePortInfo { name: unsafe { cs(info.name.as_ptr()) }, dialects });
                }
            }
        }
        out
    }

    fn unload(&mut self) {
        self.deactivate();
        if !self.plugin.is_null() {
            let p = unsafe { &*self.plugin };
            if let Some(d) = p.destroy {
                unsafe { d(self.plugin) };
            }
            self.plugin = null();
        }
        self.ext = Exts::default();
        self.params.clear();
        self.cookies.clear();
        self.presets = None;
    }

    fn values(&self, ids: Option<&[clap_id]>) -> Vec<ParamValue> {
        self.params
            .iter()
            .filter(|p| ids.map(|ids| ids.contains(&p.id)).unwrap_or(true))
            .map(|p| ParamValue { id: p.id, name: p.name.clone(), value: p.value, text: p.value_text.clone() })
            .collect()
    }
}

impl Drop for ClapBackend {
    fn drop(&mut self) {
        self.unload();
        self.module = None;
    }
}

impl Backend for ClapBackend {
    fn host_name(&self) -> &'static str {
        "ferraus-plughost"
    }

    fn formats(&self) -> Vec<String> {
        vec!["clap".into()]
    }

    fn probe(&mut self, path: &Path) -> HostResult<Vec<Descriptor>> {
        let m = self.module_for(path)?;
        Ok(m.descriptors()?.into_iter().map(|(d, _)| d).collect())
    }

    fn load(&mut self, path: &Path, plugin_id: Option<&str>) -> HostResult<PluginInfo> {
        self.unload();
        let host_ptr: *const clap_host = &self.host.clap;
        let m = self.module_for(path)?;
        let descs = m.descriptors()?;
        let (desc, _) = match plugin_id {
            Some(id) => descs.iter().find(|(d, _)| d.id == id).cloned().ok_or_else(|| {
                let ids: Vec<&str> = descs.iter().map(|(d, _)| d.id.as_str()).collect();
                load_err(format!("plugin id '{id}' is not in {} (it contains: {})", path.display(), ids.join(", ")))
            })?,
            None if descs.len() == 1 => descs[0].clone(),
            None => {
                let ids: Vec<&str> = descs.iter().map(|(d, _)| d.id.as_str()).collect();
                return Err(load_err(format!(
                    "{} contains {} plugins; choose one by id: {}",
                    path.display(),
                    descs.len(),
                    ids.join(", ")
                )));
            }
        };
        let factory = m.plugin_factory()?;
        let create = factory.create_plugin.ok_or_else(|| load_err("factory has no create_plugin"))?;
        let cid = cstring(&desc.id);
        let plugin = unsafe { create(factory, host_ptr, cid.as_ptr()) };
        if plugin.is_null() {
            return Err(load_err(format!("the factory refused to create '{}'", desc.id)));
        }
        self.plugin = plugin;
        self.plugin_id = desc.id.clone();
        let p = unsafe { &*plugin };
        let ok = p.init.map(|i| unsafe { i(plugin) }).unwrap_or(false);
        if !ok {
            if let Some(d) = p.destroy {
                unsafe { d(plugin) };
            }
            self.plugin = null();
            return Err(load_err(format!("'{}' failed to initialize", desc.name)));
        }
        self.ext = Exts {
            params: self.ext(CLAP_EXT_PARAMS),
            state: self.ext(CLAP_EXT_STATE),
            latency: self.ext(CLAP_EXT_LATENCY),
            tail: self.ext(CLAP_EXT_TAIL),
            audio_ports: self.ext(CLAP_EXT_AUDIO_PORTS),
            note_ports: self.ext(CLAP_EXT_NOTE_PORTS),
            preset_load: {
                let a: *const clap_plugin_preset_load = self.ext(CLAP_EXT_PRESET_LOAD);
                if a.is_null() {
                    self.ext(CLAP_EXT_PRESET_LOAD_COMPAT)
                } else {
                    a
                }
            },
        };
        self.pump();
        self.scan_params();
        let audio_inputs = self.scan_ports(true);
        let audio_outputs = self.scan_ports(false);
        self.note_dialects = 0;
        let note_inputs = self.scan_note_ports(true);
        let note_outputs = self.scan_note_ports(false);
        let pick_main = |ports: &[PortInfo]| -> Option<usize> {
            ports.iter().position(|p| p.main).or(if ports.is_empty() { None } else { Some(0) })
        };
        self.main_in = pick_main(&audio_inputs);
        self.main_out = pick_main(&audio_outputs);
        self.in_ports = audio_inputs.iter().map(|p| p.channels).collect();
        self.out_ports = audio_outputs.iter().map(|p| p.channels).collect();
        let has_discovery = {
            let m = self.module.as_ref().unwrap();
            !m.factory(CLAP_PRESET_DISCOVERY_FACTORY_ID).is_null()
                || !m.factory(CLAP_PRESET_DISCOVERY_FACTORY_ID_COMPAT).is_null()
        };
        let (mi, mo) = self.main_channels();
        Ok(PluginInfo {
            descriptor: desc,
            format: "clap".into(),
            params: self.params.clone(),
            audio_inputs,
            audio_outputs,
            note_inputs,
            note_outputs,
            main_in_channels: mi,
            main_out_channels: mo,
            has_state: !self.ext.state.is_null(),
            has_presets: has_discovery && !self.ext.preset_load.is_null(),
        })
    }

    fn get_params(&mut self) -> HostResult<Vec<ParamValue>> {
        self.plugin()?;
        self.pump();
        self.read_params();
        Ok(self.values(None))
    }

    fn set_params(&mut self, assigns: &[ParamAssign]) -> HostResult<Vec<ParamValue>> {
        self.plugin()?;
        let mut list = EventList::default();
        let mut ids = Vec::new();
        for a in assigns {
            let p = find_param(&self.params, &a.name).map_err(|m| HostError::new("unknown_param", m))?.clone();
            if p.readonly {
                return Err(HostError::new("bad_value", format!("parameter '{}' is read-only", p.name)));
            }
            let mut conv = self.text();
            let v = resolve_value(&p, &a.value, &mut conv).map_err(|m| HostError::new("bad_value", m))?;
            let idx = self.params.iter().position(|x| x.id == p.id).unwrap();
            let cookie = self.cookies[idx];
            list.param(0, p.id, cookie, v);
            self.pending.retain(|(id, _, _)| *id != p.id);
            self.pending.push((p.id, cookie, v));
            ids.push(p.id);
        }
        if let Some(flush) = unsafe { self.ext.params.as_ref() }.and_then(|e| e.flush) {
            let input = list.input();
            unsafe { flush(self.plugin, &input, &OUT_EVENTS) };
        }
        self.pump();
        self.read_params();
        Ok(self.values(Some(&ids)))
    }

    fn list_presets(&mut self) -> HostResult<Vec<PresetInfo>> {
        self.plugin()?;
        if self.presets.is_none() {
            let m = self.module.as_ref().unwrap();
            self.presets = Some(discover_presets(m, &self.plugin_id));
        }
        Ok(self.presets.clone().unwrap_or_default())
    }

    fn load_preset(&mut self, preset: &str) -> HostResult<Vec<ParamValue>> {
        let presets = self.list_presets()?;
        let Some(ext) = (unsafe { self.ext.preset_load.as_ref() }) else {
            return Err(HostError::new("unsupported", "this plugin does not implement CLAP preset loading"));
        };
        let found = presets
            .iter()
            .find(|p| p.name == preset)
            .or_else(|| presets.iter().find(|p| p.name.eq_ignore_ascii_case(preset)))
            .or_else(|| presets.iter().find(|p| p.load_key.as_deref() == Some(preset)))
            .or_else(|| presets.iter().find(|p| p.location.as_deref() == Some(preset)))
            .cloned();
        // A preset file the plugin's provider did not list can still be loaded by path.
        let found = match found {
            Some(f) => f,
            None if Path::new(preset).is_file() => PresetInfo {
                name: preset.to_string(),
                location_kind: "file".into(),
                location: Some(preset.to_string()),
                ..Default::default()
            },
            None => {
                let names: Vec<&str> = presets.iter().take(20).map(|p| p.name.as_str()).collect();
                return Err(HostError::new(
                    "unknown_preset",
                    format!(
                        "no preset named '{preset}'{}",
                        if names.is_empty() {
                            String::new()
                        } else {
                            format!("; presets include: {}", names.join(", "))
                        }
                    ),
                ));
            }
        };
        let kind = if found.location_kind == "plugin" {
            CLAP_PRESET_DISCOVERY_LOCATION_PLUGIN
        } else {
            CLAP_PRESET_DISCOVERY_LOCATION_FILE
        };
        let loc = found.location.as_deref().map(cstring);
        let key = found.load_key.as_deref().map(cstring);
        let f = ext.from_location.ok_or_else(|| HostError::new("unsupported", "preset-load has no from_location"))?;
        let ok = unsafe {
            f(
                self.plugin,
                kind,
                loc.as_ref().map(|c| c.as_ptr()).unwrap_or(null()),
                key.as_ref().map(|c| c.as_ptr()).unwrap_or(null()),
            )
        };
        if !ok {
            return Err(HostError::new("preset_failed", format!("the plugin could not load preset '{}'", found.name)));
        }
        self.pending.clear();
        self.pump();
        self.read_params();
        Ok(self.values(None))
    }

    fn get_state(&mut self) -> HostResult<Vec<u8>> {
        self.plugin()?;
        let Some(ext) = (unsafe { self.ext.state.as_ref() }) else {
            return Err(HostError::new("no_state", "this plugin has no state extension"));
        };
        let mut data: Vec<u8> = Vec::new();
        let stream = clap_ostream { ctx: &mut data as *mut Vec<u8> as *mut c_void, write: Some(ostream_write) };
        let ok = ext.save.map(|s| unsafe { s(self.plugin, &stream) }).unwrap_or(false);
        if !ok {
            return Err(HostError::new("no_state", "the plugin failed to save its state"));
        }
        Ok(data)
    }

    fn set_state(&mut self, state: &[u8]) -> HostResult<()> {
        self.plugin()?;
        let Some(ext) = (unsafe { self.ext.state.as_ref() }) else {
            return Err(HostError::new("no_state", "this plugin has no state extension"));
        };
        let mut st = IStream { data: state, pos: 0 };
        let stream = clap_istream { ctx: &mut st as *mut IStream as *mut c_void, read: Some(istream_read) };
        let ok = ext.load.map(|l| unsafe { l(self.plugin, &stream) }).unwrap_or(false);
        if !ok {
            return Err(HostError::new(
                "bad_state",
                "the plugin rejected the state (saved by a different plugin or version?)",
            ));
        }
        self.pending.clear();
        self.pump();
        self.read_params();
        Ok(())
    }

    fn activate(&mut self, sample_rate: f64, block_size: u32) -> HostResult<Activation> {
        self.plugin()?;
        self.deactivate();
        let p = unsafe { &*self.plugin };
        self.sample_rate = sample_rate;
        self.in_bufs = self.in_ports.iter().map(|&c| vec![vec![0.0f32; block_size as usize]; c as usize]).collect();
        self.out_bufs = self.out_ports.iter().map(|&c| vec![vec![0.0f32; block_size as usize]; c as usize]).collect();
        let ok = p.activate.map(|a| unsafe { a(self.plugin, sample_rate, 1, block_size) }).unwrap_or(false);
        if !ok {
            return Err(HostError::new(
                "activate_failed",
                format!("the plugin refused to activate at {sample_rate} Hz / {block_size} frames"),
            ));
        }
        self.active = true;
        self.host.in_process.store(true, Ordering::SeqCst);
        let started = p.start_processing.map(|s| unsafe { s(self.plugin) }).unwrap_or(true);
        self.host.in_process.store(false, Ordering::SeqCst);
        if !started {
            return Err(HostError::new("activate_failed", "start_processing failed"));
        }
        self.processing = true;
        self.steady = 0;
        self.pump();
        let latency =
            unsafe { self.ext.latency.as_ref() }.and_then(|l| l.get).map(|g| unsafe { g(self.plugin) }).unwrap_or(0);
        let tail = unsafe { self.ext.tail.as_ref() }.and_then(|t| t.get).map(|g| unsafe { g(self.plugin) });
        Ok(Activation { latency, tail })
    }

    fn process(
        &mut self,
        inputs: &[&[f32]],
        outputs: &mut [&mut [f32]],
        frames: u32,
        events: &[Event],
    ) -> HostResult<()> {
        if !self.processing {
            return Err(HostError::new("bad_request", "process before activate"));
        }
        let n = frames as usize;
        for (pi, port) in self.in_bufs.iter_mut().enumerate() {
            for (c, ch) in port.iter_mut().enumerate() {
                ch[..n].fill(0.0);
                if Some(pi) == self.main_in {
                    if let Some(src) = inputs.get(c) {
                        ch[..n].copy_from_slice(&src[..n]);
                    }
                }
            }
        }
        for port in self.out_bufs.iter_mut() {
            for ch in port.iter_mut() {
                ch[..n].fill(0.0);
            }
        }
        let mut in_ptrs: Vec<Vec<*mut f32>> =
            self.in_bufs.iter_mut().map(|p| p.iter_mut().map(|c| c.as_mut_ptr()).collect()).collect();
        let mut out_ptrs: Vec<Vec<*mut f32>> =
            self.out_bufs.iter_mut().map(|p| p.iter_mut().map(|c| c.as_mut_ptr()).collect()).collect();
        let mk = |ptrs: &mut Vec<*mut f32>| clap_sys::audio_buffer::clap_audio_buffer {
            data32: if ptrs.is_empty() { null_mut() } else { ptrs.as_mut_ptr() },
            data64: null_mut(),
            channel_count: ptrs.len() as u32,
            latency: 0,
            constant_mask: 0,
        };
        let in_audio: Vec<_> = in_ptrs.iter_mut().map(mk).collect();
        let mut out_audio: Vec<_> = out_ptrs.iter_mut().map(mk).collect();

        let mut list = EventList::default();
        for (id, cookie, v) in self.pending.drain(..) {
            list.param(0, id, cookie, v);
        }
        let use_midi =
            self.note_dialects & CLAP_NOTE_DIALECT_CLAP == 0 && self.note_dialects & CLAP_NOTE_DIALECT_MIDI != 0;
        let mut sorted: Vec<Event> = events.to_vec();
        sorted.sort_by_key(|e| e.time());
        for e in sorted {
            match e {
                Event::NoteOn { time, channel, key, velocity } => {
                    if use_midi {
                        let v = (velocity.clamp(0.0, 1.0) * 127.0).round().max(1.0) as u8;
                        list.midi(time.min(frames - 1), [0x90 | (channel & 15), key & 127, v]);
                    } else {
                        list.note(time.min(frames - 1), true, channel, key, velocity);
                    }
                }
                Event::NoteOff { time, channel, key, velocity } => {
                    if use_midi {
                        let v = (velocity.clamp(0.0, 1.0) * 127.0).round() as u8;
                        list.midi(time.min(frames - 1), [0x80 | (channel & 15), key & 127, v]);
                    } else {
                        list.note(time.min(frames - 1), false, channel, key, velocity);
                    }
                }
                Event::Midi { time, data } => list.midi(time.min(frames - 1), data),
            }
        }
        let input = list.input();

        let secs = self.steady as f64 / self.sample_rate;
        let beats = secs * 2.0; // 120 BPM
        let transport = clap_event_transport {
            header: header(std::mem::size_of::<clap_event_transport>(), 0, CLAP_EVENT_TRANSPORT),
            flags: CLAP_TRANSPORT_HAS_TEMPO
                | CLAP_TRANSPORT_HAS_BEATS_TIMELINE
                | CLAP_TRANSPORT_HAS_SECONDS_TIMELINE
                | CLAP_TRANSPORT_HAS_TIME_SIGNATURE
                | CLAP_TRANSPORT_IS_PLAYING,
            song_pos_beats: (beats * CLAP_BEATTIME_FACTOR as f64) as i64,
            song_pos_seconds: (secs * CLAP_SECTIME_FACTOR as f64) as i64,
            tempo: 120.0,
            tempo_inc: 0.0,
            loop_start_beats: 0,
            loop_end_beats: 0,
            loop_start_seconds: 0,
            loop_end_seconds: 0,
            bar_start: ((beats / 4.0).floor() * 4.0 * CLAP_BEATTIME_FACTOR as f64) as i64,
            bar_number: (beats / 4.0).floor() as i32,
            tsig_num: 4,
            tsig_denom: 4,
        };
        let proc_ = clap_process {
            steady_time: self.steady,
            frames_count: frames,
            transport: &transport,
            audio_inputs: if in_audio.is_empty() { null() } else { in_audio.as_ptr() },
            audio_outputs: if out_audio.is_empty() { null_mut() } else { out_audio.as_mut_ptr() },
            audio_inputs_count: in_audio.len() as u32,
            audio_outputs_count: out_audio.len() as u32,
            in_events: &input,
            out_events: &OUT_EVENTS,
        };
        let p = unsafe { &*self.plugin };
        let f = p.process.ok_or_else(|| HostError::new("process_failed", "plugin has no process()"))?;
        self.host.in_process.store(true, Ordering::SeqCst);
        let status = unsafe { f(self.plugin, &proc_) };
        self.host.in_process.store(false, Ordering::SeqCst);
        if status == CLAP_PROCESS_ERROR {
            return Err(HostError::new("process_failed", "the plugin returned CLAP_PROCESS_ERROR"));
        }
        if let Some(mo) = self.main_out {
            for (c, out) in outputs.iter_mut().enumerate() {
                if let Some(src) = self.out_bufs[mo].get(c) {
                    out[..n].copy_from_slice(&src[..n]);
                }
            }
        }
        self.steady += frames as i64;
        self.pump();
        Ok(())
    }

    fn deactivate(&mut self) {
        if self.plugin.is_null() {
            return;
        }
        let p = unsafe { &*self.plugin };
        if self.processing {
            self.host.in_process.store(true, Ordering::SeqCst);
            if let Some(s) = p.stop_processing {
                unsafe { s(self.plugin) };
            }
            self.host.in_process.store(false, Ordering::SeqCst);
            self.processing = false;
        }
        if self.active {
            if let Some(d) = p.deactivate {
                unsafe { d(self.plugin) };
            }
            self.active = false;
        }
    }

    fn main_channels(&self) -> (u32, u32) {
        (self.main_in.map(|i| self.in_ports[i]).unwrap_or(0), self.main_out.map(|i| self.out_ports[i]).unwrap_or(0))
    }
}
