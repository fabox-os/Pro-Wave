//! External MIDI tracks: a track that plays an outside instrument sends its notes out as MIDI
//! instead of sounding in the engine.
use prowave_engine::{
    Command, ControlEvent, EngineCore, MidiOutEvent, NoteEvent, Shared, Song, StateReader, TrackMix,
};
use std::sync::Arc;

#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC_GUARD: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

const RATE: u32 = 48_000;

struct Rig {
    core: EngineCore,
    tx: rtrb::Producer<Command>,
    midi: rtrb::Consumer<MidiOutEvent>,
    state: StateReader,
}

fn rig(notes: Vec<NoteEvent>) -> Rig {
    let (mut tx, rx) = rtrb::RingBuffer::new(32);
    let (gtx, _grx) = rtrb::RingBuffer::new(8);
    let mut core = EngineCore::new(RATE, rx, gtx, Arc::new(Shared::new()));
    let (Some(midi), Some(state)) = (core.take_midi_out_reader(), core.take_state_reader()) else {
        panic!("readers");
    };
    // Track 0 is an ordinary instrument, track 1 an external one.
    let song = Song::new(notes, vec![TrackMix::default(); 2]);
    assert!(tx.push(Command::SetSong(Box::new(song))).is_ok());
    assert!(tx
        .push(Command::SetTrackExternal {
            track: 1,
            external: true
        })
        .is_ok());
    Rig {
        core,
        tx,
        midi,
        state,
    }
}

fn note(track: u16, pitch: u8, start: f64, end: f64) -> NoteEvent {
    NoteEvent {
        start_beat: start,
        end_beat: end,
        pitch,
        vel: 90,
        track,
    }
}

fn run(rig: &mut Rig, frames: usize) {
    let mut out = vec![0.0f32; frames * 2];
    assert_no_alloc::assert_no_alloc(|| rig.core.process(&mut out));
}

fn events(rig: &mut Rig) -> Vec<MidiOutEvent> {
    std::iter::from_fn(|| rig.midi.pop().ok()).collect()
}

#[test]
fn timeline_notes_go_out_as_note_on_and_off() {
    // A beat is half a second at 120 bpm: the note lasts from beat 1 to beat 2.
    let mut r = rig(vec![note(1, 60, 1.0, 2.0)]);
    assert!(r.tx.push(Command::Play).is_ok());
    run(&mut r, RATE as usize / 4);
    assert!(events(&mut r).is_empty(), "nothing before the note starts");
    run(&mut r, RATE as usize / 2);
    assert_eq!(
        events(&mut r),
        [MidiOutEvent {
            track: 1,
            on: true,
            pitch: 60,
            vel: 90,
            controller: false,
            value: 0,
        }]
    );
    run(&mut r, RATE as usize / 2);
    assert_eq!(
        events(&mut r),
        [MidiOutEvent {
            track: 1,
            on: false,
            pitch: 60,
            vel: 0,
            controller: false,
            value: 0,
        }]
    );
}

#[test]
fn an_external_track_makes_no_sound_in_the_engine() {
    let mut r = rig(vec![note(1, 60, 0.0, 4.0)]);
    assert!(r.tx.push(Command::Play).is_ok());
    for _ in 0..8 {
        run(&mut r, 512);
    }
    let s = r.state.read();
    assert_eq!(
        s.track_peaks(1),
        (0.0, 0.0),
        "no engine sound on the external track"
    );
    assert_eq!(s.peaks, (0.0, 0.0));
    // An ordinary track still sounds.
    let mut r = rig(vec![note(0, 60, 0.0, 4.0)]);
    assert!(r.tx.push(Command::Play).is_ok());
    for _ in 0..8 {
        run(&mut r, 512);
    }
    assert!(r.state.read().track_peaks(0).0 > 0.0);
    assert!(events(&mut r).is_empty(), "and sends no MIDI");
}

#[test]
fn stopping_releases_the_notes_that_are_sounding() {
    let mut r = rig(vec![note(1, 64, 0.0, 8.0), note(1, 67, 0.0, 8.0)]);
    assert!(r.tx.push(Command::Play).is_ok());
    run(&mut r, 1024);
    assert_eq!(events(&mut r).iter().filter(|e| e.on).count(), 2);
    assert!(r.tx.push(Command::Pause).is_ok());
    run(&mut r, 256);
    let off = events(&mut r);
    assert_eq!(off.len(), 2);
    assert!(off.iter().all(|e| !e.on && e.track == 1));
    // And a seek in the middle of a note does too.
    assert!(r.tx.push(Command::Play).is_ok());
    run(&mut r, 1024);
    events(&mut r);
    assert!(r.tx.push(Command::SetPosition { beat: 20.0 }).is_ok());
    run(&mut r, 256);
    assert!(events(&mut r).iter().all(|e| !e.on));
}

#[test]
fn live_notes_go_out_and_are_released_by_note_off() {
    let mut r = rig(vec![]);
    assert!(r
        .tx
        .push(Command::NoteOn {
            pitch: 72,
            vel: 100,
            track: 1
        })
        .is_ok());
    run(&mut r, 256);
    assert_eq!(
        events(&mut r),
        [MidiOutEvent {
            track: 1,
            on: true,
            pitch: 72,
            vel: 100,
            controller: false,
            value: 0,
        }]
    );
    // A seek or the transport does not cut a held key.
    assert!(r.tx.push(Command::SetPosition { beat: 4.0 }).is_ok());
    run(&mut r, 256);
    assert!(events(&mut r).is_empty());
    assert!(r
        .tx
        .push(Command::NoteOffTrack {
            pitch: 72,
            track: 1
        })
        .is_ok());
    run(&mut r, 256);
    assert_eq!(
        events(&mut r),
        [MidiOutEvent {
            track: 1,
            on: false,
            pitch: 72,
            vel: 0,
            controller: false,
            value: 0,
        }]
    );
}

