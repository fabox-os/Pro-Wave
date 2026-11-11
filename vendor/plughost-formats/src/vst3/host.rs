//! Host-side COM objects, following the SDK's `hostclasses`, `memorystream`, `parameterchanges`,
//! and `connectionproxy`.

#![allow(non_snake_case)]

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_void};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

use plughost_core::ParameterEvent;
use plughost_core::internal::ParameterEventBuffer;
use vst3::Steinberg::Vst::IAttributeList_::AttrID;
use vst3::Steinberg::Vst::{
    Event, IAttributeList, IAttributeListTrait, IComponentHandler, IComponentHandlerTrait,
    IConnectionPoint, IConnectionPointTrait, IEventList, IEventListTrait, IHostApplication,
    IHostApplicationTrait, IMessage, IMessageTrait, IParamValueQueue, IParamValueQueueTrait,
    IParameterChanges, IParameterChangesTrait, IStreamAttributes, IStreamAttributesTrait, ParamID,
    ParamValue, PresetAttributes, RestartFlags_, StateType, String128, TChar,
};
use vst3::Steinberg::Vst::{
    IComponentHandler2, IComponentHandler2Trait, IPlugInterfaceSupport, IPlugInterfaceSupportTrait,
};
use vst3::Steinberg::{
    FIDString, IBStream, IBStream_, IBStreamTrait, TUID, int32, int64, kInvalidArgument,
    kResultFalse, kResultOk, tresult,
};
use vst3::{Class, ComPtr, ComWrapper, Interface};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(crate) fn copy_wide(text: &str, out: &mut [TChar]) {
    let mut len = 0;
    for (unit, slot) in text.encode_utf16().zip(out.iter_mut()) {
        *slot = unit;
        len += 1;
    }
    let end = len.min(out.len() - 1);
    out[end] = 0;
}

pub(crate) fn wide_string(chars: &[TChar]) -> String {
    let len = chars.iter().position(|&c| c == 0).unwrap_or(chars.len());
    String::from_utf16_lossy(&chars[..len])
}

pub(crate) struct HostApplication {
    pub name: String,
}

impl Class for HostApplication {
    type Interfaces = (IHostApplication, IPlugInterfaceSupport);
}

/// Whether the host calls the plugin interface `iid`, which plugins may ask before relying on it.
fn calls_interface(iid: &[u8; 16]) -> bool {
    use vst3::Steinberg::IPlugViewContentScaleSupport;
    use vst3::Steinberg::Vst::{
        IAudioProcessor, IComponent, IEditController, IMidiMapping, INoteExpressionController,
        IProcessContextRequirements, IUnitInfo,
    };
    [
        IComponent::IID,
        IAudioProcessor::IID,
        IEditController::IID,
        IConnectionPoint::IID,
        IUnitInfo::IID,
        IMidiMapping::IID,
        IProcessContextRequirements::IID,
        INoteExpressionController::IID,
    ]
    .contains(iid)
        // Editors are told their content scale only on Windows; macOS scales views itself.
        || (cfg!(target_os = "windows") && *iid == IPlugViewContentScaleSupport::IID)
}

impl IPlugInterfaceSupportTrait for HostApplication {
    unsafe fn isPlugInterfaceSupported(&self, iid: *const TUID) -> tresult {
        if iid.is_null() {
            return kInvalidArgument;
        }
        let iid = unsafe { *iid }.map(|b| b as u8);
        if calls_interface(&iid) {
            kResultOk
        } else {
            kResultFalse
        }
    }
}

impl IHostApplicationTrait for HostApplication {
    unsafe fn getName(&self, name: *mut String128) -> tresult {
        copy_wide(&self.name, unsafe { &mut *name });
        kResultOk
    }

    unsafe fn createInstance(
        &self,
        cid: *mut TUID,
        iid: *mut TUID,
        obj: *mut *mut c_void,
    ) -> tresult {
        let (cid, iid) = unsafe { ((*cid).map(|b| b as u8), (*iid).map(|b| b as u8)) };
        let created = if cid == IMessage::IID && iid == IMessage::IID {
            ComWrapper::new(Message::default())
                .to_com_ptr::<IMessage>()
                .map(|ptr| ptr.into_raw() as *mut c_void)
        } else if cid == IAttributeList::IID && iid == IAttributeList::IID {
            ComWrapper::new(AttributeList::default())
                .to_com_ptr::<IAttributeList>()
                .map(|ptr| ptr.into_raw() as *mut c_void)
        } else {
            None
        };
        unsafe { *obj = created.unwrap_or(std::ptr::null_mut()) };
        if created.is_some() {
            kResultOk
        } else {
            kResultFalse
        }
    }
}

