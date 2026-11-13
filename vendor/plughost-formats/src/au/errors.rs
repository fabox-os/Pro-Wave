use std::fmt;

use plughost_core::{FailureKind, InputError, Layout, SampleFormat};

use crate::errors::{
    CLOSED, INPUT, NOT_PREPARED, PARAMETER_CONVERSION, PARAMETER_RESULT, STATE_PURPOSE_UNSUPPORTED,
    state_too_large,
};
use plughost_core::messages::{
    OUTPUT_EVENT_CAPACITY, STATE_MISMATCH, UNCONVERTIBLE_OUTPUT_EVENTS, UNKNOWN_EVENT_PORT,
};

const AUDIO_BUS_METADATA: &str = "the Audio Unit returned invalid audio bus metadata";
const AUDIO_CONFIGURATION: &str = "the Audio Unit does not support the requested bus configuration";
const BYPASS_UNSUPPORTED: &str = "the Audio Unit does not support the requested bypass state";
const FACTORY_PRESET: &str = "the Audio Unit factory preset is unavailable";
const FACTORY_PRESET_METADATA: &str =
    "the Audio Unit returned invalid or excessive factory preset metadata";
const FACTORY_PRESET_SELECTION: &str = "the Audio Unit did not select the requested factory preset";
const CLASS_ID: &str = "not an Audio Unit class ID (24 hexadecimal digits)";
const CLASS_NOT_FOUND: &str = "no registered Audio Unit has class ID";
const INSTANTIATE: &str = "the Audio Unit could not be instantiated";
const SET_PARAMETER: &str = "the Audio Unit rejected the parameter change";
const SAMPLE_FORMAT_UNSUPPORTED: &str = "Audio Units process 32-bit float samples only";
const NO_BUS: &str = "the Audio Unit has no bus for the requested layout";
const LAYOUT_REFUSED: &str = "the Audio Unit refused the requested layout";
const RATE_REFUSED: &str = "the Audio Unit refused the sample rate on an inactive bus";
const ALLOCATE: &str = "the Audio Unit could not allocate render resources";
const MAXIMUM_FRAMES: &str = "the Audio Unit lowered the maximum block size";
const PROCESSING_STORAGE: &str = "could not reserve the prepared Audio Unit audio buffers";
const RENDER: &str = "the Audio Unit's render call failed";
const RENDER_OUTPUT: &str = "the Audio Unit returned render buffers that do not match the request";
const RENDER_BLOCKS: &str = "the Audio Unit returned no render or parameter scheduling block";
const STATE: &str = "the Audio Unit state could not be encoded or decoded";
const INVALIDATED: &str =
    "the Audio Unit extension connection was invalidated; create a new instance";
const NO_EDITOR: &str = "the Audio Unit has no editor view";

pub(crate) fn unconvertible_output_events(count: u64) -> String {
    format!("{UNCONVERTIBLE_OUTPUT_EVENTS} {count}")
}

/// What an `NSError` an Audio Unit call returned says: its code, which is an `OSStatus` for
/// Audio Unit errors, and its localized description.
#[derive(Debug)]
pub(crate) struct NativeError {
    pub(crate) code: isize,
    pub(crate) description: String,
}

impl fmt::Display for NativeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (code {})", self.description, self.code)
    }
}

impl std::error::Error for NativeError {}

