//! MIDI Clock (Logic Pro: Project Settings > Synchronization > MIDI Clock) for external devices:
//! 24 Timing Clock messages per quarter note while the song plays, Start or Continue when it
//! starts, Stop when it stops, and a Song Position Pointer to say where it is.
//!
//! The engine makes the messages on the audio clock ([`ClockMessage`]); the app sends them to the
//! project's clock port.

/// Timing Clock messages per quarter note.
pub const TICKS_PER_BEAT: f64 = 24.0;

/// A message of the MIDI clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClockMessage {
    /// Timing Clock (0xF8): one of the 24 per quarter note.
    Tick,
    /// Start (0xFA): play from the beginning.
    Start,
    /// Continue (0xFB): play on from the current position.
    Continue,
    /// Stop (0xFC).
    Stop,
    /// Song Position Pointer (0xF2): the position in sixteenth notes, 14 bits.
    SongPosition(u16),
}

impl ClockMessage {
    /// The bytes of the message, and how many of the three are used.
    pub fn bytes(self) -> ([u8; 3], usize) {
        match self {
            ClockMessage::Tick => ([0xf8, 0, 0], 1),
            ClockMessage::Start => ([0xfa, 0, 0], 1),
            ClockMessage::Continue => ([0xfb, 0, 0], 1),
            ClockMessage::Stop => ([0xfc, 0, 0], 1),
            ClockMessage::SongPosition(p) => {
                let p = p.min(16383);
                ([0xf2, (p & 0x7f) as u8, ((p >> 7) & 0x7f) as u8], 3)
            }
        }
    }
}

/// The Song Position for the playhead at `beats` quarter notes: the sixteenth notes since the
/// start, never more than 14 bits can hold.
pub fn song_position(beats: f64) -> u16 {
    if beats.is_finite() {
        (beats.max(0.0) * 4.0).floor().min(16383.0) as u16
    } else {
        0
    }
}

/// The Timing Clock index of the playhead: how many ticks have passed since the start.
pub fn tick_index(beats: f64) -> i64 {
    if beats.is_finite() {
        (beats.max(0.0) * TICKS_PER_BEAT).floor() as i64
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_are_the_standard_real_time_bytes() {
        assert_eq!(ClockMessage::Tick.bytes(), ([0xf8, 0, 0], 1));
        assert_eq!(ClockMessage::Start.bytes(), ([0xfa, 0, 0], 1));
        assert_eq!(ClockMessage::Continue.bytes(), ([0xfb, 0, 0], 1));
        assert_eq!(ClockMessage::Stop.bytes(), ([0xfc, 0, 0], 1));
    }

    #[test]
    fn song_position_is_sixteenths_in_two_seven_bit_bytes() {
        // Bar 2 of 4/4 is beat 4: 16 sixteenths.
        assert_eq!(song_position(4.0), 16);
        assert_eq!(ClockMessage::SongPosition(16).bytes(), ([0xf2, 16, 0], 3));
        // 200 sixteenths: LSB 200 & 0x7f = 72, MSB 1.
        assert_eq!(ClockMessage::SongPosition(200).bytes(), ([0xf2, 72, 1], 3));
        assert_eq!(song_position(-3.0), 0);
        assert_eq!(song_position(f64::NAN), 0);
        assert_eq!(song_position(1.0e9), 16383);
    }

    #[test]
    fn there_are_24_ticks_per_quarter_note() {
        assert_eq!(tick_index(0.0), 0);
        assert_eq!(tick_index(1.0), 24);
        assert_eq!(tick_index(0.5), 12);
        assert_eq!(tick_index(f64::INFINITY), 0);
    }
}
