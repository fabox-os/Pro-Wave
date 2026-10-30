//! Bridges the UI project model to `prowave-engine` using only `Command`s (never direct mutation).
use crate::model::TrackKind;
use crate::model::{Insert, Project, Region, TempoPoint, Track};
use crate::patches::synth_patch;
use prowave_engine::{
    AutoLane, AutoTarget, Chain, Command, ControlEvent, Engine, Fx, InputSource, MonitorTrack,
    NoteEvent, Pdc, Route, SendSpec, Song, SynthPatch, TrackMix, MAX_MONITORS, MAX_TRACKS,
};

#[derive(Clone, Copy, PartialEq)]
struct Mix {
    gain_db: f32,
    mute: bool,
    solo: bool,
    pan: f32,
}

/// Mirrors what the engine has accepted. Each field changes only when `Engine::send`
/// returns `true`, so a command rejected because the queue was full is retried on the next
/// `sync`. `None` means nothing has been accepted yet.
pub struct EngineSync {
    pub engine: Engine,
    playing: bool,
    /// The tracks that hear their input, as last sent.
    monitors: Vec<MonitorTrack>,
    /// Tempo, tempo changes and signature last sent.
    tempo: Option<TempoKey>,
    metronome: bool,
    cycle: (bool, f64, f64),
    master: Option<f32>,
    position: Option<f64>,
    mixes: Vec<Mix>,
    /// Last synth patch sent for each track (re-sent whenever the Library changes it).
    patches: Vec<SynthPatch>,
    /// Which tracks the engine was told are external, by track.
    externals: Vec<bool>,
    /// The controller changes the engine has.
    controls: Vec<ControlEvent>,
    /// MIDI Time Code as the engine was last told: on, frame rate, project start in frames.
    mtc: Option<(bool, prowave_engine::SmpteRate, i64)>,
    song_rev: Option<u64>,
    /// Audio layout of the song last sent with `SetSong`.
    layout: Option<AudioLayout>,
    /// The effects, routes and automation the engine has, by track.
    fx: Vec<Vec<Insert>>,
    routes: Vec<Route>,
    /// The compensation delays the engine has, by track.
    pdc: Vec<(usize, usize)>,
    automation: Vec<AutoLane>,
    /// A command was rejected during the last `sync`.
    retry: bool,
}

/// Play, or with Record and Count-in on, one bar of clicks first.
fn start_command(p: &Project) -> Command {
    if !(p.recording && p.count_in) {
        return Command::Play;
    }
    let bar = p.beats_to_bar(p.position_beats).floor().max(0.0) as u32;
    let (clicks, unit) = p.signature_of_bar(bar);
    Command::CountIn {
        clicks,
        beats_per_click: 4.0 / f64::from(unit.max(1)),
    }
}

/// The audio tracks whose I button is on.
fn monitors_of(p: &Project) -> Vec<MonitorTrack> {
    p.tracks
        .iter()
        .enumerate()
        .filter(|(_, t)| t.kind == TrackKind::Audio && t.monitor)
        .take(MAX_MONITORS)
        .map(|(i, t)| {
            let (channel, stereo) = match t.input {
                InputSource::Mono(c) => (c, false),
                InputSource::Stereo(c) => (c, true),
            };
            MonitorTrack {
                track: i.min(usize::from(u16::MAX)) as u16,
                channel,
                stereo,
            }
        })
        .collect()
}

/// Where track `i`'s signal goes: its sends, and for an Aux strip the bus it listens to.
fn route_of(p: &Project, i: usize) -> Route {
    let Some(t) = p.tracks.get(i) else {
        return Route::default();
    };
    let mut route = Route {
        bus_in: p.aux_bus_of(i),
        output: t.output,
        ..Route::default()
    };
    // An Aux strip does not send (a loop through the buses would never end).
    if t.kind != TrackKind::Aux {
        for (slot, s) in route.sends.iter_mut().zip(&t.sends) {
            if let Some(bus) = s.bus {
                *slot = SendSpec {
                    bus,
                    level: s.level,
                    pre: s.pre,
                };
            }
        }
    }
    route.sanitized()
}

/// The effects as a chain the audio thread can run (made here, off that thread).
fn chain_of(inserts: &[Insert], sample_rate: f32) -> Box<Chain> {
    Box::new(Chain::new(
        inserts
            .iter()
            .map(|i| Fx::new(i.kind, i.params, i.bypass, sample_rate))
            .collect(),
    ))
}

/// How much of a lane's value is the VCA's: only a fader lane adds the VCA's level.
fn lane_offset(p: &Project, i: usize, target: AutoTarget) -> f32 {
    match target {
        AutoTarget::Volume => p.vca_of(i).map_or(0.0, |v| v.volume_db),
        _ => 0.0,
    }
}

/// The lanes the engine should have for the project's automation.
fn automation_lanes(p: &Project) -> Vec<AutoLane> {
    let mut lanes = Vec::new();
    for (i, t) in p.tracks.iter().enumerate().take(MAX_TRACKS) {
        for (target, points) in t.auto.lanes() {
            if !points.is_empty() {
                lanes.push(
                    AutoLane {
                        track: i as u16,
                        target,
                        follow: t.auto.follows(target),
                        offset_db: lane_offset(p, i, target),
                        points: points.to_vec(),
                    }
                    .sanitized(),
                );
            }
        }
    }
    lanes
}