pub(crate) struct Message {
    id: Mutex<Option<CString>>,
    attributes: ComWrapper<AttributeList>,
}

impl Default for Message {
    fn default() -> Message {
        Message {
            id: Mutex::new(None),
            attributes: ComWrapper::new(AttributeList::default()),
        }
    }
}

impl Class for Message {
    type Interfaces = (IMessage,);
}

impl IMessageTrait for Message {
    unsafe fn getMessageID(&self) -> FIDString {
        lock(&self.id)
            .as_ref()
            .map_or(std::ptr::null(), |id| id.as_ptr())
    }

    unsafe fn setMessageID(&self, id: FIDString) {
        let id = (!id.is_null()).then(|| unsafe { CStr::from_ptr(id) }.to_owned());
        *lock(&self.id) = id;
    }

    unsafe fn getAttributes(&self) -> *mut IAttributeList {
        // The SDK host returns the list without adding a reference.
        self.attributes
            .as_com_ref::<IAttributeList>()
            .map_or(std::ptr::null_mut(), |list| list.as_ptr())
    }
}

enum Attribute {
    Int(i64),
    Float(f64),
    String(Vec<TChar>),
    Binary(Vec<u8>),
}

#[derive(Default)]
pub(crate) struct AttributeList {
    values: Mutex<HashMap<CString, Attribute>>,
}

impl Class for AttributeList {
    type Interfaces = (IAttributeList,);
}

impl AttributeList {
    fn set(&self, id: AttrID, value: Attribute) -> tresult {
        if id.is_null() {
            return kInvalidArgument;
        }
        let key = unsafe { CStr::from_ptr(id) }.to_owned();
        lock(&self.values).insert(key, value);
        kResultOk
    }
}

impl IAttributeListTrait for AttributeList {
    unsafe fn setInt(&self, id: AttrID, value: int64) -> tresult {
        self.set(id, Attribute::Int(value))
    }

    unsafe fn getInt(&self, id: AttrID, value: *mut int64) -> tresult {
        if id.is_null() || value.is_null() {
            return kInvalidArgument;
        }
        match lock(&self.values).get(unsafe { CStr::from_ptr(id) }) {
            Some(Attribute::Int(v)) => {
                unsafe { *value = *v };
                kResultOk
            }
            _ => kResultFalse,
        }
    }

    unsafe fn setFloat(&self, id: AttrID, value: f64) -> tresult {
        self.set(id, Attribute::Float(value))
    }

    unsafe fn getFloat(&self, id: AttrID, value: *mut f64) -> tresult {
        if id.is_null() || value.is_null() {
            return kInvalidArgument;
        }
        match lock(&self.values).get(unsafe { CStr::from_ptr(id) }) {
            Some(Attribute::Float(v)) => {
                unsafe { *value = *v };
                kResultOk
            }
            _ => kResultFalse,
        }
    }

    unsafe fn setString(&self, id: AttrID, string: *const TChar) -> tresult {
        if id.is_null() || string.is_null() {
            return kInvalidArgument;
        }
        let mut chars = Vec::new();
        let mut cursor = string;
        unsafe {
            while *cursor != 0 {
                chars.push(*cursor);
                cursor = cursor.add(1);
            }
        }
        self.set(id, Attribute::String(chars))
    }

    unsafe fn getString(&self, id: AttrID, string: *mut TChar, size_in_bytes: u32) -> tresult {
        if id.is_null() || string.is_null() {
            return kInvalidArgument;
        }
        match lock(&self.values).get(unsafe { CStr::from_ptr(id) }) {
            Some(Attribute::String(chars)) => {
                let capacity = size_in_bytes as usize / size_of::<TChar>();
                if capacity == 0 {
                    return kResultFalse;
                }
                let len = chars.len().min(capacity - 1);
                unsafe {
                    std::ptr::copy_nonoverlapping(chars.as_ptr(), string, len);
                    *string.add(len) = 0;
                }
                kResultOk
            }
            _ => kResultFalse,
        }
    }

