//! The operations every format provides, so the helper can host a chain of mixed formats. Audio
//! retains the explicitly selected sample precision across this interface.

use crate::audio_thread::AudioThread;
use crate::errors::{Error, Repr};
use std::ffi::c_void;
use std::sync::Arc;

use plughost_core::Capabilities;
use plughost_core::PluginRef;
use plughost_core::{
    HostIdentity, ParameterInfo, PluginInfo, PluginState, PluginTiming, ProcessConfig,
};

#[cfg(all(feature = "au", target_os = "macos"))]
use crate::au;
#[cfg(feature = "clap")]
use crate::clap;
#[cfg(feature = "vst3")]
use crate::vst3;

/// Called when a plugin asks for a new editor size; returns whether the host window now has that
/// size.
pub type ResizeRequest = Box<dyn Fn(u32, u32) -> bool>;

/// Where an opened editor draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum EditorView {
    /// In the host's parent view, at this size.
    Embedded {
        /// Width in the native view's coordinate system.
        width: u32,
        /// Height in the native view's coordinate system.
        height: u32,
    },
    /// In a window of the plugin's own; the host's parent view stays unused.
    Floating,
}

/// What a plugin asked of its editor window. A later request replaces one not yet served.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum EditorRequest {
    /// Open the editor, or bring it to the front when it is open.
    Open,
    /// Show the open editor again.
    Show,
    /// Hide the open editor, keeping it open.
    Hide,
    /// The editor closed its own window; the host detaches it.
    Closed,
}

mod sealed {
    /// Implemented by the format plugins and processors of this crate only.
    pub trait Sealed {}
    #[cfg(feature = "vst3")]
    impl Sealed for crate::vst3::Plugin {}
    #[cfg(feature = "vst3")]
    impl Sealed for crate::vst3::Processor {}
    #[cfg(all(feature = "au", target_os = "macos"))]
    impl Sealed for crate::au::Plugin {}
    #[cfg(all(feature = "au", target_os = "macos"))]
    impl Sealed for crate::au::Processor {}
    #[cfg(feature = "clap")]
    impl Sealed for crate::clap::Plugin {}
    #[cfg(feature = "clap")]
    impl Sealed for crate::clap::Processor {}
}

