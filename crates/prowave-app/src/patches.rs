//! Library patch → engine sound. Each Library category has a base synth sound (bass, pluck,
//! electric piano, strings, pad, organ, drum kit...), and every patch name adds a small, stable
//! variation (brightness, detune, attack, release) so patches in one category sound different.
//!
//! The built-in engine is a two-oscillator subtractive synth, so these are approximations of
//! the instrument families, not sampled instruments.
use crate::model::Track;
use crate::track_icons::{icon_kind, IconKind};
use prowave_engine::{Kit, SynthPatch, Wave};

fn hash(s: &str) -> u32 {
    s.bytes().fold(0x811C_9DC5u32, |h, b| {
        (h ^ b as u32).wrapping_mul(0x0100_0193)
    })
}

/// Stable pseudo-random value in -1..1 for (hash, salt).
fn jitter(h: u32, salt: u32) -> f32 {
    let x = (h ^ salt.wrapping_mul(0x9E37_79B1)).wrapping_mul(2_654_435_761);
    ((x >> 8) as f32 / (1u32 << 24) as f32) * 2.0 - 1.0
}

fn vary(mut p: SynthPatch, h: u32) -> SynthPatch {
    if p.kit != Kit::None {
        return p;
    }
    if p.cutoff < 19_000.0 {
        p.cutoff *= 2f32.powf(0.5 * jitter(h, 1));
    }
    p.detune_b += 4.0 * jitter(h, 2);
    p.attack = (p.attack * (1.0 + 0.25 * jitter(h, 3))).max(0.001);
    p.release = (p.release * (1.0 + 0.25 * jitter(h, 4))).max(0.01);
    p.resonance = (p.resonance + 0.06 * jitter(h, 5)).clamp(0.0, 0.9);
    p
}

fn base() -> SynthPatch {
    SynthPatch::default()
}

fn bass() -> SynthPatch {
    SynthPatch {
        wave_a: Wave::Saw,
        level_a: 0.8,
        wave_b: Wave::Square,
        level_b: 0.5,
        semi_b: -12.0,
        cutoff: 650.0,
        resonance: 0.25,
        filter_env: 0.55,
        attack: 0.004,
        decay: 0.25,
        sustain: 0.55,
        release: 0.12,
        gain: 1.15,
        ..base()
    }
}

fn pluck() -> SynthPatch {
    SynthPatch {
        wave_a: Wave::Saw,
        level_a: 0.7,
        wave_b: Wave::Triangle,
        level_b: 0.5,
        semi_b: 12.0,
        detune_b: 6.0,
        cutoff: 2600.0,
        resonance: 0.1,
        filter_env: 0.9,
        attack: 0.002,
        decay: 0.35,
        sustain: 0.12,
        release: 0.25,
        ..base()
    }
}

fn electric_piano() -> SynthPatch {
    SynthPatch {
        wave_a: Wave::Sine,
        level_a: 1.0,
        wave_b: Wave::Sine,
        level_b: 0.35,
        semi_b: 12.0,
        attack: 0.003,
        decay: 0.9,
        sustain: 0.3,
        release: 0.3,
        ..base()
    }
}

fn mallet() -> SynthPatch {
    SynthPatch {
        wave_a: Wave::Sine,
        level_a: 1.0,
        wave_b: Wave::Sine,
        level_b: 0.5,
        semi_b: 19.0,
        attack: 0.002,
        decay: 0.6,
        sustain: 0.0,
        release: 0.5,
        ..base()
    }
}

fn strings() -> SynthPatch {
    SynthPatch {
        wave_a: Wave::Saw,
        level_a: 0.6,
        wave_b: Wave::Saw,
        level_b: 0.6,
        semi_b: 0.0,
        detune_b: 9.0,
        cutoff: 2200.0,
        resonance: 0.1,
        filter_env: 0.3,
        attack: 0.28,
        decay: 0.4,
        sustain: 0.85,
        release: 0.5,
        vibrato_hz: 5.2,
        vibrato_cents: 7.0,
        ..base()
    }
}

fn horns() -> SynthPatch {
    SynthPatch {
        wave_a: Wave::Saw,
        level_a: 0.7,
        wave_b: Wave::Square,
        level_b: 0.4,
        semi_b: 0.0,
        detune_b: 4.0,
        cutoff: 1500.0,
        resonance: 0.1,
        filter_env: 0.8,
        attack: 0.07,
        decay: 0.2,
        sustain: 0.8,
        release: 0.18,
        ..base()
    }
}

fn piano() -> SynthPatch {
    SynthPatch {
        wave_a: Wave::Saw,
        level_a: 0.55,
        wave_b: Wave::Triangle,
        level_b: 0.45,
        semi_b: 12.0,
        detune_b: 2.0,
        cutoff: 3800.0,
        filter_env: 0.7,
        attack: 0.002,
        decay: 1.2,
        sustain: 0.22,
        release: 0.3,
        ..base()
    }
}

