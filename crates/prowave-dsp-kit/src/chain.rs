//! A chain of effects, built from a command line like `gain -6 :0 lowpass 1k 0.7q`
//! (`effects_chain.c` of the original).

use crate::effect::{get_effect_info, Buf, Effect, Stream};
use crate::error::{Error, Result};
use crate::selector::parse_selector;

/// The original's default block size.
pub const DEFAULT_BLOCK_FRAMES: usize = 2048;

/// Effects run one after another over blocks of frames.
pub struct Chain {
    istream: Stream,
    ostream: Stream,
    effects: Vec<Box<dyn Effect>>,
    block_frames: usize,
    buf1: Vec<f64>,
    buf2: Vec<f64>,
    built: bool,
    /// Frames of silence that follow the input to flush the effects (`drain_frames`), and the
    /// number of input frames seen so far.
    drain_frames: usize,
    iframes: usize,
}

impl Chain {
    /// An empty chain for `istream`.
    pub fn new(istream: Stream) -> Self {
        Self {
            istream,
            ostream: istream,
            effects: Vec::new(),
            block_frames: DEFAULT_BLOCK_FRAMES,
            buf1: Vec::new(),
            buf2: Vec::new(),
            built: false,
            drain_frames: 0,
            iframes: 0,
        }
    }

    /// Sets how many frames go through the chain at a time (the original's `-b`).
    pub fn set_block_frames(&mut self, frames: usize) {
        self.block_frames = frames.max(1);
    }

    /// The stream that comes out of the chain.
    pub fn ostream(&self) -> Stream {
        self.ostream
    }

    /// Adds the effect `name` with its arguments, working on the channels of `selector`
    /// (`None`: all of them).
    pub fn push(&mut self, name: &str, args: &[&str], selector: Option<&[bool]>) -> Result<()> {
        let info = get_effect_info(name).ok_or_else(|| Error::UnknownEffect(name.into()))?;
        let mut argv = vec![name];
        argv.extend_from_slice(args);
        let all = vec![true; self.ostream.channels];
        let made = (info.init)(info, &self.ostream, selector.unwrap_or(&all), &argv)?;
        for e in made {
            self.ostream = e.ostream();
            self.effects.push(e);
        }
        self.built = false;
        Ok(())
    }

    /// Finishes the chain (`build_effects_chain_finish`): lets each effect prepare, puts in the
    /// `align` effects that keep channels together, and works out how many frames of silence
    /// flush the effects. Called by [`Chain::from_args`]; call it after the last
    /// [`Chain::push`] to hear about errors, otherwise the first [`Chain::run`] does it.
    pub fn build(&mut self) -> Result<()> {
        if self.built || self.effects.is_empty() {
            self.built = true;
            return Ok(());
        }
        for e in self.effects.iter_mut() {
            e.prepare()?;
        }
        crate::align::align_channels(&mut self.effects)?;
        self.drain_frames = crate::align::drain_frames(&self.effects, self.istream, self.ostream);
        self.built = true;
        Ok(())
    }

    /// Builds a chain from command-line words: effect names followed by their arguments, and
    /// `:selector` words that choose the channels of the effects after them. `{` and `}`
    /// blocks are not supported yet.
    pub fn from_args(istream: Stream, argv: &[&str]) -> Result<Self> {
        let mut chain = Chain::new(istream);
        let mut selector_text = String::new();
        let mut selector = vec![true; istream.channels];
        let mut i = 0;
        while i < argv.len() {
            let w = argv[i];
            if let Some(s) = w.strip_prefix(':') {
                selector_text = s.to_string();
                selector = parse_selector(s, chain.ostream.channels)?;
                i += 1;
            } else if w == "{" || w == "}" {
                return Err(Error::Unsupported("effect blocks ({ })".into()));
            } else if get_effect_info(w).is_some() {
                let mut j = i + 1;
                while j < argv.len()
                    && get_effect_info(argv[j]).is_none()
                    && !argv[j].starts_with(':')
                    && argv[j] != "{"
                    && argv[j] != "}"
                {
                    j += 1;
                }
                let before = chain.ostream.channels;
                chain.push(w, &argv[i + 1..j], Some(&selector))?;
                // If the effect changed the number of channels the last selector is parsed
                // again for the new count.
                if chain.ostream.channels != before {
                    selector = parse_selector(&selector_text, chain.ostream.channels)?;
                }
                i = j;
            } else {
                return Err(Error::UnknownEffect(w.into()));
            }
        }
        chain.build()?;
        Ok(chain)
    }

