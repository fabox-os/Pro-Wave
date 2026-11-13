#[cfg(all(feature = "au", target_os = "macos"))]
use crate::au::AuError;
#[cfg(feature = "clap")]
use crate::clap::ClapError;
#[cfg(feature = "vst3")]
use crate::vst3::Vst3Error;
use plughost_core::{Failure, FailureKind, InputError, PluginFormat, RenderError};
use std::fmt;

const UNSUPPORTED_FORMAT: &str = "this build does not support the plugin's format";
const CONTINUOUS_PARAMETER: &str = "continuous parameters do not expose a discrete choice list";
const PRESET_UNSUPPORTED: &str = "this format has no supported preset file codec";
const FACTORY_PRESETS_UNSUPPORTED: &str = "this format has no supported factory program interface";
const PRESET_DISCOVERY_UNSUPPORTED: &str = "this format has no supported discovery preset loader";
pub(crate) const INPUT: &str = "invalid input";
pub(crate) const RENDER: &str = "rendering failed";

// Messages shared by the format-specific errors.
#[cfg(any(
    feature = "vst3",
    feature = "clap",
    all(feature = "au", target_os = "macos")
))]
pub(crate) const NOT_PREPARED: &str = "the plugin is not prepared for processing";
#[cfg(any(feature = "vst3", all(feature = "au", target_os = "macos")))]
pub(crate) const CLOSED: &str = "the plugin was closed";
#[cfg(any(feature = "vst3", feature = "clap"))]
pub(crate) const ACTIVATE: &str = "the plugin failed to activate";
#[cfg(any(feature = "vst3", feature = "clap"))]
pub(crate) const PROCESS: &str = "the plugin's process call failed";
#[cfg(any(feature = "vst3", feature = "clap"))]
pub(crate) const NO_EDITOR: &str = "the plugin has no editor for this platform";
#[cfg(any(
    feature = "vst3",
    feature = "clap",
    all(feature = "au", target_os = "macos")
))]
pub(crate) const STATE_TOO_LARGE: &str = "the plugin's saved state exceeds the maximum size";
#[cfg(any(
    feature = "vst3",
    feature = "clap",
    all(feature = "au", target_os = "macos")
))]
pub(crate) const STATE_PURPOSE_UNSUPPORTED: &str =
    "the plugin does not support the requested state context";
#[cfg(any(
    feature = "vst3",
    feature = "clap",
    all(feature = "au", target_os = "macos")
))]
pub(crate) const PARAMETER_CONVERSION: &str =
    "the plugin does not provide this parameter conversion";
#[cfg(any(
    feature = "vst3",
    feature = "clap",
    all(feature = "au", target_os = "macos")
))]
pub(crate) const PARAMETER_RESULT: &str =
    "the plugin returned an invalid parameter conversion result";

/// The message for a saved state over `MAX_STATE_BYTES`.
#[cfg(any(
    feature = "vst3",
    feature = "clap",
    all(feature = "au", target_os = "macos")
))]
pub(crate) fn state_too_large(f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(
        f,
        "{STATE_TOO_LARGE} ({} MiB)",
        plughost_core::MAX_STATE_BYTES >> 20
    )
}

/// A failure of an in-process plugin host or of the native plugin it hosts.
///
/// Use [`Error::kind`] for a stable category, [`Error::input_error`] for rejected caller input,
/// [`Error::format`] and [`Error::native_code`] for the native side, and
/// [`std::error::Error::source`] for the underlying cause. `Display` gives this failure's own
/// context without repeating its source; [`Error::failure`] flattens the whole chain.
pub struct Error(Repr);

/// What failed: the shared failures, or a format host's own. Each format's enum stays inside the
/// crate, so its variants can change without changing the public API.
pub(crate) enum Repr {
    ContinuousParameter,
    PresetUnsupported,
    FactoryPresetsUnsupported,
    PresetDiscoveryUnsupported,
    Input(InputError),
    #[cfg(feature = "vst3")]
    Vst3(Vst3Error),
    #[cfg(all(feature = "au", target_os = "macos"))]
    Au(AuError),
    #[cfg(feature = "clap")]
    Clap(ClapError),
    UnsupportedFormat(PluginFormat),
    Render(RenderError),
}

impl Error {
    pub(crate) fn new(repr: Repr) -> Error {
        Error(repr)
    }

    /// Classifies this failure for scan policy and helper diagnostics.
    pub fn kind(&self) -> FailureKind {
        match &self.0 {
            Repr::Input(_) => FailureKind::InvalidInput,
            Repr::UnsupportedFormat(_)
            | Repr::PresetUnsupported
            | Repr::ContinuousParameter
            | Repr::FactoryPresetsUnsupported
            | Repr::PresetDiscoveryUnsupported => FailureKind::Unsupported,
            Repr::Render(error) => error.kind(),
            #[cfg(feature = "vst3")]
            Repr::Vst3(error) => error.kind(),
            #[cfg(feature = "clap")]
            Repr::Clap(error) => error.kind(),
            #[cfg(all(feature = "au", target_os = "macos"))]
            Repr::Au(error) => error.kind(),
        }
    }

    /// The format whose host failed, or the requested format this build does not support. `None`
    /// for rejected caller input, whichever format checked it.
    pub fn format(&self) -> Option<PluginFormat> {
        match &self.0 {
            #[cfg(feature = "vst3")]
            Repr::Vst3(_) => Some(PluginFormat::Vst3),
            #[cfg(all(feature = "au", target_os = "macos"))]
            Repr::Au(_) => Some(PluginFormat::AudioUnit),
            #[cfg(feature = "clap")]
            Repr::Clap(_) => Some(PluginFormat::Clap),
            Repr::UnsupportedFormat(format) => Some(*format),
            _ => None,
        }
    }

