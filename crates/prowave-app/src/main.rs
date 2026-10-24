#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! Pro Wave desktop app. Pure Rust (egui/eframe) for Windows, Linux and macOS.
// Debug builds abort if the audio callback allocates (see `prowave-engine` output.rs).
#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC_GUARD: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

mod bench;
mod engine_sync;
mod event_list;
mod file_commands;
mod flex_edit;
mod graphics;
mod groove;
mod i18n;
mod import;
mod inspector;
mod library;
mod marker_track;
mod menus;
mod midi_file;
mod midi_fx;
mod midi_fx_ui;
mod midi_in;
mod midi_out;
mod mixer;
mod model;
mod mtc_in;
mod native_menu;
mod note_edit;
mod patches;
mod pattern;
mod piano_roll;
mod project_io;
mod recent_files;
mod recorder;
mod score;
mod scripter;
mod signature_track;
mod smart_tempo;
mod start_screen;
mod step_editor;
mod sync_menu;
mod takes;
mod tempo_track;
mod theme;
mod track_icons;
mod tracks_area;
mod transform;
mod transform_ui;
mod transport;
mod ui_scale;
#[cfg(test)]
mod ui_test;
mod undo;
mod updater;
mod widgets;
mod window_chrome;

use model::Project;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Bottom {
    Mixer,
    PianoRoll,
}

pub struct UiState {
    pub show_mixer: bool,
    pub show_library: bool,
    pub show_inspector: bool,
    /// List Editors (the Event list on the right).
    pub show_lists: bool,
    pub lists: event_list::ListView,
    /// The MIDI recording in progress.
    pub rec: recorder::Recorder,
    /// Help > Check for Updates… was chosen.
    pub update_check: bool,
    /// Help > Check for Updates Automatically.
    pub update_auto: bool,
    pub bottom: Bottom,
    /// Last import error or engine warning, shown in the menu bar.
    pub status: Option<String>,
    pub tracks: tracks_area::TracksView,
    pub mixer: mixer::MixerView,
    pub inspector: inspector::InspectorView,
    pub library: library::LibraryView,
    pub piano: piano_roll::PianoView,
    /// The MIDI FX window of the selected track is open.
    pub show_midi_fx: bool,
    /// The MIDI Transform window.
    pub transform: transform_ui::TransformView,
    /// The editor panel shows the Score instead of the Piano Roll for ordinary MIDI regions.
    pub score_tab: bool,
    pub score: score::ScoreView,
    /// A note the Score just added is sounding: its pitch and when it stops.
    pub score_heard: Option<(u8, f64)>,
    /// Flex Time and Flex Pitch work running in the background.
    pub flex: flex_edit::FlexState,
    /// The audio Groove Track's hits being read.
    pub groove: groove::GrooveState,
    /// File > Import > MIDI File… or Export > Selection as MIDI File…, run after the menus.
    pub midi_cmd: Option<midi_file::MidiCmd>,
    /// The Pattern and Drummer editors, which take the Piano Roll's place for those regions.
    pub steps: step_editor::StepView,
    /// True while the pointer is over the Piano Roll (it owns Delete/Cmd+R then).
    pub piano_focus: bool,
    /// Live notes from the Piano Roll, forwarded to the engine every frame.
    pub audition: Vec<piano_roll::Audition>,
    /// File menu command picked this frame; run by `App` once the panels are drawn.
    pub file_cmd: Option<file_commands::FileCmd>,
    /// A New / Open waiting for the answer to the "save changes?" prompt.
    pub ask_save: Option<file_commands::FileCmd>,
    /// True while the "revert to the last saved version?" prompt is showing.
    pub ask_revert: bool,
    /// The project picked in File > Open Recent, until `App` has opened it.
    pub recent_pick: Option<std::path::PathBuf>,
    /// File > Open Recent. In memory only until `main` loads the saved list.
    pub recent: recent_files::Recent,
    /// DSP load shown by the LCD's CPU meter (1.0 = full budget); peak-held, falls slowly.
    pub cpu_load: f32,
    /// Light (white) menu bar and menus, toggled by the Smart Controls button.
    pub light_menus: bool,
    /// The Project Chooser, while it covers the window (File > New from Template…).
    pub chooser: Option<start_screen::Chooser>,
    /// The user's interface size (1.0 = 100 %), on top of fitting small windows (`ui_scale`).
    pub ui_scale: f32,
    /// Edit > Undo / Redo picked this frame; run by `App` (which keeps the history).
    pub undo_cmd: Option<undo::UndoCmd>,
    /// Whether there is something to undo / redo (greys the Edit entries).
    pub can_undo: bool,
    pub can_redo: bool,
}

