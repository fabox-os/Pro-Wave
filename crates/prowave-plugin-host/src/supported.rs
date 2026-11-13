//! The real implementation, built on `plughost`'s helper-process hosting.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use arrayvec::ArrayVec;
use plughost::{
    AudioBusConfig, AudioBusRole, AudioDirection, AudioInputRoute, AudioSource, AutomationEvent,
    BlockContext, Chain, ChainSlot, Event, EventInputRoute, EventSource, Events, Layout,
    ParameterChange, PluginId, PluginInfo, PluginKind, PluginRef, PluginState, ProcessMode,
    RealtimeConfig, RealtimeEndpoint, RoutedChainConfig, SampleFormat, Scanner, SlotAudioConfig,
    SlotEventConfig, SpawnOptions,
};
use prowave_engine::HostedEffect;

use crate::{Error, Format, Kind, Parameter, PluginDescriptor};

/// A scanned set of installed plug-ins.
pub struct Catalog {
    helper: PathBuf,
    entries: Vec<Entry>,
}

struct Entry {
    key: String,
    reference: PluginRef,
    descriptor: PluginDescriptor,
}

/// A live plug-in: its control side and the realtime effect for the audio graph.
pub struct Hosted {
    /// Parameters, state and editor; stays on the UI thread.
    pub control: Control,
    /// Per-sample adapter for the engine.
    pub effect: Box<dyn HostedEffect>,
    /// The plug-in's latency plus the adapter's quantum, in frames.
    pub latency: u32,
    /// The parameters as they were when the plug-in loaded.
    pub parameters: Vec<Parameter>,
}

/// How the helper's native channels map onto the engine's stereo pair.
struct Routing {
    input_channels: usize,
    output_channels: usize,
    /// The channel of the main input's left side (effects only).
    main_in: Option<usize>,
    /// The channel of the main output's left side.
    main_out: usize,
}

impl Routing {
    /// The channel map of a main-bus configuration: one main input and one main output.
    fn from_main(config: &RoutedChainConfig, instrument: bool) -> Routing {
        let input_channels: usize = config.inputs.iter().map(|layout| layout.channels()).sum();
        let output_channels = config.slots.first().map_or(0, |slot| {
            slot.outputs.iter().map(|bus| bus.layout.channels()).sum()
        });
        Routing {
            input_channels,
            output_channels,
            main_in: (!instrument && input_channels >= 2).then_some(0),
            main_out: 0,
        }
    }
}

/// Configures every active native bus (main and auxiliary). Some CLAP plugins refuse the
/// per-port activation a main-bus configuration performs, but accept the buses as they already
/// are; the main input is fed from the caller and auxiliary inputs from silence.
fn all_buses_config(
    chain: &mut Chain,
    sample_rate: f64,
    quantum: usize,
) -> Result<(RoutedChainConfig, Routing), Error> {
    let buses = chain.audio_buses(0).map_err(describe)?;
    let mut chain_inputs = Vec::new();
    let mut input_routes = Vec::new();
    let mut input_channels = 0usize;
    let mut main_in = None;
    let mut external = 0usize;
    for bus in &buses {
        if bus.direction == AudioDirection::Input && bus.active != Some(false) {
            let layout = bus.layout.unwrap_or(Layout::Stereo);
            if bus.role == AudioBusRole::Main {
                main_in = Some(input_channels);
            }
            input_routes.push(AudioInputRoute::new(
                AudioBusConfig::new(bus.id, layout, true),
                AudioSource::External { bus: external },
            ));
            external += 1;
            input_channels += layout.channels();
            chain_inputs.push(layout);
        }
    }
    let mut outputs = Vec::new();
    let mut output_channels = 0usize;
    let mut main_out = 0usize;
    let mut seen_main = false;
    for bus in &buses {
        if bus.direction == AudioDirection::Output && bus.active != Some(false) {
            let layout = bus.layout.unwrap_or(Layout::Stereo);
            if bus.role == AudioBusRole::Main && !seen_main {
                main_out = output_channels;
                seen_main = true;
            }
            outputs.push(AudioBusConfig::new(bus.id, layout, true));
            output_channels += layout.channels();
        }
    }
    let ports = chain.event_ports(0).map_err(describe)?;
    let event_in = ports
        .iter()
        .find(|port| port.direction == AudioDirection::Input)
        .map(|port| port.id);
    let event_out = ports
        .iter()
        .find(|port| port.direction == AudioDirection::Output)
        .map(|port| port.id);
    let events = SlotEventConfig::new(
        event_in
            .zip(Some(EventSource::External { port: 0 }))
            .map(|(port, source)| EventInputRoute::new(port, source))
            .into_iter()
            .collect(),
        event_out.into_iter().collect(),
    );
    let config = RoutedChainConfig::new(
        sample_rate,
        quantum,
        SampleFormat::F32,
        chain_inputs,
        vec![SlotAudioConfig::new(input_routes, outputs).with_events(events)],
    )
    .with_mode(ProcessMode::Realtime)
    .with_event_inputs(1);
    Ok((
        config,
        Routing {
            input_channels,
            output_channels,
            main_in,
            main_out,
        },
    ))
}