fn pulse_sampler() -> SynthPatch {
    SynthPatch {
        wave_a: Wave::Square,
        level_a: 0.7,
        pulse_width: 0.3,
        wave_b: Wave::Triangle,
        level_b: 0.4,
        semi_b: 12.0,
        cutoff: 4000.0,
        filter_env: 0.6,
        attack: 0.002,
        decay: 0.2,
        sustain: 0.3,
        release: 0.12,
        ..base()
    }
}

fn supersaw_pad() -> SynthPatch {
    SynthPatch {
        wave_a: Wave::Saw,
        level_a: 0.55,
        wave_b: Wave::Saw,
        level_b: 0.55,
        semi_b: 0.0,
        detune_b: 14.0,
        cutoff: 2800.0,
        resonance: 0.2,
        filter_env: 0.35,
        attack: 0.25,
        decay: 0.5,
        sustain: 0.8,
        release: 0.7,
        ..base()
    }
}

fn synth_lead() -> SynthPatch {
    SynthPatch {
        wave_a: Wave::Saw,
        level_a: 0.8,
        wave_b: Wave::Saw,
        level_b: 0.7,
        semi_b: 0.0,
        detune_b: 12.0,
        cutoff: 5000.0,
        resonance: 0.25,
        filter_env: 0.4,
        attack: 0.005,
        decay: 0.15,
        sustain: 0.8,
        release: 0.15,
        vibrato_hz: 5.5,
        vibrato_cents: 10.0,
        ..base()
    }
}

fn arp_pluck() -> SynthPatch {
    SynthPatch {
        wave_a: Wave::Saw,
        level_a: 0.7,
        wave_b: Wave::Square,
        level_b: 0.3,
        semi_b: 12.0,
        cutoff: 3200.0,
        resonance: 0.3,
        filter_env: 0.9,
        attack: 0.002,
        decay: 0.18,
        sustain: 0.0,
        release: 0.12,
        ..base()
    }
}

fn organ() -> SynthPatch {
    SynthPatch {
        wave_a: Wave::Sine,
        level_a: 1.0,
        wave_b: Wave::Sine,
        level_b: 0.5,
        semi_b: 19.0,
        cutoff: 6000.0,
        attack: 0.006,
        decay: 0.0,
        sustain: 1.0,
        release: 0.04,
        vibrato_hz: 6.2,
        vibrato_cents: 6.0,
        ..base()
    }
}

fn clav() -> SynthPatch {
    SynthPatch {
        wave_a: Wave::Square,
        level_a: 0.8,
        pulse_width: 0.22,
        wave_b: Wave::Saw,
        level_b: 0.4,
        semi_b: 12.0,
        cutoff: 2400.0,
        resonance: 0.3,
        filter_env: 1.0,
        attack: 0.001,
        decay: 0.18,
        sustain: 0.15,
        release: 0.08,
        ..base()
    }
}

fn mellotron() -> SynthPatch {
    SynthPatch {
        wave_a: Wave::Saw,
        level_a: 0.5,
        wave_b: Wave::Triangle,
        level_b: 0.6,
        semi_b: 0.0,
        detune_b: 8.0,
        cutoff: 1900.0,
        attack: 0.09,
        decay: 0.5,
        sustain: 0.9,
        release: 0.35,
        vibrato_hz: 4.8,
        vibrato_cents: 5.0,
        ..base()
    }
}

fn cinematic() -> SynthPatch {
    SynthPatch {
        wave_a: Wave::Saw,
        level_a: 0.5,
        wave_b: Wave::Saw,
        level_b: 0.5,
        semi_b: 7.0,
        detune_b: 15.0,
        cutoff: 1600.0,
        resonance: 0.15,
        filter_env: 0.5,
        attack: 0.9,
        decay: 1.0,
        sustain: 0.8,
        release: 1.4,
        ..base()
    }
}

fn legacy() -> SynthPatch {
    SynthPatch {
        wave_a: Wave::Square,
        level_a: 0.6,
        pulse_width: 0.4,
        wave_b: Wave::Saw,
        level_b: 0.5,
        semi_b: 0.0,
        detune_b: 7.0,
        cutoff: 2200.0,
        resonance: 0.3,
        filter_env: 0.6,
        attack: 0.01,
        decay: 0.3,
        sustain: 0.6,
        release: 0.25,
        ..base()
    }
}

fn kit(kind: Kit) -> SynthPatch {
    SynthPatch {
        kit: kind,
        ..base()
    }
}

/// Synthesizer patches pick a sub-type from words in the patch name.
fn synth_for(name: &str) -> SynthPatch {
    let n = name.to_lowercase();
    let has = |w: &[&str]| w.iter().any(|x| n.contains(x));
    if has(&["lead", "mono"]) {
        synth_lead()
    } else if has(&["bass"]) {
        bass()
    } else if has(&["arp", "pluck"]) {
        arp_pluck()
    } else if has(&["brass"]) {
        horns()
    } else {
        supersaw_pad()
    }
}

