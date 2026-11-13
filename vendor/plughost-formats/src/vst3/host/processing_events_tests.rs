use super::*;

#[test]
fn native_parameter_queues_order_replace_bound_and_reuse_points() {
    let changes = ParameterChanges::new(3, 3).unwrap();
    let mut index = -1;
    let first = unsafe { changes.addParameterData(&9, &mut index) };
    assert_eq!(index, 0);
    let first = unsafe { vst3::ComRef::from_raw(first) }.unwrap();
    assert_eq!(unsafe { first.getParameterId() }, 9);
    assert_eq!(unsafe { first.addPoint(7, 0.7, &mut index) }, kResultOk);
    assert_eq!(unsafe { first.addPoint(1, 0.1, &mut index) }, kResultOk);
    assert_eq!(index, 0);
    changes.push(5, 0, 0.5);
    assert_eq!(unsafe { first.addPoint(1, 0.2, &mut index) }, kResultOk);
    assert_eq!(unsafe { first.addPoint(8, 0.8, &mut index) }, kResultFalse);
    assert_eq!(unsafe { first.getPointCount() }, 2);
    for (index, expected) in [(0, (1, 0.2)), (1, (7, 0.7))] {
        let (mut offset, mut value) = (-1, -1.0);
        assert_eq!(
            unsafe { first.getPoint(index, &mut offset, &mut value) },
            kResultOk
        );
        assert_eq!((offset, value), expected);
    }
    assert_eq!(last_values(&changes), [(9, 0.7), (5, 0.5)]);
    changes.clear();
    assert_eq!(unsafe { changes.getParameterCount() }, 0);
    assert!(unsafe { changes.getParameterData(0) }.is_null());
    changes.push(77, 2, 0.25);
    assert_eq!(last_values(&changes), [(77, 0.25)]);
    assert_eq!(unsafe { first.getParameterId() }, 77);
    changes.push(78, 0, 0.0);
    changes.push(79, 0, 0.0);
    assert!(unsafe { changes.addParameterData(&80, std::ptr::null_mut()) }.is_null());
}

#[test]
fn host_points_in_any_order_are_read_per_queue_in_offset_order_with_the_last_value() {
    let changes = ParameterChanges::new(2, 8).unwrap();
    for (id, offset, value) in [
        (1, 0, 0.0),
        (2, 0, 0.5),
        (1, 9, 0.9),
        (2, 3, 0.3),
        (1, 4, 0.4),
        (1, 9, 0.8),
    ] {
        changes.push(id, offset, value);
    }
    let points = |index| {
        let queue = unsafe { vst3::ComRef::from_raw(changes.getParameterData(index)) }.unwrap();
        let count = unsafe { queue.getPointCount() };
        let points: Vec<_> = (0..count)
            .map(|point| {
                let (mut offset, mut value) = (-1, -1.0);
                unsafe { queue.getPoint(point, &mut offset, &mut value) };
                (offset, value)
            })
            .collect();
        (unsafe { queue.getParameterId() }, points)
    };
    assert_eq!(points(0), (1, vec![(0, 0.0), (4, 0.4), (9, 0.8)]));
    assert_eq!(points(1), (2, vec![(0, 0.5), (3, 0.3)]));
    assert_eq!(last_values(&changes), [(1, 0.8), (2, 0.3)]);
}

fn last_values(changes: &ParameterChanges) -> Vec<(ParamID, ParamValue)> {
    let mut values = Vec::new();
    changes.each_last_value(|id, value| values.push((id, value)));
    values
}

#[test]
fn output_queues_keep_the_latest_point_of_each_parameter_without_running_out() {
    let changes = ParameterChanges::latest(2).unwrap();
    let meter = unsafe { changes.addParameterData(&3, std::ptr::null_mut()) };
    let meter = unsafe { vst3::ComRef::from_raw(meter) }.unwrap();
    for offset in 0..100 {
        let mut index = -1;
        let value = f64::from(offset) / 100.0;
        assert_eq!(
            unsafe { meter.addPoint(offset, value, &mut index) },
            kResultOk
        );
        assert_eq!(index, 0);
    }
    assert_eq!(
        unsafe { meter.addPoint(5, 0.05, std::ptr::null_mut()) },
        kResultOk
    );
    changes.push(4, 9, 0.9);
    assert_eq!(unsafe { meter.getPointCount() }, 1);
    let (mut offset, mut value) = (-1, -1.0);
    assert_eq!(
        unsafe { meter.getPoint(0, &mut offset, &mut value) },
        kResultOk
    );
    assert_eq!((offset, value), (5, 0.05));
    assert_eq!(last_values(&changes), [(3, 0.05), (4, 0.9)]);
}

#[test]
fn full_event_lists_refuse_events_and_clear_retains_a_usable_list() {
    let events = EventList::new(2).unwrap();
    let mut event: Event = unsafe { std::mem::zeroed() };
    event.sampleOffset = 1;
    assert_eq!(unsafe { events.addEvent(&mut event) }, kResultOk);
    event.sampleOffset = 2;
    assert_eq!(unsafe { events.addEvent(&mut event) }, kResultOk);
    assert_eq!(unsafe { events.addEvent(&mut event) }, kResultFalse);
    assert_eq!(unsafe { events.getEventCount() }, 2);
    assert_eq!(unsafe { events.getEvent(0, &mut event) }, kResultOk);
    assert_eq!(event.sampleOffset, 1);
    events.clear();
    event.sampleOffset = 3;
    assert_eq!(unsafe { events.addEvent(&mut event) }, kResultOk);
    assert_eq!(unsafe { events.getEventCount() }, 1);
    assert_eq!(unsafe { events.getEvent(1, &mut event) }, kResultFalse);
    assert_eq!(
        ParameterChanges::new(usize::MAX, usize::MAX)
            .err()
            .unwrap()
            .kind(),
        plughost_core::FailureKind::Configuration
    );
    assert_eq!(
        EventList::new(usize::MAX).err().unwrap().kind(),
        plughost_core::FailureKind::Configuration
    );
}
