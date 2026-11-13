//! One CLAP plugin instance. The instance stays on the thread that created it; its audio
//! processor lives in an engine shared with a processing thread.

mod audio;
mod audio_process;
mod buffers;
mod event_ports;
mod events;
mod flushing;
mod input_events;
mod output_events;
mod parameter_cache;
mod parameters;
mod presets;
mod render;
#[cfg(test)]
mod thread_tests;
#[cfg(test)]
mod timing_tests;
use std::ffi::{CString, c_void};
mod capabilities;
use std::io::Cursor;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use clack_extensions::audio_ports::{AudioPortFlags, AudioPortType};
use clack_extensions::gui::{GuiApiType, GuiConfiguration, GuiSize, Window};
use clack_extensions::note_ports::{NoteDialect, NoteDialects};
use clack_extensions::render::RenderMode;
use clack_extensions::surround::SurroundChannel;
use clack_host::events::event_types::{
    MidiEvent as ClapMidiEvent, NoteOffEvent, NoteOnEvent, ParamValueEvent,
};
use clack_host::events::{Match, Pckn};
use clack_host::plugin::PluginDescriptor;
use clack_host::prelude::*;
use plughost_core::internal::{DiagnosticBuffer, events_fit};
use plughost_core::render::Tail;
use plughost_core::{DiagnosticBatch, PluginId};
use plughost_core::{
    HostIdentity, Layout, Message, ParameterInfo, PluginFormat, PluginInfo, PluginKind,
    PluginState, PluginTiming, ProcessConfig, ProcessMode, SampleFormat,
};

use super::errors::ClapError;
use super::host::{Host, MainThread, Shared, in_audio_context};
use crate::AudioThread;
use crate::audio_thread::AudioCalls;
use crate::hosted::{EditorRequest, EditorView, ResizeRequest};
use parameter_cache::ParameterCache;

const INSTRUMENT: &str = "instrument";

fn text(value: Option<&std::ffi::CStr>) -> String {
    value
        .map(|v| v.to_string_lossy().into_owned())
        .unwrap_or_default()
}

pub(super) fn info(descriptor: &PluginDescriptor) -> Option<PluginInfo> {
    let categories: Vec<String> = descriptor
        .features()
        .map(|feature| feature.to_string_lossy().into_owned())
        .collect();
    Some({
        let mut plugin_info = PluginInfo::new(
            PluginId::new(PluginFormat::Clap, descriptor.id()?.to_str().ok()?),
            text(descriptor.name()),
            if categories.iter().any(|c| c == INSTRUMENT) {
                PluginKind::Instrument
            } else {
                PluginKind::Effect
            },
        );
        plugin_info.vendor = text(descriptor.vendor());
        plugin_info.version = text(descriptor.version());
        plugin_info.sdk_version = "CLAP".to_owned();
        plugin_info.categories = categories;
        plugin_info
    })
}

struct Engine {
    processor: Option<PluginAudioProcessor<Host>>,
    prepared: Option<Prepared>,
    /// Blocks go on while a restart request waits (see `BlockProcessor::defer_restart`).
    restart_deferred: bool,
}

struct Prepared {
    input_events: input_events::InputBuffer,
    /// Validated block edits in native form, before pending host edits may be consumed.
    automation: Vec<(u32, ClapId, f64)>,
    buffers: buffers::AudioBuffers,
    config: ProcessConfig,
    audio_config: Option<plughost_core::AudioConfig>,
    /// The ports as scanned just before activation. CLAP lets a host scan them only while the
    /// plugin is inactive, so queries while it is active answer with these.
    buses: Vec<plughost_core::AudioBusInfo>,
    event_ports: Vec<plughost_core::EventPortInfo>,
    port_support: capabilities::PortSupport,
    /// Channel count of every input and output port; the main port is at `main_input` or
    /// `main_output`.
    inputs: Vec<usize>,
    outputs: Vec<usize>,
    main_input: Option<usize>,
    main_output: Option<usize>,
    /// Per note input port index, the dialects of a port the routing uses; events reach only
    /// those ports.
    event_inputs: Vec<Option<NoteDialects>>,
    /// The latency the plugin reported as it activated; CLAP changes it only by a restart.
    latency: u32,
    /// The tail read at activation and after each block in which the plugin reported a change.
    tail: Tail,
    steady_time: u64,
    parameters: Arc<ParameterCache>,
    /// The plugin's output notes with IDs.
    sounding: output_events::Sounding,
}