/// Audio Unit loading, configuration, rendering, state, and editor failures.
#[derive(Debug)]
pub(crate) enum AuError {
    /// Caller input violated a processing or control contract.
    Input(InputError),
    /// The requested native event port does not exist or is not enabled.
    UnknownEventPort {
        /// Requested native event port ID.
        id: u64,
    },
    /// The Audio Unit returned invalid audio bus metadata.
    AudioBusMetadata,
    /// The Audio Unit does not support the requested bus configuration.
    AudioConfiguration,
    /// The Audio Unit does not support the requested bypass state.
    BypassUnsupported,
    /// The encoded native state exceeds `MAX_STATE_BYTES`.
    StateTooLarge,
    /// The Audio Unit factory preset is unavailable.
    FactoryPreset,
    /// The Audio Unit returned invalid or excessive factory preset metadata.
    FactoryPresetMetadata,
    /// The Audio Unit did not select the requested factory preset.
    FactoryPresetSelection,
    /// The plugin does not support the requested state context.
    StatePurposeUnsupported,
    /// The plugin does not support the requested parameter conversion.
    ParameterConversion,
    /// The plugin returned a non-finite or otherwise invalid parameter conversion.
    ParameterResult,
    /// The supplied Audio Unit class ID is not 24 hexadecimal digits.
    ClassId(String),
    /// No registered Audio Unit has the class ID.
    ClassNotFound(String),
    /// The Audio Unit could not be instantiated, with the error it returned if any.
    Instantiate(Option<NativeError>),
    /// Audio Units process 32-bit float samples only.
    SampleFormatUnsupported(SampleFormat),
    /// No main audio bus can provide the requested layout.
    NoBus(Layout),
    /// The Audio Unit refused the requested layout.
    LayoutRefused(Layout, NativeError),
    /// The Audio Unit refused the sample rate on a bus it is prepared with inactive.
    RateRefused(NativeError),
    /// The Audio Unit could not allocate render resources.
    Allocate(NativeError),
    /// The Audio Unit lowered the maximum block size it was given.
    MaximumFrames {
        /// The prepared maximum block size.
        requested: usize,
        /// The maximum the unit kept.
        accepted: u64,
    },
    /// `AudioUnitSetParameter` returned the supplied OSStatus error code.
    SetParameter(i32),
    /// The plugin is not prepared for processing.
    NotPrepared,
    /// Could not reserve the prepared Audio Unit audio buffers.
    ProcessingStorage,
    /// The native render call returned the supplied OSStatus error code.
    Render(i32),
    /// The Audio Unit returned render buffers that do not match the request.
    RenderOutput,
    /// The Audio Unit returned no render or parameter scheduling block.
    RenderBlocks,
    /// Native output exceeded the per-block event or system-exclusive budget; reset before processing again.
    OutputEventCapacity,
    /// The Audio Unit state could not be encoded or decoded.
    State,
    /// Saved state belongs to a different plugin format or class.
    StateMismatch,
    /// The owning plugin handle has been dropped; its processor is no longer usable.
    Closed,
    /// The Audio Unit extension connection was invalidated; create a new instance.
    Invalidated,
    /// The Audio Unit has no editor view.
    NoEditor,
}

impl fmt::Display for AuError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AuError::AudioBusMetadata => f.write_str(AUDIO_BUS_METADATA),
            AuError::AudioConfiguration => f.write_str(AUDIO_CONFIGURATION),
            AuError::BypassUnsupported => f.write_str(BYPASS_UNSUPPORTED),
            AuError::StateTooLarge => state_too_large(f),
            AuError::FactoryPreset => f.write_str(FACTORY_PRESET),
            AuError::FactoryPresetMetadata => f.write_str(FACTORY_PRESET_METADATA),
            AuError::FactoryPresetSelection => f.write_str(FACTORY_PRESET_SELECTION),
            AuError::ParameterConversion => f.write_str(PARAMETER_CONVERSION),
            AuError::ParameterResult => f.write_str(PARAMETER_RESULT),
            AuError::StatePurposeUnsupported => f.write_str(STATE_PURPOSE_UNSUPPORTED),
            AuError::Input(_) => f.write_str(INPUT),
            AuError::ClassId(id) => write!(f, "{CLASS_ID}: {id}"),
            AuError::ClassNotFound(id) => write!(f, "{CLASS_NOT_FOUND} {id}"),
            AuError::Instantiate(_) => f.write_str(INSTANTIATE),
            AuError::SetParameter(status) => write!(f, "{SET_PARAMETER} (OSStatus {status})"),
            AuError::SampleFormatUnsupported(format) => {
                write!(f, "{SAMPLE_FORMAT_UNSUPPORTED} ({format:?})")
            }
            AuError::NoBus(layout) => write!(f, "{NO_BUS} ({layout:?})"),
            AuError::LayoutRefused(layout, _) => write!(f, "{LAYOUT_REFUSED} ({layout:?})"),
            AuError::RateRefused(_) => f.write_str(RATE_REFUSED),
            AuError::Allocate(_) => f.write_str(ALLOCATE),
            AuError::MaximumFrames {
                requested,
                accepted,
            } => write!(f, "{MAXIMUM_FRAMES} from {requested} to {accepted} frames"),
            AuError::NotPrepared => f.write_str(NOT_PREPARED),
            AuError::Invalidated => f.write_str(INVALIDATED),
            AuError::ProcessingStorage => f.write_str(PROCESSING_STORAGE),
            AuError::UnknownEventPort { id } => write!(f, "{UNKNOWN_EVENT_PORT} ({id})"),
            AuError::Render(status) => write!(f, "{RENDER} (OSStatus {status})"),
            AuError::RenderOutput => f.write_str(RENDER_OUTPUT),
            AuError::RenderBlocks => f.write_str(RENDER_BLOCKS),
            AuError::OutputEventCapacity => f.write_str(OUTPUT_EVENT_CAPACITY),
            AuError::State => f.write_str(STATE),
            AuError::StateMismatch => f.write_str(STATE_MISMATCH),
            AuError::Closed => f.write_str(CLOSED),
            AuError::NoEditor => f.write_str(NO_EDITOR),
        }
    }
}

