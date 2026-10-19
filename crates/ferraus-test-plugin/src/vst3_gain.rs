//! A VST3 gain effect in the same library, so the VST3 host can be tested without a
//! third-party plugin. Adapted from the `vst3` crate's MIT/Apache-2.0 example plugin.
//!
//! Parameter 0 "Gain": plain value -60..+24 dB (normalized `(dB + 60) / 84`), display text
//! `"-6.00"` with units `dB`. The component state is the normalized gain as an `f64`.

// Enum types in the bindings differ by platform, so some casts are no-ops here only.
#![allow(non_snake_case, non_upper_case_globals, clippy::unnecessary_cast)]

use std::cell::Cell;
use std::ffi::{c_char, c_void, CString};
use std::sync::atomic::{AtomicU64, Ordering};
use std::{ptr, slice};
use vst3::{uid, Class, ComRef, ComWrapper, Steinberg::Vst::*, Steinberg::*};

fn copy_cstring(src: &str, dst: &mut [c_char]) {
    let c = CString::new(src).unwrap_or_default();
    let bytes = c.as_bytes_with_nul();
    for (s, d) in bytes.iter().zip(dst.iter_mut()) {
        *d = *s as c_char;
    }
    if bytes.len() > dst.len() {
        if let Some(last) = dst.last_mut() {
            *last = 0;
        }
    }
}

fn copy_wstring(src: &str, dst: &mut [TChar]) {
    let mut len = 0;
    for (s, d) in src.encode_utf16().zip(dst.iter_mut()) {
        *d = s as TChar;
        len += 1;
    }
    if len < dst.len() {
        dst[len] = 0;
    } else if let Some(last) = dst.last_mut() {
        *last = 0;
    }
}

unsafe fn read_wstring(s: *const TChar) -> String {
    let mut n = 0;
    while *s.add(n) != 0 && n < 1024 {
        n += 1;
    }
    String::from_utf16_lossy(slice::from_raw_parts(s as *const u16, n))
}

const NAME: &str = "Ferraus Test VST3 Gain";
const DEFAULT_NORM: f64 = 60.0 / 84.0;

fn norm_to_db(n: f64) -> f64 {
    -60.0 + 84.0 * n.clamp(0.0, 1.0)
}

fn db_to_norm(db: f64) -> f64 {
    ((db + 60.0) / 84.0).clamp(0.0, 1.0)
}

unsafe fn stream_write(stream: *mut IBStream, bytes: &[u8]) -> bool {
    match ComRef::from_raw(stream) {
        Some(s) => {
            let mut written = 0;
            s.write(bytes.as_ptr() as *mut c_void, bytes.len() as i32, &mut written) == kResultOk
                && written == bytes.len() as i32
        }
        None => false,
    }
}

unsafe fn stream_read_f64(stream: *mut IBStream) -> Option<f64> {
    let s = ComRef::from_raw(stream)?;
    let mut b = [0u8; 8];
    let mut read = 0;
    if s.read(b.as_mut_ptr() as *mut c_void, 8, &mut read) == kResultOk && read == 8 {
        Some(f64::from_le_bytes(b))
    } else {
        None
    }
}

struct GainProcessor {
    gain: AtomicU64,
}

impl Class for GainProcessor {
    type Interfaces = (IComponent, IAudioProcessor, IProcessContextRequirements);
}

impl GainProcessor {
    const CID: TUID = uid(0x5F3A7C21, 0x9B6E4D10, 0x8E2F4A6B, 0x1C0D9E77);
}

impl IPluginBaseTrait for GainProcessor {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        kResultOk
    }
    unsafe fn terminate(&self) -> tresult {
        kResultOk
    }
}

