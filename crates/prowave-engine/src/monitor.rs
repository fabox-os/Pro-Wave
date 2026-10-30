//! Input monitoring: hearing an audio track's input through its channel strip (Logic: the I
//! button). The input device and the output device run on separate clocks and may run at
//! different rates, so the input is resampled here, and a backlog is dropped rather than heard
//! as a growing delay. Runs on the audio thread: no allocation, no lock.
use rtrb::Consumer;

/// Tracks that can monitor at the same time.
pub const MAX_MONITORS: usize = 8;
/// Channels of the input that can be monitored.
const CHANNELS: usize = 32;
/// More input than this waiting (seconds) is dropped...
const MAX_BACKLOG: f32 = 0.03;
/// ...down to this much.
const TARGET_BACKLOG: f32 = 0.01;

/// One track hearing its input. `channel` counts from 0; `stereo` takes the next one too.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MonitorTrack {
    pub track: u16,
    pub channel: u16,
    pub stereo: bool,
}

/// The live input as the audio thread receives it, interleaved.
pub struct InputFeed {
    ring: Consumer<f32>,
    channels: usize,
    rate: f32,
    step: f64,
    pos: f64,
    a: [f32; CHANNELS],
    b: [f32; CHANNELS],
}

impl InputFeed {
    /// `ring` delivers `channels` interleaved channels at `rate` frames per second.
    pub fn new(ring: Consumer<f32>, channels: usize, rate: u32) -> Self {
        Self {
            ring,
            channels: channels.max(1),
            rate: rate.max(1) as f32,
            step: 1.0,
            pos: 0.0,
            a: [0.0; CHANNELS],
            b: [0.0; CHANNELS],
        }
    }

    /// Sets the output's sample rate.
    pub(crate) fn set_output_rate(&mut self, rate: f32) {
        self.step = f64::from(self.rate) / f64::from(rate.max(1.0));
    }

    /// Once per block: drops what nobody is hearing, or the part that would only add delay.
    pub(crate) fn trim(&mut self, listening: bool) {
        let waiting = self.ring.slots() / self.channels;
        let excess = if !listening {
            waiting
        } else if waiting as f32 > self.rate * MAX_BACKLOG {
            waiting - (self.rate * TARGET_BACKLOG) as usize
        } else {
            0
        };
        if let Ok(chunk) = self.ring.read_chunk(excess * self.channels) {
            chunk.commit_all();
        }
    }

    /// Moves on by one output frame.
    pub(crate) fn advance(&mut self) {
        self.pos += self.step;
        while self.pos >= 1.0 {
            self.pos -= 1.0;
            self.a = self.b;
            if self.ring.slots() >= self.channels {
                for c in 0..self.channels {
                    let s = self.ring.pop().unwrap_or(0.0);
                    if c < CHANNELS {
                        self.b[c] = s;
                    }
                }
            } else {
                // The input fell behind: silence, not a stuck sample.
                self.b = [0.0; CHANNELS];
            }
        }
    }

    /// `channel` at this output frame.
    pub(crate) fn value(&self, channel: usize) -> f32 {
        let c = channel.min(CHANNELS - 1);
        let v = self.a[c] + (self.b[c] - self.a[c]) * self.pos as f32;
        if v.is_finite() {
            v
        } else {
            0.0
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use rtrb::RingBuffer;

    #[test]
    fn the_same_rate_passes_the_samples_through() {
        let (mut tx, rx) = RingBuffer::new(64);
        let mut feed = InputFeed::new(rx, 2, 48_000);
        feed.set_output_rate(48_000.0);
        for s in [0.1, 0.2, 0.3, 0.4] {
            tx.push(s).unwrap();
        }
        feed.advance();
        feed.advance();
        assert_eq!((feed.value(0), feed.value(1)), (0.1, 0.2));
        feed.advance();
        assert_eq!((feed.value(0), feed.value(1)), (0.3, 0.4));
    }

    #[test]
    fn a_slower_input_is_interpolated() {
        let (mut tx, rx) = RingBuffer::new(64);
        let mut feed = InputFeed::new(rx, 1, 24_000);
        feed.set_output_rate(48_000.0);
        for s in [0.0, 1.0, 1.0] {
            tx.push(s).unwrap();
        }
        for _ in 0..5 {
            feed.advance();
        }
        assert!((feed.value(0) - 0.5).abs() < 1e-6, "{}", feed.value(0));
    }

    #[test]
    fn running_out_of_input_goes_silent() {
        let (mut tx, rx) = RingBuffer::new(64);
        let mut feed = InputFeed::new(rx, 1, 48_000);
        feed.set_output_rate(48_000.0);
        tx.push(0.9).unwrap();
        for _ in 0..4 {
            feed.advance();
        }
        assert_eq!(feed.value(0), 0.0);
    }

    #[test]
    fn a_backlog_never_becomes_a_delay() {
        let (mut tx, rx) = RingBuffer::new(48_000);
        let mut feed = InputFeed::new(rx, 1, 48_000);
        for _ in 0..4_800 {
            tx.push(0.1).unwrap();
        }
        feed.trim(true);
        assert_eq!(feed.ring.slots(), 480, "100 ms is cut to 10 ms");
        feed.trim(false);
        assert_eq!(feed.ring.slots(), 0, "nobody listens");
    }

    #[test]
    fn more_channels_than_it_keeps_are_skipped_in_step() {
        let (mut tx, rx) = RingBuffer::new(256);
        let mut feed = InputFeed::new(rx, CHANNELS + 2, 48_000);
        feed.set_output_rate(48_000.0);
        for frame in 0..2 {
            for c in 0..CHANNELS + 2 {
                tx.push((frame * 100 + c) as f32).unwrap();
            }
        }
        feed.advance();
        feed.advance();
        feed.advance();
        assert_eq!(feed.value(1), 101.0);
    }
}
