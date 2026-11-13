//! MIDI the unit sends through its `MIDIOutputEventBlock` while it renders, split into messages
//! within the block's budgets, while the unit's bytes are still valid.
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use block2::RcBlock;
use objc2_audio_toolbox::{AUAudioUnit, AUEventSampleTime};
use objc2_foundation::NSInteger;
use plughost_core::{Event, EventData, Events, MAX_BLOCK_EVENTS, MAX_BLOCK_SYSEX_BYTES};

use super::super::errors::AuError;

type OutputEvent = dyn Fn(AUEventSampleTime, u8, NSInteger, NonNull<u8>) -> i32;

struct Collected {
    /// Render timestamp and length of the block being rendered.
    start: AUEventSampleTime,
    frames: usize,
    /// The cables prepared for delivery; events on other cables are dropped.
    cables: Vec<u64>,
    /// Per cable, the system exclusive message an earlier call started and has not ended.
    partial: Vec<Partial>,
    /// One block's room, reserved: collecting allocates nothing for the events.
    events: Events,
    sysex: usize,
    /// The unit sent more events or system exclusive bytes than the block allows.
    overflow: bool,
}

/// A system exclusive message a call started and a later one may end, in room reserved for one
/// block's system exclusive bytes so that joining it allocates nothing.
#[derive(Default)]
struct Partial {
    open: bool,
    bytes: Vec<u8>,
}

impl Partial {
    fn reserved() -> Result<Partial, AuError> {
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(MAX_BLOCK_SYSEX_BYTES)
            .map_err(|_| AuError::ProcessingStorage)?;
        Ok(Partial { open: false, bytes })
    }

    fn start(&mut self, status: u8) {
        self.open = true;
        self.bytes.clear();
        self.bytes.push(status);
    }

    fn close(&mut self) {
        self.open = false;
        self.bytes.clear();
    }
}

impl Collected {
    fn push(&mut self, event: Event<'_>) {
        let sysex = match event.data {
            EventData::SysEx(bytes) => bytes.len(),
            _ => 0,
        };
        if self.overflow
            || self.events.len() == MAX_BLOCK_EVENTS
            || self.sysex.saturating_add(sysex) > MAX_BLOCK_SYSEX_BYTES
        {
            self.overflow = true;
            return;
        }
        if self.events.push(event).is_err() {
            self.overflow = true;
            return;
        }
        self.sysex += sysex;
    }
}

pub(super) struct Output {
    collected: Arc<Mutex<Collected>>,
    _block: RcBlock<OutputEvent>,
}