fn lock(engine: &Mutex<Engine>) -> MutexGuard<'_, Engine> {
    engine
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// An in-process native plugin owned and controlled by the thread that created it.
pub struct Plugin {
    diagnostics: DiagnosticBuffer,
    info: PluginInfo,
    instance: PluginInstance<Host>,
    engine: Arc<Mutex<Engine>>,
    audio: AudioCalls,
    parameters: Arc<ParameterCache>,
    editor: Option<ResizeRequest>,
    /// The size the host window last gave the embedded editor, which a failed resize restores,
    /// if the plugin told it.
    editor_size: Option<(u32, u32)>,
    /// While host edits are queued, how many blocks had processed when an idle first saw them
    /// waiting, and when: edits that waited that long with no block are flushed.
    idle_edits: Option<(u64, Instant)>,
    audio_active: std::collections::HashMap<(bool, u64), bool>,
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
    /// Creates the named class of a loaded module with the supplied host identity.
    pub fn new(
        entry: &super::Entry,
        class_id: &str,
        identity: &HostIdentity,
    ) -> Result<Plugin, crate::Error> {
        let diagnostics = DiagnosticBuffer::default();
        identity.validate().map_err(ClapError::Input)?;
        let entry = entry.native();
        let factory = entry.get_plugin_factory().ok_or(ClapError::NoFactory)?;
        let info = factory
            .plugin_descriptors()
            .filter_map(info)
            .find(|info| info.id == PluginId::new(PluginFormat::Clap, class_id))
            .ok_or_else(|| ClapError::ClassNotFound(class_id.to_owned()))?;
        let id =
            CString::new(class_id).map_err(|_| ClapError::ClassNotFound(class_id.to_owned()))?;
        let host_info = HostInfo::new(
            identity.name.as_str(),
            identity.vendor.as_str(),
            "",
            identity.version.as_str(),
        )
        .map_err(|_| ClapError::Input(plughost_core::InputError::HostIdentity))?;
        let instance = PluginInstance::<Host>::new(
            |_| {
                Shared::new(
                    diagnostics.clone(),
                    PluginId::new(PluginFormat::Clap, class_id),
                )
            },
            |shared| MainThread::new(shared),
            entry,
            &id,
            &host_info,
        )
        .map_err(ClapError::Instantiate)?;
        Ok(Plugin {
            diagnostics,
            info,
            instance,
            engine: Arc::new(Mutex::new(Engine {
                processor: None,
                prepared: None,
                restart_deferred: false,
            })),
            audio: AudioCalls::default(),
            parameters: Arc::default(),
            editor: None,
            editor_size: None,
            idle_edits: None,
            audio_active: std::collections::HashMap::new(),
        })
    }

    pub(crate) fn info(&self) -> &PluginInfo {
        &self.info
    }

    /// Makes the plugin's audio-thread calls between blocks on `thread`, the thread that
    /// processes it.
    pub fn set_audio_thread(&mut self, thread: Arc<dyn AudioThread>) {
        self.audio = AudioCalls::new(thread);
    }

    /// Drains messages from the CLAP host log extension, including initialization messages.
    pub(crate) fn take_diagnostics(&self) -> DiagnosticBatch {
        self.diagnostics.take()
    }

    fn shared(&self) -> &Shared {
        self.instance.access_shared_handler(|shared| {
            // SAFETY: the shared handler lives as long as the instance, which outlives `self`'s borrow.
            unsafe { &*(shared as *const Shared) }
        })
    }

    /// Stops processing on the audio thread (`stop_processing` is an audio-thread function),
    /// then deactivates on the main thread.
    fn deactivate(&mut self) {
        let processor = {
            let mut engine = lock(&self.engine);
            engine.prepared = None;
            engine.processor.take()
        };
        if let Some(processor) = processor {
            let stopped = self.audio.run(|| {
                let _audit = plughost_core::audit::plugin();
                let _floats = crate::float_mode::FlushToZero::enter();
                in_audio_context(|| processor.into_stopped())
            });
            self.instance.deactivate(stopped);
        }
    }

