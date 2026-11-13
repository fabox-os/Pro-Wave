use super::*;
impl Processor {
    pub(super) fn process_audio_impl<S: plughost_core::Sample>(
        &self,
        context: &plughost_core::BlockContext,
        input: &[&[S]],
        output: &mut [&mut [S]],
        automation: &[plughost_core::ParameterChange],
        events: &plughost_core::Events,
        produced: &mut plughost_core::Events,
    ) -> Result<(), ClapError> {
        produced.clear();
        let mut engine = lock(&self.engine);
        let Engine {
            processor: Some(processor),
            prepared: Some(prepared),
            restart_deferred,
        } = &mut *engine
        else {
            return Err(ClapError::NotPrepared);
        };
        // A restart requested during an earlier block is reported from the next one, unless it
        // is deferred; the owning thread prepares the plugin again.
        if !*restart_deferred
            && processor.access_shared_handler(|shared| {
                shared.restart_requested.load(Ordering::Relaxed)
                    || shared.latency_changed.load(Ordering::Relaxed)
            })
        {
            return Err(ClapError::RestartRequired);
        }
        context.validate().map_err(ClapError::Input)?;
        if let Some(transport) = context.transport {
            transport
                .validate_at_rate(prepared.config.sample_rate)
                .map_err(ClapError::Input)?;
        }
        plughost_core::internal::validate_event_budget(automation.len(), events)
            .map_err(ClapError::Input)?;
        if let Some(event) = events.iter().find(|event| {
            prepared
                .event_inputs
                .get(event.port)
                .copied()
                .flatten()
                .is_none()
        }) {
            return Err(ClapError::Input(plughost_core::InputError::EventPort {
                port: event.port,
            }));
        }
        let frames = context.frames;
        // Explicit bus preparation flattens every active port; main-bus preparation uses main ports.
        let all_buses = prepared.audio_config.is_some();
        let (main_inputs, main_outputs) = if all_buses {
            prepared.buffers.active_channels()
        } else {
            (
                prepared.main_input.map_or(0, |i| prepared.inputs[i]),
                prepared.main_output.map_or(0, |i| prepared.outputs[i]),
            )
        };
        if S::FORMAT != prepared.config.sample_format {
            return Err(ClapError::Input(
                plughost_core::InputError::AudioSampleFormat,
            ));
        }

        if !events_fit(events, frames) {
            return Err(ClapError::Input(plughost_core::InputError::Events));
        }
        if input.len() != main_inputs
            || output.len() != main_outputs
            || frames > prepared.config.max_block_size
            || input.iter().any(|c| c.len() != frames)
            || output.iter().any(|c| c.len() != frames)
        {
            return Err(ClapError::Input(plughost_core::InputError::AudioBuffers));
        }
        if !plughost_core::internal::changes_fit(automation, frames) {
            return Err(ClapError::Input(plughost_core::InputError::Automation));
        }
        if frames == 0 {
            return Ok(());
        }

        prepared.automation.clear();
        for change in automation {
            let parameter = prepared.parameters.get(change.id).ok_or(ClapError::Input(
                plughost_core::InputError::UnknownParameter { id: change.id },
            ))?;
            parameter
                .info
                .automation_value(change.value)
                .map_err(ClapError::Input)?;
            let plain = parameter
                .plain(change.value)
                .ok_or(ClapError::ParameterResult)?;
            prepared
                .automation
                .push((change.offset as u32, parameter.native, plain));
        }
        let transport = context
            .transport
            .map(|value| super::super::transport::event(value, prepared.config.sample_rate))
            .transpose()?;
        let next_time = prepared
            .steady_time
            .checked_add(frames as u64)
            .filter(|time| *time <= i64::MAX as u64)
            .ok_or(ClapError::Input(plughost_core::InputError::Transport))?;
        // Starting can fail without processing anything. Keep host edits queued until it
        // succeeds so the next block can retry them.
        let started = in_audio_context(|| {
            let _audit = plughost_core::audit::plugin();
            let _floats = crate::float_mode::FlushToZero::enter();
            processor.ensure_processing_started()
        })
        .map_err(ClapError::Process)?;
        // Host edits apply from the start of the block, so they come before the notes. They stay
        // queued until the plugin has processed the block.
        let input_events_buffer = &mut prepared.input_events;
        input_events_buffer.clear();
        let edits = started.access_shared_handler(|shared| {
            shared.flush_requested.store(false, Ordering::Relaxed);
            shared.copy_pending(|id, value| {
                if let Some(id) = ClapId::from_raw(id) {
                    input_events_buffer.parameter(0, id, value);
                }
            })
        });
        for &(offset, id, value) in &prepared.automation {
            input_events_buffer.parameter(offset, id, value);
        }
        for event in events {
            if let Some(Some(dialects)) = prepared.event_inputs.get(event.port) {
                input_events_buffer.note(&event, *dialects);
            }
        }
        input_events_buffer.sort();
        let parameter_events =
            started.access_shared_handler(|shared| shared.parameter_events.clone());
        let mut output_events_buffer = output_events::ProcessOutput {
            parameters: events::ParameterOutput {
                parameters: &prepared.parameters,
                queue: &parameter_events,
            },
            events: produced,
            sounding: &mut prepared.sounding,
            frames,
            sysex: 0,
            overflow: false,
            unconvertible: 0,
        };

        let buffers = prepared.buffers.typed::<S>()?;
        let (inputs, outputs) = (&mut buffers.inputs, &mut buffers.outputs);
        inputs.begin(frames);
        outputs.begin(frames);
        inputs.copy_input(input, prepared.main_input, all_buses);
        // SAFETY: NativeBuffers owns all sample arrays and pointer tables, selected only after
        // verifying S is exactly f32/f64. They remain alive for the synchronous process call.
        let input_audio = unsafe {
            clack_host::process::audio_buffers::InputAudioBuffers::from_raw_buffers(
                &inputs.raw,
                frames as u32,
            )
        };
        let mut output_audio = unsafe {
            clack_host::process::audio_buffers::OutputAudioBuffers::from_raw_buffers(
                &mut outputs.raw,
                frames as u32,
            )
        };
        let input_events = InputEvents::from_buffer(input_events_buffer);
        let mut output_events = OutputEvents::from_buffer(&mut output_events_buffer);
        let steady_time = prepared.steady_time;
        let result = in_audio_context(|| {
            let processed = {
                let _audit = plughost_core::audit::plugin();
                let _floats = crate::float_mode::FlushToZero::enter();
                started.process(
                    &input_audio,
                    &mut output_audio,
                    &input_events,
                    &mut output_events,
                    Some(steady_time),
                    transport.as_ref(),
                )
            };
            processed.map_err(ClapError::Process)
        });
        if result.is_ok() {
            processor.access_shared_handler(|shared| {
                shared.forget_delivered(edits);
                shared.blocks.fetch_add(1, Ordering::Relaxed);
            });
        }
        prepared.steady_time = next_time;
        if processor
            .access_shared_handler(|shared| shared.tail_changed.swap(false, Ordering::Relaxed))
        {
            prepared.tail = read_tail(processor);
        }
        let (overflow, unconvertible) = (
            output_events_buffer.overflow,
            output_events_buffer.unconvertible,
        );
        if unconvertible > 0 {
            processor
                .access_shared_handler(|shared| shared.unconvertible_output_events(unconvertible));
        }
        result?;
        if overflow {
            produced.clear();
            return Err(ClapError::OutputEventCapacity);
        }
        // Plugins push events in time order; the sort only enforces it.
        produced.sort_by_offset();
        outputs.copy_output(output, prepared.main_output, all_buses);
        Ok(())
    }
}
