use super::*;
use block2::RcBlock;

#[test]
fn sidechain_and_noncontiguous_outputs_keep_native_bus_identity() {
    // The only stub is AU's foreign render block. The host must route real buffer lists
    // through its pull callback and retain each output before the next render call.
    let native: RcBlock<Render> = RcBlock::new(
        move |flags: NonNull<AudioUnitRenderActionFlags>,
              time: NonNull<AudioTimeStamp>,
              frames: AUAudioFrameCount,
              output_bus: NSInteger,
              output: NonNull<AudioBufferList>,
              pull: *mut DynBlock<PullInput>| {
            let mut key = vec![0.0f32; frames as usize];
            let mut key_buffers = BufferList::new(1);
            key_buffers.point_at(&mut [&mut key], None);
            let status = unsafe {
                (*pull).call((
                    flags,
                    time,
                    frames,
                    2,
                    NonNull::new_unchecked(key_buffers.as_mut_ptr()),
                ))
            };
            if status != 0 {
                return status;
            }
            if output_bus == 3 {
                let buffers = unsafe { buffers_of(output.as_ptr()) };
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        key.as_ptr(),
                        buffers[0].mData.cast::<f32>(),
                        frames as usize,
                    )
                };
                return 0;
            }
            if output_bus != 0 {
                return kAudioUnitErr_NoConnection;
            }
            let status = unsafe { (*pull).call((flags, time, frames, 0, output)) };
            if status != 0 {
                return status;
            }
            for buffer in unsafe { buffers_of(output.as_ptr()) } {
                let samples = unsafe {
                    std::slice::from_raw_parts_mut(buffer.mData.cast::<f32>(), frames as usize)
                };
                for (sample, key) in samples.iter_mut().zip(&key) {
                    *sample *= 1.0 - key;
                }
            }
            0
        },
    );
    let inputs = [
        BusBuffer {
            index: 0,
            channels: 0..2,
            order: None,
        },
        BusBuffer {
            index: 2,
            channels: 2..3,
            order: None,
        },
    ];
    let outputs = [
        BusBuffer {
            index: 0,
            channels: 0..2,
            order: None,
        },
        BusBuffer {
            index: 3,
            channels: 2..3,
            order: None,
        },
    ];
    let left = [0.5f32; 4];
    let right = [0.8f32; 4];
    let key = [0.25f32; 4];
    let mut audio = [[0.0f32; 4]; 3];
    let mut result: Vec<&mut [f32]> = audio
        .iter_mut()
        .map(|channel| channel.as_mut_slice())
        .collect();
    let mut storage = Buffers::new(inputs.to_vec(), outputs.to_vec(), 4).unwrap();
    storage
        .render(
            RcBlock::as_ptr(&native),
            &[&left, &right, &key],
            &mut result,
            4,
            128.0,
        )
        .unwrap();
    assert_eq!(audio[0], [0.375; 4]);
    assert_eq!(audio[1], [0.6; 4]);
    assert_eq!(audio[2], key);
    assert_eq!(left, [0.5; 4]);
}

#[test]
fn repeated_partial_pulls_and_replaced_outputs_always_read_original_input() {
    let native: RcBlock<Render> = RcBlock::new(
        move |flags: NonNull<AudioUnitRenderActionFlags>,
              time: NonNull<AudioTimeStamp>,
              frames: AUAudioFrameCount,
              output_bus: NSInteger,
              output: NonNull<AudioBufferList>,
              pull: *mut DynBlock<PullInput>| {
            // The native caller can copy the block while rendering, but releases it before return.
            let pull = unsafe { RcBlock::copy(pull) }.unwrap();
            if frames > 1 {
                let mut partial_time = unsafe { *time.as_ref() };
                partial_time.mSampleTime += 1.0;
                unsafe { buffers_of(output.as_ptr())[0].mData = std::ptr::null_mut() };
                let status = pull.call((
                    flags,
                    NonNull::from(&mut partial_time),
                    frames - 1,
                    2,
                    output,
                ));
                if status != 0 {
                    return status;
                }
                let buffer = &unsafe { buffers_of(output.as_ptr()) }[0];
                unsafe {
                    std::slice::from_raw_parts_mut(buffer.mData.cast::<f32>(), frames as usize - 1)
                        .fill(99.0);
                }
            }
            // Return host-provided input scratch as native output, then modify it in place.
            unsafe { buffers_of(output.as_ptr())[0].mData = std::ptr::null_mut() };
            let status = pull.call((flags, time, frames, 2, output));
            if status != 0 {
                return status;
            }
            let buffer = &unsafe { buffers_of(output.as_ptr()) }[0];
            for sample in unsafe {
                std::slice::from_raw_parts_mut(buffer.mData.cast::<f32>(), frames as usize)
            } {
                *sample += if output_bus == 0 { 1.0 } else { 2.0 };
            }
            0
        },
    );
    let mut storage = Buffers::new(
        vec![BusBuffer {
            index: 2,
            channels: 0..1,
            order: None,
        }],
        vec![
            BusBuffer {
                index: 0,
                channels: 0..1,
                order: None,
            },
            BusBuffer {
                index: 3,
                channels: 1..2,
                order: None,
            },
        ],
        8,
    )
    .unwrap();
    for (iteration, frames) in [8, 1, 0, 7, 8].into_iter().enumerate() {
        let original: Vec<_> = (0..frames)
            .map(|frame| (iteration * 8 + frame) as f32 / 64.0)
            .collect();
        let mut left = vec![-99.0; frames];
        let mut right = vec![-99.0; frames];
        storage
            .render(
                RcBlock::as_ptr(&native),
                &[&original],
                &mut [&mut left, &mut right],
                frames,
                (iteration * 8) as f64,
            )
            .unwrap();
        for (frame, sample) in original.iter().enumerate() {
            assert_eq!(*sample, (iteration * 8 + frame) as f32 / 64.0);
            assert_eq!(left[frame], *sample + 1.0);
            assert_eq!(right[frame], *sample + 2.0);
        }
    }
}