    unsafe fn setBinary(&self, id: AttrID, data: *const c_void, size_in_bytes: u32) -> tresult {
        if id.is_null() || (data.is_null() && size_in_bytes != 0) {
            return kInvalidArgument;
        }
        let bytes = if size_in_bytes == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(data as *const u8, size_in_bytes as usize) }
        };
        self.set(id, Attribute::Binary(bytes.to_vec()))
    }

    unsafe fn getBinary(
        &self,
        id: AttrID,
        data: *mut *const c_void,
        size_in_bytes: *mut u32,
    ) -> tresult {
        if id.is_null() || data.is_null() || size_in_bytes.is_null() {
            return kInvalidArgument;
        }
        match lock(&self.values).get(unsafe { CStr::from_ptr(id) }) {
            Some(Attribute::Binary(bytes)) => {
                // The pointer stays valid while the attribute is not replaced, as in the SDK.
                unsafe {
                    *data = bytes.as_ptr() as *const c_void;
                    *size_in_bytes = bytes.len() as u32;
                }
                kResultOk
            }
            _ => kResultFalse,
        }
    }
}

/// What a state stream holds, as the plugin sees it through the stream attributes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StateKind {
    /// State saved in or restored from a project.
    Project,
    /// A normal preset, which VST3 marks by leaving the state type out.
    Preset,
}

#[cfg(test)]
#[path = "host/state_tests.rs"]
mod state_tests;

#[cfg(test)]
#[path = "host/attribute_tests.rs"]
mod attribute_tests;

#[cfg(test)]
#[path = "host/editor_tests.rs"]
mod editor_tests;

#[cfg(test)]
#[path = "host/interface_tests.rs"]
mod interface_tests;

/// A state stream with the attributes that tell the plugin what the state is.
pub(crate) struct MemoryStream {
    inner: Mutex<(Vec<u8>, usize)>,
    attributes: ComWrapper<AttributeList>,
    limit: usize,
    exceeded: AtomicBool,
}

impl Class for MemoryStream {
    type Interfaces = (IBStream, IStreamAttributes);
}

impl MemoryStream {
    pub fn new() -> ComWrapper<MemoryStream> {
        MemoryStream::from_bytes(&[], StateKind::Project)
    }

    pub fn from_bytes(bytes: &[u8], kind: StateKind) -> ComWrapper<MemoryStream> {
        Self::with_bytes(bytes, kind, plughost_core::MAX_STATE_BYTES)
    }

    pub fn bounded(kind: StateKind, limit: usize) -> ComWrapper<MemoryStream> {
        Self::with_bytes(&[], kind, limit)
    }

    fn with_bytes(bytes: &[u8], kind: StateKind, limit: usize) -> ComWrapper<MemoryStream> {
        let attributes = ComWrapper::new(AttributeList::default());
        if kind == StateKind::Project {
            let project: Vec<TChar> = unsafe { CStr::from_ptr(StateType::kProject) }
                .to_bytes()
                .iter()
                .map(|&b| TChar::from(b))
                .collect();
            attributes.set(PresetAttributes::kStateType, Attribute::String(project));
        }
        ComWrapper::new(MemoryStream {
            inner: Mutex::new((bytes.to_vec(), 0)),
            attributes,
            limit,
            exceeded: AtomicBool::new(false),
        })
    }

    pub fn take_bytes(&self) -> Vec<u8> {
        std::mem::take(&mut lock(&self.inner).0)
    }

    pub fn exceeded(&self) -> bool {
        self.exceeded.load(Ordering::Relaxed)
    }

    pub fn rewind(&self) {
        lock(&self.inner).1 = 0;
    }
}

pub(crate) fn stream_ptr(stream: &ComWrapper<MemoryStream>) -> *mut IBStream {
    stream
        .as_com_ref::<IBStream>()
        .map_or(std::ptr::null_mut(), |s| s.as_ptr())
}

impl IStreamAttributesTrait for MemoryStream {
    unsafe fn getFileName(&self, _name: *mut String128) -> tresult {
        kResultFalse
    }

    unsafe fn getAttributes(&self) -> *mut IAttributeList {
        self.attributes
            .as_com_ref::<IAttributeList>()
            .map_or(std::ptr::null_mut(), |list| list.as_ptr())
    }
}

