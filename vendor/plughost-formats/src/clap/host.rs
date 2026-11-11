//! The host side of a CLAP instance: the callbacks the plugin can make, and the plugin extensions
//! the host uses. Requests that must be served on the main thread are recorded here and served by
//! `Plugin::idle`.

use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

use clack_extensions::ambisonic::{HostAmbisonic, HostAmbisonicImpl, PluginAmbisonic};
use clack_extensions::audio_ports::{
    AudioPortRescanFlags, HostAudioPorts, HostAudioPortsImpl, PluginAudioPorts,
};
use clack_extensions::audio_ports_activation::PluginAudioPortsActivation;
use clack_extensions::audio_ports_config::{
    HostAudioPortsConfig, HostAudioPortsConfigImpl, PluginAudioPortsConfig,
    PluginAudioPortsConfigInfo,
};
use clack_extensions::configurable_audio_ports::PluginConfigurableAudioPorts;
use clack_extensions::gui::{GuiSize, HostGui, HostGuiImpl, PluginGui};
use clack_extensions::latency::{HostLatency, HostLatencyImpl, PluginLatency};
use clack_extensions::log::{HostLog, HostLogImpl, LogSeverity};
use clack_extensions::note_ports::{
    HostNotePorts, HostNotePortsImpl, NoteDialects, NotePortRescanFlags, PluginNotePorts,
};
use clack_extensions::params::{
    HostParams, HostParamsImplMainThread, HostParamsImplShared, ParamClearFlags, ParamRescanFlags,
    PluginParams,
};
use clack_extensions::preset_discovery::{HostPresetLoad, PluginPresetLoad};
use clack_extensions::render::PluginRender;
mod presets;
use clack_extensions::state::{HostState, HostStateImpl, PluginState};
use clack_extensions::state_context::PluginStateContext;
use clack_extensions::surround::{HostSurround, HostSurroundImpl, PluginSurround};
use clack_extensions::tail::{HostTail, HostTailImpl, PluginTail};
use clack_extensions::thread_check::{HostThreadCheck, HostThreadCheckImpl};
use clack_extensions::timer::{HostTimer, HostTimerImpl, PluginTimer, TimerId};
use clack_host::prelude::*;
use plughost_core::internal::{DiagnosticBuffer, ParameterEventBuffer};
use plughost_core::{DiagnosticSeverity, ParameterEvent, PluginId};

thread_local! {
    /// Set while this thread calls into the plugin's audio processor under the engine lock, which
    /// makes it the plugin's audio thread for that call.
    pub static AUDIO_CONTEXT: Cell<bool> = const { Cell::new(false) };
}

/// Runs `call` as the plugin's audio thread.
pub fn in_audio_context<R>(call: impl FnOnce() -> R) -> R {
    AUDIO_CONTEXT.with(|flag| flag.set(true));
    let result = call();
    AUDIO_CONTEXT.with(|flag| flag.set(false));
    result
}

pub struct Host;

impl HostHandlers for Host {
    type Shared<'a> = Shared;
    type MainThread<'a> = MainThread<'a>;
    type AudioProcessor<'a> = AudioThread<'a>;

    fn declare_extensions(builder: &mut HostExtensions<Self>, _shared: &Shared) {
        builder
            .register::<HostAmbisonic>()
            .register::<HostAudioPorts>()
            .register::<HostAudioPortsConfig>()
            .register::<HostGui>()
            .register::<HostLatency>()
            .register::<HostTail>()
            .register::<HostLog>()
            .register::<HostNotePorts>()
            .register::<HostParams>()
            .register::<HostPresetLoad>()
            .register::<HostState>()
            .register::<HostSurround>()
            .register::<HostThreadCheck>()
            .register::<HostTimer>();
    }
}

