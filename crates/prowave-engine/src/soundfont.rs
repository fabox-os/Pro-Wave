//! SoundFont (`.sf2`) instruments: General MIDI and other sample-based sounds played inside the
//! engine, the way Logic Pro's software instruments play their sounds. The synthesis is done by
//! `rustysynth`, a pure Rust SoundFont synthesizer, so the engine links no native library.
//!
//! Loading ([`SoundFontBank::load`], [`SfPlayer::new`]) reads and allocates, so it runs off the
//! audio thread and the finished player is handed over with a command. The audio thread only
//! calls [`SfPlayer::note_on`], [`SfPlayer::note_off`] and [`SfPlayer::render`], which do not
//! allocate.

use std::sync::Arc;

use rustysynth::{SoundFont, Synthesizer, SynthesizerSettings};

/// The largest SoundFont accepted, in bytes. The General MIDI banks are about 150 MB.
pub const MAX_SOUNDFONT_BYTES: usize = 512 * 1024 * 1024;

/// The most notes one track's SoundFont sounds at once.
const MAX_POLYPHONY: usize = 64;

/// Rendering block size in frames: a note that starts mid-block sounds at the next block, so
/// this is the timing error of soundfont notes (16 frames is 0.33 ms at 48 kHz).
const BLOCK_FRAMES: usize = 16;

/// The bank number of the drum kits (General MIDI channel 10).
const DRUM_BANK: u16 = 128;
/// The synthesizer channel that plays drum kits.
const DRUM_CHANNEL: i32 = 9;

/// One preset (sound) of a SoundFont: its name and how to select it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresetInfo {
    pub name: String,
    /// Bank number; [`DRUM_BANK`] (128) for drum kits.
    pub bank: u16,
    /// Program number, 0-127.
    pub program: u8,
}

impl PresetInfo {
    /// Whether this preset is a drum kit (it plays on the drum channel).
    pub fn is_drums(&self) -> bool {
        self.bank == DRUM_BANK
    }
}

/// A parsed SoundFont and its presets. Clone-free and shared by every player that uses it.
pub struct SoundFontBank {
    font: Arc<SoundFont>,
    presets: Vec<PresetInfo>,
}

impl SoundFontBank {
    /// Parses a SoundFont from its bytes. Malformed files are an error, never a panic.
    pub fn load(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_SOUNDFONT_BYTES {
            return Err(format!(
                "the SoundFont is larger than {} MB",
                MAX_SOUNDFONT_BYTES >> 20
            ));
        }
        let mut reader = bytes;
        Self::load_reader(&mut reader)
    }

    /// Parses a SoundFont as it is read from `reader`, so a big file is never held twice in
    /// memory and the caller can count the bytes read to show progress. The reader should be
    /// buffered (the headers are read a few bytes at a time).
    pub fn load_reader<R: std::io::Read>(reader: &mut R) -> Result<Self, String> {
        let font = SoundFont::new(reader).map_err(|e| format!("not a SoundFont: {e:?}"))?;
        let presets = font
            .get_presets()
            .iter()
            .map(|p| PresetInfo {
                name: p.get_name().to_string(),
                bank: p.get_bank_number().clamp(0, i32::from(u16::MAX)) as u16,
                program: p.get_patch_number().clamp(0, 127) as u8,
            })
            .collect();
        Ok(Self {
            font: Arc::new(font),
            presets,
        })
    }

    /// Every preset of the SoundFont, in file order.
    pub fn presets(&self) -> &[PresetInfo] {
        &self.presets
    }
}

/// One track's SoundFont instrument: a synthesizer set to one preset. Make it with
/// [`SfPlayer::new`] off the audio thread.
pub struct SfPlayer {
    synth: Synthesizer,
    channel: i32,
    sample_rate: i32,
}