impl IBStreamTrait for MemoryStream {
    unsafe fn read(
        &self,
        buffer: *mut c_void,
        num_bytes: int32,
        num_bytes_read: *mut int32,
    ) -> tresult {
        if !num_bytes_read.is_null() {
            unsafe { *num_bytes_read = 0 };
        }
        if num_bytes < 0 || (buffer.is_null() && num_bytes != 0) {
            return kInvalidArgument;
        }
        let mut inner = lock(&self.inner);
        let (data, pos) = &mut *inner;
        let available = data.len().saturating_sub(*pos);
        let count = (num_bytes as usize).min(available);
        unsafe {
            // A seek beyond current data may be legal. Even zero-length pointer arithmetic past
            // the allocation is invalid, so only form pointers when bytes will actually copy.
            if count != 0 {
                std::ptr::copy_nonoverlapping(data.as_ptr().add(*pos), buffer as *mut u8, count);
            }
            if !num_bytes_read.is_null() {
                *num_bytes_read = count as int32;
            }
        }
        *pos += count;
        kResultOk
    }

    unsafe fn write(
        &self,
        buffer: *mut c_void,
        num_bytes: int32,
        num_bytes_written: *mut int32,
    ) -> tresult {
        if !num_bytes_written.is_null() {
            unsafe { *num_bytes_written = 0 };
        }
        if num_bytes < 0 || (buffer.is_null() && num_bytes != 0) {
            return kInvalidArgument;
        }
        let mut inner = lock(&self.inner);
        let (data, pos) = &mut *inner;
        let count = num_bytes as usize;
        if self.exceeded() || count > self.limit.saturating_sub(*pos) {
            self.exceeded.store(true, Ordering::Relaxed);
            return kResultFalse;
        }
        if count == 0 {
            return kResultOk;
        }
        let bytes = unsafe { std::slice::from_raw_parts(buffer as *const u8, count) };
        if data.len() < *pos + count {
            if *pos + count > data.capacity() && data.capacity() > self.limit / 2 {
                data.reserve_exact(self.limit - data.len());
            }
            data.resize(*pos + count, 0);
        }
        data[*pos..*pos + count].copy_from_slice(bytes);
        *pos += count;
        if !num_bytes_written.is_null() {
            unsafe { *num_bytes_written = count as int32 };
        }
        kResultOk
    }

    unsafe fn seek(&self, pos: int64, mode: int32, result: *mut int64) -> tresult {
        let mut inner = lock(&self.inner);
        let base = match mode as IBStream_::IStreamSeekMode {
            IBStream_::IStreamSeekMode_::kIBSeekSet => 0,
            IBStream_::IStreamSeekMode_::kIBSeekCur => inner.1 as i64,
            IBStream_::IStreamSeekMode_::kIBSeekEnd => inner.0.len() as i64,
            _ => return kInvalidArgument,
        };
        let Some(target) = base.checked_add(pos) else {
            self.exceeded.store(true, Ordering::Relaxed);
            return kInvalidArgument;
        };
        if target < 0 {
            return kInvalidArgument;
        }
        if target as u64 > self.limit as u64 {
            self.exceeded.store(true, Ordering::Relaxed);
            return kResultFalse;
        }
        inner.1 = target as usize;
        if !result.is_null() {
            unsafe { *result = target };
        }
        kResultOk
    }

    unsafe fn tell(&self, pos: *mut int64) -> tresult {
        if pos.is_null() {
            return kInvalidArgument;
        }
        unsafe { *pos = lock(&self.inner).1 as int64 };
        kResultOk
    }
}

mod output_events;
mod processing_events;
pub(crate) use output_events::OutputEventList;
pub(crate) use processing_events::{EventList, ParameterChanges, ParameterInput};

/// Receives edits the controller reports with performEdit, the way a GUI edit reaches the host.
pub(crate) struct ComponentHandler {
    pub events: ParameterEventBuffer,
    pending: Mutex<Vec<(ParamID, ParamValue)>>,
    restart_flags: AtomicI32,
    /// The controller changed its MIDI controller assignments; kept apart from the restart
    /// flags the processing path takes.
    midi_mapping_changed: AtomicBool,
    /// The controller asked for its editor with requestOpenEditor.
    editor_requested: AtomicBool,
    /// The controller changed its parameter list or metadata; the cached list is stale.
    parameters_changed: AtomicBool,
    /// Counts the controller's reports of changed values, which the processor may not have.
    values_generation: AtomicU32,
    /// Counts the times the owning thread took the pending edits, which a block that copied them
    /// before must not forget again.
    taken: AtomicU64,
    /// Output events the processor produced that have no MIDI 1.0 form.
    unconvertible_output_events: AtomicU64,
}

