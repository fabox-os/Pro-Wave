//! Audio recording, as in Logic: while Record and Play are both on, every armed audio track
//! (R button) records its input into a new region; with none armed, the selected audio track
//! does. The region is red and grows with the song position, and takes the track's colour and
//! the waveform when the recording stops.
//!
//! The input is open only while a track wants it, so the microphone is not held for nothing.
//! The recordings are files in this session's media folder; saving the project copies them
//! into it.

use crate::i18n::{tr, trf};
use crate::model::{Project, Region, RegionParams, TrackKind};
use prowave_engine::{input_devices, AudioFile, Capture, InputInfo, InputSource};
use std::path::PathBuf;

/// How much of a level meter's value is left at the next frame.
const METER_FALL: f32 = 0.85;

/// The audio input device of this computer that Pro Wave records from (Logic: Settings >
/// Audio > Input Device). It belongs to the computer, not to a project. `default()` never
/// touches the disk (used by tests); [`InputChoice::load`] is the real, remembered one.
#[derive(Default)]
pub struct InputChoice {
    /// The list as of the last [`InputChoice::refresh`].
    pub devices: Vec<InputInfo>,
    /// The chosen device's id; `None` is the system's default input.
    pub device: Option<String>,
    changed: bool,
    file: Option<PathBuf>,
}

impl InputChoice {
    pub fn load() -> Self {
        let file = crate::recent_files::config_dir().map(|d| d.join("audio-input"));
        let device = file
            .as_deref()
            .and_then(|f| std::fs::read_to_string(f).ok())
            .map(|text| text.trim().to_string())
            .filter(|id| !id.is_empty());
        Self {
            device,
            file,
            ..Self::default()
        }
    }

    /// Asks the system for the inputs (this can take a moment, so not on every frame).
    pub fn refresh(&mut self) {
        self.devices = input_devices();
    }

    /// Records from `device` from now on, and remembers it. Best effort: a choice that cannot
    /// be saved is not worth interrupting the user for.
    pub fn pick(&mut self, device: Option<String>) {
        if device == self.device {
            return;
        }
        self.device = device;
        self.changed = true;
        let Some(file) = &self.file else { return };
        let result = file
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(file, self.device.as_deref().unwrap_or_default()));
        if let Err(e) = result {
            tracing::warn!("could not save the audio input: {e}");
        }
    }

    /// How many inputs the chosen device has, for the track's Input menu.
    pub fn channels(&self) -> u16 {
        let chosen = self.devices.iter().find(|d| match &self.device {
            Some(id) => d.id == *id,
            None => d.is_default,
        });
        chosen
            .or(self.devices.first())
            .map_or(2, |d| d.channels)
            .min(prowave_engine::MAX_INPUT_CHANNELS as u16)
    }
}

/// The name of an input in the channel strip's Input slot.
pub fn input_label(source: InputSource) -> String {
    match source {
        InputSource::Mono(c) => trf("Input {}", &[&(c + 1)]),
        InputSource::Stereo(c) => trf("Input {}", &[&format!("{}-{}", c + 1, c + 2)]),
    }
}

/// What [`AudioRecorder::update`] did, for the app to react to.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Nothing,
    /// Recording ended: the last region that was kept, if any, and a warning, if any.
    Finished(Option<(usize, usize)>, Option<String>),
    /// The input cannot be used or the recording cannot start.
    Failed(String),
}

/// One track's recording in progress.
struct Take {
    track: usize,
    region: usize,
    id: u32,
    start: f64,
    path: PathBuf,
}

/// The recording in progress, and the open input.
#[derive(Default)]
pub struct AudioRecorder {
    capture: Option<Capture>,
    /// Opening the input failed: not tried again until something changes.
    failed: bool,
    takes: Vec<Take>,
    /// Numbers the files of this session.
    count: u32,
    /// Level of each input channel, for the meters.
    levels: Vec<f32>,
    fresh: Vec<f32>,
}