/// Plugin extensions, queried once while the plugin initializes.
#[derive(Default)]
pub struct Extensions {
    pub preset_load: Option<PluginPresetLoad>,
    pub audio_ports: Option<PluginAudioPorts>,
    pub audio_config: Option<PluginAudioPortsConfig>,
    pub audio_config_info: Option<PluginAudioPortsConfigInfo>,
    pub audio_activation: Option<PluginAudioPortsActivation>,
    pub ambisonic: Option<PluginAmbisonic>,
    pub configurable_audio: Option<PluginConfigurableAudioPorts>,
    pub gui: Option<PluginGui>,
    pub latency: Option<PluginLatency>,
    pub note_ports: Option<PluginNotePorts>,
    pub params: Option<PluginParams>,
    pub render: Option<PluginRender>,
    pub state: Option<PluginState>,
    pub state_context: Option<PluginStateContext>,
    pub surround: Option<PluginSurround>,
    pub tail: Option<PluginTail>,
    pub timer: Option<PluginTimer>,
}

pub struct Shared {
    pub parameter_events: ParameterEventBuffer,
    pub preset_load_failed: AtomicBool,
    diagnostics: DiagnosticBuffer,
    plugin: PluginId,
    main_thread: ThreadId,
    pub extensions: OnceLock<Extensions>,
    pub restart_requested: AtomicBool,
    pub audio_ports_reset: AtomicBool,
    pub latency_changed: AtomicBool,
    /// The plugin reported a changed tail from its audio thread, which reads it after the block.
    pub tail_changed: AtomicBool,
    pub callback_requested: AtomicBool,
    pub flush_requested: AtomicBool,
    /// Counts the blocks that processed, which take the queued host edits.
    pub blocks: AtomicU64,
    /// The plugin rescanned its parameter info; the cached parameter list is stale.
    pub parameters_changed: AtomicBool,
    /// An editor size the plugin asked for, applied on the main thread.
    pub resize_requested: Mutex<Option<(u32, u32)>>,
    /// The plugin's latest request to show, hide or close its editor.
    pub editor_request: Mutex<Option<crate::EditorRequest>>,
    /// Whether the host has the plugin's editor open, which visibility requests need.
    pub editor_open: AtomicBool,
    /// Whether that editor draws in the host's window, which resize requests need.
    pub editor_embedded: AtomicBool,
    /// Host edits not yet delivered to the plugin, as `(parameter ID, plain value)`.
    pub pending: Mutex<Vec<(u32, f64)>>,
    /// How often a load dropped the pending edits; changed and read only under `pending`'s lock.
    pending_dropped: AtomicU64,
}

impl Shared {
    pub fn new(diagnostics: DiagnosticBuffer, plugin: PluginId) -> Shared {
        Shared {
            parameter_events: ParameterEventBuffer::default(),
            preset_load_failed: AtomicBool::new(false),
            diagnostics,
            plugin,
            main_thread: std::thread::current().id(),
            extensions: OnceLock::new(),
            restart_requested: AtomicBool::new(false),
            audio_ports_reset: AtomicBool::new(false),
            latency_changed: AtomicBool::new(false),
            tail_changed: AtomicBool::new(false),
            callback_requested: AtomicBool::new(false),
            flush_requested: AtomicBool::new(false),
            blocks: AtomicU64::new(0),
            parameters_changed: AtomicBool::new(true),
            resize_requested: Mutex::new(None),
            editor_request: Mutex::new(None),
            editor_open: AtomicBool::new(false),
            editor_embedded: AtomicBool::new(false),
            pending: Mutex::new(Vec::with_capacity(plughost_core::MAX_BLOCK_EVENTS)),
            pending_dropped: AtomicU64::new(0),
        }
    }

    pub fn extensions(&self) -> &Extensions {
        self.extensions.get_or_init(Extensions::default)
    }

    pub fn has_pending(&self) -> bool {
        !self
            .pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_empty()
    }

    /// Control-path snapshot; draining retains the producer's prepared capacity.
    pub fn take_pending(&self) -> Vec<(u32, f64)> {
        let mut pending = self.pending.lock().unwrap_or_else(|p| p.into_inner());
        self.pending_dropped.fetch_add(1, Ordering::Relaxed);
        pending.drain(..).collect()
    }

    /// Processing sends the edits without taking them, so that a block that fails leaves them for
    /// the next one: the values are absolute, so one the plugin already has may reach it again.
    /// Returns what it sent, for [`Shared::forget_delivered`].
    pub fn copy_pending(&self, mut send: impl FnMut(u32, f64)) -> Sent {
        let pending = self.pending.lock().unwrap_or_else(|p| p.into_inner());
        for &(id, value) in pending.iter() {
            send(id, value);
        }
        Sent {
            count: pending.len(),
            dropped: self.pending_dropped.load(Ordering::Relaxed),
        }
    }