impl Default for ComponentHandler {
    fn default() -> Self {
        Self {
            events: ParameterEventBuffer::default(),
            pending: Mutex::new(Vec::with_capacity(plughost_core::MAX_BLOCK_EVENTS)),
            restart_flags: AtomicI32::new(0),
            midi_mapping_changed: AtomicBool::new(false),
            editor_requested: AtomicBool::new(false),
            parameters_changed: AtomicBool::new(false),
            values_generation: AtomicU32::new(0),
            taken: AtomicU64::new(0),
            unconvertible_output_events: AtomicU64::new(0),
        }
    }
}

impl Class for ComponentHandler {
    type Interfaces = (IComponentHandler, IComponentHandler2);
}

impl ComponentHandler {
    /// Edits reported with performEdit that have not reached the processor yet.
    pub fn take_pending(&self) -> Vec<(ParamID, ParamValue)> {
        let mut pending = lock(&self.pending);
        self.taken.fetch_add(1, Ordering::Relaxed);
        pending.drain(..).collect()
    }

    /// Copies the pending edits into `sent`, which has room for every pending edit, without
    /// taking them: a block that fails leaves them for the next one. Returns the count of takes
    /// for [`Self::forget_delivered`].
    pub fn copy_pending(&self, sent: &mut Vec<(ParamID, ParamValue)>) -> u64 {
        let pending = lock(&self.pending);
        sent.clear();
        sent.extend_from_slice(&pending);
        self.taken.load(Ordering::Relaxed)
    }

    /// Forgets the edits a block delivered from [`Self::copy_pending`]. performEdit replaces an
    /// edit in its place and adds new ones after, so the delivered edits are the queue's first
    /// ones; one replaced since keeps its newer value. Nothing is forgotten when the owning
    /// thread took the queue meanwhile.
    pub fn forget_delivered(&self, sent: &[(ParamID, ParamValue)], taken: u64) {
        let mut pending = lock(&self.pending);
        if self.taken.load(Ordering::Relaxed) != taken {
            return;
        }
        let mut index = 0;
        pending.retain(|edit| {
            let delivered = sent.get(index) == Some(edit);
            index += 1;
            !delivered
        });
    }

    pub fn can_edit(&self, id: ParamID) -> bool {
        let pending = lock(&self.pending);
        pending.len() < plughost_core::MAX_BLOCK_EVENTS
            || pending.iter().any(|(pending_id, _)| *pending_id == id)
    }

    pub fn has_pending(&self) -> bool {
        !lock(&self.pending).is_empty()
    }

    pub fn restart_flags(&self) -> i32 {
        self.restart_flags.load(Ordering::Relaxed)
    }

    pub fn take_restart_flags(&self) -> i32 {
        self.restart_flags.swap(0, Ordering::Relaxed)
    }

    /// Records restart requests taken with [`Self::take_restart_flags`] again.
    pub fn restore_restart_flags(&self, flags: i32) {
        self.restart_flags.fetch_or(flags, Ordering::Relaxed);
    }

    pub fn take_midi_mapping_changed(&self) -> bool {
        self.midi_mapping_changed.swap(false, Ordering::Relaxed)
    }

    pub fn take_editor_requested(&self) -> bool {
        self.editor_requested.swap(false, Ordering::Relaxed)
    }

    pub fn take_parameters_changed(&self) -> bool {
        self.parameters_changed.swap(false, Ordering::Relaxed)
    }

    pub fn values_generation(&self) -> u32 {
        self.values_generation.load(Ordering::Relaxed)
    }

    pub fn add_unconvertible_output_events(&self, count: u64) {
        if count != 0 {
            self.unconvertible_output_events
                .fetch_add(count, Ordering::Relaxed);
        }
    }

    pub fn take_unconvertible_output_events(&self) -> u64 {
        self.unconvertible_output_events.swap(0, Ordering::Relaxed)
    }
}

impl IComponentHandlerTrait for ComponentHandler {
    unsafe fn beginEdit(&self, id: ParamID) -> tresult {
        let _audit = plughost_core::audit::callback();
        self.events
            .record(ParameterEvent::BeginEdit { id: u64::from(id) });
        kResultOk
    }