impl IComponentTrait for GainProcessor {
    unsafe fn getControllerClassId(&self, class_id: *mut TUID) -> tresult {
        *class_id = GainController::CID;
        kResultOk
    }
    unsafe fn setIoMode(&self, _mode: IoMode) -> tresult {
        kResultOk
    }
    unsafe fn getBusCount(&self, media_type: MediaType, _dir: BusDirection) -> i32 {
        if media_type == MediaTypes_::kAudio as MediaType {
            1
        } else {
            0
        }
    }
    unsafe fn getBusInfo(&self, media_type: MediaType, dir: BusDirection, index: i32, bus: *mut BusInfo) -> tresult {
        if media_type != MediaTypes_::kAudio as MediaType || index != 0 {
            return kInvalidArgument;
        }
        let bus = &mut *bus;
        bus.mediaType = MediaTypes_::kAudio as MediaType;
        bus.direction = dir;
        bus.channelCount = 2;
        copy_wstring(if dir == BusDirections_::kInput as BusDirection { "Input" } else { "Output" }, &mut bus.name);
        bus.busType = BusTypes_::kMain as BusType;
        bus.flags = BusInfo_::BusFlags_::kDefaultActive;
        kResultOk
    }
    unsafe fn getRoutingInfo(&self, _i: *mut RoutingInfo, _o: *mut RoutingInfo) -> tresult {
        kNotImplemented
    }
    unsafe fn activateBus(&self, _t: MediaType, _d: BusDirection, _i: i32, _s: TBool) -> tresult {
        kResultOk
    }
    unsafe fn setActive(&self, _state: TBool) -> tresult {
        kResultOk
    }
    unsafe fn setState(&self, state: *mut IBStream) -> tresult {
        match stream_read_f64(state) {
            Some(v) => {
                self.gain.store(v.to_bits(), Ordering::Relaxed);
                kResultOk
            }
            None => kResultFalse,
        }
    }
    unsafe fn getState(&self, state: *mut IBStream) -> tresult {
        let v = f64::from_bits(self.gain.load(Ordering::Relaxed));
        if stream_write(state, &v.to_le_bytes()) {
            kResultOk
        } else {
            kResultFalse
        }
    }
}

impl IAudioProcessorTrait for GainProcessor {
    unsafe fn setBusArrangements(
        &self,
        inputs: *mut SpeakerArrangement,
        num_ins: i32,
        outputs: *mut SpeakerArrangement,
        num_outs: i32,
    ) -> tresult {
        if num_ins == 1 && num_outs == 1 && *inputs == SpeakerArr::kStereo && *outputs == SpeakerArr::kStereo {
            kResultTrue
        } else {
            kResultFalse
        }
    }
    unsafe fn getBusArrangement(&self, _dir: BusDirection, index: i32, arr: *mut SpeakerArrangement) -> tresult {
        if index == 0 {
            *arr = SpeakerArr::kStereo;
            kResultOk
        } else {
            kInvalidArgument
        }
    }
    unsafe fn canProcessSampleSize(&self, size: i32) -> tresult {
        if size == SymbolicSampleSizes_::kSample32 as i32 {
            kResultOk
        } else {
            kNotImplemented
        }
    }
    unsafe fn getLatencySamples(&self) -> u32 {
        0
    }
    unsafe fn setupProcessing(&self, _setup: *mut ProcessSetup) -> tresult {
        kResultOk
    }
    unsafe fn setProcessing(&self, _state: TBool) -> tresult {
        kResultOk
    }
    unsafe fn process(&self, data: *mut ProcessData) -> tresult {
        let d = &*data;
        if let Some(changes) = ComRef::from_raw(d.inputParameterChanges) {
            for i in 0..changes.getParameterCount() {
                if let Some(q) = ComRef::from_raw(changes.getParameterData(i)) {
                    let n = q.getPointCount();
                    let (mut off, mut v) = (0, 0.0);
                    if q.getParameterId() == 0 && n > 0 && q.getPoint(n - 1, &mut off, &mut v) == kResultTrue {
                        self.gain.store(v.to_bits(), Ordering::Relaxed);
                    }
                }
            }
        }
        let frames = d.numSamples as usize;
        if frames == 0 || d.numInputs < 1 || d.numOutputs < 1 {
            return kResultOk;
        }
        let g = 10f64.powf(norm_to_db(f64::from_bits(self.gain.load(Ordering::Relaxed))) / 20.0) as f32;
        let ins = &*d.inputs;
        let outs = &*d.outputs;
        let n = ins.numChannels.min(outs.numChannels) as usize;
        let ic = slice::from_raw_parts(ins.__field0.channelBuffers32, ins.numChannels as usize);
        let oc = slice::from_raw_parts(outs.__field0.channelBuffers32, outs.numChannels as usize);
        for c in 0..n {
            let x = slice::from_raw_parts(ic[c], frames);
            let y = slice::from_raw_parts_mut(oc[c], frames);
            for i in 0..frames {
                y[i] = x[i] * g;
            }
        }
        kResultOk
    }
    unsafe fn getTailSamples(&self) -> u32 {
        kNoTail
    }
}

