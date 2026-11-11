//! AU bus negotiation. Channel counts are constraints, not speaker arrangements.
use std::ops::Range;
use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::{AnyThread, msg_send};
use objc2_audio_toolbox::{
    AUAudioUnit, AUAudioUnitBus, AUAudioUnitBusArray, kAudioUnitType_Generator,
    kAudioUnitType_MIDIProcessor, kAudioUnitType_MusicDevice, kAudioUnitType_MusicEffect,
};
use objc2_avf_audio::{AVAudioChannelLayout, AVAudioFormat};
use objc2_core_audio_types::{
    kAudioChannelLayoutTag_Atmos_5_1_2 as SURROUND_512,
    kAudioChannelLayoutTag_Atmos_5_1_4 as SURROUND_514,
    kAudioChannelLayoutTag_Atmos_7_1_2 as SURROUND_712,
    kAudioChannelLayoutTag_Atmos_7_1_4 as SURROUND_714,
    kAudioChannelLayoutTag_Atmos_9_1_6 as SURROUND_916,
    kAudioChannelLayoutTag_AudioUnit_7_0 as SURROUND_70,
    kAudioChannelLayoutTag_DiscreteInOrder as DISCRETE_IN_ORDER,
    kAudioChannelLayoutTag_HOA_ACN_SN3D as HOA_ACN_SN3D, kAudioChannelLayoutTag_MPEG_3_0_A as LCR,
    kAudioChannelLayoutTag_MPEG_5_0_A as SURROUND_50, kAudioChannelLayoutTag_Mono as MONO,
    kAudioChannelLayoutTag_Quadraphonic as QUAD, kAudioChannelLayoutTag_Stereo as STEREO,
    kAudioChannelLayoutTag_WAVE_5_1_A as SURROUND_51,
    kAudioChannelLayoutTag_WAVE_7_1 as SURROUND_71,
};
use objc2_foundation::NSError;
use plughost_core::{
    AudioBusConfig, AudioBusInfo, AudioBusRole, AudioConfig, AudioConfiguration, AudioDirection,
    Layout, ProcessConfig, Support,
};

use super::{AuError, Plugin, describe, lock};

#[derive(Clone)]
pub(super) struct BusBuffer {
    pub index: usize,
    pub channels: Range<usize>,
    /// The portable channel of each native channel, when the orders differ.
    pub order: Option<&'static [usize]>,
}

impl BusBuffer {
    /// The portable channel, within this bus, of native channel `native`.
    pub fn portable(&self, native: usize) -> usize {
        self.order.map_or(native, |order| order[native])
    }
}

impl Plugin {
    pub(crate) fn audio_buses(&self) -> Result<Vec<AudioBusInfo>, AuError> {
        let unit = self.shared_unit()?;
        let config = lock(&self.engine)
            .prepared
            .as_ref()
            .map(|prepared| prepared.config.clone());
        buses(&unit, config.as_ref())
    }

    /// AU reports channel-count constraints, not enumerated configuration identities.
    pub(crate) fn audio_configurations(&self) -> Result<Vec<AudioConfiguration>, AuError> {
        let engine = lock(&self.engine);
        engine.unit()?;
        Ok(Vec::new())
    }

    /// The unit's MIDI input, which instruments, music effects and MIDI processors have, and
    /// its MIDI outputs, one per cable. Output IDs are cable numbers.
    /// The MIDI input and the MIDI output cables. Units take MIDI 1.0 alone, so expressions other
    /// than pressure do not reach them; a unit declares whether it takes MPE.
    pub(crate) fn event_ports(&self) -> Result<Vec<plughost_core::EventPortInfo>, AuError> {
        let unit = self.shared_unit()?;
        let mpe = Support::from(unsafe { unit.supportsMPE() });
        let port = |index: u32, name: String, direction| {
            let mut event_port_info =
                plughost_core::EventPortInfo::new(u64::from(index), index, name, direction);
            event_port_info.midi = Support::Supported;
            event_port_info.sysex = Support::Supported;
            event_port_info.note_expression = Support::Unsupported;
            event_port_info.mpe = match direction {
                AudioDirection::Input => mpe,
                _ => Support::Unknown,
            };
            event_port_info
        };
        let outputs = midi_output_names(&unit);
        Ok(self
            .midi_input()?
            .then(|| port(0, String::new(), AudioDirection::Input))
            .into_iter()
            .chain(
                outputs
                    .into_iter()
                    .zip(0..)
                    .map(|(name, index)| port(index, name, AudioDirection::Output)),
            )
            .collect())
    }