/// A plugin instance, used on the thread that created it. It is sealed: the format plugins of
/// this crate implement it.
pub trait HostedPlugin: sealed::Sealed {
    /// Drains plugin-authored log messages; formats without a native log callback return empty.
    fn take_diagnostics(&self) -> plughost_core::DiagnosticBatch {
        plughost_core::DiagnosticBatch::default()
    }
    /// Drains parameter values, gestures, rescans, and dirty-state notifications.
    fn take_parameter_events(&mut self) -> plughost_core::ParameterEventBatch;
    /// Lists native factory presets or programs. The default reports unsupported.
    fn factory_presets(&mut self) -> Result<Vec<plughost_core::FactoryPreset>, Error> {
        Err(Error::new(Repr::FactoryPresetsUnsupported))
    }
    /// Selects a native factory preset or program. The default reports unsupported.
    fn select_factory_preset(
        &mut self,
        _preset: &plughost_core::FactoryPresetId,
    ) -> Result<(), Error> {
        Err(Error::new(Repr::FactoryPresetsUnsupported))
    }
    /// Loads a provider-discovered preset by native location and optional load key.
    fn load_discovered_preset(
        &mut self,
        _location: &plughost_core::PresetLocation,
        _load_key: Option<&str>,
    ) -> Result<(), Error> {
        Err(Error::new(Repr::PresetDiscoveryUnsupported))
    }
    /// Reads native audio bus metadata in input-then-output order, retaining native indices.
    fn audio_buses(&mut self) -> Result<Vec<plughost_core::AudioBusInfo>, Error>;
    /// Native event ports in both directions, inputs first.
    fn event_ports(&mut self) -> Result<Vec<plughost_core::EventPortInfo>, Error>;
    /// Lists named native bus configurations when the format supports them. A CLAP plugin that
    /// has configurations but cannot describe their ports (`audio-ports-config` without
    /// `audio-ports-config-info`) fails with an unsupported-audio error.
    fn audio_configurations(&mut self) -> Result<Vec<plughost_core::AudioConfiguration>, Error>;
    /// Applies explicit audio and event bus configuration and returns the resulting bus metadata.
    fn prepare_audio(
        &mut self,
        config: &plughost_core::AudioConfig,
    ) -> Result<Vec<plughost_core::AudioBusInfo>, Error>;
    /// Queries native bypass support and its current state.
    fn bypass(&mut self) -> Result<plughost_core::BypassState, Error>;
    /// Requests native bypass; it does not implement a host-side dry signal path.
    fn set_bypass(&mut self, enabled: bool) -> Result<(), Error>;
    /// Metadata identifying this loaded plugin instance.
    fn info(&self) -> &PluginInfo;
    /// Queries the instance for currently available native features.
    fn capabilities(&mut self) -> Result<Capabilities, Error>;
    /// Prepares the legacy main-bus processing configuration.
    fn prepare(&mut self, config: &ProcessConfig) -> Result<(), Error>;
    /// A handle that processes this plugin from another thread.
    fn processor(&self) -> Box<dyn BlockProcessor>;
    /// Makes the audio-thread calls this plugin needs between blocks on `thread`, the thread
    /// that runs its [`BlockProcessor`] (see [`AudioThread`]).
    fn set_audio_thread(&mut self, _thread: Arc<dyn AudioThread>) {}
    /// Reads the prepared plugin's timing on this thread, which is what its processor reports
    /// until the next read: the tail the plugin reports now, and the latency it was activated
    /// with, with a change it announced pending until it is prepared again or reset.
    fn timing(&self) -> Result<PluginTiming, Error>;
    /// Opportunistically reads timing on the owning thread. `None` means unavailable or not
    /// implemented. Native hosts do not wait for their processing lock or flush pending edits;
    /// use [`Self::timing`] when a fresh read or an error is required. CLAP reads native tail
    /// only after a change notification; VST3 and AU query it on each successful poll.
    fn poll_timing(&self) -> Option<PluginTiming> {
        None
    }
    /// Each parameter with its current normalized value, clamped to `0..=1`, or `None` when the
    /// plugin does not give one, or gives one or a range that is not finite.
    fn parameters(&mut self) -> Vec<(ParameterInfo, Option<f64>)>;
    /// Reads native units, grouping, flags, and value-range metadata for a parameter.
    fn parameter_details(&mut self, id: u64) -> Result<plughost_core::ParameterDetails, Error>;
    /// Reads a bounded page of native labels and values; does not edit the parameter.
    fn parameter_choices(
        &mut self,
        id: u64,
        start: u64,
        count: u32,
    ) -> Result<plughost_core::ParameterChoicePage, Error> {
        crate::parameter_choices::read(self, id, start, count)
    }
    /// Changes a parameter the way an edit in the plugin's editor does.
    fn set_parameter(&mut self, id: u64, value: f64) -> Result<(), Error>;
    /// Formats a normalized value using the plugin's native display conversion.
    fn parameter_text(&mut self, id: u64, value: f64) -> Result<String, Error>;
    /// Parses native display text to a normalized value without editing the parameter.
    fn parameter_from_text(&mut self, id: u64, text: &str) -> Result<f64, Error>;
    /// Converts a normalized parameter value to native units.
    fn parameter_to_plain(&mut self, id: u64, value: f64) -> Result<f64, Error>;
    /// Converts native parameter units to a normalized value.
    fn parameter_to_normalized(&mut self, id: u64, plain: f64) -> Result<f64, Error>;
    /// Saves the state, after pending edits reach the processor.
    fn save_state(&mut self, purpose: plughost_core::StatePurpose) -> Result<PluginState, Error>;
    /// Restores opaque native state for this plugin identity and the requested context.
    fn restore_state(
        &mut self,
        state: &PluginState,
        purpose: plughost_core::StatePurpose,
    ) -> Result<(), Error>;
    /// Clears internal audio state (tails, delay lines), keeping settings.
    fn reset(&mut self) -> Result<(), Error>;
    /// Opens the editor in `parent` (`NSView` on macOS, `HWND` on Windows), or in its own window
    /// when the plugin draws only there.
    ///
    /// # Safety
    ///
    /// `parent` must be a valid native view that outlives the editor.
    unsafe fn open_editor(
        &mut self,
        parent: *mut c_void,
        resize: ResizeRequest,
    ) -> Result<EditorView, Error>;
    /// Closes and detaches this instance's editor, if open.
    fn close_editor(&mut self);
    /// Tells the open editor that its window was shown or hidden.
    fn set_editor_visible(&mut self, _visible: bool) {}
    /// The plugin's latest editor request since the last call.
    #[must_use]
    fn take_editor_request(&mut self) -> Option<EditorRequest> {
        None
    }
    /// Whether the editor accepts host-requested resizing. The default is false.
    fn editor_can_resize(&mut self) -> bool {
        false
    }
    /// Whether the editor and the panels it opens run in an app extension's process rather than
    /// this one, as those of an AUv3 unit loaded out of process do. The default is false.
    fn editor_in_extension(&self) -> bool {
        false
    }
    /// Offers the size the user dragged the editor window to; returns the size the editor took,
    /// or `None` when the editor keeps its own size.
    fn resize_editor(&mut self, _width: u32, _height: u32) -> Option<(u32, u32)> {
        None
    }
    /// Sends a Windows monitor scale to an editor that supports explicit scaling, returning
    /// its resulting size in physical pixels. Other editors keep their own size, and on other
    /// platforms it does nothing and returns `None`.
    fn set_editor_scale(&mut self, scale: f64) -> Option<(u32, u32)> {
        let _ = scale;
        None
    }
    /// Serves requests the plugin made of the owning thread (callbacks, timers). Call it
    /// regularly from that thread's event loop.
    fn idle(&mut self) {}
}

