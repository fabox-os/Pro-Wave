//! A browser preview plays live next to the running song, ends by itself, and never
//! allocates or frees on the audio thread.
use prowave_engine::{Command, EngineCore, Garbage, Preview, Shared};
use std::sync::Arc;

#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC_GUARD: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

#[test]
fn a_preview_plays_during_playback_then_is_returned_as_garbage() {
    let (mut tx, rx) = rtrb::RingBuffer::new(8);
    let (gtx, mut grx) = rtrb::RingBuffer::new(8);
    let mut core = EngineCore::new(48_000, rx, gtx, Arc::new(Shared::new()));
    assert!(tx.push(Command::SetMasterGain { linear: 1.0 }).is_ok());
    assert!(tx.push(Command::Play).is_ok());
    let mut buf = [0.0f32; 256];
    core.process(&mut buf);
    assert!(buf.iter().all(|s| *s == 0.0), "an empty song is silent");

    let preview = Preview::from_stereo(&vec![0.25f32; 2 * 100], 48_000, 48_000);
    assert!(tx
        .push(Command::SetPreview(Some(Box::new(preview))))
        .is_ok());
    assert_no_alloc::assert_no_alloc(|| core.process(&mut buf));
    assert!(
        buf[..200].iter().all(|s| (*s - 0.25).abs() < 1e-3),
        "the preview is heard while the transport plays"
    );
    assert!(buf[200..].iter().all(|s| *s == 0.0), "it ends by itself");
    let mut rest = [0.0f32; 256];
    assert_no_alloc::assert_no_alloc(|| core.process(&mut rest));
    assert!(
        matches!(grx.pop(), Ok(Garbage::Preview(_))),
        "the finished preview is freed off the audio thread"
    );
}
