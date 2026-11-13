use super::*;
use plughost_core::{AudioBusRole, Layout};

fn buses() -> Vec<AudioBusInfo> {
    [AudioDirection::Input, AudioDirection::Output]
        .into_iter()
        .flat_map(|direction| {
            (0..2).map(move |index| {
                let mut audio_bus_info = AudioBusInfo::new(
                    u64::from(index) + 20,
                    index,
                    String::new(),
                    direction,
                    if index == 1 {
                        AudioBusRole::Main
                    } else {
                        AudioBusRole::Auxiliary
                    },
                    1,
                );
                audio_bus_info.layout = Some(Layout::Mono);
                audio_bus_info.active = Some(index == 1);
                audio_bus_info.f32 = Support::Supported;
                audio_bus_info.f64 = if index == 0 {
                    Support::Unsupported
                } else {
                    Support::Supported
                };
                audio_bus_info
            })
        })
        .collect()
}

#[test]
fn native_writes_and_short_blocks_do_not_leak_into_reused_audio() {
    verify(1.25f32);
    verify(1.0 + f64::EPSILON);
}
fn verify<S: Sample + PartialEq + std::fmt::Debug>(value: S) {
    let mut storage = AudioBuffers::new(&buses(), S::FORMAT, 8).unwrap();
    assert_eq!(storage.active_channels(), (1, 1));
    let buffers = storage.typed::<S>().unwrap();
    for frames in [8, 1, 0, 7, 8] {
        let input = vec![value; frames];
        let mut output = vec![value; frames];
        buffers.inputs.begin(frames);
        buffers.outputs.begin(frames);
        buffers.inputs.copy_input(&[&input], Some(1), true);
        // Only the foreign boundary is simulated. Inspect and modify real host-owned arrays
        // through the CLAP ABI, including the inactive f32 ports in an f64 configuration.
        unsafe {
            for raw in [&mut buffers.inputs.raw[0], &mut buffers.outputs.raw[0]] {
                assert_eq!(raw.channel_count, 1);
                assert_eq!(raw.constant_mask, u64::MAX);
                assert!(raw.data64.is_null());
                let samples = std::slice::from_raw_parts_mut(*raw.data32, frames);
                assert!(samples.iter().all(|sample| *sample == 0.0));
                samples.fill(99.0);
            }
            let samples = |raw: &clap_audio_buffer| {
                assert_eq!(raw.channel_count, 1);
                assert_eq!(raw.constant_mask, 0);
                assert_eq!(raw.latency, 0);
                let pointers = if S::FORMAT == SampleFormat::F32 {
                    assert!(raw.data64.is_null());
                    raw.data32.cast::<*mut S>()
                } else {
                    assert!(raw.data32.is_null());
                    raw.data64.cast::<*mut S>()
                };
                std::slice::from_raw_parts_mut(*pointers, frames)
            };
            let source = samples(&buffers.inputs.raw[1]);
            assert_eq!(source, input);
            let target = samples(&buffers.outputs.raw[1]);
            assert!(target.iter().all(|sample| *sample == S::default()));
            if frames != 0 {
                target[0] = source[0];
            }
            source.fill(S::default());
            buffers
                .outputs
                .copy_output(&mut [&mut output], Some(1), true);
            // A partial write on the next call must not reuse any of these values.
            samples(&buffers.outputs.raw[1]).fill(value);
        }
        for raw in [&mut buffers.inputs.raw[1], &mut buffers.outputs.raw[1]] {
            raw.constant_mask = u64::MAX;
            raw.latency = 123;
            raw.channel_count = 0;
            raw.data32 = std::ptr::null_mut();
            raw.data64 = std::ptr::null_mut();
        }
        assert_eq!(input, vec![value; frames]);
        if frames != 0 {
            assert_eq!(output[0], value);
            assert!(output[1..].iter().all(|sample| *sample == S::default()));
        }
    }
}

#[test]
fn switching_between_main_and_all_ports_restores_silence_masks_and_input_data() {
    let mut buses = buses();
    for bus in &mut buses {
        bus.active = Some(true);
    }
    let mut storage = AudioBuffers::new(&buses, SampleFormat::F32, 8).unwrap();
    let input = &mut storage.typed::<f32>().unwrap().inputs;
    for all in [true, false, true, false] {
        input.begin(8);
        if all {
            input.copy_input(&[&[0.25; 8], &[1.0; 8]], Some(1), true);
        } else {
            input.copy_input(&[&[1.0; 8]], Some(1), false);
        }
        unsafe {
            let key = std::slice::from_raw_parts(*input.raw[0].data32, 8);
            assert_eq!(key, if all { &[0.25; 8] } else { &[0.0; 8] });
        }
        assert_eq!(input.raw[0].constant_mask, if all { 0 } else { u64::MAX });
    }
}

#[test]
fn empty_ports_need_no_samples() {
    let mut empty = AudioBuffers::new(&[], SampleFormat::F64, 8).unwrap();
    assert_eq!(empty.active_channels(), (0, 0));
    let storage = empty.typed::<f64>().unwrap();
    storage.inputs.begin(8);
    storage.outputs.begin(8);
    assert!(storage.inputs.raw.is_empty() && storage.outputs.raw.is_empty());
}
