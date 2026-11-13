use std::fmt;

use clack_extensions::gui::GuiError;
use clack_host::entry::PluginEntryError;
use clack_host::prelude::PluginInstanceError;
use plughost_core::{FailureKind, InputError, Layout, SampleFormat};

use crate::errors::{
    ACTIVATE, INPUT, NO_EDITOR, NOT_PREPARED, PARAMETER_CONVERSION, PARAMETER_RESULT, PROCESS,
    STATE_PURPOSE_UNSUPPORTED, state_too_large,
};
use plughost_core::messages::{
    OUTPUT_EVENT_CAPACITY, STATE_MISMATCH, UNCONVERTIBLE_OUTPUT_EVENTS, UNKNOWN_EVENT_PORT,
};

const AUDIO_METADATA: &str = "the CLAP plugin returned invalid audio bus metadata";
const AUDIO_UNSUPPORTED: &str = "the CLAP plugin does not support this audio bus operation";
const AUDIO_CONFIGURATION: &str = "the CLAP plugin refused the requested audio bus configuration";
const BYPASS_UNSUPPORTED: &str = "the CLAP plugin does not provide a writable bypass parameter";
const PRESET_UNSUPPORTED: &str = "the CLAP module does not support the requested preset operation";
const PRESET_DISCOVERY: &str = "the CLAP provider failed to discover presets";
const PRESET_METADATA: &str = "the CLAP provider returned malformed preset metadata";
pub(super) const PRESET_LOAD: &str = "the CLAP plugin failed to load the preset";
const PRESET_INPUT: &str = "the CLAP preset location, provider ID, or load key is invalid";
const NOTE_PORT_METADATA: &str = "the CLAP plugin returned invalid note port metadata";
const PRESET_LOCATION: &str = "the CLAP preset location could not be read";
const BAR_POSITION_REQUIRED: &str = "CLAP beat transport requires an explicit bar position because it has no separate bar-valid flag";
const RESTART_REQUIRED: &str = "the plugin requested explicit re-preparation";
const EVENT_STORAGE: &str = "could not reserve the prepared CLAP input events";
const LOAD: &str = "the CLAP module could not be loaded";
const NO_FACTORY: &str = "the CLAP module has no plugin factory";
const CLASS_NOT_FOUND: &str = "the CLAP module has no plugin with ID";
const INSTANTIATE: &str = "the CLAP plugin could not be created";
const SAMPLE_FORMAT_UNSUPPORTED: &str =
    "the CLAP audio ports do not support the requested sample format";
const LAYOUT_REFUSED: &str = "the plugin's main audio port does not have the requested layout";
const REALTIME_ONLY: &str = "the plugin cannot render offline (hard real-time requirement)";
const PROCESSING_STORAGE: &str = "could not reserve the prepared CLAP audio buffers";
const STATE: &str = "the plugin failed to save or load its state";
const STATE_UNSUPPORTED: &str = "the plugin does not provide the CLAP state extension";
const EDITOR: &str = "the plugin's editor failed to open";

const TOO_MANY_PARAMETERS: &str = "the CLAP plugin reported more parameters than are read";

/// The diagnostic for output events with no MIDI form in one block.
pub(crate) fn unconvertible_output_events(count: u64) -> String {
    format!("{UNCONVERTIBLE_OUTPUT_EVENTS} {count}")
}

/// The diagnostic for a parameter count past the `read` the host lists.
pub(crate) fn too_many_parameters(count: u32, read: u32) -> String {
    format!("{TOO_MANY_PARAMETERS} ({count}; the first {read} are listed)")
}