/// A live hosted instrument: its control side and the realtime instrument for the audio graph.
pub struct HostedInstrument {
    /// Parameters, state and editor; stays on the UI thread.
    pub control: Control,
    /// Per-sample adapter for the engine.
    pub instrument: Box<dyn prowave_engine::HostedInstrument>,
    /// The plug-in's latency plus the adapter's quantum, in frames.
    pub latency: u32,
    /// The parameters as they were when the plug-in loaded.
    pub parameters: Vec<Parameter>,
}

/// The control side of a live plug-in.
pub struct Control {
    chain: Chain,
    /// Set by the audio-thread adapter when the endpoint needs a resynchronization.
    resync: Arc<AtomicBool>,
}

/// True when this build can host plug-ins.
pub fn available() -> bool {
    crate::helper_path().is_some()
}

/// Scans the standard plug-in folders (plus `extra`) through the helper and returns a catalog.
///
/// This blocks for as long as the scan takes: call it from a worker thread.
pub fn scan(extra: &[PathBuf]) -> Result<Catalog, Error> {
    let helper = crate::helper_path()
        .ok_or_else(|| Error::new("the Pro Wave plug-in helper is not installed"))?;
    let cache = crate::default_cache_path();
    if let Some(parent) = cache.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut scanner = Scanner::new(&helper, &cache);
    if !extra.is_empty() {
        scanner = scanner.directories(extra.to_vec());
    }
    let catalog = scanner.scan(&|_| {}).map_err(describe)?;
    let mut entries: Vec<Entry> = catalog
        .plugins()
        .into_iter()
        .filter_map(|(reference, info)| {
            let (key, descriptor) = descriptor_of(&reference, info)?;
            Some(Entry {
                key,
                reference,
                descriptor,
            })
        })
        .collect();
    entries.sort_by(|a, b| {
        a.descriptor
            .name
            .to_lowercase()
            .cmp(&b.descriptor.name.to_lowercase())
            .then_with(|| a.descriptor.format.badge().cmp(b.descriptor.format.badge()))
    });
    entries.dedup_by(|a, b| a.key == b.key);
    Ok(Catalog { helper, entries })
}

impl Catalog {
    /// Every installed class, sorted by name.
    pub fn plugins(&self) -> Vec<PluginDescriptor> {
        self.entries
            .iter()
            .map(|entry| entry.descriptor.clone())
            .collect()
    }

    /// How many classes each format contributed.
    pub fn counts(&self) -> [(Format, usize); 3] {
        Format::ALL.map(|format| {
            (
                format,
                self.entries
                    .iter()
                    .filter(|entry| entry.descriptor.format == format)
                    .count(),
            )
        })
    }