    unsafe fn performEdit(&self, id: ParamID, value_normalized: ParamValue) -> tresult {
        let _audit = plughost_core::audit::callback();
        // A value past an end by rounding is that end. One further out is not a value: the
        // application reads the values again, as the controller and the processor may differ.
        let Some(value_normalized) = crate::parameter_values::reported(value_normalized, 0.0, 1.0)
        else {
            self.events.record(ParameterEvent::ValuesChanged);
            return kInvalidArgument;
        };
        let mut pending = lock(&self.pending);
        if pending.len() == plughost_core::MAX_BLOCK_EVENTS
            && !pending.iter().any(|(pending_id, _)| *pending_id == id)
        {
            self.events.record(ParameterEvent::ValuesChanged);
            return kResultFalse;
        }
        self.events.record(ParameterEvent::Value {
            id: u64::from(id),
            normalized: value_normalized,
        });
        match pending.iter_mut().find(|(pending_id, _)| *pending_id == id) {
            Some(edit) => edit.1 = value_normalized,
            None => pending.push((id, value_normalized)),
        }
        kResultOk
    }

    unsafe fn endEdit(&self, id: ParamID) -> tresult {
        let _audit = plughost_core::audit::callback();
        self.events
            .record(ParameterEvent::EndEdit { id: u64::from(id) });
        kResultOk
    }

    unsafe fn restartComponent(&self, flags: int32) -> tresult {
        let _audit = plughost_core::audit::callback();
        if flags
            & (RestartFlags_::kParamTitlesChanged
                | RestartFlags_::kReloadComponent
                | RestartFlags_::kMidiCCAssignmentChanged
                | RestartFlags_::kParamIDMappingChanged)
            != 0
        {
            self.events.record(ParameterEvent::MetadataChanged);
        }
        if flags
            & (RestartFlags_::kParamTitlesChanged
                | RestartFlags_::kReloadComponent
                | RestartFlags_::kParamIDMappingChanged)
            != 0
        {
            self.parameters_changed.store(true, Ordering::Relaxed);
        }
        if flags & RestartFlags_::kParamValuesChanged != 0 {
            self.events.record(ParameterEvent::ValuesChanged);
            self.values_generation.fetch_add(1, Ordering::Relaxed);
        }
        if flags & RestartFlags_::kMidiCCAssignmentChanged as int32 != 0 {
            self.midi_mapping_changed.store(true, Ordering::Relaxed);
        }
        self.restart_flags.fetch_or(flags, Ordering::Relaxed);
        kResultOk
    }
}

impl IComponentHandler2Trait for ComponentHandler {
    unsafe fn setDirty(&self, state: vst3::Steinberg::TBool) -> tresult {
        let _audit = plughost_core::audit::callback();
        self.events
            .record(ParameterEvent::Dirty { dirty: state != 0 });
        kResultOk
    }

    /// Accepts the editor view type, which is the only view the host opens.
    unsafe fn requestOpenEditor(&self, name: FIDString) -> tresult {
        let _audit = plughost_core::audit::callback();
        let editor = name.is_null()
            || unsafe { CStr::from_ptr(name) }
                == unsafe { CStr::from_ptr(vst3::Steinberg::Vst::ViewType::kEditor) };
        if !editor {
            return kResultFalse;
        }
        self.editor_requested.store(true, Ordering::Relaxed);
        kResultOk
    }
    unsafe fn startGroupEdit(&self) -> tresult {
        let _audit = plughost_core::audit::callback();
        kResultFalse
    }
    unsafe fn finishGroupEdit(&self) -> tresult {
        let _audit = plughost_core::audit::callback();
        kResultFalse
    }
}

/// Forwards messages between component and controller on whatever thread sends them, as hosts
/// that connect the two directly do. The SDK's ConnectionProxy drops those not sent on the UI
/// thread, which leaves plugins that poll their processor from a display-link thread (FabFilter's
/// level displays) without an answer.
pub(crate) struct ConnectionProxy {
    destination: ComPtr<IConnectionPoint>,
}

impl ConnectionProxy {
    pub fn new(destination: ComPtr<IConnectionPoint>) -> ComWrapper<ConnectionProxy> {
        ComWrapper::new(ConnectionProxy { destination })
    }

    pub fn ptr(proxy: &ComWrapper<ConnectionProxy>) -> *mut IConnectionPoint {
        proxy
            .as_com_ref::<IConnectionPoint>()
            .map_or(std::ptr::null_mut(), |p| p.as_ptr())
    }
}

impl Class for ConnectionProxy {
    type Interfaces = (IConnectionPoint,);
}

impl IConnectionPointTrait for ConnectionProxy {
    unsafe fn connect(&self, _other: *mut IConnectionPoint) -> tresult {
        kResultOk
    }

    unsafe fn disconnect(&self, _other: *mut IConnectionPoint) -> tresult {
        kResultOk
    }

    unsafe fn notify(&self, message: *mut IMessage) -> tresult {
        unsafe { self.destination.notify(message) }
    }
}
