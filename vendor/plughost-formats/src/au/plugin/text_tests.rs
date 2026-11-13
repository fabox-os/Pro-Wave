use std::ptr::NonNull;

use objc2::AllocAnyThread;
use objc2_foundation::NSString;

#[test]
fn text_a_unit_gives_with_an_unpaired_surrogate_keeps_the_rest() {
    let units = [
        u16::from(b'G'),
        0xD800,
        u16::from(b'a'),
        u16::from(b'i'),
        u16::from(b'n'),
    ];
    // SAFETY: the pointer and length describe `units`, which the string copies.
    let string = unsafe {
        NSString::initWithCharacters_length(
            NSString::alloc(),
            NonNull::from(&units).cast(),
            units.len(),
        )
    };
    assert_eq!(super::text(&string), "G\u{FFFD}ain");
}