impl std::error::Error for AuError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Input(error) => Some(error),
            Self::Instantiate(Some(error))
            | Self::LayoutRefused(_, error)
            | Self::RateRefused(error)
            | Self::Allocate(error) => Some(error),
            _ => None,
        }
    }
}

impl AuError {
    /// The `OSStatus` of the failed call, or the code of the `NSError` it returned.
    pub(crate) fn native_code(&self) -> Option<i32> {
        match self {
            Self::Render(status) | Self::SetParameter(status) => Some(*status),
            Self::Instantiate(Some(error))
            | Self::LayoutRefused(_, error)
            | Self::RateRefused(error)
            | Self::Allocate(error) => i32::try_from(error.code).ok(),
            _ => None,
        }
    }

    /// Classifies this failure for scan policy and helper diagnostics.
    pub(crate) fn kind(&self) -> FailureKind {
        match self {
            Self::AudioBusMetadata | Self::RenderBlocks => FailureKind::Host,
            Self::AudioConfiguration | Self::ProcessingStorage => FailureKind::Configuration,
            Self::BypassUnsupported => FailureKind::Unsupported,
            Self::StateTooLarge => FailureKind::State,
            Self::FactoryPreset => FailureKind::InvalidInput,
            Self::FactoryPresetMetadata => FailureKind::Host,
            Self::FactoryPresetSelection => FailureKind::State,
            Self::StatePurposeUnsupported => FailureKind::Unsupported,
            Self::ParameterConversion => FailureKind::Unsupported,
            Self::ParameterResult => FailureKind::Host,
            Self::Input(_) | Self::ClassId(_) | Self::UnknownEventPort { .. } => {
                FailureKind::InvalidInput
            }
            Self::ClassNotFound(_) => FailureKind::NotFound,
            Self::Instantiate(_) => FailureKind::Load,
            Self::SetParameter(_) => FailureKind::State,
            Self::SampleFormatUnsupported(_) | Self::NoEditor => FailureKind::Unsupported,
            Self::NoBus(_)
            | Self::LayoutRefused(_, _)
            | Self::RateRefused(_)
            | Self::Allocate(_)
            | Self::MaximumFrames { .. } => FailureKind::Configuration,
            Self::NotPrepared => FailureKind::NotPrepared,
            Self::Closed => FailureKind::Closed,
            Self::Invalidated => FailureKind::RestartRequired,
            Self::StateMismatch => FailureKind::StateMismatch,
            Self::State => FailureKind::State,
            Self::Render(_) | Self::RenderOutput | Self::OutputEventCapacity => {
                FailureKind::Processing
            }
        }
    }
}
