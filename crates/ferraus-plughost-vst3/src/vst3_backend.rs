//! VST3 backend over the `vst3` crate's raw COM bindings. Single-threaded and offline:
//! the host thread is both the UI/main thread and the processing thread.
//!
//! State blobs are a small container so component and controller state travel together:
//! `b"FV3S"`, `u32` version (1), `u32` length + component state, `u32` length + controller
//! state (little-endian). A blob without the magic is treated as raw component state.

// The bindings' enum and string types differ by platform (e.g. i32 vs u32 enums on
// Windows), so casts that are no-ops here are needed elsewhere.
#![allow(non_snake_case, non_upper_case_globals, clippy::missing_safety_doc, clippy::unnecessary_cast)]

use ferraus_plugins::host_rt::{Activation, Backend, HostError, HostResult};
use ferraus_plugins::protocol::{
    find_param, infer_unit, resolve_value, Descriptor, Event as PEvent, NotePortInfo, ParamAssign, ParamInfo,
    ParamText, ParamValue as PValue, PluginInfo, PortInfo, PresetInfo,
};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::{c_char, c_void, CStr, CString};
use std::mem::ManuallyDrop;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComPtr, ComRef, ComWrapper, Interface};
use IAttributeList_::AttrID;

fn load_err(msg: impl Into<String>) -> HostError {
    HostError::new("load_failed", msg)
}

fn w2s(w: &[TChar]) -> String {
    let n = w.iter().position(|c| *c == 0).unwrap_or(w.len());
    String::from_utf16_lossy(&w[..n].iter().map(|c| *c as u16).collect::<Vec<u16>>())
}

fn c2s(c: &[c_char]) -> String {
    let n = c.iter().position(|b| *b == 0).unwrap_or(c.len());
    String::from_utf8_lossy(&c[..n].iter().map(|b| *b as u8).collect::<Vec<u8>>()).into_owned()
}

fn s2w(s: &str) -> Vec<TChar> {
    let mut v: Vec<TChar> = s.encode_utf16().map(|c| c as TChar).collect();
    v.push(0);
    v
}

fn ok(r: tresult) -> bool {
    r == kResultOk
}

/// TUID as 32 upper-case hex digits (the byte order of the bundle's memory layout).
fn cid_hex(t: &TUID) -> String {
    t.iter().map(|b| format!("{:02X}", *b as u8)).collect()
}

fn iid_ptr<I: Interface>() -> FIDString {
    I::IID.as_ptr() as FIDString
}

fn same_iid<I: Interface>(p: *const TUID) -> bool {
    if p.is_null() {
        return false;
    }
    let a = unsafe { &*(p as *const [u8; 16]) };
    a == &I::IID
}

// ---------------------------------------------------------------------------------------
// COM objects the host provides

struct MemStream {
    data: RefCell<Vec<u8>>,
    pos: Cell<usize>,
}

impl Class for MemStream {
    type Interfaces = (IBStream,);
}

impl MemStream {
    fn new(data: Vec<u8>) -> ComWrapper<MemStream> {
        ComWrapper::new(MemStream { data: RefCell::new(data), pos: Cell::new(0) })
    }
}

impl IBStreamTrait for MemStream {
    unsafe fn read(&self, buffer: *mut c_void, num: int32, read: *mut int32) -> tresult {
        let d = self.data.borrow();
        let pos = self.pos.get().min(d.len());
        let n = (num.max(0) as usize).min(d.len() - pos);
        if n > 0 {
            std::ptr::copy_nonoverlapping(d.as_ptr().add(pos), buffer as *mut u8, n);
        }
        self.pos.set(pos + n);
        if !read.is_null() {
            *read = n as int32;
        }
        kResultOk
    }

    unsafe fn write(&self, buffer: *mut c_void, num: int32, written: *mut int32) -> tresult {
        let n = num.max(0) as usize;
        let mut d = self.data.borrow_mut();
        let pos = self.pos.get();
        if d.len() < pos + n {
            d.resize(pos + n, 0);
        }
        if n > 0 {
            std::ptr::copy_nonoverlapping(buffer as *const u8, d.as_mut_ptr().add(pos), n);
        }
        self.pos.set(pos + n);
        if !written.is_null() {
            *written = n as int32;
        }
        kResultOk
    }

    unsafe fn seek(&self, pos: int64, mode: int32, result: *mut int64) -> tresult {
        let len = self.data.borrow().len() as i64;
        let base = match mode as u32 {
            m if m == IBStream_::IStreamSeekMode_::kIBSeekSet as u32 => 0,
            m if m == IBStream_::IStreamSeekMode_::kIBSeekCur as u32 => self.pos.get() as i64,
            m if m == IBStream_::IStreamSeekMode_::kIBSeekEnd as u32 => len,
            _ => return kInvalidArgument,
        };
        let p = (base + pos).max(0);
        self.pos.set(p as usize);
        if !result.is_null() {
            *result = p;
        }
        kResultOk
    }

    unsafe fn tell(&self, pos: *mut int64) -> tresult {
        if !pos.is_null() {
            *pos = self.pos.get() as int64;
        }
        kResultOk
    }
}

enum Attr {
    Int(i64),
    Float(f64),
    Str(Vec<TChar>),
    Bin(Vec<u8>),
}

struct AttrList {
    map: RefCell<HashMap<String, Attr>>,
}

impl Class for AttrList {
    type Interfaces = (IAttributeList,);
}

unsafe fn attr_key(id: AttrID) -> String {
    if id.is_null() {
        String::new()
    } else {
        CStr::from_ptr(id).to_string_lossy().into_owned()
    }
}

impl IAttributeListTrait for AttrList {
    unsafe fn setInt(&self, id: AttrID, value: int64) -> tresult {
        self.map.borrow_mut().insert(attr_key(id), Attr::Int(value));
        kResultOk
    }
    unsafe fn getInt(&self, id: AttrID, value: *mut int64) -> tresult {
        match self.map.borrow().get(&attr_key(id)) {
            Some(Attr::Int(v)) => {
                *value = *v;
                kResultOk
            }
            _ => kResultFalse,
        }
    }
    unsafe fn setFloat(&self, id: AttrID, value: f64) -> tresult {
        self.map.borrow_mut().insert(attr_key(id), Attr::Float(value));
        kResultOk
    }
    unsafe fn getFloat(&self, id: AttrID, value: *mut f64) -> tresult {
        match self.map.borrow().get(&attr_key(id)) {
            Some(Attr::Float(v)) => {
                *value = *v;
                kResultOk
            }
            _ => kResultFalse,
        }
    }
    unsafe fn setString(&self, id: AttrID, s: *const TChar) -> tresult {
        let mut v = Vec::new();
        if !s.is_null() {
            let mut i = 0;
            while *s.add(i) != 0 && i < 1 << 20 {
                v.push(*s.add(i));
                i += 1;
            }
        }
        v.push(0);
        self.map.borrow_mut().insert(attr_key(id), Attr::Str(v));
        kResultOk
    }
    unsafe fn getString(&self, id: AttrID, s: *mut TChar, size_bytes: uint32) -> tresult {
        match self.map.borrow().get(&attr_key(id)) {
            Some(Attr::Str(v)) => {
                let cap = size_bytes as usize / 2;
                if cap == 0 {
                    return kResultFalse;
                }
                let n = v.len().min(cap);
                std::ptr::copy_nonoverlapping(v.as_ptr(), s, n);
                *s.add(n - 1) = 0;
                kResultOk
            }
            _ => kResultFalse,
        }
    }
    unsafe fn setBinary(&self, id: AttrID, data: *const c_void, size: uint32) -> tresult {
        let v = if data.is_null() {
            Vec::new()
        } else {
            std::slice::from_raw_parts(data as *const u8, size as usize).to_vec()
        };
        self.map.borrow_mut().insert(attr_key(id), Attr::Bin(v));
        kResultOk
    }
    unsafe fn getBinary(&self, id: AttrID, data: *mut *const c_void, size: *mut uint32) -> tresult {
        match self.map.borrow().get(&attr_key(id)) {
            Some(Attr::Bin(v)) => {
                *data = v.as_ptr() as *const c_void;
                *size = v.len() as uint32;
                kResultOk
            }
            _ => kResultFalse,
        }
    }
}