impl IProcessContextRequirementsTrait for GainProcessor {
    unsafe fn getProcessContextRequirements(&self) -> u32 {
        0
    }
}

struct GainController {
    gain: Cell<f64>,
}

impl Class for GainController {
    type Interfaces = (IEditController,);
}

impl GainController {
    const CID: TUID = uid(0x0A7E5B39, 0x4C2D4F81, 0x9D3B6E2A, 0x7F1E0C55);
}

impl IPluginBaseTrait for GainController {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        kResultOk
    }
    unsafe fn terminate(&self) -> tresult {
        kResultOk
    }
}

impl IEditControllerTrait for GainController {
    unsafe fn setComponentState(&self, state: *mut IBStream) -> tresult {
        match stream_read_f64(state) {
            Some(v) => {
                self.gain.set(v);
                kResultOk
            }
            None => kResultFalse,
        }
    }
    unsafe fn setState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }
    unsafe fn getState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }
    unsafe fn getParameterCount(&self) -> i32 {
        1
    }
    unsafe fn getParameterInfo(&self, index: i32, info: *mut ParameterInfo) -> tresult {
        if index != 0 {
            return kInvalidArgument;
        }
        let info = &mut *info;
        info.id = 0;
        copy_wstring("Gain", &mut info.title);
        copy_wstring("Gain", &mut info.shortTitle);
        copy_wstring("dB", &mut info.units);
        info.stepCount = 0;
        info.defaultNormalizedValue = DEFAULT_NORM;
        info.unitId = 0;
        info.flags = ParameterInfo_::ParameterFlags_::kCanAutomate as i32;
        kResultOk
    }
    unsafe fn getParamStringByValue(&self, id: u32, value: f64, string: *mut String128) -> tresult {
        if id != 0 {
            return kInvalidArgument;
        }
        copy_wstring(&format!("{:.2}", norm_to_db(value)), &mut *string);
        kResultOk
    }
    unsafe fn getParamValueByString(&self, id: u32, string: *mut TChar, value: *mut f64) -> tresult {
        if id != 0 || string.is_null() {
            return kInvalidArgument;
        }
        let t = read_wstring(string).trim().to_ascii_lowercase();
        let t = t.strip_suffix("db").unwrap_or(&t).trim().to_string();
        match t.parse::<f64>() {
            Ok(db) => {
                *value = db_to_norm(db);
                kResultOk
            }
            Err(_) => kResultFalse,
        }
    }
    unsafe fn normalizedParamToPlain(&self, _id: u32, value: f64) -> f64 {
        norm_to_db(value)
    }
    unsafe fn plainParamToNormalized(&self, _id: u32, plain: f64) -> f64 {
        db_to_norm(plain)
    }
    unsafe fn getParamNormalized(&self, id: u32) -> f64 {
        if id == 0 {
            self.gain.get()
        } else {
            0.0
        }
    }
    unsafe fn setParamNormalized(&self, id: u32, value: f64) -> tresult {
        if id != 0 {
            return kInvalidArgument;
        }
        self.gain.set(value);
        kResultOk
    }
    unsafe fn setComponentHandler(&self, _handler: *mut IComponentHandler) -> tresult {
        kResultOk
    }
    unsafe fn createView(&self, _name: *const c_char) -> *mut IPlugView {
        ptr::null_mut()
    }
}