fn lock(collected: &Mutex<Collected>) -> std::sync::MutexGuard<'_, Collected> {
    collected
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Output {
    /// Installs the collector as the unit's MIDI output block, before render resources are
    /// allocated. Messages with no MIDI 1.0 event form are counted in `unconvertible`.
    pub fn install(
        unit: &AUAudioUnit,
        cables: Vec<u64>,
        unconvertible: Arc<AtomicU64>,
    ) -> Result<Output, AuError> {
        let events = Events::try_for_block().map_err(|_| AuError::ProcessingStorage)?;
        let partial = cables
            .iter()
            .map(|_| Partial::reserved())
            .collect::<Result<_, _>>()?;
        let collected = Arc::new(Mutex::new(Collected {
            start: 0,
            frames: 0,
            partial,
            cables,
            events,
            sysex: 0,
            overflow: false,
        }));
        let sink = Arc::clone(&collected);
        let block = RcBlock::new(
            move |time: AUEventSampleTime, cable: u8, length: NSInteger, bytes: NonNull<u8>| {
                let _audit = plughost_core::audit::callback();
                let Ok(length) = usize::try_from(length) else {
                    return 0;
                };
                // SAFETY: the unit passes `length` readable bytes, valid during this call.
                let bytes = unsafe { std::slice::from_raw_parts(bytes.as_ptr(), length) };
                let mut collected = lock(&sink);
                let Some(index) = collected
                    .cables
                    .iter()
                    .position(|&prepared| prepared == u64::from(cable))
                else {
                    return 0;
                };
                // Events outside the block, and immediate ones, land on its nearest frame.
                let last = collected.frames.saturating_sub(1);
                let offset = usize::try_from(time.saturating_sub(collected.start))
                    .unwrap_or(0)
                    .min(last);
                let mut partial = std::mem::take(&mut collected.partial[index]);
                let result = split(bytes, &mut partial, |data| {
                    collected.push(Event::new(offset, data).on_port(usize::from(cable)))
                });
                collected.partial[index] = partial;
                match result {
                    Ok(skipped) if skipped > 0 => {
                        unconvertible.fetch_add(skipped, Ordering::Relaxed);
                    }
                    Ok(_) => {}
                    Err(_) => collected.overflow = true,
                }
                0
            },
        );
        unsafe { unit.setMIDIOutputEventBlock(RcBlock::as_ptr(&block)) };
        Ok(Output {
            collected,
            _block: block,
        })
    }

    /// Starts collecting the block rendered at `start` for `frames` samples.
    pub fn begin(&self, start: f64, frames: usize) {
        let mut collected = lock(&self.collected);
        collected.start = start as AUEventSampleTime;
        collected.frames = frames;
        collected.events.clear();
        collected.sysex = 0;
        collected.overflow = false;
    }

    /// Forgets the system exclusive messages the unit started before it was reset.
    pub fn reset(&self) {
        lock(&self.collected)
            .partial
            .iter_mut()
            .for_each(Partial::close);
    }

    /// Moves the block's events into `into` in offset order, keeping the unit's order at equal
    /// offsets; fails when the unit exceeded the block's budgets, whose events are discarded.
    pub fn take(&self, into: &mut Events) -> Result<(), AuError> {
        let mut collected = lock(&self.collected);
        if collected.overflow {
            collected.events.clear();
            return Err(AuError::OutputEventCapacity);
        }
        for event in &collected.events {
            // The events fit one block's budget, which `into` has room for.
            into.push(event).map_err(|_| AuError::OutputEventCapacity)?;
        }
        collected.events.clear();
        into.sort_by_offset();
        Ok(())
    }
}

