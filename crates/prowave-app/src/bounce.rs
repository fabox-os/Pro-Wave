//! File > Bounce > Project or Section…: plays the song through the engine and writes what is
//! heard into a WAV file.
//!
//! The engine hands over its finished master frames (after every effect, send, automation lane
//! and the master fader, see `Command::SetCapture`), so the file is exactly the mix. A bounce
//! runs at playing speed: a four-minute song takes four minutes.

use prowave_engine::{write_wav, Command, Engine};
use rtrb::{Consumer, RingBuffer};
use std::path::PathBuf;

use crate::model::Project;

/// What the File > Bounce menu asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmd {
    /// Bounce the project, or the cycle section when the cycle is on.
    ProjectOrSection,
    /// Stop the bounce without writing anything.
    Cancel,
}

/// The engine's ring holds about 1.4 s at 48 kHz, far more than one frame's audio, so a slow
/// frame cannot lose the mix.
const RING_FRAMES: usize = 65_536;

/// A bounce in progress.
pub struct Bounce {
    received: Consumer<[f32; 2]>,
    /// The master frames gathered so far, stereo interleaved.
    samples: Vec<f32>,
    pub start_beat: f64,
    pub end_beat: f64,
    /// The playhead's place and the cycle before the bounce, put back when it ends.
    was_at: f64,
    was_cycle: bool,
    path: PathBuf,
    sample_rate: u32,
}

impl Bounce {
    /// Starts bouncing `start_beat` to `end_beat` into `path`, and plays from there.
    pub fn start(
        engine: &mut Engine,
        project: &mut Project,
        start_beat: f64,
        end_beat: f64,
        path: PathBuf,
    ) -> Result<Self, String> {
        let (producer, received) = RingBuffer::<[f32; 2]>::new(RING_FRAMES);
        if !engine.send(Command::SetCapture(Some(producer))) {
            return Err("the engine is busy; try the bounce again".to_string());
        }
        let bounce = Self {
            received,
            samples: Vec::new(),
            start_beat,
            end_beat,
            was_at: project.position_beats,
            was_cycle: project.cycle,
            path,
            sample_rate: engine.sample_rate(),
        };
        // A bounce plays once through, from its start.
        project.cycle = false;
        project.position_beats = start_beat;
        project.playing = true;
        Ok(bounce)
    }

    /// Takes the frames the engine has ready. `false` when the bounce should end.
    pub fn poll(&mut self, project: &Project) -> bool {
        self.collect();
        project.playing && project.position_beats < self.end_beat
    }

    /// The share of the section that has been played, 0 to 1.
    pub fn progress(&self, project: &Project) -> f32 {
        let span = self.end_beat - self.start_beat;
        if span <= 0.0 {
            return 1.0;
        }
        (((project.position_beats - self.start_beat) / span) as f32).clamp(0.0, 1.0)
    }

    /// Stops the bounce and puts the transport back. With `reached_end`, the frames gathered
    /// are written to the file; without it (the user stopped) nothing is written.
    pub fn finish(
        mut self,
        engine: &mut Engine,
        project: &mut Project,
        reached_end: bool,
    ) -> Result<Option<PathBuf>, String> {
        engine.send(Command::SetCapture(None));
        project.playing = false;
        project.cycle = self.was_cycle;
        project.position_beats = self.was_at;
        self.collect();
        if !reached_end {
            return Ok(None);
        }
        write_wav(&self.path, &self.samples, self.sample_rate)
            .map_err(|e| format!("{}: {e}", self.path.display()))?;
        Ok(Some(self.path))
    }

    fn collect(&mut self) {
        while let Ok(frame) = self.received.pop() {
            self.samples.push(frame[0]);
            self.samples.push(frame[1]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bounce with no engine behind it: its state maths is what the tests check.
    fn bounced(start_beat: f64, end_beat: f64) -> Bounce {
        let (producer, received) = RingBuffer::<[f32; 2]>::new(64);
        drop(producer);
        Bounce {
            received,
            samples: Vec::new(),
            start_beat,
            end_beat,
            was_at: 0.0,
            was_cycle: false,
            path: PathBuf::from("bounce.wav"),
            sample_rate: 48_000,
        }
    }

    #[test]
    fn a_bounce_runs_from_its_start_to_its_end_beat() {
        let mut running = bounced(4.0, 12.0);
        let mut project = Project::empty();
        project.playing = true;
        project.position_beats = 4.0;
        assert!(running.poll(&project), "just started");
        assert_eq!(running.progress(&project), 0.0);
        project.position_beats = 8.0;
        assert!(running.poll(&project));
        assert!((running.progress(&project) - 0.5).abs() < 1e-6);
        project.position_beats = 12.0;
        assert!(!running.poll(&project), "the end beat ends it");
        assert_eq!(running.progress(&project), 1.0);
    }

    #[test]
    fn stopping_the_transport_ends_a_bounce() {
        let mut running = bounced(0.0, 16.0);
        let mut project = Project::empty();
        project.position_beats = 3.0;
        project.playing = false;
        assert!(!running.poll(&project));
        assert!((running.progress(&project) - 3.0 / 16.0).abs() < 1e-6);
    }
}