    /// The active legacy processing configuration, or `None` before preparation.
    pub fn config(&self) -> Option<ProcessConfig> {
        lock(&self.engine)
            .prepared
            .as_ref()
            .map(|prepared| prepared.config)
    }

    pub(crate) fn processor(&self) -> Processor {
        Processor {
            engine: Arc::clone(&self.engine),
        }
    }

    /// Processes one block on this thread. `input` and `output` hold the main port channels
    /// after [`HostedPlugin::prepare`], or every active port's channels in native order after
    /// [`HostedPlugin::prepare_audio`]. `events` address note input ports by native index: the
    /// first one after [`HostedPlugin::prepare`], the requested ones after
    /// [`HostedPlugin::prepare_audio`]; an event on another port fails the block with
    /// `InputError::EventPort`. Notes go as CLAP note events when the port takes them, other
    /// messages (and notes on MIDI-only ports) as MIDI events when it takes MIDI; messages the
    /// port takes in neither form are dropped.
    ///
    /// [`HostedPlugin::prepare`]: crate::HostedPlugin::prepare
    /// [`HostedPlugin::prepare_audio`]: crate::HostedPlugin::prepare_audio
    pub fn process<S: plughost_core::Sample>(
        &mut self,
        context: &plughost_core::BlockContext,
        input: &[&[S]],
        output: &mut [&mut [S]],
        automation: &[plughost_core::ParameterChange],
        events: &plughost_core::Events,
        produced: &mut plughost_core::Events,
    ) -> Result<(), crate::Error> {
        Ok(self
            .processor()
            .process(context, input, output, automation, events, produced)?)
    }

    /// The prepared plugin's timing, with its tail read again: a plugin reports a changed tail
    /// only from a block it processes, so a tail it changed since, for example with a state it
    /// loaded, shows here before the next block.
    pub(crate) fn timing(&self) -> Result<PluginTiming, ClapError> {
        if self.shared().has_pending() {
            self.flush_active();
        }
        let mut engine = lock(&self.engine);
        let Engine {
            processor: Some(processor),
            prepared: Some(prepared),
            ..
        } = &mut *engine
        else {
            return Err(ClapError::NotPrepared);
        };
        self.shared().tail_changed.store(false, Ordering::Relaxed);
        prepared.tail = read_tail(processor);
        Ok(timing(processor, prepared))
    }

    pub(crate) fn poll_timing(&self) -> Option<PluginTiming> {
        let mut engine = match self.engine.try_lock() {
            Ok(engine) => engine,
            Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return None,
        };
        let Engine {
            processor: Some(processor),
            prepared: Some(prepared),
            ..
        } = &mut *engine
        else {
            return None;
        };
        if self.shared().tail_changed.swap(false, Ordering::Relaxed) {
            prepared.tail = read_tail(processor);
        }
        Some(timing(processor, prepared))
    }

    /// The cached parameter list, read again after the plugin rescanned its parameter info. The
    /// processing thread's copy follows.
    fn parameter_cache(&mut self) -> Arc<ParameterCache> {
        if self
            .shared()
            .parameters_changed
            .swap(false, Ordering::Relaxed)
        {
            let params = self.shared().extensions().params;
            let (parameters, count) = ParameterCache::read(params, &self.instance.plugin_handle());
            if count > parameter_cache::MAX_PARAMETERS {
                self.shared()
                    .plugin_misbehaving(&super::errors::too_many_parameters(
                        count,
                        parameter_cache::MAX_PARAMETERS,
                    ));
            }
            self.parameters = Arc::new(parameters);
            if let Some(prepared) = &mut lock(&self.engine).prepared {
                prepared.parameters = Arc::clone(&self.parameters);
            }
        }
        Arc::clone(&self.parameters)
    }