struct Message {
    id: RefCell<CString>,
    attrs: ComWrapper<AttrList>,
}

impl Class for Message {
    type Interfaces = (IMessage,);
}

impl IMessageTrait for Message {
    unsafe fn getMessageID(&self) -> FIDString {
        self.id.borrow().as_ptr()
    }
    unsafe fn setMessageID(&self, id: FIDString) {
        let s = if id.is_null() { CString::default() } else { CStr::from_ptr(id).to_owned() };
        *self.id.borrow_mut() = s;
    }
    unsafe fn getAttributes(&self) -> *mut IAttributeList {
        self.attrs.as_com_ref::<IAttributeList>().map(|r| r.as_ptr()).unwrap_or(null_mut())
    }
}

struct HostApp;

impl Class for HostApp {
    type Interfaces = (IHostApplication,);
}

unsafe fn query<C: Class>(w: &ComWrapper<C>, iid: *mut TUID, obj: *mut *mut c_void) -> tresult
where
    ComWrapper<C>: Sized,
{
    match w.to_com_ptr::<FUnknown>() {
        Some(u) => {
            let p = u.as_ptr();
            ((*(*p).vtbl).queryInterface)(p, iid, obj)
        }
        None => kNoInterface,
    }
}

impl IHostApplicationTrait for HostApp {
    unsafe fn getName(&self, name: *mut String128) -> tresult {
        let w = s2w("ferraus");
        let dst = &mut *name;
        for (i, c) in w.iter().take(127).enumerate() {
            dst[i] = *c;
        }
        dst[w.len().min(127)] = 0;
        kResultOk
    }
    unsafe fn createInstance(&self, cid: *mut TUID, iid: *mut TUID, obj: *mut *mut c_void) -> tresult {
        if obj.is_null() {
            return kInvalidArgument;
        }
        *obj = null_mut();
        if same_iid::<IMessage>(cid) {
            let m = ComWrapper::new(Message {
                id: RefCell::new(CString::default()),
                attrs: ComWrapper::new(AttrList { map: RefCell::new(HashMap::new()) }),
            });
            return query(&m, iid, obj);
        }
        if same_iid::<IAttributeList>(cid) {
            let a = ComWrapper::new(AttrList { map: RefCell::new(HashMap::new()) });
            return query(&a, iid, obj);
        }
        kNoInterface
    }
}

struct Handler;

impl Class for Handler {
    type Interfaces = (IComponentHandler,);
}

impl IComponentHandlerTrait for Handler {
    unsafe fn beginEdit(&self, _id: ParamID) -> tresult {
        kResultOk
    }
    unsafe fn performEdit(&self, _id: ParamID, _v: ParamValue) -> tresult {
        kResultOk
    }
    unsafe fn endEdit(&self, _id: ParamID) -> tresult {
        kResultOk
    }
    unsafe fn restartComponent(&self, _flags: int32) -> tresult {
        kResultOk
    }
}

struct EventList {
    events: RefCell<Vec<Event>>,
}

impl Class for EventList {
    type Interfaces = (IEventList,);
}

impl IEventListTrait for EventList {
    unsafe fn getEventCount(&self) -> int32 {
        self.events.borrow().len() as int32
    }
    unsafe fn getEvent(&self, index: int32, e: *mut Event) -> tresult {
        match self.events.borrow().get(index as usize) {
            Some(ev) => {
                *e = *ev;
                kResultOk
            }
            None => kInvalidArgument,
        }
    }
    unsafe fn addEvent(&self, e: *mut Event) -> tresult {
        if !e.is_null() {
            self.events.borrow_mut().push(*e);
        }
        kResultOk
    }
}

struct ParamQueue {
    id: ParamID,
    points: RefCell<Vec<(int32, f64)>>,
}

impl Class for ParamQueue {
    type Interfaces = (IParamValueQueue,);
}

impl IParamValueQueueTrait for ParamQueue {
    unsafe fn getParameterId(&self) -> ParamID {
        self.id
    }
    unsafe fn getPointCount(&self) -> int32 {
        self.points.borrow().len() as int32
    }
    unsafe fn getPoint(&self, index: int32, offset: *mut int32, value: *mut ParamValue) -> tresult {
        match self.points.borrow().get(index as usize) {
            Some((o, v)) => {
                *offset = *o;
                *value = *v;
                kResultOk
            }
            None => kInvalidArgument,
        }
    }
    unsafe fn addPoint(&self, offset: int32, value: ParamValue, index: *mut int32) -> tresult {
        let mut p = self.points.borrow_mut();
        p.push((offset, value));
        if !index.is_null() {
            *index = p.len() as int32 - 1;
        }
        kResultOk
    }
}

struct ParamChanges {
    queues: RefCell<Vec<ComWrapper<ParamQueue>>>,
}

impl Class for ParamChanges {
    type Interfaces = (IParameterChanges,);
}

impl ParamChanges {
    fn new() -> ComWrapper<ParamChanges> {
        ComWrapper::new(ParamChanges { queues: RefCell::new(Vec::new()) })
    }
}

impl IParameterChangesTrait for ParamChanges {
    unsafe fn getParameterCount(&self) -> int32 {
        self.queues.borrow().len() as int32
    }
    unsafe fn getParameterData(&self, index: int32) -> *mut IParamValueQueue {
        self.queues
            .borrow()
            .get(index as usize)
            .and_then(|q| q.as_com_ref::<IParamValueQueue>().map(|r| r.as_ptr()))
            .unwrap_or(null_mut())
    }
    unsafe fn addParameterData(&self, id: *const ParamID, index: *mut int32) -> *mut IParamValueQueue {
        if id.is_null() {
            return null_mut();
        }
        let mut qs = self.queues.borrow_mut();
        let i = match qs.iter().position(|q| q.id == *id) {
            Some(i) => i,
            None => {
                qs.push(ComWrapper::new(ParamQueue { id: *id, points: RefCell::new(Vec::new()) }));
                qs.len() - 1
            }
        };
        if !index.is_null() {
            *index = i as int32;
        }
        qs[i].as_com_ref::<IParamValueQueue>().map(|r| r.as_ptr()).unwrap_or(null_mut())
    }
}

