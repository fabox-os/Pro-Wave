use std::fmt;

use plughost_core::{FailureKind, InputError, Layout, SampleFormat};

use crate::errors::{
    ACTIVATE, CLOSED, INPUT, NO_EDITOR, NOT_PREPARED, PARAMETER_CONVERSION, PARAMETER_RESULT,
    PROCESS, STATE_PURPOSE_UNSUPPORTED, state_too_large,
};
use plughost_core::messages::{
    MAIN_BUS, OUTPUT_EVENT_CAPACITY, STATE_MISMATCH, UNCONVERTIBLE_OUTPUT_EVENTS,
    UNKNOWN_EVENT_PORT,
};

const AUDIO_BUS_METADATA: &str = "the plugin returned invalid audio bus metadata";
const AUDIO_CONFIGURATIONS_UNSUPPORTED: &str = "VST3 does not enumerate named audio configurations";
const UNKNOWN_AUDIO_BUS: &str = "the plugin has no requested audio bus";
const AUDIO_BUS_ACTIVATION: &str = "the plugin refused activation for an audio bus";
const BYPASS_UNSUPPORTED: &str = "the plugin has no writable native bypass parameter";
const BYPASS_EDIT: &str = "the plugin rejected the bypass change";
const FACTORY_PRESETS_UNSUPPORTED: &str =
    "the plugin does not support native factory program selection";
const INVALID_FACTORY_PRESET: &str = "the factory program does not belong to this plugin";
const FACTORY_PRESET_METADATA: &str = "the plugin returned invalid factory program metadata";
const FACTORY_PRESET_SELECT: &str = "the plugin rejected the factory program";
const EVENT_STORAGE: &str = "could not reserve the prepared VST3 event storage";
#[cfg(target_os = "macos")]
const BUNDLE_OPEN: &str = "could not open the VST3 bundle";
const BUNDLE_LOAD: &str = "could not load the VST3 module binary";
const NO_ENTRY_POINT: &str = "the module does not export";
const ENTRY_FAILED: &str = "the module's entry function failed";
const NO_FACTORY: &str = "GetPluginFactory returned no factory";
const CLASS_NOT_FOUND: &str = "the module has no audio module class with ID";
const CREATE_COMPONENT: &str = "the factory could not create the component";
const INITIALIZE_COMPONENT: &str = "the component failed to initialize";
const NO_AUDIO_PROCESSOR: &str = "the component does not implement IAudioProcessor";
const NO_CONTROLLER_CLASS: &str = "the component names no edit controller class";
const CREATE_CONTROLLER: &str = "the factory could not create the edit controller";
const INITIALIZE_CONTROLLER: &str = "the edit controller failed to initialize";
const SAMPLE_FORMAT_UNSUPPORTED: &str = "the plugin cannot process samples of format";
const PROCESS_MODE_UNSUPPORTED: &str = "VST3 has no processing mode for";
const LAYOUT_REFUSED: &str = "the plugin refused the requested layout";
const SETUP_REJECTED: &str =
    "the plugin rejected the processing setup (sample rate, block size, sample format, or mode)";
const PROCESSING_STORAGE: &str = "could not reserve the prepared VST3 audio buffers";
const RESTART_REQUIRED: &str =
    "the plugin changed its buses or asked to be reloaded; prepare it again";
const GET_STATE: &str = "the plugin failed to save its state";
const SET_STATE: &str = "the plugin failed to restore its state";
const PRESET: &str = "not a valid .vstpreset file";
const EDITOR_ATTACH: &str = "the plugin's editor failed to attach";
const PARAMETER_GROUPS: &str = "the plugin returned an invalid parameter group hierarchy";

/// The diagnostic for output events with no MIDI form since the last report.
pub(crate) fn unconvertible_output_events(count: u64) -> String {
    format!("{UNCONVERTIBLE_OUTPUT_EVENTS} {count}")
}