impl UiState {
    fn new() -> Self {
        Self {
            show_mixer: true,
            show_library: true,
            show_inspector: true,
            show_lists: false,
            lists: event_list::ListView::default(),
            rec: recorder::Recorder::default(),
            update_check: false,
            update_auto: true,
            bottom: Bottom::Mixer,
            status: None,
            tracks: tracks_area::TracksView::default(),
            mixer: mixer::MixerView::default(),
            inspector: inspector::InspectorView::default(),
            library: library::LibraryView::default(),
            piano: piano_roll::PianoView::default(),
            steps: step_editor::StepView::default(),
            show_midi_fx: false,
            transform: transform_ui::TransformView::default(),
            score_tab: false,
            score: score::ScoreView::default(),
            score_heard: None,
            flex: flex_edit::FlexState::default(),
            groove: groove::GrooveState::default(),
            midi_cmd: None,
            piano_focus: false,
            audition: Vec::new(),
            file_cmd: None,
            ask_save: None,
            ask_revert: false,
            recent_pick: None,
            recent: recent_files::Recent::default(),
            cpu_load: 0.0,
            light_menus: false,
            chooser: None,
            ui_scale: 1.0,
            undo_cmd: None,
            can_undo: false,
            can_redo: false,
        }
    }

    /// After a project is replaced: drop editor state (selections, open regions) that indexes
    /// into the old project, keeping which panels are shown.
    fn reset_views(&mut self) {
        self.status = None;
        self.tracks = tracks_area::TracksView::default();
        self.mixer = mixer::MixerView::default();
        self.inspector = inspector::InspectorView::default();
        self.piano = piano_roll::PianoView::default();
        self.piano_focus = false;
        self.audition.clear();
    }
}

struct App {
    project: Project,
    ui: UiState,
    sync: Option<engine_sync::EngineSync>,
    /// The `.prowave` file the project was opened from or last saved to.
    path: Option<std::path::PathBuf>,
    /// Content at the last open/save, to detect unsaved changes (see `file_commands`).
    saved: prowave_project::ProjectDoc,
    /// macOS: the menus in the system menu bar (elsewhere they are drawn in the window).
    native_menu: Option<native_menu::NativeMenu>,
    /// `--bench <file>`: measuring startup, memory and DSP load, then closing.
    bench: Option<bench::Probe>,
    /// `--layout-shots <folder>`: pictures of the layout at several window sizes, then closing.
    shots: Option<bench::LayoutShots>,
    /// Edit > Undo / Redo.
    history: undo::History,
    /// Frames since the history last looked for an edit.
    frames_since_record: u32,
    /// The MIDI keyboard (none while benchmarking).
    midi: Option<midi_in::MidiIn>,
    /// Sends the notes of External MIDI tracks to their ports (none while benchmarking).
    midi_out: Option<midi_out::MidiOut>,
    /// Reads MIDI Time Code from the project's port (none while benchmarking).
    mtc_in: Option<mtc_in::MtcIn>,
    /// Makes the transport follow that time code.
    chase: mtc_in::Chase,
    /// Looks for a newer release and installs it (Windows).
    updater: updater::Updater,
}

impl App {
    /// File > Import > MIDI File… and File > Export > Selection as MIDI File…
    fn run_midi_cmd(&mut self, cmd: midi_file::MidiCmd) {
        let (p, s) = (&mut self.project, &mut self.ui);
        match cmd {
            midi_file::MidiCmd::Import => {
                let picked = rfd::FileDialog::new()
                    .add_filter(i18n::tr("MIDI File"), &["mid", "midi", "smf"])
                    .pick_file();
                if let Some(path) = picked {
                    s.status = Some(match midi_file::import_path(p, &path) {
                        Ok(n) => i18n::trf("imported {} tracks from {}", &[&n, &path.display()]),
                        Err(e) => i18n::trf("cannot import {}: {}", &[&path.display(), &e]),
                    });
                }
            }
            midi_file::MidiCmd::Export => {
                // The selected region, or all the MIDI of the project.
                let only = s.tracks.selected.filter(|(t, r)| {
                    p.tracks.get(*t).is_some_and(|tr| {
                        tr.kind != model::TrackKind::Audio && *r < tr.regions.len()
                    })
                });
                let picked = rfd::FileDialog::new()
                    .add_filter(i18n::tr("MIDI File"), &["mid"])
                    .set_file_name(format!("{}.mid", p.name))
                    .save_file();
                if let Some(path) = picked {
                    s.status = Some(match midi_file::export_path(p, only, &path) {
                        Ok(()) => i18n::trf("saved {}", &[&path.display()]),
                        Err(e) => i18n::trf("cannot write {}: {}", &[&path.display(), &e]),
                    });
                }
            }
        }
    }

