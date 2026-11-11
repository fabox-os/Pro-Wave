use super::MainThread;
use clack_extensions::preset_discovery::{HostPresetLoadImpl, preset_data::Location};
use plughost_core::DiagnosticSeverity;
use std::ffi::CStr;
use std::sync::atomic::Ordering;
impl HostPresetLoadImpl for MainThread<'_> {
    fn on_error(
        &self,
        _location: Location,
        _key: Option<&CStr>,
        os_error: i32,
        message: Option<&CStr>,
    ) {
        self.shared
            .preset_load_failed
            .store(true, Ordering::Relaxed);
        let text = message
            .map(|m| m.to_string_lossy().into_owned())
            .unwrap_or_else(|| super::super::errors::PRESET_LOAD.to_owned());
        self.shared.diagnostics.record(
            DiagnosticSeverity::Error,
            &format!("{text} (OS {os_error})"),
            Some(&self.shared.plugin),
            None,
            None,
        );
    }
    fn loaded(&self, _location: Location, _key: Option<&CStr>) {
        self.shared.diagnostics.record(
            DiagnosticSeverity::Info,
            super::super::errors::PRESET_LOADED,
            Some(&self.shared.plugin),
            None,
            None,
        );
    }
}