struct Factory;

impl Class for Factory {
    type Interfaces = (IPluginFactory2,);
}

impl IPluginFactoryTrait for Factory {
    unsafe fn getFactoryInfo(&self, info: *mut PFactoryInfo) -> tresult {
        let info = &mut *info;
        copy_cstring("ferraus", &mut info.vendor);
        copy_cstring("https://github.com/djynnius/ferraus", &mut info.url);
        copy_cstring("", &mut info.email);
        info.flags = PFactoryInfo_::FactoryFlags_::kUnicode as int32;
        kResultOk
    }
    unsafe fn countClasses(&self) -> i32 {
        2
    }
    unsafe fn getClassInfo(&self, index: i32, info: *mut PClassInfo) -> tresult {
        let info = &mut *info;
        info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as int32;
        copy_cstring(NAME, &mut info.name);
        match index {
            0 => {
                info.cid = GainProcessor::CID;
                copy_cstring("Audio Module Class", &mut info.category);
            }
            1 => {
                info.cid = GainController::CID;
                copy_cstring("Component Controller Class", &mut info.category);
            }
            _ => return kInvalidArgument,
        }
        kResultOk
    }
    unsafe fn createInstance(&self, cid: FIDString, iid: FIDString, obj: *mut *mut c_void) -> tresult {
        let instance = match *(cid as *const TUID) {
            GainProcessor::CID => {
                ComWrapper::new(GainProcessor { gain: AtomicU64::new(DEFAULT_NORM.to_bits()) }).to_com_ptr::<FUnknown>()
            }
            GainController::CID => {
                ComWrapper::new(GainController { gain: Cell::new(DEFAULT_NORM) }).to_com_ptr::<FUnknown>()
            }
            _ => None,
        };
        match instance {
            Some(i) => {
                let p = i.as_ptr();
                ((*(*p).vtbl).queryInterface)(p, iid as *mut TUID, obj)
            }
            None => kInvalidArgument,
        }
    }
}

impl IPluginFactory2Trait for Factory {
    unsafe fn getClassInfo2(&self, index: i32, info: *mut PClassInfo2) -> tresult {
        let i = &mut *info;
        i.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as int32;
        copy_cstring(NAME, &mut i.name);
        copy_cstring("ferraus", &mut i.vendor);
        copy_cstring("0.1.0", &mut i.version);
        copy_cstring("VST 3.7", &mut i.sdkVersion);
        i.classFlags = 0;
        match index {
            0 => {
                i.cid = GainProcessor::CID;
                copy_cstring("Audio Module Class", &mut i.category);
                copy_cstring("Fx|Tools", &mut i.subCategories);
            }
            1 => {
                i.cid = GainController::CID;
                copy_cstring("Component Controller Class", &mut i.category);
                copy_cstring("", &mut i.subCategories);
            }
            _ => return kInvalidArgument,
        }
        kResultOk
    }
}

#[cfg(target_os = "windows")]
#[no_mangle]
extern "system" fn InitDll() -> bool {
    true
}

#[cfg(target_os = "windows")]
#[no_mangle]
extern "system" fn ExitDll() -> bool {
    true
}

#[cfg(target_os = "macos")]
#[no_mangle]
extern "system" fn bundleEntry(_bundle_ref: *mut c_void) -> bool {
    true
}

#[cfg(target_os = "macos")]
#[no_mangle]
extern "system" fn bundleExit() -> bool {
    true
}

#[cfg(target_os = "linux")]
#[no_mangle]
extern "system" fn ModuleEntry(_library_handle: *mut c_void) -> bool {
    true
}

#[cfg(target_os = "linux")]
#[no_mangle]
extern "system" fn ModuleExit() -> bool {
    true
}

/// The VST3 entry point.
#[no_mangle]
extern "system" fn GetPluginFactory() -> *mut IPluginFactory {
    match ComWrapper::new(Factory).to_com_ptr::<IPluginFactory>() {
        Some(p) => p.into_raw(),
        None => ptr::null_mut(),
    }
}