    fn import(&mut self, path: &std::path::Path, ctx: &egui::Context) {
        if midi_file::is_midi_path(path) {
            self.ui.status = Some(match midi_file::import_path(&mut self.project, path) {
                Ok(n) => i18n::trf("imported {} tracks from {}", &[&n, &path.display()]),
                Err(e) => i18n::trf("cannot import {}: {}", &[&path.display(), &e]),
            });
            return;
        }
        if !import::is_audio_path(path) {
            self.ui.status = Some(i18n::trf("not an audio file: {}", &[&path.display()]));
            return;
        }
        if let Err(e) = import::import_audio(&mut self.project, path, ctx) {
            self.ui.status = Some(i18n::trf("cannot import {}: {}", &[&path.display(), &e]));
        }
    }
}

impl App {
    /// Records finished edits for Undo, and runs Undo / Redo. An edit is looked for when a
    /// gesture ends (button let go, key, menu) and every half second (menus in the macOS menu
    /// bar send no input events), never in the middle of a drag.
    fn track_history(&mut self, ctx: &egui::Context) {
        self.frames_since_record += 1;
        let (gesture_ended, dragging) = ctx.input(|i| {
            let ended = i.pointer.any_released()
                || i.events.iter().any(|e| {
                    matches!(
                        e,
                        egui::Event::Key { pressed: true, .. }
                            | egui::Event::Text(_)
                            | egui::Event::Paste(_)
                    )
                });
            (ended, i.pointer.any_down())
        });
        let undo_cmd = self.ui.undo_cmd.take();
        // A MIDI recording is one edit: it is recorded when it ends, not on every frame.
        let recording = self.ui.rec.active();
        if !dragging
            && !recording
            && (gesture_ended || undo_cmd.is_some() || self.frames_since_record >= 30)
        {
            self.frames_since_record = 0;
            self.history.record(&self.project);
        }
        let changed = match undo_cmd {
            Some(undo::UndoCmd::Undo) => self.history.undo(&mut self.project),
            Some(undo::UndoCmd::Redo) => self.history.redo(&mut self.project),
            Some(undo::UndoCmd::Clear) => {
                self.history.clear(&self.project);
                false
            }
            None => false,
        };
        if changed {
            // Track and region numbers may point elsewhere now.
            self.ui.tracks.selected = None;
            self.ui.piano.selected.clear();
            ctx.request_repaint();
        }
        self.ui.can_undo = self.history.can_undo();
        self.ui.can_redo = self.history.can_redo();
    }

    /// MIDI Time Code in: the transport follows the port the project chases, if any.
    fn follow_time_code(&mut self, ctx: &egui::Context) {
        let Some(input) = self.mtc_in.as_mut() else {
            return;
        };
        input.sync(self.project.mtc_in.as_deref());
        // The keyboard input does not take the port the time code comes from.
        if let Some(midi) = self.midi.as_mut() {
            midi.avoid(self.project.mtc_in.as_deref());
        }
        let mut events = Vec::new();
        input.poll(&mut events);
        self.chase
            .update(&mut self.project, &events, std::time::Instant::now());
        if self.chase.is_following() {
            // Look again soon enough to notice when the time code stops.
            ctx.request_repaint_after(mtc_in::STALL / 3);
        }
    }

    /// MIDI keyboard notes: play them on the selected instrument and, while recording, put
    /// them into the new region.
    fn play_and_record_midi(&mut self, ctx: &egui::Context) {
        let mut input = Vec::new();
        if let Some(midi) = self.midi.as_mut() {
            midi.poll(&mut input);
        }
        let p = &mut self.project;
        let s = &mut self.ui;
        let track = s.rec.target().map_or(p.selected_track, |(t, _)| t);
        let plays = p
            .tracks
            .get(track)
            .is_some_and(|t| t.kind != model::TrackKind::Audio);
        for note in &input {
            if !plays {
                break;
            }
            s.audition.push(match *note {
                recorder::LiveNote::On { pitch, vel } => piano_roll::Audition::On {
                    pitch,
                    vel,
                    track: track.min(usize::from(u16::MAX)) as u16,
                },
                recorder::LiveNote::Off { pitch } => piano_roll::Audition::Off { pitch },
                // The engine has no sustain pedal yet: it is only recorded.
                recorder::LiveNote::Sustain { .. } => continue,
            });
        }
        match recorder::update(&mut s.rec, p, &input) {
            recorder::Outcome::Started(t, r) => {
                s.tracks.selected = Some((t, r));
                s.piano.selected.clear();
            }
            recorder::Outcome::Finished(kept) => {
                s.tracks.selected = kept;
                s.piano.selected.clear();
            }
            recorder::Outcome::NoTrack => {
                s.status = Some(i18n::tr("Record: there is no instrument track").into());
            }
            recorder::Outcome::Nothing => {}
        }
        if s.rec.active() || !input.is_empty() {
            ctx.request_repaint();
        }
    }