// ---------------------------------------------------------------------------------------
// module loading

/// The shared library inside a `.vst3` bundle for this OS (or the file itself).
pub fn vst3_binary(bundle: &Path) -> HostResult<PathBuf> {
    if bundle.is_file() {
        return Ok(bundle.to_path_buf());
    }
    if !bundle.is_dir() {
        return Err(load_err(format!("{} does not exist", bundle.display())));
    }
    let stem = bundle.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let contents = bundle.join("Contents");
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => {
            if cfg!(windows) {
                "arm64"
            } else {
                "aarch64"
            }
        }
        "x86" => "i386",
        a => a,
    };
    let mut candidates: Vec<PathBuf> = Vec::new();
    if cfg!(target_os = "macos") {
        let macos = contents.join("MacOS");
        if let Ok(text) = std::fs::read_to_string(contents.join("Info.plist")) {
            let k = "<key>CFBundleExecutable</key>";
            if let Some(i) = text.find(k) {
                let rest = &text[i + k.len()..];
                if let (Some(a), Some(b)) = (rest.find("<string>"), rest.find("</string>")) {
                    candidates.push(macos.join(rest[a + 8..b].trim()));
                }
            }
        }
        candidates.push(macos.join(&stem));
    } else if cfg!(windows) {
        candidates.push(contents.join(format!("{arch}-win")).join(format!("{stem}.vst3")));
        candidates.push(contents.join("x86_64-win").join(format!("{stem}.vst3")));
    } else {
        candidates.push(contents.join(format!("{arch}-linux")).join(format!("{stem}.so")));
    }
    for c in &candidates {
        if c.is_file() {
            return Ok(c.clone());
        }
    }
    Err(load_err(format!(
        "{} has no binary for this platform (looked for {})",
        bundle.display(),
        candidates.iter().map(|c| c.display().to_string()).collect::<Vec<_>>().join(", ")
    )))
}

#[cfg(target_os = "macos")]
mod cf {
    use std::ffi::c_void;
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        pub fn CFURLCreateFromFileSystemRepresentation(
            alloc: *const c_void,
            buf: *const u8,
            len: isize,
            is_dir: u8,
        ) -> *const c_void;
        pub fn CFBundleCreate(alloc: *const c_void, url: *const c_void) -> *mut c_void;
        pub fn CFRelease(cf: *const c_void);
    }
}

type ExitFn = unsafe extern "system" fn() -> bool;

struct Module {
    factory: Option<ComPtr<IPluginFactory>>,
    exit: Option<ExitFn>,
    path: PathBuf,
    bundle_ref: *mut c_void,
    // never unloaded (see the CLAP host for why)
    _lib: ManuallyDrop<libloading::Library>,
}

impl Module {
    fn open(bundle: &Path) -> HostResult<Module> {
        let bin = vst3_binary(bundle)?;
        let mut bundle_ref: *mut c_void = null_mut();
        #[cfg(unix)]
        let (lib, handle) = unsafe {
            let l = libloading::os::unix::Library::open(
                Some(&bin),
                libloading::os::unix::RTLD_NOW | libloading::os::unix::RTLD_LOCAL,
            )
            .map_err(|e| load_err(format!("cannot load {}: {e}", bin.display())))?;
            let h = l.into_raw();
            (libloading::Library::from(libloading::os::unix::Library::from_raw(h)), h)
        };
        #[cfg(windows)]
        let (lib, handle) = unsafe {
            (
                libloading::Library::new(&bin).map_err(|e| load_err(format!("cannot load {}: {e}", bin.display())))?,
                null_mut::<c_void>(),
            )
        };
        let sym = |name: &[u8]| -> Option<*const c_void> { unsafe { lib.get::<*const c_void>(name).ok().map(|s| *s) } };
        let exit: Option<ExitFn>;
        unsafe {
            if cfg!(target_os = "macos") {
                if let Some(entry) = sym(b"bundleEntry\0").or_else(|| sym(b"BundleEntry\0")) {
                    #[cfg(target_os = "macos")]
                    {
                        let p = bundle.as_os_str().as_encoded_bytes();
                        let url = cf::CFURLCreateFromFileSystemRepresentation(null(), p.as_ptr(), p.len() as isize, 1);
                        if !url.is_null() {
                            bundle_ref = cf::CFBundleCreate(null(), url);
                            cf::CFRelease(url);
                        }
                    }
                    let f: unsafe extern "system" fn(*mut c_void) -> bool = std::mem::transmute(entry);
                    if !f(bundle_ref) {
                        return Err(load_err("the plugin's bundleEntry() failed"));
                    }
                }
                exit = sym(b"bundleExit\0").or_else(|| sym(b"BundleExit\0")).map(|p| std::mem::transmute(p));
            } else if cfg!(windows) {
                if let Some(init) = sym(b"InitDll\0") {
                    let f: ExitFn = std::mem::transmute(init);
                    if !f() {
                        return Err(load_err("the plugin's InitDll() failed"));
                    }
                }
                exit = sym(b"ExitDll\0").map(|p| std::mem::transmute(p));
            } else {
                if let Some(entry) = sym(b"ModuleEntry\0") {
                    let f: unsafe extern "system" fn(*mut c_void) -> bool = std::mem::transmute(entry);
                    if !f(handle) {
                        return Err(load_err("the plugin's ModuleEntry() failed"));
                    }
                }
                exit = sym(b"ModuleExit\0").map(|p| std::mem::transmute(p));
            }
        }
        let _ = handle;
        let get = sym(b"GetPluginFactory\0")
            .ok_or_else(|| load_err(format!("{} exports no GetPluginFactory", bin.display())))?;
        let get: unsafe extern "system" fn() -> *mut IPluginFactory = unsafe { std::mem::transmute(get) };
        let factory = unsafe { ComPtr::from_raw(get()) }.ok_or_else(|| load_err("GetPluginFactory returned null"))?;
        Ok(Module {
            factory: Some(factory),
            exit,
            path: bundle.to_path_buf(),
            bundle_ref,
            _lib: ManuallyDrop::new(lib),
        })
    }

    fn factory(&self) -> &ComPtr<IPluginFactory> {
        self.factory.as_ref().unwrap()
    }

    /// Audio module classes: (descriptor, cid).
    fn classes(&self) -> Vec<(Descriptor, TUID)> {
        let f = self.factory();
        let mut finfo: PFactoryInfo = unsafe { std::mem::zeroed() };
        let vendor = if ok(unsafe { f.getFactoryInfo(&mut finfo) }) { c2s(&finfo.vendor) } else { String::new() };
        let url = c2s(&finfo.url);
        let f2 = f.cast::<IPluginFactory2>();
        let mut out = Vec::new();
        for i in 0..unsafe { f.countClasses() } {
            let mut info: PClassInfo = unsafe { std::mem::zeroed() };
            if !ok(unsafe { f.getClassInfo(i, &mut info) }) {
                continue;
            }
            if c2s(&info.category) != "Audio Module Class" {
                continue;
            }
            let (mut subs, mut cvendor, mut version) = (String::new(), String::new(), String::new());
            if let Some(f2) = &f2 {
                let mut i2: PClassInfo2 = unsafe { std::mem::zeroed() };
                if ok(unsafe { f2.getClassInfo2(i, &mut i2) }) {
                    subs = c2s(&i2.subCategories);
                    cvendor = c2s(&i2.vendor);
                    version = c2s(&i2.version);
                }
            }
            let sub: Vec<String> =
                subs.split('|').map(|s| s.trim().to_ascii_lowercase()).filter(|s| !s.is_empty()).collect();
            let mut features = sub.clone();
            if sub.iter().any(|s| s == "instrument") {
                features.push("instrument".into());
            } else if sub.iter().any(|s| s == "fx") {
                features.push("audio-effect".into());
            }
            let kind = Descriptor::classify(&features);
            out.push((
                Descriptor {
                    id: cid_hex(&info.cid),
                    name: c2s(&info.name),
                    vendor: if cvendor.is_empty() { vendor.clone() } else { cvendor },
                    version,
                    description: String::new(),
                    url: url.clone(),
                    features,
                    kind,
                },
                info.cid,
            ));
        }
        out
    }