    /// Forgets the edits a block delivered, keeping those queued since and the producer's
    /// storage. A state or preset load on the owning thread that dropped the queue meanwhile
    /// took them already, and what is queued now came after it.
    pub fn forget_delivered(&self, sent: Sent) {
        let mut pending = self.pending.lock().unwrap_or_else(|p| p.into_inner());
        if self.pending_dropped.load(Ordering::Relaxed) == sent.dropped {
            let count = sent.count.min(pending.len());
            pending.drain(..count);
        }
    }
}

/// The host edits a block sent, from [`Shared::copy_pending`].
#[derive(Clone, Copy)]
pub struct Sent {
    count: usize,
    dropped: u64,
}

impl<'a> SharedHandler<'a> for Shared {
    fn initializing(&self, instance: InitializingPluginHandle<'a>) {
        let _ = self.extensions.set(Extensions {
            preset_load: instance.get_extension(),
            audio_ports: instance.get_extension(),
            audio_config: instance.get_extension(),
            audio_config_info: instance.get_extension(),
            audio_activation: instance.get_extension(),
            ambisonic: instance.get_extension(),
            configurable_audio: instance.get_extension(),
            gui: instance.get_extension(),
            latency: instance.get_extension(),
            note_ports: instance.get_extension(),
            params: instance.get_extension(),
            render: instance.get_extension(),
            state: instance.get_extension(),
            state_context: instance.get_extension(),
            surround: instance.get_extension(),
            tail: instance.get_extension(),
            timer: instance.get_extension(),
        });
    }

    fn request_restart(&self) {
        let _audit = plughost_core::audit::callback();
        self.restart_requested.store(true, Ordering::Relaxed);
    }

    fn request_process(&self) {
        let _audit = plughost_core::audit::callback();
    }

    fn request_callback(&self) {
        let _audit = plughost_core::audit::callback();
        self.callback_requested.store(true, Ordering::Relaxed);
    }
}

impl Shared {
    /// Reports, from the processing thread, output events that have no MIDI 1.0 form.
    pub fn unconvertible_output_events(&self, count: u64) {
        self.diagnostics.record(
            DiagnosticSeverity::Warning,
            &super::errors::unconvertible_output_events(count),
            Some(&self.plugin),
            None,
            None,
        );
    }

    /// Reports something the plugin did against CLAP's rules, which the host did not follow.
    pub fn plugin_misbehaving(&self, message: &str) {
        self.diagnostics.record(
            DiagnosticSeverity::PluginMisbehaving,
            message,
            Some(&self.plugin),
            None,
            None,
        );
    }
}

impl HostLogImpl for Shared {
    fn log(&self, severity: LogSeverity, message: &str) {
        let _audit = plughost_core::audit::callback();
        let severity = match severity {
            LogSeverity::Debug => DiagnosticSeverity::Debug,
            LogSeverity::Info => DiagnosticSeverity::Info,
            LogSeverity::Warning => DiagnosticSeverity::Warning,
            LogSeverity::Error => DiagnosticSeverity::Error,
            LogSeverity::Fatal => DiagnosticSeverity::Fatal,
            LogSeverity::HostMisbehaving => DiagnosticSeverity::HostMisbehaving,
            LogSeverity::PluginMisbehaving => DiagnosticSeverity::PluginMisbehaving,
        };
        self.diagnostics
            .record(severity, message, Some(&self.plugin), None, None);
    }
}

impl HostThreadCheckImpl for Shared {
    fn is_main_thread(&self) -> bool {
        let _audit = plughost_core::audit::callback();
        std::thread::current().id() == self.main_thread
    }

    fn is_audio_thread(&self) -> bool {
        let _audit = plughost_core::audit::callback();
        AUDIO_CONTEXT.with(Cell::get)
    }
}

impl HostParamsImplShared for Shared {
    fn request_flush(&self) {
        let _audit = plughost_core::audit::callback();
        self.flush_requested.store(true, Ordering::Relaxed);
    }
}