    /// Each parameter with its current value, normalized over its range and clamped to it, or
    /// `None` when the plugin does not give one, or it or its range is not finite. Queued host
    /// edits reach the plugin first.
    pub(crate) fn parameters(&mut self) -> Vec<(ParameterInfo, Option<f64>)> {
        if self.shared().has_pending() {
            self.flush();
        }
        let Some(params) = self.shared().extensions().params else {
            return Vec::new();
        };
        let cache = self.parameter_cache();
        let handle = self.instance.plugin_handle();
        cache
            .iter()
            .map(|parameter| {
                let value = params
                    .get_value(&handle, parameter.native)
                    .and_then(|plain| parameter.normalized(plain));
                (parameter.info.clone(), value)
            })
            .collect()
    }

    /// Queues a host edit of a normalized value. An inactive plugin is flushed with it right
    /// away; an active one takes it with the next block, or with a flush from [`Plugin::idle`]
    /// once no block came for two of the longest blocks.
    pub(crate) fn set_parameter(&mut self, id: u64, value: f64) -> Result<(), ClapError> {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(ClapError::Input(plughost_core::InputError::ParameterValue));
        }
        let cache = self.parameter_cache();
        let parameter = cache.get(id).ok_or(ClapError::Input(
            plughost_core::InputError::UnknownParameter { id },
        ))?;
        if parameter.info.flags.read_only {
            return Err(ClapError::Input(
                plughost_core::InputError::ReadOnlyParameter { id },
            ));
        }
        let plain = parameter.plain(value).ok_or(ClapError::ParameterResult)?;
        let mut pending = self
            .shared()
            .pending
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if pending.len() == plughost_core::MAX_BLOCK_EVENTS {
            return Err(ClapError::Input(plughost_core::InputError::EventCapacity));
        }
        pending.push((parameter.native.get(), plain));
        drop(pending);
        // An inactive plugin gets no blocks; its editor shows the edit once it is flushed.
        if lock(&self.engine).processor.is_none() {
            self.flush();
        }
        Ok(())
    }

    pub(crate) fn save_state(
        &mut self,
        purpose: plughost_core::StatePurpose,
    ) -> Result<PluginState, ClapError> {
        let context = self.shared().extensions().state_context;
        if context.is_none() && purpose != plughost_core::StatePurpose::Project {
            return Err(ClapError::StatePurposeUnsupported);
        }
        let context_type = state_context(purpose)?;
        self.flush();
        let state = self
            .shared()
            .extensions()
            .state
            .ok_or(ClapError::StateUnsupported)?;
        let mut bytes = crate::state_stream::StateWriter::new(plughost_core::MAX_STATE_BYTES);
        let result = if let Some(context) = context {
            context.save(&self.instance.plugin_handle(), &mut bytes, context_type)
        } else {
            state.save(&self.instance.plugin_handle(), &mut bytes)
        };
        if bytes.exceeded() {
            return Err(ClapError::StateTooLarge);
        }
        result.map_err(|_| ClapError::State)?;
        Ok(PluginState::new(&self.info, bytes.into_bytes(), None))
    }

    pub(crate) fn restore_state(
        &mut self,
        state: &PluginState,
        purpose: plughost_core::StatePurpose,
    ) -> Result<(), ClapError> {
        state.validate().map_err(ClapError::Input)?;
        let context = self.shared().extensions().state_context;
        if context.is_none() && purpose != plughost_core::StatePurpose::Project {
            return Err(ClapError::StatePurposeUnsupported);
        }
        let context_type = state_context(purpose)?;
        if state.id() != &self.info.id {
            return Err(ClapError::StateMismatch);
        }
        self.shared().take_pending();
        let extension = self
            .shared()
            .extensions()
            .state
            .ok_or(ClapError::StateUnsupported)?;
        if let Some(context) = context {
            context.load(
                &self.instance.plugin_handle(),
                &mut Cursor::new(state.component()),
                context_type,
            )
        } else {
            extension.load(
                &self.instance.plugin_handle(),
                &mut Cursor::new(state.component()),
            )
        }
        .map_err(|_| ClapError::State)
    }

    /// Clears the plugin's internal audio state with its native `reset`, an audio-thread
    /// function, keeping its settings.
    pub(crate) fn reset(&mut self) -> Result<(), ClapError> {
        let engine = Arc::clone(&self.engine);
        self.audio.run(move || {
            let mut engine = lock(&engine);
            // An inactive plugin holds no audio state to clear.
            let Some(processor) = engine.processor.as_mut() else {
                return Ok(());
            };
            {
                let _audit = plughost_core::audit::plugin();
                let _floats = crate::float_mode::FlushToZero::enter();
                in_audio_context(|| processor.reset());
            }
            if let Some(prepared) = &mut engine.prepared {
                // Reset ended the plugin's notes.
                prepared.sounding.clear();
            }
            Ok(())
        })
    }

    /// Opens the editor embedded in `parent` and returns its size.
    ///
    /// # Safety
    ///
    /// `parent` must be a valid native view that outlives the editor.
    pub(crate) unsafe fn open_editor(
        &mut self,
        parent: *mut c_void,
        resize: ResizeRequest,
    ) -> Result<EditorView, ClapError> {
        self.close_editor();
        let gui = self.shared().extensions().gui.ok_or(ClapError::NoEditor)?;
        let api = GuiApiType::default_for_current_platform().ok_or(ClapError::NoEditor)?;
        let handle = self.instance.plugin_handle();
        // An editor that cannot draw into the host's view may still open its own window.
        let configuration = [false, true]
            .map(|is_floating| GuiConfiguration {
                api_type: api,
                is_floating,
            })
            .into_iter()
            .find(|configuration| gui.is_api_supported(&handle, *configuration))
            .ok_or(ClapError::NoEditor)?;
        gui.create(&handle, configuration)
            .map_err(ClapError::Editor)?;
        if configuration.is_floating {
            if let Ok(title) = CString::new(self.info.name.as_str()) {
                gui.suggest_title(&handle, &title);
            }
            if let Err(error) = gui.show(&handle) {
                gui.destroy(&handle);
                return Err(ClapError::Editor(error));
            }
            // A floating editor sizes its own window.
            self.editor = Some(Box::new(|_, _| false));
            self.shared().editor_open.store(true, Ordering::Relaxed);
            return Ok(EditorView::Floating);
        }
        #[cfg(target_os = "macos")]
        let window = unsafe { Window::from_cocoa_nsview(parent) };
        #[cfg(target_os = "windows")]
        let window = unsafe { Window::from_win32_hwnd(parent) };
        let attached = unsafe { gui.set_parent(&handle, window) };
        if let Err(error) = attached {
            gui.destroy(&handle);
            return Err(ClapError::Editor(error));
        }
        let _ = gui.show(&handle);
        self.editor_size = gui.get_size(&handle).map(|size| (size.width, size.height));
        let (width, height) = self.editor_size.unwrap_or_default();
        self.editor = Some(resize);
        self.shared().editor_open.store(true, Ordering::Relaxed);
        self.shared().editor_embedded.store(true, Ordering::Relaxed);
        Ok(EditorView::Embedded { width, height })
    }

    pub(crate) fn set_editor_visible(&mut self, visible: bool) {
        if self.editor.is_none() {
            return;
        }
        if let Some(gui) = self.shared().extensions().gui {
            let handle = self.instance.plugin_handle();
            let _ = if visible {
                gui.show(&handle)
            } else {
                gui.hide(&handle)
            };
        }
    }

    pub(crate) fn take_editor_request(&mut self) -> Option<EditorRequest> {
        self.shared()
            .editor_request
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }

    pub(crate) fn close_editor(&mut self) {
        self.shared().editor_open.store(false, Ordering::Relaxed);
        self.shared()
            .editor_embedded
            .store(false, Ordering::Relaxed);
        if self.editor.take().is_some()
            && let Some(gui) = self.shared().extensions().gui
        {
            gui.destroy(&self.instance.plugin_handle());
        }
    }

    pub(crate) fn editor_can_resize(&mut self) -> bool {
        match (&self.editor, self.shared().extensions().gui) {
            (Some(_), Some(gui)) => gui.can_resize(&self.instance.plugin_handle()),
            _ => false,
        }
    }

    /// Applies the monitor scale when the Windows GUI supports it and returns its pixel size.
    #[cfg(target_os = "windows")]
    pub(crate) fn set_editor_scale(&mut self, scale: f64) -> Option<(u32, u32)> {
        self.editor.as_ref()?;
        let gui = self.shared().extensions().gui?;
        let handle = self.instance.plugin_handle();
        gui.set_scale(&handle, scale).ok()?;
        let size = gui.get_size(&handle)?;
        self.editor_size = Some((size.width, size.height));
        Some((size.width, size.height))
    }

    pub(crate) fn resize_editor(&mut self, width: u32, height: u32) -> Option<(u32, u32)> {
        self.editor.as_ref()?;
        let gui = self.shared().extensions().gui?;
        let handle = self.instance.plugin_handle();
        let size = gui
            .adjust_size(&handle, GuiSize { width, height })
            .unwrap_or(GuiSize { width, height });
        gui.set_size(&handle, size).ok()?;
        self.editor_size = Some((size.width, size.height));
        Some((size.width, size.height))
    }

    /// Serves what the plugin asked of the main thread: callbacks, timers, parameter flushes and
    /// editor resizes. A restart it asked for needs the plugin prepared again. Call it regularly
    /// from the owning thread.
    pub(crate) fn idle(&mut self) {
        let (callback, flush, timer, requested) = {
            let shared = self.shared();
            (
                shared.callback_requested.swap(false, Ordering::Relaxed),
                shared.flush_requested.load(Ordering::Relaxed),
                shared.extensions().timer,
                shared
                    .resize_requested
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .take(),
            )
        };
        if callback {
            self.instance.call_on_main_thread_callback();
        }
        if let Some(timer) = timer {
            let now = Instant::now();
            let due: Vec<_> = self.instance.access_handler(|main| {
                let mut timers = main.timers.borrow_mut();
                timers
                    .iter_mut()
                    .filter(|t| t.due <= now)
                    .map(|t| {
                        t.due = now + t.period;
                        t.id
                    })
                    .collect()
            });
            for id in due {
                // An earlier callback of this tick may have removed the timer.
                let registered = self
                    .instance
                    .access_handler(|main| main.timers.borrow().iter().any(|timer| timer.id == id));
                if registered {
                    timer.on_timer(&self.instance.plugin_handle(), id);
                }
            }
        }
        // An active plugin takes host edits with the next block. When blocks stopped coming,
        // edits that waited at least two of the longest blocks with none are flushed; blocks that
        // come, as realtime ones do, take them.
        let blocks = self.shared().blocks.load(Ordering::Relaxed);
        self.idle_edits = match self.idle_edits {
            _ if !self.shared().has_pending() => None,
            Some((seen, since)) if seen == blocks => Some((seen, since)),
            _ => Some((blocks, Instant::now())),
        };
        let stalled = self.idle_edits.is_some_and(|(_, since)| {
            let longest = lock(&self.engine).prepared.as_ref().map(|prepared| {
                prepared.config.max_block_size as f64 / prepared.config.sample_rate
            });
            longest.is_none_or(|longest| since.elapsed().as_secs_f64() >= 2.0 * longest)
        });
        if flush || stalled {
            self.flush();
        }
        if let (Some((width, height)), Some(resize)) = (requested, &self.editor) {
            if resize(width, height) {
                self.editor_size = Some((width, height));
            } else if let (Some(gui), Some((width, height))) =
                (self.shared().extensions().gui, self.editor_size)
            {
                // The plugin may already draw at the size it asked for.
                let _ = gui.set_size(&self.instance.plugin_handle(), GuiSize { width, height });
            }
        }
    }
}

