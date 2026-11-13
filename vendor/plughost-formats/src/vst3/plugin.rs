use std::cell::RefCell;
use std::ffi::c_void;
mod audio;
mod capabilities;
mod events;
mod parameters;
mod programs;
#[cfg(test)]
mod timing_tests;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use plughost_core::render::{Process, Tail};
use plughost_core::{
    HostIdentity, ParameterChange, ParameterEvent, ParameterFlags, ParameterInfo, PluginFormat,
    PluginInfo, PluginState, PluginTiming, ProcessConfig, Sample,
};
use vst3::Steinberg::Vst::ParameterInfo_::ParameterFlags_;
use vst3::Steinberg::Vst::{
    IComponentHandlerTrait, IComponentTrait, IEditController, IEditControllerTrait, ParamID,
    ParameterInfo as Vst3ParameterInfo,
};
use vst3::Steinberg::{kNotImplemented, kResultOk};
use vst3::{ComPtr, ComWrapper};

use super::editor::Editor;
use super::engine::{Engine, Processor, Shared, lock};
use super::errors::Vst3Error;
use super::host::{
    ComponentHandler, HostApplication, MemoryStream, StateKind, stream_ptr, wide_string,
};
use super::instance::{Instance, midi_assignments};
use super::module::Module;
use super::parameter_cache::ParameterCache;
use super::preset::Preset;
use crate::AudioThread;
use crate::audio_thread::AudioCalls;
use crate::hosted::ResizeRequest;

/// One VST3 plugin instance hosted in this process.
///
/// Call every method on the thread that created the plugin; its controller and editor live there.
/// Processing can move to another thread with [`HostedPlugin::processor`]. Edits the plugin reports
/// from its editor, and edits made with [`HostedPlugin::set_parameter`], reach the processor with
/// the next process call, or with [`Plugin::flush`]. Values the processor reports, and automation
/// it received, reach the controller when this thread next reads it or calls
/// [`HostedPlugin::idle`].
///
/// [`HostedPlugin::processor`]: crate::HostedPlugin::processor
/// [`HostedPlugin::set_parameter`]: crate::HostedPlugin::set_parameter
/// [`HostedPlugin::idle`]: crate::HostedPlugin::idle
pub struct Plugin {
    info: PluginInfo,
    controller: ComPtr<IEditController>,
    handler: ComWrapper<ComponentHandler>,
    parameters: RefCell<Arc<ParameterCache>>,
    engine: Shared,
    audio: AudioCalls,
    editor: Option<Editor>,
    _host: ComWrapper<HostApplication>,
    /// Declared last: the module stays loaded until every pointer into it is released.
    _module: Rc<Module>,
}

impl std::fmt::Debug for Plugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Plugin")
            .field("info", &self.info)
            .field("editor_open", &self.editor.is_some())
            .finish_non_exhaustive()
    }
}

impl Plugin {
    /// Creates and initializes an instance of the class with `class_id` from `module`.
    pub fn new(
        module: &Rc<Module>,
        class_id: &str,
        identity: &HostIdentity,
    ) -> Result<Plugin, crate::Error> {
        identity.validate()?;
        let (cid, info) = module.find_class(class_id)?;
        let host = ComWrapper::new(HostApplication {
            name: identity.name.clone(),
        });
        let instance = Instance::create(module, &cid, &host)?;
        Ok(Plugin {
            info,
            controller: instance.controller.clone(),
            handler: instance.handler.clone(),
            parameters: RefCell::new(Arc::new(ParameterCache::read(&instance.controller))),
            engine: Arc::new(Mutex::new(Engine {
                instance: Some(Arc::new(instance)),
                prepared: None,
                restart_deferred: false,
            })),
            audio: AudioCalls::default(),
            editor: None,
            _host: host,
            _module: Rc::clone(module),
        })
    }

    pub(crate) fn info(&self) -> &PluginInfo {
        &self.info
    }

