//! Pro Wave's own names for the effects, with the same meaning as the originals.
//!
//! The command names of the original (`lowpass`, `crossfeed`, `dither`…) stay what the
//! command-line parser and the tests use, so the port can be compared with the original sample
//! by sample. In the app every component has a name of its own, which [`Chain::push`] and
//! [`Chain::from_args`] accept as well. The family name comes first and the meaning last, like
//! Pro Wave's other effects (Kestrel EQ, Mosaic Decimator, Dustbowl Lo-Fi).
//!
//! [`Chain::push`]: crate::Chain::push
//! [`Chain::from_args`]: crate::Chain::from_args

/// One component: the original's command, Pro Wave's name, and what it does.
#[derive(Clone, Copy, Debug)]
pub struct Entry {
    pub command: &'static str,
    pub name: &'static str,
    pub meaning: &'static str,
    /// Whether this crate has the effect yet.
    pub ported: bool,
}

macro_rules! e {
    ($c:expr, $n:expr, $m:expr, $p:expr) => {
        Entry {
            command: $c,
            name: $n,
            meaning: $m,
            ported: $p,
        }
    };
}

/// Every component of the original.
pub static ENTRIES: [Entry; 40] = [
    e!(
        "lowpass_1",
        "Lattice One-Pole Low-Pass",
        "first-order low-pass filter",
        true
    ),
    e!(
        "highpass_1",
        "Lattice One-Pole High-Pass",
        "first-order high-pass filter",
        true
    ),
    e!(
        "allpass_1",
        "Lattice One-Pole All-Pass",
        "first-order all-pass (phase) filter",
        true
    ),
    e!(
        "lowshelf_1",
        "Lattice One-Pole Low Shelf",
        "first-order low shelf",
        true
    ),
    e!(
        "highshelf_1",
        "Lattice One-Pole High Shelf",
        "first-order high shelf",
        true
    ),
    e!(
        "lowpass_1p",
        "Lattice Smoother",
        "single-pole low-pass (exponential moving average)",
        true
    ),
    e!(
        "lowpass",
        "Lattice Low-Pass",
        "second-order low-pass filter",
        true
    ),
    e!(
        "highpass",
        "Lattice High-Pass",
        "second-order high-pass filter",
        true
    ),
    e!(
        "bandpass_skirt",
        "Lattice Band-Pass Skirt",
        "band-pass filter with constant skirt gain",
        true
    ),
    e!(
        "bandpass_peak",
        "Lattice Band-Pass Peak",
        "band-pass filter with constant peak gain",
        true
    ),
    e!("notch", "Lattice Notch", "notch (band-reject) filter", true),
    e!(
        "allpass",
        "Lattice All-Pass",
        "second-order all-pass (phase) filter",
        true
    ),
    e!("eq", "Lattice Bell", "second-order peaking EQ", true),
    e!(
        "lowshelf",
        "Lattice Low Shelf",
        "second-order low shelf",
        true
    ),
    e!(
        "highshelf",
        "Lattice High Shelf",
        "second-order high shelf",
        true
    ),
    e!(
        "lowpass_transform",
        "Pivot Low-Pass Transform",
        "moves a low-pass pole and zero",
        true
    ),
    e!(
        "highpass_transform",
        "Pivot Transform",
        "moves a high-pass pole and zero (Linkwitz transform)",
        true
    ),
    e!(
        "linkwitz_transform",
        "Pivot Transform (Linkwitz)",
        "Linkwitz transform, an alias of the high-pass transform",
        true
    ),
    e!(
        "deemph",
        "Aurora De-Emphasis",
        "Compact Disc de-emphasis filter",
        true
    ),
    e!(
        "biquad",
        "Lattice Coefficients",
        "a biquad from its raw coefficients",
        true
    ),
    e!("gain", "Level Trim", "gain in decibels", true),
    e!("mult", "Level Scale", "multiplies by a factor", true),
    e!("add", "Level Offset", "adds a constant (DC offset)", true),
    e!("crossfeed", "Harbor Crossfeed", "headphone crossfeed", true),
    e!(
        "matrix4",
        "Quartet Upmixer",
        "active matrix upmixer for music (2/2 and 2/4)",
        false
    ),
    e!(
        "matrix4_mb",
        "Quartet Multiband Upmixer",
        "the upmixer steered in many frequency bands",
        false
    ),
    e!(
        "remix",
        "Patchbay Remix",
        "mixes and reorders channels",
        true
    ),
    e!(
        "st2ms",
        "Mirror Mid/Side Encode",
        "stereo to mid/side",
        true
    ),
    e!(
        "ms2st",
        "Mirror Mid/Side Decode",
        "mid/side to stereo",
        true
    ),
    e!(
        "delay",
        "Echo Dock Delay",
        "delay line in seconds, milliseconds or samples",
        false
    ),
    e!(
        "resample",
        "Cascade Resampler",
        "high-quality sinc sample-rate converter",
        false
    ),
    e!("fir", "Ribbon FIR", "FIR filter by FFT convolution", false),
    e!(
        "fir_p",
        "Ribbon FIR Partitioned",
        "zero-latency partitioned convolution",
        false
    ),
    e!(
        "hilbert",
        "Quadrature Hilbert",
        "FIR Hilbert transform (phase shifter)",
        false
    ),
    e!(
        "sinc",
        "Needle Sinc Filter",
        "windowed-sinc low-pass or high-pass",
        false
    ),
    e!(
        "decorrelate",
        "Haze Decorrelator",
        "all-pass decorrelator for widening",
        true
    ),
    e!("noise", "Grain Noise", "adds TPDF noise", true),
    e!(
        "dither",
        "Velvet Dither",
        "dither with optional noise shaping",
        true
    ),
    e!(
        "stats",
        "Gauge Statistics",
        "peak, RMS, DC offset and crest factor of each channel",
        false
    ),
    e!(
        "levels",
        "Gauge Meters",
        "real-time level meter per channel",
        false
    ),
];