    fn create<I: Interface>(&self, cid: &TUID) -> Option<ComPtr<I>> {
        let mut obj: *mut c_void = null_mut();
        let r = unsafe { self.factory().createInstance(cid.as_ptr() as FIDString, iid_ptr::<I>(), &mut obj) };
        if !ok(r) || obj.is_null() {
            return None;
        }
        unsafe { ComPtr::from_raw(obj as *mut I) }
    }
}

impl Drop for Module {
    fn drop(&mut self) {
        self.factory = None;
        if let Some(exit) = self.exit {
            unsafe { exit() };
        }
        #[cfg(target_os = "macos")]
        if !self.bundle_ref.is_null() {
            unsafe { cf::CFRelease(self.bundle_ref) };
        }
    }
}

// ---------------------------------------------------------------------------------------
// the backend

struct Text<'a> {
    controller: &'a ComPtr<IEditController>,
}

impl ParamText for Text<'_> {
    fn text_to_value(&mut self, id: u32, text: &str) -> Option<f64> {
        let mut w = s2w(text);
        let mut norm = 0.0;
        if ok(unsafe { self.controller.getParamValueByString(id, w.as_mut_ptr(), &mut norm) }) {
            Some(unsafe { self.controller.normalizedParamToPlain(id, norm) })
        } else {
            None
        }
    }

    fn value_to_text(&mut self, id: u32, value: f64) -> Option<String> {
        let norm = unsafe { self.controller.plainParamToNormalized(id, value) };
        let mut s: String128 = [0; 128];
        if ok(unsafe { self.controller.getParamStringByValue(id, norm, &mut s) }) {
            Some(w2s(&s))
        } else {
            None
        }
    }
}

struct Program {
    param: u32,
    presets: Vec<PresetInfo>,
}

pub struct Vst3Backend {
    module: Option<Module>,
    host: ComWrapper<HostApp>,
    handler: ComWrapper<Handler>,
    component: Option<ComPtr<IComponent>>,
    processor: Option<ComPtr<IAudioProcessor>>,
    controller: Option<ComPtr<IEditController>>,
    separate: bool,
    connections: Option<(ComPtr<IConnectionPoint>, ComPtr<IConnectionPoint>)>,
    params: Vec<ParamInfo>,
    flags: Vec<i32>,
    pending: Vec<(u32, f64)>,
    in_buses: Vec<u32>,
    out_buses: Vec<u32>,
    main_in: Option<usize>,
    main_out: Option<usize>,
    event_buses: i32,
    active: bool,
    sample_rate: f64,
    block: u32,
    steady: i64,
    in_bufs: Vec<Vec<Vec<f32>>>,
    out_bufs: Vec<Vec<Vec<f32>>>,
    program: Option<Program>,
}

impl Default for Vst3Backend {
    fn default() -> Self {
        Self::new()
    }
}

impl Vst3Backend {
    pub fn new() -> Self {
        Vst3Backend {
            module: None,
            host: ComWrapper::new(HostApp),
            handler: ComWrapper::new(Handler),
            component: None,
            processor: None,
            controller: None,
            separate: false,
            connections: None,
            params: Vec::new(),
            flags: Vec::new(),
            pending: Vec::new(),
            in_buses: Vec::new(),
            out_buses: Vec::new(),
            main_in: None,
            main_out: None,
            event_buses: 0,
            active: false,
            sample_rate: 48000.0,
            block: 512,
            steady: 0,
            in_bufs: Vec::new(),
            out_bufs: Vec::new(),
            program: None,
        }
    }

    fn module_for(&mut self, path: &Path) -> HostResult<&Module> {
        if self.module.as_ref().map(|m| m.path != path).unwrap_or(true) {
            if self.component.is_some() {
                return Err(HostError::new("bad_request", "a different plugin is already loaded in this host"));
            }
            self.module = None;
            self.module = Some(Module::open(path)?);
        }
        Ok(self.module.as_ref().unwrap())
    }

    fn ctl(&self) -> HostResult<&ComPtr<IEditController>> {
        self.controller.as_ref().ok_or_else(|| HostError::new("bad_request", "no plugin loaded"))
    }

    fn host_ptr(&self) -> *mut FUnknown {
        self.host.to_com_ptr::<FUnknown>().map(|p| p.as_ptr()).unwrap_or(null_mut())
    }

    fn refresh_params(&mut self) {
        let Some(c) = self.controller.clone() else { return };
        let mut t = Text { controller: &c };
        for p in self.params.iter_mut() {
            let n = unsafe { c.getParamNormalized(p.id) };
            p.value = unsafe { c.normalizedParamToPlain(p.id, n) };
            p.value_text = t.value_to_text(p.id, p.value).unwrap_or_default();
        }
    }

    fn scan_params(&mut self) {
        self.params.clear();
        self.flags.clear();
        let Some(c) = self.controller.clone() else { return };
        let n = unsafe { c.getParameterCount() };
        for i in 0..n {
            let mut info: ParameterInfo = unsafe { std::mem::zeroed() };
            if !ok(unsafe { c.getParameterInfo(i, &mut info) }) {
                continue;
            }
            let id = info.id;
            let (min, max, def) = unsafe {
                (
                    c.normalizedParamToPlain(id, 0.0),
                    c.normalizedParamToPlain(id, 1.0),
                    c.normalizedParamToPlain(id, info.defaultNormalizedValue),
                )
            };
            let mut s: String128 = [0; 128];
            let default_text = if ok(unsafe { c.getParamStringByValue(id, info.defaultNormalizedValue, &mut s) }) {
                w2s(&s)
            } else {
                String::new()
            };
            let units = w2s(&info.units);
            let f = info.flags;
            use ParameterInfo_::ParameterFlags_ as PF;
            self.params.push(ParamInfo {
                id,
                name: w2s(&info.title),
                module: String::new(),
                min,
                max,
                default: def,
                value: def,
                unit: if units.trim().is_empty() { infer_unit(&default_text) } else { Some(units.trim().to_string()) },
                default_text,
                value_text: String::new(),
                stepped: info.stepCount > 0,
                hidden: f & PF::kIsHidden as i32 != 0,
                readonly: f & PF::kIsReadOnly as i32 != 0,
                bypass: f & PF::kIsBypass as i32 != 0,
                automatable: f & PF::kCanAutomate as i32 != 0,
            });
            self.flags.push(f);
        }
        self.refresh_params();
    }