/// Whether `sent` is what [`automation_lanes`] would make, without making it (it runs every
/// frame, and a long recording has many points).
fn automation_unchanged(p: &Project, sent: &[AutoLane]) -> bool {
    let mut it = sent.iter();
    for (i, t) in p.tracks.iter().enumerate().take(MAX_TRACKS) {
        for (target, points) in t.auto.lanes() {
            if points.is_empty() {
                continue;
            }
            let Some(lane) = it.next() else {
                return false;
            };
            let (lo, hi) = crate::model::Automation::range(target);
            let (lo, hi) = match target {
                AutoTarget::Volume => (-96.0f32.max(lo), hi),
                _ => (lo, hi),
            };
            if lane.track as usize != i
                || lane.target != target
                || lane.follow != t.auto.follows(target)
                || lane.offset_db != lane_offset(p, i, target)
                || lane.points.len() != points.len().min(prowave_engine::MAX_AUTO_POINTS)
                || !lane.points.iter().zip(points).all(|(a, b)| {
                    a.beat == b.beat && a.value == b.value.clamp(lo, hi) && a.curve == b.curve
                })
            {
                return false;
            }
        }
    }
    it.next().is_none()
}

/// The delay every effect of the strip puts on the sound, in samples.
fn latency_of(t: &Track, sample_rate: f32) -> usize {
    t.inserts
        .iter()
        .filter(|i| !i.bypass)
        .map(|i| i.kind.latency_samples(sample_rate))
        .sum()
}

/// Delay compensation for every track: (before the fader and sends, after them), in samples.
/// Strips whose effects look ahead delay the sound; the others wait for them so everything
/// stays in time. The signal sent to a bus is aligned too, and an Aux strip's own effects
/// are made up for after the fader.
fn pdc_plan(p: &Project, sample_rate: f32) -> Vec<(usize, usize)> {
    let sounds = |t: &Track| !matches!(t.kind, TrackKind::Vca | TrackKind::Folder);
    let lat: Vec<usize> = p
        .tracks
        .iter()
        .map(|t| latency_of(t, sample_rate))
        .collect();
    let (mut plain, mut aux) = (0, 0);
    for (i, _) in p.tracks.iter().enumerate().filter(|(_, t)| sounds(t)) {
        if p.aux_bus_of(i) > 0 {
            aux = aux.max(lat[i]);
        } else {
            plain = plain.max(lat[i]);
        }
    }
    p.tracks
        .iter()
        .enumerate()
        .map(|(i, t)| {
            if !sounds(t) {
                (0, 0)
            } else if p.aux_bus_of(i) > 0 {
                (0, aux - lat[i])
            } else {
                (plain - lat[i], aux)
            }
        })
        .collect()
}

/// Engine track number for UI track `i`. Tracks past the engine's `MAX_TRACKS` have none:
/// they are left out of the song (silent) instead of being folded onto the last track.
fn track_index(i: usize) -> Option<u16> {
    (i < MAX_TRACKS).then_some(i as u16)
}

/// The mix the engine plays track `i` with: its fader and its VCA's, Mute and Solo from the
/// track or its VCA.
fn mix_of(p: &Project, i: usize) -> Mix {
    let (gain_db, mute, solo) = p.track_mix_of(i);
    Mix {
        gain_db,
        mute,
        solo,
        pan: p.tracks[i].pan,
    }
}

/// Every track's mix as an engine song carries it.
fn track_mixes(p: &Project) -> Vec<TrackMix> {
    mixes(p)
        .into_iter()
        .map(|m| TrackMix {
            gain_db: m.gain_db,
            mute: m.mute,
            solo: m.solo,
            pan: m.pan,
        })
        .collect()
}

fn mixes(p: &Project) -> Vec<Mix> {
    (0..p.tracks.len().min(MAX_TRACKS))
        .map(|i| mix_of(p, i))
        .collect()
}

/// Notes of one MIDI region as the engine should play them, with the Region Inspector
/// (quantize, transpose, velocity) and Track Inspector (transpose, velocity, key and
/// velocity limits) applied non-destructively, as Logic does on playback.
pub(crate) fn region_notes(t: &Track, r: &Region, origin: f64, track: u16) -> Vec<NoteEvent> {
    let (rp, tp) = (r.params, t.params);
    r.notes
        .iter()
        .filter_map(|n| {
            let start = match rp.quantize {
                Some(q) if q > 0.0 => (n.start / q).round() * q,
                _ => n.start,
            };
            let pitch = n.pitch as i32 + rp.transpose as i32 + tp.transpose as i32;
            let vel = (n.vel as i32 + rp.velocity as i32 + tp.velocity as i32).clamp(1, 127);
            let in_keys = (tp.key_limit.0 as i32..=tp.key_limit.1 as i32).contains(&pitch);
            let in_vels = (tp.vel_limit.0 as i32..=tp.vel_limit.1 as i32).contains(&vel);
            (in_keys && in_vels && (0..=127).contains(&pitch)).then_some(NoteEvent {
                start_beat: origin + start as f64,
                end_beat: origin + (start + n.len) as f64,
                pitch: pitch as u8,
                vel: vel as u8,
                track,
            })
        })
        .collect()
}

/// Every unmuted region on an engine track, with its engine track number and start beat
/// (quarter notes, as the engine counts them).
fn playing_regions(p: &Project) -> impl Iterator<Item = (u16, &Track, &Region, f64)> {
    p.tracks
        .iter()
        .enumerate()
        .filter_map(|(ti, t)| Some((track_index(ti)?, t)))
        .flat_map(move |(track, t)| {
            t.regions
                .iter()
                .filter(|r| !r.params.mute)
                .map(move |r| (track, t, r, p.region_start_beats(r)))
        })
}