#[test]
fn invalid_pulls_and_render_failures_leave_the_storage_reusable() {
    use std::{cell::Cell, rc::Rc};
    let calls = Rc::new(Cell::new(0));
    let observed = Rc::clone(&calls);
    let native: RcBlock<Render> = RcBlock::new(
        move |flags: NonNull<AudioUnitRenderActionFlags>,
              time: NonNull<AudioTimeStamp>,
              frames: AUAudioFrameCount,
              _output_bus: NSInteger,
              output: NonNull<AudioBufferList>,
              pull: *mut DynBlock<PullInput>| {
            let call = observed.get();
            observed.set(call + 1);
            if call == 0 {
                for (count, bus) in [(frames + 1, 2), (frames, -1), (frames, 1)] {
                    let status = unsafe { (*pull).call((flags, time, count, bus, output)) };
                    if status != kAudioUnitErr_NoConnection {
                        return -99;
                    }
                }
                // A valid foreign list with the wrong number of channels must also be rejected.
                let mut wrong = BufferList::new(2);
                let (mut left, mut right) = ([0.0; 4], [0.0; 4]);
                wrong.point_at(&mut [&mut left, &mut right], None);
                let status = unsafe {
                    (*pull).call((
                        flags,
                        time,
                        frames,
                        2,
                        NonNull::new_unchecked(wrong.as_mut_ptr()),
                    ))
                };
                if status != kAudioUnitErr_NoConnection {
                    return -99;
                }
                unsafe { buffers_of(output.as_ptr())[0].mNumberChannels = 2 };
                if unsafe { (*pull).call((flags, time, frames, 2, output)) }
                    != kAudioUnitErr_NoConnection
                {
                    return -99;
                }
                unsafe {
                    buffers_of(output.as_ptr())[0].mNumberChannels = 1;
                    buffers_of(output.as_ptr())[0].mDataByteSize = 1;
                }
                if unsafe { (*pull).call((flags, time, frames, 2, output)) }
                    != kAudioUnitErr_NoConnection
                {
                    return -99;
                }
                unsafe { (*output.as_ptr()).mNumberBuffers = 0 };
                return -77;
            }
            if call == 1 {
                unsafe { (*output.as_ptr()).mNumberBuffers = 0 };
                return 0;
            }
            if call == 2 {
                unsafe { buffers_of(output.as_ptr())[0].mDataByteSize = 1 };
                return 0;
            }
            if call == 3 {
                unsafe { buffers_of(output.as_ptr())[0].mData = std::ptr::null_mut() };
                return 0;
            }
            unsafe { (*pull).call((flags, time, frames, 2, output)) }
        },
    );
    let mut storage = Buffers::new(
        vec![BusBuffer {
            index: 2,
            channels: 0..1,
            order: None,
        }],
        vec![BusBuffer {
            index: 3,
            channels: 0..1,
            order: None,
        }],
        4,
    )
    .unwrap();
    let original = [1.0, 2.0, 3.0, 4.0];
    for expected in [
        "Err(Render(-77))",
        "Err(RenderOutput)",
        "Err(RenderOutput)",
        "Err(RenderOutput)",
        "Ok(())",
    ] {
        let mut output = [99.0; 4];
        let result = storage.render(
            RcBlock::as_ptr(&native),
            &[&original],
            &mut [&mut output],
            4,
            128.0,
        );
        assert_eq!(format!("{result:?}"), expected);
        assert_eq!(output, if result.is_ok() { original } else { [99.0; 4] });
        assert_eq!(original, [1.0, 2.0, 3.0, 4.0]);
    }
    assert_eq!(calls.get(), 5);
}