    fn buses(&mut self) -> (Vec<PortInfo>, Vec<PortInfo>, Vec<NotePortInfo>) {
        let comp = self.component.clone().unwrap();
        let audio = MediaTypes_::kAudio as MediaType;
        let event = MediaTypes_::kEvent as MediaType;
        let input = BusDirections_::kInput as BusDirection;
        let output = BusDirections_::kOutput as BusDirection;
        let read = |dir: BusDirection| -> Vec<PortInfo> {
            let n = unsafe { comp.getBusCount(audio, dir) };
            (0..n)
                .map(|i| {
                    let mut b: BusInfo = unsafe { std::mem::zeroed() };
                    if ok(unsafe { comp.getBusInfo(audio, dir, i, &mut b) }) {
                        PortInfo {
                            name: w2s(&b.name),
                            channels: b.channelCount.max(0) as u32,
                            main: b.busType == BusTypes_::kMain as BusType,
                        }
                    } else {
                        PortInfo::default()
                    }
                })
                .collect()
        };
        let ins = read(input);
        let outs = read(output);
        let pick = |p: &[PortInfo]| p.iter().position(|x| x.main).or(if p.is_empty() { None } else { Some(0) });
        self.main_in = pick(&ins);
        self.main_out = pick(&outs);
        for (i, _) in ins.iter().enumerate() {
            unsafe { comp.activateBus(audio, input, i as i32, (Some(i) == self.main_in) as TBool) };
        }
        for (i, _) in outs.iter().enumerate() {
            unsafe { comp.activateBus(audio, output, i as i32, (Some(i) == self.main_out) as TBool) };
        }
        self.event_buses = unsafe { comp.getBusCount(event, input) };
        let mut notes = Vec::new();
        for i in 0..self.event_buses {
            let mut b: BusInfo = unsafe { std::mem::zeroed() };
            if ok(unsafe { comp.getBusInfo(event, input, i, &mut b) }) {
                notes.push(NotePortInfo { name: w2s(&b.name), dialects: vec!["midi".into()] });
            }
            unsafe { comp.activateBus(event, input, i, (i == 0) as TBool) };
        }
        // keep the plugin's own speaker arrangements, but tell it they are in use
        if let Some(p) = &self.processor {
            let mut ia: Vec<SpeakerArrangement> = (0..ins.len())
                .map(|i| {
                    let mut a: SpeakerArrangement = 0;
                    unsafe { p.getBusArrangement(input, i as i32, &mut a) };
                    a
                })
                .collect();
            let mut oa: Vec<SpeakerArrangement> = (0..outs.len())
                .map(|i| {
                    let mut a: SpeakerArrangement = 0;
                    unsafe { p.getBusArrangement(output, i as i32, &mut a) };
                    a
                })
                .collect();
            unsafe { p.setBusArrangements(ia.as_mut_ptr(), ia.len() as i32, oa.as_mut_ptr(), oa.len() as i32) };
        }
        self.in_buses = ins.iter().map(|p| p.channels).collect();
        self.out_buses = outs.iter().map(|p| p.channels).collect();
        (ins, outs, notes)
    }

    fn scan_programs(&mut self) {
        self.program = None;
        let Some(c) = self.controller.clone() else { return };
        let Some(pi) = self
            .params
            .iter()
            .enumerate()
            .find(|(i, _)| self.flags[*i] & ParameterInfo_::ParameterFlags_::kIsProgramChange as i32 != 0)
        else {
            return;
        };
        let param = pi.1.id;
        let mut presets = Vec::new();
        if let Some(units) = c.cast::<IUnitInfo>() {
            let lists = unsafe { units.getProgramListCount() };
            for l in 0..lists {
                let mut info: ProgramListInfo = unsafe { std::mem::zeroed() };
                if !ok(unsafe { units.getProgramListInfo(l, &mut info) }) {
                    continue;
                }
                let list_name = w2s(&info.name);
                for p in 0..info.programCount.min(4096) {
                    let mut s: String128 = [0; 128];
                    if ok(unsafe { units.getProgramName(info.id, p, &mut s) }) {
                        presets.push(PresetInfo {
                            name: w2s(&s),
                            load_key: Some(format!("{}:{p}", info.id)),
                            location_kind: "program".into(),
                            location: Some(list_name.clone()),
                            ..Default::default()
                        });
                    }
                }
                break; // the program-change parameter addresses the first list
            }
        }
        if presets.is_empty() {
            // no program list: use the parameter's own step labels
            let p = &self.params[pi.0];
            let mut t = Text { controller: &c };
            let steps = (p.max - p.min).round() as i64;
            for k in 0..=steps.clamp(0, 4096) {
                let v = p.min + k as f64;
                let name = t.value_to_text(p.id, v).unwrap_or_else(|| format!("Program {k}"));
                presets.push(PresetInfo {
                    name,
                    load_key: Some(format!("0:{k}")),
                    location_kind: "program".into(),
                    ..Default::default()
                });
            }
        }
        self.program = Some(Program { param, presets });
    }