/// The controller changes external instruments get: those recorded in the regions of External
/// MIDI tracks (the sustain pedal among them), at their place on the timeline.
fn song_controls(p: &Project) -> Vec<ControlEvent> {
    p.tracks
        .iter()
        .enumerate()
        .filter_map(|(ti, t)| Some((track_index(ti)?, t)))
        .filter(|(_, t)| t.kind == TrackKind::ExternalMidi)
        .flat_map(|(track, t)| {
            let mut out = Vec::new();
            let (mut from, mut to) = (f64::MAX, 0.0f64);
            for r in t.regions.iter().filter(|r| !r.params.mute) {
                let origin = p.region_start_beats(r);
                from = from.min(origin);
                to = to.max(p.region_end_beats(r));
                out.extend(r.controls.iter().map(|c| ControlEvent {
                    beat: origin + f64::from(c.start),
                    track,
                    number: c.num.min(127),
                    value: c.val.min(127),
                }));
            }
            // A Modulator on the track sends its controller along all the regions.
            out.extend(
                crate::midi_fx::controls(&t.midi_fx, from, to)
                    .into_iter()
                    .map(|m| ControlEvent {
                        beat: m.beat,
                        track,
                        number: m.number,
                        value: m.value,
                    }),
            );
            out
        })
        .collect()
}

#[cfg(test)]
pub(crate) fn song_notes_for_test(p: &Project) -> Vec<NoteEvent> {
    song_notes(p)
}

fn song_notes(p: &Project) -> Vec<NoteEvent> {
    // The Groove Track's timing, for the tracks that follow it.
    let groove = p
        .tracks
        .iter()
        .any(|t| t.params.follow_groove)
        .then(|| crate::groove::template(p))
        .flatten();
    p.tracks
        .iter()
        .enumerate()
        .filter_map(|(ti, t)| Some((track_index(ti)?, t)))
        .flat_map(|(track, t)| {
            let mut notes: Vec<NoteEvent> = t
                .regions
                .iter()
                .filter(|r| !r.params.mute)
                .flat_map(|r| region_notes(t, r, p.region_start_beats(r), track))
                .collect();
            if let Some(g) = groove
                .as_ref()
                .filter(|_| t.params.follow_groove && !t.params.groove)
            {
                crate::groove::apply(&mut notes, g);
            }
            // The track's MIDI FX change what it plays.
            if t.midi_fx.iter().any(|s| s.on) {
                crate::midi_fx::apply(&t.midi_fx, notes)
            } else {
                notes
            }
        })
        .collect()
}

/// What the engine's streamed audio depends on: the track count and each playing audio
/// region's file, track and start. When an edit leaves it unchanged (MIDI edits, mixer
/// changes), only the notes are swapped and the audio streams keep running.
#[derive(PartialEq, Default, Debug)]
struct AudioLayout {
    tracks: usize,
    clips: Vec<(std::path::PathBuf, u16, u64, String)>,
}

/// The edits of region `r` of track `t`: its own gain, fades and reverse, with the fades
/// lengthened where it crossfades with another region.
fn clip_edit(p: &Project, t: &Track, r: &Region) -> prowave_engine::ClipEdit {
    let ti = p.tracks.iter().position(|x| std::ptr::eq(x, t));
    let ri = t.regions.iter().position(|x| std::ptr::eq(x, r));
    match (ti, ri) {
        (Some(ti), Some(ri)) => p.clip_edit_of(ti, ri),
        _ => r.params.clip_edit(),
    }
}

fn audio_layout(p: &Project) -> AudioLayout {
    AudioLayout {
        tracks: p.tracks.len(),
        clips: playing_regions(p)
            .filter_map(|(track, t, r, origin)| {
                let a = r.audio.as_ref()?;
                // Gain, fades and reverse are set when a clip starts streaming.
                let edit = format!("{:?}", clip_edit(p, t, r));
                Some((a.file.path.clone(), track, origin.to_bits(), edit))
            })
            .collect(),
    }
}

fn build_song(engine: &Engine, p: &Project) -> Song {
    // Every clip reads ahead as much as the whole song's prefetch memory allows.
    let streamed = playing_regions(p)
        .filter(|(_, _, r, _)| r.audio.is_some())
        .count();
    let clips = playing_regions(p)
        .filter_map(|(track, t, r, origin)| {
            let a = r.audio.as_ref()?;
            Some(engine.stream_clip_in(&a.file, track, origin, clip_edit(p, t, r), streamed))
        })
        .collect();
    let notes = song_notes(p);
    let tracks = track_mixes(p);
    Song::new(notes, tracks).with_clips(clips)
}

/// The project's MIDI as an engine song, without audio regions (which need a running
/// `Engine` to stream). Used by the offline benchmark.
pub fn offline_song(p: &Project) -> Song {
    Song::new(song_notes(p), track_mixes(p))
}

/// The project's Tempo track and time signature as an engine tempo map.
fn tempo_map(p: &Project) -> Command {
    Command::SetTempoMap(Box::new(p.tempo_map()))
}

/// What the engine's tempo map depends on: the start tempo, the tempo changes, the first
/// signature and the signature changes.
type TempoKey = (f32, Vec<TempoPoint>, (u8, u8), Vec<crate::model::SigChange>);

fn tempo_key(p: &Project) -> TempoKey {
    (p.tempo, p.tempo_points(), p.sig, p.sig_changes.clone())
}

impl EngineSync {
    pub fn new(engine: Engine, p: &Project) -> Self {
        let mut sync = Self {
            engine,
            playing: false,
            monitors: Vec::new(),
            tempo: None,
            metronome: false,
            cycle: (false, 0.0, 0.0),
            master: None,
            position: None,
            mixes: Vec::new(),
            patches: Vec::new(),
            externals: Vec::new(),
            controls: Vec::new(),
            mtc: None,
            song_rev: None,
            layout: None,
            fx: Vec::new(),
            routes: Vec::new(),
            pdc: Vec::new(),
            automation: Vec::new(),
            retry: false,
        };
        sync.push(p);
        sync
    }