    /// Loads `key` at `sample_rate` in quanta of `quantum`, restoring `state` when it belongs
    /// to the same class.
    pub fn instantiate(
        &self,
        key: &str,
        sample_rate: f64,
        quantum: usize,
        state: Option<&[u8]>,
    ) -> Result<Hosted, Error> {
        let entry = self
            .entries
            .iter()
            .find(|entry| entry.key == key)
            .ok_or_else(|| Error::new(format!("plug-in `{key}` is not installed")))?;
        let quantum = quantum.max(1);
        let make_slot = || {
            let mut slot = ChainSlot::new(entry.reference.clone());
            if let Some(bytes) = state {
                if let Ok(parsed) = PluginState::from_bytes(bytes) {
                    slot = slot.with_state(parsed);
                }
            }
            slot
        };
        let spawn = || {
            Chain::spawn(&self.helper, &[make_slot()], &SpawnOptions::default()).map_err(describe)
        };
        let mut chain = spawn()?;
        let mut routing = None;
        if let Ok(mut config) =
            chain.main_bus_config(sample_rate, quantum, Layout::Stereo, &[Layout::Stereo])
        {
            config.mode = ProcessMode::Realtime;
            if chain.prepare_audio(&config).is_ok() {
                routing = Some(Routing::from_main(&config, false));
            }
        }
        let routing = match routing {
            Some(routing) => routing,
            None => {
                drop(chain);
                chain = spawn()?;
                let (config, routing) = all_buses_config(&mut chain, sample_rate, quantum)?;
                chain.prepare_audio(&config).map_err(describe)?;
                routing
            }
        };
        let parameters = read_parameters(&mut chain)?;
        let ids: Vec<u64> = parameters.iter().map(|parameter| parameter.id).collect();
        let endpoint = chain
            .realtime_endpoint(&RealtimeConfig::new(quantum))
            .map_err(describe)?;
        let latency = chain.start_realtime().map_err(describe)?;
        let resync = Arc::new(AtomicBool::new(false));
        let effect = Box::new(RealtimeEffect::new(
            endpoint,
            latency,
            quantum,
            ids,
            Arc::clone(&resync),
            routing.input_channels,
            routing.main_in,
            routing.output_channels,
            routing.main_out,
        ));
        let latency = latency.saturating_add(u32::try_from(quantum).unwrap_or(u32::MAX));
        Ok(Hosted {
            control: Control { chain, resync },
            effect,
            latency,
            parameters,
        })
    }

    /// Loads instrument `key` at `sample_rate` in quanta of `quantum`, restoring `state` when
    /// it belongs to the same class.
    ///
    /// An instrument has no audio input bus, so the chain is configured with no input layout
    /// and its event input port receives the notes. Without that, the host would ask for a
    /// stereo main input bus and the plug-in would refuse with "no main audio bus".
    pub fn instantiate_instrument(
        &self,
        key: &str,
        sample_rate: f64,
        quantum: usize,
        state: Option<&[u8]>,
    ) -> Result<HostedInstrument, Error> {
        let entry = self
            .entries
            .iter()
            .find(|entry| entry.key == key)
            .ok_or_else(|| Error::new(format!("plug-in `{key}` is not installed")))?;
        let quantum = quantum.max(1);
        let make_slot = || {
            let mut slot = ChainSlot::new(entry.reference.clone());
            if let Some(bytes) = state {
                if let Ok(parsed) = PluginState::from_bytes(bytes) {
                    slot = slot.with_state(parsed);
                }
            }
            slot
        };
        let spawn = || {
            Chain::spawn(&self.helper, &[make_slot()], &SpawnOptions::default()).map_err(describe)
        };
        let mut chain = spawn()?;
        let mut routing = None;
        if let Ok(mut config) =
            chain.main_bus_config(sample_rate, quantum, Layout::None, &[Layout::Stereo])
        {
            config.mode = ProcessMode::Realtime;
            if chain.prepare_audio(&config).is_ok() {
                routing = Some(Routing::from_main(&config, true));
            }
        }
        let routing = match routing {
            Some(routing) => routing,
            None => {
                drop(chain);
                chain = spawn()?;
                let (config, mut routing) = all_buses_config(&mut chain, sample_rate, quantum)?;
                // An instrument has no audio input to play, whatever auxiliary buses exist.
                routing.main_in = None;
                chain.prepare_audio(&config).map_err(describe)?;
                routing
            }
        };
        let parameters = read_parameters(&mut chain)?;
        let ids: Vec<u64> = parameters.iter().map(|parameter| parameter.id).collect();
        let endpoint = chain
            .realtime_endpoint(&RealtimeConfig::new(quantum))
            .map_err(describe)?;
        let latency = chain.start_realtime().map_err(describe)?;
        let resync = Arc::new(AtomicBool::new(false));
        let instrument = Box::new(RealtimeInstrument::new(
            endpoint,
            latency,
            quantum,
            ids,
            Arc::clone(&resync),
            routing.input_channels,
            routing.output_channels,
            routing.main_out,
        ));
        let latency = latency.saturating_add(u32::try_from(quantum).unwrap_or(u32::MAX));
        Ok(HostedInstrument {
            control: Control { chain, resync },
            instrument,
            latency,
            parameters,
        })
    }
}

