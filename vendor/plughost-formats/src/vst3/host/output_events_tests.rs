use super::*;
use vst3::Steinberg::Vst::{Event__type0, NoteExpressionValueEvent, NoteOffEvent, NoteOnEvent};

fn native(kind: u32, body: Event__type0) -> Event {
    Event {
        busIndex: 0,
        sampleOffset: 0,
        ppqPosition: 0.0,
        flags: 0,
        r#type: kind as u16,
        __field0: body,
    }
}

fn note_on(pitch: i16, id: i32, tuning: f32) -> Event {
    native(
        NOTE_ON,
        Event__type0 {
            noteOn: NoteOnEvent {
                channel: 0,
                pitch,
                tuning,
                velocity: 0.5,
                length: 0,
                noteId: id,
            },
        },
    )
}

fn note_off(pitch: i16, id: i32) -> Event {
    native(
        NOTE_OFF,
        Event__type0 {
            noteOff: NoteOffEvent {
                channel: 0,
                pitch,
                velocity: 0.0,
                noteId: id,
                tuning: 0.0,
            },
        },
    )
}

/// Five octaves up for the note with `id`.
fn tuning(id: i32) -> Event {
    native(
        NOTE_EXPRESSION_VALUE,
        Event__type0 {
            noteExpressionValue: NoteExpressionValueEvent {
                typeId: note_expression::type_id(ExpressionKind::Tuning).unwrap(),
                noteId: id,
                value: 0.75,
            },
        },
    )
}

/// What the plugin's `events` become in one block, and how many had no portable form.
fn block(list: &OutputEventList, events: &[Event]) -> (Events, u64) {
    for event in events {
        let mut event = *event;
        assert_eq!(unsafe { list.addEvent(&mut event) }, kResultOk);
    }
    let mut taken = Events::for_block();
    list.take(&mut taken, 1).unwrap();
    (taken, list.take_unconvertible())
}

fn data(events: &Events) -> Vec<EventData<'_>> {
    events.iter().map(|event| event.data).collect()
}

fn five_octaves_up(key: u8, id: Option<u32>) -> EventData<'static> {
    let mut expression = NoteExpression::new(0, key, ExpressionKind::Tuning, 60.0);
    expression.id = id;
    EventData::Expression(expression)
}

#[test]
fn notes_ended_by_key_leave_room_to_follow_later_notes() {
    let list = OutputEventList::new().unwrap();
    for id in 0..MAX_BLOCK_EVENTS as i32 {
        let (_, unconvertible) = block(&list, &[note_on(60, id, 0.0), note_off(60, -1)]);
        assert_eq!(unconvertible, 0);
    }
    let (taken, unconvertible) = block(&list, &[note_on(62, 9999, 0.0), tuning(9999)]);
    assert_eq!(unconvertible, 0);
    assert_eq!(data(&taken)[1], five_octaves_up(62, Some(9999)));
}

#[test]
fn notes_with_ids_from_the_plugins_own_range_take_their_expressions_by_key() {
    let list = OutputEventList::new().unwrap();
    let (taken, unconvertible) = block(&list, &[note_on(60, -5000, 0.0), tuning(-5000)]);
    assert_eq!(unconvertible, 0);
    assert_eq!(
        data(&taken),
        [
            EventData::NoteOn(Note::new(0, 60, 0.5)),
            five_octaves_up(60, None)
        ]
    );
}

#[test]
fn a_note_on_with_a_tuning_that_is_not_a_number_plays_untuned() {
    let list = OutputEventList::new().unwrap();
    let (taken, unconvertible) = block(&list, &[note_on(60, 3, f32::NAN)]);
    assert_eq!(unconvertible, 0);
    assert_eq!(
        data(&taken),
        [EventData::NoteOn(Note::new(0, 60, 0.5).with_id(3))]
    );
}