/// Processes a plugin from a processing thread. It is sealed: the format processors of this crate
/// implement it.
pub trait BlockProcessor: sealed::Sealed + Send {
    /// Processes one block in the prepared precision. `events` are in offset order, inside the
    /// block, on active native event input ports by index. `produced` is replaced with the
    /// plugin's output events in offset order, on native event output ports by index; pass
    /// `Events::for_block()` to process without allocating for them. A plugin that
    /// produces more events or system exclusive bytes than one block allows fails the block,
    /// which the caller resolves with a reset.
    fn process_audio_f32(
        &self,
        context: &plughost_core::BlockContext,
        input: &[&[f32]],
        output: &mut [&mut [f32]],
        automation: &[plughost_core::ParameterChange],
        events: &plughost_core::Events,
        produced: &mut plughost_core::Events,
    ) -> Result<(), Error>;
    /// Processes f64 audio with the event, automation, and capacity contracts of [`Self::process_audio_f32`].
    fn process_audio_f64(
        &self,
        context: &plughost_core::BlockContext,
        input: &[&[f64]],
        output: &mut [&mut [f64]],
        automation: &[plughost_core::ParameterChange],
        events: &plughost_core::Events,
        produced: &mut plughost_core::Events,
    ) -> Result<(), Error>;
    /// The timing the owning thread last read (see [`HostedPlugin::timing`]), without calling the
    /// plugin.
    fn timing(&self) -> Result<PluginTiming, Error>;
    /// Whether a native restart request prevents further processing until re-preparation.
    fn restart_required(&self) -> bool {
        false
    }
    /// Lets blocks go on with the current activation while a VST3 or CLAP restart request waits,
    /// as a realtime stream needs, instead of failing with `FailureKind::RestartRequired`. The
    /// request stays reported by [`Self::restart_required`] and the timing until the owning
    /// thread prepares the plugin again. Off until enabled; formats without such requests ignore
    /// it.
    fn defer_restart(&self, deferred: bool) {
        let _ = deferred;
    }
}

