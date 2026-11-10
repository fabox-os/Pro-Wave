//! Hosted VST3, Audio Unit and CLAP plug-ins: the Plug-in Manager window and the controls of
//! the plug-in on the Stereo Out.
//!
//! The plug-in itself runs in a helper process (see `prowave_plugin_host`); this module owns
//! the control side that stays on the UI thread, while the realtime adapter is handed to the
//! engine. Projects keep the class key and the plug-in's state blob, so a reopened project loads
//! the same plug-in with the same settings when it is still installed.

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use egui::{Color32, RichText};
use prowave_plugin_host as host;

use crate::engine_sync::EngineSync;
use crate::model::{HostedInsert, Project};
use crate::UiState;

/// Accent color per format: VST3 green, Audio Units amber, CLAP blue.
fn format_color(format: host::Format) -> Color32 {
    match format {
        host::Format::Vst3 => Color32::from_rgb(0x6f, 0xcf, 0x97),
        host::Format::AudioUnit => Color32::from_rgb(0xe8, 0xb4, 0x5a),
        host::Format::Clap => Color32::from_rgb(0x7f, 0xa8, 0xe8),
    }
}

/// What the windows asked for, applied after drawing so borrows stay simple.
enum Action {
    Scan,
    Insert(String),
    Remove,
    Bypass(bool),
    OpenControls,
    CloseControls,
    OpenEditor,
    SetParameter { id: u64, value: f64 },
    ResetParameters,
}

/// The plug-in runtime: catalog, the live control chain, and window state.
pub struct Plugins {
    catalog: Option<host::Catalog>,
    scanning: bool,
    scan_rx: Option<Receiver<Result<host::Catalog, host::Error>>>,
    control: Option<host::Control>,
    parameters: Vec<host::Parameter>,
    /// A project's plug-in saved while the catalog is not scanned yet.
    pending: Option<HostedInsert>,
    pub manager_open: bool,
    pub window_open: bool,
    filter: Option<host::Format>,
    search: String,
    selected: Option<String>,
    error: Option<String>,
    last_parameter_refresh: Instant,
}

impl Default for Plugins {
    fn default() -> Self {
        Self {
            catalog: None,
            scanning: false,
            scan_rx: None,
            control: None,
            parameters: Vec::new(),
            pending: None,
            manager_open: false,
            window_open: false,
            filter: None,
            search: String::new(),
            selected: None,
            error: None,
            last_parameter_refresh: Instant::now() - Duration::from_secs(60),
        }
    }
}

/// A reborrow of the engine link, so the same option can be used more than once.
fn reborrow<'a>(sync: &'a mut Option<&mut EngineSync>) -> Option<&'a mut EngineSync> {
    sync.as_mut().map(|sync| &mut **sync)
}

impl Plugins {
    /// Called every frame: polls the scanner, restores a pending project plug-in, and draws the
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
        if self.pending.is_some() && self.catalog.is_some() {
            self.restore_pending(p, reborrow(&mut sync), s);
        }