impl Control {}

impl Control {
    /// Moves a parameter; `value` is normalized to 0..=1.
    pub fn set_parameter(&mut self, id: u64, value: f64) -> Result<(), Error> {
        if !value.is_finite() {
            return Err(Error::new("a parameter value must be finite"));
        }
        self.chain
            .set_parameter(0, id, value.clamp(0.0, 1.0))
            .map_err(describe)
    }

    /// Reads the current parameter list and values.
    pub fn parameters(&mut self) -> Result<Vec<Parameter>, Error> {
        read_parameters(&mut self.chain)
    }

    /// The plug-in's whole state, for a project file.
    pub fn state(&mut self) -> Result<Vec<u8>, Error> {
        let snapshot = self.chain.snapshot().map_err(describe)?;
        snapshot
            .slots
            .into_iter()
            .next()
            .and_then(|slot| slot.state)
            .map(|state| state.to_bytes())
            .ok_or_else(|| Error::new("the plug-in returned no state"))
    }

    /// Logic's Bypass: the plug-in keeps running but its output is not used.
    pub fn set_bypass(&mut self, bypass: bool) -> Result<(), Error> {
        self.chain.set_bypass(0, bypass).map_err(describe)
    }

    /// Resynchronizes the endpoint after a block it could not render. Call from the UI thread.
    pub fn resync_if_needed(&mut self) -> Result<(), Error> {
        if self.resync.swap(false, Ordering::AcqRel) {
            self.chain.resync_realtime().map(|_| ()).map_err(describe)?;
        }
        Ok(())
    }

    /// Opens the plug-in's own editor window (it belongs to the helper process).
    pub fn open_editor(&mut self) -> Result<(), Error> {
        self.chain.open_editor(0).map_err(describe)
    }

    pub fn close_editor(&mut self) -> Result<(), Error> {
        self.chain.close_editor(0).map_err(describe)
    }

    pub fn editor_open(&mut self) -> Result<bool, Error> {
        self.chain.editor_open(0).map_err(describe)
    }
}

fn read_parameters(chain: &mut Chain) -> Result<Vec<Parameter>, Error> {
    let parameters = chain.parameters(0).map_err(describe)?;
    Ok(parameters
        .into_iter()
        .map(|(info, value)| Parameter {
            id: info.id,
            title: info.title,
            units: info.units,
            steps: info.step_count,
            discrete: info.flags.discrete,
            default: info.default_value,
            value,
        })
        .collect())
}

fn format_of(id: &PluginId) -> Option<Format> {
    match id.format() {
        plughost::PluginFormat::Vst3 => Some(Format::Vst3),
        plughost::PluginFormat::AudioUnit => Some(Format::AudioUnit),
        plughost::PluginFormat::Clap => Some(Format::Clap),
        // plughost and this host know the same three formats today; a future one is skipped
        // rather than shown with the wrong badge.
        _ => None,
    }
}

fn descriptor_of(reference: &PluginRef, info: &PluginInfo) -> Option<(String, PluginDescriptor)> {
    let format = format_of(&info.id)?;
    let key = format!("{}:{}", format.key(), reference.id().class_id().as_str());
    Some((
        key.clone(),
        PluginDescriptor {
            key,
            format,
            name: info.name.clone(),
            vendor: info.vendor.clone(),
            version: info.version.clone(),
            kind: match info.kind {
                PluginKind::Instrument => Kind::Instrument,
                _ => Kind::Effect,
            },
            categories: info.categories.clone(),
            path: reference.bundle().map(|path| path.display().to_string()),
        },
    ))
}

