//! Hosted VST3, Audio Unit and CLAP plug-ins: the Plug-in Manager window and the controls of a
//! hosted plug-in on a channel strip.
//!
//! The plug-in itself runs in a helper process (see `prowave_plugin_host`); this module owns
//! the control side that stays on the UI thread, while the realtime adapter is handed to the
//! engine. A strip holds at most one hosted plug-in, after its built-in inserts. Projects keep
//! the class key and the plug-in's state blob, so a reopened project loads the same plug-ins
//! with the same settings when they are still installed.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use egui::{Color32, RichText};
use prowave_plugin_host as host;

use crate::automation;
use crate::engine_sync::EngineSync;
use crate::model::{AutoTarget, HostedInsert, Project, HOSTED_SLOT};
use crate::UiState;

/// Accent color per format: VST3 green, Audio Units amber, CLAP blue.
fn format_color(format: host::Format) -> Color32 {
    match format {
        host::Format::Vst3 => Color32::from_rgb(0x6f, 0xcf, 0x97),
        host::Format::AudioUnit => Color32::from_rgb(0xe8, 0xb4, 0x5a),
        host::Format::Clap => Color32::from_rgb(0x7f, 0xa8, 0xe8),
    }
}

/// The hosted plug-ins a picker offers for `role`: effects for the Audio FX menu, instruments
/// for the instrument slot.
fn picker_choices(
    plugins: Vec<host::PluginDescriptor>,
    role: Role,
) -> Vec<crate::plugin_picker::Hosted> {
    let kind = match role {
        Role::Effect => host::Kind::Effect,
        Role::Instrument => host::Kind::Instrument,
    };
    plugins
        .into_iter()
        .filter(|plugin| plugin.kind == kind)
        .map(|plugin| crate::plugin_picker::Hosted {
            key: plugin.key,
            name: plugin.name,
            vendor: plugin.vendor,
            format: plugin.format.label().to_string(),
        })
        .collect()
}

/// Which channel strip a hosted plug-in belongs to.
///
/// AGENT: controls are keyed by track index. Moving or deleting tracks after a plug-in is
/// loaded can point a control at the wrong strip until the project is reopened; stable track
/// ids are the follow-up (see `engine_sync`'s track-index commands).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Target {
    Master,
    Track(usize),
}

/// Whether a strip's hosted plug-in is an effect (Audio FX) or its instrument.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Role {
    Effect,
    Instrument,
}

impl Target {
    fn label(self, p: &Project) -> String {
        match self {
            Target::Master => "Stereo Out".to_string(),
            Target::Track(i) => p
                .tracks
                .get(i)
                .map_or_else(|| format!("Track {}", i + 1), |t| t.name.clone()),
        }
    }
}

/// Where the project now holds the insert with class key `key`, if anywhere.
fn target_with_key(p: &Project, key: &str, role: Role) -> Option<Target> {
    match role {
        Role::Effect => {
            if p.master_hosted
                .as_ref()
                .is_some_and(|saved| saved.key == key)
            {
                return Some(Target::Master);
            }
            p.tracks
                .iter()
                .position(|track| track.hosted.as_ref().is_some_and(|saved| saved.key == key))
                .map(Target::Track)
        }
        Role::Instrument => p
            .tracks
            .iter()
            .position(|track| {
                track
                    .hosted_instrument
                    .as_ref()
                    .is_some_and(|saved| saved.key == key)
            })
            .map(Target::Track),
    }
}

/// The live control side of one hosted plug-in.
struct Active {
    control: host::Control,
    parameters: Vec<host::Parameter>,
    last_refresh: Instant,
    /// The class key this control was loaded for, to notice a track that moved or went away.
    key: String,
}

/// What the windows asked for, applied after drawing so borrows stay simple.
enum Action {
    Scan,
    /// Loads a plug-in on a strip: an effect in an Audio FX slot or an instrument on a track.
    Insert {
        target: Target,
        role: Role,
        key: String,
    },
    Remove {
        target: Target,
        role: Role,
    },
    Bypass {
        target: Target,
        role: Role,
        on: bool,
    },
    OpenControls {
        target: Target,
        role: Role,
    },
    CloseControls,
    OpenEditor {
        target: Target,
        role: Role,
    },
    SetParameter {
        target: Target,
        role: Role,
        id: u64,
        param: Option<u8>,
        value: f64,
        ended: bool,
    },
    ResetParameters {
        target: Target,
        role: Role,
    },
}

/// The plug-in runtime: catalog, live control chains, and window state.
#[derive(Default)]
pub struct Plugins {
    catalog: Option<host::Catalog>,
    scanning: bool,
    scan_rx: Option<Receiver<Result<host::Catalog, host::Error>>>,
    /// One control chain per strip and role with a hosted plug-in.
    active: HashMap<(Target, Role), Active>,
    /// A project's plug-ins saved while the catalog is not scanned yet.
    pending: Vec<(Target, Role, HostedInsert)>,
    pub manager_open: bool,
    /// The target and role whose parameters window is open.
    window: Option<(Target, Role)>,
    filter: Option<host::Format>,
    search: String,
    selected: Option<String>,
    error: Option<String>,
    /// A hosted parameter changed since the last save/snapshot.
    dirty: bool,
    /// Whether the automatic scan at launch has been tried.
    scan_attempted: bool,
    /// The catalog changed and must be published to the picker's list.
    catalog_dirty: bool,
    /// When the live parameter lists were last fetched for change detection.
    last_poll: Option<Instant>,
}

/// A reborrow of the engine link, so the same option can be used more than once.
fn reborrow<'a>(sync: &'a mut Option<&mut EngineSync>) -> Option<&'a mut EngineSync> {
    sync.as_mut().map(|sync| &mut **sync)
}

/// The hosted plug-in of `target` for `role`, if the project has one.
fn insert_of(p: &Project, target: Target, role: Role) -> Option<&HostedInsert> {
    match (target, role) {
        (Target::Master, Role::Effect) => p.master_hosted.as_ref(),
        (Target::Master, Role::Instrument) => None,
        (Target::Track(i), Role::Effect) => p.tracks.get(i).and_then(|t| t.hosted.as_ref()),
        (Target::Track(i), Role::Instrument) => {
            p.tracks.get(i).and_then(|t| t.hosted_instrument.as_ref())
        }
    }
}