/// Loads the plugin class `plugin` names, in this process.
///
/// # Safety
///
/// Loading runs the plugin's own code, in this process unless it is an Audio Unit macOS runs in
/// its extension process, with everything the process may do, and nothing checks that code: a
/// plugin that is not sound can corrupt or end the process. Load only plugins trusted to behave,
/// or load them in a process of their own, as `plughost`'s helper does.
pub unsafe fn load(
    plugin: &PluginRef,
    host: &HostIdentity,
) -> Result<Box<dyn HostedPlugin>, Error> {
    host.validate()?;
    // A VST3 or CLAP reference always has a bundle and an Audio Unit one none.
    match (plugin.id().format(), plugin.bundle()) {
        #[cfg(feature = "vst3")]
        (plughost_core::PluginFormat::Vst3, Some(bundle)) => {
            // SAFETY: the caller accepts running the plugin's code.
            let module = unsafe { vst3::Module::load_with_host(bundle, host) }?;
            let class_id = plugin.id().class_id().as_str();
            Ok(Box::new(vst3::Plugin::new(&module, class_id, host)?))
        }
        #[cfg(all(feature = "au", target_os = "macos"))]
        (plughost_core::PluginFormat::AudioUnit, _) => {
            // SAFETY: the caller accepts running the plugin's code.
            let plugin = unsafe { au::Plugin::new(plugin.id().class_id().as_str()) }?;
            Ok(Box::new(plugin))
        }
        #[cfg(feature = "clap")]
        (plughost_core::PluginFormat::Clap, Some(bundle)) => {
            // SAFETY: the caller accepts running the plugin's code.
            let entry = unsafe { clap::Entry::load(bundle) }?;
            let class_id = plugin.id().class_id().as_str();
            Ok(Box::new(clap::Plugin::new(&entry, class_id, host)?))
        }
        (format, _) => Err(Error::new(Repr::UnsupportedFormat(format))),
    }
}

#[cfg(feature = "vst3")]
impl HostedPlugin for vst3::Plugin {
    fn take_diagnostics(&self) -> plughost_core::DiagnosticBatch {
        vst3::Plugin::take_diagnostics(self)
    }
    fn audio_buses(&mut self) -> Result<Vec<plughost_core::AudioBusInfo>, Error> {
        Ok(vst3::Plugin::audio_buses(self)?)
    }
    fn event_ports(&mut self) -> Result<Vec<plughost_core::EventPortInfo>, Error> {
        Ok(vst3::Plugin::event_ports(self)?)
    }
    fn audio_configurations(&mut self) -> Result<Vec<plughost_core::AudioConfiguration>, Error> {
        Ok(vst3::Plugin::audio_configurations(self)?)
    }
    fn prepare_audio(
        &mut self,
        config: &plughost_core::AudioConfig,
    ) -> Result<Vec<plughost_core::AudioBusInfo>, Error> {
        Ok(vst3::Plugin::prepare_audio(self, config)?)
    }
    fn bypass(&mut self) -> Result<plughost_core::BypassState, Error> {
        Ok(vst3::Plugin::bypass(self)?)
    }
    fn set_bypass(&mut self, enabled: bool) -> Result<(), Error> {
        Ok(vst3::Plugin::set_bypass(self, enabled)?)
    }

    fn take_parameter_events(&mut self) -> plughost_core::ParameterEventBatch {
        vst3::Plugin::take_parameter_events(self)
    }
    fn factory_presets(&mut self) -> Result<Vec<plughost_core::FactoryPreset>, Error> {
        Ok(vst3::Plugin::factory_presets(self)?)
    }
    fn select_factory_preset(
        &mut self,
        preset: &plughost_core::FactoryPresetId,
    ) -> Result<(), Error> {
        Ok(vst3::Plugin::select_factory_preset(self, preset)?)
    }

    fn capabilities(&mut self) -> Result<Capabilities, Error> {
        Ok(vst3::Plugin::capabilities(self)?)
    }
    fn info(&self) -> &PluginInfo {
        vst3::Plugin::info(self)
    }

    fn prepare(&mut self, config: &ProcessConfig) -> Result<(), Error> {
        Ok(vst3::Plugin::prepare(self, config)?)
    }

    fn processor(&self) -> Box<dyn BlockProcessor> {
        Box::new(vst3::Plugin::processor(self))
    }

    fn set_audio_thread(&mut self, thread: Arc<dyn AudioThread>) {
        vst3::Plugin::set_audio_thread(self, thread);
    }

    fn timing(&self) -> Result<PluginTiming, Error> {
        Ok(vst3::Plugin::timing(self)?)
    }
    fn poll_timing(&self) -> Option<PluginTiming> {
        vst3::Plugin::poll_timing(self)
    }

    fn parameters(&mut self) -> Vec<(ParameterInfo, Option<f64>)> {
        vst3::Plugin::parameters(self)
            .into_iter()
            .map(|info| {
                let value = self.parameter_value(info.id);
                (info, value)
            })
            .collect()
    }

