//! The controller's parameter list, read once per metadata change, and the values the processing
//! thread hands back to the controller.
//!
//! VST3 calls the edit controller on the thread that owns the plugin only. The processing thread
//! therefore never calls `setParamNormalized`; it stores the latest value per parameter here and
//! the owning thread delivers it (JUCE's `EditControllerParameterDispatcher` does the same).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

use plughost_core::ParameterInfo;
use vst3::ComPtr;
use vst3::Steinberg::Vst::{IEditController, IEditControllerTrait, ParamID};
use vst3::Steinberg::kResultOk;

/// The value still has to reach the controller.
const TO_CONTROLLER: u8 = 1;
/// The value came from the processor's output parameter changes; the application is told.
const FROM_PROCESSOR: u8 = 2;

pub(crate) struct ParameterCache {
    infos: Vec<ParameterInfo>,
    units: Vec<i32>,
    index: HashMap<u64, usize>,
    values: Vec<AtomicU64>,
    pending: Vec<AtomicU8>,
    dirty: AtomicBool,
}

impl ParameterCache {
    /// Reads every parameter the controller describes. Call on the thread that owns the plugin.
    pub fn read(controller: &ComPtr<IEditController>) -> ParameterCache {
        let mut infos = Vec::new();
        let mut units = Vec::new();
        for index in 0..unsafe { controller.getParameterCount() } {
            // SAFETY: ParameterInfo is plain C data the controller fills in.
            let mut info = unsafe { std::mem::zeroed() };
            if unsafe { controller.getParameterInfo(index, &mut info) } == kResultOk {
                infos.push(super::plugin::parameter_info(&info));
                units.push(info.unitId);
            }
        }
        let index = infos
            .iter()
            .enumerate()
            .map(|(index, info)| (info.id, index))
            .collect();
        ParameterCache {
            values: infos.iter().map(|_| AtomicU64::new(0)).collect(),
            pending: infos.iter().map(|_| AtomicU8::new(0)).collect(),
            infos,
            units,
            index,
            dirty: AtomicBool::new(false),
        }
    }

    pub fn infos(&self) -> &[ParameterInfo] {
        &self.infos
    }

    pub fn len(&self) -> usize {
        self.infos.len()
    }

    pub fn index(&self, id: u64) -> Option<usize> {
        self.index.get(&id).copied()
    }

    pub fn find(&self, id: u64) -> Option<&ParameterInfo> {
        self.index(id).map(|index| &self.infos[index])
    }

    pub fn info(&self, index: usize) -> &ParameterInfo {
        &self.infos[index]
    }

    /// The native ID; every cached ID came from a `ParamID`.
    pub fn native_id(&self, index: usize) -> ParamID {
        self.infos[index].id as ParamID
    }

    pub fn unit(&self, index: usize) -> i32 {
        self.units[index]
    }

    /// Stores the value the processor now has for the controller. Processing thread; never
    /// blocks or allocates.
    pub fn hand_to_controller(&self, index: usize, value: f64, from_processor: bool) {
        self.values[index].store(value.to_bits(), Ordering::Relaxed);
        let flags = TO_CONTROLLER | if from_processor { FROM_PROCESSOR } else { 0 };
        self.pending[index].fetch_or(flags, Ordering::Release);
        self.dirty.store(true, Ordering::Release);
    }

    /// Takes the values stored since the last call, with whether each came from the processor.
    /// Call on the thread that owns the controller.
    pub fn take_controller_values(&self, mut deliver: impl FnMut(ParamID, f64, bool)) {
        if !self.dirty.swap(false, Ordering::Acquire) {
            return;
        }
        for (index, pending) in self.pending.iter().enumerate() {
            let flags = pending.swap(0, Ordering::Acquire);
            if flags != 0 {
                let value = f64::from_bits(self.values[index].load(Ordering::Relaxed));
                deliver(self.native_id(index), value, flags & FROM_PROCESSOR != 0);
            }
        }
    }
}