/// The per-sample adapter the engine runs. It batches the engine's samples into the helper's
/// quanta; all of its buffers are reserved when it is created on the UI thread.
/// The per-sample adapter the engine runs. It batches the engine's samples into the helper's
/// quanta; all of its buffers are reserved when it is created on the UI thread.
pub struct RealtimeEffect {
    endpoint: RealtimeEndpoint,
    quantum: usize,
    /// One buffer per chain input channel. The main input carries the strip's audio; auxiliary
    /// inputs (some CLAP plug-ins have one) stay silent.
    inputs: Vec<Vec<f32>>,
    /// One buffer per chain output channel. The main output pair is what the strip plays;
    /// auxiliary outputs are discarded.
    outputs: Vec<Vec<f32>>,
    main_in: Option<usize>,
    main_out: usize,
    filled: usize,
    played: usize,
    events: Events,
    produced: Events,
    /// The plug-in's latency plus one adapter quantum, in frames.
    latency: usize,
    bypass: bool,
    /// The native parameter ID of each index in the host's parameter list.
    ids: Vec<u64>,
    /// Automation changes waiting for the next block, and the events built from them. Both
    /// reserve their room when the effect is created, so the audio thread never allocates.
    pending: Vec<(u64, f64)>,
    automation: Vec<AutomationEvent>,
    /// Told to the control side when a block fails; it resynchronizes the endpoint.
    resync: Arc<AtomicBool>,
}

impl RealtimeEffect {
    #[allow(clippy::too_many_arguments)]
    fn new(
        endpoint: RealtimeEndpoint,
        latency: u32,
        quantum: usize,
        ids: Vec<u64>,
        resync: Arc<AtomicBool>,
        input_channels: usize,
        main_in: Option<usize>,
        output_channels: usize,
        main_out: usize,
    ) -> Self {
        let main_in = main_in.filter(|main| main + 1 < input_channels);
        let main_out = if main_out + 1 < output_channels {
            main_out
        } else {
            0
        };
        Self {
            endpoint,
            quantum,
            inputs: (0..input_channels).map(|_| vec![0.0; quantum]).collect(),
            outputs: (0..output_channels).map(|_| vec![0.0; quantum]).collect(),
            main_in,
            main_out,
            filled: 0,
            played: quantum,
            events: Events::for_block(),
            produced: Events::for_block(),
            latency: latency as usize + quantum,
            bypass: false,
            ids,
            pending: Vec::with_capacity(32),
            automation: Vec::with_capacity(32),
            resync,
        }
    }
}

impl HostedEffect for RealtimeEffect {
    fn process(&mut self, left: f32, right: f32) -> (f32, f32) {
        // Start of an adapter quantum: exchange the input block just collected for the
        // helper's output block, then play it sample by sample.
        if self.played >= self.quantum {
            if self.filled == self.quantum {
                self.automation.clear();
                for (id, value) in self.pending.drain(..) {
                    self.automation
                        .push(AutomationEvent::new(0, ParameterChange::new(id, 0, value)));
                }
                let mut ins: ArrayVec<&[f32], 16> = ArrayVec::new();
                let mut too_many = false;
                for channel in &self.inputs {
                    if ins.try_push(channel.as_slice()).is_err() {
                        too_many = true;
                        break;
                    }
                }
                let mut outs: ArrayVec<&mut [f32], 16> = ArrayVec::new();
                if !too_many {
                    for channel in &mut self.outputs {
                        if outs.try_push(channel.as_mut_slice()).is_err() {
                            too_many = true;
                            break;
                        }
                    }
                }
                if too_many {
                    drop(outs);
                    drop(ins);
                    for channel in &mut self.outputs {
                        channel.fill(0.0);
                    }
                    self.resync.store(true, Ordering::Relaxed);
                } else {
                    // `plughost` 0.2.3 (pinned in Cargo.toml), `src/realtime.rs` lines 1-6
                    // ("The callback never waits for the helper") and
                    // `src/realtime/endpoint.rs` lines 1-4 ("allocates nothing, takes no lock
                    // and waits for nothing"). A helper that misses the one-quantum latency
                    // window never blocks this thread: the call returns
                    // `RealtimeOutput::Fallback(FallbackReason::Missed)` and fills the output
                    // with the configured `Fallback` (silence here, from
                    // `RealtimeConfig::new`, ramped over one quantum). Every later callback
                    // keeps playing that fallback until a completed quantum arrives (plughost
                    // tracks the misses in its realtime status); an error from the call fills
                    // silence and requests a resync below.
                    let result = self.endpoint.process_audio_f32(
                        &BlockContext::new(self.quantum),
                        ins.as_slice(),
                        outs.as_mut_slice(),
                        &self.automation,
                        &self.events,
                        &mut self.produced,
                    );
                    if result.is_err() {
                        drop(outs);
                        drop(ins);
                        for channel in &mut self.outputs {
                            channel.fill(0.0);
                        }
                        self.resync.store(true, Ordering::Relaxed);
                    }
                }
                self.events.clear();
                self.produced.clear();
                self.filled = 0;
            } else {
                // The first quantum has no input yet.
                for channel in &mut self.outputs {
                    channel.fill(0.0);
                }
            }
            self.played = 0;
        }
        let out = if self.bypass {
            (left, right)
        } else {
            let l = self
                .outputs
                .get(self.main_out)
                .map_or(0.0, |c| c[self.played]);
            let r = self
                .outputs
                .get(self.main_out + 1)
                .map_or(0.0, |c| c[self.played]);
            (l, r)
        };
        self.played += 1;
        if self.filled < self.quantum {
            if let Some(main) = self.main_in {
                if main + 1 < self.inputs.len() {
                    self.inputs[main][self.filled] = left;
                    self.inputs[main + 1][self.filled] = right;
                }
            }
            for (index, channel) in self.inputs.iter_mut().enumerate() {
                if self.main_in != Some(index) && self.main_in.is_none_or(|main| index != main + 1)
                {
                    channel[self.filled] = 0.0;
                }
            }
            self.filled += 1;
        }
        out
    }