    /// Call once per frame: push UI-side changes to the engine, then pull the playhead back.
    pub fn sync(&mut self, p: &mut Project) {
        let counted = !self.playing && p.playing && p.recording && p.count_in;
        let seek_done = self.push(p);
        if counted && self.playing {
            p.counting_in = true;
        } else if p.counting_in && (!p.playing || self.engine.state().playing) {
            p.counting_in = false;
        }
        // Engine owns time while playing, unless a seek is still waiting to be accepted
        // (then the UI keeps the requested position and `push` retries it).
        if seek_done {
            if p.playing {
                p.position_beats = self.engine.state().position_beats;
            }
            self.position = Some(p.position_beats);
        }
    }

    /// Sends every change the engine has not accepted yet. Returns `false` while a playhead
    /// move is still pending.
    fn push(&mut self, p: &Project) -> bool {
        self.retry = false;
        // Tracks or regions changed. Re-streaming audio reopens every file and briefly drops
        // it out, so swap only the notes unless the audio layout itself changed.
        if self.song_rev != Some(p.song_rev) {
            let layout = audio_layout(p);
            let sent = if self.layout.as_ref() == Some(&layout) {
                self.send(Command::set_notes(song_notes(p)))
            } else {
                let song = build_song(&self.engine, p);
                let sent = self.send(Command::SetSong(Box::new(song)));
                if sent {
                    self.mixes = mixes(p);
                    self.layout = Some(layout);
                    // The new song has no controllers yet: they follow below.
                    self.controls.clear();
                }
                sent
            };
            if sent {
                self.song_rev = Some(p.song_rev);
            }
        }
        let controls = song_controls(p);
        if self.song_rev == Some(p.song_rev)
            && controls != self.controls
            && self.send(Command::set_controls(controls.clone()))
        {
            self.controls = controls;
        }
        // The user moved the playhead (ruler drag, rewind buttons, menu).
        let seek_done = match self.position {
            Some(q) if (p.position_beats - q).abs() <= 1e-9 => true,
            _ => self.send(Command::SetPosition {
                beat: p.position_beats,
            }),
        };
        let monitors = monitors_of(p);
        if monitors != self.monitors {
            let mut tracks = [MonitorTrack::default(); MAX_MONITORS];
            tracks[..monitors.len()].copy_from_slice(&monitors);
            let count = monitors.len() as u8;
            if self.send(Command::SetMonitors { tracks, count }) {
                self.monitors = monitors;
            }
        }
        if p.playing != self.playing
            && self.send(if p.playing {
                start_command(p)
            } else {
                Command::Pause
            })
        {
            self.playing = p.playing;
        }
        let key = tempo_key(p);
        if self.tempo.as_ref() != Some(&key) && self.send(tempo_map(p)) {
            self.tempo = Some(key);
        }
        let mtc = (p.mtc_out.is_some(), p.smpte_rate, p.smpte_offset_frames());
        if self.mtc != Some(mtc)
            && self.send(Command::SetMtc {
                on: mtc.0,
                rate: mtc.1,
                offset_frames: mtc.2,
            })
        {
            self.mtc = Some(mtc);
        }
        if p.metronome != self.metronome && self.send(Command::SetMetronome { on: p.metronome }) {
            self.metronome = p.metronome;
        }
        let cycle = (p.cycle, p.cycle_start, p.cycle_end);
        if cycle != self.cycle
            && self.send(Command::SetCycle {
                on: p.cycle,
                start_beat: p.cycle_start,
                end_beat: p.cycle_end,
            })
        {
            self.cycle = cycle;
        }
        if self.master != Some(p.master_vol)
            && self.send(Command::SetMasterGain {
                linear: p.master_vol,
            })
        {
            self.master = Some(p.master_vol);
        }
        // Library patches: send each track's synth patch whenever it changes. Stop at the
        // first rejection so `patches` stays indexed by track; the rest go next frame.
        for (i, t) in p.tracks.iter().enumerate().take(MAX_TRACKS) {
            let patch = synth_patch(t);
            if self.patches.get(i) == Some(&patch) {
                continue;
            }
            let track = i as u16;
            if !self.send(Command::SetTrackPatch { track, patch }) {
                break;
            }
            if i < self.patches.len() {
                self.patches[i] = patch;
            } else {
                self.patches.push(patch);
            }
        }
        // External MIDI tracks: their notes go out as MIDI instead of sounding here.
        for (i, t) in p.tracks.iter().enumerate().take(MAX_TRACKS) {
            let external = t.kind == TrackKind::ExternalMidi;
            if self.externals.get(i).copied().unwrap_or(false) == external {
                continue;
            }
            if !self.send(Command::SetTrackExternal {
                track: i as u16,
                external,
            }) {
                break;
            }
            self.externals
                .resize(self.externals.len().max(i + 1), false);
            self.externals[i] = external;
        }
        self.sync_strips(p);
        // Mixer moves. Until a song is accepted, its `SetSong` carries the mix instead.
        if self.layout.is_some() {
            for i in 0..p.tracks.len().min(MAX_TRACKS) {
                let m = mix_of(p, i);
                if self.mixes.get(i) == Some(&m) {
                    continue;
                }
                let mix = TrackMix {
                    gain_db: m.gain_db,
                    mute: m.mute,
                    solo: m.solo,
                    pan: m.pan,
                };
                if !self.send(Command::SetTrackMix {
                    track: i as u16,
                    mix,
                }) {
                    break;
                }
                if let Some(slot) = self.mixes.get_mut(i) {
                    *slot = m;
                }
            }
        }
        seek_done
    }