    /// Runs the interleaved signal `input` through the chain, `block_frames` at a time, and
    /// returns what comes out so far. Effects with latency hold back some frames; call
    /// [`Chain::drain`] after the last input to get them, or use [`Chain::process`].
    pub fn run(&mut self, input: &[f64]) -> Vec<f64> {
        if self.build().is_err() {
            return Vec::new();
        }
        let ich = self.istream.channels.max(1);
        let total = input.len() / ich;
        self.iframes += total;
        let mut out = Vec::with_capacity(total * self.ostream.channels);
        let mut start = 0;
        while start < total {
            let n = self.block_frames.min(total - start);
            self.run_block(&input[start * ich..(start + n) * ich], &mut out);
            start += n;
        }
        out
    }

    /// Runs one block through every effect and appends the result to `out`.
    fn run_block(&mut self, block: &[f64], out: &mut Vec<f64>) {
        let ich = self.istream.channels.max(1);
        let n = block.len() / ich;
        let max_ch = self
            .effects
            .iter()
            .map(|e| e.ostream().channels)
            .chain([ich])
            .max()
            .unwrap_or(ich);
        let mut cap = n;
        for e in &self.effects {
            cap = cap.max(e.buffer_frames(cap));
        }
        let len = cap * max_ch;
        if self.buf1.len() < len {
            self.buf1.resize(len, 0.0);
        }
        if self.buf2.len() < len {
            self.buf2.resize(len, 0.0);
        }
        self.buf1[..n * ich].copy_from_slice(block);
        let mut frames = n;
        let mut in_first = true;
        for e in self.effects.iter_mut() {
            let (a, b) = if in_first {
                (&mut self.buf1, &mut self.buf2)
            } else {
                (&mut self.buf2, &mut self.buf1)
            };
            if e.run(&mut frames, a, b) == Buf::Output {
                in_first = !in_first;
            }
        }
        let result = if in_first { &self.buf1 } else { &self.buf2 };
        out.extend_from_slice(&result[..frames * self.ostream.channels]);
    }

    /// Flushes the chain after the last input (`drain_effects_chain`): feeds the silence that
    /// the effects need to pass on what they still hold, and returns what comes out.
    pub fn drain(&mut self) -> Vec<f64> {
        let mut out = Vec::new();
        if self.iframes < 1 || self.build().is_err() {
            return out;
        }
        let ich = self.istream.channels.max(1);
        while self.drain_frames > 0 {
            let n = self.block_frames.min(self.drain_frames);
            self.drain_frames -= n;
            let zeros = vec![0.0; n * ich];
            self.run_block(&zeros, &mut out);
        }
        out
    }

    /// Runs `input` through the chain and flushes it: what `dsp` writes for a whole file.
    pub fn process(&mut self, input: &[f64]) -> Vec<f64> {
        let mut out = self.run(input);
        out.extend(self.drain());
        out
    }

    /// Whether any effect changes the signal in a way that dither at the output would help
    /// (`effects_chain_needs_dither`).
    pub fn needs_dither(&self) -> bool {
        self.effects
            .iter()
            .any(|e| !e.no_dither() && !e.is_dither())
    }

    /// Tells the dither effects the precision of the output and whether dithering is wanted
    /// (`effects_chain_set_dither_params`). Returns whether the caller should add dither of its
    /// own at the output, which is the case when there is no dither effect after the last
    /// effect that needs it.
    pub fn set_dither_params(&mut self, bits: i32, enabled: bool) -> bool {
        let mut r = true;
        for e in self.effects.iter_mut() {
            if e.set_dither_params(bits, enabled) {
                r = false;
            } else if !e.no_dither() {
                r = true;
            }
        }
        r && enabled
    }

    /// Forgets the signal in every effect.
    pub fn reset(&mut self) {
        for e in self.effects.iter_mut() {
            e.reset();
        }
    }
}
