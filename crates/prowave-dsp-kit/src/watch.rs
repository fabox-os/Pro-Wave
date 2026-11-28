//! `watch` (`watch.c`): a sub-chain loaded from an effects file that is loaded again when the
//! file changes, with a 100 ms crossfade from the old effects to the new ones.
//!
//! The original polls the file's modification time from a thread of its own every second.
//! The crate reads no files and starts no threads, so here the host does that part: the
//! chain gives a [`WatchHandle`] for every `watch` effect ([`crate::Chain::watches`]), and
//! the host calls [`WatchHandle::reload`] (off the audio thread) when the file has changed.
//! The new sub-chain is built and checked there, as the original's worker does, and the
//! audio thread takes it at the start of its next block without waiting for a lock.
//!
//! As in the original, a reload is refused when the new effects have another output sample
//! rate or number of channels, or need longer buffers; dither with `auto` bits is switched
//! off in reloaded effects.
//!
//! Logic Pro equivalent: none (Logic reloads plug-in settings, not effect chains).

use std::sync::{Arc, Mutex};

use crate::chain::{split_words, Chain, EffectsFiles};
use crate::effect::{Buf, Effect, Stream};
use crate::error::{Error, Result};

/// `EFFECTS_CHAIN_XFADE_TIME` in ms.
const XFADE_TIME: f64 = 100.0;
/// `EFFECTS_FILE_EOF_MARKER`
const EOF_MARKER: &str = "#EOF#";

/// What the host and the audio thread share.
struct Slot {
    /// A checked chain waiting for the audio thread.
    pending: Option<Chain>,
    /// Chains the audio thread is done with, for the host to drop (no freeing on the audio
    /// thread).
    retired: Vec<Chain>,
    /// The longest block seen so far, and the output the current chain gives for it.
    in_frames: usize,
    out_frames: usize,
}

struct Shared {
    path: String,
    istream: Stream,
    ostream: Stream,
    mask: Vec<bool>,
    enforce_eof_marker: bool,
    slot: Mutex<Slot>,
}

/// The host's side of a `watch` effect.
#[derive(Clone)]
pub struct WatchHandle {
    shared: Arc<Shared>,
}

impl WatchHandle {
    /// The full path of the watched file.
    pub fn path(&self) -> &str {
        &self.shared.path
    }

    /// `watch_reload`: reads the file again through `files` and hands the new effects to
    /// the audio thread. On an error the effects that play now stay.
    pub fn reload(&self, files: &mut EffectsFiles) -> Result<()> {
        let sh = &self.shared;
        let text = read_effects_file(&sh.path, files, sh.enforce_eof_marker)?;
        let mut chain = build_sub_chain(sh.istream, &sh.mask, &sh.path, &text, files)?;
        let os = chain.ostream();
        if os.fs != sh.ostream.fs {
            return Err(Error::Message(format!(
                "watch: error: sample rate mismatch: {}",
                sh.path
            )));
        }
        if os.channels != sh.ostream.channels {
            return Err(Error::Message(format!(
                "watch: error: channels mismatch: {}",
                sh.path
            )));
        }
        // switch off auto dither in reloaded effects, as the original does
        chain.set_dither_params(0, false);
        let mut slot = sh
            .slot
            .lock()
            .map_err(|_| Error::Message("watch: lock".into()))?;
        if chain.max_out_frames(slot.in_frames) > slot.out_frames {
            return Err(Error::Message(format!(
                "watch: error: buffer length: {}",
                sh.path
            )));
        }
        chain.prepare_buffers(slot.in_frames);
        slot.pending = Some(chain);
        slot.retired.clear();
        Ok(())
    }

    /// Drops the effects the audio thread no longer uses.
    pub fn collect(&self) {
        if let Ok(mut slot) = self.shared.slot.lock() {
            slot.retired.clear();
        }
    }
}

/// Reads an effects file, checking the end-of-file marker when asked (`-e`).
fn read_effects_file(
    path: &str,
    files: &mut EffectsFiles,
    enforce_eof_marker: bool,
) -> Result<String> {
    let text = (files.read)(path)
        .map_err(|e| Error::Message(format!("error: failed to load effects file: {path}: {e}")))?;
    if enforce_eof_marker {
        let t = text.trim_end();
        let l = EOF_MARKER.len();
        let ok =
            t.ends_with(EOF_MARKER) && (t.len() == l || t.as_bytes()[t.len() - l - 1] == b'\n');
        if !ok {
            return Err(Error::Message(format!(
                "error: no valid end-of-file marker: {path}"
            )));
        }
    }
    Ok(text)
}

