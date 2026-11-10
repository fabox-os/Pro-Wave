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
    Insert {
        target: Target,
        key: String,
    },
    Remove(Target),
    Bypass {
        target: Target,
        on: bool,
    },
    OpenControls(Target),
    CloseControls,
    OpenEditor(Target),
    SetParameter {
        target: Target,
        id: u64,
        param: Option<u8>,
        value: f64,
        ended: bool,
    },
    ResetParameters(Target),
}

/// The plug-in runtime: catalog, live control chains, and window state.
#[derive(Default)]
pub struct Plugins {
    catalog: Option<host::Catalog>,
    scanning: bool,
    scan_rx: Option<Receiver<Result<host::Catalog, host::Error>>>,
    /// One control chain per strip with a hosted plug-in.
    active: HashMap<Target, Active>,
    /// A project's plug-ins saved while the catalog is not scanned yet.
    pending: Vec<(Target, HostedInsert)>,
    pub manager_open: bool,
    /// The target whose parameters window is open.
    window: Option<Target>,
    filter: Option<host::Format>,
    search: String,
    selected: Option<String>,
    error: Option<String>,
    /// A hosted parameter changed since the last save/snapshot.
    dirty: bool,
    /// When the live parameter lists were last fetched for change detection.
    last_poll: Option<Instant>,
}

/// A reborrow of the engine link, so the same option can be used more than once.
fn reborrow<'a>(sync: &'a mut Option<&mut EngineSync>) -> Option<&'a mut EngineSync> {
    sync.as_mut().map(|sync| &mut **sync)
}

/// The hosted plug-in of `target`, if the project has one.
fn insert_of(p: &Project, target: Target) -> Option<&HostedInsert> {
    match target {
        Target::Master => p.master_hosted.as_ref(),
        Target::Track(i) => p.tracks.get(i).and_then(|t| t.hosted.as_ref()),
    }
}