fn by_category(category: &str, name: &str) -> SynthPatch {
    match category {
        "Bass" => bass(),
        "Drum Kit" | "Percussion" => kit(Kit::Acoustic),
        "Electronic Drum Kit" => kit(Kit::Electronic),
        "Guitar" | "Guitar Amp" | "World" => pluck(),
        "Keyboards" | "Vintage Electric Piano" => electric_piano(),
        "Mallet" => mallet(),
        "Orchestral" | "Studio Strings" => strings(),
        "Studio Horns" => horns(),
        "Piano" => piano(),
        "Quick Sampler" => pulse_sampler(),
        "Synthesizer" => synth_for(name),
        "Vintage B3 Organ" => organ(),
        "Vintage Clav" => clav(),
        "Vintage Mellotron" => mellotron(),
        "Arpeggiator" => arp_pluck(),
        "Cinematic" => cinematic(),
        "Legacy" => legacy(),
        _ => base(),
    }
}

fn by_icon(kind: IconKind, name: &str) -> SynthPatch {
    match kind {
        IconKind::DrumKit | IconKind::DrummerTile => kit(Kit::Acoustic),
        IconKind::Piano => piano(),
        IconKind::WoodKeys => electric_piano(),
        IconKind::Synth => synth_for(name),
        IconKind::Bass => bass(),
        IconKind::Guitar => pluck(),
        IconKind::Strings => strings(),
        // Never played (these tracks have no notes); any patch will do.
        IconKind::Mic
        | IconKind::Waveform
        | IconKind::Note
        | IconKind::Aux
        | IconKind::Folder
        | IconKind::Vca
        | IconKind::ExternalMidi => base(),
    }
}

/// The synth patch a track should play: its Library category if a patch was loaded, else the
/// instrument family implied by its name and icon.
pub fn synth_patch(t: &Track) -> SynthPatch {
    let p = match t.category {
        Some(cat) => by_category(cat, &t.name),
        None => by_icon(icon_kind(t), &t.name),
    };
    vary(p, hash(&t.name))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every Library patch, played over the demo song's notes on one track: no sample and no
    /// peak meter may go NaN or infinite (a GitHub report: Mellow Grand's meter stuck at inf).
    #[test]
    fn every_library_patch_plays_finite_audio() {
        let mut bad = Vec::new();
        for cat in CATEGORIES {
            for name in cat.patches {
                let mut p = Project::demo();
                let mut t = p.tracks.remove(0);
                t.category = Some(cat.name);
                t.name = (*name).to_string();
                p.tracks = vec![t];
                let r = crate::bench::render(&p, 6.0);
                if r.non_finite > 0 || !r.bad_meters.is_empty() {
                    bad.push(format!(
                        "{} / {name}: {} bad samples",
                        cat.name, r.non_finite
                    ));
                }
            }
        }
        assert!(bad.is_empty(), "non-finite audio from: {bad:#?}");
    }
    use crate::library::CATEGORIES;
    use crate::model::Project;

    fn track_with(category: &'static str, name: &str) -> Track {
        let mut t = Project::demo().tracks.remove(0);
        t.category = Some(category);
        t.name = name.to_string();
        t
    }

    #[test]
    fn every_library_patch_has_a_sane_patch() {
        for c in CATEGORIES {
            for name in c.patches {
                let p = synth_patch(&track_with(c.name, name));
                assert!(
                    p.cutoff >= 20.0 && p.attack >= 0.001 && p.release >= 0.01,
                    "{name}"
                );
                assert!(
                    (0.0..=1.0).contains(&p.sustain) && (0.0..=1.0).contains(&p.resonance),
                    "{name}"
                );
                assert!(
                    p.gain > 0.0 && p.level_a.is_finite() && p.detune_b.is_finite(),
                    "{name}"
                );
            }
        }
    }

    #[test]
    fn patches_in_one_category_differ_and_categories_differ() {
        let a = synth_patch(&track_with("Synthesizer", "Airy Glass Pad"));
        let b = synth_patch(&track_with("Synthesizer", "Analog Drift Pad"));
        assert_ne!(a, b);
        let lead = synth_patch(&track_with("Synthesizer", "Bright Saw Lead"));
        assert!(lead.attack < a.attack, "leads attack faster than pads");
        assert!(
            synth_patch(&track_with("Bass", "Round Upright")).cutoff
                < synth_patch(&track_with("Piano", "Felt Upright")).cutoff
        );
        assert_eq!(
            synth_patch(&track_with("Drum Kit", "Dry Studio Kit")).kit,
            Kit::Acoustic
        );
        assert_eq!(
            synth_patch(&track_with("Electronic Drum Kit", "Classic 808 Kit")).kit,
            Kit::Electronic
        );
    }

    #[test]
    fn demo_tracks_map_to_their_instrument_family() {
        let p = Project::demo();
        let by_name = |n: &str| p.tracks.iter().find(|t| t.name == n).map(synth_patch);
        assert_eq!(by_name("Drummer").map(|x| x.kit), Some(Kit::Acoustic));
        assert_eq!(by_name("Electric Bass").map(|x| x.wave_a), Some(Wave::Saw));
        assert_eq!(
            by_name("Steinway Grand Piano").map(|x| x.wave_b),
            Some(Wave::Triangle)
        );
    }
}
