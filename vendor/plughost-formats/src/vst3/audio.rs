//! Native bus discovery and complete arrangement/activation negotiation.
use super::{errors::Vst3Error, host::wide_string, instance::Instance};
use plughost_core::{
    AudioBusConfig, AudioBusInfo, AudioBusRole, AudioConfig, AudioDirection, EventPortInfo, Layout,
    ProcessConfig, Support,
};
use vst3::Steinberg::Vst::{
    BusDirections_, BusInfo, BusTypes_, IAudioProcessorTrait, IComponentTrait, MediaTypes_,
    SpeakerArr, SpeakerArrangement, SymbolicSampleSizes_,
};
use vst3::Steinberg::{kResultFalse, kResultOk};

/// A speaker arrangement has at most 64 speakers.
const MAX_BUS_CHANNELS: i32 = 64;

pub(crate) fn direction(value: AudioDirection) -> i32 {
    match value {
        AudioDirection::Input => BusDirections_::kInput as i32,
        AudioDirection::Output => BusDirections_::kOutput as i32,
        // A direction VST3 does not have, which has no buses.
        _ => -1,
    }
}
/// VST3 orders a bus's channels by speaker bit, which is the portable order of every layout.
/// Arrangements with other speakers (k51_2 has top front instead of top middle speakers) are
/// not portable layouts, and a layout without an arrangement is refused.
pub(crate) fn arrangement(layout: Layout) -> Option<SpeakerArrangement> {
    Some(match layout {
        Layout::None => SpeakerArr::kEmpty,
        Layout::Mono => SpeakerArr::kMono,
        Layout::Stereo => SpeakerArr::kStereo,
        Layout::Surround51 => SpeakerArr::k51,
        Layout::Surround71 => SpeakerArr::k71Music,
        Layout::Lcr => SpeakerArr::k30Cine,
        Layout::Quad => SpeakerArr::k40Music,
        Layout::Surround50 => SpeakerArr::k50,
        Layout::Surround70 => SpeakerArr::k70Music,
        Layout::Surround512 => SpeakerArr::k51_2_TS,
        Layout::Surround514 => SpeakerArr::k51_4,
        Layout::Surround712 => SpeakerArr::k71_2,
        Layout::Surround714 => SpeakerArr::k71_4,
        Layout::Surround916 => SpeakerArr::k91_6_W,
        Layout::Ambisonics1 => SpeakerArr::kAmbi1stOrderACN,
        Layout::Ambisonics2 => SpeakerArr::kAmbi2cdOrderACN,
        Layout::Ambisonics3 => SpeakerArr::kAmbi3rdOrderACN,
        Layout::Ambisonics4 => SpeakerArr::kAmbi4thOrderACN,
        _ => return None,
    })
}
fn layout(value: SpeakerArrangement) -> Option<Layout> {
    Layout::ALL
        .into_iter()
        .find(|&layout| arrangement(layout) == Some(value))
}

/// What the owning thread asks the component and processor about, besides the audio buses: read
/// before the plugin activates, and answered from here while it is prepared. VST3 allows these
/// queries on the UI thread while the processor does not process. The event ports' note
/// expressions are the controller's, asked when the ports are.
#[derive(Clone)]
pub(crate) struct Queried {
    pub event_ports: Vec<EventPortInfo>,
    pub f32: Support,
    pub f64: Support,
    pub note_input: Support,
    pub note_output: Support,
}

impl Instance {
    /// Whether the processor processes samples of `size`.
    fn precision(&self, size: i32) -> Support {
        match unsafe { self.processor.canProcessSampleSize(size) } {
            result if result == kResultOk => Support::Supported,
            result if result == kResultFalse => Support::Unsupported,
            _ => Support::Unknown,
        }
    }

    /// Reads [`Queried`], without calling the controller. Call it while the processor does not
    /// process.
    pub(crate) fn queried(&self) -> Result<Queried, Vst3Error> {
        let notes = |direction| {
            let count = unsafe {
                self.component
                    .getBusCount(MediaTypes_::kEvent as i32, direction)
            };
            if count < 0 {
                Support::Unknown
            } else {
                (count > 0).into()
            }
        };
        Ok(Queried {
            event_ports: self.event_ports_with(|_| Support::Unknown)?,
            f32: self.precision(SymbolicSampleSizes_::kSample32 as i32),
            f64: self.precision(SymbolicSampleSizes_::kSample64 as i32),
            note_input: notes(BusDirections_::kInput as i32),
            note_output: notes(BusDirections_::kOutput as i32),
        })
    }

    /// The audio buses. Call it while the processor does not process.
    pub fn audio_buses(&self) -> Result<Vec<AudioBusInfo>, Vst3Error> {
        let f32 = self.precision(SymbolicSampleSizes_::kSample32 as i32);
        let f64 = self.precision(SymbolicSampleSizes_::kSample64 as i32);
        let mut buses = Vec::new();
        for dir in [AudioDirection::Input, AudioDirection::Output] {
            let count = unsafe {
                self.component
                    .getBusCount(MediaTypes_::kAudio as i32, direction(dir))
            };
            if count > plughost_core::MAX_AUDIO_BUSES as i32 {
                return Err(Vst3Error::AudioBusMetadata);
            }
            for index in 0..count {
                let mut info: BusInfo = unsafe { std::mem::zeroed() };
                let mut speakers = SpeakerArr::kEmpty;
                if unsafe {
                    self.component.getBusInfo(
                        MediaTypes_::kAudio as i32,
                        direction(dir),
                        index,
                        &mut info,
                    )
                } != kResultOk
                    || unsafe {
                        self.processor
                            .getBusArrangement(direction(dir), index, &mut speakers)
                    } != kResultOk
                    || !(0..=MAX_BUS_CHANNELS).contains(&info.channelCount)
                {
                    return Err(Vst3Error::AudioBusMetadata);
                }
                // Hosts size buffers from `channelCount`, as the SDK's host does, even when a
                // plugin's arrangement disagrees. Bus types other than main are auxiliary.
                let role = if info.busType == BusTypes_::kMain as i32 {
                    AudioBusRole::Main
                } else {
                    AudioBusRole::Auxiliary
                };
                buses.push({
                    let mut audio_bus_info = AudioBusInfo::new(
                        index as u64,
                        index as u32,
                        wide_string(&info.name),
                        dir,
                        role,
                        info.channelCount as u32,
                    );
                    audio_bus_info.layout = layout(speakers);
                    // kDefaultActive is a default, not the current activation state.
                    audio_bus_info.active = None;
                    audio_bus_info.f32 = f32;
                    audio_bus_info.f64 = f64;
                    audio_bus_info
                });
            }
        }
        Ok(buses)
    }