/// CLAP loading, extensions, processing, state, and editor failures.
#[derive(Debug)]
pub(crate) enum ClapError {
    /// Caller input violated a processing or control contract.
    Input(InputError),
    /// The CLAP plugin returned invalid audio bus metadata.
    AudioMetadata,
    /// The CLAP plugin does not support this audio bus operation.
    AudioUnsupported,
    /// The CLAP plugin refused the requested audio bus configuration.
    AudioConfiguration,
    /// The CLAP plugin does not provide a writable bypass parameter.
    BypassUnsupported,
    /// The CLAP module does not support the requested preset operation.
    PresetUnsupported,
    /// The CLAP provider failed to discover presets.
    PresetDiscovery,
    /// The CLAP provider returned malformed preset metadata.
    PresetMetadata,
    /// The CLAP plugin failed to load the preset.
    PresetLoad,
    /// The CLAP preset location, provider ID, or load key is invalid.
    PresetInput,
    /// The CLAP plugin returned invalid note port metadata.
    NotePortMetadata,
    /// The requested native event port does not exist or is not enabled.
    UnknownEventPort {
        /// Requested native event port ID.
        id: u64,
    },
    /// Native output exceeded the per-block event or system-exclusive budget; reset before processing again.
    OutputEventCapacity,
    /// The CLAP preset location could not be read.
    PresetLocation(std::io::Error),
    /// The plugin does not support the requested state context.
    StatePurposeUnsupported,
    /// The plugin does not support the requested parameter conversion.
    ParameterConversion,
    /// The plugin returned a non-finite or otherwise invalid parameter conversion.
    ParameterResult,
    /// The CLAP module could not be loaded.
    Load(PluginEntryError),
    /// The CLAP module has no plugin factory.
    NoFactory,
    /// The factory has no plugin with the supplied class ID.
    ClassNotFound(String),
    /// The CLAP plugin could not be created.
    Instantiate(PluginInstanceError),
    /// The CLAP audio ports do not support the requested sample format.
    SampleFormatUnsupported(SampleFormat),
    /// The plugin refused the requested channel layout.
    LayoutRefused {
        /// The layout the host requested.
        requested: Layout,
        /// The channel count the plugin exposed instead.
        plugin_channels: u32,
    },
    /// The plugin cannot render offline (hard real-time requirement).
    RealtimeOnly,
    /// The plugin failed to activate.
    Activate(PluginInstanceError),
    /// The plugin is not prepared for processing.
    NotPrepared,
    /// The plugin requested explicit re-preparation.
    RestartRequired,
    /// CLAP beat transport requires an explicit bar position because it has no separate bar-valid flag.
    BarPositionRequired,
    /// Could not reserve the prepared CLAP audio buffers.
    ProcessingStorage,
    /// Could not reserve the prepared CLAP input events.
    EventStorage,
    /// The plugin failed to start processing or its process call failed.
    Process(PluginInstanceError),
    /// The plugin failed to save or load its state.
    State,
    /// The encoded native state exceeds `MAX_STATE_BYTES`.
    StateTooLarge,
    /// The plugin does not provide the CLAP state extension.
    StateUnsupported,
    /// Saved state belongs to a different plugin format or class.
    StateMismatch,
    /// The plugin has no editor for this platform.
    NoEditor,
    /// The plugin's editor failed to open.
    Editor(GuiError),
}

impl fmt::Display for ClapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AudioMetadata => f.write_str(AUDIO_METADATA),
            Self::AudioUnsupported => f.write_str(AUDIO_UNSUPPORTED),
            Self::AudioConfiguration => f.write_str(AUDIO_CONFIGURATION),
            Self::BypassUnsupported => f.write_str(BYPASS_UNSUPPORTED),
            Self::PresetUnsupported => f.write_str(PRESET_UNSUPPORTED),
            Self::PresetDiscovery => f.write_str(PRESET_DISCOVERY),
            Self::PresetMetadata => f.write_str(PRESET_METADATA),
            Self::PresetLoad => f.write_str(PRESET_LOAD),
            Self::PresetInput => f.write_str(PRESET_INPUT),
            Self::NotePortMetadata => f.write_str(NOTE_PORT_METADATA),
            Self::UnknownEventPort { id } => write!(f, "{UNKNOWN_EVENT_PORT} ({id})"),
            Self::OutputEventCapacity => f.write_str(OUTPUT_EVENT_CAPACITY),
            Self::PresetLocation(_) => f.write_str(PRESET_LOCATION),
            ClapError::ParameterConversion => f.write_str(PARAMETER_CONVERSION),
            ClapError::ParameterResult => f.write_str(PARAMETER_RESULT),
            ClapError::StatePurposeUnsupported => f.write_str(STATE_PURPOSE_UNSUPPORTED),
            ClapError::Input(_) => f.write_str(INPUT),
            ClapError::Load(_) => f.write_str(LOAD),
            ClapError::NoFactory => f.write_str(NO_FACTORY),
            ClapError::ClassNotFound(id) => write!(f, "{CLASS_NOT_FOUND} {id}"),
            ClapError::Instantiate(_) => f.write_str(INSTANTIATE),
            ClapError::SampleFormatUnsupported(format) => {
                write!(f, "{SAMPLE_FORMAT_UNSUPPORTED} ({format:?})")
            }
            ClapError::LayoutRefused {
                requested,
                plugin_channels,
            } => write!(
                f,
                "{LAYOUT_REFUSED} ({requested:?}; the port has {plugin_channels} channels)"
            ),
            ClapError::RealtimeOnly => f.write_str(REALTIME_ONLY),
            ClapError::Activate(_) => f.write_str(ACTIVATE),
            ClapError::BarPositionRequired => f.write_str(BAR_POSITION_REQUIRED),
            ClapError::RestartRequired => f.write_str(RESTART_REQUIRED),
            ClapError::NotPrepared => f.write_str(NOT_PREPARED),
            ClapError::EventStorage => f.write_str(EVENT_STORAGE),
            ClapError::ProcessingStorage => f.write_str(PROCESSING_STORAGE),
            ClapError::Process(_) => f.write_str(PROCESS),
            ClapError::State => f.write_str(STATE),
            ClapError::StateTooLarge => state_too_large(f),
            ClapError::StateUnsupported => f.write_str(STATE_UNSUPPORTED),
            ClapError::StateMismatch => f.write_str(STATE_MISMATCH),
            ClapError::NoEditor => f.write_str(NO_EDITOR),
            ClapError::Editor(_) => f.write_str(EDITOR),
        }
    }
}