    fn parameter_details(&mut self, id: u64) -> Result<plughost_core::ParameterDetails, Error> {
        Ok(vst3::Plugin::parameter_details(self, id)?)
    }

    fn set_parameter(&mut self, id: u64, value: f64) -> Result<(), Error> {
        vst3::Plugin::set_parameter(self, id, value).map_err(Error::from)
    }

    fn parameter_text(&mut self, id: u64, value: f64) -> Result<String, Error> {
        Ok(vst3::Plugin::parameter_text(self, id, value)?)
    }

    fn parameter_from_text(&mut self, id: u64, text: &str) -> Result<f64, Error> {
        Ok(vst3::Plugin::parameter_from_text(self, id, text)?)
    }

    fn parameter_to_plain(&mut self, id: u64, value: f64) -> Result<f64, Error> {
        Ok(vst3::Plugin::parameter_to_plain(self, id, value)?)
    }

    fn parameter_to_normalized(&mut self, id: u64, plain: f64) -> Result<f64, Error> {
        Ok(vst3::Plugin::parameter_to_normalized(self, id, plain)?)
    }

    fn save_state(&mut self, purpose: plughost_core::StatePurpose) -> Result<PluginState, Error> {
        Ok(vst3::Plugin::save_state(self, purpose)?)
    }
    fn restore_state(
        &mut self,
        state: &PluginState,
        purpose: plughost_core::StatePurpose,
    ) -> Result<(), Error> {
        Ok(vst3::Plugin::restore_state(self, state, purpose)?)
    }

    fn reset(&mut self) -> Result<(), Error> {
        Ok(vst3::Plugin::reset(self)?)
    }

    unsafe fn open_editor(
        &mut self,
        parent: *mut c_void,
        resize: ResizeRequest,
    ) -> Result<EditorView, Error> {
        let (width, height) = unsafe { vst3::Plugin::open_editor(self, parent, resize)? };
        Ok(EditorView::Embedded { width, height })
    }

    fn close_editor(&mut self) {
        vst3::Plugin::close_editor(self);
    }

    fn take_editor_request(&mut self) -> Option<EditorRequest> {
        vst3::Plugin::take_editor_request(self)
    }

    fn editor_can_resize(&mut self) -> bool {
        vst3::Plugin::editor_can_resize(self)
    }

    fn resize_editor(&mut self, width: u32, height: u32) -> Option<(u32, u32)> {
        vst3::Plugin::resize_editor(self, width, height)
    }

    #[cfg(target_os = "windows")]
    fn set_editor_scale(&mut self, scale: f64) -> Option<(u32, u32)> {
        vst3::Plugin::set_editor_scale(self, scale)
    }

    fn idle(&mut self) {
        vst3::Plugin::idle(self);
    }
}

#[cfg(feature = "vst3")]
impl BlockProcessor for vst3::Processor {
    fn process_audio_f32(
        &self,
        context: &plughost_core::BlockContext,
        input: &[&[f32]],
        output: &mut [&mut [f32]],
        automation: &[plughost_core::ParameterChange],
        events: &plughost_core::Events,
        produced: &mut plughost_core::Events,
    ) -> Result<(), Error> {
        Ok(vst3::Processor::process(
            self, context, input, output, automation, events, produced,
        )?)
    }

    fn process_audio_f64(
        &self,
        context: &plughost_core::BlockContext,
        input: &[&[f64]],
        output: &mut [&mut [f64]],
        automation: &[plughost_core::ParameterChange],
        events: &plughost_core::Events,
        produced: &mut plughost_core::Events,
    ) -> Result<(), Error> {
        Ok(vst3::Processor::process(
            self, context, input, output, automation, events, produced,
        )?)
    }

    fn timing(&self) -> Result<PluginTiming, Error> {
        Ok(vst3::Processor::timing(self)?)
    }
    fn restart_required(&self) -> bool {
        vst3::Processor::restart_required(self)
    }
    fn defer_restart(&self, deferred: bool) {
        vst3::Processor::defer_restart(self, deferred);
    }
}

