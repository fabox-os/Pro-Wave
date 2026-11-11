//! Parameter delivery between blocks on the thread assigned by CLAP.

use super::*;
use plughost_core::internal::ParameterEventBuffer;

impl Plugin {
    /// Delivers queued host edits and any changes the plugin asked to flush.
    pub(super) fn flush(&mut self) {
        let parameters = self.parameter_cache();
        if lock(&self.engine).processor.is_some() {
            self.flush_active();
        } else if let Some(params) = self.shared().extensions().params {
            let flush = Flush::take(self.shared());
            if let Some(mut handle) = self.instance.inactive_plugin_handle() {
                flush.deliver(&parameters, |input, output| {
                    params.flush(&mut handle, input, output)
                });
            }
        }
    }

    /// Timing queries use this through a shared borrow. Activation and parameter metadata are
    /// owned by the main thread; its latest cache is also the one used to queue host edits.
    pub(super) fn flush_active(&self) {
        let shared = self.shared();
        let Some(params) = shared.extensions().params else {
            return;
        };
        let engine = Arc::clone(&self.engine);
        let parameters = Arc::clone(&self.parameters);
        self.audio.run(|| {
            if let Some(processor) = lock(&engine).processor.as_mut() {
                Flush::take(shared).deliver(&parameters, |input, output| {
                    let _audit = plughost_core::audit::plugin();
                    let _floats = crate::float_mode::FlushToZero::enter();
                    in_audio_context(|| {
                        params.flush_active(&mut processor.plugin_handle(), input, output)
                    });
                });
            }
        });
    }
}

struct Flush {
    input: EventBuffer,
    output: ParameterEventBuffer,
}

impl Flush {
    fn take(shared: &Shared) -> Self {
        shared.flush_requested.store(false, Ordering::Relaxed);
        let mut input = EventBuffer::new();
        for (id, value) in shared.take_pending() {
            if let Some(id) = ClapId::from_raw(id) {
                input.push(&ParamValueEvent::new(0, id, Pckn::match_all(), value));
            }
        }
        Self {
            input,
            output: shared.parameter_events.clone(),
        }
    }

    fn deliver(
        self,
        parameters: &ParameterCache,
        flush: impl FnOnce(&InputEvents, &mut OutputEvents),
    ) {
        let mut output = events::ParameterOutput {
            parameters,
            queue: &self.output,
        };
        flush(
            &InputEvents::from_buffer(&self.input),
            &mut OutputEvents::from_buffer(&mut output),
        );
    }
}