    /// Negotiates the main bus layouts, sets up processing, and activates the plugin.
    /// Reserves native audio storage for the maximum block size. Larger maxima increase
    /// memory requirements; native plugin code and control callbacks may still allocate while processing.
    pub(crate) fn prepare(&mut self, config: &ProcessConfig) -> Result<(), Vst3Error> {
        let parameters = self.parameter_cache();
        lock(&self.engine).prepare(config, &parameters)?;
        self.seed_held_values();
        Ok(())
    }

    /// Hands the controller's current values to the prepared processing side; see
    /// [`super::engine::Prepared::seed_held_values`].
    pub(super) fn seed_held_values(&self) {
        // Values automation left on the processing side reach the controller first.
        self.sync_controller();
        let parameters = self.parameter_cache();
        let generation = self.handler.values_generation();
        let values: Vec<f64> = (0..parameters.len())
            // A value with no normalized form is unknown to the processing side, not 0.
            .map(|index| {
                self.controller_value(parameters.native_id(index))
                    .unwrap_or(f64::NAN)
            })
            .collect();
        if let Some(prepared) = &mut lock(&self.engine).prepared {
            prepared.seed_held_values(&values, generation);
        }
    }

    /// Stops processing and deactivates the plugin.
    pub fn unprepare(&mut self) {
        lock(&self.engine).unprepare();
    }

    /// The active legacy processing configuration, or `None` before preparation.
    pub fn config(&self) -> Option<ProcessConfig> {
        lock(&self.engine)
            .prepared
            .as_ref()
            .map(|prepared| prepared.config)
    }

    /// Processes one block with one native process call. `input` and `output` hold one slice per
    /// active bus channel in native bus order, all of the same length, at most the prepared block
    /// size. `changes` take effect at their sample offsets and hold until the next change.
    /// `events` go to the event input as notes and, through the controller's MIDI mapping, as
    /// parameter changes; MIDI messages without an assignment are dropped.
    pub fn process<S: Sample>(
        &mut self,
        context: &plughost_core::BlockContext,
        input: &[&[S]],
        output: &mut [&mut [S]],
        changes: &[ParameterChange],
        events: &plughost_core::Events,
        produced: &mut plughost_core::Events,
    ) -> Result<(), crate::Error> {
        Ok(lock(&self.engine).process(context, input, output, changes, events, produced)?)
    }

    /// A handle that processes this plugin from another thread.
    pub(crate) fn processor(&self) -> Processor {
        Processor::new(&self.engine)
    }

    /// Delivers edits to a prepared processor on `thread`, the thread that processes it.
    pub fn set_audio_thread(&mut self, thread: Arc<dyn AudioThread>) {
        self.audio = AudioCalls::new(thread);
    }

    /// Delivers edits that have not reached the processor yet, with a process call that carries
    /// no audio (`numSamples = 0`).
    pub fn flush(&mut self) -> Result<(), crate::Error> {
        Ok(self.flush_edits()?)
    }

    pub(super) fn flush_edits(&mut self) -> Result<(), Vst3Error> {
        self.deliver_edits()?;
        self.sync_controller();
        Ok(())
    }

    /// Makes the process call of [`Plugin::flush`]. VST3 calls `process` on the processing
    /// thread, so a prepared processor takes it on its audio thread; an unprepared one is
    /// activated for it on this thread, which owns activation.
    fn deliver_edits(&self) -> Result<(), Vst3Error> {
        let parameters = self.parameter_cache();
        if lock(&self.engine).prepared.is_none() {
            return lock(&self.engine).flush(&parameters);
        }
        let engine = Arc::clone(&self.engine);
        self.audio.run(move || lock(&engine).flush(&parameters))
    }

    /// The prepared plugin's timing, with the tail read again on this thread.
    pub(crate) fn timing(&self) -> Result<PluginTiming, Vst3Error> {
        if self.handler.has_pending() && lock(&self.engine).prepared.is_some() {
            self.deliver_edits()?;
        }
        let mut engine = lock(&self.engine);
        engine.read_tail();
        engine.timing()
    }