fn insert_of_mut(p: &mut Project, target: Target, role: Role) -> Option<&mut HostedInsert> {
    match (target, role) {
        (Target::Master, Role::Effect) => p.master_hosted.as_mut(),
        (Target::Master, Role::Instrument) => None,
        (Target::Track(i), Role::Effect) => p.tracks.get_mut(i).and_then(|t| t.hosted.as_mut()),
        (Target::Track(i), Role::Instrument) => p
            .tracks
            .get_mut(i)
            .and_then(|t| t.hosted_instrument.as_mut()),
    }
}

impl Plugins {
    /// Called every frame: polls the scanner, restores pending project plug-ins, and draws the
    /// windows.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        p: &mut Project,
        s: &mut UiState,
        mut sync: Option<&mut EngineSync>,
    ) {
        self.poll_scan();
        // A scan runs on a worker thread; keep repainting so its result is polled and the
        // pickers fill in even while the window would otherwise be idle.
        if self.scanning {
            ctx.request_repaint_after(Duration::from_millis(150));
        }
        // Scan once at launch, so the Audio FX and instrument pickers list third-party
        // plug-ins without the user having to open the Plug-in Manager first.
        if self.catalog.is_none() && !self.scanning && !self.scan_attempted && host::available() {
            self.scan();
        }
        if self.catalog_dirty {
            if let Some(catalog) = &self.catalog {
                s.mixer.hosted_plugins = picker_choices(catalog.plugins(), Role::Effect);
                s.mixer.hosted_instruments = picker_choices(catalog.plugins(), Role::Instrument);
            }
            self.catalog_dirty = false;
        }
        if s.plugin_manager {
            s.plugin_manager = false;
            self.manager_open = true;
            if self.catalog.is_none() && !self.scanning && host::available() {
                self.scan();
            }
        }
        if !self.pending.is_empty() && self.catalog.is_some() {
            self.restore_pending(p, reborrow(&mut sync), s);
        }

        self.reconcile(p, reborrow(&mut sync));
        let mut resync_error = None;
        for active in self.active.values_mut() {
            if let Err(error) = active.control.resync_if_needed() {
                resync_error = Some(error.to_string());
            }
        }
        if let Some(error) = resync_error {
            self.error = Some(error);
        }
        let due = self
            .last_poll
            .is_none_or(|at| at.elapsed() >= Duration::from_secs(1));
        if due && !self.active.is_empty() {
            let targets: Vec<(Target, Role)> = self.active.keys().copied().collect();
            for (target, role) in targets {
                self.refresh_now(target, role);
            }
            self.last_poll = Some(Instant::now());
        }

        let mut actions = Vec::new();
        // A hosted plug-in chosen in a picker (drawn after this call last frame).
        if let Some((strip, key)) = s.mixer.pending_hosted.take() {
            let target = if strip == crate::mixer::MASTER_STRIP {
                Target::Master
            } else {
                Target::Track(strip)
            };
            actions.push(Action::Insert {
                target,
                role: Role::Effect,
                key,
            });
        }
        if let Some((strip, key)) = s.mixer.pending_hosted_instrument.take() {
            if strip != crate::mixer::MASTER_STRIP {
                actions.push(Action::Insert {
                    target: Target::Track(strip),
                    role: Role::Instrument,
                    key,
                });
            }
        }
        if let Some((strip, act)) = s.mixer.pending_hosted_act.take() {
            let target = if strip == crate::mixer::MASTER_STRIP {
                Target::Master
            } else {
                Target::Track(strip)
            };
            actions.push(match act {
                crate::mixer::HostedAct::Open => Action::OpenEditor {
                    target,
                    role: Role::Effect,
                },
                crate::mixer::HostedAct::Bypass(on) => Action::Bypass {
                    target,
                    role: Role::Effect,
                    on,
                },
                crate::mixer::HostedAct::Remove => Action::Remove {
                    target,
                    role: Role::Effect,
                },
            });
        }
        if let Some((strip, act)) = s.mixer.pending_instrument.take() {
            let target = Target::Track(strip);
            actions.push(match act {
                crate::mixer::InstrumentAct::Open => Action::OpenEditor {
                    target,
                    role: Role::Instrument,
                },
                crate::mixer::InstrumentAct::Bypass(on) => Action::Bypass {
                    target,
                    role: Role::Instrument,
                    on,
                },
                crate::mixer::InstrumentAct::Remove => Action::Remove {
                    target,
                    role: Role::Instrument,
                },
            });
        }
        if self.manager_open {
            self.draw_manager(ctx, p, &mut actions);
        }
        if let Some((target, role)) = self.window {
            self.draw_parameters(ctx, p, target, role, &mut actions);
            self.refresh(target, role);
            ctx.request_repaint_after(Duration::from_millis(500));
        }
        self.apply(actions, p, s, reborrow(&mut sync));
    }

    /// After opening a project: load every saved plug-in, or remember them until the scan
    /// finishes.
    /// After opening a project: load every saved plug-in, or remember them until the scan
    /// finishes.
    pub fn restore(&mut self, p: &mut Project, mut sync: Option<&mut EngineSync>, s: &mut UiState) {
        // A project replace invalidates every hosted stage the engine still has.
        if let Some(sync) = reborrow(&mut sync) {
            sync.clear_hosted();
            sync.clear_hosted_instruments();
        }
        self.active.clear();
        self.pending.clear();
        self.window = None;
        self.dirty = false;
        if let Some(saved) = p.master_hosted.clone() {
            self.pending.push((Target::Master, Role::Effect, saved));
        }
        for (i, track) in p.tracks.iter().enumerate() {
            if let Some(saved) = track.hosted.clone() {
                self.pending.push((Target::Track(i), Role::Effect, saved));
            }
            if let Some(saved) = track.hosted_instrument.clone() {
                self.pending
                    .push((Target::Track(i), Role::Instrument, saved));
            }
        }
        if self.pending.is_empty() {
            return;
        }
        if self.catalog.is_none() {
            if !self.scanning && host::available() {
                self.scan();
            }
            return;
        }
        if sync.is_none() {
            self.error = Some("hosted plug-ins need an audio output to load".to_string());
            return;
        }
        self.restore_pending(p, sync, s);
    }

    /// Drops control chains whose project insert moved to another track or disappeared.
    ///
    /// Controls are keyed by track index, so deleting or reordering tracks can leave one pointing
    /// at a different strip. Comparing the class key catches that: the stale engine stage is
    /// removed rather than processing another track's audio. When the insert is still in the
    /// project under another index (a track moved), it is queued for loading there again with
    /// its saved state, so the plug-in is not lost until the project is reopened.
    /// Drops control chains whose project insert moved to another track or disappeared.
    ///
    /// Controls are keyed by track index, so deleting or reordering tracks can leave one pointing
    /// at a different strip. Comparing the class key catches that: the stale engine stage is
    /// removed rather than processing another track's audio. When the insert is still in the
    /// project under another index (a track moved), it is queued for loading there again with
    /// its saved state, so the plug-in is not lost until the project is reopened.
    fn reconcile(&mut self, p: &mut Project, mut sync: Option<&mut EngineSync>) {
        if self.active.is_empty() {
            return;
        }
        let stale: Vec<((Target, Role), String)> = self
            .active
            .iter()
            .filter(|((target, role), active)| {
                insert_of(p, *target, *role).is_none_or(|saved| saved.key != active.key)
            })
            .map(|(key, active)| (*key, active.key.clone()))
            .collect();
        let mut moved: Vec<(Target, Role, HostedInsert)> = Vec::new();
        for ((target, role), key) in stale {
            if let Some(sync) = reborrow(&mut sync) {
                match (target, role) {
                    (Target::Master, Role::Effect) => {
                        sync.set_master_hosted(None, false);
                    }
                    (Target::Track(i), Role::Effect) => {
                        sync.set_track_hosted(i, None, false);
                    }
                    (Target::Track(i), Role::Instrument) => {
                        sync.set_track_hosted_instrument(i, None);
                    }
                    (Target::Master, Role::Instrument) => {}
                }
            }
            self.active.remove(&(target, role));
            if self.window == Some((target, role)) {
                self.window = None;
            }
            // The same insert under another index: load it there again.
            if let Some(new) = target_with_key(p, &key, role) {
                if new != target && !self.active.contains_key(&(new, role)) {
                    if let Some(saved) = insert_of(p, new, role) {
                        moved.push((new, role, saved.clone()));
                    }
                }
            }
        }
        self.pending.extend(moved);
    }

    /// Saves every live plug-in's state into the project before it is written to disk.
    /// Saves every live plug-in's state into the project before it is written to disk.
    pub fn snapshot(&mut self, p: &mut Project) {
        let states: Vec<(Target, Role, Vec<u8>)> = self
            .active
            .iter_mut()
            .filter_map(|((target, role), active)| {
                active
                    .control
                    .state()
                    .ok()
                    .map(|state| (*target, *role, state))
            })
            .collect();
        for (target, role, state) in states {
            if let Some(saved) = insert_of_mut(p, target, role) {
                saved.state = state;
            }
        }
        self.dirty = false;
    }

    /// True when a hosted parameter changed but the project's saved snapshot is stale.
    pub fn dirty(&self) -> bool {
        self.dirty
    }

    fn poll_scan(&mut self) {
        let Some(rx) = self.scan_rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(catalog)) => {
                let total: usize = catalog.counts().iter().map(|(_, count)| count).sum();
                tracing::info!("plug-in scan: {total} installed");
                self.catalog = Some(catalog);
                self.catalog_dirty = true;
                self.scanning = false;
                self.scan_rx = None;
            }
            Ok(Err(error)) => {
                self.error = Some(error.to_string());
                self.scanning = false;
                self.scan_rx = None;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.error = Some("the plug-in scan stopped unexpectedly".to_string());
                self.scanning = false;
                self.scan_rx = None;
            }
        }
    }

    fn scan(&mut self) {
        self.scanning = true;
        self.scan_attempted = true;
        self.error = None;
        tracing::info!("scanning for VST3, Audio Unit and CLAP plug-ins");
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(host::scan(&[]));
        });
        self.scan_rx = Some(rx);
    }

    fn restore_pending(
        &mut self,
        p: &mut Project,
        mut sync: Option<&mut EngineSync>,
        s: &mut UiState,
    ) {
        while let Some((target, role, saved)) = self.pending.pop() {
            let Some(sync) = reborrow(&mut sync) else {
                self.pending.clear();
                return;
            };
            if let Err(error) = self.load(target, role, &saved.key, Some(&saved.state), p, sync) {
                self.error = Some(error.clone());
                s.status = Some(error);
            }
        }
    }

    /// Instantiates `key`, sends its realtime adapter to the engine and keeps its controls.
    /// Instantiates `key`, sends its realtime adapter to the engine and keeps its controls.
    fn load(
        &mut self,
        target: Target,
        role: Role,
        key: &str,
        state: Option<&[u8]>,
        p: &mut Project,
        sync: &mut EngineSync,
    ) -> Result<(), String> {
        let catalog = self
            .catalog
            .as_ref()
            .ok_or_else(|| "scan for plug-ins first".to_string())?;
        let descriptor = catalog
            .plugins()
            .into_iter()
            .find(|plugin| plugin.key == key)
            .ok_or_else(|| format!("plug-in not installed: {key}"))?;
        let previous = insert_of(p, target, role).cloned();
        // Replacing the same plug-in keeps its settings; a different one starts fresh.
        let state = state.map(<[u8]>::to_vec).or_else(|| {
            previous
                .as_ref()
                .filter(|previous| previous.key == key && !previous.state.is_empty())
                .map(|previous| previous.state.clone())
        });
        let bypass = previous.as_ref().is_some_and(|saved| saved.bypass);
        let mut control;
        let latency;
        let parameters;
        match role {
            Role::Effect => {
                let hosted = catalog
                    .instantiate(
                        key,
                        sync.sample_rate(),
                        host::DEFAULT_QUANTUM,
                        state.as_deref(),
                    )
                    .map_err(|error| error.to_string())?;
                let host::Hosted {
                    control: loaded,
                    effect,
                    latency: loaded_latency,
                    parameters: loaded_parameters,
                } = hosted;
                control = loaded;
                latency = loaded_latency;
                parameters = loaded_parameters;
                if let Err(error) = control.set_bypass(bypass) {
                    self.error = Some(error.to_string());
                }
                let sent = match target {
                    Target::Master => sync.set_master_hosted(Some(effect), bypass),
                    Target::Track(i) => sync.set_track_hosted(i, Some(effect), bypass),
                };
                if !sent {
                    return Err("the audio engine is busy; try again".to_string());
                }
            }
            Role::Instrument => {
                let Target::Track(i) = target else {
                    return Err("an instrument needs an instrument track".to_string());
                };
                let hosted = catalog
                    .instantiate_instrument(
                        key,
                        sync.sample_rate(),
                        host::DEFAULT_QUANTUM,
                        state.as_deref(),
                    )
                    .map_err(|error| error.to_string())?;
                let host::HostedInstrument {
                    control: loaded,
                    mut instrument,
                    latency: loaded_latency,
                    parameters: loaded_parameters,
                } = hosted;
                control = loaded;
                latency = loaded_latency;
                parameters = loaded_parameters;
                if let Err(error) = control.set_bypass(bypass) {
                    self.error = Some(error.to_string());
                }
                instrument.set_bypass(bypass);
                if !sync.set_track_hosted_instrument(i, Some(instrument)) {
                    return Err("the audio engine is busy; try again".to_string());
                }
            }
        }
        let fresh = control.state().unwrap_or_default();
        let meta: Vec<crate::model::HostedParam> = parameters
            .iter()
            .map(|parameter| crate::model::HostedParam {
                id: parameter.id,
                title: parameter.title.clone(),
                default: parameter.default.unwrap_or(0.0) as f32,
                value: parameter.value.or(parameter.default).unwrap_or(0.0) as f32,
                steps: parameter.steps,
                discrete: parameter.discrete,
            })
            .collect();
        self.active.insert(
            (target, role),
            Active {
                control,
                parameters,
                last_refresh: Instant::now(),
                key: key.to_string(),
            },
        );
        let mut insert = HostedInsert::new(&descriptor, fresh);
        insert.bypass = bypass;
        insert.latency = latency;
        insert.parameters = meta;
        if let Some(state) = state {
            insert.state = state;
        }
        match (target, role) {
            (Target::Master, Role::Effect) => p.master_hosted = Some(insert),
            (Target::Track(i), Role::Effect) => {
                if let Some(track) = p.tracks.get_mut(i) {
                    track.hosted = Some(insert);
                }
            }
            (Target::Track(i), Role::Instrument) => {
                if let Some(track) = p.tracks.get_mut(i) {
                    track.hosted_instrument = Some(insert);
                }
            }
            (Target::Master, Role::Instrument) => {}
        }
        tracing::info!(
            "loaded {} ({}) on {} with {} frames of latency",
            descriptor.name,
            descriptor.format.label(),
            target.label(p),
            latency
        );
        Ok(())
    }

    fn apply(
        &mut self,
        actions: Vec<Action>,
        p: &mut Project,
        s: &mut UiState,
        mut sync: Option<&mut EngineSync>,
    ) {
        for action in actions {
            match action {
                Action::Scan => self.scan(),
                Action::Insert { target, role, key } => {
                    let Some(sync) = reborrow(&mut sync) else {
                        self.error = Some("there is no audio output".to_string());
                        continue;
                    };
                    match self.load(target, role, &key, None, p, sync) {
                        Ok(()) => {
                            self.open_editor(target, role);
                            s.status = Some(format!(
                                "inserted {} on {}",
                                insert_of(p, target, role)
                                    .map_or("the plug-in", |insert| insert.name.as_str()),
                                target.label(p)
                            ));
                        }
                        Err(error) => {
                            self.error = Some(error.clone());
                            s.status = Some(error);
                        }
                    }
                }
                Action::Remove { target, role } => {
                    if let Some(sync) = reborrow(&mut sync) {
                        match (target, role) {
                            (Target::Master, _) => {
                                sync.set_master_hosted(None, false);
                            }
                            (Target::Track(i), Role::Effect) => {
                                sync.set_track_hosted(i, None, false);
                            }
                            (Target::Track(i), Role::Instrument) => {
                                sync.set_track_hosted_instrument(i, None);
                            }
                        }
                    }
                    self.active.remove(&(target, role));
                    if self.window == Some((target, role)) {
                        self.window = None;
                    }
                    match (target, role) {
                        (Target::Master, _) => p.master_hosted = None,
                        (Target::Track(i), Role::Effect) => {
                            if let Some(track) = p.tracks.get_mut(i) {
                                track.hosted = None;
                            }
                        }
                        (Target::Track(i), Role::Instrument) => {
                            if let Some(track) = p.tracks.get_mut(i) {
                                track.hosted_instrument = None;
                            }
                        }
                    }
                }
                Action::Bypass { target, role, on } => {
                    if let Some(saved) = insert_of_mut(p, target, role) {
                        saved.bypass = on;
                    }
                    if let Some(active) = self.active.get_mut(&(target, role)) {
                        if let Err(error) = active.control.set_bypass(on) {
                            self.error = Some(error.to_string());
                        }
                    }
                    if let Some(sync) = reborrow(&mut sync) {
                        match (target, role) {
                            (Target::Master, _) => {
                                sync.set_master_hosted_bypass(on);
                            }
                            (Target::Track(i), Role::Effect) => {
                                sync.set_track_hosted_bypass(i, on);
                            }
                            (Target::Track(i), Role::Instrument) => {
                                sync.set_track_instrument_bypass(i, on);
                            }
                        }
                    }
                }
                Action::OpenControls { target, role } => {
                    self.window = Some((target, role));
                    self.refresh_now(target, role);
                }
                Action::CloseControls => {
                    self.window = None;
                    self.snapshot(p);
                }
                Action::OpenEditor { target, role } => self.open_editor(target, role),
                Action::SetParameter {
                    target,
                    role,
                    id,
                    param,
                    value,
                    ended,
                } => {
                    if let Some(active) = self.active.get_mut(&(target, role)) {
                        match active.control.set_parameter(id, value) {
                            Ok(()) => self.dirty = true,
                            Err(error) => self.error = Some(error.to_string()),
                        }
                    }
                    if let Some(param) = param {
                        if let Some(saved) = insert_of_mut(p, target, role) {
                            if let Some(parameter) = saved.parameters.get_mut(param as usize) {
                                parameter.value = value as f32;
                            }
                        }
                        // Instrument parameters do not have automation lanes yet; effect
                        // parameters keep the hosted lane on the strip.
                        if let (Role::Effect, Target::Track(i)) = (role, target) {
                            let (beat, playing) = (p.position_beats, p.playing);
                            if let Some(track) = p.tracks.get_mut(i) {
                                let auto = AutoTarget::Fx {
                                    slot: HOSTED_SLOT,
                                    param,
                                };
                                automation::moved(track, auto, value as f32);
                                if ended {
                                    automation::released(track, auto, beat, playing);
                                }
                            }
                        }
                    }
                }
                Action::ResetParameters { target, role } => {
                    let defaults: Vec<(u64, f64)> = self
                        .active
                        .get(&(target, role))
                        .map(|active| {
                            active
                                .parameters
                                .iter()
                                .map(|parameter| (parameter.id, parameter.default.unwrap_or(0.0)))
                                .collect()
                        })
                        .unwrap_or_default();
                    if let Some(active) = self.active.get_mut(&(target, role)) {
                        for (id, value) in &defaults {
                            let _ = active.control.set_parameter(*id, *value);
                        }
                        if let Ok(parameters) = active.control.parameters() {
                            active.parameters = parameters;
                        }
                    }
                    if let Some(saved) = insert_of_mut(p, target, role) {
                        for parameter in &mut saved.parameters {
                            if let Some((_, default)) =
                                defaults.iter().find(|(id, _)| *id == parameter.id)
                            {
                                parameter.value = *default as f32;
                            }
                        }
                    }
                    self.dirty = true;
                }
            }
        }
    }

    /// Opens the plug-in's own GUI, as Logic does; a plug-in without an editor falls back to
    /// the parameter window.
    fn open_editor(&mut self, target: Target, role: Role) {
        if let Some(active) = self.active.get_mut(&(target, role)) {
            if let Err(error) = active.control.open_editor() {
                tracing::info!("plug-in has no editor ({error}); showing the parameter window");
                self.window = Some((target, role));
            }
        }
    }

    fn refresh_now(&mut self, target: Target, role: Role) {
        let Some(active) = self.active.get_mut(&(target, role)) else {
            return;
        };
        match active.control.parameters() {
            Ok(parameters) => {
                let changed = parameters.len() != active.parameters.len()
                    || parameters
                        .iter()
                        .zip(&active.parameters)
                        .any(|(new, old)| new.id != old.id || new.value != old.value);
                if changed {
                    self.dirty = true;
                }
                active.parameters = parameters;
            }
            Err(error) => self.error = Some(error.to_string()),
        }
        active.last_refresh = Instant::now();
    }

    fn refresh(&mut self, target: Target, role: Role) {
        let due = self
            .active
            .get(&(target, role))
            .is_some_and(|active| active.last_refresh.elapsed() >= Duration::from_millis(700));
        if due {
            self.refresh_now(target, role);
        }
    }

    fn draw_manager(&mut self, ctx: &egui::Context, p: &Project, actions: &mut Vec<Action>) {
        let mut open = self.manager_open;
        egui::Window::new("Plug-in Manager")
            .id(egui::Id::unique("plugin_manager"))
            .open(&mut open)
            .default_size([860.0, 560.0])
            .min_size([620.0, 380.0])
            .resizable(true)
            .show(ctx, |ui| {
                if !host::available() {
                    ui.add_space(12.0);
                    ui.label(
                        RichText::new("Plug-in hosting is not available on this platform.")
                            .size(14.0),
                    );
                    ui.label(
                        "Pro Wave hosts VST3, Audio Unit and CLAP plug-ins on macOS (Apple Silicon) and Windows x64.",
                    );
                    return;
                }
                ui.horizontal(|ui| {
                    ui.heading("Plug-ins");
                    ui.add_space(8.0);
                    if ui.button("Scan").clicked() {
                        actions.push(Action::Scan);
                    }
                    ui.add_space(8.0);
                    if self.scanning {
                        ui.spinner();
                        ui.label(RichText::new("Scanning…").weak());
                    } else if let Some(catalog) = &self.catalog {
                        let counts = catalog.counts();
                        let total: usize = counts.iter().map(|(_, count)| count).sum();
                        ui.label(RichText::new(format!("{total} installed")).weak());
                    }
                    if let Some(error) = &self.error {
                        ui.add_space(8.0);
                        ui.label(RichText::new(error).color(Color32::from_rgb(0xe0, 0x7a, 0x5f)));
                    }
                });
                ui.separator();
                ui.horizontal_top(|ui| {
                    self.draw_filters(ui);
                    ui.separator();
                    ui.vertical(|ui| {
                        self.draw_search(ui);
                        ui.add_space(6.0);
                        self.draw_list(ui, p, actions);
                    });
                    ui.separator();
                    self.draw_details(ui, p, actions);
                });
            });
        self.manager_open = open;
    }

    fn draw_filters(&mut self, ui: &mut egui::Ui) {
        ui.set_width(168.0);
        ui.add_space(4.0);
        let counts = self.catalog.as_ref().map_or(
            [
                (host::Format::Vst3, 0),
                (host::Format::AudioUnit, 0),
                (host::Format::Clap, 0),
            ],
            |catalog| catalog.counts(),
        );
        let total: usize = counts.iter().map(|(_, count)| count).sum();
        if ui
            .selectable_label(self.filter.is_none(), format!("All  ({total})"))
            .clicked()
        {
            self.filter = None;
        }
        for (format, count) in counts {
            let selected = self.filter == Some(format);
            let response = ui.horizontal(|ui| {
                let (dot, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                ui.painter()
                    .circle_filled(dot.center(), 4.0, format_color(format));
                let text = if selected {
                    RichText::new(format.label()).strong().color(Color32::WHITE)
                } else {
                    RichText::new(format.label())
                };
                let response = ui.selectable_label(selected, text);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(count.to_string()).weak().small());
                });
                response
            });
            if response.inner.clicked() {
                self.filter = Some(format);
            }
        }
        ui.add_space(10.0);
        ui.label(
            RichText::new(
                "VST3: ~/Library/Audio/Plug-Ins/VST3 · AU: registered with macOS · CLAP: ~/Library/Audio/Plug-Ins/CLAP",
            )
            .small()
            .weak(),
        );
    }

    fn draw_search(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Search").weak().small());
            ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .hint_text("Name or vendor")
                    .desired_width(260.0),
            );
            if !self.search.is_empty() && ui.small_button("Clear").clicked() {
                self.search.clear();
            }
        });
    }

    fn filtered(&self) -> Vec<host::PluginDescriptor> {
        let needle = self.search.trim().to_lowercase();
        self.catalog
            .as_ref()
            .map(|catalog| {
                catalog
                    .plugins()
                    .into_iter()
                    .filter(|plugin| self.filter.is_none_or(|format| plugin.format == format))
                    .filter(|plugin| {
                        needle.is_empty()
                            || plugin.name.to_lowercase().contains(&needle)
                            || plugin.vendor.to_lowercase().contains(&needle)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn draw_list(&mut self, ui: &mut egui::Ui, p: &Project, actions: &mut Vec<Action>) {
        let plugins = self.filtered();
        if plugins.is_empty() {
            ui.add_space(30.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new(if self.scanning {
                        "Scanning plug-ins…"
                    } else {
                        "No plug-ins found"
                    })
                    .size(15.0),
                );
                if !self.scanning {
                    ui.label(
                        RichText::new(
                            "Install a VST3, Audio Unit or CLAP plug-in, then scan again.",
                        )
                        .weak(),
                    );
                }
            });
            return;
        }
        egui::ScrollArea::vertical()
            .id_salt("plugin_list")
            .max_height(ui.available_height().max(220.0))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                for plugin in &plugins {
                    let selected = self.selected.as_deref() == Some(plugin.key.as_str());
                    let (rect, response) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), 32.0),
                        egui::Sense::click(),
                    );
                    let painter = ui.painter();
                    if selected || response.hovered() {
                        painter.rect_filled(
                            rect,
                            3.0,
                            if selected {
                                Color32::from_gray(0x4a)
                            } else {
                                Color32::from_gray(0x34)
                            },
                        );
                    }
                    painter.text(
                        rect.left_center() + egui::vec2(10.0, -7.0),
                        egui::Align2::LEFT_CENTER,
                        &plugin.name,
                        egui::FontId::proportional(13.0),
                        if selected {
                            Color32::WHITE
                        } else {
                            crate::theme::TEXT
                        },
                    );
                    let vendor = if plugin.vendor.trim().is_empty() {
                        plugin.kind.label().to_string()
                    } else {
                        format!("{} · {}", plugin.vendor, plugin.kind.label())
                    };
                    painter.text(
                        rect.left_center() + egui::vec2(10.0, 8.0),
                        egui::Align2::LEFT_CENTER,
                        vendor,
                        egui::FontId::proportional(10.0),
                        Color32::from_gray(0x8f),
                    );
                    let badge = egui::Rect::from_center_size(
                        rect.right_center() - egui::vec2(28.0, 0.0),
                        egui::vec2(38.0, 16.0),
                    );
                    painter.rect_filled(
                        badge,
                        3.0,
                        format_color(plugin.format).gamma_multiply(0.18),
                    );
                    painter.text(
                        badge.center(),
                        egui::Align2::CENTER_CENTER,
                        plugin.format.badge(),
                        egui::FontId::monospace(9.5),
                        format_color(plugin.format),
                    );
                    if response.clicked() {
                        self.selected = Some(plugin.key.clone());
                    }
                    if response.double_clicked() {
                        if plugin.kind == host::Kind::Instrument {
                            if p.tracks.get(p.selected_track).is_some() {
                                actions.push(Action::Insert {
                                    target: Target::Track(p.selected_track),
                                    role: Role::Instrument,
                                    key: plugin.key.clone(),
                                });
                            }
                        } else {
                            let target = if p.tracks.get(p.selected_track).is_some() {
                                Target::Track(p.selected_track)
                            } else {
                                Target::Master
                            };
                            actions.push(Action::Insert {
                                target,
                                role: Role::Effect,
                                key: plugin.key.clone(),
                            });
                        }
                    }
                }
            });
    }

    fn selected_plugin(&self) -> Option<host::PluginDescriptor> {
        let key = self.selected.as_deref()?;
        self.catalog
            .as_ref()?
            .plugins()
            .into_iter()
            .find(|plugin| plugin.key == key)
    }

    fn draw_details(&mut self, ui: &mut egui::Ui, p: &Project, actions: &mut Vec<Action>) {
        ui.set_width(268.0);
        ui.add_space(4.0);
        if let Some(plugin) = self.selected_plugin() {
            ui.label(RichText::new(&plugin.name).size(18.0).strong());
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(plugin.format.badge())
                        .monospace()
                        .color(format_color(plugin.format)),
                );
                if !plugin.vendor.is_empty() {
                    ui.label(RichText::new(&plugin.vendor).weak());
                }
                if !plugin.version.is_empty() {
                    ui.label(RichText::new(format!("v{}", plugin.version)).weak().small());
                }
            });
            ui.add_space(8.0);
            ui.label(RichText::new(plugin.kind.label()).weak());
            if !plugin.categories.is_empty() {
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    for category in plugin.categories.iter().take(6) {
                        ui.label(
                            RichText::new(category)
                                .small()
                                .background_color(Color32::from_gray(0x35)),
                        );
                    }
                });
            }
            if let Some(path) = &plugin.path {
                ui.add_space(10.0);
                ui.label(RichText::new("Location").weak().small());
                ui.label(RichText::new(path).monospace().small());
            }
            ui.add_space(14.0);
            if plugin.kind == host::Kind::Instrument {
                if p.tracks.get(p.selected_track).is_some()
                    && ui
                        .add_sized(
                            [ui.available_width(), 30.0],
                            egui::Button::new(format!(
                                "Insert Instrument on {}",
                                Target::Track(p.selected_track).label(p)
                            )),
                        )
                        .clicked()
                {
                    actions.push(Action::Insert {
                        target: Target::Track(p.selected_track),
                        role: Role::Instrument,
                        key: plugin.key.clone(),
                    });
                }
            } else {
                if ui
                    .add_sized(
                        [ui.available_width(), 30.0],
                        egui::Button::new("Insert on Stereo Out"),
                    )
                    .clicked()
                {
                    actions.push(Action::Insert {
                        target: Target::Master,
                        role: Role::Effect,
                        key: plugin.key.clone(),
                    });
                }
                if p.tracks.get(p.selected_track).is_some()
                    && ui
                        .add_sized(
                            [ui.available_width(), 30.0],
                            egui::Button::new(format!(
                                "Insert on {}",
                                Target::Track(p.selected_track).label(p)
                            )),
                        )
                        .clicked()
                {
                    actions.push(Action::Insert {
                        target: Target::Track(p.selected_track),
                        role: Role::Effect,
                        key: plugin.key.clone(),
                    });
                }
            }
        } else {
            ui.add_space(20.0);
            ui.label(RichText::new("Select a plug-in to see its details.").weak());
        }

        ui.add_space(18.0);
        ui.separator();
        self.draw_hosted_section(ui, p, Target::Master, Role::Effect, actions);
        if p.tracks.get(p.selected_track).is_some() {
            ui.add_space(10.0);
            self.draw_hosted_section(
                ui,
                p,
                Target::Track(p.selected_track),
                Role::Effect,
                actions,
            );
            ui.add_space(10.0);
            self.draw_hosted_section(
                ui,
                p,
                Target::Track(p.selected_track),
                Role::Instrument,
                actions,
            );
        }
    }

    fn draw_hosted_section(
        &mut self,
        ui: &mut egui::Ui,
        p: &Project,
        target: Target,
        role: Role,
        actions: &mut Vec<Action>,
    ) {
        let what = match role {
            Role::Effect => "Hosted plug-in",
            Role::Instrument => "Instrument",
        };
        ui.add_space(10.0);
        ui.label(
            RichText::new(format!("{what} on {}", target.label(p)))
                .weak()
                .small(),
        );
        let Some(saved) = insert_of(p, target, role) else {
            ui.add_space(4.0);
            ui.label(
                RichText::new(match role {
                    Role::Effect => "No hosted plug-in.",
                    Role::Instrument => "No hosted instrument.",
                })
                .weak(),
            );
            return;
        };
        ui.add_space(4.0);
        ui.label(RichText::new(&saved.name).size(15.0).strong());
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(saved.format.badge())
                    .monospace()
                    .color(format_color(saved.format)),
            );
            let mut bypass = saved.bypass;
            if ui.checkbox(&mut bypass, "Bypass").changed() {
                actions.push(Action::Bypass {
                    target,
                    role,
                    on: bypass,
                });
            }
        });
        ui.horizontal(|ui| {
            if ui.button("Controls…").clicked() {
                actions.push(Action::OpenControls { target, role });
            }
            if ui.button("Plug-in editor").clicked() {
                actions.push(Action::OpenEditor { target, role });
            }
            if ui.button("Remove").clicked() {
                actions.push(Action::Remove { target, role });
            }
        });
    }

    fn draw_parameters(
        &mut self,
        ctx: &egui::Context,
        p: &mut Project,
        target: Target,
        role: Role,
        actions: &mut Vec<Action>,
    ) {
        let title = insert_of(p, target, role)
            .map_or_else(|| "Plug-in".to_string(), |saved| saved.name.clone());
        let beat = p.position_beats;
        let mut open = self.window == Some((target, role));
        egui::Window::new(format!("{title} · {}", target.label(p)))
            .id(egui::Id::unique("plugin_controls"))
            .open(&mut open)
            .default_size([480.0, 540.0])
            .min_size([360.0, 260.0])
            .resizable(true)
            .show(ctx, |ui| {
                let Some(saved) = insert_of(p, target, role) else {
                    ui.label(RichText::new("The plug-in was removed.").weak());
                    return;
                };
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(saved.format.badge())
                            .monospace()
                            .color(format_color(saved.format)),
                    );
                    ui.label(RichText::new(&saved.vendor).weak());
                    let mut bypass = saved.bypass;
                    if ui.checkbox(&mut bypass, "Bypass").changed() {
                        actions.push(Action::Bypass { target, role, on: bypass });
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Reset").clicked() {
                            actions.push(Action::ResetParameters { target, role });
                        }
                        if ui.button("Editor").clicked() {
                            actions.push(Action::OpenEditor { target, role });
                        }
                    });
                });
                ui.separator();
                let Some(active) = self.active.get_mut(&(target, role)) else {
                    ui.add_space(10.0);
                    ui.label(RichText::new("The plug-in is not loaded.").weak());
                    return;
                };
                if active.parameters.is_empty() {
                    ui.add_space(10.0);
                    ui.label(RichText::new("This plug-in exposes no parameters.").weak());
                    return;
                }
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (param, parameter) in active.parameters.iter_mut().enumerate() {
                        let Some(stored) = parameter.value else {
                            continue;
                        };
                        // Read-mode lanes move the control; the engine's automation reaches the
                        // plug-in through the host adapter, so the value shown follows it.
                        // Automation targets carry a u8 parameter index; a plug-in with more
                        // than 256 parameters can still be controlled, just not automated.
                        let auto_param = u8::try_from(param).ok();
                        // Instruments do not have automation lanes yet.
                        let auto = match role {
                            Role::Effect => auto_param.map(|param| AutoTarget::Fx {
                                slot: HOSTED_SLOT,
                                param,
                            }),
                            Role::Instrument => None,
                        };
                        let track_i = match target {
                            Target::Track(i) => Some(i),
                            Target::Master => None,
                        };
                        let played = auto
                            .zip(track_i)
                            .and_then(|(auto, i)| p.tracks.get(i).and_then(|t| t.auto.played(auto, beat)));
                        let editable = match auto {
                            Some(auto) => track_i.is_none_or(|i| {
                                p.tracks
                                    .get(i)
                                    .is_some_and(|track| automation::editable(track, auto))
                            }),
                            None => true,
                        };
                        let automated = auto.is_some_and(|auto| {
                            track_i.and_then(|i| p.tracks.get(i)).is_some_and(|track| {
                                !track.auto.lane(auto).is_empty()
                            })
                        });
                        ui.horizontal(|ui| {
                            let label = ui.add_sized(
                                [148.0, 18.0],
                                egui::Label::new(RichText::new(&parameter.title).size(12.0))
                                    .truncate(),
                            );
                            if automated {
                                label.on_hover_text("Automated");
                            }
                            let mut v = played.map(f64::from).unwrap_or(stored);
                            let mut slider = egui::Slider::new(&mut v, 0.0..=1.0)
                                .show_value(false)
                                .clamping(egui::SliderClamping::Always);
                            if parameter.discrete && parameter.steps > 0 {
                                slider = slider.step_by(1.0 / f64::from(parameter.steps));
                            }
                            let response =
                                ui.add_enabled_ui(editable, |ui| ui.add_sized([220.0, 18.0], slider));
                            let response = response.inner;
                            if response.changed() {
                                parameter.value = Some(v);
                                actions.push(Action::SetParameter {
                                    target,
                                    role,
                                    id: parameter.id,
                                    param: auto_param,
                                    value: v,
                                    ended: !response.dragged(),
                                });
                            } else if response.drag_stopped() {
                                actions.push(Action::SetParameter {
                                    target,
                                    role,
                                    id: parameter.id,
                                    param: auto_param,
                                    value: v,
                                    ended: true,
                                });
                            }
                            let text = if parameter.units.is_empty() {
                                format!("{:.0} %", v * 100.0)
                            } else {
                                format!("{:.0} % {}", v * 100.0, parameter.units)
                            };
                            ui.add_sized(
                                [70.0, 18.0],
                                egui::Label::new(RichText::new(text).monospace().small()),
                            );
                        });
                    }
                });
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "Values are the plug-in's normalized parameters. Use the plug-in editor for its own controls.",
                    )
                    .weak()
                    .small(),
                );
            });
        if !open && self.window == Some((target, role)) {
            actions.push(Action::CloseControls);
        }
        if open {
            self.window = Some((target, role));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Track, TrackKind};

    fn insert(key: &str) -> HostedInsert {
        HostedInsert {
            key: key.to_string(),
            name: key.to_string(),
            vendor: String::new(),
            format: host::Format::Clap,
            bypass: false,
            state: Vec::new(),
            latency: 0,
            parameters: Vec::new(),
        }
    }

    #[test]
    fn a_moved_insert_is_found_under_its_new_track_index() {
        let mut p = Project::empty();
        p.tracks.clear();
        p.master_hosted = None;
        let mut a = Track::new("A", TrackKind::Audio);
        a.name = "A".into();
        let mut b = Track::new("B", TrackKind::Audio);
        b.name = "B".into();
        b.hosted = Some(insert("clap:test"));
        p.tracks.push(a);
        p.tracks.push(b);
        assert_eq!(
            target_with_key(&p, "clap:test", Role::Effect),
            Some(Target::Track(1))
        );
        // Moving the track moves the insert with it.
        let moved = p.tracks.remove(1);
        p.tracks.insert(0, moved);
        assert_eq!(
            target_with_key(&p, "clap:test", Role::Effect),
            Some(Target::Track(0))
        );
        // Deleting the track leaves nothing to point at: the control is unloaded, not reused.
        p.tracks.clear();
        assert_eq!(target_with_key(&p, "clap:test", Role::Effect), None);
        // The master insert is found too.
        p.master_hosted = Some(insert("au:master"));
        assert_eq!(
            target_with_key(&p, "au:master", Role::Effect),
            Some(Target::Master)
        );
    }

    fn descriptor(name: &str, kind: host::Kind) -> host::PluginDescriptor {
        host::PluginDescriptor {
            key: format!("vst3:{name}"),
            format: host::Format::Vst3,
            name: name.to_string(),
            vendor: String::new(),
            version: String::new(),
            kind,
            categories: Vec::new(),
            path: None,
        }
    }

    #[test]
    fn the_audio_fx_picker_offers_effects_but_not_instruments() {
        let plugins = vec![
            descriptor("Surge XT", host::Kind::Instrument),
            descriptor("Surge XT Effects", host::Kind::Effect),
        ];
        let choices = picker_choices(plugins.clone(), Role::Effect);
        assert_eq!(choices.len(), 1);
        assert_eq!(choices[0].name, "Surge XT Effects");
        assert_eq!(choices[0].key, "vst3:Surge XT Effects");
        assert_eq!(choices[0].format, host::Format::Vst3.label());

        let instruments = picker_choices(plugins, Role::Instrument);
        assert_eq!(instruments.len(), 1);
        assert_eq!(instruments[0].name, "Surge XT");
    }
}