impl Drop for Plugin {
    fn drop(&mut self) {
        self.close_editor();
        self.deactivate();
    }
}

/// The native speaker of each portable channel, for layouts CLAP's surround speakers express.
/// CLAP has no wide speakers for 9.1.6. Ls and Rs are CLAP's back speakers.
fn surround_order(layout: Layout) -> Option<&'static [SurroundChannel]> {
    use SurroundChannel::{
        BackLeft, BackRight, FrontCenter, FrontLeft, FrontRight, LowFrequency, SideLeft, SideRight,
        TopBackLeft, TopBackRight, TopFrontLeft, TopFrontRight, TopSideLeft, TopSideRight,
    };
    match layout {
        Layout::Lcr => Some(&[FrontLeft, FrontRight, FrontCenter]),
        Layout::Quad => Some(&[FrontLeft, FrontRight, BackLeft, BackRight]),
        Layout::Surround50 => Some(&[FrontLeft, FrontRight, FrontCenter, BackLeft, BackRight]),
        Layout::Surround51 => Some(&[
            FrontLeft,
            FrontRight,
            FrontCenter,
            LowFrequency,
            BackLeft,
            BackRight,
        ]),
        Layout::Surround70 => Some(&[
            FrontLeft,
            FrontRight,
            FrontCenter,
            BackLeft,
            BackRight,
            SideLeft,
            SideRight,
        ]),
        Layout::Surround71 => Some(&[
            FrontLeft,
            FrontRight,
            FrontCenter,
            LowFrequency,
            BackLeft,
            BackRight,
            SideLeft,
            SideRight,
        ]),
        Layout::Surround512 => Some(&[
            FrontLeft,
            FrontRight,
            FrontCenter,
            LowFrequency,
            BackLeft,
            BackRight,
            TopSideLeft,
            TopSideRight,
        ]),
        Layout::Surround514 => Some(&[
            FrontLeft,
            FrontRight,
            FrontCenter,
            LowFrequency,
            BackLeft,
            BackRight,
            TopFrontLeft,
            TopFrontRight,
            TopBackLeft,
            TopBackRight,
        ]),
        Layout::Surround712 => Some(&[
            FrontLeft,
            FrontRight,
            FrontCenter,
            LowFrequency,
            BackLeft,
            BackRight,
            SideLeft,
            SideRight,
            TopSideLeft,
            TopSideRight,
        ]),
        Layout::Surround714 => Some(&[
            FrontLeft,
            FrontRight,
            FrontCenter,
            LowFrequency,
            BackLeft,
            BackRight,
            SideLeft,
            SideRight,
            TopFrontLeft,
            TopFrontRight,
            TopBackLeft,
            TopBackRight,
        ]),
        _ => None,
    }
}