/// Splits a MIDI byte stream into channel and complete system exclusive messages, following
/// running status. A system exclusive message the bytes do not end stays open in `sysex` and goes
/// on with the next call's bytes; real-time messages inside it are skipped. A message that cannot
/// fit the block's system exclusive budget, including its terminator, is discarded with an
/// overflow error. Returns how many messages had no such form (system common and real-time
/// messages, or malformed bytes, which end the stream).
fn split(
    mut bytes: &[u8],
    sysex: &mut Partial,
    mut each: impl FnMut(EventData<'_>),
) -> Result<u64, AuError> {
    let mut unconvertible = 0;
    let mut running = None;
    while let Some(&first) = bytes.first() {
        if sysex.open {
            let message = &mut sysex.bytes;
            let data = bytes
                .iter()
                .position(|&byte| byte >= 0x80)
                .unwrap_or(bytes.len());
            // Leave room for F7 before copying data, even when a clock follows this chunk.
            if message.len().saturating_add(data) >= MAX_BLOCK_SYSEX_BYTES {
                sysex.close();
                return Err(AuError::OutputEventCapacity);
            }
            message.extend_from_slice(&bytes[..data]);
            bytes = &bytes[data..];
            match bytes.first() {
                Some(0xF7) => {
                    message.push(0xF7);
                    bytes = &bytes[1..];
                }
                Some(0xF8..) => {
                    unconvertible += 1;
                    bytes = &bytes[1..];
                    continue;
                }
                Some(_) => {
                    sysex.close();
                    return Ok(unconvertible + 1);
                }
                None => return Ok(unconvertible),
            }
            each(EventData::SysEx(&sysex.bytes));
            sysex.close();
            continue;
        }
        let (status, rest) = match (first, running) {
            (0x80.., _) => (first, &bytes[1..]),
            (_, Some(status)) => (status, bytes),
            _ => return Ok(unconvertible + 1),
        };
        match status {
            0xF0 => {
                sysex.start(status);
                running = None;
                bytes = rest;
            }
            0x80..=0xEF => {
                let length = if matches!(status & 0xF0, 0xC0 | 0xD0) {
                    1
                } else {
                    2
                };
                let Some(data) = rest
                    .get(..length)
                    .filter(|data| data.iter().all(|&b| b < 0x80))
                else {
                    return Ok(unconvertible + 1);
                };
                let mut message = [status, 0, 0];
                message[1..=length].copy_from_slice(data);
                each(EventData::Midi(message));
                running = Some(status);
                bytes = &rest[length..];
            }
            _ => {
                unconvertible += 1;
                let length = match status {
                    0xF1 | 0xF3 => 1,
                    0xF2 => 2,
                    _ => 0,
                };
                // System common messages cancel running status; real-time messages do not.
                if status < 0xF8 {
                    running = None;
                }
                bytes = rest.get(length..).unwrap_or_default();
            }
        }
    }
    Ok(unconvertible)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn messages(bytes: &[u8]) -> (Events, u64) {
        let mut found = Events::new();
        let skipped = split(bytes, &mut Partial::default(), |data| {
            found.push(Event::new(0, data)).unwrap()
        })
        .unwrap();
        (found, skipped)
    }

    #[test]
    fn a_byte_stream_splits_into_channel_and_system_exclusive_messages() {
        let (found, skipped) = messages(&[
            0x90, 60, 100, 62, 90,   // note on, then another under running status
            0xF8, // clock, between messages
            0xC1, 5, // program change
            0xF0, 1, 2, 3, 0xF7, // system exclusive
            0xB0, 7, 99,
        ]);
        assert_eq!(
            found,
            [
                Event::new(0, EventData::Midi([0x90, 60, 100])),
                Event::new(0, EventData::Midi([0x90, 62, 90])),
                Event::new(0, EventData::Midi([0xC1, 5, 0])),
                Event::sysex(0, &[0xF0, 1, 2, 3, 0xF7]),
                Event::new(0, EventData::Midi([0xB0, 7, 99])),
            ]
        );
        assert_eq!(skipped, 1);
    }

    #[test]
    fn malformed_bytes_end_the_stream_as_unconvertible() {
        for bytes in [
            &[0x90, 60][..],           // truncated channel message
            &[60, 100][..],            // data without status
            &[0xF0, 1, 0x90, 60][..],  // system exclusive cut by a status byte
            &[0x90, 60, 0x80, 60][..], // status byte inside the data
        ] {
            let (found, skipped) = messages(bytes);
            assert!(found.is_empty(), "{bytes:?}");
            assert_eq!(skipped, 1, "{bytes:?}");
        }
        let (found, skipped) = messages(&[0xB0, 1, 2, 0xF2]);
        assert_eq!(found, [Event::new(0, EventData::Midi([0xB0, 1, 2]))]);
        assert_eq!(skipped, 1);
    }

    #[test]
    fn real_time_messages_inside_system_exclusive_are_skipped() {
        let (found, skipped) = messages(&[0xF0, 1, 0xF8, 2, 0xFE, 3, 0xF7, 0x80, 60, 0]);
        assert_eq!(
            found,
            [
                Event::sysex(0, &[0xF0, 1, 2, 3, 0xF7]),
                Event::new(0, EventData::Midi([0x80, 60, 0])),
            ]
        );
        assert_eq!(skipped, 2);
    }

    #[test]
    fn system_exclusive_split_over_calls_is_joined() {
        let mut sysex = Partial::default();
        let mut found = Events::new();
        for bytes in [
            &[0x90, 60, 1, 0xF0, 1][..],
            &[2, 0xF8][..],
            &[3, 0xF7, 0xC0, 4][..],
        ] {
            split(bytes, &mut sysex, |data| {
                found.push(Event::new(0, data)).unwrap()
            })
            .unwrap();
        }
        assert_eq!(
            found,
            [
                Event::new(0, EventData::Midi([0x90, 60, 1])),
                Event::sysex(0, &[0xF0, 1, 2, 3, 0xF7]),
                Event::new(0, EventData::Midi([0xC0, 4, 0])),
            ]
        );
        assert!(!sysex.open);
    }

    #[test]
    fn system_exclusive_beyond_the_block_budget_is_rejected_without_emitting() {
        let mut sysex = Partial::default();
        let mut found = Events::new();
        let start = [0xF0, 1];
        let rest = vec![2; MAX_BLOCK_SYSEX_BYTES];
        split(&start, &mut sysex, |data| {
            found.push(Event::new(0, data)).unwrap()
        })
        .unwrap();
        assert!(matches!(
            split(&rest, &mut sysex, |data| found
                .push(Event::new(0, data))
                .unwrap()),
            Err(AuError::OutputEventCapacity)
        ));
        assert!(!sysex.open);
        assert!(found.is_empty());
    }
}
