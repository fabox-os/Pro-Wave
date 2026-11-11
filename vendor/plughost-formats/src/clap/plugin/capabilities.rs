use clack_extensions::audio_ports::{AudioPortFlags, AudioPortInfoBuffer};
use clack_extensions::gui::{GuiApiType, GuiConfiguration};
use clack_extensions::note_ports::{NoteDialect, NotePortInfoBuffer};
use clack_extensions::params::{ParamInfoBuffer, ParamInfoFlags};
use plughost_core::{Capabilities, Support};

use super::Plugin;
use super::parameter_cache::MAX_PARAMETERS;

/// What the plugin's audio and note ports support, scanned while it is inactive.
#[derive(Clone, Copy)]
pub(super) struct PortSupport {
    note_input: Support,
    note_output: Support,
    f32: Support,
    f64: Support,
}

impl Plugin {
    /// Queries advertised extensions and the current ports/parameters, without opening a GUI.
    /// An active plugin's ports are those it was activated with.
    pub(crate) fn capabilities(&mut self) -> Capabilities {
        let cached = super::lock(&self.engine)
            .prepared
            .as_ref()
            .map(|prepared| prepared.port_support);
        let ports = cached.unwrap_or_else(|| self.port_support());
        let extensions = self.shared().extensions();
        let (audio, params, gui, state) = (
            extensions.audio_ports,
            extensions.params,
            extensions.gui,
            extensions.state,
        );
        let handle = self.instance.plugin_handle();
        let embedded_editor = gui
            .is_some_and(|gui| {
                GuiApiType::default_for_current_platform().is_some_and(|api_type| {
                    gui.is_api_supported(
                        &handle,
                        GuiConfiguration {
                            api_type,
                            is_floating: false,
                        },
                    )
                })
            })
            .into();
        let mut automation = Support::Unsupported;
        if let Some(params) = params {
            let mut buffer = ParamInfoBuffer::new();
            let count = params.count(&handle);
            // Parameters past those the host reads are not looked at.
            if count > MAX_PARAMETERS {
                automation = Support::Unknown;
            }
            for index in 0..count.min(MAX_PARAMETERS) {
                match params.get_info(&handle, index, &mut buffer) {
                    Some(info) if info.flags.contains(ParamInfoFlags::IS_AUTOMATABLE) => {
                        automation = Support::Supported;
                        break;
                    }
                    Some(_) => {}
                    None => automation = Support::Unknown,
                }
            }
        }
        {
            let mut capabilities = Capabilities::default();
            capabilities.embedded_editor = embedded_editor;
            capabilities.bus_discovery = audio.is_some().into();
            capabilities.note_input = ports.note_input;
            capabilities.note_output = ports.note_output;
            capabilities.sample_accurate_automation = automation;
            capabilities.state = state.is_some().into();
            // Discovery uses a separate factory; the current instance query does not inspect it.
            capabilities.factory_presets = Support::Unknown;
            capabilities.f32 = ports.f32;
            capabilities.f64 = ports.f64;
            capabilities
        }
    }

    /// Scans the ports, which CLAP allows only while the plugin is inactive.
    pub(super) fn port_support(&mut self) -> PortSupport {
        let extensions = self.shared().extensions();
        let (audio, notes) = (extensions.audio_ports, extensions.note_ports);
        let handle = self.instance.plugin_handle();
        let note_support = |is_input| {
            let Some(notes) = notes else {
                return Support::Unsupported;
            };
            let count = notes.count(&handle, is_input);
            // Events address ports by a 16-bit index; the port list fails for more.
            if count > u32::from(u16::MAX) {
                return Support::Unknown;
            }
            let mut buffer = NotePortInfoBuffer::new();
            let mut support = Support::Unsupported;
            for index in 0..count {
                match notes.get(&handle, index, is_input, &mut buffer) {
                    Some(port)
                        if [NoteDialect::Clap, NoteDialect::Midi, NoteDialect::MidiMpe]
                            .iter()
                            .any(|&dialect| port.supported_dialects.contains(dialect.into())) =>
                    {
                        return Support::Supported;
                    }
                    Some(_) => {}
                    None => support = Support::Unknown,
                }
            }
            support
        };
        let (mut f32, mut f64) = (Support::Unsupported, Support::Unsupported);
        if let Some(audio) = audio {
            let mut buffer = AudioPortInfoBuffer::new();
            let mut seen = false;
            let mut missing = false;
            let mut all_f64 = true;
            for is_input in [true, false] {
                let count = audio.count(&handle, is_input);
                // The bus list fails for more.
                if count as usize > plughost_core::MAX_AUDIO_BUSES {
                    seen = true;
                    missing = true;
                    continue;
                }
                for index in 0..count {
                    seen = true;
                    match audio.get(&handle, index, is_input, &mut buffer) {
                        Some(port) => {
                            all_f64 &= port.flags.contains(AudioPortFlags::SUPPORTS_64BITS)
                        }
                        None => missing = true,
                    }
                }
            }
            if seen {
                // CLAP requires 32-bit support on audio ports; 64-bit is explicitly advertised.
                f32 = if missing {
                    Support::Unknown
                } else {
                    Support::Supported
                };
                f64 = if !all_f64 {
                    Support::Unsupported
                } else if missing {
                    Support::Unknown
                } else {
                    Support::Supported
                };
            }
        }
        PortSupport {
            note_input: note_support(true),
            note_output: note_support(false),
            f32,
            f64,
        }
    }
}