/// Ambisonic layouts exchange ACN-ordered, SN3D-normalized channels.
fn is_ambisonic(layout: Layout) -> bool {
    matches!(
        layout,
        Layout::Ambisonics1 | Layout::Ambisonics2 | Layout::Ambisonics3 | Layout::Ambisonics4
    )
}

/// Processes a CLAP plugin from a processing thread.
#[derive(Clone)]
pub struct Processor {
    engine: Arc<Mutex<Engine>>,
}

impl std::fmt::Debug for Processor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Processor")
            .field("engine", &Arc::as_ptr(&self.engine))
            .finish_non_exhaustive()
    }
}

impl Processor {
    /// Processes one block. `input` and `output` hold the main port channels after
    /// [`Plugin::prepare`], or every active port's channels in native order after
    /// [`Plugin::prepare_audio`]. `events` go to their requested note input ports: notes as CLAP
    /// note events when the port takes them, other messages (and notes on MIDI-only ports) as MIDI
    /// events when it takes MIDI; the rest are not delivered. `produced` receives the output
    /// events in offset order.
    pub(crate) fn process<S: plughost_core::Sample>(
        &self,
        context: &plughost_core::BlockContext,
        input: &[&[S]],
        output: &mut [&mut [S]],
        automation: &[plughost_core::ParameterChange],
        events: &plughost_core::Events,
        produced: &mut plughost_core::Events,
    ) -> Result<(), ClapError> {
        self.process_audio_impl(context, input, output, automation, events, produced)
    }
    pub(crate) fn defer_restart(&self, deferred: bool) {
        lock(&self.engine).restart_deferred = deferred;
    }