    /// Deliver pending parameter changes to the processor (a zero-length process call,
    /// activating briefly if needed) so the component state reflects them.
    fn flush_params(&mut self) -> HostResult<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let was_active = self.active;
        if !was_active {
            self.start(self.sample_rate, self.block)?;
        }
        let r = self.run_block(&[], &mut [], 0, &[]);
        if !was_active {
            self.stop();
        }
        r
    }

    fn start(&mut self, sr: f64, block: u32) -> HostResult<()> {
        let (Some(comp), Some(proc_)) = (self.component.clone(), self.processor.clone()) else {
            return Err(HostError::new("bad_request", "no plugin loaded"));
        };
        let mut setup = ProcessSetup {
            processMode: ProcessModes_::kOffline as i32,
            symbolicSampleSize: SymbolicSampleSizes_::kSample32 as i32,
            maxSamplesPerBlock: block as i32,
            sampleRate: sr,
        };
        if !ok(unsafe { proc_.canProcessSampleSize(SymbolicSampleSizes_::kSample32 as i32) }) {
            return Err(HostError::new("activate_failed", "the plugin cannot process 32-bit float audio"));
        }
        if !ok(unsafe { proc_.setupProcessing(&mut setup) }) {
            return Err(HostError::new("activate_failed", format!("setupProcessing failed at {sr} Hz / {block}")));
        }
        if !ok(unsafe { comp.setActive(1) }) {
            return Err(HostError::new("activate_failed", "the plugin refused to activate"));
        }
        unsafe { proc_.setProcessing(1) };
        self.active = true;
        self.sample_rate = sr;
        self.block = block;
        self.in_bufs = self.in_buses.iter().map(|&c| vec![vec![0.0f32; block as usize]; c as usize]).collect();
        self.out_bufs = self.out_buses.iter().map(|&c| vec![vec![0.0f32; block as usize]; c as usize]).collect();
        Ok(())
    }

    fn stop(&mut self) {
        if !self.active {
            return;
        }
        if let Some(p) = &self.processor {
            unsafe { p.setProcessing(0) };
        }
        if let Some(c) = &self.component {
            unsafe { c.setActive(0) };
        }
        self.active = false;
    }

    fn run_block(
        &mut self,
        inputs: &[&[f32]],
        outputs: &mut [&mut [f32]],
        frames: u32,
        events: &[PEvent],
    ) -> HostResult<()> {
        let proc_ = self.processor.clone().ok_or_else(|| HostError::new("bad_request", "no plugin loaded"))?;
        let n = frames as usize;
        for (bi, bus) in self.in_bufs.iter_mut().enumerate() {
            for (c, ch) in bus.iter_mut().enumerate() {
                ch[..n].fill(0.0);
                if Some(bi) == self.main_in {
                    if let Some(src) = inputs.get(c) {
                        ch[..n].copy_from_slice(&src[..n]);
                    }
                }
            }
        }
        for bus in self.out_bufs.iter_mut() {
            for ch in bus.iter_mut() {
                ch[..n].fill(0.0);
            }
        }
        let mut in_ptrs: Vec<Vec<*mut f32>> =
            self.in_bufs.iter_mut().map(|b| b.iter_mut().map(|c| c.as_mut_ptr()).collect()).collect();
        let mut out_ptrs: Vec<Vec<*mut f32>> =
            self.out_bufs.iter_mut().map(|b| b.iter_mut().map(|c| c.as_mut_ptr()).collect()).collect();
        let mk = |p: &mut Vec<*mut f32>| AudioBusBuffers {
            numChannels: p.len() as int32,
            silenceFlags: 0,
            __field0: AudioBusBuffers__type0 {
                channelBuffers32: if p.is_empty() { null_mut() } else { p.as_mut_ptr() },
            },
        };
        let mut in_audio: Vec<AudioBusBuffers> = in_ptrs.iter_mut().map(mk).collect();
        let mut out_audio: Vec<AudioBusBuffers> = out_ptrs.iter_mut().map(mk).collect();

        let changes = ParamChanges::new();
        for (id, v) in self.pending.drain(..) {
            let mut idx = 0;
            let q = unsafe { changes.addParameterData(&id, &mut idx) };
            if let Some(q) = unsafe { ComRef::from_raw(q) } {
                unsafe { q.addPoint(0, v, &mut idx) };
            }
        }
        let out_changes = ParamChanges::new();
        let list = ComWrapper::new(EventList { events: RefCell::new(Vec::new()) });
        let out_list = ComWrapper::new(EventList { events: RefCell::new(Vec::new()) });
        let mut sorted = events.to_vec();
        sorted.sort_by_key(|e| e.time());
        for e in sorted {
            let ev = match e {
                PEvent::NoteOn { time, channel, key, velocity } => Some(note(time, true, channel, key, velocity)),
                PEvent::NoteOff { time, channel, key, velocity } => Some(note(time, false, channel, key, velocity)),
                PEvent::Midi { time, data } => match data[0] & 0xF0 {
                    0x90 if data[2] > 0 => Some(note(time, true, data[0] & 15, data[1], data[2] as f32 / 127.0)),
                    0x80 | 0x90 => Some(note(time, false, data[0] & 15, data[1], data[2] as f32 / 127.0)),
                    _ => None, // CCs need IMidiMapping; not supported yet
                },
            };
            if let Some(mut ev) = ev {
                ev.sampleOffset = ev.sampleOffset.min(frames.saturating_sub(1) as i32);
                list.events.borrow_mut().push(ev);
            }
        }
        let mut ctx: ProcessContext = unsafe { std::mem::zeroed() };
        use ProcessContext_::StatesAndFlags_ as SF;
        ctx.state = SF::kPlaying | SF::kTempoValid | SF::kProjectTimeMusicValid | SF::kTimeSigValid;
        ctx.sampleRate = self.sample_rate;
        ctx.projectTimeSamples = self.steady;
        ctx.continousTimeSamples = self.steady;
        ctx.projectTimeMusic = self.steady as f64 / self.sample_rate * 2.0;
        ctx.tempo = 120.0;
        ctx.timeSigNumerator = 4;
        ctx.timeSigDenominator = 4;
        let ptr_of = |w: &ComWrapper<ParamChanges>| {
            w.as_com_ref::<IParameterChanges>().map(|r| r.as_ptr()).unwrap_or(null_mut())
        };
        let ev_of = |w: &ComWrapper<EventList>| w.as_com_ref::<IEventList>().map(|r| r.as_ptr()).unwrap_or(null_mut());
        let mut data = ProcessData {
            processMode: ProcessModes_::kOffline as i32,
            symbolicSampleSize: SymbolicSampleSizes_::kSample32 as i32,
            numSamples: frames as int32,
            numInputs: in_audio.len() as int32,
            numOutputs: out_audio.len() as int32,
            inputs: if in_audio.is_empty() { null_mut() } else { in_audio.as_mut_ptr() },
            outputs: if out_audio.is_empty() { null_mut() } else { out_audio.as_mut_ptr() },
            inputParameterChanges: ptr_of(&changes),
            outputParameterChanges: ptr_of(&out_changes),
            inputEvents: if self.event_buses > 0 { ev_of(&list) } else { null_mut() },
            outputEvents: ev_of(&out_list),
            processContext: &mut ctx,
        };
        let r = unsafe { proc_.process(&mut data) };
        if !ok(r) {
            return Err(HostError::new("process_failed", format!("the plugin's process() returned {r}")));
        }
        if let Some(mo) = self.main_out {
            for (c, out) in outputs.iter_mut().enumerate() {
                if let Some(src) = self.out_bufs[mo].get(c) {
                    out[..n].copy_from_slice(&src[..n]);
                }
            }
        }
        self.steady += frames as i64;
        Ok(())
    }

    fn unload(&mut self) {
        self.stop();
        if let Some((a, b)) = self.connections.take() {
            unsafe {
                a.disconnect(b.as_ptr());
                b.disconnect(a.as_ptr());
            }
        }
        if let Some(c) = self.controller.take() {
            unsafe { c.setComponentHandler(null_mut()) };
            if self.separate {
                unsafe { c.terminate() };
            }
        }
        self.processor = None;
        if let Some(c) = self.component.take() {
            unsafe { c.terminate() };
        }
        self.params.clear();
        self.flags.clear();
        self.pending.clear();
        self.program = None;
    }

    fn values(&self, ids: Option<&[u32]>) -> Vec<PValue> {
        self.params
            .iter()
            .filter(|p| ids.map(|i| i.contains(&p.id)).unwrap_or(true))
            .map(|p| PValue { id: p.id, name: p.name.clone(), value: p.value, text: p.value_text.clone() })
            .collect()
    }

    fn apply_component_state(&mut self, comp_state: &[u8], ctl_state: &[u8]) -> HostResult<()> {
        let comp = self.component.clone().ok_or_else(|| HostError::new("bad_request", "no plugin loaded"))?;
        let s = MemStream::new(comp_state.to_vec());
        let sp = s.as_com_ref::<IBStream>().map(|r| r.as_ptr()).unwrap_or(null_mut());
        if !ok(unsafe { comp.setState(sp) }) {
            return Err(HostError::new("bad_state", "the plugin rejected the component state"));
        }
        if let Some(c) = self.controller.clone() {
            let s2 = MemStream::new(comp_state.to_vec());
            unsafe { c.setComponentState(s2.as_com_ref::<IBStream>().map(|r| r.as_ptr()).unwrap_or(null_mut())) };
            if !ctl_state.is_empty() {
                let s3 = MemStream::new(ctl_state.to_vec());
                unsafe { c.setState(s3.as_com_ref::<IBStream>().map(|r| r.as_ptr()).unwrap_or(null_mut())) };
            }
        }
        self.pending.clear();
        self.refresh_params();
        Ok(())
    }

    /// Load a `.vstpreset` file (component and controller chunks).
    fn load_vstpreset(&mut self, path: &Path) -> HostResult<()> {
        let b = std::fs::read(path)
            .map_err(|e| HostError::new("unknown_preset", format!("cannot read {}: {e}", path.display())))?;
        let bad = || HostError::new("bad_state", format!("{} is not a valid .vstpreset file", path.display()));
        if b.len() < 48 || &b[..4] != b"VST3" {
            return Err(bad());
        }
        let list = i64::from_le_bytes(b[40..48].try_into().unwrap()) as usize;
        if list + 8 > b.len() || &b[list..list + 4] != b"List" {
            return Err(bad());
        }
        let count = i32::from_le_bytes(b[list + 4..list + 8].try_into().unwrap()).max(0) as usize;
        let (mut comp, mut ctl) = (Vec::new(), Vec::new());
        for i in 0..count.min(64) {
            let e = list + 8 + i * 20;
            if e + 20 > b.len() {
                return Err(bad());
            }
            let id = &b[e..e + 4];
            let off = i64::from_le_bytes(b[e + 4..e + 12].try_into().unwrap()) as usize;
            let size = i64::from_le_bytes(b[e + 12..e + 20].try_into().unwrap()) as usize;
            if off + size > b.len() {
                return Err(bad());
            }
            match id {
                b"Comp" => comp = b[off..off + size].to_vec(),
                b"Cont" => ctl = b[off..off + size].to_vec(),
                _ => {}
            }
        }
        if comp.is_empty() {
            return Err(bad());
        }
        self.apply_component_state(&comp, &ctl)
    }
}

