use super::*;
use plughost_core::{AudioBusRole, Layout, Support};

fn buses() -> Vec<AudioBusInfo> {
    [AudioDirection::Input, AudioDirection::Output]
        .into_iter()
        .flat_map(|direction| {
            (0..3).map(move |index| {
                let mut audio_bus_info = AudioBusInfo::new(
                    u64::from(index),
                    index,
                    String::new(),
                    direction,
                    if index == 1 {
                        AudioBusRole::Main
                    } else {
                        AudioBusRole::Auxiliary
                    },
                    if index == 2 { 0 } else { 1 },
                );
                audio_bus_info.layout = Some(if index == 2 {
                    Layout::None
                } else {
                    Layout::Mono
                });
                audio_bus_info.active = Some(index == 1);
                audio_bus_info.f32 = Support::Supported;
                audio_bus_info.f64 = Support::Supported;
                audio_bus_info
            })
        })
        .collect()
}

#[test]
fn native_writes_cannot_contaminate_inputs_or_later_blocks() {
    verify(1.25f32);
    verify(1.0 + f64::EPSILON);
}
fn verify<S: Sample + PartialEq + std::fmt::Debug>(value: S) {
    let mut buffers = AudioBuffers::new(&buses(), S::FORMAT, 8).unwrap();
    assert_eq!(buffers.channels(), (1, 1));
    for (start, frames) in [(0, 8), (3, 1), (2, 0), (1, 7), (0, 8)] {
        let input = vec![value; start + frames + 2];
        let mut output = vec![S::default(); input.len()];
        buffers
            .bind(&[&input], &mut [&mut output], start..start + frames)
            .unwrap();
        let (inputs, outputs) = buffers.raw();
        assert_eq!(inputs.len(), 3);
        assert_eq!(outputs.len(), 3);
        assert_eq!(inputs[0].silenceFlags, u64::MAX);
        assert_eq!(inputs[1].silenceFlags, 0);
        assert_eq!(inputs[2].numChannels, 0);
        assert_eq!(outputs[2].numChannels, 0);
        assert_eq!(outputs[1].silenceFlags, 0);
        // The simulated foreign boundary may write through every native sample pointer,
        // alter output flags, and overwrite input memory after reading it.
        unsafe {
            let samples = |bus: &mut AudioBusBuffers| {
                assert_eq!(bus.numChannels, 1);
                let channel = *bus.__field0.channelBuffers32.cast::<*mut S>();
                std::slice::from_raw_parts_mut(channel, frames)
            };
            let silent = samples(&mut inputs[0]);
            assert!(silent.iter().all(|sample| *sample == S::default()));
            silent.fill(value);
            let discard = samples(&mut outputs[0]);
            assert!(discard.iter().all(|sample| *sample == S::default()));
            discard.fill(value);
            let source = samples(&mut inputs[1]);
            assert_eq!(source, &input[start..start + frames]);
            samples(&mut outputs[1]).copy_from_slice(source);
            source.fill(S::default());
        }
        outputs[1].silenceFlags = u64::MAX;
        outputs[1].numChannels = 0;
        buffers.clear();
        assert_eq!(input, vec![value; start + frames + 2]);
        assert_eq!(
            &output[start..start + frames],
            &input[start..start + frames]
        );
        assert!(output[..start].iter().all(|sample| *sample == S::default()));
        assert!(
            output[start + frames..]
                .iter()
                .all(|sample| *sample == S::default())
        );
    }
}

#[test]
fn impossible_native_storage_reservation_is_a_configuration_failure() {
    let error = AudioBuffers::new(&buses(), SampleFormat::F64, usize::MAX)
        .err()
        .unwrap();
    assert!(matches!(error, Vst3Error::ProcessingStorage));
    assert_eq!(error.kind(), plughost_core::FailureKind::Configuration);
}
