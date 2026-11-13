//! Prepared CLAP sample storage and pointer tables. No caller-owned pointer is retained.
use std::any::Any;

use clap_sys::audio_buffer::clap_audio_buffer;
use plughost_core::{AudioBusInfo, AudioDirection, Sample, SampleFormat, Support};

use super::ClapError;

pub(super) enum AudioBuffers {
    F32(Buffers<f32>),
    F64(Buffers<f64>),
}
impl AudioBuffers {
    pub fn new(
        buses: &[AudioBusInfo],
        format: SampleFormat,
        frames: usize,
    ) -> Result<Self, ClapError> {
        Ok(match format {
            SampleFormat::F32 => Self::F32(Buffers::new(buses, frames)?),
            SampleFormat::F64 => Self::F64(Buffers::new(buses, frames)?),
            _ => {
                return Err(ClapError::Input(
                    plughost_core::InputError::AudioSampleFormat,
                ));
            }
        })
    }
    pub fn typed<S: Sample>(&mut self) -> Result<&mut Buffers<S>, ClapError> {
        let storage: &mut dyn Any = match self {
            Self::F32(storage) => storage,
            Self::F64(storage) => storage,
        };
        storage.downcast_mut().ok_or(ClapError::Input(
            plughost_core::InputError::AudioSampleFormat,
        ))
    }
    pub fn active_channels(&self) -> (usize, usize) {
        match self {
            Self::F32(storage) => (
                storage.inputs.active_channels,
                storage.outputs.active_channels,
            ),
            Self::F64(storage) => (
                storage.inputs.active_channels,
                storage.outputs.active_channels,
            ),
        }
    }
}
pub(super) struct Buffers<S> {
    pub inputs: NativeBuffers<S>,
    pub outputs: NativeBuffers<S>,
}
impl<S: Sample> Buffers<S> {
    fn new(buses: &[AudioBusInfo], frames: usize) -> Result<Self, ClapError> {
        Ok(Self {
            inputs: NativeBuffers::new(buses, AudioDirection::Input, frames)?,
            outputs: NativeBuffers::new(buses, AudioDirection::Output, frames)?,
        })
    }
}
pub(super) struct NativeBuffers<S> {
    ports: Vec<Port<S>>,
    pub raw: Vec<clap_audio_buffer>,
    active_channels: usize,
}
struct Port<S> {
    samples: Vec<Vec<S>>,
    silence32: Vec<Vec<f32>>,
    pointers: Vec<*mut S>,
    pointers32: Vec<*mut f32>,
    active: bool,
}
// SAFETY: every pointer refers to storage owned by these vectors, never to caller memory.
// Moving the owner preserves the allocations. The engine mutex serializes access, and native
// calls are synchronous; no native pointer is used after deactivation drops this storage.
unsafe impl<S: Send> Send for NativeBuffers<S> {}

fn channels<S: Sample>(count: usize, frames: usize) -> Result<Vec<Vec<S>>, ClapError> {
    (0..count)
        .map(|_| {
            let mut samples = Vec::new();
            samples
                .try_reserve_exact(frames)
                .map_err(|_| ClapError::ProcessingStorage)?;
            Ok(samples)
        })
        .collect()
}
impl<S: Sample> NativeBuffers<S> {
    fn new(
        buses: &[AudioBusInfo],
        direction: AudioDirection,
        frames: usize,
    ) -> Result<Self, ClapError> {
        let mut active_channels = 0;
        let ports = buses
            .iter()
            .filter(|bus| bus.direction == direction)
            .map(|bus| {
                let count = bus.channels as usize;
                let active = bus.active != Some(false);
                if active {
                    active_channels += count;
                }
                let use_silence32 =
                    !active && S::FORMAT == SampleFormat::F64 && bus.f64 != Support::Supported;
                Ok(Port {
                    samples: channels(count, frames)?,
                    silence32: channels(if use_silence32 { count } else { 0 }, frames)?,
                    pointers: vec![std::ptr::null_mut(); count],
                    pointers32: vec![std::ptr::null_mut(); if use_silence32 { count } else { 0 }],
                    active,
                })
            })
            .collect::<Result<Vec<_>, ClapError>>()?;
        let raw = ports
            .iter()
            .map(|port| clap_audio_buffer {
                data32: std::ptr::null_mut(),
                data64: std::ptr::null_mut(),
                channel_count: port.pointers.len() as u32,
                latency: 0,
                constant_mask: 0,
            })
            .collect();
        Ok(Self {
            ports,
            raw,
            active_channels,
        })
    }
    /// Reset the current block, including native-writable metadata and inactive f32 storage.
    pub fn begin(&mut self, frames: usize) {
        for (port, raw) in self.ports.iter_mut().zip(&mut self.raw) {
            for (samples, pointer) in port.samples.iter_mut().zip(&mut port.pointers) {
                samples.resize(frames, S::default());
                samples.fill(S::default());
                *pointer = samples.as_mut_ptr();
            }
            for (samples, pointer) in port.silence32.iter_mut().zip(&mut port.pointers32) {
                samples.resize(frames, 0.0);
                samples.fill(0.0);
                *pointer = samples.as_mut_ptr();
            }
            let use32 = S::FORMAT == SampleFormat::F32 || !port.silence32.is_empty();
            *raw = clap_audio_buffer {
                data32: if use32 {
                    if port.silence32.is_empty() {
                        port.pointers.as_mut_ptr().cast()
                    } else {
                        port.pointers32.as_mut_ptr()
                    }
                } else {
                    std::ptr::null_mut()
                },
                data64: if use32 {
                    std::ptr::null_mut()
                } else {
                    port.pointers.as_mut_ptr().cast()
                },
                channel_count: port.pointers.len() as u32,
                latency: 0,
                constant_mask: if port.active { 0 } else { u64::MAX },
            };
        }
    }
    pub fn copy_input(&mut self, input: &[&[S]], main: Option<usize>, all: bool) {
        let mut offset = 0;
        for (index, port) in self.ports.iter_mut().enumerate() {
            if (all && port.active) || (!all && main == Some(index)) {
                for channel in &mut port.samples {
                    channel.copy_from_slice(input[offset]);
                    offset += 1;
                }
            } else {
                self.raw[index].constant_mask = u64::MAX;
            }
        }
    }
    pub fn copy_output(&self, output: &mut [&mut [S]], main: Option<usize>, all: bool) {
        let mut offset = 0;
        for (index, port) in self.ports.iter().enumerate() {
            if (all && port.active) || (!all && main == Some(index)) {
                for channel in &port.samples {
                    output[offset].copy_from_slice(channel);
                    offset += 1;
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "buffers_tests.rs"]
mod tests;