    pub(super) fn midi_input(&self) -> Result<bool, AuError> {
        let kind = super::super::components::parse_class_id(self.info.id.class_id().as_str())?
            .componentType;
        Ok([
            kAudioUnitType_MusicDevice,
            kAudioUnitType_MusicEffect,
            kAudioUnitType_MIDIProcessor,
        ]
        .contains(&kind))
    }

    /// Negotiates every native bus and reserves host audio storage for the maximum block size.
    /// Failure after deallocation leaves this instance unprepared; callers needing atomic
    /// reconfiguration must stage a replacement instance.
    pub(crate) fn prepare_audio(
        &mut self,
        config: &AudioConfig,
    ) -> Result<Vec<AudioBusInfo>, AuError> {
        let midi_input = self.midi_input()?;
        if self.restored_unprepared.is_some() && std::mem::take(&mut self.edited) {
            super::run_loop_for(super::EDIT_SETTLE);
        }
        let mut engine = lock(&self.engine);
        let outputs = midi_output_names(engine.unit()?).len() as u64;
        if let Some(&id) = config
            .events
            .outputs
            .iter()
            .filter(|&&id| id >= outputs)
            .chain(
                config
                    .events
                    .inputs
                    .iter()
                    .filter(|&&id| id != 0 || !midi_input),
            )
            .next()
        {
            return Err(AuError::UnknownEventPort { id });
        }
        engine.prepare_audio(config, self.restored_unprepared.as_ref())?;
        self.restored_unprepared = None;
        buses(engine.unit()?, Some(config))
    }
}

/// The names of the unit's MIDI outputs, one per cable.
pub(super) fn midi_output_names(unit: &AUAudioUnit) -> Vec<String> {
    let names = unsafe { unit.MIDIOutputNames() };
    (0..names.count())
        .map(|index| super::text(&names.objectAtIndex(index)))
        .collect()
}

fn format(bus: &AUAudioUnitBus) -> Result<Retained<AVAudioFormat>, AuError> {
    let format: Option<Retained<AVAudioFormat>> = unsafe { msg_send![bus, format] };
    format.ok_or(AuError::AudioBusMetadata)
}

fn layout(format: &AVAudioFormat, requested: Option<Layout>) -> Option<Layout> {
    match unsafe { format.channelLayout() } {
        Some(layout) => match unsafe { layout.layoutTag() } {
            // DiscreteInOrder explicitly preserves lane order, without assigning speakers.
            // Preserve the caller's negotiated labels; never infer them from channel count.
            tag if tag & 0xffff_0000 == DISCRETE_IN_ORDER => requested.filter(|layout| {
                layout.channels() == (tag & 0xffff) as usize
                    && layout.channels() == unsafe { format.channelCount() } as usize
            }),
            tag => Layout::ALL
                .into_iter()
                .find(|&layout| self::tag(layout).is_ok_and(|candidate| candidate == tag)),
        },
        // AVAudioFormat explicitly permits omitted layouts for mono and stereo only.
        None => match unsafe { format.channelCount() } {
            1 => Some(Layout::Mono),
            2 => Some(Layout::Stereo),
            _ => None,
        },
    }
}

fn arrays(unit: &AUAudioUnit) -> [(AudioDirection, Retained<AUAudioUnitBusArray>); 2] {
    unsafe {
        [
            (AudioDirection::Input, unit.inputBusses()),
            (AudioDirection::Output, unit.outputBusses()),
        ]
    }
}