impl std::error::Error for ClapError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Input(error) => Some(error),
            // The entry error's display already includes its own cause; skip that level so the
            // chain names each cause once.
            Self::Load(error) => Some(error.source().unwrap_or(error)),
            Self::Instantiate(error) | Self::Activate(error) | Self::Process(error) => Some(error),
            Self::Editor(error) => Some(error),
            Self::PresetLocation(error) => Some(error),
            _ => None,
        }
    }
}

impl ClapError {
    /// Classifies this failure for scan policy and helper diagnostics.
    pub(crate) fn kind(&self) -> FailureKind {
        match self {
            Self::AudioMetadata => FailureKind::Host,
            Self::AudioUnsupported | Self::BypassUnsupported => FailureKind::Unsupported,
            Self::AudioConfiguration | Self::ProcessingStorage | Self::EventStorage => {
                FailureKind::Configuration
            }
            Self::PresetUnsupported => FailureKind::Unsupported,
            Self::PresetDiscovery | Self::PresetMetadata | Self::PresetLoad => FailureKind::State,
            Self::PresetInput | Self::UnknownEventPort { .. } => FailureKind::InvalidInput,
            Self::NotePortMetadata => FailureKind::Host,
            Self::OutputEventCapacity => FailureKind::Processing,
            Self::PresetLocation(error) if error.kind() == std::io::ErrorKind::NotFound => {
                FailureKind::NotFound
            }
            Self::PresetLocation(_) => FailureKind::Host,
            Self::StatePurposeUnsupported => FailureKind::Unsupported,
            Self::ParameterConversion => FailureKind::Unsupported,
            Self::ParameterResult => FailureKind::Host,
            Self::Input(_) => FailureKind::InvalidInput,
            Self::ClassNotFound(_) => FailureKind::NotFound,
            Self::SampleFormatUnsupported(_)
            | Self::RealtimeOnly
            | Self::StateUnsupported
            | Self::NoEditor => FailureKind::Unsupported,
            Self::Load(_) | Self::NoFactory | Self::Instantiate(_) => FailureKind::Load,
            Self::LayoutRefused { .. } | Self::Activate(_) => FailureKind::Configuration,
            Self::BarPositionRequired => FailureKind::Unsupported,
            Self::RestartRequired => FailureKind::RestartRequired,
            Self::NotPrepared => FailureKind::NotPrepared,
            Self::StateMismatch => FailureKind::StateMismatch,
            Self::State | Self::StateTooLarge => FailureKind::State,
            Self::Process(_) => FailureKind::Processing,
            Self::Editor(_) => FailureKind::Editor,
        }
    }
}

pub(super) const PRESET_DISCOVERY_FAILED: &str = "preset discovery failed";
pub(super) const GUI_REQUEST_WITHOUT_EDITOR: &str =
    "the plugin asked to show or hide an editor that is not open";
pub(super) const GUI_RESIZE_WITHOUT_EMBEDDED_EDITOR: &str =
    "the plugin asked to resize an editor that is not open in the host's window";

pub(super) const PRESET_LOADED: &str = "the CLAP plugin reported a loaded preset";
pub(super) const TIMER_OFF_MAIN_THREAD: &str =
    "the CLAP plugin registered or unregistered a timer off the main thread, which was refused";