    /// As [`Plugin::timing`], unless the processing thread holds the engine.
    pub(crate) fn poll_timing(&self) -> Option<PluginTiming> {
        let try_lock = || match self.engine.try_lock() {
            Ok(engine) => Some(engine),
            Err(std::sync::TryLockError::Poisoned(error)) => Some(error.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => None,
        };
        let mut engine = try_lock()?;
        engine.read_tail();
        engine.timing().ok()
    }

    /// The optional VST3 processor's requested context flags, queried on the owning thread
    /// during preparation. Unavailable caller time fields are never fabricated to satisfy them.
    pub fn process_context_requirements(&self) -> Result<Option<u32>, crate::Error> {
        Ok(lock(&self.engine)
            .prepared
            .as_ref()
            .map(|prepared| prepared.context_requirements)
            .ok_or(Vst3Error::NotPrepared)?)
    }

    pub(crate) fn parameters(&self) -> Vec<ParameterInfo> {
        self.parameter_cache().infos().to_vec()
    }

    /// The controller's normalized value, clamped to `0..=1`, or `None` for an ID the plugin
    /// does not have or a value that is not finite. Some controllers show a restored or host-set
    /// value late; the processor's value is what [`HostedPlugin::save_state`] stores.
    ///
    /// [`HostedPlugin::save_state`]: crate::HostedPlugin::save_state
    pub fn parameter_value(&self, id: u64) -> Option<f64> {
        let parameters = self.parameter_cache();
        let id = parameters.native_id(parameters.index(id)?);
        self.sync_controller();
        self.controller_value(id)
    }

    /// The controller's value of `id`, clamped, or `None` when it has no normalized form.
    fn controller_value(&self, id: ParamID) -> Option<f64> {
        let value = unsafe { self.controller.getParamNormalized(id) };
        crate::parameter_values::normalized(value, 0.0, 1.0)
    }

    /// Changes a parameter the way an edit in the plugin's editor does: the controller takes the
    /// value, and the processor receives it with the next process call or flush.
    /// Unknown/read-only IDs and non-finite or unnormalized values are rejected before editing.
    pub(crate) fn set_parameter(&mut self, id: u64, value: f64) -> Result<(), Vst3Error> {
        let parameters = self.parameter_cache();
        let index = parameters.index(id).ok_or(Vst3Error::Input(
            plughost_core::InputError::UnknownParameter { id },
        ))?;
        parameters
            .info(index)
            .validate_edit(value)
            .map_err(Vst3Error::Input)?;
        let id = parameters.native_id(index);
        if !self.handler.can_edit(id) {
            return Err(Vst3Error::Input(plughost_core::InputError::EventCapacity));
        }
        unsafe {
            self.controller.setParamNormalized(id, value);
            if self.handler.performEdit(id, value) != kResultOk {
                return Err(Vst3Error::Input(plughost_core::InputError::EventCapacity));
            }
        }
        Ok(())
    }

    /// The cached parameter list, read again after the controller reported a change. A new list
    /// also replaces the processing thread's copy.
    fn parameter_cache(&self) -> Arc<ParameterCache> {
        if self.handler.take_parameters_changed() {
            let fresh = Arc::new(ParameterCache::read(&self.controller));
            if let Some(prepared) = &mut lock(&self.engine).prepared {
                prepared.set_parameters(&fresh);
            }
            // The processing thread no longer writes to the old list; deliver what it left.
            self.sync_controller();
            *self.parameters.borrow_mut() = fresh;
        }
        Arc::clone(&self.parameters.borrow())
    }

    /// Delivers the values the processing thread handed back to the controller, and tells the
    /// application about those the processor reported.
    fn sync_controller(&self) {
        let parameters = Arc::clone(&self.parameters.borrow());
        parameters.take_controller_values(|id, value, from_processor| {
            unsafe { self.controller.setParamNormalized(id, value) };
            if from_processor && plughost_core::internal::validate_normalized(value).is_ok() {
                self.handler.events.record(ParameterEvent::Value {
                    id: u64::from(id),
                    normalized: value,
                });
            }
        });
    }

    /// Saves the component and controller state, after delivering pending edits to the processor.
    pub(crate) fn save_state(
        &mut self,
        purpose: plughost_core::StatePurpose,
    ) -> Result<PluginState, Vst3Error> {
        let kind = state_kind(purpose)?;
        let component = MemoryStream::from_bytes(&[], kind);
        self.deliver_edits()?;
        let instance = lock(&self.engine).handle()?;
        let result = unsafe { instance.component.getState(stream_ptr(&component)) };
        if component.exceeded() {
            return Err(Vst3Error::StateTooLarge);
        }
        if result != kResultOk {
            return Err(Vst3Error::GetState(result));
        }
        self.sync_controller();
        let component = component.take_bytes();
        let controller =
            MemoryStream::bounded(kind, plughost_core::MAX_STATE_BYTES - component.len());
        let result = unsafe { self.controller.getState(stream_ptr(&controller)) };
        if controller.exceeded() {
            return Err(Vst3Error::StateTooLarge);
        }
        // As on restore, a controller without state of its own may not implement it; it then
        // has none, which differs from an empty state.
        let controller = if result == kResultOk {
            Some(controller.take_bytes())
        } else if result == kNotImplemented {
            None
        } else {
            return Err(Vst3Error::GetState(result));
        };
        Ok(PluginState::new(&self.info, component, controller))
    }

    /// Restores a state saved from this plugin class. Edits not yet delivered are discarded.
    pub(crate) fn restore_state(
        &mut self,
        state: &PluginState,
        purpose: plughost_core::StatePurpose,
    ) -> Result<(), Vst3Error> {
        self.restore(state, state_kind(purpose)?)
    }

    fn restore(&mut self, state: &PluginState, kind: StateKind) -> Result<(), Vst3Error> {
        state.validate().map_err(Vst3Error::Input)?;
        if state.id() != &self.info.id {
            return Err(Vst3Error::StateMismatch);
        }
        // Values the processing thread handed back predate the state and must not overwrite it.
        self.sync_controller();
        let component = MemoryStream::from_bytes(state.component(), kind);
        let result = {
            let mut engine = lock(&self.engine);
            let result = {
                let instance = engine.instance()?;
                instance.handler.take_pending();
                unsafe { instance.component.setState(stream_ptr(&component)) }
            };
            if let Some(prepared) = &mut engine.prepared {
                prepared.forget_held_values();
            }
            result
        };
        if result != kResultOk {
            return Err(Vst3Error::SetState(result));
        }
        // A controller that is also the component still takes the component state here, as other
        // hosts do; some keep their controller-side values only through this call.
        component.rewind();
        controller_result(unsafe { self.controller.setComponentState(stream_ptr(&component)) })?;
        // An empty controller state is not passed on, as before 0.2.0: some controllers that
        // write nothing refuse to read nothing.
        if let Some(controller) = state.controller().filter(|state| !state.is_empty()) {
            let stream = MemoryStream::from_bytes(controller, kind);
            controller_result(unsafe { self.controller.setState(stream_ptr(&stream)) })?;
        }
        self.seed_held_values();
        Ok(())
    }

    /// Restores a `.vstpreset` saved for this plugin class, telling the plugin it is a preset.
    pub fn restore_preset(&mut self, preset: &Preset) -> Result<(), crate::Error> {
        if plughost_core::PluginId::new(PluginFormat::Vst3, preset.class_id.as_str())
            != self.info.id
        {
            return Err(Vst3Error::StateMismatch.into());
        }
        let controller = preset.controller.as_ref().map_or(0, Vec::len);
        if preset.component.len().saturating_add(controller) > plughost_core::MAX_STATE_BYTES {
            return Err(plughost_core::InputError::StateSize.into());
        }
        // The identity is the loaded plugin's: a preset holds none but its class ID.
        let state = PluginState::new(
            &self.info,
            preset.component.clone(),
            preset.controller.clone(),
        );
        Ok(self.restore(&state, StateKind::Preset)?)
    }

    /// Clears the plugin's internal audio state (reverb tails, delay lines, envelopes) by
    /// deactivating and reactivating it, keeping its configuration, parameters, and editor.
    pub(crate) fn reset(&mut self) -> Result<(), Vst3Error> {
        lock(&self.engine).reset()
    }

    /// Opens the plugin's editor in `parent`, a native view the caller owns (`NSView` on macOS,
    /// `HWND` on Windows), and returns its size. `resize` is called when the editor asks for a new
    /// size.
    ///
    /// # Safety
    ///
    /// `parent` must be a valid native view that stays alive until [`Plugin::close_editor`] or
    /// until the plugin is dropped.
    pub(crate) unsafe fn open_editor(
        &mut self,
        parent: *mut c_void,
        resize: ResizeRequest,
    ) -> Result<(u32, u32), Vst3Error> {
        self.close_editor();
        let (editor, size) = unsafe { Editor::attach(&self.controller, parent, resize)? };
        self.editor = Some(editor);
        Ok(size)
    }

    pub(crate) fn close_editor(&mut self) {
        self.editor = None;
    }

    pub(crate) fn take_editor_request(&self) -> Option<crate::EditorRequest> {
        self.handler
            .take_editor_requested()
            .then_some(crate::EditorRequest::Open)
    }

    pub(crate) fn editor_can_resize(&self) -> bool {
        self.editor.as_ref().is_some_and(Editor::can_resize)
    }

    /// Applies the monitor scale when the Windows view supports it and returns its pixel size.
    #[cfg(target_os = "windows")]
    pub(crate) fn set_editor_scale(&mut self, scale: f64) -> Option<(u32, u32)> {
        self.editor.as_ref()?.set_scale(scale)
    }

    /// Offers the size the user dragged the editor window to; returns the size the view accepted.
    pub(crate) fn resize_editor(&mut self, width: u32, height: u32) -> Option<(u32, u32)> {
        self.editor
            .as_ref()
            .map(|editor| editor.resize(width, height))
    }

    /// Serves what the controller needs from the owning thread: values the processing thread
    /// handed back, and new MIDI controller assignments after it reports them. Call it regularly
    /// from that thread.
    pub(crate) fn idle(&mut self) {
        self.sync_controller();
        if self.handler.take_midi_mapping_changed() {
            let parameters = self.parameter_cache();
            let inputs = lock(&self.engine)
                .prepared
                .as_ref()
                .map(|prepared| prepared.event_inputs.clone());
            if let Some(inputs) = inputs {
                let assignments = midi_assignments(&self.controller, &parameters, &inputs);
                if let Some(prepared) = &mut lock(&self.engine).prepared {
                    prepared.midi_assignments = assignments;
                }
            }
        }
    }
}

impl Drop for Plugin {
    fn drop(&mut self) {
        self.editor = None;
        let mut engine = lock(&self.engine);
        engine.unprepare();
        // Terminates the instance while its module is still loaded; processors still held on
        // other threads now fail with Vst3Error::Closed.
        engine.instance = None;
    }
}

impl<S: Sample> Process<S> for Plugin {
    type Error = crate::Error;

