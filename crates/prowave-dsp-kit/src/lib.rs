//! A Rust port of the effects of the `dsp` audio processor (https://github.com/bmc0/dsp,
//! ISC licence, see `NOTICE`), with the same effect names, the same arguments and the same
//! numbers coming out.
//!
//! Logic Pro equivalent: the single-purpose utilities Logic spreads over its Channel EQ,
//! Gain, Direction Mixer, Delay and Dither plug-ins, plus the crossover, upmixing and
//! convolution tools it does not have.
//!
//! # How it works
//!
//! Audio is a block of interleaved `f64` samples (the original's `sample_t`). An [`Effect`]
//! processes a block in place or into a second buffer, exactly like the original's `run()`.
//! A [`Chain`] strings effects together the way the `dsp` command line does:
//!
//! ```
//! use prowave_dsp_kit::{Chain, Stream};
//!
//! // `dsp ... gain -6 lowpass 1k 0.7q` on a stereo stream at 48 kHz.
//! let mut chain = Chain::from_args(
//!     Stream { fs: 48_000, channels: 2 },
//!     &["gain", "-6", "lowpass", "1k", "0.7q"],
//! )
//! .unwrap();
//! let out = chain.run(&[0.5; 2 * 1024]);
//! assert_eq!(out.len(), 2 * 1024);
//! ```
//!
//! # What is and is not ported
//!
//! The effects are ported; the program around them is not. Reading and writing files and
//! sound cards (ALSA, PulseAudio, ao, ffmpeg, libsndfile) is what Pro Wave's own engine,
//! `cpal` and `symphonia` already do (so a filter comes as `coefs:` or as samples in memory, see
//! [`fir_util`]), the zita-convolver library is replaced by Pro Wave's own convolver (`-z` uses
//! `fir_p`, as the original does without the library), and the LADSPA frontend by Pro Wave's
//! LADSPA host.
//!
//! Everything here allocates when an effect is made and not while it runs, and uses no
//! platform calls, so it builds for `wasm32-unknown-unknown` too.
#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod align;
pub mod allpass;
pub mod args;
pub mod biquad;
pub mod catalog;
pub mod chain;
pub mod crossfeed;
pub mod decorrelate;
pub mod delay;
pub mod dither;
pub mod effect;
pub mod error;
pub mod fir;
pub mod fir_p;
pub mod fir_util;
pub mod gain;
pub mod hilbert;
pub mod matrix4;
mod matrix4_common;
mod mod_tables;
pub mod noise;
mod optimize;
pub mod remix;
pub mod resample;
pub mod reverse_iir;
pub mod rng;
pub mod selector;
pub mod sinc;
pub mod smoothing;
pub mod st2ms;
pub mod window;

/// Starts the program-wide random sequences over (the delays of `decorrelate` and the noise of
/// the modulated `delay`), as they are at the start of a run of the original program. Effects
/// made afterwards get the same numbers as the first ones did.
pub fn reset_random_seeds() {
    decorrelate::reset_seed();
    delay::reset_seed();
}

pub use chain::Chain;
pub use effect::{get_effect_info, Buf, Effect, EffectInfo, Stream};
pub use error::Error;
