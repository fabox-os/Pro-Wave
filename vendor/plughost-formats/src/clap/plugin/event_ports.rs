//! Note ports: their description, and the input ports a routing uses. CLAP ports cannot be
//! deactivated; the plugin receives events only on requested input ports.
use super::*;
use clack_extensions::note_ports::NotePortInfoBuffer;
use plughost_core::{AudioDirection, EventConfig, EventPortInfo, Support};

/// One note port, as the plugin describes it.
struct NotePort {
    id: u64,
    name: String,
    dialects: NoteDialects,
}

impl Plugin {
    fn note_ports(&mut self, input: bool) -> Result<Vec<NotePort>, ClapError> {
        let Some(extension) = self.shared().extensions().note_ports else {
            return Ok(Vec::new());
        };
        let handle = self.instance.plugin_handle();
        let count = extension.count(&handle, input);
        // Events address ports by a 16-bit index.
        if count > u32::from(u16::MAX) {
            return Err(ClapError::NotePortMetadata);
        }
        let mut buffer = NotePortInfoBuffer::new();
        (0..count)
            .map(|index| {
                let info = extension
                    .get(&handle, index, input, &mut buffer)
                    .ok_or(ClapError::NotePortMetadata)?;
                Ok(NotePort {
                    id: u64::from(info.id.get()),
                    name: String::from_utf8_lossy(info.name).into_owned(),
                    dialects: info.supported_dialects,
                })
            })
            .collect()
    }

    /// Note ports in both directions, inputs first. Channel messages need the CLAP or a MIDI
    /// dialect; system exclusive messages need the MIDI dialect, note expressions the CLAP dialect,
    /// and MPE the MIDI dialect with MPE.
    pub(crate) fn event_ports(&mut self) -> Result<Vec<EventPortInfo>, ClapError> {
        if let Some(prepared) = &lock(&self.engine).prepared {
            return Ok(prepared.event_ports.clone());
        }
        let mut ports = Vec::new();
        for (input, direction) in [
            (true, AudioDirection::Input),
            (false, AudioDirection::Output),
        ] {
            for (index, port) in self.note_ports(input)?.into_iter().enumerate() {
                let midi = port.dialects.supports(NoteDialect::Midi);
                ports.push({
                    let mut event_port_info =
                        EventPortInfo::new(port.id, index as u32, port.name, direction);
                    event_port_info.midi = Support::from(
                        midi || port.dialects.supports(NoteDialect::Clap)
                            || port.dialects.supports(NoteDialect::MidiMpe),
                    );
                    event_port_info.sysex = Support::from(midi);
                    event_port_info.note_expression =
                        Support::from(port.dialects.supports(NoteDialect::Clap));
                    event_port_info.mpe =
                        Support::from(port.dialects.supports(NoteDialect::MidiMpe));
                    event_port_info
                });
            }
        }
        Ok(ports)
    }

    /// The first note input and output, which main-bus preparation uses.
    pub(super) fn main_event_config(&mut self) -> Result<EventConfig, ClapError> {
        let mut first = |input| -> Result<Vec<u64>, ClapError> {
            Ok(self
                .note_ports(input)?
                .first()
                .map(|port| port.id)
                .into_iter()
                .collect())
        };
        Ok(EventConfig::new(first(true)?, first(false)?))
    }

    /// Per input port index, the dialects of a requested port and `None` for the others.
    pub(super) fn event_inputs(
        &mut self,
        config: &EventConfig,
    ) -> Result<Vec<Option<NoteDialects>>, ClapError> {
        let inputs = self.note_ports(true)?;
        let outputs = self.note_ports(false)?;
        for (requested, ports) in [(&config.inputs, &inputs), (&config.outputs, &outputs)] {
            if let Some(&id) = requested
                .iter()
                .find(|&&id| !ports.iter().any(|port| port.id == id))
            {
                return Err(ClapError::UnknownEventPort { id });
            }
        }
        Ok(inputs
            .iter()
            .map(|port| config.inputs.contains(&port.id).then_some(port.dialects))
            .collect())
    }
}