    fn sample_rate(&self) -> f64 {
        self.config().map_or(0.0, |config| config.sample_rate)
    }

    fn max_block_size(&self) -> usize {
        self.config().map_or(0, |config| config.max_block_size)
    }

    fn input_channels(&self) -> usize {
        lock(&self.engine).prepared.as_ref().map_or(0, |prepared| {
            prepared
                .audio_buses
                .iter()
                .filter(|bus| {
                    bus.direction == plughost_core::AudioDirection::Input
                        && bus.active == Some(true)
                })
                .map(|bus| bus.channels as usize)
                .sum()
        })
    }

    fn output_channels(&self) -> usize {
        lock(&self.engine).prepared.as_ref().map_or(0, |prepared| {
            prepared
                .audio_buses
                .iter()
                .filter(|bus| {
                    bus.direction == plughost_core::AudioDirection::Output
                        && bus.active == Some(true)
                })
                .map(|bus| bus.channels as usize)
                .sum()
        })
    }

    fn latency(&self) -> Result<u32, crate::Error> {
        Ok(Plugin::timing(self)?.latency)
    }

    fn tail(&self) -> Result<Tail, crate::Error> {
        Ok(Plugin::timing(self)?.tail)
    }

    /// A latency the plugin announced is applied when it activates again, and many plugins delay
    /// by it at once, so a render does not start on the latency it was activated with.
    fn begin_render(&mut self) -> Result<(), crate::Error> {
        if lock(&self.engine).timing()?.latency_changed {
            return Err(plughost_core::RenderError::LatencyNotApplied.into());
        }
        Ok(())
    }

