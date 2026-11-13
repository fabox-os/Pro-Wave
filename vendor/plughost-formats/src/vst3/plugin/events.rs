use super::*;
use plughost_core::{Diagnostic, DiagnosticBatch, DiagnosticSeverity, ParameterEventBatch};

impl Plugin {
    /// Takes the controller's notifications, after delivering the values the processing thread
    /// handed back and reading a changed parameter list again.
    pub(crate) fn take_parameter_events(&mut self) -> ParameterEventBatch {
        self.sync_controller();
        self.parameter_cache();
        let batch = self.handler.events.take();
        // The plugin changed its values itself; the processing side takes them again.
        if batch.resync_required || batch.events.contains(&ParameterEvent::ValuesChanged) {
            self.seed_held_values();
        }
        batch
    }

    /// Reports output events the plugin produced that have no MIDI 1.0 form, which are not
    /// delivered.
    pub(crate) fn take_diagnostics(&self) -> DiagnosticBatch {
        let unconvertible = self.handler.take_unconvertible_output_events();
        DiagnosticBatch::new(
            (unconvertible > 0)
                .then(|| {
                    Diagnostic::new(
                        DiagnosticSeverity::Warning,
                        super::super::errors::unconvertible_output_events(unconvertible),
                        None,
                        None,
                        None,
                        false,
                    )
                })
                .into_iter()
                .collect(),
            0,
        )
    }
}
