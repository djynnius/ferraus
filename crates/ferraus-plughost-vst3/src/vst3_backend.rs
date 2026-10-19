//! VST3 backend (work in progress).

use ferraus_plugins::host_rt::{Activation, Backend, HostError, HostResult};
use ferraus_plugins::protocol::*;
use std::path::Path;

pub struct Vst3Backend;

impl Vst3Backend {
    pub fn new() -> Self {
        Vst3Backend
    }
}

fn na<T>() -> HostResult<T> {
    Err(HostError::new("unsupported", "VST3 hosting is not available in this build"))
}

impl Backend for Vst3Backend {
    fn host_name(&self) -> &'static str {
        "ferraus-plughost-vst3"
    }
    fn formats(&self) -> Vec<String> {
        vec!["vst3".into()]
    }
    fn probe(&mut self, _path: &Path) -> HostResult<Vec<Descriptor>> {
        na()
    }
    fn load(&mut self, _path: &Path, _id: Option<&str>) -> HostResult<PluginInfo> {
        na()
    }
    fn get_params(&mut self) -> HostResult<Vec<ParamValue>> {
        na()
    }
    fn set_params(&mut self, _p: &[ParamAssign]) -> HostResult<Vec<ParamValue>> {
        na()
    }
    fn list_presets(&mut self) -> HostResult<Vec<PresetInfo>> {
        na()
    }
    fn load_preset(&mut self, _p: &str) -> HostResult<Vec<ParamValue>> {
        na()
    }
    fn get_state(&mut self) -> HostResult<Vec<u8>> {
        na()
    }
    fn set_state(&mut self, _s: &[u8]) -> HostResult<()> {
        na()
    }
    fn activate(&mut self, _sr: f64, _b: u32) -> HostResult<Activation> {
        na()
    }
    fn process(&mut self, _i: &[&[f32]], _o: &mut [&mut [f32]], _f: u32, _e: &[Event]) -> HostResult<()> {
        na()
    }
    fn deactivate(&mut self) {}
    fn main_channels(&self) -> (u32, u32) {
        (0, 0)
    }
}