pub(super) fn buses(
    unit: &AUAudioUnit,
    config: Option<&AudioConfig>,
) -> Result<Vec<AudioBusInfo>, AuError> {
    let mut result = Vec::new();
    for (direction, array) in arrays(unit) {
        let count = unsafe { array.count() };
        if count > plughost_core::MAX_AUDIO_BUSES {
            return Err(AuError::AudioBusMetadata);
        }
        for index in 0..count {
            let bus = unsafe { array.objectAtIndexedSubscript(index) };
            let format = format(&bus)?;
            let channels = unsafe { format.channelCount() };
            if unsafe { bus.index() } != index
                || channels as usize > plughost_core::MAX_AUDIO_CHANNELS
            {
                return Err(AuError::AudioBusMetadata);
            }
            result.push({
                // AU's primary input/output is element zero; additional elements are auxiliaries.
                let mut audio_bus_info = AudioBusInfo::new(
                    index as u64,
                    index as u32,
                    unsafe { bus.name() }
                        .map(|name| super::text(&name))
                        .unwrap_or_default(),
                    direction,
                    if index == 0 {
                        AudioBusRole::Main
                    } else {
                        AudioBusRole::Auxiliary
                    },
                    channels,
                );
                audio_bus_info.layout = layout(
                    &format,
                    config.and_then(|config| {
                        let requested = match direction {
                            AudioDirection::Input => &config.inputs,
                            AudioDirection::Output => &config.outputs,
                            _ => return None,
                        };
                        requested
                            .iter()
                            .find(|bus| bus.id == index as u64 && bus.active)
                            .map(|bus| bus.layout)
                    }),
                );
                audio_bus_info.active = Some(unsafe { bus.isEnabled() });
                audio_bus_info.f32 = if unsafe { format.isStandard() } {
                    Support::Supported
                } else {
                    Support::Unknown
                };
                audio_bus_info.f64 = Support::Unsupported;
                audio_bus_info
            });
        }
    }
    Ok(result)
}

pub(super) fn main_bus_config(
    unit: &AUAudioUnit,
    config: &ProcessConfig,
    midi_input: bool,
) -> Result<AudioConfig, AuError> {
    config.validate().map_err(AuError::Input)?;
    let buses = buses(unit, None)?;
    let configs = |direction, requested| {
        buses
            .iter()
            .filter(|bus| bus.direction == direction)
            .map(|bus| {
                AudioBusConfig::new(
                    bus.id,
                    if bus.index == 0 {
                        requested
                    } else {
                        Layout::None
                    },
                    bus.index == 0 && requested != Layout::None,
                )
            })
            .collect()
    };
    if config.input != Layout::None
        && !buses
            .iter()
            .any(|bus| bus.direction == AudioDirection::Input)
    {
        return Err(AuError::NoBus(config.input));
    }
    if config.output != Layout::None
        && !buses
            .iter()
            .any(|bus| bus.direction == AudioDirection::Output)
    {
        return Err(AuError::NoBus(config.output));
    }
    Ok(AudioConfig::new(
        config.sample_rate,
        config.max_block_size,
        config.sample_format,
        configs(AudioDirection::Input, config.input),
        configs(AudioDirection::Output, config.output),
    )
    .with_mode(config.mode)
    .with_events(plughost_core::EventConfig::new(
        midi_input.then_some(0).into_iter().collect(),
        (!midi_output_names(unit).is_empty())
            .then_some(0)
            .into_iter()
            .collect(),
    )))
}

fn tag(layout: Layout) -> Result<u32, AuError> {
    match layout {
        Layout::Mono => Ok(MONO),
        Layout::Stereo => Ok(STEREO),
        Layout::Surround51 => Ok(SURROUND_51),
        Layout::Surround71 => Ok(SURROUND_71),
        Layout::Lcr => Ok(LCR),
        Layout::Quad => Ok(QUAD),
        Layout::Surround50 => Ok(SURROUND_50),
        Layout::Surround70 => Ok(SURROUND_70),
        Layout::Surround512 => Ok(SURROUND_512),
        Layout::Surround514 => Ok(SURROUND_514),
        Layout::Surround712 => Ok(SURROUND_712),
        Layout::Surround714 => Ok(SURROUND_714),
        Layout::Surround916 => Ok(SURROUND_916),
        // The ambisonic tag carries the channel count, not the order.
        Layout::Ambisonics1 | Layout::Ambisonics2 | Layout::Ambisonics3 | Layout::Ambisonics4 => {
            Ok(HOA_ACN_SN3D | layout.channels() as u32)
        }
        _ => Err(AuError::AudioConfiguration),
    }
}

/// The portable channel of each channel of the layout's tag, for tags whose speaker order differs
/// from the portable order.
fn native_order(layout: Layout) -> Option<&'static [usize]> {
    match layout {
        // L R Ls Rs C Rls Rrs
        Layout::Surround70 => Some(&[0, 1, 5, 6, 2, 3, 4]),
        // L R C LFE Ls Rs Rls Rrs Ltm Rtm
        Layout::Surround712 => Some(&[0, 1, 2, 3, 6, 7, 4, 5, 8, 9]),
        // L R C LFE Ls Rs Rls Rrs Vhl Vhr Ltr Rtr
        Layout::Surround714 => Some(&[0, 1, 2, 3, 6, 7, 4, 5, 8, 9, 10, 11]),
        // L R C LFE Ls Rs Rls Rrs Lw Rw Vhl Vhr Ltm Rtm Ltr Rtr
        Layout::Surround916 => Some(&[0, 1, 2, 3, 6, 7, 4, 5, 14, 15, 8, 9, 12, 13, 10, 11]),
        _ => None,
    }
}