    /// The window while the Project Chooser is up: the menu bar and the chooser, with the
    /// File key commands and prompts still working.
    fn chooser_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if let Some(menu) = self.native_menu.as_mut() {
            menu.update(ctx, &mut self.project, &mut self.ui);
        }
        if let Some(cmd) = file_commands::pressed(ctx) {
            self.ui.file_cmd = Some(cmd);
        }
        if let Some(sync) = self.sync.as_mut() {
            sync.sync(&mut self.project);
        }
        // Audio files dropped on the chooser go into the open project.
        let dropped: Vec<std::path::PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        if !dropped.is_empty() {
            self.ui.chooser = None;
            for path in dropped {
                self.import(&path, ctx);
            }
        }
        top_bar(ui, &mut self.project, &mut self.ui);
        egui::CentralPanel::default()
            .frame(egui::Frame::new())
            .show(ui, |ui| start_screen::show(ui, &mut self.ui));
        if let Some(cmd) = self.ui.file_cmd.take() {
            self.run_file_cmd(cmd, ctx);
        }
        self.show_save_prompt(ctx);
        window_chrome::resize_edges(ctx);
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let _frame_time = bench::frame_timer();
        let dt = ctx.input(|i| i.stable_dt) as f64;
        ui_scale::apply(&ctx, self.ui.ui_scale);
        tracks_area::poll_normalize(&ctx, &mut self.project, &mut self.ui);
        smart_tempo::poll(&ctx, &mut self.project, &mut self.ui);
        flex_edit::poll(&ctx, &mut self.project, &mut self.ui);
        groove::poll(&ctx, &mut self.project, &mut self.ui);
        if let Some(probe) = self.bench.as_mut() {
            if probe.frame(&ctx) {
                self.bench = None;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        if let Some(shots) = self.shots.as_mut() {
            if shots.frame(&ctx) {
                self.shots = None;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        if self.ui.chooser.is_some() {
            self.chooser_ui(ui, &ctx);
            return;
        }
        if !ctx.egui_wants_keyboard_input() && ctx.input(|i| i.key_pressed(egui::Key::Space)) {
            self.project.playing = !self.project.playing;
        }
        if let Some(menu) = self.native_menu.as_mut() {
            menu.update(&ctx, &mut self.project, &mut self.ui);
        }
        if let Some(cmd) = file_commands::pressed(&ctx) {
            self.ui.file_cmd = Some(cmd);
        }
        menus::handle_keys(&ctx, &mut self.project, &mut self.ui);
        let dropped: Vec<std::path::PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        for path in dropped {
            self.import(&path, &ctx);
        }
        if let Some(out) = self.midi_out.as_mut() {
            out.sync(&self.project);
        }
        self.follow_time_code(&ctx);
        // Pattern and Drummer regions follow their length and the bars under them.
        self.project.refresh_generated();
        if let Some(sync) = self.sync.as_mut() {
            sync.sync(&mut self.project);
            if sync.needs_retry() {
                ctx.request_repaint();
            }
            let load = sync.take_dsp_load();
            self.ui.cpu_load = load.max(self.ui.cpu_load - dt as f32 * 0.8);
            if sync.underruns() > 0 {
                self.ui.status = Some(i18n::trf(
                    "disk too slow: {} underruns",
                    &[&sync.underruns()],
                ));
            }
        } else if self.project.playing {
            // No audio device: fall back to a UI-driven clock.
            let map = self.project.tempo_map();
            self.project.position_beats = map.advance(self.project.position_beats, dt);
        }
        if self.project.playing {
            ctx.request_repaint();
        }
        let master_peaks = self.sync.as_mut().map(|s| s.master_peaks());
        let meters = match self.sync.as_mut() {
            Some(sync) => mixer::Meters {
                tracks: sync.track_meters(self.project.tracks.len()),
                master: sync.master_peaks(),
            },
            None => mixer::Meters::default(),
        };
        let p = &mut self.project;
        let s = &mut self.ui;

        top_bar(ui, p, s);
        egui::Panel::top("control")
            .exact_size(58.0)
            .frame(egui::Frame::new().fill(egui::Color32::from_gray(0x47)))
            .show(ui, |ui| transport::control_bar(ui, p, s, master_peaks));
        if s.show_library {
            egui::Panel::left("library")
                .exact_size(290.0)
                .frame(egui::Frame::new())
                .show(ui, |ui| library::show(ui, p, s));
        }
        if s.show_inspector {
            egui::Panel::left("inspector")
                .exact_size(244.0)
                .frame(egui::Frame::new())
                .show(ui, |ui| inspector::show(ui, p, s, &meters));
        }
        if s.show_lists {
            egui::Panel::right("lists")
                .exact_size(event_list::WIDTH)
                .frame(egui::Frame::new())
                .show(ui, |ui| event_list::show(ui, p, s));
        }
        if s.show_mixer || s.bottom == Bottom::PianoRoll {
            // About half of the space under the control bar (500 points at most), so the Tracks
            // area keeps room on short screens. The Piano Roll can be pulled taller; the mixer
            // has a fixed height, as in Logic: its strips have a natural size and a taller panel
            // would only stretch the faders and meters. (It follows the window, so it is never
            // outside its range when the window shrinks.)
            let room = ui.available_height();
            let usual = (room * 0.48).clamp(160.0, 500.0);
            let panel = egui::Panel::bottom("bottom").frame(egui::Frame::new());
            let panel = if s.bottom == Bottom::Mixer {
                panel.resizable(false).exact_size(usual)
            } else {
                panel.resizable(true).default_size(usual)
            };
            panel.show(ui, |ui| match s.bottom {
                Bottom::Mixer => mixer::show(ui, p, s, &meters),
                Bottom::PianoRoll => piano_roll::show(ui, p, s),
            });
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::new())
            .show(ui, |ui| tracks_area::show(ui, p, s, &meters));

        self.play_and_record_midi(&ctx);
        if std::mem::take(&mut self.ui.update_check) {
            self.updater.start_check(true);
        }
        let unsaved = || file_commands::content(&self.project) != self.saved;
        if self.updater.show(&ctx, &unsaved) == updater::Action::Quit {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        // Forward live notes (piano keys, note auditioning) to the engine.
        // Notes on tracks past the engine's limit are silent, like the rest of those tracks.
        // A rejected event stays queued (with everything after it, to keep On/Off order)
        // and is retried next frame, so a Note Off is never lost.
        if let Some(sync) = self.sync.as_mut() {
            let mut sent = 0;
            for ev in &self.ui.audition {
                let cmd = match *ev {
                    piano_roll::Audition::On { track, .. }
                        if usize::from(track) >= prowave_engine::MAX_TRACKS =>
                    {
                        None
                    }
                    piano_roll::Audition::On { pitch, vel, track } => {
                        Some(prowave_engine::Command::NoteOn { pitch, vel, track })
                    }
                    piano_roll::Audition::Off { pitch } => {
                        Some(prowave_engine::Command::NoteOff { pitch })
                    }
                };
                if cmd.is_some_and(|cmd| !sync.engine.send(cmd)) {
                    break;
                }
                sent += 1;
            }
            self.ui.audition.drain(..sent);
            if !self.ui.audition.is_empty() {
                ctx.request_repaint();
            }
        } else {
            self.ui.audition.clear();
        }
        if let Some(cmd) = self.ui.file_cmd.take() {
            self.run_file_cmd(cmd, &ctx);
        }
        if let Some(cmd) = self.ui.midi_cmd.take() {
            self.run_midi_cmd(cmd);
        }
        midi_fx_ui::show(&ctx, &mut self.project, &mut self.ui);
        transform_ui::show(&ctx, &mut self.project, &mut self.ui);
        smart_tempo::show(&ctx, &mut self.project, &mut self.ui);
        self.show_save_prompt(&ctx);
        self.show_revert_prompt(&ctx);
        self.track_history(&ctx);
        window_chrome::resize_edges(&ctx);
    }
}

/// The in-window menu bar; on macOS, where the menus are in the system menu bar, only a line
/// for errors and warnings while there is one.
fn top_bar(ui: &mut egui::Ui, p: &mut Project, s: &mut UiState) {
    let frame = egui::Frame::side_top_panel(ui.style()).fill(menus::bar_fill(s.light_menus));
    if menus::IN_WINDOW {
        let bar = egui::Panel::top("menu")
            .frame(frame)
            .show(ui, |ui| menus::menu_bar(ui, p, s));
        if window_chrome::CUSTOM {
            let fg = menus::bar_text(s.light_menus);
            window_chrome::bar_controls(ui, bar.response.rect, bar.inner + 8.0, fg);
        }
    } else if menus::has_status(p, s) {
        egui::Panel::top("status")
            .frame(frame)
            .show(ui, |ui| ui.horizontal(|ui| menus::status_line(ui, p, s)));
    }
}

/// Log to stderr; `RUST_LOG` overrides the default (warnings, plus info from ProWave; the
/// Vulkan loader's harmless layer-manifest warnings from `wgpu_hal` are hidden).
fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,wgpu_hal=error,prowave=info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

/// The Pro Wave logo, 256×256 with transparency (generated from dist/icons/prowave-1024.png).
const APP_ICON_PNG: &[u8] = include_bytes!("../assets/icon-256.png");

/// The Flex and Smart Tempo layout pictures (`PROWAVE_SHOT_SCENE=flex…`): a drum loop on an
/// audio track, made in the temporary folder, with Flex markers, a Flex Pitch MIDI track and
/// a Groove Track; `flexeditor` opens the Smart Tempo Editor on it, `flexmenu` the loop's
/// right-click menu and `flexgroove` a track's.
fn shot_flex_scene(app: &mut App, ctx: &egui::Context, kind: &str) {
    let dir = std::env::temp_dir().join("prowave-shots");
    let _ = std::fs::create_dir_all(&dir);
    let wav = dir.join("Drum Loop.wav");
    // Two bars of a 96 BPM beat: kick, snare and hats, a little late here and there.
    let rate = 44_100usize;
    let beat = 60.0 / 96.0;
    let frames = (beat * 8.0 * 2.0 * rate as f64) as usize;
    let mut v = vec![0f32; frames];
    let mut seed = 7u32;
    for k in 0..32 {
        let t = k as f64 * beat / 2.0 + if k % 4 == 3 { 0.025 } else { 0.0 };
        let at = (t * rate as f64) as usize;
        let (len, level, low) = match k % 4 {
            0 => (9000, 0.9, true),
            2 => (6000, 0.8, false),
            _ => (2500, 0.07, false),
        };
        for i in 0..len {
            if at + i >= frames {
                break;
            }
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = (seed >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0;
            let decay = (-(i as f32) / (len as f32 / 5.0)).exp();
            let body = if low {
                (i as f32 * 60.0 * std::f32::consts::TAU / rate as f32).sin()
            } else {
                noise
            };
            v[at + i] += body * level * decay;
        }
    }
    let stereo: Vec<f32> = v.iter().flat_map(|s| [*s, *s]).collect();
    if prowave_engine::write_wav(&wav, &stereo, rate as u32).is_err() {
        return;
    }
    let pr = &mut app.project;
    pr.position_beats = 0.0;
    if import::import_audio(pr, &wav, ctx).is_err() {
        return;
    }
    let t = pr.tracks.len() - 1;
    pr.tracks[t].regions[0].start_bar = 3.0;
    // Move the audio track near the top, where the pictures see it.
    let track = pr.tracks.remove(t);
    pr.tracks.insert(5, track);
    let t = 5;
    flex_edit::set_mode(pr, t, 0, prowave_engine::FlexMode::Rhythmic);
    if let Some(f) = pr.tracks[t].regions[0]
        .audio
        .as_mut()
        .and_then(|a| a.flex.as_mut())
    {
        f.markers = flex_edit::markers_from(
            (1..32).map(|k| (k as f64 * beat / 2.0, k as f64 * beat / 2.0 * 0.98)),
        );
    }
    groove::set_groove_track(pr, Some(t));
    pr.tracks[0].params.follow_groove = true;
    pr.selected_track = t;
    app.ui.tracks.selected = Some((t, 0));
    match kind {
        "editor" => smart_tempo::open_editor(&app.project, &mut app.ui, t, 0),
        "menu" => app.ui.tracks.shot_menu = Some((t, Some(0))),
        "groove" => app.ui.tracks.shot_menu = Some((0, None)),
        _ => {}
    }
}

fn main() -> eframe::Result {
    bench::mark_start();
    // Started by an update that is waiting for the program to close: put the update in place.
    if updater::run_helper_if_requested() {
        return Ok(());
    }
    let all_args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    let bench_out = bench::requested(&all_args);
    let shots_dir = bench::shots_requested(&all_args);
    if bench_out.is_none() && shots_dir.is_none() {
        updater::clean_up_after_update();
    }
    // Measuring runs leave the user's settings (first launch, recent projects, size) alone.
    let measuring = bench_out.is_some() || shots_dir.is_some();
    // Release builds: on a crash, write a report to the temp folder instead of a raw panic.
    human_panic::setup_panic!(human_panic::Metadata::new(
        "Pro Wave",
        env!("CARGO_PKG_VERSION")
    ));
    init_logging();
    i18n::init();
    project_io::remove_stale_media(std::time::Duration::from_secs(7 * 24 * 3600));
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1600.0, 980.0])
        // Smaller than this the panels have no room left, even scaled down.
        .with_min_inner_size([800.0, 520.0])
        // Fill the screen, as Logic does; on a small screen 1600 x 980 would not fit.
        .with_maximized(true)
        // Just the app name, on every system: no project name or view in the title bar.
        .with_title("Pro Wave")
        // Window class: lets Linux desktops match the window to its .desktop entry and icon.
        .with_app_id("prowave")
        // Windows and Linux: no title bar from the system; the menu bar row has the window
        // buttons (see window_chrome.rs).
        .with_decorations(!window_chrome::CUSTOM);
    // Title bar / taskbar icon. A broken icon must not stop the app from starting.
    // macOS takes the Dock icon from the app bundle (prowave.icns, on the macOS icon tile);
    // setting one here would swap it for the bare logo once the app is running. An empty
    // icon tells eframe to leave the Dock icon alone instead of using its default egui logo.
    if cfg!(target_os = "macos") {
        viewport = viewport.with_icon(egui::IconData::default());
    } else {
        match eframe::icon_data::from_png_bytes(APP_ICON_PNG) {
            Ok(icon) => viewport = viewport.with_icon(icon),
            Err(e) => tracing::warn!("app icon could not be decoded: {e}"),
        }
    }
    let opts = eframe::NativeOptions {
        viewport,
        wgpu_options: graphics::config(),
        ..Default::default()
    };
    let result = eframe::run_native(
        "Pro Wave",
        opts,
        Box::new(|cc| {
            theme::apply(&cc.egui_ctx);
            // Cmd/Ctrl + = / - / 0 resize the interface through `ui_scale` instead, which
            // also keeps the fit for small windows.
            cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);
            // The very first start shows the demo song; later starts begin clean. A benchmark
            // run always starts clean and leaves the first launch to the user.
            let project =
                if shots_dir.is_some() || (!measuring && recent_files::take_first_launch()) {
                    Project::demo()
                } else {
                    Project::empty()
                };
            let mut audio_error = None;
            let mut midi_out = None;
            let sync = match prowave_engine::Engine::start() {
                Ok(e) => {
                    let mut sync = engine_sync::EngineSync::new(e, &project);
                    if !measuring {
                        midi_out = sync
                            .take_midi_out()
                            .zip(sync.take_mtc_out())
                            .map(|(notes, mtc)| midi_out::MidiOut::start(notes, mtc));
                    }
                    Some(sync)
                }
                Err(err) => {
                    tracing::warn!("audio disabled: {err}");
                    audio_error =
                        Some(i18n::trf("No sound: {}. Check your audio device.", &[&err]));
                    None
                }
            };
            let mut app = App {
                saved: file_commands::content(&project),
                history: undo::History::new(&project),
                project,
                ui: UiState::new(),
                sync,
                path: None,
                native_menu: None,
                bench: bench_out.clone().map(bench::Probe::new),
                shots: shots_dir.clone().map(bench::LayoutShots::new),
                frames_since_record: 0,
                midi: (!measuring).then(midi_in::MidiIn::new),
                midi_out,
                mtc_in: (!measuring).then(|| mtc_in::MtcIn::new(cc.egui_ctx.clone())),
                chase: mtc_in::Chase::default(),
                updater: updater::Updater::new(),
            };
            if !measuring {
                app.ui.update_auto = updater::auto_enabled();
                app.updater.check_at_startup();
            }
            #[cfg(target_os = "macos")]
            {
                app.native_menu = Some(native_menu::NativeMenu::install(&cc.egui_ctx));
            }
            app.ui.status = audio_error;
            if let Some(n) = measuring
                .then(|| {
                    std::env::var("PROWAVE_BENCH_TRACKS")
                        .ok()?
                        .parse::<usize>()
                        .ok()
                })
                .flatten()
            {
                app.project = bench::many_tracks(n.clamp(1, 1000));
                app.saved = file_commands::content(&app.project);
                app.history.reset(&app.project);
            }
            if app.shots.is_some() {
                // The pictures also show the Tempo track, with a few tempo changes.
                app.ui.tracks.show_tempo = true;
                app.ui.tracks.show_markers = true;
                app.ui.tracks.show_signatures = true;
                app.project.set_signature_from(5, (3, 4));
                app.project.set_signature_from(9, (6, 8));
                // A VCA and an External MIDI track near the front, to see their strips.
                {
                    use model::{Track, TrackKind};
                    let pr = &mut app.project;
                    pr.tracks.insert(1, Track::new("VCA 1", TrackKind::Vca));
                    pr.vca_inserted(1);
                    let mut ext = Track::new("External MIDI 1", TrackKind::ExternalMidi);
                    ext.midi_port = Some("USB MIDI Interface".into());
                    ext.midi_channel = 3;
                    pr.tracks.insert(2, ext);
                    pr.vca_inserted(2);
                    pr.assign_vca(0, Some(1));
                    pr.assign_vca(3, Some(1));
                    pr.tracks[1].volume_db = -4.0;
                }
                if let Some(r) = app
                    .project
                    .tracks
                    .get_mut(4)
                    .and_then(|t| t.regions.get_mut(0))
                {
                    r.params.fade_in = 4.0;
                    r.params.fade_out = 8.0;
                }
                for (bar, name) in [(0.0, "Intro"), (4.0, "Verse"), (12.0, "Chorus")] {
                    let i = app.project.add_marker(app.project.bar_to_beats(bar));
                    app.project.markers[i].name = name.into();
                }
                app.project.add_tempo_change(32.0, 132.0);
                app.project.add_tempo_change(48.0, 104.0);
                // A tempo curve from the first change to the second.
                app.project.set_tempo_ramp(1, true);
                // `PROWAVE_SHOT_SCENE=pattern` or `drummer` opens that editor on a new region
                // of the Drummer track.
                let scene = std::env::var("PROWAVE_SHOT_SCENE").unwrap_or_default();
                if scene == "score" || scene == "midifx" {
                    app.ui.tracks.selected = Some((0, 0));
                    app.project.selected_track = 0;
                    app.ui.bottom = Bottom::PianoRoll;
                    app.ui.score_tab = true;
                    app.ui.show_midi_fx = scene == "midifx";
                    app.project.tracks[0].midi_fx = vec![
                        midi_fx::FxSlot {
                            fx: midi_fx::MidiFx::Arpeggiator {
                                rate: pattern::StepRate::Eighth,
                                pattern: midi_fx::ArpPattern::UpDown,
                                octaves: 2,
                                gate: 80,
                            },
                            on: true,
                        },
                        midi_fx::FxSlot {
                            fx: midi_fx::MidiFx::Scale {
                                root: 0,
                                minor: true,
                            },
                            on: true,
                        },
                    ];
                }
                if let Some(kind) = scene.strip_prefix("flex") {
                    shot_flex_scene(&mut app, &cc.egui_ctx, kind);
                }
                if scene == "pattern" || scene == "drummer" {
                    let pr = &mut app.project;
                    if let Some(t) = pr
                        .tracks
                        .iter()
                        .position(|t| t.kind == model::TrackKind::Drummer)
                    {
                        if let Some(ri) = pr.add_generated_region(t, scene == "drummer", 40.0) {
                            pr.selected_track = t;
                            app.ui.tracks.selected = Some((t, ri));
                            app.ui.bottom = Bottom::PianoRoll;
                        }
                    }
                }
                app.saved = file_commands::content(&app.project);
                app.history.reset(&app.project);
            }
            if !measuring {
                app.ui.recent = recent_files::Recent::load();
                app.ui.ui_scale = ui_scale::load();
            }
            // `prowave song.prowave` opens a project; `prowave song.wav drums.aiff` imports each
            // file as an audio track (into the opened project, if any).
            // `--play` starts the transport on launch; `--piano` opens the Piano Roll.
            // A benchmark run takes no other arguments.
            let args: Vec<std::ffi::OsString> = if measuring {
                Vec::new()
            } else {
                std::env::args_os().skip(1).collect()
            };
            let is_project = |a: &std::ffi::OsString| {
                std::path::Path::new(a)
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case(prowave_project::EXTENSION))
            };
            if let Some(file) = args.iter().find(|a| is_project(a)) {
                app.open(std::path::Path::new(file), &cc.egui_ctx);
            }
            for arg in args.iter().filter(|a| !is_project(a)) {
                if arg == "--play" {
                    app.project.playing = true;
                } else if arg == "--piano" {
                    app.ui.bottom = Bottom::PianoRoll;
                } else {
                    app.import(std::path::Path::new(arg), &cc.egui_ctx);
                }
            }
            Ok(Box::new(app))
        }),
    );
    // The app (and with it the engine's open media files) is gone by now.
    project_io::remove_session_media();
    if let (Some(out), Err(e)) = (&bench_out, &result) {
        bench::report_without_window(out, &e.to_string());
        return Ok(());
    }
    result
}

#[cfg(test)]
mod tests {
    #[test]
    fn app_icon_decodes_as_a_square_rgba_image() {
        let icon = eframe::icon_data::from_png_bytes(super::APP_ICON_PNG).unwrap();
        assert_eq!((icon.width, icon.height), (256, 256));
        assert_eq!(icon.rgba.len(), 256 * 256 * 4);
    }
}