/// The audio tracks that record: the armed ones, or else the selected one.
pub fn targets(p: &Project) -> Vec<usize> {
    let armed: Vec<usize> = p
        .tracks
        .iter()
        .enumerate()
        .filter(|(_, t)| t.kind == TrackKind::Audio && t.rec)
        .map(|(i, _)| i)
        .collect();
    if !armed.is_empty() {
        return armed;
    }
    let selected = p.tracks.get(p.selected_track);
    if selected.is_some_and(|t| t.kind == TrackKind::Audio) {
        vec![p.selected_track]
    } else {
        Vec::new()
    }
}

/// Whether Record means audio now, so that the MIDI recorder leaves it alone.
pub fn takes_over(p: &Project) -> bool {
    p.recording && !targets(p).is_empty()
}

/// Whether a track needs the input open now.
fn needs_input(p: &Project) -> bool {
    takes_over(p)
        || p.tracks
            .iter()
            .any(|t| t.kind == TrackKind::Audio && (t.rec || t.monitor))
}

/// The file for a recording on track `name`.
fn file_name(name: &str, count: u32) -> String {
    let clean: String = name
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_'))
        .take(40)
        .collect();
    let clean = clean.trim();
    let clean = if clean.is_empty() { "Audio" } else { clean };
    format!("{clean}-{count}.wav")
}

/// Whether region `r` of track `t` is one being recorded (its name starts with `*`, as for
/// MIDI recording). If not, the track or the region was deleted or moved.
fn is_open(p: &Project, t: usize, r: usize) -> bool {
    p.tracks
        .get(t)
        .and_then(|t| t.regions.get(r))
        .is_some_and(|region| region.name.starts_with('*'))
}

/// Turns the red region into the finished audio region: its length is the file's.
fn apply_take(
    p: &mut Project,
    t: usize,
    r: usize,
    start: f64,
    file: AudioFile,
    ctx: &egui::Context,
) {
    let end = p.tempo_map().advance(start, file.duration_secs());
    let len_bars = (p.beats_to_bar(end) - p.beats_to_bar(start)) as f32;
    let name = p.tracks[t].name.clone();
    let region = &mut p.tracks[t].regions[r];
    region.name = name;
    region.len_bars = len_bars;
    region.audio = Some(crate::import::audio_region(file, ctx));
    p.song_rev += 1;
}

impl AudioRecorder {
    pub fn active(&self) -> bool {
        !self.takes.is_empty()
    }