    fn latency_samples(&self) -> usize {
        self.latency
    }

    fn set_bypass(&mut self, bypass: bool) {
        self.bypass = bypass;
    }

    fn set_param_norm(&mut self, param: usize, value: f32) {
        let Some(&id) = self.ids.get(param) else {
            return;
        };
        if !value.is_finite() {
            return;
        }
        let value = f64::from(value.clamp(0.0, 1.0));
        if self.pending.len() < self.pending.capacity() {
            self.pending.push((id, value));
        } else if let Some(last) = self.pending.last_mut() {
            *last = (id, value);
        }
    }
}

/// The adapter that turns the engine's per-sample calls into the helper's quanta for an
/// instrument: no audio input, stereo from the main output, and MIDI events at the sample they
/// start.
pub struct RealtimeInstrument {
    endpoint: RealtimeEndpoint,
    quantum: usize,
    /// One buffer per chain input channel; an instrument's inputs (auxiliary ones included)
    /// receive silence.
    inputs: Vec<Vec<f32>>,
    /// One buffer per chain output channel; the main output pair is played.
    outputs: Vec<Vec<f32>>,
    main_out: usize,
    /// Samples collected in the quantum being prepared, and the output sample being played.
    filled: usize,
    played: usize,
    /// Events of the quantum being prepared, and those of the next one (notes that arrive while
    /// the current quantum is already full).
    events: Events,
    next_events: Events,
    produced: Events,
    /// The plug-in's latency plus one adapter quantum, in frames.
    latency: usize,
    /// The native parameter ID of each index in the host's parameter list.
    ids: Vec<u64>,
    pending: Vec<(u64, f64)>,
    automation: Vec<AutomationEvent>,
    resync: Arc<AtomicBool>,
    bypass: bool,
    /// Notes this adapter started and has not ended, for `all_notes_off`.
    active: Vec<(u8, u8)>,
}

impl RealtimeInstrument {
    #[allow(clippy::too_many_arguments)]
    fn new(
        endpoint: RealtimeEndpoint,
        latency: u32,
        quantum: usize,
        ids: Vec<u64>,
        resync: Arc<AtomicBool>,
        input_channels: usize,
        output_channels: usize,
        main_out: usize,
    ) -> Self {
        let main_out = if main_out + 1 < output_channels {
            main_out
        } else {
            0
        };
        Self {
            endpoint,
            quantum,
            inputs: (0..input_channels).map(|_| vec![0.0; quantum]).collect(),
            outputs: (0..output_channels).map(|_| vec![0.0; quantum]).collect(),
            main_out,
            filled: 0,
            played: quantum,
            events: Events::for_block(),
            next_events: Events::for_block(),
            produced: Events::for_block(),
            latency: latency as usize + quantum,
            ids,
            pending: Vec::with_capacity(32),
            automation: Vec::with_capacity(32),
            resync,
            bypass: false,
            active: Vec::with_capacity(128),
        }
    }

    /// Queues `event` in the quantum it belongs to. The caller has already set its offset.
    fn push_event(&mut self, event: Event<'_>) {
        let target = if self.filled < self.quantum {
            &mut self.events
        } else {
            &mut self.next_events
        };
        let _ = target.push_within_capacity(event);
    }
}