pub(super) fn validate_configuration(
    unit: &AUAudioUnit,
    config: &AudioConfig,
) -> Result<(), AuError> {
    if config.max_block_size > (u32::MAX as usize / size_of::<f32>())
        || !config.outputs.iter().any(|bus| bus.active)
        || config.configuration.is_some()
    {
        return Err(AuError::AudioConfiguration);
    }
    // Formats are negotiated with setFormat when configuring; channel capabilities are the only
    // constraint checked beforehand.
    for ((_, array), requested) in arrays(unit)
        .into_iter()
        .zip([&config.inputs, &config.outputs])
    {
        let count = unsafe { array.count() };
        if requested.iter().any(|bus| bus.id >= count as u64) {
            return Err(AuError::AudioConfiguration);
        }
    }
    if let Some(capabilities) = unsafe { unit.channelCapabilities() } {
        if capabilities.count() % 2 != 0 {
            return Err(AuError::AudioBusMetadata);
        }
        let counts = |buses: &[AudioBusConfig]| {
            let main = buses
                .iter()
                .find(|bus| bus.id == 0 && bus.active)
                .map_or(0, |bus| bus.layout.channels() as i64);
            let total = buses
                .iter()
                .filter(|bus| bus.active)
                .map(|bus| bus.layout.channels() as i64)
                .sum();
            (main, total)
        };
        let input = counts(&config.inputs);
        let output = counts(&config.outputs);
        // Instruments, generators and MIDI processors may leave their input off whatever channel
        // pairs they list (JUCE lists only the pairs with the input on).
        let input_off = input.1 == 0 && input_optional(unit);
        if !capabilities.is_empty()
            && !(0..capabilities.count()).step_by(2).any(|index| {
                matches_capability(
                    capabilities.objectAtIndex(index).longLongValue(),
                    capabilities.objectAtIndex(index + 1).longLongValue(),
                    input,
                    output,
                ) || input_off
                    && matches_capability(
                        -2,
                        capabilities.objectAtIndex(index + 1).longLongValue(),
                        input,
                        output,
                    )
            })
        {
            return Err(AuError::AudioConfiguration);
        }
    }
    Ok(())
}

/// Whether the unit's type makes audio input optional.
fn input_optional(unit: &AUAudioUnit) -> bool {
    let kind = unsafe { unit.componentDescription() }.componentType;
    [
        kAudioUnitType_MusicDevice,
        kAudioUnitType_Generator,
        kAudioUnitType_MIDIProcessor,
    ]
    .contains(&kind)
}

fn matches_capability(input: i64, output: i64, inputs: (i64, i64), outputs: (i64, i64)) -> bool {
    let side = |constraint: i64, counts: (i64, i64)| match constraint {
        -1 | -2 => true,
        value if value < -2 => value.checked_neg().is_some_and(|limit| counts.1 <= limit),
        0 => counts.1 == 0,
        value => counts.0 == value,
    };
    side(input, inputs)
        && side(output, outputs)
        && (!(input == -1 && output == -1) || inputs.0 == outputs.0)
}