#[test]
fn pulls_receive_the_block_input_whatever_their_timestamp() {
    let native: RcBlock<Render> = RcBlock::new(
        move |flags: NonNull<AudioUnitRenderActionFlags>,
              _time: NonNull<AudioTimeStamp>,
              frames: AUAudioFrameCount,
              _bus: NSInteger,
              output: NonNull<AudioBufferList>,
              pull: *mut DynBlock<PullInput>| {
            // A unit that keeps its own sample time instead of passing the render timestamp on.
            let mut own: AudioTimeStamp = unsafe { std::mem::zeroed() };
            unsafe { (*pull).call((flags, NonNull::from(&mut own), frames, 0, output)) }
        },
    );
    let mut storage = Buffers::new(
        vec![BusBuffer {
            index: 0,
            channels: 0..1,
            order: None,
        }],
        vec![BusBuffer {
            index: 0,
            channels: 0..1,
            order: None,
        }],
        4,
    )
    .unwrap();
    let input = [1.0, 2.0, 3.0, 4.0];
    let mut output = [0.0; 4];
    storage
        .render(
            RcBlock::as_ptr(&native),
            &[&input],
            &mut [&mut output],
            4,
            4096.0,
        )
        .unwrap();
    assert_eq!(output, input);
}

#[test]
fn instruments_receive_no_pull_block() {
    let native: RcBlock<Render> = RcBlock::new(
        move |_flags: NonNull<AudioUnitRenderActionFlags>,
              _time: NonNull<AudioTimeStamp>,
              frames: AUAudioFrameCount,
              _bus: NSInteger,
              output: NonNull<AudioBufferList>,
              pull: *mut DynBlock<PullInput>| {
            if !pull.is_null() {
                return -99;
            }
            let buffer = &unsafe { buffers_of(output.as_ptr()) }[0];
            unsafe {
                std::slice::from_raw_parts_mut(buffer.mData.cast::<f32>(), frames as usize)
                    .fill(0.25);
            }
            0
        },
    );
    let mut storage = Buffers::new(
        vec![],
        vec![BusBuffer {
            index: 0,
            channels: 0..1,
            order: None,
        }],
        8,
    )
    .unwrap();
    let mut output = [0.0; 8];
    storage
        .render(RcBlock::as_ptr(&native), &[], &mut [&mut output], 8, 0.0)
        .unwrap();
    assert_eq!(output, [0.25; 8]);
}

#[test]
fn native_channel_order_maps_each_portable_channel_both_ways() {
    // The unit adds 100 times the native channel index to each channel it pulls.
    let native: RcBlock<Render> = RcBlock::new(
        move |flags: NonNull<AudioUnitRenderActionFlags>,
              time: NonNull<AudioTimeStamp>,
              frames: AUAudioFrameCount,
              _bus: NSInteger,
              output: NonNull<AudioBufferList>,
              pull: *mut DynBlock<PullInput>| {
            let status = unsafe { (*pull).call((flags, time, frames, 0, output)) };
            if status != 0 {
                return status;
            }
            for (native, buffer) in unsafe { buffers_of(output.as_ptr()) }.iter().enumerate() {
                for sample in unsafe {
                    std::slice::from_raw_parts_mut(buffer.mData.cast::<f32>(), frames as usize)
                } {
                    *sample += 100.0 * native as f32;
                }
            }
            0
        },
    );
    let order: &'static [usize] = &[0, 1, 5, 6, 2, 3, 4];
    let bus = BusBuffer {
        index: 0,
        channels: 0..order.len(),
        order: Some(order),
    };
    let mut storage = Buffers::new(vec![bus.clone()], vec![bus], 4).unwrap();
    let input: Vec<Vec<f32>> = (0..order.len())
        .map(|channel| vec![channel as f32; 4])
        .collect();
    let inputs: Vec<&[f32]> = input.iter().map(Vec::as_slice).collect();
    let mut output = vec![vec![0.0f32; 4]; order.len()];
    let mut outputs: Vec<&mut [f32]> = output.iter_mut().map(Vec::as_mut_slice).collect();
    storage
        .render(RcBlock::as_ptr(&native), &inputs, &mut outputs, 4, 0.0)
        .unwrap();
    for (native, &portable) in order.iter().enumerate() {
        assert_eq!(
            output[portable],
            vec![portable as f32 + 100.0 * native as f32; 4],
            "portable channel {portable}"
        );
    }
}