/// VST3 loading, lifecycle, processing, state, and editor failures.
#[derive(Debug)]
pub(crate) enum Vst3Error {
    /// The plugin returned invalid audio bus metadata.
    AudioBusMetadata,
    /// VST3 does not enumerate named audio configurations.
    AudioConfigurationsUnsupported,
    /// The requested native audio bus does not exist.
    UnknownAudioBus {
        /// Requested native audio bus ID.
        id: u64,
    },
    /// The plugin rejected activation or deactivation of an audio bus.
    AudioBusActivation {
        /// Requested native audio bus ID.
        id: u64,
        /// The native result code returned by the plugin.
        code: i32,
    },
    /// The plugin has no writable native bypass parameter.
    BypassUnsupported,
    /// The plugin rejected the bypass change. The payload is a native result code.
    BypassEdit(i32),
    /// The plugin does not support native factory program selection.
    FactoryPresetsUnsupported,
    /// The factory program does not belong to this plugin.
    InvalidFactoryPreset,
    /// The plugin returned invalid factory program metadata.
    FactoryPresetMetadata,
    /// The plugin rejected the factory program. The payload is a native result code.
    FactoryPresetSelect(i32),
    /// The plugin returned an invalid parameter group hierarchy.
    ParameterGroups,
    /// Caller input violated a processing or control contract.
    Input(InputError),
    /// The plugin does not support the requested state context.
    StatePurposeUnsupported,
    /// The plugin does not support the requested parameter conversion.
    ParameterConversion,
    /// The plugin returned a non-finite or otherwise invalid parameter conversion.
    ParameterResult,
    /// Could not open the VST3 bundle.
    #[cfg(target_os = "macos")]
    BundleOpen,
    /// Could not load the VST3 module binary, with the loader's error where it gives one.
    BundleLoad(Option<Box<dyn std::error::Error + Send + Sync>>),
    /// The module does not export the named required entry point.
    NoEntryPoint(&'static str),
    /// The module's entry function failed.
    EntryFailed,
    /// GetPluginFactory returned no factory.
    NoFactory,
    /// The factory has no plugin with the supplied class ID.
    ClassNotFound(String),
    /// The factory could not create the component.
    CreateComponent,
    /// The component failed to initialize. The payload is a native result code.
    InitializeComponent(i32),
    /// The component does not implement IAudioProcessor.
    NoAudioProcessor,
    /// The component names no edit controller class.
    NoControllerClass,
    /// The factory could not create the edit controller.
    CreateController,
    /// The edit controller failed to initialize. The payload is a native result code.
    InitializeController(i32),
    /// The plugin cannot process samples in the supplied format.
    SampleFormatUnsupported(SampleFormat),
    /// VST3 has no processing mode for the requested one.
    ProcessModeUnsupported(plughost_core::ProcessMode),
    /// No main audio bus can provide the requested layout.
    NoBus(Layout),
    /// The plugin refused the requested channel layout.
    LayoutRefused {
        /// The layout the host requested.
        requested: Layout,
        /// The channel count the plugin exposed instead.
        plugin_channels: usize,
    },
    /// The plugin rejected the processing setup (sample rate, block size, sample format, or mode). The payload is a native result code.
    SetupRejected(i32),
    /// The plugin failed to activate. The payload is a native result code.
    Activate(i32),
    /// The plugin is not prepared for processing.
    NotPrepared,
    /// Could not reserve the prepared VST3 audio buffers.
    ProcessingStorage,
    /// Could not reserve the prepared VST3 event storage.
    EventStorage,
    /// Native output exceeded the per-block event or system-exclusive budget; reset before processing again.
    OutputEventCapacity,
    /// The requested native event port does not exist or is not enabled.
    UnknownEventPort {
        /// Requested native event port ID.
        id: u64,
    },
    /// The plugin's process call failed. The payload is a native result code.
    Process(i32),
    /// The plugin changed its buses or asked to be reloaded; prepare it again. The payload contains native restart flags.
    RestartRequired(i32),
    /// The plugin failed to save its state. The payload is a native result code.
    GetState(i32),
    /// Aggregate native processor and controller state exceeds `MAX_STATE_BYTES`.
    StateTooLarge,
    /// The plugin failed to restore its state. The payload is a native result code.
    SetState(i32),
    /// Saved state belongs to a different plugin format or class.
    StateMismatch,
    /// Not a valid .vstpreset file.
    Preset,
    /// The owning plugin handle has been dropped; its processor is no longer usable.
    Closed,
    /// The plugin has no editor for this platform.
    NoEditor,
    /// The plugin's editor failed to attach. The payload is a native result code.
    EditorAttach(i32),
}

impl fmt::Display for Vst3Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AudioBusMetadata => f.write_str(AUDIO_BUS_METADATA),
            Self::AudioConfigurationsUnsupported => f.write_str(AUDIO_CONFIGURATIONS_UNSUPPORTED),
            Self::UnknownAudioBus { id } => write!(f, "{UNKNOWN_AUDIO_BUS} ({id})"),
            Self::AudioBusActivation { id, code } => {
                write!(f, "{AUDIO_BUS_ACTIVATION} (bus {id}, tresult {code})")
            }
            Self::BypassUnsupported => f.write_str(BYPASS_UNSUPPORTED),
            Self::BypassEdit(code) => write!(f, "{BYPASS_EDIT} (tresult {code})"),
            Self::FactoryPresetsUnsupported => f.write_str(FACTORY_PRESETS_UNSUPPORTED),
            Self::InvalidFactoryPreset => f.write_str(INVALID_FACTORY_PRESET),
            Self::FactoryPresetMetadata => f.write_str(FACTORY_PRESET_METADATA),
            Self::FactoryPresetSelect(code) => {
                write!(f, "{FACTORY_PRESET_SELECT} (tresult {code})")
            }
            Vst3Error::ParameterConversion => f.write_str(PARAMETER_CONVERSION),
            Vst3Error::ParameterResult => f.write_str(PARAMETER_RESULT),
            Vst3Error::ParameterGroups => f.write_str(PARAMETER_GROUPS),
            Vst3Error::StatePurposeUnsupported => f.write_str(STATE_PURPOSE_UNSUPPORTED),
            Vst3Error::Input(_) => f.write_str(INPUT),
            #[cfg(target_os = "macos")]
            Vst3Error::BundleOpen => f.write_str(BUNDLE_OPEN),
            Vst3Error::BundleLoad(_) => f.write_str(BUNDLE_LOAD),
            Vst3Error::NoEntryPoint(name) => write!(f, "{NO_ENTRY_POINT} {name}"),
            Vst3Error::EntryFailed => f.write_str(ENTRY_FAILED),
            Vst3Error::NoFactory => f.write_str(NO_FACTORY),
            Vst3Error::ClassNotFound(id) => write!(f, "{CLASS_NOT_FOUND} {id}"),
            Vst3Error::CreateComponent => f.write_str(CREATE_COMPONENT),
            Vst3Error::InitializeComponent(code) => {
                write!(f, "{INITIALIZE_COMPONENT} (tresult {code})")
            }
            Vst3Error::NoAudioProcessor => f.write_str(NO_AUDIO_PROCESSOR),
            Vst3Error::NoControllerClass => f.write_str(NO_CONTROLLER_CLASS),
            Vst3Error::CreateController => f.write_str(CREATE_CONTROLLER),
            Vst3Error::InitializeController(code) => {
                write!(f, "{INITIALIZE_CONTROLLER} (tresult {code})")
            }
            Vst3Error::SampleFormatUnsupported(format) => {
                write!(f, "{SAMPLE_FORMAT_UNSUPPORTED} {format:?}")
            }
            Vst3Error::ProcessModeUnsupported(mode) => {
                write!(f, "{PROCESS_MODE_UNSUPPORTED} {mode:?}")
            }
            Vst3Error::NoBus(layout) => write!(f, "{MAIN_BUS} ({layout:?})"),
            Vst3Error::LayoutRefused {
                requested,
                plugin_channels,
            } => write!(
                f,
                "{LAYOUT_REFUSED} ({requested:?}; the plugin wants {plugin_channels} channels)"
            ),
            Vst3Error::SetupRejected(code) => write!(f, "{SETUP_REJECTED} (tresult {code})"),
            Vst3Error::Activate(code) => write!(f, "{ACTIVATE} (tresult {code})"),
            Vst3Error::NotPrepared => f.write_str(NOT_PREPARED),
            Vst3Error::EventStorage => f.write_str(EVENT_STORAGE),
            Vst3Error::OutputEventCapacity => f.write_str(OUTPUT_EVENT_CAPACITY),
            Vst3Error::UnknownEventPort { id } => write!(f, "{UNKNOWN_EVENT_PORT} ({id})"),
            Vst3Error::ProcessingStorage => f.write_str(PROCESSING_STORAGE),
            Vst3Error::Process(code) => write!(f, "{PROCESS} (tresult {code})"),
            Vst3Error::RestartRequired(flags) => write!(f, "{RESTART_REQUIRED} (flags {flags:#x})"),
            Vst3Error::GetState(code) => write!(f, "{GET_STATE} (tresult {code})"),
            Vst3Error::StateTooLarge => state_too_large(f),
            Vst3Error::SetState(code) => write!(f, "{SET_STATE} (tresult {code})"),
            Vst3Error::StateMismatch => f.write_str(STATE_MISMATCH),
            Vst3Error::Preset => f.write_str(PRESET),
            Vst3Error::Closed => f.write_str(CLOSED),
            Vst3Error::NoEditor => f.write_str(NO_EDITOR),
            Vst3Error::EditorAttach(code) => write!(f, "{EDITOR_ATTACH} (tresult {code})"),
        }
    }
}

