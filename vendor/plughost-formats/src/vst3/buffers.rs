//! Prepared native bus storage. Caller-owned output pointers live only during a process call.
use std::any::Any;
use std::ops::Range;

use plughost_core::{AudioBusInfo, AudioDirection, Sample, SampleFormat};
use vst3::Steinberg::Vst::{AudioBusBuffers, AudioBusBuffers__type0};

use super::errors::Vst3Error;

pub(super) enum AudioBuffers {
    F32(Buffers<f32>),
    F64(Buffers<f64>),
}
impl AudioBuffers {
    pub fn new(
        buses: &[AudioBusInfo],
        format: SampleFormat,
        frames: usize,
    ) -> Result<Self, Vst3Error> {
        Ok(match format {
            SampleFormat::F32 => Self::F32(Buffers::new(buses, frames)?),
            SampleFormat::F64 => Self::F64(Buffers::new(buses, frames)?),
            _ => {
                return Err(Vst3Error::Input(
                    plughost_core::InputError::AudioSampleFormat,
                ));
            }
        })
    }
    /// The engine validates the type, channel counts, and block length before binding.
    pub fn bind<S: Sample>(
        &mut self,
        input: &[&[S]],
        output: &mut [&mut [S]],
        range: Range<usize>,
    ) -> Result<(), Vst3Error> {
        let storage: &mut dyn Any = match self {
            Self::F32(storage) => storage,
            Self::F64(storage) => storage,
        };
        let storage = storage
            .downcast_mut::<Buffers<S>>()
            .ok_or(Vst3Error::Input(
                plughost_core::InputError::AudioSampleFormat,
            ))?;
        storage.inputs.bind(input, &mut [], range.clone());
        storage.outputs.bind(&[], output, range);
        Ok(())
    }
    pub fn raw(&mut self) -> (&mut [AudioBusBuffers], &mut [AudioBusBuffers]) {
        match self {
            Self::F32(storage) => (&mut storage.inputs.raw, &mut storage.outputs.raw),
            Self::F64(storage) => (&mut storage.inputs.raw, &mut storage.outputs.raw),
        }
    }
    pub fn clear(&mut self) {
        match self {
            Self::F32(storage) => {
                storage.inputs.clear();
                storage.outputs.clear();
            }
            Self::F64(storage) => {
                storage.inputs.clear();
                storage.outputs.clear();
            }
        }
    }
    pub fn channels(&self) -> (usize, usize) {
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
    inputs: Direction<S>,
    outputs: Direction<S>,
}
impl<S: Sample> Buffers<S> {
    fn new(buses: &[AudioBusInfo], frames: usize) -> Result<Self, Vst3Error> {
        Ok(Self {
            inputs: Direction::new(buses, AudioDirection::Input, frames)?,
            outputs: Direction::new(buses, AudioDirection::Output, frames)?,
        })
    }
}
struct Direction<S> {
    input: bool,
    buses: Vec<Bus<S>>,
    raw: Vec<AudioBusBuffers>,
    active_channels: usize,
}
struct Bus<S> {
    active: bool,
    samples: Vec<Vec<S>>,
    pointers: Vec<*mut S>,
}
// SAFETY: sample storage is owned and S is Send. Raw pointers are populated only while the
// engine is exclusively borrowed under its mutex, used synchronously, and cleared before return.
// Moving a Direction never accesses or changes the allocation addresses behind its vectors.
unsafe impl<S: Send> Send for Direction<S> {}

impl<S: Sample> Direction<S> {
    fn new(
        buses: &[AudioBusInfo],
        direction: AudioDirection,
        frames: usize,
    ) -> Result<Self, Vst3Error> {
        let input = direction == AudioDirection::Input;
        let mut active_channels = 0;
        let buses = buses
            .iter()
            .filter(|bus| bus.direction == direction)
            .map(|bus| {
                let active = bus.active == Some(true);
                let count = bus.channels as usize;
                if active {
                    active_channels += count;
                }
                let samples = (0..if input || !active { count } else { 0 })
                    .map(|_| {
                        let mut samples = Vec::new();
                        samples
                            .try_reserve_exact(frames)
                            .map_err(|_| Vst3Error::ProcessingStorage)?;
                        Ok(samples)
                    })
                    .collect::<Result<_, Vst3Error>>()?;
                Ok(Bus {
                    active,
                    samples,
                    pointers: vec![std::ptr::null_mut(); count],
                })
            })
            .collect::<Result<Vec<_>, Vst3Error>>()?;
        let raw = buses
            .iter()
            .map(|bus| bus_buffers::<S>(bus.pointers.len(), std::ptr::null_mut(), false))
            .collect();
        Ok(Self {
            input,
            buses,
            raw,
            active_channels,
        })
    }
    fn bind(&mut self, input: &[&[S]], output: &mut [&mut [S]], range: Range<usize>) {
        let frames = range.len();
        let mut offset = 0;
        for (bus, raw) in self.buses.iter_mut().zip(&mut self.raw) {
            if self.input || !bus.active {
                for (channel, pointer) in bus.samples.iter_mut().zip(&mut bus.pointers) {
                    channel.resize(frames, S::default());
                    if self.input && bus.active {
                        channel.copy_from_slice(&input[offset][range.clone()]);
                        offset += 1;
                    } else {
                        // Plugins may have written into inactive input or discarded output storage.
                        channel.fill(S::default());
                    }
                    *pointer = channel.as_mut_ptr();
                }
            } else {
                for pointer in &mut bus.pointers {
                    *pointer = output[offset][range.clone()].as_mut_ptr();
                    offset += 1;
                }
            }
            // Reset metadata as well as pointers: the plugin may change output silence flags.
            *raw = bus_buffers::<S>(
                bus.pointers.len(),
                bus.pointers.as_mut_ptr(),
                self.input && !bus.active,
            );
        }
    }
    fn clear(&mut self) {
        for (bus, raw) in self.buses.iter_mut().zip(&mut self.raw) {
            bus.pointers.fill(std::ptr::null_mut());
            *raw = bus_buffers::<S>(bus.pointers.len(), std::ptr::null_mut(), false);
        }
    }
}
fn bus_buffers<S: Sample>(count: usize, pointers: *mut *mut S, silent: bool) -> AudioBusBuffers {
    AudioBusBuffers {
        numChannels: count as i32,
        silenceFlags: if silent { u64::MAX } else { 0 },
        // Only f32 and f64 are samples; preparing refuses other formats.
        __field0: match S::FORMAT {
            SampleFormat::F64 => AudioBusBuffers__type0 {
                channelBuffers64: pointers.cast(),
            },
            _ => AudioBusBuffers__type0 {
                channelBuffers32: pointers.cast(),
            },
        },
    }
}

#[cfg(test)]
#[path = "buffers_tests.rs"]
mod tests;