/// `build_effects_chain_from_file`: the effects of `text` over the whole stream, with `mask`
/// as the channels they see; paths in it are relative to the file's directory.
fn build_sub_chain(
    istream: Stream,
    mask: &[bool],
    path: &str,
    text: &str,
    files: &mut EffectsFiles,
) -> Result<Chain> {
    let words = split_words(text)?;
    let argv: Vec<&str> = words.iter().map(String::as_str).collect();
    let dir = path
        .rfind('/')
        .map_or_else(|| ".".to_string(), |b| path[..b].to_string());
    Chain::from_words_masked(istream, &argv, Some(files), Some(dir), Some(mask))
}

/// `watch_effect_init`, called by the chain's parser with the path already resolved.
pub(crate) fn make(
    istream: Stream,
    selector: &[bool],
    argv: &[&str],
    files: &mut EffectsFiles,
    resolve: impl Fn(&str) -> String,
) -> Result<(Box<dyn Effect>, WatchHandle)> {
    let name = argv[0];
    let argc = argv.len();
    let mut g = crate::args::Getopt::default();
    let mut enforce_eof_marker = false;
    while let Some(opt) = g.next(argc.saturating_sub(1), argv, "e") {
        match opt {
            'e' => enforce_eof_marker = true,
            _ => {
                return Err(Error::Message(format!(
                    "{name}: usage: {name} [-e] [~/]path"
                )))
            }
        }
    }
    if g.ind + 1 != argc {
        return Err(Error::Message(format!(
            "{name}: usage: {name} [-e] [~/]path"
        )));
    }
    let path = resolve(argv[g.ind]);
    let text = read_effects_file(&path, files, enforce_eof_marker)?;
    let chain = build_sub_chain(istream, selector, &path, &text, files)?;
    let ostream = chain.ostream();
    let shared = Arc::new(Shared {
        path,
        istream,
        ostream,
        mask: selector.to_vec(),
        enforce_eof_marker,
        slot: Mutex::new(Slot {
            pending: None,
            retired: Vec::with_capacity(4),
            in_frames: 0,
            out_frames: 0,
        }),
    });
    let handle = WatchHandle {
        shared: Arc::clone(&shared),
    };
    Ok((
        Box::new(Watch {
            istream,
            ostream,
            mask: selector.to_vec(),
            chain: Some(chain),
            xfade: None,
            pos: 0,
            frames: 0,
            done: None,
            in_frames: 0,
            out_frames: 0,
            sizes_published: true,
            shared,
        }),
        handle,
    ))
}

struct Watch {
    istream: Stream,
    ostream: Stream,
    mask: Vec<bool>,
    chain: Option<Chain>,
    /// The chain being faded in, and the crossfade position and length (`xfade_state`).
    xfade: Option<Chain>,
    pos: usize,
    frames: usize,
    /// A chain that has been replaced, until it can go to the host.
    done: Option<Chain>,
    in_frames: usize,
    out_frames: usize,
    /// Whether the host knows the current block size (told when the lock is free).
    sizes_published: bool,
    shared: Arc<Shared>,
}

impl Watch {
    /// `watch_finish_xfade`
    fn finish_xfade(&mut self) {
        if let Some(new) = self.xfade.take() {
            self.done = self.chain.replace(new);
        }
        self.pos = 0;
    }

    /// Takes a reloaded chain, and hands a replaced one back to the host, if the host is not
    /// holding the lock right now.
    fn exchange(&mut self) {
        // a second reference to the shared state (no allocation), so `self` stays free
        let shared = Arc::clone(&self.shared);
        let Ok(mut slot) = shared.slot.try_lock() else {
            return;
        };
        if !self.sizes_published {
            slot.in_frames = self.in_frames;
            slot.out_frames = self.out_frames;
            self.sizes_published = true;
        }
        if self.done.is_some() && slot.retired.len() < slot.retired.capacity() {
            if let Some(old) = self.done.take() {
                slot.retired.push(old);
            }
        }
        if self.pos == 0 && self.done.is_none() {
            if let Some(new) = slot.pending.take() {
                // `effects_chain_xfade_begin`
                let fs = self
                    .chain
                    .as_ref()
                    .map_or(self.ostream.fs, |c| c.ostream().fs);
                self.frames = (XFADE_TIME / 1000.0 * f64::from(fs)).round() as usize;
                self.pos = self.frames;
                self.xfade = Some(new);
                if self.pos == 0 || self.chain.is_none() {
                    self.finish_xfade(); // no crossfade
                }
            }
        }
    }
}