    /// The regions being recorded now (drawn red).
    pub fn regions(&self) -> impl Iterator<Item = (usize, usize)> + '_ {
        self.takes.iter().map(|t| (t.track, t.region))
    }

    /// While a track is armed or recording its meter shows its input, as in Logic.
    pub fn show_levels(&self, p: &Project, meters: &mut [(f32, f32)]) {
        if self.capture.is_none() {
            return;
        }
        let at = |c: u16| self.levels.get(usize::from(c)).copied().unwrap_or(0.0);
        for (i, t) in p.tracks.iter().enumerate() {
            let listens = t.rec || t.monitor || self.takes.iter().any(|k| k.track == i);
            if t.kind != TrackKind::Audio || !listens {
                continue;
            }
            if let Some(meter) = meters.get_mut(i) {
                *meter = match t.input {
                    InputSource::Mono(c) => (at(c), at(c)),
                    InputSource::Stereo(c) => (at(c), at(c + 1)),
                };
            }
        }
    }

    /// The live input of a newly opened device, for the engine's monitoring.
    pub fn take_feed(&mut self) -> Option<Box<prowave_engine::InputFeed>> {
        self.capture.as_mut()?.take_monitor_feed()
    }

    /// Whether the input is open (the meters then need redrawing).
    pub fn listening(&self) -> bool {
        self.capture.is_some()
    }

    /// Call once per frame. `latency_us` is the delay of the output.
    pub fn update(
        &mut self,
        choice: &mut InputChoice,
        p: &mut Project,
        latency_us: u32,
        ctx: &egui::Context,
    ) -> Outcome {
        let changed = std::mem::take(&mut choice.changed);
        let wanted = needs_input(p);
        if self.takes.is_empty() && (changed || !wanted) {
            self.capture = None;
            self.failed = false;
            self.levels.clear();
        }
        let mut error = None;
        if wanted && self.capture.is_none() && !self.failed {
            match Capture::open(choice.device.as_deref()) {
                Ok(capture) => {
                    tracing::info!(
                        "audio input {} ({} channels at {} Hz)",
                        capture.device_name(),
                        capture.channels(),
                        capture.sample_rate()
                    );
                    self.capture = Some(capture);
                }
                Err(e) => {
                    self.failed = true;
                    error = Some(trf("Cannot open the audio input: {}", &[&e]));
                }
            }
        }
        self.read_levels();
        let record = p.recording && p.playing && !p.counting_in;
        if self.takes.is_empty() {
            if self.failed && takes_over(p) {
                p.recording = false;
            } else if record && !targets(p).is_empty() {
                let outcome = self.start(p, latency_us);
                if outcome != Outcome::Nothing {
                    return outcome;
                }
            }
        } else if record {
            self.grow(p);
        } else {
            return self.finish(p, ctx);
        }
        error.map_or(Outcome::Nothing, Outcome::Failed)
    }

    fn read_levels(&mut self) {
        let Some(capture) = &self.capture else { return };
        capture.take_peaks(&mut self.fresh);
        self.levels.resize(self.fresh.len(), 0.0);
        for (level, peak) in self.levels.iter_mut().zip(&self.fresh) {
            *level = peak.max(*level * METER_FALL);
        }
    }

    fn start(&mut self, p: &mut Project, latency_us: u32) -> Outcome {
        let Some(capture) = self.capture.as_mut() else {
            p.recording = false;
            return Outcome::Failed(tr("Record: there is no audio input").into());
        };
        let dir = crate::project_io::recordings_dir();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            p.recording = false;
            return Outcome::Failed(trf("Cannot record: {}", &[&e]));
        }
        let start = p.position_beats.max(0.0);
        let (start_bar, mut error) = (p.beats_to_bar(start) as f32 + 1.0, None);
        for t in targets(p) {
            self.count += 1;
            let track = &mut p.tracks[t];
            let path = dir.join(file_name(&track.name, self.count));
            match capture.start(&path, track.input, latency_us) {
                Ok(id) => {
                    track.regions.push(Region {
                        name: format!("*{}", track.name),
                        start_bar,
                        len_bars: 0.01,
                        notes: Vec::new(),
                        controls: Vec::new(),
                        folder: None,
                        generated: None,
                        audio: None,
                        params: RegionParams::default(),
                    });
                    self.takes.push(Take {
                        track: t,
                        region: track.regions.len() - 1,
                        id,
                        start,
                        path,
                    });
                }
                Err(e) => error = Some(trf("Cannot record: {}", &[&e])),
            }
        }
        if self.takes.is_empty() {
            p.recording = false;
        }
        error.map_or(Outcome::Nothing, Outcome::Failed)
    }

    fn grow(&mut self, p: &mut Project) {
        let takes = std::mem::take(&mut self.takes);
        for take in takes {
            if is_open(p, take.track, take.region) {
                let now = p.position_beats.max(take.start);
                let len = (p.beats_to_bar(now) - p.beats_to_bar(take.start)) as f32;
                let region = &mut p.tracks[take.track].regions[take.region];
                region.len_bars = region.len_bars.max(len);
                self.takes.push(take);
            } else if let Some(capture) = self.capture.as_mut() {
                // Its region is gone: nothing would play this file.
                let _ = capture.stop(take.id);
                let _ = std::fs::remove_file(&take.path);
            }
        }
    }

    fn finish(&mut self, p: &mut Project, ctx: &egui::Context) -> Outcome {
        p.recording = false;
        let (mut kept, mut warning) = (None, None);
        for take in std::mem::take(&mut self.takes) {
            let Some(capture) = self.capture.as_mut() else {
                break;
            };
            let done = capture.stop(take.id);
            let open = is_open(p, take.track, take.region);
            let file = match &done {
                Ok(d) if d.frames > 0 => AudioFile::open(&d.path).map_err(|e| e.to_string()),
                Ok(_) => Err(tr("no audio arrived from the input").to_string()),
                Err(e) => Err(e.to_string()),
            };
            match (open, file) {
                (true, Ok(file)) => {
                    apply_take(p, take.track, take.region, take.start, file, ctx);
                    kept = Some((take.track, take.region));
                    if done.is_ok_and(|d| d.lost) {
                        warning = Some(tr("Part of the recording was lost").to_string());
                    }
                }
                (open, file) => {
                    if open {
                        p.tracks[take.track].regions.remove(take.region);
                    }
                    if let Err(e) = file {
                        warning = Some(trf("Cannot record: {}", &[&e]));
                    }
                    let _ = std::fs::remove_file(&take.path);
                }
            }
        }
        Outcome::Finished(kept, warning)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::model::Track;

    fn project(kinds: &[TrackKind]) -> Project {
        let mut p = Project::empty();
        p.tracks = kinds.iter().map(|k| Track::new("T", *k)).collect();
        p.selected_track = 0;
        p
    }

    #[test]
    fn armed_audio_tracks_record_and_else_the_selected_one() {
        let mut p = project(&[TrackKind::Audio, TrackKind::Instrument, TrackKind::Audio]);
        assert_eq!(targets(&p), vec![0], "nothing armed: the selected track");
        p.tracks[2].rec = true;
        p.tracks[1].rec = true;
        assert_eq!(targets(&p), vec![2], "armed tracks, audio only");
        p.tracks[0].rec = true;
        assert_eq!(targets(&p), vec![0, 2], "all at once");
    }

    #[test]
    fn midi_keeps_record_unless_an_audio_track_is_the_target() {
        let mut p = project(&[TrackKind::Instrument, TrackKind::Audio]);
        p.recording = true;
        assert!(!takes_over(&p), "an instrument is selected");
        p.selected_track = 1;
        assert!(takes_over(&p));
        p.recording = false;
        assert!(!takes_over(&p));
    }

    #[test]
    fn the_input_is_held_only_while_a_track_wants_it() {
        let mut p = project(&[TrackKind::Audio, TrackKind::Instrument]);
        assert!(!needs_input(&p));
        p.tracks[1].rec = true;
        assert!(!needs_input(&p), "an instrument needs no audio input");
        p.tracks[0].rec = true;
        assert!(needs_input(&p), "armed");
        p.tracks[0].rec = false;
        p.recording = true;
        assert!(needs_input(&p), "about to record the selected track");
        p.recording = false;
        p.tracks[0].monitor = true;
        assert!(needs_input(&p), "listening to the input");
    }

    #[test]
    fn a_track_name_makes_a_safe_file_name() {
        assert_eq!(file_name("Lead Vox", 3), "Lead Vox-3.wav");
        assert_eq!(file_name("../../etc/passwd", 1), "etcpasswd-1.wav");
        assert_eq!(file_name("  ", 2), "Audio-2.wav");
        assert!(file_name(&"x".repeat(500), 1).len() < 60);
    }

    #[test]
    fn input_names() {
        assert_eq!(input_label(InputSource::Mono(0)), "Input 1");
        assert_eq!(input_label(InputSource::Stereo(2)), "Input 3-4");
    }

    #[test]
    fn the_channels_of_the_chosen_device_or_of_the_default() {
        let info = |id: &str, channels, is_default| InputInfo {
            id: id.into(),
            name: id.into(),
            channels,
            is_default,
        };
        let mut c = InputChoice {
            devices: vec![info("a", 2, true), info("b", 8, false)],
            ..InputChoice::default()
        };
        assert_eq!(c.channels(), 2);
        c.pick(Some("b".into()));
        assert_eq!(c.channels(), 8);
        c.pick(Some("gone".into()));
        assert_eq!(c.channels(), 2, "a device that left");
        assert_eq!(
            InputChoice::default().channels(),
            2,
            "before the first look"
        );
    }

    #[test]
    fn a_new_device_reopens_the_input_only_when_it_changed() {
        let mut c = InputChoice::default();
        c.pick(None);
        assert!(!c.changed, "the same choice");
        c.pick(Some("b".into()));
        assert!(c.changed);
    }

    #[test]
    fn a_finished_take_becomes_an_audio_region_of_the_files_length() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("take.wav");
        // 2 seconds at 120 BPM are 4 quarter notes: one bar of 4/4.
        prowave_engine::write_wav(&path, &vec![0.1; 48_000 * 2 * 2], 48_000).unwrap();
        let mut p = project(&[TrackKind::Audio]);
        p.tracks[0].name = "Vox".into();
        p.tempo = 120.0;
        p.tracks[0].regions.push(Region {
            name: "*Vox".into(),
            start_bar: 2.0,
            len_bars: 0.01,
            notes: Vec::new(),
            controls: Vec::new(),
            folder: None,
            generated: None,
            audio: None,
            params: RegionParams::default(),
        });
        let rev = p.song_rev;
        assert!(is_open(&p, 0, 0));
        let file = AudioFile::open(&path).unwrap();
        apply_take(&mut p, 0, 0, 4.0, file, &egui::Context::default());
        let region = &p.tracks[0].regions[0];
        assert_eq!(region.name, "Vox");
        assert!((region.len_bars - 1.0).abs() < 1e-3, "{}", region.len_bars);
        assert!(region.audio.is_some());
        assert!(!is_open(&p, 0, 0), "no longer being recorded");
        assert_eq!(p.song_rev, rev + 1, "the engine hears about it");
    }

    #[test]
    fn a_region_that_was_deleted_is_not_written_to() {
        let p = project(&[TrackKind::Audio]);
        assert!(!is_open(&p, 0, 0));
        assert!(!is_open(&p, 5, 0));
    }

    #[test]
    fn armed_tracks_meter_their_input() {
        let mut p = project(&[TrackKind::Audio, TrackKind::Audio]);
        p.tracks[0].rec = true;
        p.tracks[0].input = InputSource::Stereo(0);
        let rec = AudioRecorder::default();
        let mut meters = vec![(0.5, 0.5), (0.5, 0.5)];
        rec.show_levels(&p, &mut meters);
        assert_eq!(meters, vec![(0.5, 0.5), (0.5, 0.5)], "the input is closed");
    }

    /// The whole path on real hardware: arm, record, stop, a region with the audio. Needs an
    /// input device, so it runs only on request:
    /// `cargo test -p prowave-app records_from_the_real_input -- --ignored --nocapture`
    #[test]
    #[ignore = "needs an audio input device"]
    fn records_from_the_real_input() {
        let ctx = egui::Context::default();
        let mut p = project(&[TrackKind::Audio]);
        p.tracks[0].name = "Vox".into();
        p.tracks[0].rec = true;
        let (mut rec, mut choice) = (AudioRecorder::default(), InputChoice::default());
        assert_eq!(rec.update(&mut choice, &mut p, 0, &ctx), Outcome::Nothing);
        assert!(rec.listening(), "arming opens the input");

        p.position_beats = 4.0;
        p.recording = true;
        p.playing = true;
        for _ in 0..50 {
            assert_eq!(rec.update(&mut choice, &mut p, 0, &ctx), Outcome::Nothing);
            std::thread::sleep(std::time::Duration::from_millis(20));
            p.position_beats += 0.04;
        }
        assert_eq!(rec.regions().collect::<Vec<_>>(), vec![(0, 0)]);
        assert!(
            p.tracks[0].regions[0].len_bars > 0.2,
            "the red region grows"
        );

        p.playing = false;
        let outcome = rec.update(&mut choice, &mut p, 0, &ctx);
        assert_eq!(outcome, Outcome::Finished(Some((0, 0)), None));
        assert!(!p.recording && !rec.active());
        let region = &p.tracks[0].regions[0];
        let file = &region.audio.as_ref().unwrap().file;
        println!("recorded {} frames at {} Hz", file.frames, file.sample_rate);
        assert!(file.duration_secs() > 0.8, "{}", file.duration_secs());
        assert_eq!(region.start_bar, 2.0);
        assert_eq!(region.name, "Vox");

        p.tracks[0].rec = false;
        rec.update(&mut choice, &mut p, 0, &ctx);
        assert!(!rec.listening(), "the input closes when nothing wants it");
    }
}