    pub(crate) fn restart_required(&self) -> bool {
        lock(&self.engine)
            .processor
            .as_ref()
            .is_some_and(|processor| {
                processor.access_shared_handler(|shared| {
                    shared.restart_requested.load(Ordering::Relaxed)
                        || shared.latency_changed.load(Ordering::Relaxed)
                })
            })
    }

    /// The latency read as the plugin activated and the tail last read; no call into the plugin.
    pub(crate) fn timing(&self) -> Result<PluginTiming, ClapError> {
        let engine = lock(&self.engine);
        match (&engine.processor, &engine.prepared) {
            (Some(processor), Some(prepared)) => Ok(timing(processor, prepared)),
            _ => Err(ClapError::NotPrepared),
        }
    }
}

/// A restart request stops processing until the plugin is prepared again, which is also how a
/// CLAP plugin changes its latency.
fn timing(processor: &PluginAudioProcessor<Host>, prepared: &Prepared) -> PluginTiming {
    let mut timing = PluginTiming::new(prepared.latency, prepared.tail);
    timing.restart_required = processor.access_shared_handler(|shared| {
        shared.restart_requested.load(Ordering::Relaxed)
            || shared.latency_changed.load(Ordering::Relaxed)
    });
    timing
}

/// The tail the plugin reports, read as its audio thread under the engine lock: as it activates,
/// after a block in which it reported a change, which CLAP does from the audio thread, and when
/// the owning thread reads the timing.
fn read_tail(processor: &mut PluginAudioProcessor<Host>) -> Tail {
    let Some(extension) = processor.access_shared_handler(|shared| shared.extensions().tail) else {
        return Tail::Samples(0);
    };
    let tail = {
        let _audit = plughost_core::audit::query();
        in_audio_context(|| extension.get(&processor.plugin_handle()))
    };
    if tail.is_infinite() {
        Tail::Infinite
    } else {
        Tail::Samples(tail.to_raw())
    }
}