        let mut actions = Vec::new();
        if self.manager_open {
            self.draw_manager(ctx, p, &mut actions);
        }
        if self.window_open {
            self.draw_parameters(ctx, p, &mut actions);
            self.refresh_parameters();
            ctx.request_repaint_after(Duration::from_millis(500));
        }
        self.apply(actions, p, s, reborrow(&mut sync));
    }

    /// After opening a project: load the saved plug-in, or remember it until the scan finishes.
    pub fn restore(&mut self, p: &mut Project, mut sync: Option<&mut EngineSync>, s: &mut UiState) {
        // A project replace invalidates every hosted stage the engine still has.
        if let Some(sync) = reborrow(&mut sync) {
            sync.clear_hosted();
        }
        self.control = None;
        self.parameters.clear();
        self.window_open = false;
        let Some(saved) = p.master_hosted.clone() else {
            self.pending = None;
            return;
        };
        if self.catalog.is_none() {
            self.pending = Some(saved);
            if !self.scanning && host::available() {
                self.scan();
            }
            return;
        }
        let Some(sync) = sync else {
            self.error = Some("the hosted plug-in needs an audio output to load".to_string());
            return;
        };
        match self.load(&saved.key, Some(&saved.state), p, sync) {
            Ok(()) => self.pending = None,
            Err(error) => {
                self.error = Some(error.clone());
                s.status = Some(error);
                self.pending = None;
            }
        }
    }

    /// Saves the live plug-in's state into the project before it is written to disk.
    pub fn snapshot(&mut self, p: &mut Project) {
        let (Some(control), Some(saved)) = (self.control.as_mut(), p.master_hosted.as_mut()) else {
            return;
        };
        if let Ok(state) = control.state() {
            saved.state = state;
        }
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

    fn restore_pending(&mut self, p: &mut Project, sync: Option<&mut EngineSync>, s: &mut UiState) {
        let Some(saved) = self.pending.clone() else {
            return;
        };
        let Some(sync) = sync else {
            self.pending = None;
            return;
        };
        match self.load(&saved.key, Some(&saved.state), p, sync) {
            Ok(()) => self.pending = None,
            Err(error) => {
                self.error = Some(error.clone());
                s.status = Some(error);
                self.pending = None;
            }
        }
    }

    /// Instantiates `key`, sends its realtime adapter to the engine and keeps its controls.
    fn load(
        &mut self,
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
        let previous = p.master_hosted.clone();
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
        if !sync.set_master_hosted(Some(effect), bypass) {
            return Err("the audio engine is busy; try again".to_string());
        }
        let fresh = control.state().unwrap_or_default();
        self.control = Some(control);
        self.parameters = parameters;
        self.last_parameter_refresh = Instant::now();
        let mut insert = HostedInsert::new(&descriptor, fresh);
        insert.bypass = bypass;
        if let Some(state) = state {
            insert.state = state;
        }
        p.master_hosted = Some(insert);
        tracing::info!(
            "loaded {} ({}) with {} frames of latency",
            descriptor.name,
            descriptor.format.label(),
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
                Action::Insert(key) => {
                    let Some(sync) = reborrow(&mut sync) else {
                        self.error = Some("there is no audio output".to_string());
                        continue;
                    };
                    match self.load(&key, None, p, sync) {
                        Ok(()) => {
                            s.status = Some(format!(
                                "inserted {} on Stereo Out",
                                p.master_hosted
                                    .as_ref()
                                    .map_or("the plug-in", |insert| insert.name.as_str())
                            ));
                        }
                        Err(error) => {
                            self.error = Some(error.clone());
                            s.status = Some(error);
                        }
                    }
                }
                Action::Remove => {
                    if let Some(sync) = reborrow(&mut sync) {
                        sync.set_master_hosted(None, false);
                    }
                    self.control = None;
                    self.parameters.clear();
                    self.window_open = false;
                    p.master_hosted = None;
                }
                Action::Bypass(bypass) => {
                    if let Some(saved) = p.master_hosted.as_mut() {
                        saved.bypass = bypass;
                    }
                    if let Some(control) = self.control.as_mut() {
                        if let Err(error) = control.set_bypass(bypass) {
                            self.error = Some(error.to_string());
                        }
                    }
                    if let Some(sync) = reborrow(&mut sync) {
                        sync.set_master_hosted_bypass(bypass);
                    }
                }
                Action::OpenControls => {
                    self.window_open = true;
                    self.refresh_parameters_now();
                }
                Action::CloseControls => {
                    self.window_open = false;
                    self.snapshot(p);
                }
                Action::OpenEditor => {
                    if let Some(control) = self.control.as_mut() {
                        if let Err(error) = control.open_editor() {
                            self.error = Some(error.to_string());
                        }
                    }
                }
                Action::SetParameter { id, value } => {
                    if let Some(control) = self.control.as_mut() {
                        if let Err(error) = control.set_parameter(id, value) {
                            self.error = Some(error.to_string());
                        }
                    }
                }
                Action::ResetParameters => {
                    let defaults: Vec<(u64, f64)> = self
                        .parameters
                        .iter()
                        .filter_map(|parameter| {
                            parameter.default.map(|value| (parameter.id, value))
                        })
                        .collect();
                    for (id, value) in defaults {
                        if let Some(control) = self.control.as_mut() {
                            let _ = control.set_parameter(id, value);
                        }
                        if let Some(parameter) = self
                            .parameters
                            .iter_mut()
                            .find(|parameter| parameter.id == id)
                        {
                            parameter.value = Some(value);
                        }
                    }
                }
            }
        }
    }

    fn refresh_parameters_now(&mut self) {
        let Some(control) = self.control.as_mut() else {
            return;
        };
        match control.parameters() {
            Ok(parameters) => self.parameters = parameters,
            Err(error) => self.error = Some(error.to_string()),
        }
        self.last_parameter_refresh = Instant::now();
    }

    fn refresh_parameters(&mut self) {
        if self.last_parameter_refresh.elapsed() >= Duration::from_millis(700) {
            self.refresh_parameters_now();
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
                        self.draw_list(ui, actions);
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

    fn draw_list(&mut self, ui: &mut egui::Ui, _actions: &mut Vec<Action>) {
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
                        _actions.push(Action::Insert(plugin.key.clone()));
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
                actions.push(Action::Insert(plugin.key.clone()));
            }
        } else {
            ui.add_space(20.0);
            ui.label(RichText::new("Select a plug-in to see its details.").weak());
        }

        ui.add_space(18.0);
        ui.separator();
        ui.add_space(10.0);
        ui.label(RichText::new("On Stereo Out").weak().small());
        let Some(saved) = p.master_hosted.as_ref() else {
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
                actions.push(Action::Bypass(bypass));
            }
        });
        ui.horizontal(|ui| {
            if ui.button("Controls…").clicked() {
                actions.push(Action::OpenControls);
            }
            if ui.button("Plug-in editor").clicked() {
                actions.push(Action::OpenEditor);
            }
            if ui.button("Remove").clicked() {
                actions.push(Action::Remove);
            }
        });
    }

    fn draw_parameters(&mut self, ctx: &egui::Context, p: &mut Project, actions: &mut Vec<Action>) {
        let title = p
            .master_hosted
            .as_ref()
            .map_or_else(|| "Plug-in".to_string(), |saved| saved.name.clone());
        let mut open = self.window_open;
        egui::Window::new(format!("{title} · Stereo Out"))
            .id(egui::Id::unique("plugin_controls"))
            .open(&mut open)
            .default_size([480.0, 540.0])
            .min_size([360.0, 260.0])
            .resizable(true)
            .show(ctx, |ui| {
                let Some(saved) = p.master_hosted.as_ref() else {
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
                        actions.push(Action::Bypass(bypass));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Reset").clicked() {
                            actions.push(Action::ResetParameters);
                        }
                        if ui.button("Editor").clicked() {
                            actions.push(Action::OpenEditor);
                        }
                    });
                });
                ui.separator();
                if self.parameters.is_empty() {
                    ui.add_space(10.0);
                    ui.label(RichText::new("This plug-in exposes no parameters.").weak());
                    return;
                }
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for parameter in &mut self.parameters {
                        let Some(value) = parameter.value else {
                            continue;
                        };
                        ui.horizontal(|ui| {
                            ui.add_sized(
                                [148.0, 18.0],
                                egui::Label::new(RichText::new(&parameter.title).size(12.0))
                                    .truncate(),
                            );
                            let mut v = value;
                            let mut slider = egui::Slider::new(&mut v, 0.0..=1.0)
                                .show_value(false)
                                .clamping(egui::SliderClamping::Always);
                            if parameter.discrete && parameter.steps > 0 {
                                slider = slider.step_by(1.0 / f64::from(parameter.steps));
                            }
                            let response = ui.add_sized([220.0, 18.0], slider);
                            if response.changed() {
                                parameter.value = Some(v);
                                actions.push(Action::SetParameter {
                                    id: parameter.id,
                                    value: v,
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
        if !open && self.window_open {
            actions.push(Action::CloseControls);
        }
        self.window_open = open;
    }
}