#[test]
fn a_track_that_stops_being_external_releases_its_notes() {
    let mut r = rig(vec![note(1, 60, 0.0, 8.0)]);
    assert!(r.tx.push(Command::Play).is_ok());
    run(&mut r, 1024);
    events(&mut r);
    assert!(r
        .tx
        .push(Command::SetTrackExternal {
            track: 1,
            external: false
        })
        .is_ok());
    run(&mut r, 256);
    let out = events(&mut r);
    assert_eq!(out.len(), 1);
    assert!(!out[0].on);
}

fn cc(beat: f64, track: u16, number: u8, value: u16) -> ControlEvent {
    ControlEvent {
        beat,
        track,
        number,
        value,
    }
}

fn controller(track: u16, number: u8, value: u16) -> MidiOutEvent {
    MidiOutEvent {
        track,
        on: false,
        pitch: number,
        vel: value.min(127) as u8,
        controller: true,
        value,
    }
}

#[test]
fn controllers_go_out_on_time_for_external_tracks_only() {
    let mut r = rig(vec![]);
    assert!(r
        .tx
        .push(Command::set_controls(vec![
            cc(2.0, 1, 64, 0),
            cc(1.0, 1, 64, 127),
            cc(1.0, 0, 64, 127), // an ordinary instrument: nothing goes out
            cc(1.5, 1, 1, 33),
        ]))
        .is_ok());
    assert!(r.tx.push(Command::Play).is_ok());
    run(&mut r, RATE as usize / 4); // 0.5 beat in
    assert!(events(&mut r).is_empty());
    run(&mut r, RATE as usize / 2); // beat 1.25
    assert_eq!(events(&mut r), [controller(1, 64, 127)]);
    run(&mut r, RATE as usize / 4); // beat 1.75
    assert_eq!(events(&mut r), [controller(1, 1, 33)]);
    run(&mut r, RATE as usize / 2); // beat 2.25
    assert_eq!(events(&mut r), [controller(1, 64, 0)]);
}

#[test]
fn a_seek_picks_up_from_there_and_a_stop_lets_the_pedal_up() {
    let mut r = rig(vec![]);
    assert!(r
        .tx
        .push(Command::set_controls(vec![
            cc(1.0, 1, 64, 127),
            cc(5.0, 1, 64, 0)
        ]))
        .is_ok());
    assert!(r.tx.push(Command::SetPosition { beat: 3.0 }).is_ok());
    assert!(r.tx.push(Command::Play).is_ok());
    run(&mut r, RATE as usize / 8);
    assert!(
        events(&mut r).is_empty(),
        "the pedal press at beat 1 is behind us"
    );
    // Pedal down in the song at beat 6: jump back to 0, let it play to the press, then pause.
    assert!(r.tx.push(Command::SetPosition { beat: 0.0 }).is_ok());
    run(&mut r, RATE as usize);
    assert_eq!(events(&mut r), [controller(1, 64, 127)]);
    assert!(r.tx.push(Command::Pause).is_ok());
    run(&mut r, 256);
    assert_eq!(
        events(&mut r),
        [controller(1, 64, 0)],
        "paused: the pedal goes up"
    );
    assert!(r.tx.push(Command::Pause).is_ok());
    run(&mut r, 256);
    assert!(events(&mut r).is_empty(), "and only once");
}

#[test]
fn a_song_carries_its_controllers() {
    let song = Song::new(vec![], vec![TrackMix::default(); 2])
        .with_controls(vec![cc(2.0, 1, 7, 90), cc(0.5, 1, 7, 10)]);
    assert_eq!(song.controls[0].beat, 0.5);
    let mut r = rig(vec![]);
    assert!(r.tx.push(Command::SetSong(Box::new(song))).is_ok());
    assert!(r
        .tx
        .push(Command::SetTrackExternal {
            track: 1,
            external: true
        })
        .is_ok());
    assert!(r.tx.push(Command::Play).is_ok());
    run(&mut r, RATE as usize / 2);
    assert_eq!(events(&mut r), [controller(1, 7, 10)]);
}

/// Pitch bend (kind 128) and channel pressure (129) reach the sender with their full value, on
/// the beat, for an external track only.
#[test]
fn pitch_bend_and_pressure_go_out_with_their_full_value() {
    let mut r = rig(vec![]);
    assert!(r
        .tx
        .push(Command::set_controls(vec![
            cc(1.0, 1, 128, 16383),
            cc(1.5, 1, 129, 90),
            cc(2.0, 0, 128, 4000),
        ]))
        .is_ok());
    assert!(r
        .tx
        .push(Command::SetTrackExternal {
            track: 1,
            external: true
        })
        .is_ok());
    assert!(r.tx.push(Command::Play).is_ok());
    run(&mut r, 48_000 * 3);
    let out = events(&mut r);
    assert_eq!(
        out,
        vec![controller(1, 128, 16383), controller(1, 129, 90)],
        "track 0 is not external, so its bend stays inside the engine"
    );
}