impl prowave_engine::HostedInstrument for RealtimeInstrument {
    fn process(&mut self) -> (f32, f32) {
        if self.played >= self.quantum {
            if self.filled == self.quantum {
                self.automation.clear();
                for (id, value) in self.pending.drain(..) {
                    self.automation
                        .push(AutomationEvent::new(0, ParameterChange::new(id, 0, value)));
                }
                let mut ins: ArrayVec<&[f32], 16> = ArrayVec::new();
                let mut too_many = false;
                for channel in &self.inputs {
                    if ins.try_push(channel.as_slice()).is_err() {
                        too_many = true;
                        break;
                    }
                }
                let mut outs: ArrayVec<&mut [f32], 16> = ArrayVec::new();
                if !too_many {
                    for channel in &mut self.outputs {
                        if outs.try_push(channel.as_mut_slice()).is_err() {
                            too_many = true;
                            break;
                        }
                    }
                }
                if too_many {
                    drop(outs);
                    drop(ins);
                    for channel in &mut self.outputs {
                        channel.fill(0.0);
                    }
                    self.resync.store(true, Ordering::Relaxed);
                } else {
                    let result = self.endpoint.process_audio_f32(
                        &BlockContext::new(self.quantum),
                        ins.as_slice(),
                        outs.as_mut_slice(),
                        &self.automation,
                        &self.events,
                        &mut self.produced,
                    );
                    if result.is_err() {
                        drop(outs);
                        drop(ins);
                        for channel in &mut self.outputs {
                            channel.fill(0.0);
                        }
                        self.resync.store(true, Ordering::Relaxed);
                    }
                }
                self.events.clear();
                std::mem::swap(&mut self.events, &mut self.next_events);
                self.next_events.clear();
                self.produced.clear();
                self.filled = 0;
            } else {
                // The first quantum has no output from the helper yet.
                for channel in &mut self.outputs {
                    channel.fill(0.0);
                }
            }
            self.played = 0;
        }
        let out = if self.bypass {
            // An instrument has no audio to pass through, so bypass is silence.
            (0.0, 0.0)
        } else {
            let l = self
                .outputs
                .get(self.main_out)
                .map_or(0.0, |c| c[self.played]);
            let r = self
                .outputs
                .get(self.main_out + 1)
                .map_or(0.0, |c| c[self.played]);
            (l, r)
        };
        self.played += 1;
        self.filled += 1;
        out
    }

    fn note_on(&mut self, channel: u8, key: u8, velocity: u8) {
        let offset = if self.filled < self.quantum {
            self.filled
        } else {
            0
        };
        self.push_event(Event::note_on(offset, channel, key, velocity.max(1)));
        if !self.active.contains(&(channel, key)) && self.active.len() < self.active.capacity() {
            self.active.push((channel, key));
        }
    }

    fn note_off(&mut self, channel: u8, key: u8) {
        let offset = if self.filled < self.quantum {
            self.filled
        } else {
            0
        };
        self.push_event(Event::note_off(offset, channel, key, 0));
        if let Some(at) = self.active.iter().position(|&note| note == (channel, key)) {
            self.active.swap_remove(at);
        }
    }

    fn all_notes_off(&mut self) {
        let offset = if self.filled < self.quantum {
            self.filled
        } else {
            0
        };
        let notes = std::mem::take(&mut self.active);
        for &(channel, key) in &notes {
            self.push_event(Event::note_off(offset, channel, key, 0));
        }
        self.active = notes;
        self.active.clear();
    }

    fn latency_samples(&self) -> usize {
        self.latency
    }

    fn set_param_norm(&mut self, param: usize, value: f32) {
        let Some(&id) = self.ids.get(param) else {
            return;
        };
        if !value.is_finite() {
            return;
        }
        let value = f64::from(value.clamp(0.0, 1.0));
        if self.pending.len() < self.pending.capacity() {
            self.pending.push((id, value));
        } else if let Some(last) = self.pending.last_mut() {
            *last = (id, value);
        }
    }

    fn set_bypass(&mut self, bypass: bool) {
        self.bypass = bypass;
    }
}

/// The error followed by each cause; plughost keeps the native reason in `source()`.
fn describe(error: impl std::error::Error) -> Error {
    let mut text = error.to_string();
    let mut cause = error.source();
    while let Some(error) = cause {
        text = format!("{text}: {error}");
        cause = error.source();
    }
    Error(text)
}