#[cfg(all(feature = "au", target_os = "macos"))]
impl HostedPlugin for au::Plugin {
    fn take_diagnostics(&self) -> plughost_core::DiagnosticBatch {
        au::Plugin::take_diagnostics(self)
    }
    fn audio_buses(&mut self) -> Result<Vec<plughost_core::AudioBusInfo>, Error> {
        Ok(au::Plugin::audio_buses(self)?)
    }
    fn event_ports(&mut self) -> Result<Vec<plughost_core::EventPortInfo>, Error> {
        Ok(au::Plugin::event_ports(self)?)
    }
    fn audio_configurations(&mut self) -> Result<Vec<plughost_core::AudioConfiguration>, Error> {
        Ok(au::Plugin::audio_configurations(self)?)
    }
    fn prepare_audio(
        &mut self,
        config: &plughost_core::AudioConfig,
    ) -> Result<Vec<plughost_core::AudioBusInfo>, Error> {
        Ok(au::Plugin::prepare_audio(self, config)?)
    }
    fn bypass(&mut self) -> Result<plughost_core::BypassState, Error> {
        Ok(au::Plugin::bypass(self)?)
    }
    fn set_bypass(&mut self, enabled: bool) -> Result<(), Error> {
        Ok(au::Plugin::set_bypass(self, enabled)?)
    }

    fn take_parameter_events(&mut self) -> plughost_core::ParameterEventBatch {
        au::Plugin::take_parameter_events(self)
    }
    fn factory_presets(&mut self) -> Result<Vec<plughost_core::FactoryPreset>, Error> {
        Ok(au::Plugin::factory_presets(self)?)
    }
    fn select_factory_preset(
        &mut self,
        preset: &plughost_core::FactoryPresetId,
    ) -> Result<(), Error> {
        Ok(au::Plugin::select_factory_preset(self, preset)?)
    }

    fn capabilities(&mut self) -> Result<Capabilities, Error> {
        Ok(au::Plugin::capabilities(self)?)
    }
    fn info(&self) -> &PluginInfo {
        au::Plugin::info(self)
    }

    fn prepare(&mut self, config: &ProcessConfig) -> Result<(), Error> {
        Ok(au::Plugin::prepare(self, config)?)
    }

    fn processor(&self) -> Box<dyn BlockProcessor> {
        Box::new(au::Plugin::processor(self))
    }

    fn timing(&self) -> Result<PluginTiming, Error> {
        Ok(au::Plugin::timing(self)?)
    }
    fn poll_timing(&self) -> Option<PluginTiming> {
        au::Plugin::poll_timing(self)
    }

    fn parameters(&mut self) -> Vec<(ParameterInfo, Option<f64>)> {
        au::Plugin::parameters(self)
    }

    fn parameter_details(&mut self, id: u64) -> Result<plughost_core::ParameterDetails, Error> {
        Ok(au::Plugin::parameter_details(self, id)?)
    }

    fn set_parameter(&mut self, id: u64, value: f64) -> Result<(), Error> {
        au::Plugin::set_parameter(self, id, value).map_err(Error::from)
    }

    fn parameter_text(&mut self, id: u64, value: f64) -> Result<String, Error> {
        Ok(au::Plugin::parameter_text(self, id, value)?)
    }

    fn parameter_from_text(&mut self, id: u64, text: &str) -> Result<f64, Error> {
        Ok(au::Plugin::parameter_from_text(self, id, text)?)
    }

    fn parameter_to_plain(&mut self, id: u64, value: f64) -> Result<f64, Error> {
        Ok(au::Plugin::parameter_to_plain(self, id, value)?)
    }

    fn parameter_to_normalized(&mut self, id: u64, plain: f64) -> Result<f64, Error> {
        Ok(au::Plugin::parameter_to_normalized(self, id, plain)?)
    }

    fn save_state(&mut self, purpose: plughost_core::StatePurpose) -> Result<PluginState, Error> {
        Ok(au::Plugin::save_state(self, purpose)?)
    }
    fn restore_state(
        &mut self,
        state: &PluginState,
        purpose: plughost_core::StatePurpose,
    ) -> Result<(), Error> {
        Ok(au::Plugin::restore_state(self, state, purpose)?)
    }

    fn reset(&mut self) -> Result<(), Error> {
        Ok(au::Plugin::reset(self)?)
    }

    unsafe fn open_editor(
        &mut self,
        parent: *mut c_void,
        resize: ResizeRequest,
    ) -> Result<EditorView, Error> {
        let (width, height) = unsafe { au::Plugin::open_editor(self, parent, resize)? };
        Ok(EditorView::Embedded { width, height })
    }