pub(super) fn configure(
    unit: &AUAudioUnit,
    config: &AudioConfig,
) -> Result<(Vec<BusBuffer>, Vec<BusBuffer>), AuError> {
    let mut sides = [Vec::new(), Vec::new()];
    for (((_, array), requested), buffers) in arrays(unit)
        .into_iter()
        .zip([&config.inputs, &config.outputs])
        .zip(&mut sides)
    {
        let mut offset = 0;
        for index in 0..unsafe { array.count() } {
            let request = requested.iter().find(|bus| bus.id == index as u64);
            let active = request.is_some_and(|request| request.active);
            let bus = unsafe { array.objectAtIndexedSubscript(index) };
            if let Some(request) = request.filter(|request| request.active) {
                let format = unsafe {
                    AVAudioFormat::initStandardFormatWithSampleRate_channelLayout(
                        AVAudioFormat::alloc(),
                        config.sample_rate,
                        &AVAudioChannelLayout::layoutWithLayoutTag(tag(request.layout)?),
                    )
                };
                let set: Result<(), Retained<NSError>> =
                    unsafe { msg_send![&*bus, setFormat: &*format, error: _] };
                set.map_err(|error| AuError::LayoutRefused(request.layout, describe(&error)))?;
                let actual = self::format(&bus)?;
                // A unit that took the format as discrete lanes keeps them in portable order.
                let order = unsafe { actual.channelLayout() }
                    .filter(|native| {
                        tag(request.layout).ok() == Some(unsafe { native.layoutTag() })
                    })
                    .and_then(|_| native_order(request.layout));
                if layout(&actual, Some(request.layout)) != Some(request.layout)
                    || unsafe { actual.sampleRate() } != config.sample_rate
                    || !unsafe { actual.isStandard() }
                {
                    return Err(AuError::AudioConfiguration);
                }
                let end = offset + request.layout.channels();
                buffers.push(BusBuffer {
                    index,
                    channels: offset..end,
                    order,
                });
                offset = end;
            } else {
                // Units may refuse to allocate while any bus, enabled or not, has another rate.
                let current = self::format(&bus)?;
                if unsafe { current.sampleRate() } != config.sample_rate {
                    let mut description = unsafe { *current.streamDescription().as_ptr() };
                    description.mSampleRate = config.sample_rate;
                    let format = unsafe {
                        AVAudioFormat::initWithStreamDescription_channelLayout(
                            AVAudioFormat::alloc(),
                            NonNull::from(&description),
                            current.channelLayout().as_deref(),
                        )
                    }
                    .ok_or(AuError::AudioConfiguration)?;
                    let set: Result<(), Retained<NSError>> =
                        unsafe { msg_send![&*bus, setFormat: &*format, error: _] };
                    set.map_err(|error| AuError::RateRefused(describe(&error)))?;
                }
            }
            unsafe { bus.setEnabled(active) };
            if unsafe { bus.isEnabled() } != active {
                return Err(AuError::AudioConfiguration);
            }
        }
    }
    let [inputs, outputs] = sides;
    Ok((inputs, outputs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discrete_order_retains_only_explicit_matching_channel_labels() {
        let format = |tag| unsafe {
            AVAudioFormat::initStandardFormatWithSampleRate_channelLayout(
                AVAudioFormat::alloc(),
                48_000.0,
                &AVAudioChannelLayout::layoutWithLayoutTag(tag),
            )
        };
        let discrete = format(DISCRETE_IN_ORDER | 6);
        assert_eq!(layout(&discrete, None), None);
        assert_eq!(
            layout(&discrete, Some(Layout::Surround51)),
            Some(Layout::Surround51)
        );
        assert_eq!(layout(&discrete, Some(Layout::Surround71)), None);
        let different_order = format(objc2_core_audio_types::kAudioChannelLayoutTag_MPEG_5_1_B);
        assert_eq!(layout(&different_order, Some(Layout::Surround51)), None);
        let unknown = format(objc2_core_audio_types::kAudioChannelLayoutTag_Unknown | 6);
        assert_eq!(layout(&unknown, Some(Layout::Surround51)), None);
    }

    /// The system's channel labels of a layout tag, in native order.
    fn system_labels(tag: u32) -> Vec<u32> {
        use objc2_audio_toolbox::{
            AudioFormatGetProperty, AudioFormatGetPropertyInfo,
            kAudioFormatProperty_ChannelLayoutForTag,
        };
        use objc2_core_audio_types::{AudioChannelDescription, AudioChannelLayout};
        let specifier = (&raw const tag).cast();
        let mut size = 0u32;
        let status = unsafe {
            AudioFormatGetPropertyInfo(
                kAudioFormatProperty_ChannelLayoutForTag,
                size_of::<u32>() as u32,
                specifier,
                std::ptr::NonNull::from(&mut size),
            )
        };
        assert_eq!(status, 0, "{tag:#x}");
        // u64 storage keeps the layout aligned.
        let mut storage = vec![0u64; (size as usize).div_ceil(size_of::<u64>())];
        let layout = storage.as_mut_ptr().cast::<AudioChannelLayout>();
        let status = unsafe {
            AudioFormatGetProperty(
                kAudioFormatProperty_ChannelLayoutForTag,
                size_of::<u32>() as u32,
                specifier,
                &mut size,
                layout.cast(),
            )
        };
        assert_eq!(status, 0, "{tag:#x}");
        let descriptions: &[AudioChannelDescription] = unsafe {
            std::slice::from_raw_parts(
                (*layout).mChannelDescriptions.as_ptr(),
                (*layout).mNumberChannelDescriptions as usize,
            )
        };
        descriptions.iter().map(|d| d.mChannelLabel).collect()
    }

    #[test]
    fn native_orders_place_every_portable_channel_where_the_tag_has_its_speaker() {
        use objc2_core_audio_types::{
            kAudioChannelLabel_Center as C, kAudioChannelLabel_HOA_ACN_0 as ACN,
            kAudioChannelLabel_LFEScreen as LFE, kAudioChannelLabel_Left as L,
            kAudioChannelLabel_LeftSurround as LS, kAudioChannelLabel_LeftTopMiddle as LTM,
            kAudioChannelLabel_LeftTopRear as LTR, kAudioChannelLabel_LeftWide as LW,
            kAudioChannelLabel_Mono as M, kAudioChannelLabel_RearSurroundLeft as RLS,
            kAudioChannelLabel_RearSurroundRight as RRS, kAudioChannelLabel_Right as R,
            kAudioChannelLabel_RightSurround as RS, kAudioChannelLabel_RightTopMiddle as RTM,
            kAudioChannelLabel_RightTopRear as RTR, kAudioChannelLabel_RightWide as RW,
            kAudioChannelLabel_VerticalHeightLeft as LTF,
            kAudioChannelLabel_VerticalHeightRight as RTF,
        };
        // The system's speaker for each portable channel. Layouts with side surrounds have the
        // portable Ls Rs at the rear.
        let portable = |layout| -> Vec<u32> {
            match layout {
                Layout::Mono => vec![M],
                Layout::Stereo => vec![L, R],
                Layout::Lcr => vec![L, R, C],
                Layout::Quad => vec![L, R, LS, RS],
                Layout::Surround50 => vec![L, R, C, LS, RS],
                Layout::Surround51 => vec![L, R, C, LFE, LS, RS],
                Layout::Surround70 => vec![L, R, C, RLS, RRS, LS, RS],
                Layout::Surround71 => vec![L, R, C, LFE, RLS, RRS, LS, RS],
                Layout::Surround512 => vec![L, R, C, LFE, LS, RS, LTM, RTM],
                Layout::Surround514 => vec![L, R, C, LFE, LS, RS, LTF, RTF, LTR, RTR],
                Layout::Surround712 => vec![L, R, C, LFE, RLS, RRS, LS, RS, LTM, RTM],
                Layout::Surround714 => {
                    vec![L, R, C, LFE, RLS, RRS, LS, RS, LTF, RTF, LTR, RTR]
                }
                Layout::Surround916 => vec![
                    L, R, C, LFE, RLS, RRS, LS, RS, LTF, RTF, LTR, RTR, LTM, RTM, LW, RW,
                ],
                Layout::Ambisonics1
                | Layout::Ambisonics2
                | Layout::Ambisonics3
                | Layout::Ambisonics4 => (0..layout.channels() as u32).map(|n| ACN | n).collect(),
                _ => Vec::new(),
            }
        };
        for layout in Layout::ALL
            .into_iter()
            .filter(|&layout| layout != Layout::None)
        {
            let expected = portable(layout);
            assert_eq!(expected.len(), layout.channels(), "{layout:?}");
            let labels = system_labels(tag(layout).unwrap());
            let order = native_order(layout);
            let placed: Vec<_> = (0..labels.len())
                .map(|native| expected[order.map_or(native, |order| order[native])])
                .collect();
            assert_eq!(placed, labels, "{layout:?}");
        }
    }

    #[test]
    fn channel_constraints_keep_equality_independence_and_total_limits_distinct() {
        assert!(matches_capability(-1, -1, (2, 3), (2, 2)));
        assert!(!matches_capability(-1, -1, (1, 1), (2, 2)));
        assert!(matches_capability(-1, -2, (1, 1), (8, 8)));
        assert!(matches_capability(-16, 2, (8, 16), (2, 2)));
        assert!(!matches_capability(-16, 2, (8, 17), (2, 2)));
        assert!(matches_capability(0, 2, (0, 0), (2, 2)));
        assert!(!matches_capability(0, 2, (0, 1), (2, 2)));
    }
}
