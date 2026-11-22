//! Hosted effect plug-ins in the engine's Audio FX slot ([`BlockEffect`]).
//!
//! Logic Pro equivalent: a third-party plug-in in an Audio FX slot. The engine knows nothing
//! about plug-in formats: the app loads a LADSPA library (`prowave-ladspa`) and hands the engine
//! an object that implements [`BlockEffect`]. It is made, and dropped, off the audio thread (an
//! effect inside a [`crate::Chain`] leaves through the garbage queue with the chain), so a
//! plug-in's `instantiate` and `cleanup` never run on the audio thread. Only
//! [`BlockEffect::process`] and [`BlockEffect::set_param`] do.
//!
//! Plug-ins work on blocks, the engine on frames. [`PluginSlot`] gathers [`PLUGIN_BLOCK`]
//! frames, runs the plug-in once, and plays the result one block later. That is
//! [`PLUGIN_BLOCK`] samples of latency, which the mixer's delay compensation makes up for.

use std::fmt;

/// Frames a hosted effect processes at a time, and the latency it adds (2.7 ms at 48 kHz).
pub const PLUGIN_BLOCK: usize = 128;

/// An effect plug-in, as the engine sees it. Stereo in, stereo out.
pub trait BlockEffect: Send {
    /// Sets parameter `index` (the plug-in's control inputs, in port order) to `norm`, a
    /// position from 0 to 1 along its range. An index the plug-in does not have is ignored.
    /// Called on the audio thread: it must not allocate or block.
    fn set_param(&mut self, index: usize, norm: f32);

    /// Processes `in_l.len()` frames (always [`PLUGIN_BLOCK`]; the four slices have equal
    /// length). Called on the audio thread.
    fn process(&mut self, in_l: &[f32], in_r: &[f32], out_l: &mut [f32], out_r: &mut [f32]);

    /// The delay the plug-in itself reports, in samples, on top of [`PLUGIN_BLOCK`].
    fn latency_samples(&self) -> usize {
        0
    }

    /// Clears the plug-in's memory (tails, delay lines).
    fn reset(&mut self);

    /// Whether the effect's settings (the slot's parameter values) drive the plug-in. A plug-in
    /// that keeps its own settings and is moved only by automation says no.
    fn takes_settings(&self) -> bool {
        true
    }

    /// Another handle on the same plug-in, for a chain that is rebuilt while the plug-in plays
    /// (a plug-in that lives in its own process can be shared this way). Called off the audio
    /// thread; only one handle is ever played at a time. `None` (the default) means an
    /// instance cannot be shared: the copy of the slot passes the sound through.
    fn boxed_clone(&self) -> Option<Box<dyn BlockEffect>> {
        None
    }

    /// Offline rendering (see `EngineCore::set_offline`) is not in real time: a plug-in that runs
    /// in another process must answer every block instead of letting a late answer pass dry.
    /// A plug-in that runs on the engine's own thread needs nothing, so the default does nothing.
    fn set_offline(&mut self, _offline: bool) {}
}

/// The audio thread's side of a hosted effect: the plug-in, and the block it is fed with.
pub(crate) struct PluginSlot {
    plugin: Option<Box<dyn BlockEffect>>,
    in_l: [f32; PLUGIN_BLOCK],
    in_r: [f32; PLUGIN_BLOCK],
    out_l: [f32; PLUGIN_BLOCK],
    out_r: [f32; PLUGIN_BLOCK],
    pos: usize,
}

impl PluginSlot {
    /// A slot with no plug-in: it passes the sound through, [`PLUGIN_BLOCK`] samples late, so a
    /// plug-in that could not be loaded does not change the song's timing.
    pub(crate) fn empty() -> Self {
        Self {
            plugin: None,
            in_l: [0.0; PLUGIN_BLOCK],
            in_r: [0.0; PLUGIN_BLOCK],
            out_l: [0.0; PLUGIN_BLOCK],
            out_r: [0.0; PLUGIN_BLOCK],
            pos: 0,
        }
    }

    pub(crate) fn set_plugin(&mut self, plugin: Box<dyn BlockEffect>) {
        self.plugin = Some(plugin);
        self.clear();
    }

    pub(crate) fn has_plugin(&self) -> bool {
        self.plugin.is_some()
    }

    /// The plug-in's own latency, in samples (not counting the block).
    pub(crate) fn reported_latency(&self) -> usize {
        self.plugin.as_ref().map_or(0, |p| p.latency_samples())
    }

    /// The slot's settings, one per parameter, for a plug-in that is driven by them.
    pub(crate) fn apply_settings(&mut self, settings: &[f32]) {
        if let Some(p) = self.plugin.as_mut().filter(|p| p.takes_settings()) {
            for (i, v) in settings.iter().enumerate() {
                p.set_param(i, v.clamp(0.0, 1.0));
            }
        }
    }