    fn close_editor(&mut self) {
        au::Plugin::close_editor(self);
    }

    fn editor_in_extension(&self) -> bool {
        au::Plugin::editor_in_extension(self)
    }

    fn idle(&mut self) {
        au::Plugin::idle(self);
    }
}

#[cfg(all(feature = "au", target_os = "macos"))]
impl BlockProcessor for au::Processor {
    fn restart_required(&self) -> bool {
        au::Processor::restart_required(self)
    }
    fn process_audio_f32(
        &self,
        context: &plughost_core::BlockContext,
        input: &[&[f32]],
        output: &mut [&mut [f32]],
        automation: &[plughost_core::ParameterChange],
        events: &plughost_core::Events,
        produced: &mut plughost_core::Events,
    ) -> Result<(), Error> {
        Ok(au::Processor::process(
            self, context, input, output, automation, events, produced,
        )?)
    }

    fn process_audio_f64(
        &self,
        context: &plughost_core::BlockContext,
        input: &[&[f64]],
        output: &mut [&mut [f64]],
        automation: &[plughost_core::ParameterChange],
        events: &plughost_core::Events,
        produced: &mut plughost_core::Events,
    ) -> Result<(), Error> {
        let _ = (context, input, output, automation, events, produced);
        Err(au::AuError::SampleFormatUnsupported(plughost_core::SampleFormat::F64).into())
    }

    fn timing(&self) -> Result<PluginTiming, Error> {
        Ok(au::Processor::timing(self)?)
    }
}

#[cfg(feature = "clap")]
impl HostedPlugin for clap::Plugin {
    fn audio_buses(&mut self) -> Result<Vec<plughost_core::AudioBusInfo>, Error> {
        Ok(clap::Plugin::audio_buses(self)?)
    }
    fn event_ports(&mut self) -> Result<Vec<plughost_core::EventPortInfo>, Error> {
        Ok(clap::Plugin::event_ports(self)?)
    }
    fn audio_configurations(&mut self) -> Result<Vec<plughost_core::AudioConfiguration>, Error> {
        Ok(clap::Plugin::audio_configurations(self)?)
    }
    fn prepare_audio(
        &mut self,
        config: &plughost_core::AudioConfig,
    ) -> Result<Vec<plughost_core::AudioBusInfo>, Error> {
        Ok(clap::Plugin::prepare_audio(self, config)?)
    }
    fn bypass(&mut self) -> Result<plughost_core::BypassState, Error> {
        Ok(clap::Plugin::bypass(self)?)
    }
    fn set_bypass(&mut self, enabled: bool) -> Result<(), Error> {
        Ok(clap::Plugin::set_bypass(self, enabled)?)
    }

    fn take_parameter_events(&mut self) -> plughost_core::ParameterEventBatch {
        clap::Plugin::take_parameter_events(self)
    }
    fn load_discovered_preset(
        &mut self,
        location: &plughost_core::PresetLocation,
        load_key: Option<&str>,
    ) -> Result<(), Error> {
        Ok(clap::Plugin::load_discovered_preset(
            self, location, load_key,
        )?)
    }

    fn take_diagnostics(&self) -> plughost_core::DiagnosticBatch {
        clap::Plugin::take_diagnostics(self)
    }
    fn capabilities(&mut self) -> Result<Capabilities, Error> {
        Ok(clap::Plugin::capabilities(self))
    }
    fn info(&self) -> &PluginInfo {
        clap::Plugin::info(self)
    }

    fn prepare(&mut self, config: &ProcessConfig) -> Result<(), Error> {
        Ok(clap::Plugin::prepare(self, config)?)
    }

    fn processor(&self) -> Box<dyn BlockProcessor> {
        Box::new(clap::Plugin::processor(self))
    }

    fn set_audio_thread(&mut self, thread: Arc<dyn AudioThread>) {
        clap::Plugin::set_audio_thread(self, thread);
    }

    fn timing(&self) -> Result<PluginTiming, Error> {
        Ok(clap::Plugin::timing(self)?)
    }
    fn poll_timing(&self) -> Option<PluginTiming> {
        clap::Plugin::poll_timing(self)
    }

    fn parameters(&mut self) -> Vec<(ParameterInfo, Option<f64>)> {
        clap::Plugin::parameters(self)
    }