fn state_context(
    purpose: plughost_core::StatePurpose,
) -> Result<clack_extensions::state_context::StateContextType, ClapError> {
    use clack_extensions::state_context::StateContextType;
    Ok(match purpose {
        plughost_core::StatePurpose::Project => StateContextType::ForProject,
        plughost_core::StatePurpose::Preset => StateContextType::ForPreset,
        plughost_core::StatePurpose::Duplicate => StateContextType::ForDuplicate,
        _ => return Err(ClapError::StatePurposeUnsupported),
    })
}

#[cfg(all(test, feature = "vst3"))]
mod layout_tests {
    use super::*;
    use vst3::Steinberg::Vst::*;

    /// The CLAP speaker of each VST3 speaker that has one.
    fn clap_speaker(speaker: Speaker) -> Option<SurroundChannel> {
        use SurroundChannel::*;
        [
            (kSpeakerL, FrontLeft),
            (kSpeakerR, FrontRight),
            (kSpeakerC, FrontCenter),
            (kSpeakerLfe, LowFrequency),
            (kSpeakerLs, BackLeft),
            (kSpeakerRs, BackRight),
            (kSpeakerSl, SideLeft),
            (kSpeakerSr, SideRight),
            (kSpeakerTfl, TopFrontLeft),
            (kSpeakerTfr, TopFrontRight),
            (kSpeakerTrl, TopBackLeft),
            (kSpeakerTrr, TopBackRight),
            (kSpeakerTsl, TopSideLeft),
            (kSpeakerTsr, TopSideRight),
        ]
        .into_iter()
        .find(|&(vst3, _)| vst3 == speaker)
        .map(|(_, clap)| clap)
    }

    #[test]
    fn surround_orders_are_the_speakers_of_the_vst3_arrangements() {
        let surround = Layout::ALL
            .into_iter()
            .filter(|&layout| layout.channels() > 2 && !is_ambisonic(layout));
        for layout in surround {
            let arrangement = crate::vst3::audio::arrangement(layout).unwrap();
            let speakers: Option<Vec<_>> = (0..64)
                .map(|bit| 1u64 << bit)
                .filter(|speaker| arrangement & speaker != 0)
                .map(clap_speaker)
                .collect();
            assert_eq!(surround_order(layout), speakers.as_deref(), "{layout:?}");
        }
    }
}
