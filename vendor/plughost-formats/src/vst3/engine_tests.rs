use super::*;

#[test]
fn a_later_point_holds_the_previous_value_until_its_offset() {
    let held = |value, block, offset| Held {
        value,
        block,
        offset,
        queue: None,
    };
    // From an earlier block, or earlier in this block: hold right before the new point.
    assert_eq!(held(0.5, 0, 7).hold_before(64, 10), Some((9, 0.5)));
    assert_eq!(held(0.5, 64, 3).hold_before(64, 10), Some((9, 0.5)));
    // At the start of the block, or with a point already right before or at the offset: none.
    assert_eq!(held(0.5, 0, 7).hold_before(64, 0), None);
    assert_eq!(held(0.5, 64, 9).hold_before(64, 10), None);
    assert_eq!(held(0.5, 64, 10).hold_before(64, 10), None);
    // The processor's value is unknown: a hold would invent one.
    assert_eq!(UNKNOWN.hold_before(64, 10), None);
}