    pub(crate) fn set_param(&mut self, index: usize, norm: f32) {
        if let Some(p) = self.plugin.as_mut() {
            p.set_param(index, norm.clamp(0.0, 1.0));
        }
    }

    /// Offline rendering on or off for the plug-in (see [`BlockEffect::set_offline`]).
    pub(crate) fn set_offline(&mut self, offline: bool) {
        if let Some(p) = self.plugin.as_mut() {
            p.set_offline(offline);
        }
    }

    /// One frame in, one frame out ([`PLUGIN_BLOCK`] frames late).
    #[inline]
    pub(crate) fn tick(&mut self, l: f32, r: f32) -> (f32, f32) {
        let out = (self.out_l[self.pos], self.out_r[self.pos]);
        self.in_l[self.pos] = l;
        self.in_r[self.pos] = r;
        self.pos += 1;
        if self.pos == PLUGIN_BLOCK {
            self.pos = 0;
            match self.plugin.as_mut() {
                Some(p) => p.process(&self.in_l, &self.in_r, &mut self.out_l, &mut self.out_r),
                None => {
                    self.out_l = self.in_l;
                    self.out_r = self.in_r;
                }
            }
        }
        out
    }

    fn clear(&mut self) {
        self.in_l.fill(0.0);
        self.in_r.fill(0.0);
        self.out_l.fill(0.0);
        self.out_r.fill(0.0);
        self.pos = 0;
    }

    pub(crate) fn reset(&mut self) {
        self.clear();
        if let Some(p) = self.plugin.as_mut() {
            p.reset();
        }
    }
}

impl Clone for PluginSlot {
    /// A copy shares the plug-in when it can ([`BlockEffect::boxed_clone`]); otherwise it has
    /// none and passes the sound through.
    fn clone(&self) -> Self {
        let mut copy = Self::empty();
        copy.plugin = self.plugin.as_ref().and_then(|p| p.boxed_clone());
        copy
    }
}

impl fmt::Debug for PluginSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PluginSlot")
            .field("loaded", &self.plugin.is_some())
            .field("pos", &self.pos)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Doubles the sound and reports a latency; remembers parameter 0.
    struct Doubler {
        param: f32,
    }

    impl BlockEffect for Doubler {
        fn set_param(&mut self, index: usize, norm: f32) {
            if index == 0 {
                self.param = norm;
            }
        }
        fn process(&mut self, in_l: &[f32], in_r: &[f32], out_l: &mut [f32], out_r: &mut [f32]) {
            for i in 0..in_l.len() {
                out_l[i] = in_l[i] * 2.0;
                out_r[i] = in_r[i] * 2.0 * self.param;
            }
        }
        fn latency_samples(&self) -> usize {
            7
        }
        fn reset(&mut self) {}
    }

    #[test]
    fn an_empty_slot_passes_the_sound_one_block_late() {
        let mut slot = PluginSlot::empty();
        let mut heard = Vec::new();
        for i in 0..PLUGIN_BLOCK * 2 {
            heard.push(slot.tick(i as f32, -(i as f32)).0);
        }
        assert!(heard[..PLUGIN_BLOCK].iter().all(|x| *x == 0.0));
        for i in 0..PLUGIN_BLOCK {
            assert_eq!(heard[PLUGIN_BLOCK + i], i as f32);
        }
        assert!(!slot.has_plugin());
    }

    #[test]
    fn a_plugin_processes_whole_blocks_and_hears_its_parameters() {
        let mut slot = PluginSlot::empty();
        slot.set_plugin(Box::new(Doubler { param: 0.0 }));
        slot.set_param(0, 0.5);
        assert_eq!(slot.reported_latency(), 7);
        let mut last = (0.0, 0.0);
        for _ in 0..PLUGIN_BLOCK * 2 {
            last = slot.tick(0.25, 0.25);
        }
        assert_eq!(last, (0.5, 0.25));
        // A position outside 0..1 is put into range.
        slot.set_param(0, 7.0);
        for _ in 0..PLUGIN_BLOCK * 2 {
            last = slot.tick(0.25, 0.25);
        }
        assert_eq!(last, (0.5, 0.5));
    }

    #[test]
    fn a_copy_of_a_slot_has_no_plugin() {
        let mut slot = PluginSlot::empty();
        slot.set_plugin(Box::new(Doubler { param: 1.0 }));
        assert!(!slot.clone().has_plugin());
    }

    #[test]
    fn the_slot_does_not_allocate_while_it_runs() {
        let mut slot = PluginSlot::empty();
        slot.set_plugin(Box::new(Doubler { param: 1.0 }));
        assert_no_alloc::assert_no_alloc(|| {
            for i in 0..PLUGIN_BLOCK * 4 {
                slot.set_param(0, 0.5);
                slot.tick(i as f32 * 0.001, 0.0);
            }
            slot.reset();
        });
    }
}