    /// Effects, sends and automation. Each is sent only when it differs from what the engine
    /// has; a rejected command is tried again next frame.
    fn sync_strips(&mut self, p: &Project) {
        let sr = self.engine.sample_rate() as f32;
        let n = p.tracks.len().min(MAX_TRACKS).max(self.fx.len());
        self.fx.resize_with(n, Vec::new);
        self.routes.resize(n, Route::default());
        for i in 0..n {
            let want: &[Insert] = p
                .tracks
                .get(i)
                .filter(|_| i < MAX_TRACKS)
                .map_or(&[], |t| &t.inserts);
            let same_kinds = self.fx[i].len() == want.len()
                && self.fx[i].iter().zip(want).all(|(a, b)| a.kind == b.kind);
            if !same_kinds {
                let chain = chain_of(want, sr);
                if !self.send(Command::SetInserts {
                    track: i as u16,
                    chain,
                }) {
                    return;
                }
                self.fx[i] = want.to_vec();
            } else {
                for (slot, (have, w)) in self.fx[i].iter_mut().zip(want).enumerate() {
                    if have == w {
                        continue;
                    }
                    let cmd = Command::SetFx {
                        track: i as u16,
                        slot: slot as u8,
                        params: w.params,
                        bypass: w.bypass,
                    };
                    if !self.engine.send(cmd) {
                        self.retry = true;
                        return;
                    }
                    *have = *w;
                }
            }
            let route = route_of(p, i);
            if self.routes[i] != route {
                if !self.send(Command::SetRoute {
                    track: i as u16,
                    route,
                }) {
                    return;
                }
                self.routes[i] = route;
            }
        }
        // Delay compensation follows the effects (a limiter looks ahead).
        let plan = pdc_plan(p, sr);
        self.pdc.resize(n, (0, 0));
        for (i, want) in plan.iter().copied().enumerate().take(n) {
            if self.pdc[i] != want {
                let cmd = Command::SetPdc {
                    track: i as u16,
                    pdc: Box::new(Pdc::new(want.0, want.1)),
                };
                if !self.send(cmd) {
                    return;
                }
                self.pdc[i] = want;
            }
        }
        // Tracks that are gone have no compensation either.
        for i in plan.len()..n {
            if self.pdc[i] != (0, 0) {
                let cmd = Command::SetPdc {
                    track: i as u16,
                    pdc: Box::new(Pdc::default()),
                };
                if !self.send(cmd) {
                    return;
                }
                self.pdc[i] = (0, 0);
            }
        }
        if !automation_unchanged(p, &self.automation) {
            let lanes = automation_lanes(p);
            if self.send(Command::SetAutomation(lanes.clone())) {
                self.automation = lanes;
            }
        }
    }

    /// `Engine::send`, remembering a rejection so the caller keeps redrawing to retry.
    fn send(&mut self, cmd: Command) -> bool {
        let sent = self.engine.send(cmd);
        self.retry |= !sent;
        sent
    }

    /// The MIDI the engine sends to external instruments (once).
    pub fn take_midi_out(&mut self) -> Option<rtrb::Consumer<prowave_engine::MidiOutEvent>> {
        self.engine.take_midi_out()
    }

    /// The MIDI Time Code the engine sends (once).
    pub fn take_mtc_out(&mut self) -> Option<rtrb::Consumer<prowave_engine::MtcMessage>> {
        self.engine.take_mtc_out()
    }

    /// Whether a command was rejected (queue full) and is waiting to be retried.
    pub fn needs_retry(&self) -> bool {
        self.retry
    }