impl SfPlayer {
    /// A player for `preset` at `sample_rate` (Hz). Allocates; call off the audio thread.
    pub fn new(
        bank: &SoundFontBank,
        preset: &PresetInfo,
        sample_rate: f32,
    ) -> Result<Self, String> {
        let rate = sample_rate.round().clamp(16_000.0, 192_000.0) as i32;
        let mut settings = SynthesizerSettings::new(rate);
        settings.block_size = BLOCK_FRAMES;
        settings.maximum_polyphony = MAX_POLYPHONY;
        let mut synth = Synthesizer::new(&bank.font, &settings).map_err(|e| format!("{e:?}"))?;
        let channel = if preset.is_drums() { DRUM_CHANNEL } else { 0 };
        if !preset.is_drums() && preset.bank != 0 {
            // Bank select (controller 0), then the program change.
            synth.process_midi_message(channel, 0xB0, 0, i32::from(preset.bank.min(127)));
        }
        synth.process_midi_message(channel, 0xC0, i32::from(preset.program), 0);
        Ok(Self {
            synth,
            channel,
            sample_rate: rate,
        })
    }

    /// The sample rate the player renders at (Hz).
    pub fn sample_rate(&self) -> f32 {
        self.sample_rate as f32
    }

    /// Starts `pitch` (MIDI note number) at velocity `vel` (1-127; 0 is treated as 1).
    pub fn note_on(&mut self, pitch: u8, vel: u8) {
        let velocity = i32::from(vel.clamp(1, 127));
        self.synth
            .note_on(self.channel, i32::from(pitch.min(127)), velocity);
    }

    /// Releases the sounding `pitch`; it fades out with its release time.
    pub fn note_off(&mut self, pitch: u8) {
        self.synth.note_off(self.channel, i32::from(pitch.min(127)));
    }

    /// Releases every sounding note at once.
    pub fn all_notes_off(&mut self) {
        self.synth.note_off_all(false);
    }

    /// One frame of the instrument's output, `(left, right)`. Does not allocate.
    pub fn render(&mut self) -> (f32, f32) {
        let mut left = [0f32; 1];
        let mut right = [0f32; 1];
        self.synth.render(&mut left, &mut right);
        (left[0], right[0])
    }
}

#[cfg(test)]
pub(crate) mod test_font {
    //! A tiny valid SoundFont built in code, so the tests need no sample file: one sine sample
    //! (440 Hz, one second at 22050 Hz) as one instrument, used by two presets: bank 0 program
    //! 0 ("Test Sine") and bank 128 program 0 ("Test Drums").

    fn chunk(id: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut v = id.to_vec();
        v.extend_from_slice(&(data.len() as u32).to_le_bytes());
        v.extend_from_slice(data);
        if data.len() % 2 == 1 {
            v.push(0);
        }
        v
    }

    fn list(kind: &[u8; 4], parts: &[Vec<u8>]) -> Vec<u8> {
        let mut body = kind.to_vec();
        for p in parts {
            body.extend_from_slice(p);
        }
        chunk(b"LIST", &body)
    }

    fn name20(s: &str) -> [u8; 20] {
        let mut out = [0u8; 20];
        out[..s.len()].copy_from_slice(s.as_bytes());
        out
    }

    /// A generator record: operator and amount.
    fn gen(op: u16, amount: u16) -> Vec<u8> {
        let mut v = op.to_le_bytes().to_vec();
        v.extend_from_slice(&amount.to_le_bytes());
        v
    }

    /// A bag record: index of its first generator and of its first modulator.
    fn bag(gen_index: u16) -> Vec<u8> {
        let mut v = gen_index.to_le_bytes().to_vec();
        v.extend_from_slice(&0u16.to_le_bytes());
        v
    }