    pub fn main_audio_config(&self, config: &ProcessConfig) -> Result<AudioConfig, Vst3Error> {
        config.validate().map_err(Vst3Error::Input)?;
        let buses = self.audio_buses()?;
        let main = |dir, requested| -> Result<Vec<AudioBusConfig>, Vst3Error> {
            let Some(bus) = buses
                .iter()
                .find(|bus| bus.direction == dir && bus.role == AudioBusRole::Main)
            else {
                return if requested == Layout::None {
                    Ok(Vec::new())
                } else {
                    Err(Vst3Error::NoBus(requested))
                };
            };
            Ok(vec![AudioBusConfig::new(
                bus.id,
                requested,
                requested != Layout::None,
            )])
        };
        Ok(AudioConfig::new(
            config.sample_rate,
            config.max_block_size,
            config.sample_format,
            main(AudioDirection::Input, config.input)?,
            main(AudioDirection::Output, config.output)?,
        )
        .with_mode(config.mode)
        .with_events(self.main_event_config()))
    }

    pub fn negotiate_audio(&self, config: &AudioConfig) -> Result<Vec<AudioBusInfo>, Vst3Error> {
        let before = self.audio_buses()?;
        let requested = |dir| {
            if dir == AudioDirection::Input {
                &config.inputs
            } else {
                &config.outputs
            }
        };
        let mut arrangements = [Vec::new(), Vec::new()];
        for (index, dir) in [AudioDirection::Input, AudioDirection::Output]
            .into_iter()
            .enumerate()
        {
            let native: Vec<_> = before.iter().filter(|bus| bus.direction == dir).collect();
            for request in requested(dir) {
                if !native.iter().any(|bus| bus.id == request.id) {
                    return Err(Vst3Error::UnknownAudioBus { id: request.id });
                }
            }
            for bus in native {
                let mut speakers = SpeakerArr::kEmpty;
                if unsafe {
                    self.processor.getBusArrangement(
                        direction(dir),
                        bus.index as i32,
                        &mut speakers,
                    )
                } != kResultOk
                {
                    return Err(Vst3Error::AudioBusMetadata);
                }
                if let Some(request) = requested(dir).iter().find(|request| request.id == bus.id)
                    && request.layout != Layout::None
                {
                    speakers = arrangement(request.layout).ok_or(Vst3Error::LayoutRefused {
                        requested: request.layout,
                        plugin_channels: 0,
                    })?;
                }
                arrangements[index].push(speakers);
            }
        }
        let [inputs, outputs] = &mut arrangements;
        // kResultFalse means the plugin adapted or kept its arrangements; either way the host reads
        // them back, and only a mismatch with the request is a refusal.
        unsafe {
            self.processor.setBusArrangements(
                inputs.as_mut_ptr(),
                inputs.len() as i32,
                outputs.as_mut_ptr(),
                outputs.len() as i32,
            )
        };
        for dir in [AudioDirection::Input, AudioDirection::Output] {
            for request in requested(dir)
                .iter()
                .filter(|request| request.layout != Layout::None)
            {
                let mut actual = SpeakerArr::kEmpty;
                if unsafe {
                    self.processor
                        .getBusArrangement(direction(dir), request.id as i32, &mut actual)
                } != kResultOk
                {
                    return Err(Vst3Error::AudioBusMetadata);
                }
                if Some(actual) != arrangement(request.layout) {
                    return Err(Vst3Error::LayoutRefused {
                        requested: request.layout,
                        plugin_channels: actual.count_ones() as usize,
                    });
                }
            }
        }
        let mut buses = self.audio_buses()?;
        for bus in &mut buses {
            let request = requested(bus.direction)
                .iter()
                .find(|request| request.id == bus.id);
            let active = request.is_some_and(|request| request.active);
            if let Some(request) = request
                && request.layout != Layout::None
                && bus.layout != Some(request.layout)
            {
                return Err(Vst3Error::LayoutRefused {
                    requested: request.layout,
                    plugin_channels: bus.channels as usize,
                });
            }
            let result = unsafe {
                self.component.activateBus(
                    MediaTypes_::kAudio as i32,
                    direction(bus.direction),
                    bus.index as i32,
                    u8::from(active),
                )
            };
            if result != kResultOk {
                return Err(Vst3Error::AudioBusActivation {
                    id: bus.id,
                    code: result,
                });
            }
            bus.active = Some(active);
        }
        Ok(buses)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_layout_has_its_own_arrangement_of_its_channel_count() {
        for layout in Layout::ALL {
            let speakers = arrangement(layout).unwrap();
            assert_eq!(
                speakers.count_ones() as usize,
                layout.channels(),
                "{layout:?}"
            );
            assert_eq!(super::layout(speakers), Some(layout));
        }
    }
}