impl std::error::Error for Vst3Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Input(error) => Some(error),
            Self::BundleLoad(Some(error)) => Some(error.as_ref()),
            _ => None,
        }
    }
}

impl Vst3Error {
    /// The `tresult` the plugin returned.
    pub(crate) fn native_code(&self) -> Option<i32> {
        match self {
            Self::AudioBusActivation { code, .. }
            | Self::BypassEdit(code)
            | Self::FactoryPresetSelect(code)
            | Self::InitializeComponent(code)
            | Self::InitializeController(code)
            | Self::SetupRejected(code)
            | Self::Activate(code)
            | Self::Process(code)
            | Self::GetState(code)
            | Self::SetState(code)
            | Self::EditorAttach(code) => Some(*code),
            _ => None,
        }
    }

    /// Classifies this failure for scan policy and helper diagnostics.
    pub(crate) fn kind(&self) -> FailureKind {
        match self {
            Self::AudioBusMetadata => FailureKind::Host,
            Self::UnknownAudioBus { .. } | Self::UnknownEventPort { .. } => {
                FailureKind::InvalidInput
            }
            Self::OutputEventCapacity => FailureKind::Processing,
            Self::AudioConfigurationsUnsupported | Self::BypassUnsupported => {
                FailureKind::Unsupported
            }
            Self::AudioBusActivation { .. } => FailureKind::Configuration,
            Self::BypassEdit(_) => FailureKind::State,
            Self::FactoryPresetsUnsupported => FailureKind::Unsupported,
            Self::InvalidFactoryPreset => FailureKind::InvalidInput,
            Self::FactoryPresetMetadata => FailureKind::Host,
            Self::FactoryPresetSelect(_) => FailureKind::State,
            Self::ParameterGroups => FailureKind::Host,
            Self::StatePurposeUnsupported => FailureKind::Unsupported,
            Self::ParameterConversion => FailureKind::Unsupported,
            Self::ParameterResult => FailureKind::Host,
            Self::Input(_) | Self::Preset => FailureKind::InvalidInput,
            Self::ClassNotFound(_) => FailureKind::NotFound,
            Self::SampleFormatUnsupported(_)
            | Self::ProcessModeUnsupported(_)
            | Self::NoEditor
            | Self::NoAudioProcessor => FailureKind::Unsupported,
            #[cfg(target_os = "macos")]
            Self::BundleOpen => FailureKind::Load,
            Self::BundleLoad(_)
            | Self::NoEntryPoint(_)
            | Self::EntryFailed
            | Self::NoFactory
            | Self::CreateComponent
            | Self::InitializeComponent(_)
            | Self::NoControllerClass
            | Self::CreateController
            | Self::InitializeController(_) => FailureKind::Load,
            Self::EventStorage
            | Self::ProcessingStorage
            | Self::NoBus(_)
            | Self::LayoutRefused { .. }
            | Self::SetupRejected(_)
            | Self::Activate(_) => FailureKind::Configuration,
            Self::NotPrepared => FailureKind::NotPrepared,
            Self::Closed => FailureKind::Closed,
            Self::RestartRequired(_) => FailureKind::RestartRequired,
            Self::StateMismatch => FailureKind::StateMismatch,
            Self::GetState(code) | Self::SetState(code)
                if *code == vst3::Steinberg::kNotImplemented
                    || *code == vst3::Steinberg::kNoInterface =>
            {
                FailureKind::Unsupported
            }
            Self::GetState(_) | Self::SetState(_) | Self::StateTooLarge => FailureKind::State,
            Self::Process(_) => FailureKind::Processing,
            Self::EditorAttach(_) => FailureKind::Editor,
        }
    }
}
