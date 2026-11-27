//! The effect interface and the table of effects (`effect.h` and `effect.c` of the original).

use crate::error::{Error, Result};

/// The shape of a stream: sample rate and number of interleaved channels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stream {
    pub fs: u32,
    pub channels: usize,
}

/// Which buffer holds the result of [`Effect::run`]: the one the effect was given to work on
/// (in place) or the second one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Buf {
    Input,
    Output,
}

/// One processing step. `run` takes the number of frames in `ibuf` (it may change it) and two
/// interleaved buffers, and says which one holds the result.
pub trait Effect: Send {
    /// The name the effect has on the command line.
    fn name(&self) -> &'static str;
    fn istream(&self) -> Stream;
    fn ostream(&self) -> Stream;
    /// Processes `*frames` frames; returns which buffer holds the `*frames` result frames.
    fn run(&mut self, frames: &mut usize, ibuf: &mut [f64], obuf: &mut [f64]) -> Buf;
    /// Forgets the signal (filter memory, delay lines).
    fn reset(&mut self) {}
    /// The number of frames each buffer must be able to hold when `frames` come in.
    fn buffer_frames(&self, frames: usize) -> usize {
        frames
    }
    /// Whether the effect leaves the signal at a precision where dither would be pointless
    /// (`EFFECT_FLAG_NO_DITHER`: meters, statistics, pure channel routing).
    fn no_dither(&self) -> bool {
        false
    }
    /// Gives a dither effect the precision of the output (`dither_effect_set_params`); returns
    /// whether this effect is a dither effect.
    fn set_dither_params(&mut self, _bits: i32, _enabled: bool) -> bool {
        false
    }
    /// Whether this is a dither effect.
    fn is_dither(&self) -> bool {
        false
    }
    /// Last step before the chain is used (`prepare`): work that depends on the other settings.
    fn prepare(&mut self) -> Result<()> {
        Ok(())
    }
    /// Which input channels each output channel depends on (`channel_deps`): `deps[out][in]`.
    /// The rows start as the identity. Returns whether the effect says anything.
    fn channel_deps(&self, _deps: &mut [Vec<bool>]) -> bool {
        false
    }
    /// Whether output channel *i* depends on input channel *i* only (`EFFECT_FLAG_CH_DEPS_IDENTITY`)
    /// when the effect gives no [`Effect::channel_deps`]. The default says yes.
    fn ch_deps_identity(&self) -> bool {
        true
    }
    /// The channels the effect works on, for effects that mix them without saying how.
    fn channel_selector(&self) -> Option<&[bool]> {
        None
    }
    /// Whether all input channels must be aligned before this effect (`EFFECT_FLAG_ALIGN_BARRIER`).
    fn align_barrier(&self) -> bool {
        false
    }
    /// Adds the effect's latency and the delay it was asked for, per channel
    /// (`channel_offsets`). Returns whether the effect has any.
    fn channel_offsets(&self, _latency: &mut [isize], _req_delay: &mut [isize]) -> bool {
        false
    }
    /// Adds the number of frames the effect needs to flush its memory, per channel
    /// (`drain_samples`). Returns whether the effect says anything.
    fn drain_samples(&self, _samples: &mut [isize]) -> bool {
        false
    }
}

/// The function that makes the effects of a command from its arguments: usually one, but
/// `delay` makes a delay and a modulation stage, or nothing when the delay is zero.
pub type InitFn = fn(&EffectInfo, &Stream, &[bool], &[&str]) -> Result<Vec<Box<dyn Effect>>>;

/// How to make an effect: `init(info, input stream, channel selector, argv)` where `argv[0]` is
/// the effect's name, as on the command line.
pub struct EffectInfo {
    pub name: &'static str,
    pub usage: &'static str,
    pub init: InitFn,
    /// Which effect of a family `init` makes.
    pub number: i32,
}

impl EffectInfo {
    /// The error for arguments that do not fit the usage line.
    pub fn usage_error(&self) -> Error {
        Error::Usage {
            effect: self.name.into(),
            usage: self.usage.into(),
        }
    }
}

/// Every effect, in the order of the original's table.
pub fn effects() -> impl Iterator<Item = &'static EffectInfo> {
    crate::biquad::INFO
        .iter()
        .chain(crate::gain::INFO.iter())
        .chain(crate::crossfeed::INFO.iter())
        .chain(crate::remix::INFO.iter())
        .chain(crate::st2ms::INFO.iter())
        .chain(crate::delay::INFO.iter())
        .chain(crate::decorrelate::INFO.iter())
        .chain(crate::noise::INFO.iter())
        .chain(crate::dither::INFO.iter())
}

/// The effect called `name`, if there is one.
///
/// Pro Wave's names for the effects (see [`crate::catalog`]) find them as well.
pub fn get_effect_info(name: &str) -> Option<&'static EffectInfo> {
    let command = crate::catalog::command_for(name).unwrap_or(name);
    effects().find(|e| e.name == command)
}

/// The effect's channel selector as a vector of `n` entries: the given one, or all channels.
pub(crate) fn default_selector(selector: &[bool], n: usize) -> Vec<bool> {
    if selector.len() == n {
        selector.to_vec()
    } else {
        vec![true; n]
    }
}