fn insert_of_mut(p: &mut Project, target: Target) -> Option<&mut HostedInsert> {
    match target {
        Target::Master => p.master_hosted.as_mut(),
        Target::Track(i) => p.tracks.get_mut(i).and_then(|t| t.hosted.as_mut()),
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
            let targets: Vec<Target> = self.active.keys().copied().collect();
            for target in targets {
                self.refresh_now(target);
            }
            self.last_poll = Some(Instant::now());
        }

        let mut actions = Vec::new();
        if self.manager_open {
            self.draw_manager(ctx, p, &mut actions);
        }
        if let Some(target) = self.window {
            self.draw_parameters(ctx, p, target, &mut actions);
            self.refresh(target);
            ctx.request_repaint_after(Duration::from_millis(500));
        }
        self.apply(actions, p, s, reborrow(&mut sync));
    }

    /// After opening a project: load every saved plug-in, or remember them until the scan
    /// finishes.
    pub fn restore(&mut self, p: &mut Project, mut sync: Option<&mut EngineSync>, s: &mut UiState) {
        // A project replace invalidates every hosted stage the engine still has.
        if let Some(sync) = reborrow(&mut sync) {
            sync.clear_hosted();
        }
        self.active.clear();
        self.pending.clear();
        self.window = None;
        self.dirty = false;
        if let Some(saved) = p.master_hosted.clone() {
            self.pending.push((Target::Master, saved));
        }
        for (i, track) in p.tracks.iter().enumerate() {
            if let Some(saved) = track.hosted.clone() {
                self.pending.push((Target::Track(i), saved));
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
    /// removed rather than processing another track's audio. The plug-in is unloaded (re-insert
    /// it, or reopen the project); moving the live instance to the new index is a follow-up.
    fn reconcile(&mut self, p: &Project, mut sync: Option<&mut EngineSync>) {
        if self.active.is_empty() {
            return;
        }
        let stale: Vec<Target> = self
            .active
            .iter()
            .filter(|(target, active)| {
                insert_of(p, **target).is_none_or(|saved| saved.key != active.key)
            })
            .map(|(target, _)| *target)
            .collect();
        for target in stale {
            if let Some(sync) = reborrow(&mut sync) {
                match target {
                    Target::Master => {
                        sync.set_master_hosted(None, false);
                    }
                    Target::Track(i) => {
                        sync.set_track_hosted(i, None, false);
                    }
                }
            }
            self.active.remove(&target);
            if self.window == Some(target) {
                self.window = None;
            }
        }
    }

    /// Saves every live plug-in's state into the project before it is written to disk.
    pub fn snapshot(&mut self, p: &mut Project) {
        let states: Vec<(Target, Vec<u8>)> = self
            .active
            .iter_mut()
            .filter_map(|(target, active)| {
                active.control.state().ok().map(|state| (*target, state))
            })
            .collect();
        for (target, state) in states {
            if let Some(saved) = insert_of_mut(p, target) {
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
                self.catalog = Some(catalog);
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
        self.error = None;
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
        while let Some((target, saved)) = self.pending.pop() {
            let Some(sync) = reborrow(&mut sync) else {
                self.pending.clear();
                return;
            };
            if let Err(error) = self.load(target, &saved.key, Some(&saved.state), p, sync) {
                self.error = Some(error.clone());
                s.status = Some(error);
            }
        }
    }

    /// Instantiates `key`, sends its realtime adapter to the engine and keeps its controls.
    fn load(
        &mut self,
        target: Target,
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
        let previous = insert_of(p, target).cloned();
        // Replacing the same plug-in keeps its settings; a different one starts fresh.
        let state = state.map(<[u8]>::to_vec).or_else(|| {
            previous
                .as_ref()
                .filter(|previous| previous.key == key && !previous.state.is_empty())
                .map(|previous| previous.state.clone())
        });
        let hosted = catalog
            .instantiate(
                key,
                sync.sample_rate(),
                host::DEFAULT_QUANTUM,
                state.as_deref(),
            )
            .map_err(|error| error.to_string())?;
        let host::Hosted {
            mut control,
            effect,
            latency,
            parameters,
        } = hosted;
        let bypass = previous.as_ref().is_some_and(|saved| saved.bypass);
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
            target,
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
        match target {
            Target::Master => p.master_hosted = Some(insert),
            Target::Track(i) => {
                if let Some(track) = p.tracks.get_mut(i) {
                    track.hosted = Some(insert);
                }
            }
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
                Action::Insert { target, key } => {
                    let Some(sync) = reborrow(&mut sync) else {
                        self.error = Some("there is no audio output".to_string());
                        continue;
                    };
                    match self.load(target, &key, None, p, sync) {
                        Ok(()) => {
                            s.status = Some(format!(
                                "inserted {} on {}",
                                insert_of(p, target).map_or("the plug-in", |i| i.name.as_str()),
                                target.label(p)
                            ));
                        }
                        Err(error) => {
                            self.error = Some(error.clone());
                            s.status = Some(error);
                        }
                    }
                }
                Action::Remove(target) => {
                    if let Some(sync) = reborrow(&mut sync) {
                        match target {
                            Target::Master => {
                                sync.set_master_hosted(None, false);
                            }
                            Target::Track(i) => {
                                sync.set_track_hosted(i, None, false);
                            }
                        }
                    }
                    self.active.remove(&target);
                    if self.window == Some(target) {
                        self.window = None;
                    }
                    match target {
                        Target::Master => p.master_hosted = None,
                        Target::Track(i) => {
                            if let Some(track) = p.tracks.get_mut(i) {
                                track.hosted = None;
                            }
                        }
                    }
                }
                Action::Bypass { target, on } => {
                    if let Some(saved) = insert_of_mut(p, target) {
                        saved.bypass = on;
                    }
                    if let Some(active) = self.active.get_mut(&target) {
                        if let Err(error) = active.control.set_bypass(on) {
                            self.error = Some(error.to_string());
                        }
                    }
                    if let Some(sync) = reborrow(&mut sync) {
                        match target {
                            Target::Master => {
                                sync.set_master_hosted_bypass(on);
                            }
                            Target::Track(i) => {
                                sync.set_track_hosted_bypass(i, on);
                            }
                        }
                    }
                }
                Action::OpenControls(target) => {
                    self.window = Some(target);
                    self.refresh_now(target);
                }
                Action::CloseControls => {
                    self.window = None;
                    self.snapshot(p);
                }
                Action::OpenEditor(target) => {
                    if let Some(active) = self.active.get_mut(&target) {
                        if let Err(error) = active.control.open_editor() {
                            self.error = Some(error.to_string());
                        }
                    }
                }
                Action::SetParameter {
                    target,
                    id,
                    param,
                    value,
                    ended,
                } => {
                    if let Some(active) = self.active.get_mut(&target) {
                        match active.control.set_parameter(id, value) {
                            Ok(()) => self.dirty = true,
                            Err(error) => self.error = Some(error.to_string()),
                        }
                    }
                    if let Some(param) = param {
                        if let Some(saved) = insert_of_mut(p, target) {
                            if let Some(parameter) = saved.parameters.get_mut(param as usize) {
                                parameter.value = value as f32;
                            }
                        }
                        if let Target::Track(i) = target {
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
                Action::ResetParameters(target) => {
                    let defaults: Vec<(u64, f64)> = self
                        .active
                        .get(&target)
                        .map(|active| {
                            active
                                .parameters
                                .iter()
                                .filter_map(|parameter| {
                                    parameter.default.map(|value| (parameter.id, value))
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    for (id, value) in defaults {
                        if let Some(active) = self.active.get_mut(&target) {
                            let _ = active.control.set_parameter(id, value);
                            self.dirty = true;
                            if let Some(parameter) =
                                active.parameters.iter_mut().find(|p| p.id == id)
                            {
                                parameter.value = Some(value);
                            }
                        }
                    }
                }
            }
        }
    }

    fn refresh_now(&mut self, target: Target) {
        let Some(active) = self.active.get_mut(&target) else {
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

    fn refresh(&mut self, target: Target) {
        let due = self
            .active
            .get(&target)
            .is_some_and(|active| active.last_refresh.elapsed() >= Duration::from_millis(700));
        if due {
            self.refresh_now(target);
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
                        let target = if p.tracks.get(p.selected_track).is_some() {
                            Target::Track(p.selected_track)
                        } else {
                            Target::Master
                        };
                        actions.push(Action::Insert {
                            target,
                            key: plugin.key.clone(),
                        });
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
            if ui
                .add_sized(
                    [ui.available_width(), 30.0],
                    egui::Button::new("Insert on Stereo Out"),
                )
                .clicked()
            {
                actions.push(Action::Insert {
                    target: Target::Master,
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
                    key: plugin.key.clone(),
                });
            }
        } else {
            ui.add_space(20.0);
            ui.label(RichText::new("Select a plug-in to see its details.").weak());
        }

        ui.add_space(18.0);
        ui.separator();
        self.draw_hosted_section(ui, p, Target::Master, actions);
        if p.tracks.get(p.selected_track).is_some() {
            ui.add_space(10.0);
            self.draw_hosted_section(ui, p, Target::Track(p.selected_track), actions);
        }
    }

    fn draw_hosted_section(
        &mut self,
        ui: &mut egui::Ui,
        p: &Project,
        target: Target,
        actions: &mut Vec<Action>,
    ) {
        ui.add_space(10.0);
        ui.label(
            RichText::new(format!("On {}", target.label(p)))
                .weak()
                .small(),
        );
        let Some(saved) = insert_of(p, target) else {
            ui.add_space(4.0);
            ui.label(RichText::new("No hosted plug-in.").weak());
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
                actions.push(Action::Bypass { target, on: bypass });
            }
        });
        ui.horizontal(|ui| {
            if ui.button("Controls…").clicked() {
                actions.push(Action::OpenControls(target));
            }
            if ui.button("Plug-in editor").clicked() {
                actions.push(Action::OpenEditor(target));
            }
            if ui.button("Remove").clicked() {
                actions.push(Action::Remove(target));
            }
        });
    }

    fn draw_parameters(
        &mut self,
        ctx: &egui::Context,
        p: &mut Project,
        target: Target,
        actions: &mut Vec<Action>,
    ) {
        let title =
            insert_of(p, target).map_or_else(|| "Plug-in".to_string(), |saved| saved.name.clone());
        let beat = p.position_beats;
        let mut open = self.window == Some(target);
        egui::Window::new(format!("{title} · {}", target.label(p)))
            .id(egui::Id::unique("plugin_controls"))
            .open(&mut open)
            .default_size([480.0, 540.0])
            .min_size([360.0, 260.0])
            .resizable(true)
            .show(ctx, |ui| {
                let Some(saved) = insert_of(p, target) else {
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
                        actions.push(Action::Bypass { target, on: bypass });
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Reset").clicked() {
                            actions.push(Action::ResetParameters(target));
                        }
                        if ui.button("Editor").clicked() {
                            actions.push(Action::OpenEditor(target));
                        }
                    });
                });
                ui.separator();
                let Some(active) = self.active.get_mut(&target) else {
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
                        let auto = auto_param.map(|param| AutoTarget::Fx {
                            slot: HOSTED_SLOT,
                            param,
                        });
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
                                    id: parameter.id,
                                    param: auto_param,
                                    value: v,
                                    ended: !response.dragged(),
                                });
                            } else if response.drag_stopped() {
                                actions.push(Action::SetParameter {
                                    target,
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
        if !open && self.window == Some(target) {
            actions.push(Action::CloseControls);
        }
        if open {
            self.window = Some(target);
        }
    }
}