    fn parameter_choices(
        &mut self,
        id: u64,
        start: u64,
        count: u32,
    ) -> Result<plughost_core::ParameterChoicePage, Error> {
        clap::Plugin::parameter_choices(self, id, start, count)
    }

    fn parameter_details(&mut self, id: u64) -> Result<plughost_core::ParameterDetails, Error> {
        Ok(clap::Plugin::parameter_details(self, id)?)
    }

    fn set_parameter(&mut self, id: u64, value: f64) -> Result<(), Error> {
        clap::Plugin::set_parameter(self, id, value).map_err(Error::from)
    }

    fn parameter_text(&mut self, id: u64, value: f64) -> Result<String, Error> {
        Ok(clap::Plugin::parameter_text(self, id, value)?)
    }

    fn parameter_from_text(&mut self, id: u64, text: &str) -> Result<f64, Error> {
        Ok(clap::Plugin::parameter_from_text(self, id, text)?)
    }

    fn parameter_to_plain(&mut self, id: u64, value: f64) -> Result<f64, Error> {
        Ok(clap::Plugin::parameter_to_plain(self, id, value)?)
    }

    fn parameter_to_normalized(&mut self, id: u64, plain: f64) -> Result<f64, Error> {
        Ok(clap::Plugin::parameter_to_normalized(self, id, plain)?)
    }

    fn save_state(&mut self, purpose: plughost_core::StatePurpose) -> Result<PluginState, Error> {
        Ok(clap::Plugin::save_state(self, purpose)?)
    }
    fn restore_state(
        &mut self,
        state: &PluginState,
        purpose: plughost_core::StatePurpose,
    ) -> Result<(), Error> {
        Ok(clap::Plugin::restore_state(self, state, purpose)?)
    }

    fn reset(&mut self) -> Result<(), Error> {
        Ok(clap::Plugin::reset(self)?)
    }

    unsafe fn open_editor(
        &mut self,
        parent: *mut c_void,
        resize: ResizeRequest,
    ) -> Result<EditorView, Error> {
        Ok(unsafe { clap::Plugin::open_editor(self, parent, resize)? })
    }

    fn close_editor(&mut self) {
        clap::Plugin::close_editor(self);
    }

    fn set_editor_visible(&mut self, visible: bool) {
        clap::Plugin::set_editor_visible(self, visible);
    }

    fn take_editor_request(&mut self) -> Option<EditorRequest> {
        clap::Plugin::take_editor_request(self)
    }

    fn editor_can_resize(&mut self) -> bool {
        clap::Plugin::editor_can_resize(self)
    }

    fn resize_editor(&mut self, width: u32, height: u32) -> Option<(u32, u32)> {
        clap::Plugin::resize_editor(self, width, height)
    }

    #[cfg(target_os = "windows")]
    fn set_editor_scale(&mut self, scale: f64) -> Option<(u32, u32)> {
        clap::Plugin::set_editor_scale(self, scale)
    }

    fn idle(&mut self) {
        clap::Plugin::idle(self);
    }
}

#[cfg(feature = "clap")]
impl BlockProcessor for clap::Processor {
    fn process_audio_f32(
        &self,
        context: &plughost_core::BlockContext,
        input: &[&[f32]],
        output: &mut [&mut [f32]],
        automation: &[plughost_core::ParameterChange],
        events: &plughost_core::Events,
        produced: &mut plughost_core::Events,
    ) -> Result<(), Error> {
        Ok(clap::Processor::process(
            self, context, input, output, automation, events, produced,
        )?)
    }

    fn process_audio_f64(
        &self,
        context: &plughost_core::BlockContext,
        input: &[&[f64]],
        output: &mut [&mut [f64]],
        automation: &[plughost_core::ParameterChange],
        events: &plughost_core::Events,
        produced: &mut plughost_core::Events,
    ) -> Result<(), Error> {
        Ok(clap::Processor::process(
            self, context, input, output, automation, events, produced,
        )?)
    }

    fn timing(&self) -> Result<PluginTiming, Error> {
        Ok(clap::Processor::timing(self)?)
    }
    fn restart_required(&self) -> bool {
        clap::Processor::restart_required(self)
    }
    fn defer_restart(&self, deferred: bool) {
        clap::Processor::defer_restart(self, deferred);
    }
}
