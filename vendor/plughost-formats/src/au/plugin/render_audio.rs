//! Prepared AU buffers with a call-scoped pull block for immutable caller input.
use std::ptr::NonNull;

use block2::{DynBlock, RcBlock, StackBlock};
use objc2_audio_toolbox::{
    AUAudioFrameCount, AudioUnitRenderActionFlags, kAudioUnitErr_NoConnection,
};
use objc2_core_audio_types::{AudioBufferList, AudioTimeStamp, AudioTimeStampFlags};
use objc2_foundation::NSInteger;

use super::super::buffers::{BufferList, buffers_of};
use super::audio::BusBuffer;
use super::{AuError, Prepared, PullInput, Render};

pub(super) struct Buffers {
    input_buses: Vec<Option<BusBuffer>>,
    samples: Vec<Vec<f32>>,
    sources: Vec<*mut f32>,
    outputs: Vec<(BusBuffer, BufferList)>,
}
impl Buffers {
    pub fn new(
        inputs: Vec<BusBuffer>,
        outputs: Vec<BusBuffer>,
        maximum: usize,
    ) -> Result<Self, AuError> {
        let channels = inputs.iter().map(|bus| bus.channels.len()).sum();
        let mut input_buses = vec![None; inputs.last().map_or(0, |bus| bus.index + 1)];
        for bus in inputs {
            let index = bus.index;
            input_buses[index] = Some(bus);
        }
        let samples = (0..channels)
            .map(|_| {
                let mut samples = Vec::new();
                samples
                    .try_reserve_exact(maximum)
                    .map_err(|_| AuError::ProcessingStorage)?;
                Ok(samples)
            })
            .collect::<Result<_, AuError>>()?;
        Ok(Self {
            input_buses,
            samples,
            sources: vec![std::ptr::null_mut(); channels],
            outputs: outputs
                .into_iter()
                .map(|bus| {
                    let list = BufferList::new(bus.channels.len());
                    (bus, list)
                })
                .collect(),
        })
    }
    fn render(
        &mut self,
        render: *mut DynBlock<Render>,
        input: &[&[f32]],
        output: &mut [&mut [f32]],
        frames: usize,
        position: f64,
    ) -> Result<(), AuError> {
        for (samples, source) in self.samples.iter_mut().zip(&mut self.sources) {
            samples.resize(frames, 0.0);
            *source = samples.as_mut_ptr();
        }
        let input_buses = &self.input_buses;
        let sources = &self.sources;
        // This block and its borrowed caller input live through every synchronous render call.
        // Every pull refills writable scratch from immutable input, including repeated pulls
        // after another output has modified its input buffers in place. Like other hosts, it
        // serves the block's input from its start whatever timestamp the unit passes.
        let pull = StackBlock::new(
            move |_flags: NonNull<AudioUnitRenderActionFlags>,
                  _time: NonNull<AudioTimeStamp>,
                  count: AUAudioFrameCount,
                  bus: NSInteger,
                  list: NonNull<AudioBufferList>| {
                let _audit = plughost_core::audit::callback();
                let Some(input_bus) = usize::try_from(bus)
                    .ok()
                    .and_then(|index| input_buses.get(index))
                    .and_then(Option::as_ref)
                else {
                    return kAudioUnitErr_NoConnection;
                };
                if count as usize > frames {
                    return kAudioUnitErr_NoConnection;
                }
                let buffers = unsafe { buffers_of(list.as_ptr()) };
                if buffers.len() != input_bus.channels.len()
                    || buffers.iter().any(|buffer| buffer.mNumberChannels != 1)
                {
                    return kAudioUnitErr_NoConnection;
                }
                let bytes = count.saturating_mul(size_of::<f32>() as u32);
                if buffers
                    .iter()
                    .any(|buffer| !buffer.mData.is_null() && buffer.mDataByteSize < bytes)
                {
                    return kAudioUnitErr_NoConnection;
                }
                for (native, buffer) in buffers.iter_mut().enumerate() {
                    let index = input_bus.channels.start + input_bus.portable(native);
                    // SAFETY: count fits this block. Scratch is owned and reserved for the maximum
                    // block; caller input stays immutable through the native call.
                    let source = input[index].as_ptr();
                    if buffer.mData.is_null() {
                        buffer.mData = sources[index].cast();
                    }
                    unsafe { std::ptr::copy(source, buffer.mData.cast::<f32>(), count as usize) };
                    buffer.mDataByteSize = bytes;
                }
                0
            },
        );
        // The FFI block type erases the borrow lifetime. AU must only pull synchronously within
        // this render call, just as it must not retain the caller's input/output sample pointers.
        let pull_ptr = if input.is_empty() {
            std::ptr::null_mut()
        } else {
            NonNull::from(&*pull).as_ptr().cast::<DynBlock<PullInput>>()
        };
        for (bus, list) in &mut self.outputs {
            let channels = &mut output[bus.channels.clone()];
            list.point_at(channels, bus.order);
            let result = render_output(render, bus, list, channels, frames, position, pull_ptr);
            // Native may replace the list's pointers or header, including on failure.
            list.clear();
            result?;
        }
        Ok(())
    }
}
pub(super) fn render(
    prepared: &mut Prepared,
    input: &[&[f32]],
    output: &mut [&mut [f32]],
    frames: usize,
    position: f64,
) -> Result<(), AuError> {
    prepared.buffers.render(
        RcBlock::as_ptr(&prepared.render),
        input,
        output,
        frames,
        position,
    )
}
fn render_output(
    render: *mut DynBlock<Render>,
    bus: &BusBuffer,
    list: &mut BufferList,
    channels: &mut [&mut [f32]],
    frames: usize,
    position: f64,
    pull: *mut DynBlock<PullInput>,
) -> Result<(), AuError> {
    let mut flags = AudioUnitRenderActionFlags::empty();
    let mut time: AudioTimeStamp = unsafe { std::mem::zeroed() };
    time.mSampleTime = position;
    time.mFlags = AudioTimeStampFlags::SampleTimeValid;
    let status = unsafe {
        let _audit = plughost_core::audit::plugin();
        let _floats = crate::float_mode::FlushToZero::enter();
        (*render).call((
            NonNull::from(&mut flags),
            NonNull::from(&mut time),
            frames as AUAudioFrameCount,
            bus.index as NSInteger,
            NonNull::new_unchecked(list.as_mut_ptr()),
            pull,
        ))
    };
    if status != 0 {
        return Err(AuError::Render(status));
    }
    let bytes = frames
        .checked_mul(size_of::<f32>())
        .ok_or(AuError::RenderOutput)?;
    if unsafe { (*list.as_mut_ptr()).mNumberBuffers } as usize != channels.len() {
        return Err(AuError::RenderOutput);
    }
    for (native, buffer) in list.buffers().iter().enumerate() {
        let channel = &mut channels[bus.portable(native)];
        if buffer.mNumberChannels != 1
            || (buffer.mDataByteSize as usize) < bytes
            || buffer.mData.is_null()
        {
            return Err(AuError::RenderOutput);
        }
        let rendered = buffer.mData.cast::<f32>();
        if rendered != channel.as_mut_ptr() {
            unsafe { std::ptr::copy(rendered, channel.as_mut_ptr(), frames) };
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "render_audio_tests.rs"]
mod tests;
