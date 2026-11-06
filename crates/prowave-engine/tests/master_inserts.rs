//! The effects of the master bus (Stereo Out) process the whole mix, can be changed while it
//! plays, and neither allocate nor free on the audio thread.
use prowave_engine::{Chain, Command, EngineCore, Fx, FxKind, Garbage, Shared, MASTER_TRACK};
use std::sync::Arc;

#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC_GUARD: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

const SR: u32 = 48_000;

fn rms(core: &mut EngineCore, frames: usize) -> f32 {
    let mut buf = vec![0.0f32; frames * 2];
    assert_no_alloc::assert_no_alloc(|| core.process(&mut buf));
    (buf.iter().map(|s| s * s).sum::<f32>() / buf.len() as f32).sqrt()
}

#[test]
fn the_master_chain_filters_the_mix_and_follows_settings() {
    let (mut tx, rx) = rtrb::RingBuffer::new(16);
    let (gtx, mut grx) = rtrb::RingBuffer::new(8);
    let mut core = EngineCore::new(SR, rx, gtx, Arc::new(Shared::new()));
    assert!(tx.push(Command::SetMasterGain { linear: 1.0 }).is_ok());
    // A 440 Hz tone, played by the test oscillator, goes straight to the master bus.
    assert!(tx
        .push(Command::SetTestTone {
            hz: 440.0,
            gain: 0.5
        })
        .is_ok());
    let open = rms(&mut core, 4800);
    assert!(open > 0.3, "the tone is there: {open}");

    // A low pass at 100 Hz on the master takes most of it away.
    let mut params = FxKind::Filter.defaults();
    params[0] = 100.0;
    let chain = Chain::new(vec![Fx::new(FxKind::Filter, params, false, SR as f32)]);
    assert!(tx.push(Command::SetMasterInserts(Box::new(chain))).is_ok());
    rms(&mut core, 4800);
    let filtered = rms(&mut core, 4800);
    assert!(filtered < open * 0.2, "{filtered} vs {open}");

    // Opening the cut-off while it plays (SetFx on the master) lets the tone back.
    params[0] = 12_000.0;
    assert!(tx
        .push(Command::SetFx {
            track: MASTER_TRACK,
            slot: 0,
            params,
            bypass: false,
        })
        .is_ok());
    rms(&mut core, 4800);
    let again = rms(&mut core, 4800);
    assert!(again > open * 0.8, "{again} vs {open}");

    // Bypassed, it passes the mix as it is.
    params[0] = 100.0;
    assert!(tx
        .push(Command::SetFx {
            track: MASTER_TRACK,
            slot: 0,
            params,
            bypass: true,
        })
        .is_ok());
    rms(&mut core, 4800);
    assert!(rms(&mut core, 4800) > open * 0.8);

    // Replacing the chain hands the old one back to be freed off the audio thread.
    assert!(tx.push(Command::SetMasterInserts(Box::default())).is_ok());
    rms(&mut core, 256);
    assert!(matches!(grx.pop(), Ok(Garbage::Chain(_))));
}