impl Shared {
    fn request_visibility(&self, request: crate::EditorRequest) -> Result<(), HostError> {
        if !self.editor_open.load(Ordering::Relaxed) {
            return Err(HostError::Message(
                super::errors::GUI_REQUEST_WITHOUT_EDITOR,
            ));
        }
        self.request_editor(request);
        Ok(())
    }

    fn request_editor(&self, request: crate::EditorRequest) {
        *self
            .editor_request
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(request);
    }
}

impl HostGuiImpl for Shared {
    fn resize_hints_changed(&self) {}

    /// Accepts a size for the main thread to give the host window; a size the window cannot take
    /// is reverted there. A floating editor sizes its own window.
    fn request_resize(&self, size: GuiSize) -> Result<(), HostError> {
        if !self.editor_embedded.load(Ordering::Relaxed) {
            return Err(HostError::Message(
                super::errors::GUI_RESIZE_WITHOUT_EMBEDDED_EDITOR,
            ));
        }
        *self
            .resize_requested
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some((size.width, size.height));
        Ok(())
    }

    fn request_show(&self) -> Result<(), HostError> {
        self.request_visibility(crate::EditorRequest::Show)
    }

    fn request_hide(&self) -> Result<(), HostError> {
        self.request_visibility(crate::EditorRequest::Hide)
    }

    /// A destroyed GUI is acknowledged with destroy when the host closes the editor; one only
    /// hidden by its window closing is destroyed there too, since the host no longer shows it.
    fn closed(&self, _was_destroyed: bool) {
        self.request_editor(crate::EditorRequest::Closed);
    }
}

pub struct Timer {
    pub id: TimerId,
    pub period: Duration,
    pub due: Instant,
}

pub struct MainThread<'a> {
    pub shared: &'a Shared,
    pub timers: RefCell<Vec<Timer>>,
    next_timer: Cell<u32>,
}

impl<'a> MainThread<'a> {
    pub fn new(shared: &'a Shared) -> MainThread<'a> {
        MainThread {
            shared,
            timers: RefCell::new(Vec::new()),
            next_timer: Cell::new(1),
        }
    }
}

impl<'a> MainThreadHandler<'a> for MainThread<'a> {}

impl MainThread<'_> {
    /// Refuses and reports a timer call made off the main thread, before it touches the timer
    /// list, which only the main thread may use. CLAP allows the calls on the main thread only.
    fn timer_call(&self) -> Result<(), HostError> {
        if std::thread::current().id() == self.shared.main_thread {
            return Ok(());
        }
        self.shared
            .plugin_misbehaving(super::errors::TIMER_OFF_MAIN_THREAD);
        Err(HostError::Message(super::errors::TIMER_OFF_MAIN_THREAD))
    }
}

impl HostTimerImpl for MainThread<'_> {
    fn register_timer(&self, period_ms: u32) -> Result<TimerId, HostError> {
        self.timer_call()?;
        let id = TimerId(self.next_timer.get());
        self.next_timer.set(id.0 + 1);
        let period = Duration::from_millis(u64::from(period_ms.max(1)));
        self.timers.borrow_mut().push(Timer {
            id,
            period,
            due: Instant::now() + period,
        });
        Ok(id)
    }

    fn unregister_timer(&self, timer_id: TimerId) -> Result<(), HostError> {
        self.timer_call()?;
        self.timers
            .borrow_mut()
            .retain(|timer| timer.id != timer_id);
        Ok(())
    }
}

impl HostLatencyImpl for MainThread<'_> {
    fn changed(&self) {
        self.shared.latency_changed.store(true, Ordering::Relaxed);
    }
}

impl HostParamsImplMainThread for MainThread<'_> {
    fn rescan(&self, flags: ParamRescanFlags) {
        if flags.intersects(ParamRescanFlags::ALL | ParamRescanFlags::INFO) {
            self.shared
                .parameters_changed
                .store(true, Ordering::Relaxed);
        }
        if flags.intersects(ParamRescanFlags::ALL | ParamRescanFlags::INFO | ParamRescanFlags::TEXT)
        {
            self.shared
                .parameter_events
                .record(ParameterEvent::MetadataChanged);
        }
        if flags.intersects(ParamRescanFlags::ALL | ParamRescanFlags::VALUES) {
            self.shared
                .parameter_events
                .record(ParameterEvent::ValuesChanged);
        }
    }

    fn clear(&self, _param_id: ClapId, _flags: ParamClearFlags) {}
}