    pub fn underruns(&self) -> u32 {
        self.engine
            .shared()
            .underruns
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Post-fader stereo peaks for the first `n` tracks (linear), for the mixer meters.
    pub fn track_meters(&mut self, n: usize) -> Vec<(f32, f32)> {
        let state = self.engine.state();
        (0..n).map(|i| state.track_peaks(i)).collect()
    }

    /// Gain reduction in dB of the compressors of the first `n` tracks, for the mixer.
    pub fn gain_reduction(&mut self, n: usize) -> Vec<f32> {
        let state = self.engine.state();
        (0..n).map(|i| state.gain_reduction(i)).collect()
    }

    /// Highest audio-callback DSP load since the last call (1.0 = full budget).
    pub fn take_dsp_load(&self) -> f32 {
        self.engine.shared().take_dsp_load()
    }

    pub fn master_peaks(&mut self) -> (f32, f32) {
        self.engine.state().peaks
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Note, RegionParams, TrackKind, TrackParams};
    use prowave_engine::TempoMap;

    fn track(notes: Vec<Note>) -> Track {
        Track {
            name: "t".into(),
            kind: TrackKind::Instrument,
            color: egui::Color32::GREEN,
            mute: false,
            solo: false,
            rec: false,
            monitor: false,
            input: prowave_engine::InputSource::Mono(0),
            output: 0,
            volume_db: 0.0,
            pan: 0.0,
            inserts: vec![],
            instrument: None,
            regions: vec![Region {
                name: "r".into(),
                start_bar: 1.0,
                len_bars: 1.0,
                notes,
                controls: Vec::new(),
                folder: None,
                generated: None,
                audio: None,
                params: RegionParams::default(),
            }],
            params: TrackParams::default(),
            category: None,
            sends: Vec::new(),
            auto: crate::model::Automation::default(),
            group: None,
            height: 1.0,
            indent: 0,
            folded: false,
            vca: None,
            midi_port: None,
            midi_channel: 1,
            midi_fx: Vec::new(),
        }
    }

    fn note(start: f32, pitch: u8, vel: u8) -> Note {
        Note {
            start,
            len: 0.5,
            pitch,
            vel,
        }
    }

    #[test]
    fn sends_aux_buses_and_automation_reach_the_engine_as_the_project_says() {
        use crate::model::{AutoMode, AutoTarget, SendSlot};
        let mut p = Project::demo();
        let a1 = p.add_track(TrackKind::Aux);
        let a2 = p.add_track(TrackKind::Aux);
        p.tracks[0].sends = vec![
            SendSlot {
                bus: Some(2),
                level: 0.5,
                pre: true,
            },
            SendSlot {
                bus: None,
                level: 0.9,
                pre: false,
            },
        ];
        // Aux strips take Bus 1, Bus 2... in order, and do not send themselves.
        p.tracks[a1].sends = p.tracks[0].sends.clone();
        assert_eq!(
            (p.aux_bus_of(a1), p.aux_bus_of(a2), p.aux_bus_of(0)),
            (1, 2, 0)
        );
        assert_eq!(route_of(&p, a2).bus_in, 2);
        assert!(!route_of(&p, a1).has_sends());
        let r = route_of(&p, 0);
        assert_eq!((r.bus_in, r.sends[0].bus, r.sends[0].pre), (0, 2, true));
        assert_eq!(r.sends[1].bus, 0, "an empty slot sends nothing");
        // More than eight Aux strips: the ninth hears no bus.
        for _ in 0..8 {
            p.add_track(TrackKind::Aux);
        }
        assert_eq!(p.aux_bus_of(p.tracks.len() - 1), 0);

        // Automation: the lanes sent are what the project has, and the comparison knows it.
        assert!(automation_unchanged(&p, &[]));
        p.tracks[1].auto.mode = AutoMode::Read;
        p.tracks[1].auto.set_point(AutoTarget::Volume, 2.0, -9.0);
        p.tracks[1].auto.set_point(AutoTarget::Pan, 1.0, 0.25);
        let lanes = automation_lanes(&p);
        assert_eq!(lanes.len(), 2);
        assert!(lanes.iter().all(|l| l.track == 1 && l.follow));
        assert!(automation_unchanged(&p, &lanes));
        p.tracks[1].auto.set_point(AutoTarget::Volume, 3.0, -9.0);
        assert!(!automation_unchanged(&p, &lanes));
        let lanes = automation_lanes(&p);
        p.tracks[1].auto.mode = AutoMode::Write;
        assert!(!automation_unchanged(&p, &lanes), "no longer played");
        assert!(automation_lanes(&p).iter().all(|l| !l.follow));
        // A VCA's level is added to the lane.
        let lanes = automation_lanes(&p);
        let vca = p.add_track(TrackKind::Vca);
        p.tracks[vca].volume_db = -4.0;
        p.assign_vca(1, Some(vca));
        assert!(!automation_unchanged(&p, &lanes));
        assert_eq!(automation_lanes(&p)[0].offset_db, -4.0);
        // The effects as the audio thread gets them.
        let chain = chain_of(&p.tracks[0].inserts, 48_000.0);
        assert_eq!(chain.fx.len(), p.tracks[0].inserts.len());
    }

    #[test]
    fn a_look_ahead_effect_delays_the_other_strips_so_everything_stays_in_time() {
        use crate::model::Insert;
        use prowave_engine::FxKind;
        let sr = 48_000.0;
        let mut p = Project::empty();
        let a = p.add_track(TrackKind::Audio);
        let b = p.add_track(TrackKind::Audio);
        let aux = p.add_track(TrackKind::Aux);
        let vca = p.add_track(TrackKind::Vca);
        // Nothing looks ahead: nothing is delayed.
        assert!(pdc_plan(&p, sr).iter().all(|d| *d == (0, 0)));
        let look = FxKind::Limiter.latency_samples(sr);
        assert!(look > 0);
        p.tracks[a].inserts = vec![Insert::new(FxKind::Limiter)];
        let plan = pdc_plan(&p, sr);
        assert_eq!(latency_of(&p.tracks[a], sr), look);
        // The limiter's own strip is the latest: the others wait for it before their sends
        // and faders; none of them needs more after.
        assert_eq!(plan[a], (0, 0));
        assert_eq!(plan[b], (look, 0));
        assert_eq!(plan[0], (look, 0));
        assert_eq!(plan[vca], (0, 0), "a VCA has no sound");
        // A bypassed limiter delays nothing.
        p.tracks[a].inserts[0].bypass = true;
        assert!(pdc_plan(&p, sr).iter().all(|d| *d == (0, 0)));
        p.tracks[a].inserts[0].bypass = false;
        // A limiter on an Aux strip: every other strip is delayed after its fader, by as much
        // as the Aux strip is late; the Aux strip itself needs nothing.
        p.tracks[aux].inserts = vec![Insert::new(FxKind::Limiter)];
        let plan = pdc_plan(&p, sr);
        assert_eq!(plan[aux], (0, 0));
        assert_eq!(plan[b], (look, look));
        assert_eq!(plan[a], (0, look));
        // Two limiters in a row add up.
        p.tracks[b].inserts = vec![Insert::new(FxKind::Limiter), Insert::new(FxKind::Limiter)];
        assert_eq!(latency_of(&p.tracks[b], sr), 2 * look);
        assert_eq!(pdc_plan(&p, sr)[a].0, look);
    }

    #[test]
    fn quantize_transpose_and_velocity_are_applied() {
        let mut t = track(vec![note(0.9, 60, 100)]);
        t.regions[0].params = RegionParams {
            mute: false,
            quantize: Some(0.25),
            transpose: 12,
            velocity: -10,
            ..RegionParams::default()
        };
        t.params.transpose = -2;
        let out = region_notes(&t, &t.regions[0], 4.0, 3);
        assert_eq!(out.len(), 1);
        assert!(
            (out[0].start_beat - 5.0).abs() < 1e-6,
            "{}",
            out[0].start_beat
        );
        assert_eq!((out[0].pitch, out[0].vel, out[0].track), (70, 90, 3));
    }

    #[test]
    fn key_and_velocity_limits_filter_notes() {
        let mut t = track(vec![
            note(0.0, 40, 100),
            note(1.0, 60, 100),
            note(2.0, 60, 20),
        ]);
        t.params.key_limit = (48, 72);
        t.params.vel_limit = (30, 127);
        let out = region_notes(&t, &t.regions[0], 0.0, 0);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].pitch, 60);
        assert!((out[0].start_beat - 1.0).abs() < 1e-6);
    }

    #[test]
    fn velocity_is_clamped_and_out_of_range_pitch_dropped() {
        let mut t = track(vec![note(0.0, 120, 120), note(1.0, 60, 120)]);
        t.regions[0].params.transpose = 12;
        t.regions[0].params.velocity = 50;
        let out = region_notes(&t, &t.regions[0], 0.0, 0);
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].pitch, out[0].vel), (72, 127));
    }

    #[test]
    fn regions_start_where_the_engine_puts_the_bar_in_any_signature() {
        let mut p = Project::demo();
        p.tracks.truncate(1);
        p.tracks[0].regions.truncate(1);
        p.tracks[0].regions[0].params.mute = false;
        p.tracks[0].regions[0].start_bar = 3.0;
        for (sig, quarters_per_bar) in [((4, 4), 4.0), ((6, 8), 3.0), ((7, 8), 3.5), ((2, 2), 4.0)]
        {
            p.sig = sig;
            assert_eq!(p.bar_len(0), quarters_per_bar, "{sig:?}");
            let (_, _, _, origin) = playing_regions(&p).next().unwrap();
            // Bar 3 (1-based) is the engine's bar 2 (0-based).
            let engine_beat = TempoMap::constant(p.tempo, sig).beat_of_bar(2);
            assert!(
                (origin - engine_beat).abs() < 1e-9,
                "{sig:?}: {origin} vs {engine_beat}"
            );
        }
    }

    #[test]
    fn external_tracks_send_the_controllers_recorded_in_their_regions() {
        use crate::model::{Control, TrackKind};
        let mut p = Project::demo();
        let ext = p.add_track(TrackKind::ExternalMidi);
        let plain = p.add_track(TrackKind::Instrument);
        for t in [ext, plain] {
            p.tracks[t].regions.push(crate::model::Region {
                name: "r".into(),
                start_bar: 3.0,
                len_bars: 2.0,
                notes: vec![],
                controls: vec![
                    Control {
                        start: 1.0,
                        num: 64,
                        val: 127,
                    },
                    Control {
                        start: 2.0,
                        num: 64,
                        val: 0,
                    },
                    Control {
                        start: 0.5,
                        num: 1,
                        val: 200,
                    },
                ],
                folder: None,
                audio: None,
                params: RegionParams::default(),
                generated: None,
            });
        }
        let c = song_controls(&p);
        assert_eq!(c.len(), 3, "only the External MIDI track: {c:?}");
        assert!(c.iter().all(|e| e.track == ext as u16));
        // Bar 3 starts at beat 8.
        let mut beats: Vec<f64> = c.iter().map(|e| e.beat).collect();
        beats.sort_by(f64::total_cmp);
        assert_eq!(beats, [8.5, 9.0, 10.0]);
        assert!(c.iter().all(|e| e.value <= 127), "wild values are repaired");
        p.tracks[ext].regions[0].params.mute = true;
        assert!(song_controls(&p).is_empty(), "a muted region sends nothing");
    }

    #[test]
    fn a_tracks_midi_fx_change_the_notes_the_engine_gets() {
        use crate::midi_fx::{FxSlot, MidiFx};
        let mut p = Project::demo();
        let plain = song_notes(&p);
        p.tracks[0].midi_fx = vec![FxSlot {
            fx: MidiFx::Transposer { semitones: 7 },
            on: true,
        }];
        let fx = song_notes(&p);
        assert_eq!(plain.len(), fx.len());
        let first =
            |notes: &[NoteEvent]| notes.iter().filter(|n| n.track == 0).map(|n| n.pitch).max();
        assert_eq!(first(&fx), first(&plain).map(|x| x + 7));
        // The other tracks are as they were.
        let other = |notes: &[NoteEvent]| notes.iter().filter(|n| n.track == 1).count();
        assert_eq!(other(&fx), other(&plain));
        // Switched off, the notes are the original ones.
        p.tracks[0].midi_fx[0].on = false;
        assert_eq!(song_notes(&p).len(), plain.len());
        assert_eq!(first(&song_notes(&p)), first(&plain));
    }

    #[test]
    fn regions_and_the_engine_agree_on_bars_with_several_signatures() {
        let mut p = Project::demo();
        p.set_signature_from(3, (3, 4));
        p.set_signature_from(5, (6, 8));
        p.set_signature_from(7, (7, 8));
        p.set_signature_from(10, (5, 4));
        let map = p.tempo_map();
        for bar in 0..14u32 {
            let engine = map.beat_of_bar(bar);
            let app = p.bar_to_beats(bar as f64);
            assert!((engine - app).abs() < 1e-9, "bar {bar}: {engine} vs {app}");
        }
        for beats in [0.0, 3.9, 8.0, 12.5, 14.0, 20.0, 23.4, 30.0, 41.0] {
            let pos = map.position(beats);
            let bars = p.beats_to_bar(beats);
            assert_eq!(pos.bar as f64, bars.floor(), "bar of {beats}");
        }
        // A change of signature is sent to the engine as a change of the tempo map.
        let before = tempo_key(&p);
        p.set_signature_from(12, (2, 4));
        assert_ne!(tempo_key(&p), before);
    }

    #[test]
    fn editing_the_signature_repositions_the_notes_sent_to_the_engine() {
        let mut p = Project::demo();
        p.tracks.truncate(1);
        p.tracks[0].regions.truncate(1);
        p.tracks[0].regions[0].params.mute = false;
        p.tracks[0].regions[0].start_bar = 3.0;
        let first = |p: &Project| {
            song_notes(p)
                .iter()
                .map(|n| n.start_beat)
                .fold(f64::MAX, f64::min)
        };
        let (before, rev) = (first(&p), p.song_rev);
        // 4/4 -> 3/4: bar 3 starts at quarter note 6 instead of 8, so every note moves 2 earlier.
        assert!(p.step_signature(crate::model::SigPart::Numerator, -1));
        assert!(
            (before - first(&p) - 2.0).abs() < 1e-9,
            "{before} -> {}",
            first(&p)
        );
        assert_ne!(p.song_rev, rev, "the engine is told to rebuild the song");
    }

    #[test]
    fn bar_display_counts_signature_beats() {
        let mut p = Project::demo();
        // 4.75 quarter notes: bar 2 + 1.75 quarters.
        assert_eq!(p.bbdt(4.75), (2, 1, 4, 1));
        // In 6/8 a bar is 3 quarters: bar 2 + 1.75 quarters = 3.5 eighths -> beat 4, 16th 2.
        p.sig = (6, 8);
        assert_eq!(p.bbdt(4.75), (2, 4, 2, 1));
        p.position_beats = 4.75;
        assert_eq!(p.bar_beat_tick(), (2, 4, 480));
    }

    #[test]
    fn tracks_past_the_engine_limit_are_left_out_not_aliased() {
        let mut p = Project::demo();
        p.tracks.truncate(1);
        p.tracks[0].regions.truncate(1);
        p.tracks[0].regions[0].params.mute = false;
        p.tracks = vec![p.tracks[0].clone(); MAX_TRACKS + 3];
        let notes = song_notes(&p);
        let per_track = notes.iter().filter(|n| n.track == 0).count();
        assert!(per_track > 0);
        assert_eq!(notes.len(), per_track * MAX_TRACKS);
        assert_eq!(
            notes
                .iter()
                .filter(|n| usize::from(n.track) == MAX_TRACKS - 1)
                .count(),
            per_track,
            "the last engine track must not collect notes from later tracks"
        );
        assert_eq!(mixes(&p).len(), MAX_TRACKS);
        assert_eq!(track_index(MAX_TRACKS - 1), Some((MAX_TRACKS - 1) as u16));
        assert_eq!(track_index(MAX_TRACKS), None);
    }

    #[test]
    fn only_audio_changes_need_a_full_song_rebuild() {
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("loop.wav");
        crate::project_io::tests::write_wav(&wav);
        let ctx = egui::Context::default();
        let mut p = Project::demo();
        crate::import::import_audio(&mut p, &wav, &ctx).unwrap();
        let before = audio_layout(&p);
        assert_eq!(before.clips.len(), 1);

        // MIDI edits and mixer moves keep the streamed audio.
        p.tracks[0].regions[0].notes[0].pitch += 1;
        p.tracks[0].regions[0].start_bar += 1.0;
        p.tracks[1].volume_db = -12.0;
        assert_eq!(audio_layout(&p), before);

        // Moving or muting the audio region, or adding a track, does not.
        let audio = p.tracks.len() - 1;
        p.tracks[audio].regions[0].start_bar += 1.0;
        assert_ne!(audio_layout(&p), before);
        p.tracks[audio].regions[0].start_bar -= 1.0;
        p.tracks[audio].regions[0].params.mute = true;
        assert_ne!(audio_layout(&p), before);
        p.tracks[audio].regions[0].params.mute = false;
        // Nor do gain, fades or reverse, which a clip gets when it starts streaming.
        for edit in [
            |r: &mut RegionParams| r.gain_db = -6.0,
            |r: &mut RegionParams| r.fade_out = 1.0,
            |r: &mut RegionParams| r.reverse = true,
        ] {
            let mut q = Project::demo();
            q.tracks = p.tracks.clone();
            let before = audio_layout(&q);
            edit(&mut q.tracks[audio].regions[0].params);
            assert_ne!(audio_layout(&q), before);
        }
        p.tracks.push(p.tracks[0].clone());
        assert_ne!(audio_layout(&p), before);
    }

    #[test]
    fn record_with_count_in_clicks_one_bar_of_the_signature() {
        let mut p = Project::empty();
        assert!(matches!(start_command(&p), Command::Play), "not recording");
        p.recording = true;
        p.count_in = false;
        assert!(matches!(start_command(&p), Command::Play), "count-in off");
        p.count_in = true;
        p.sig = (6, 8);
        match start_command(&p) {
            Command::CountIn {
                clicks,
                beats_per_click,
            } => assert_eq!((clicks, beats_per_click), (6, 0.5)),
            _ => panic!("no count-in"),
        }
    }

    #[test]
    fn the_i_button_makes_a_monitor_of_the_tracks_input() {
        let mut p = Project::empty();
        p.tracks.clear();
        p.tracks.push(Track::new("Keys", TrackKind::Instrument));
        p.tracks.push(Track::new("Vox", TrackKind::Audio));
        p.tracks[0].monitor = true;
        assert!(monitors_of(&p).is_empty(), "audio tracks only");
        p.tracks[1].monitor = true;
        p.tracks[1].input = InputSource::Stereo(2);
        assert_eq!(
            monitors_of(&p),
            vec![MonitorTrack {
                track: 1,
                channel: 2,
                stereo: true
            }]
        );
    }

    #[test]
    fn a_track_can_play_into_a_bus_but_an_aux_cannot() {
        let mut p = Project::empty();
        p.tracks.clear();
        p.tracks.push(Track::new("Vox", TrackKind::Audio));
        p.tracks.push(Track::new("Aux 1", TrackKind::Aux));
        p.tracks[0].output = 3;
        p.tracks[1].output = 3;
        assert_eq!(route_of(&p, 0).output, 3);
        assert_eq!(route_of(&p, 1).output, 0);
    }
}