    fn validate_automation(
        &mut self,
        automation: &[plughost_core::AutomationEvent],
    ) -> Result<(), crate::Error> {
        let parameters = self.parameter_cache();
        for event in automation {
            if event.slot != 0 {
                return Err(plughost_core::InputError::Slot.into());
            }
            parameters
                .find(event.change.id)
                .ok_or(plughost_core::InputError::UnknownParameter {
                    id: event.change.id,
                })?
                .automation_value(event.change.value)?;
        }
        Ok(())
    }

    fn validate_events(&mut self, events: &plughost_core::Events) -> Result<(), crate::Error> {
        let engine = lock(&self.engine);
        let prepared = engine.prepared.as_ref().ok_or(Vst3Error::NotPrepared)?;
        if let Some(event) = events.iter().find(|event| {
            !prepared
                .event_inputs
                .get(event.port)
                .copied()
                .unwrap_or(false)
        }) {
            return Err(plughost_core::InputError::EventPort { port: event.port }.into());
        }
        Ok(())
    }

    fn process(
        &mut self,
        context: &plughost_core::BlockContext,
        input: &[&[S]],
        output: &mut [&mut [S]],
        automation: &[plughost_core::AutomationEvent],
        events: &plughost_core::Events,
        produced: &mut plughost_core::Events,
    ) -> Result<(), crate::Error> {
        if automation.iter().any(|event| event.slot != 0) {
            return Err(plughost_core::InputError::Slot.into());
        }
        let changes: Vec<_> = automation.iter().map(|event| event.change).collect();
        Plugin::process(self, context, input, output, &changes, events, produced)
    }
}

pub(super) fn parameter_info(info: &Vst3ParameterInfo) -> ParameterInfo {
    let flag = |bit: i32| info.flags & bit != 0;
    {
        let mut parameter_info = ParameterInfo::new(u64::from(info.id), wide_string(&info.title));
        parameter_info.short_title = wide_string(&info.shortTitle);
        parameter_info.units = wide_string(&info.units);
        parameter_info.step_count = info.stepCount.max(0) as u32;
        parameter_info.default_value = Some(info.defaultNormalizedValue);
        parameter_info.flags = {
            let mut parameter_flags = ParameterFlags::default();
            parameter_flags.discrete = info.stepCount > 0;
            parameter_flags.automatable = flag(ParameterFlags_::kCanAutomate);
            parameter_flags.read_only = flag(ParameterFlags_::kIsReadOnly);
            parameter_flags.hidden = flag(ParameterFlags_::kIsHidden);
            parameter_flags.bypass = flag(ParameterFlags_::kIsBypass);
            parameter_flags.list = flag(ParameterFlags_::kIsList);
            parameter_flags.program_change = flag(ParameterFlags_::kIsProgramChange);
            parameter_flags
        };
        parameter_info
    }
}

fn state_kind(purpose: plughost_core::StatePurpose) -> Result<StateKind, Vst3Error> {
    match purpose {
        plughost_core::StatePurpose::Project => Ok(StateKind::Project),
        plughost_core::StatePurpose::Preset => Ok(StateKind::Preset),
        _ => Err(Vst3Error::StatePurposeUnsupported),
    }
}

/// A controller that keeps no state of its own answers kNotImplemented, as the SDK's default
/// controller does; only a rejection fails the restore.
fn controller_result(result: i32) -> Result<(), Vst3Error> {
    if result == kResultOk || result == kNotImplemented {
        Ok(())
    } else {
        Err(Vst3Error::SetState(result))
    }
}