/// The entry for a command or for Pro Wave's name of it (case-insensitive).
pub fn find(name: &str) -> Option<&'static Entry> {
    ENTRIES
        .iter()
        .find(|e| e.command == name || e.name.eq_ignore_ascii_case(name))
}

/// The original's command for `name`, which may be a command or Pro Wave's name.
pub fn command_for(name: &str) -> Option<&'static str> {
    find(name).map(|e| e.command)
}

/// Pro Wave's name for `command`.
pub fn display_name(command: &str) -> Option<&'static str> {
    ENTRIES
        .iter()
        .find(|e| e.command == command)
        .map(|e| e.name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_name_is_unique_and_none_is_the_originals_command() {
        for (i, a) in ENTRIES.iter().enumerate() {
            assert_ne!(a.name.to_lowercase(), a.command, "{}", a.name);
            for b in &ENTRIES[i + 1..] {
                assert_ne!(a.command, b.command);
                assert_ne!(a.name.to_lowercase(), b.name.to_lowercase());
            }
        }
    }

    #[test]
    fn every_effect_that_is_ported_has_an_entry_and_the_other_way_round() {
        for info in crate::effect::effects() {
            let entry = ENTRIES.iter().find(|e| e.command == info.name);
            assert!(
                entry.is_some_and(|e| e.ported),
                "{} not in the catalog",
                info.name
            );
        }
        for e in ENTRIES.iter().filter(|e| e.ported) {
            assert!(
                crate::effect::effects().any(|i| i.name == e.command),
                "{} is marked ported but missing",
                e.command
            );
        }
    }

    #[test]
    fn both_spellings_find_the_same_effect() {
        assert_eq!(command_for("Lattice Low-Pass"), Some("lowpass"));
        assert_eq!(command_for("lattice low-pass"), Some("lowpass"));
        assert_eq!(command_for("lowpass"), Some("lowpass"));
        assert_eq!(display_name("dither"), Some("Velvet Dither"));
        assert_eq!(command_for("nothing"), None);
    }
}