    /// The result code the native plugin or system call returned: a VST3 `tresult`, or an Audio
    /// Unit `OSStatus` (an `NSError` code where the call reports one). CLAP calls report none.
    pub fn native_code(&self) -> Option<i32> {
        match &self.0 {
            #[cfg(feature = "vst3")]
            Repr::Vst3(error) => error.native_code(),
            #[cfg(all(feature = "au", target_os = "macos"))]
            Repr::Au(error) => error.native_code(),
            _ => None,
        }
    }

    /// Caller input rejected by the owning format before it changed the plugin.
    pub fn input_error(&self) -> Option<InputError> {
        match &self.0 {
            Repr::Input(error) | Repr::Render(RenderError::Input(error)) => Some(*error),
            _ => None,
        }
    }

    /// A plugin produced more events than one block allows; the block's events are lost, so
    /// the caller resynchronizes with a reset.
    pub fn is_output_event_overflow(&self) -> bool {
        match &self.0 {
            #[cfg(feature = "vst3")]
            Repr::Vst3(Vst3Error::OutputEventCapacity) => true,
            #[cfg(feature = "clap")]
            Repr::Clap(ClapError::OutputEventCapacity) => true,
            #[cfg(all(feature = "au", target_os = "macos"))]
            Repr::Au(AuError::OutputEventCapacity) => true,
            _ => false,
        }
    }

    /// Copies the failure category and the diagnostic of the whole source chain into a
    /// transportable report.
    pub fn failure(&self) -> Failure {
        let mut message = self.to_string();
        let mut cause = std::error::Error::source(self);
        while let Some(error) = cause {
            message.push_str(": ");
            message.push_str(&error.to_string());
            cause = error.source();
        }
        Failure::new(self.kind(), message)
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut debug = f.debug_struct("Error");
        debug.field("kind", &self.kind());
        if let Some(format) = self.format() {
            debug.field("format", &format);
        }
        if let Some(code) = self.native_code() {
            debug.field("native_code", &code);
        }
        debug.field("message", &self.to_string());
        if let Some(source) = std::error::Error::source(self) {
            debug.field("source", &source);
        }
        debug.finish()
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Repr::ContinuousParameter => f.write_str(CONTINUOUS_PARAMETER),
            Repr::PresetUnsupported => f.write_str(PRESET_UNSUPPORTED),
            Repr::FactoryPresetsUnsupported => f.write_str(FACTORY_PRESETS_UNSUPPORTED),
            Repr::PresetDiscoveryUnsupported => f.write_str(PRESET_DISCOVERY_UNSUPPORTED),
            Repr::Input(_) => f.write_str(INPUT),
            #[cfg(feature = "vst3")]
            Repr::Vst3(error) => error.fmt(f),
            #[cfg(all(feature = "au", target_os = "macos"))]
            Repr::Au(error) => error.fmt(f),
            #[cfg(feature = "clap")]
            Repr::Clap(error) => error.fmt(f),
            Repr::UnsupportedFormat(format) => write!(f, "{UNSUPPORTED_FORMAT} ({format:?})"),
            Repr::Render(_) => f.write_str(RENDER),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.0 {
            Repr::Input(error) => Some(error),
            Repr::Render(error) => Some(error),
            #[cfg(feature = "vst3")]
            Repr::Vst3(error) => error.source(),
            #[cfg(all(feature = "au", target_os = "macos"))]
            Repr::Au(error) => error.source(),
            #[cfg(feature = "clap")]
            Repr::Clap(error) => error.source(),
            _ => None,
        }
    }
}

impl From<InputError> for Error {
    fn from(error: InputError) -> Error {
        Error(Repr::Input(error))
    }
}

impl From<RenderError> for Error {
    fn from(error: RenderError) -> Error {
        Error(Repr::Render(error))
    }
}

// Caller input a format rejected is one shared failure, whichever format checked it.
#[cfg(feature = "vst3")]
impl From<Vst3Error> for Error {
    fn from(error: Vst3Error) -> Error {
        match error {
            Vst3Error::Input(error) => Error(Repr::Input(error)),
            error => Error(Repr::Vst3(error)),
        }
    }
}

#[cfg(all(feature = "au", target_os = "macos"))]
impl From<AuError> for Error {
    fn from(error: AuError) -> Error {
        match error {
            AuError::Input(error) => Error(Repr::Input(error)),
            error => Error(Repr::Au(error)),
        }
    }
}

#[cfg(feature = "clap")]
impl From<ClapError> for Error {
    fn from(error: ClapError) -> Error {
        match error {
            ClapError::Input(error) => Error(Repr::Input(error)),
            error => Error(Repr::Clap(error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_gives_the_context_and_the_failure_the_whole_chain() {
        let error = Error::from(InputError::Events);
        assert_eq!(error.to_string(), INPUT);
        assert_eq!(error.input_error(), Some(InputError::Events));
        let source = std::error::Error::source(&error).unwrap();
        assert_eq!(source.to_string(), InputError::Events.to_string());
        let failure = error.failure();
        assert_eq!(failure.kind, FailureKind::InvalidInput);
        assert_eq!(failure.message, format!("{INPUT}: {}", InputError::Events));
    }
}
