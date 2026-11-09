//! The real implementation, built on `plughost`'s helper-process hosting.

use std::path::PathBuf;

use plughost::{
    BlockContext, Chain, ChainSlot, Events, Layout, PluginId, PluginInfo, PluginKind, PluginRef,
    PluginState, ProcessMode, RealtimeConfig, RealtimeEndpoint, Scanner, SpawnOptions,
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

/// The control side of a live plug-in.
pub struct Control {
    chain: Chain,
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
        let mut slot = ChainSlot::new(entry.reference.clone());
        if let Some(bytes) = state {
            if let Ok(parsed) = PluginState::from_bytes(bytes) {
                slot = slot.with_state(parsed);
            }
        }
        let mut chain =
            Chain::spawn(&self.helper, &[slot], &SpawnOptions::default()).map_err(describe)?;
        let mut config = chain
            .main_bus_config(sample_rate, quantum, Layout::Stereo, &[Layout::Stereo])
            .map_err(describe)?;
        config.mode = ProcessMode::Realtime;
        chain.prepare_audio(&config).map_err(describe)?;
        let parameters = read_parameters(&mut chain)?;
        let endpoint = chain
            .realtime_endpoint(&RealtimeConfig::new(quantum))
            .map_err(describe)?;
        let latency = chain.start_realtime().map_err(describe)?;
        let effect = Box::new(RealtimeEffect::new(endpoint, latency, quantum));
        Ok(Hosted {
            control: Control { chain },
            effect,
            latency,
            parameters,
        })
    }
}

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
pub struct RealtimeEffect {
    endpoint: RealtimeEndpoint,
    quantum: usize,
    input_l: Vec<f32>,
    input_r: Vec<f32>,
    output_l: Vec<f32>,
    output_r: Vec<f32>,
    filled: usize,
    played: usize,
    events: Events,
    produced: Events,
    /// The plug-in's latency plus one adapter quantum, in frames.
    latency: usize,
    bypass: bool,
}

impl RealtimeEffect {
    fn new(endpoint: RealtimeEndpoint, latency: u32, quantum: usize) -> Self {
        Self {
            endpoint,
            quantum,
            input_l: vec![0.0; quantum],
            input_r: vec![0.0; quantum],
            output_l: vec![0.0; quantum],
            output_r: vec![0.0; quantum],
            filled: 0,
            played: quantum,
            events: Events::for_block(),
            produced: Events::for_block(),
            latency: latency as usize + quantum,
            bypass: false,
        }
    }
}

impl HostedEffect for RealtimeEffect {
    fn process(&mut self, left: f32, right: f32) -> (f32, f32) {
        // Start of an adapter quantum: exchange the input block just collected for the
        // helper's output block, then play it sample by sample.
        if self.played >= self.quantum {
            if self.filled == self.quantum {
                let result = self.endpoint.process_audio_f32(
                    &BlockContext::new(self.quantum),
                    &[&self.input_l[..], &self.input_r[..]],
                    &mut [&mut self.output_l[..], &mut self.output_r[..]],
                    &[],
                    &self.events,
                    &mut self.produced,
                );
                if result.is_err() {
                    self.output_l.fill(0.0);
                    self.output_r.fill(0.0);
                }
                self.events.clear();
                self.produced.clear();
                self.filled = 0;
            } else {
                // The first quantum has no input yet.
                self.output_l.fill(0.0);
                self.output_r.fill(0.0);
            }
            self.played = 0;
        }
        let out = if self.bypass {
            (left, right)
        } else {
            (self.output_l[self.played], self.output_r[self.played])
        };
        self.played += 1;
        self.input_l[self.filled] = left;
        self.input_r[self.filled] = right;
        self.filled += 1;
        out
    }

    fn latency_samples(&self) -> usize {
        self.latency
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