impl Effect for Watch {
    fn name(&self) -> &'static str {
        "watch"
    }
    fn istream(&self) -> Stream {
        self.istream
    }
    fn ostream(&self) -> Stream {
        self.ostream
    }
    fn ch_deps_identity(&self) -> bool {
        false
    }
    /// `watch_effect_channel_deps`
    fn channel_deps(&self, deps: &mut [Vec<bool>]) -> bool {
        let n_in = self.istream.channels;
        for (i, row) in deps.iter_mut().enumerate().take(self.ostream.channels) {
            if i >= n_in || self.mask[i] {
                row[..n_in].copy_from_slice(&self.mask[..n_in]);
            }
        }
        true
    }
    /// `watch_effect_buffer_frames`: sizes the sub-chain for the longest block.
    fn buffer_frames(&self, frames: usize) -> usize {
        if frames > self.in_frames {
            self.chain
                .as_ref()
                .map_or(frames, |c| c.max_out_frames(frames))
        } else {
            self.out_frames
        }
    }
    fn prepare(&mut self) -> Result<()> {
        Ok(())
    }
    fn run(&mut self, frames: &mut usize, ibuf: &mut [f64], obuf: &mut [f64]) -> Buf {
        let (ich, och) = (self.istream.channels, self.ostream.channels);
        if *frames > self.in_frames {
            // the first (or a longer) block: size the buffers, and let reloads know
            self.in_frames = *frames;
            if let Some(c) = self.chain.as_mut() {
                self.out_frames = c.max_out_frames(*frames);
                c.prepare_buffers(*frames);
            }
            self.sizes_published = false;
        }
        self.exchange();
        let input = &ibuf[..*frames * ich];
        let Some(chain) = self.chain.as_mut() else {
            ibuf[..*frames * och.min(ich)].fill(0.0);
            return Buf::Input;
        };
        if self.pos == 0 {
            let out = chain.run_once(input);
            *frames = out.len() / och.max(1);
            obuf[..out.len()].copy_from_slice(out);
            return Buf::Output;
        }
        // `effects_chain_xfade_run`
        let Some(new) = self.xfade.as_mut() else {
            self.pos = 0;
            return Buf::Input;
        };
        let has_output = new.oframes() > 0;
        let old_out = chain.run_once(input);
        let f0 = old_out.len() / och.max(1);
        obuf[..old_out.len()].copy_from_slice(old_out);
        let new_out = new.run_once(input);
        let f1 = new_out.len() / och.max(1);
        // no output from the new chain yet (`oframes <= 0` after the run)
        if !has_output && f1 == 0 {
            *frames = f0;
            return Buf::Output;
        }
        let min_f = f0.min(f1);
        let mut adj = self.frames;
        let mut offset = 0;
        if !has_output {
            offset = (f0 - min_f) * och;
            *frames = f0;
        } else if f0 != f1 {
            if min_f < self.pos {
                adj = (min_f as f64 / self.pos as f64 * self.frames as f64).round() as usize;
                self.pos = min_f;
            }
            // frames past the old chain's output come from the new chain alone
            for i in f0..f1 {
                obuf[i * och..(i + 1) * och].copy_from_slice(&new_out[i * och..(i + 1) * och]);
            }
            *frames = f1;
        } else {
            *frames = f0;
        }
        for i in 0..min_f {
            let m = if self.pos > 0 {
                let m = (adj as f64 - self.pos as f64) / adj as f64;
                self.pos -= 1;
                m
            } else {
                1.0
            };
            for k in 0..och {
                let o = i * och + offset + k;
                obuf[o] = new_out[i * och + k] * m + obuf[o] * (1.0 - m);
            }
        }
        if self.pos == 0 {
            self.finish_xfade();
        }
        Buf::Output
    }
    fn reset(&mut self) {
        if self.pos > 0 {
            self.finish_xfade();
        }
        if let Some(c) = self.chain.as_mut() {
            c.reset();
        }
    }
    /// `watch_effect_drain2`: the sub-chain's own flushing, one block at a time.
    fn drain2(&mut self, frames: &mut isize, buf1: &mut [f64], _buf2: &mut [f64]) -> Option<Buf> {
        if self.pos > 0 {
            self.finish_xfade();
        }
        let och = self.ostream.channels.max(1);
        match self
            .chain
            .as_mut()
            .and_then(|c| c.drain_once((*frames).max(0) as usize))
        {
            Some(out) => {
                *frames = (out.len() / och) as isize;
                buf1[..out.len()].copy_from_slice(out);
            }
            None => *frames = -1,
        }
        Some(Buf::Input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    const S: Stream = Stream {
        fs: 48_000,
        channels: 2,
    };

    fn with_file<T>(text: &Rc<RefCell<String>>, f: impl FnOnce(&mut EffectsFiles) -> T) -> T {
        let t = Rc::clone(text);
        let mut read = move |p: &str| {
            if p == "fx.txt" {
                Ok(t.borrow().clone())
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    p.to_string(),
                ))
            }
        };
        let mut files = EffectsFiles {
            dir: None,
            home: None,
            read: &mut read,
        };
        f(&mut files)
    }

    #[test]
    fn a_reload_crossfades_to_the_new_effects_over_100_ms() {
        let text = Rc::new(RefCell::new("gain -6".to_string()));
        let mut c = with_file(&text, |f| {
            Chain::from_args_with_files(S, &["watch", "fx.txt"], f)
        })
        .unwrap();
        assert_eq!(c.watches().len(), 1);
        assert_eq!(c.watches()[0].path(), "fx.txt");
        c.set_block_frames(1000);
        let x = vec![0.5; 2 * 3000];
        let before = c.run(&x);
        let g = 10f64.powf(-6.0 / 20.0);
        assert!(before.iter().all(|v| (v - 0.5 * g).abs() < 1e-12));

        *text.borrow_mut() = "gain 0 # louder".to_string();
        let h = c.watches()[0].clone();
        with_file(&text, |f| h.reload(f)).unwrap();
        let y = c.run(&vec![0.5; 2 * 6000]);
        // the first frame is still the old effects, then a linear fade over 4800 frames
        assert!((y[0] - 0.5 * g).abs() < 1e-12);
        let mid = 2 * 2400;
        assert!((y[mid] - 0.5 * (g + 1.0) / 2.0).abs() < 1e-3, "{}", y[mid]);
        assert!(y[2 * 4800..].iter().all(|v| (v - 0.5).abs() < 1e-12));
        h.collect();
    }

    #[test]
    fn a_reload_that_does_not_fit_is_refused_and_the_old_effects_stay() {
        let text = Rc::new(RefCell::new("gain -6".to_string()));
        let mut c = with_file(&text, |f| {
            Chain::from_args_with_files(S, &["watch", "fx.txt"], f)
        })
        .unwrap();
        let h = c.watches()[0].clone();
        c.run(&[0.5; 2 * 100]);
        for bad in ["remix 0 1 0", "resample 44100", "bogus 1"] {
            *text.borrow_mut() = bad.to_string();
            assert!(with_file(&text, |f| h.reload(f)).is_err(), "{bad}");
        }
        let y = c.run(&[0.5; 2 * 100]);
        assert!((y[0] - 0.5 * 10f64.powf(-6.0 / 20.0)).abs() < 1e-12);
    }

    #[test]
    fn the_end_of_file_marker_is_checked_with_e() {
        let text = Rc::new(RefCell::new("gain -6\n".to_string()));
        let mk =
            |a: &[&str]| with_file(&text, |f| Chain::from_args_with_files(S, a, f)).map(|_| ());
        assert!(mk(&["watch", "fx.txt"]).is_ok());
        assert!(mk(&["watch", "-e", "fx.txt"]).is_err());
        *text.borrow_mut() = "gain -6\n#EOF#\n\n".to_string();
        assert!(mk(&["watch", "-e", "fx.txt"]).is_ok());
        *text.borrow_mut() = "gain -6 #EOF#".to_string();
        assert!(mk(&["watch", "-e", "fx.txt"]).is_err());
        assert!(mk(&["watch"]).is_err());
        assert!(matches!(
            Chain::from_args(S, &["watch", "fx.txt"]),
            Err(Error::Unsupported(_))
        ));
    }
}