    /// The bytes of a valid SoundFont with one sine sample.
    pub fn sine_font() -> Vec<u8> {
        const RATE: u32 = 22_050;
        const LEN: usize = 22_050;
        let smpl: Vec<u8> = (0..LEN)
            .flat_map(|i| {
                let t = i as f32 / RATE as f32;
                ((f32::sin(std::f32::consts::TAU * 440.0 * t) * 12_000.0) as i16).to_le_bytes()
            })
            .collect();

        let info = list(
            b"INFO",
            &[
                chunk(b"ifil", &[2, 0, 1, 0]),
                chunk(b"INAM", b"Test Sine\0"),
            ],
        );
        let sdta = list(b"sdta", &[chunk(b"smpl", &smpl)]);

        // Sample header: name, start, end (inclusive), loop start, loop end, rate, root key,
        // pitch correction, link, type (1 = mono). The sample is not looped.
        let sample = |name: &str, end_sample: u32| {
            let mut v = name20(name).to_vec();
            v.extend_from_slice(&0u32.to_le_bytes());
            v.extend_from_slice(&end_sample.to_le_bytes());
            v.extend_from_slice(&0u32.to_le_bytes());
            v.extend_from_slice(&end_sample.to_le_bytes());
            v.extend_from_slice(&RATE.to_le_bytes());
            v.push(69); // root key A4
            v.push(0); // pitch correction
            v.extend_from_slice(&0u16.to_le_bytes()); // link
            v.extend_from_slice(&1u16.to_le_bytes()); // type: mono
            v
        };
        let mut shdr = sample("sine", LEN as u32 - 1);
        shdr.extend(sample("EOS", 0));

        // Instrument zone: sample 0. Generator 53 is the sample id.
        let igen = [gen(53, 0), gen(0, 0)].concat();
        // Bags: one zone, then the terminal bag (both reference generators 0 and 1).
        let ibag = [bag(0), bag(1)].concat();
        // Instruments: the one instrument, then the terminal record (its bag index is the
        // number of zone bags).
        let mut inst = name20("Test Sine").to_vec();
        inst.extend_from_slice(&0u16.to_le_bytes());
        inst.extend(name20("EOI"));
        inst.extend_from_slice(&1u16.to_le_bytes());

        // Preset zones: each preset uses instrument 0 (generator 41), then the terminal zone.
        let pgen = [gen(41, 0), gen(41, 0), gen(0, 0)].concat();
        let pbag = [bag(0), bag(1), bag(2)].concat();
        let mut phdr = Vec::new();
        for (name, bank, zone) in [("Test Sine", 0u16, 0u16), ("Test Drums", 128u16, 1u16)] {
            phdr.extend_from_slice(&name20(name));
            phdr.extend_from_slice(&0u16.to_le_bytes()); // program number
            phdr.extend_from_slice(&bank.to_le_bytes());
            phdr.extend_from_slice(&zone.to_le_bytes()); // first zone
            phdr.extend_from_slice(&[0u8; 12]); // library, genre, morphology
        }
        // Terminal record: its zone index is the zone count.
        phdr.extend(name20("EOP"));
        phdr.extend_from_slice(&[0u8; 4]); // preset, bank
        phdr.extend_from_slice(&2u16.to_le_bytes()); // zone count
        phdr.extend_from_slice(&[0u8; 12]);

        let pdta = list(
            b"pdta",
            &[
                chunk(b"phdr", &phdr),
                chunk(b"pbag", &pbag),
                chunk(b"pmod", &[0u8; 10]),
                chunk(b"pgen", &pgen),
                chunk(b"inst", &inst),
                chunk(b"ibag", &ibag),
                chunk(b"imod", &[0u8; 10]),
                chunk(b"igen", &igen),
                chunk(b"shdr", &shdr),
            ],
        );

        let mut body = b"sfbk".to_vec();
        body.extend(info);
        body.extend(sdta);
        body.extend(pdta);
        let mut out = b"RIFF".to_vec();
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend(body);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::test_font::sine_font;
    use super::*;

    fn sine_bank() -> SoundFontBank {
        SoundFontBank::load(&sine_font()).unwrap_or_else(|e| panic!("{e}"))
    }

    fn preset(bank: &SoundFontBank, name: &str) -> PresetInfo {
        let Some(p) = bank.presets().iter().find(|p| p.name == name).cloned() else {
            panic!("preset {name}");
        };
        p
    }

    fn energy(player: &mut SfPlayer, frames: usize) -> f64 {
        (0..frames)
            .map(|_| {
                let (l, r) = player.render();
                f64::from(l * l + r * r)
            })
            .sum()
    }

    #[test]
    fn the_presets_of_a_soundfont_are_listed_with_their_banks() {
        let bank = sine_bank();
        let names: Vec<(&str, u16, u8)> = bank
            .presets()
            .iter()
            .map(|p| (p.name.as_str(), p.bank, p.program))
            .collect();
        assert_eq!(names, [("Test Sine", 0, 0), ("Test Drums", 128, 0)]);
        assert!(preset(&bank, "Test Drums").is_drums());
        assert!(!preset(&bank, "Test Sine").is_drums());
    }

    #[test]
    fn a_note_sounds_then_fades_after_note_off() {
        let bank = sine_bank();
        let mut player = SfPlayer::new(&bank, &preset(&bank, "Test Sine"), 48_000.0)
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(energy(&mut player, 512), 0.0, "silent before a note");
        player.note_on(69, 100);
        let loud = energy(&mut player, 4_800);
        assert!(loud > 1.0, "the note sounds: {loud}");
        player.note_off(69);
        // The release fades the note out well within a few seconds.
        let _ = energy(&mut player, 48_000 * 3);
        let tail = energy(&mut player, 4_800);
        assert!(tail < loud * 1e-4, "the note releases: {tail} vs {loud}");
    }

    #[test]
    fn velocity_and_pitch_change_the_sound() {
        let bank = sine_bank();
        let mut soft = SfPlayer::new(&bank, &preset(&bank, "Test Sine"), 48_000.0)
            .unwrap_or_else(|e| panic!("{e}"));
        let mut hard = SfPlayer::new(&bank, &preset(&bank, "Test Sine"), 48_000.0)
            .unwrap_or_else(|e| panic!("{e}"));
        soft.note_on(69, 20);
        hard.note_on(69, 127);
        assert!(energy(&mut hard, 4_800) > energy(&mut soft, 4_800) * 4.0);
    }

    #[test]
    fn drum_kits_play_on_the_drum_channel() {
        let bank = sine_bank();
        let mut drums = SfPlayer::new(&bank, &preset(&bank, "Test Drums"), 48_000.0)
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(drums.channel, DRUM_CHANNEL);
        drums.note_on(36, 100);
        assert!(energy(&mut drums, 4_800) > 1.0);
    }

    #[test]
    fn broken_files_are_errors_not_panics() {
        assert!(SoundFontBank::load(b"").is_err());
        assert!(SoundFontBank::load(b"RIFF\0\0\0\0WAVE").is_err());
        assert!(SoundFontBank::load(&[0u8; 64]).is_err());
        let mut cut = sine_font();
        cut.truncate(cut.len() / 2);
        assert!(SoundFontBank::load(&cut).is_err());
        let too_big = vec![0u8; MAX_SOUNDFONT_BYTES + 1];
        assert!(SoundFontBank::load(&too_big).is_err());
    }

    #[test]
    fn playing_and_rendering_do_not_allocate() {
        let bank = sine_bank();
        let mut player = SfPlayer::new(&bank, &preset(&bank, "Test Sine"), 48_000.0)
            .unwrap_or_else(|e| panic!("{e}"));
        assert_no_alloc::assert_no_alloc(|| {
            player.note_on(60, 90);
            for _ in 0..2_048 {
                let _ = player.render();
            }
            player.note_off(60);
            for _ in 0..2_048 {
                let _ = player.render();
            }
        });
    }

    /// The General MIDI SoundFont named by `PROWAVE_TEST_SF2`, if set (it is not in the repo).
    #[test]
    fn a_general_midi_soundfont_loads_and_plays_if_one_is_provided() {
        let Ok(path) = std::env::var("PROWAVE_TEST_SF2") else {
            return;
        };
        let Ok(bytes) = std::fs::read(&path) else {
            panic!("cannot read {path}");
        };
        let bank = SoundFontBank::load(&bytes).unwrap_or_else(|e| panic!("{e}"));
        assert!(
            bank.presets().len() >= 128,
            "{} presets",
            bank.presets().len()
        );
        let piano = preset(&bank, &bank.presets()[0].name.clone());
        let mut player = SfPlayer::new(&bank, &piano, 48_000.0).unwrap_or_else(|e| panic!("{e}"));
        player.note_on(60, 100);
        assert!(energy(&mut player, 9_600) > 1.0, "the piano sounds");
    }
}