impl HostStateImpl for MainThread<'_> {
    fn mark_dirty(&self) {
        self.shared
            .parameter_events
            .record(ParameterEvent::Dirty { dirty: true });
    }
}

impl HostNotePortsImpl for MainThread<'_> {
    fn supported_dialects(&self) -> NoteDialects {
        NoteDialects::CLAP | NoteDialects::MIDI
    }

    /// Port names are read when needed; only a changed port list needs the plugin prepared again.
    fn rescan(&self, flags: NotePortRescanFlags) {
        if flags.contains(NotePortRescanFlags::ALL) {
            self.shared.restart_requested.store(true, Ordering::Relaxed);
        }
    }
}

impl HostSurroundImpl for MainThread<'_> {
    fn changed(&self) {
        self.shared.restart_requested.store(true, Ordering::Relaxed);
    }
}

impl HostAmbisonicImpl for MainThread<'_> {
    fn changed(&self) {
        self.shared.restart_requested.store(true, Ordering::Relaxed);
    }
}

impl HostAudioPortsImpl for MainThread<'_> {
    fn is_rescan_flag_supported(&self, flag: AudioPortRescanFlags) -> bool {
        AudioPortRescanFlags::all().contains(flag)
    }

    /// Port names are read when needed, and a plugin may change them while active. Every other
    /// change needs the plugin prepared again.
    fn rescan(&self, flags: AudioPortRescanFlags) {
        if flags.contains(AudioPortRescanFlags::LIST) {
            self.shared.audio_ports_reset.store(true, Ordering::Relaxed);
        }
        if !flags.difference(AudioPortRescanFlags::NAMES).is_empty() {
            self.shared.restart_requested.store(true, Ordering::Relaxed);
        }
    }
}

pub struct AudioThread<'a> {
    pub shared: &'a Shared,
}
impl<'a> AudioProcessorHandler<'a> for AudioThread<'a> {}
impl HostTailImpl for AudioThread<'_> {
    fn changed(&mut self) {
        let _audit = plughost_core::audit::callback();
        self.shared.tail_changed.store(true, Ordering::Relaxed);
    }
}

impl HostAudioPortsConfigImpl for MainThread<'_> {
    fn rescan(&self) {
        self.shared.restart_requested.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plughost_core::PluginFormat;

    fn shared() -> Shared {
        Shared::new(
            DiagnosticBuffer::default(),
            PluginId::new(PluginFormat::Clap, ""),
        )
    }

    #[test]
    fn only_an_editor_in_the_host_window_may_ask_for_a_size() {
        let shared = shared();
        let size = GuiSize {
            width: 300,
            height: 150,
        };
        assert!(HostGuiImpl::request_resize(&shared, size).is_err());
        shared.editor_open.store(true, Ordering::Relaxed);
        assert!(HostGuiImpl::request_resize(&shared, size).is_err());
        assert_eq!(*shared.resize_requested.lock().unwrap(), None);
        shared.editor_embedded.store(true, Ordering::Relaxed);
        assert!(HostGuiImpl::request_resize(&shared, size).is_ok());
        assert_eq!(*shared.resize_requested.lock().unwrap(), Some((300, 150)));
    }

    #[test]
    fn renamed_ports_do_not_require_a_restart() {
        let shared = shared();
        let main = MainThread::new(&shared);
        HostAudioPortsImpl::rescan(&main, AudioPortRescanFlags::NAMES);
        HostNotePortsImpl::rescan(&main, NotePortRescanFlags::NAMES);
        assert!(!shared.restart_requested.load(Ordering::Relaxed));
        HostAudioPortsImpl::rescan(&main, AudioPortRescanFlags::CHANNEL_COUNT);
        assert!(shared.restart_requested.load(Ordering::Relaxed));
        shared.restart_requested.store(false, Ordering::Relaxed);
        HostNotePortsImpl::rescan(&main, NotePortRescanFlags::ALL);
        assert!(shared.restart_requested.load(Ordering::Relaxed));
    }
}