fn note(time: u32, on: bool, channel: u8, key: u8, velocity: f32) -> Event {
    let mut e: Event = unsafe { std::mem::zeroed() };
    e.busIndex = 0;
    e.sampleOffset = time as int32;
    e.ppqPosition = 0.0;
    e.flags = 0;
    if on {
        e.r#type = Event_::EventTypes_::kNoteOnEvent as u16;
        e.__field0 = Event__type0 {
            noteOn: NoteOnEvent {
                channel: channel as i16,
                pitch: key as i16,
                tuning: 0.0,
                velocity,
                length: 0,
                noteId: -1,
            },
        };
    } else {
        e.r#type = Event_::EventTypes_::kNoteOffEvent as u16;
        e.__field0 = Event__type0 {
            noteOff: NoteOffEvent { channel: channel as i16, pitch: key as i16, velocity, noteId: -1, tuning: 0.0 },
        };
    }
    e
}

impl Drop for Vst3Backend {
    fn drop(&mut self) {
        self.unload();
        self.module = None;
    }
}

impl Backend for Vst3Backend {
    fn host_name(&self) -> &'static str {
        "ferraus-plughost-vst3"
    }

    fn formats(&self) -> Vec<String> {
        vec!["vst3".into()]
    }

    fn probe(&mut self, path: &Path) -> HostResult<Vec<Descriptor>> {
        Ok(self.module_for(path)?.classes().into_iter().map(|(d, _)| d).collect())
    }

    fn load(&mut self, path: &Path, plugin_id: Option<&str>) -> HostResult<PluginInfo> {
        self.unload();
        let host_ptr = self.host_ptr();
        let m = self.module_for(path)?;
        let classes = m.classes();
        let (desc, cid) = match plugin_id {
            // moduleinfo.json CIDs can use the COM byte order on Windows; a bundle with a
            // single audio class is unambiguous either way
            Some(id) => classes
                .iter()
                .find(|(d, _)| d.id.eq_ignore_ascii_case(id) || d.name == id)
                .or(if classes.len() == 1 { classes.first() } else { None })
                .cloned()
                .ok_or_else(|| {
                    load_err(format!(
                        "class '{id}' is not in {} (it has: {})",
                        path.display(),
                        classes.iter().map(|(d, _)| format!("{} {}", d.id, d.name)).collect::<Vec<_>>().join(", ")
                    ))
                })?,
            None if classes.len() == 1 => classes[0].clone(),
            None => {
                return Err(load_err(format!(
                    "{} contains {} audio classes; choose one by id: {}",
                    path.display(),
                    classes.len(),
                    classes.iter().map(|(d, _)| d.id.clone()).collect::<Vec<_>>().join(", ")
                )))
            }
        };
        let component: ComPtr<IComponent> =
            m.create(&cid).ok_or_else(|| load_err(format!("the factory refused to create '{}'", desc.name)))?;
        if !ok(unsafe { component.initialize(host_ptr) }) {
            return Err(load_err(format!("'{}' failed to initialize", desc.name)));
        }
        let processor = component.cast::<IAudioProcessor>();
        if processor.is_none() {
            unsafe { component.terminate() };
            return Err(load_err(format!("'{}' has no audio processor", desc.name)));
        }
        unsafe { component.setIoMode(IoModes_::kOfflineProcessing as IoMode) };
        let (controller, separate) = match component.cast::<IEditController>() {
            Some(c) => (Some(c), false),
            None => {
                let mut ccid: TUID = [0; 16];
                if ok(unsafe { component.getControllerClassId(&mut ccid) }) {
                    match m.create::<IEditController>(&ccid) {
                        Some(c) => {
                            if ok(unsafe { c.initialize(host_ptr) }) {
                                (Some(c), true)
                            } else {
                                (None, false)
                            }
                        }
                        None => (None, false),
                    }
                } else {
                    (None, false)
                }
            }
        };
        let Some(controller) = controller else {
            unsafe { component.terminate() };
            return Err(load_err(format!(
                "'{}' has no edit controller, so its parameters cannot be addressed",
                desc.name
            )));
        };
        if separate {
            if let (Some(a), Some(b)) = (component.cast::<IConnectionPoint>(), controller.cast::<IConnectionPoint>()) {
                unsafe {
                    a.connect(b.as_ptr());
                    b.connect(a.as_ptr());
                }
                self.connections = Some((a, b));
            }
            // controller learns the component's initial state
            let s = MemStream::new(Vec::new());
            let sp = s.as_com_ref::<IBStream>().map(|r| r.as_ptr()).unwrap_or(null_mut());
            if ok(unsafe { component.getState(sp) }) {
                let bytes = s.data.borrow().clone();
                let s2 = MemStream::new(bytes);
                unsafe {
                    controller.setComponentState(s2.as_com_ref::<IBStream>().map(|r| r.as_ptr()).unwrap_or(null_mut()))
                };
            }
        }
        let hp = self.handler.as_com_ref::<IComponentHandler>().map(|r| r.as_ptr()).unwrap_or(null_mut());
        unsafe { controller.setComponentHandler(hp) };
        self.component = Some(component);
        self.processor = processor;
        self.controller = Some(controller);
        self.separate = separate;
        let (ins, outs, notes) = self.buses();
        self.scan_params();
        self.scan_programs();
        let (mi, mo) = self.main_channels();
        Ok(PluginInfo {
            descriptor: desc,
            format: "vst3".into(),
            params: self.params.clone(),
            audio_inputs: ins,
            audio_outputs: outs,
            note_inputs: notes,
            note_outputs: Vec::new(),
            main_in_channels: mi,
            main_out_channels: mo,
            has_state: true,
            has_presets: self.program.as_ref().map(|p| !p.presets.is_empty()).unwrap_or(false),
        })
    }

    fn get_params(&mut self) -> HostResult<Vec<PValue>> {
        self.ctl()?;
        self.refresh_params();
        Ok(self.values(None))
    }

    fn set_params(&mut self, assigns: &[ParamAssign]) -> HostResult<Vec<PValue>> {
        let c = self.ctl()?.clone();
        let mut ids = Vec::new();
        for a in assigns {
            let p = find_param(&self.params, &a.name).map_err(|m| HostError::new("unknown_param", m))?.clone();
            if p.readonly {
                return Err(HostError::new("bad_value", format!("parameter '{}' is read-only", p.name)));
            }
            let v = a.value.trim();
            let norm = if let Some(x) = v.strip_prefix("norm:") {
                let x: f64 =
                    x.trim().parse().map_err(|_| HostError::new("bad_value", format!("'{v}' is not norm:<0..1>")))?;
                if !(0.0..=1.0).contains(&x) {
                    return Err(HostError::new("bad_value", format!("{v} is outside norm:0..1")));
                }
                x
            } else {
                let mut t = Text { controller: &c };
                let plain = resolve_value(&p, v, &mut t).map_err(|m| HostError::new("bad_value", m))?;
                unsafe { c.plainParamToNormalized(p.id, plain) }
            };
            unsafe { c.setParamNormalized(p.id, norm) };
            self.pending.retain(|(id, _)| *id != p.id);
            self.pending.push((p.id, norm));
            ids.push(p.id);
        }
        self.flush_params()?;
        self.refresh_params();
        Ok(self.values(Some(&ids)))
    }

    fn list_presets(&mut self) -> HostResult<Vec<PresetInfo>> {
        self.ctl()?;
        Ok(self.program.as_ref().map(|p| p.presets.clone()).unwrap_or_default())
    }

    fn load_preset(&mut self, preset: &str) -> HostResult<Vec<PValue>> {
        let c = self.ctl()?.clone();
        if preset.to_ascii_lowercase().ends_with(".vstpreset") && Path::new(preset).is_file() {
            self.load_vstpreset(Path::new(preset))?;
            return Ok(self.values(None));
        }
        let Some(prog) = &self.program else {
            return Err(HostError::new(
                "unsupported",
                "this plugin exposes no program list; pass a .vstpreset file instead",
            ));
        };
        let found = prog
            .presets
            .iter()
            .position(|p| p.name == preset)
            .or_else(|| prog.presets.iter().position(|p| p.name.eq_ignore_ascii_case(preset)))
            .or_else(|| prog.presets.iter().position(|p| p.load_key.as_deref() == Some(preset)));
        let Some(index) = found else {
            let names: Vec<&str> = prog.presets.iter().take(20).map(|p| p.name.as_str()).collect();
            return Err(HostError::new(
                "unknown_preset",
                format!("no preset named '{preset}'; presets include: {}", names.join(", ")),
            ));
        };
        let program_index = prog.presets[index]
            .load_key
            .as_deref()
            .and_then(|k| k.split(':').nth(1))
            .and_then(|k| k.parse::<f64>().ok())
            .unwrap_or(index as f64);
        let param = prog.param;
        let pinfo = self.params.iter().find(|p| p.id == param).cloned();
        let plain = pinfo.map(|p| p.min + program_index).unwrap_or(program_index);
        let norm = unsafe { c.plainParamToNormalized(param, plain) };
        unsafe { c.setParamNormalized(param, norm) };
        self.pending.push((param, norm));
        self.flush_params()?;
        self.refresh_params();
        Ok(self.values(None))
    }

    fn get_state(&mut self) -> HostResult<Vec<u8>> {
        self.flush_params()?;
        let comp = self.component.clone().ok_or_else(|| HostError::new("bad_request", "no plugin loaded"))?;
        let s = MemStream::new(Vec::new());
        if !ok(unsafe { comp.getState(s.as_com_ref::<IBStream>().map(|r| r.as_ptr()).unwrap_or(null_mut())) }) {
            return Err(HostError::new("no_state", "the plugin failed to save its state"));
        }
        let comp_bytes = s.data.borrow().clone();
        let mut ctl_bytes = Vec::new();
        if self.separate {
            if let Some(c) = &self.controller {
                let s2 = MemStream::new(Vec::new());
                if ok(unsafe { c.getState(s2.as_com_ref::<IBStream>().map(|r| r.as_ptr()).unwrap_or(null_mut())) }) {
                    ctl_bytes = s2.data.borrow().clone();
                }
            }
        }
        let mut out = b"FV3S".to_vec();
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&(comp_bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(&comp_bytes);
        out.extend_from_slice(&(ctl_bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(&ctl_bytes);
        Ok(out)
    }

    fn set_state(&mut self, state: &[u8]) -> HostResult<()> {
        let (comp, ctl) = if state.len() >= 12 && &state[..4] == b"FV3S" {
            let rd = |o: usize| -> Option<usize> {
                state.get(o..o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize)
            };
            let bad = || HostError::new("bad_state", "truncated ferraus VST3 state");
            let n1 = rd(8).ok_or_else(bad)?;
            let c1 = state.get(12..12 + n1).ok_or_else(bad)?;
            let n2 = rd(12 + n1).ok_or_else(bad)?;
            let c2 = state.get(16 + n1..16 + n1 + n2).ok_or_else(bad)?;
            (c1.to_vec(), c2.to_vec())
        } else {
            (state.to_vec(), Vec::new())
        };
        self.apply_component_state(&comp, &ctl)
    }

    fn activate(&mut self, sample_rate: f64, block_size: u32) -> HostResult<Activation> {
        self.stop();
        self.flush_params()?;
        self.start(sample_rate, block_size)?;
        self.steady = 0;
        let p = self.processor.clone().unwrap();
        let latency = unsafe { p.getLatencySamples() };
        let tail = unsafe { p.getTailSamples() };
        Ok(Activation { latency, tail: Some(tail) })
    }

    fn process(
        &mut self,
        inputs: &[&[f32]],
        outputs: &mut [&mut [f32]],
        frames: u32,
        events: &[PEvent],
    ) -> HostResult<()> {
        if !self.active {
            return Err(HostError::new("bad_request", "process before activate"));
        }
        self.run_block(inputs, outputs, frames, events)
    }

    fn deactivate(&mut self) {
        self.stop();
    }

    fn main_channels(&self) -> (u32, u32) {
        (self.main_in.map(|i| self.in_buses[i]).unwrap_or(0), self.main_out.map(|i| self.out_buses[i]).unwrap_or(0))
    }
}
