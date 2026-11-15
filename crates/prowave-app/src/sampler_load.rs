//! Building the Sampler's zones for a track (Logic: loading an instrument into the Sampler):
//! from an SFZ instrument, an EXS instrument, or one audio file played across the keyboard (the
//! Quick Sampler's way: drop a sound in and play it).
//!
//! Reading the instrument and decoding its samples takes a while, so it runs on a background
//! thread (see `engine_sync.rs`); the finished [`Sampler`] is handed to the engine. A sample
//! that is missing is skipped (and counted in the error text when none could be loaded); an
//! instrument that names more audio than [`MAX_TOTAL_FRAMES`] stops there, so a wrong file
//! cannot use unbounded memory. SFZ and EXS parsing is done by `prowave-sfz` and `prowave-exs`,
//! which refuse sample paths that leave the instrument's folder.

use prowave_engine::sample_file::{read_sample, SampleData};
use prowave_engine::sampler::{LoopMode, Zone};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

/// The most sample frames one instrument may load in all (8 bytes each: 256 MB).
pub const MAX_TOTAL_FRAMES: usize = 32 * 1024 * 1024;

/// Where a track's Sampler gets its sounds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SamplerSource {
    /// An SFZ instrument file (its samples are in its folder).
    Sfz(PathBuf),
    /// An EXS instrument file (its samples are found by file name in its folder, or the
    /// `Samples` folder next to it).
    Exs(PathBuf),
    /// One audio file played on every key: `root` is the key it sounds at, `looped` repeats it
    /// for as long as the key is held.
    Audio {
        path: PathBuf,
        root: u8,
        looped: bool,
    },
}

impl SamplerSource {
    /// The file the sounds come from.
    pub fn path(&self) -> &Path {
        match self {
            Self::Sfz(p) | Self::Exs(p) => p,
            Self::Audio { path, .. } => path,
        }
    }

    /// A name for the Inspector: the file's name.
    pub fn name(&self) -> String {
        self.path()
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
    }

    /// The kind as the project file writes it.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Sfz(_) => "sfz",
            Self::Exs(_) => "exs",
            Self::Audio { .. } => "audio",
        }
    }
}

/// Reads decoded samples once each, within the memory limit.
struct Loader {
    cache: HashMap<PathBuf, Option<Arc<SampleData>>>,
    frames: usize,
    missing: usize,
}

impl Loader {
    fn new() -> Self {
        Self {
            cache: HashMap::new(),
            frames: 0,
            missing: 0,
        }
    }

    /// The sample at `path`, or `None` (counted as missing) when it cannot be read or the
    /// memory limit is reached.
    fn get(&mut self, path: &Path) -> Option<Arc<SampleData>> {
        if let Some(known) = self.cache.get(path) {
            return known.clone();
        }
        let loaded = match read_sample(path) {
            Ok(s) if self.frames + s.frames.len() <= MAX_TOTAL_FRAMES => {
                self.frames += s.frames.len();
                Some(Arc::new(s))
            }
            Ok(_) => {
                tracing::warn!("sampler: {} would pass the memory limit", path.display());
                None
            }
            Err(e) => {
                tracing::warn!("sampler: {e}");
                None
            }
        };
        if loaded.is_none() {
            self.missing += 1;
        }
        self.cache.insert(path.to_path_buf(), loaded.clone());
        loaded
    }
}

/// The zones for `source`. An error (the instrument cannot be read, or none of its samples
/// could be loaded) is returned as text; it is also remembered for [`error`].
pub fn build_zones(source: &SamplerSource) -> Result<Vec<Zone>, String> {
    let result = match source {
        SamplerSource::Sfz(path) => zones_from_sfz(path),
        SamplerSource::Exs(path) => zones_from_exs(path),
        SamplerSource::Audio { path, root, looped } => zone_from_audio(path, *root, *looped),
    };
    set_error(source.path(), result.as_ref().err().cloned());
    result
}

fn zone_from_audio(path: &Path, root: u8, looped: bool) -> Result<Vec<Zone>, String> {
    let sample = Arc::new(read_sample(path)?);
    let mut zone = Zone::new(sample);
    zone.root_key = root.min(127);
    if looped {
        zone.loop_mode = LoopMode::Continuous;
    }
    // A short fade-out when the key is let go; the Release dial adds to it.
    zone.release = 0.02;
    Ok(vec![zone])
}

fn zones_from_sfz(path: &Path) -> Result<Vec<Zone>, String> {
    let instrument = prowave_sfz::load(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let dir = path.parent().map_or_else(PathBuf::new, Path::to_path_buf);
    let mut loader = Loader::new();
    let mut zones = Vec::new();
    for r in &instrument.regions {
        // SFZ's built-in generators (`*sine`) name no file.
        if r.sample.starts_with('*') {
            continue;
        }
        let Some(sample) = loader.get(&dir.join(&r.sample)) else {
            continue;
        };
        let len = sample.frames.len();
        let mut z = Zone::new(sample);
        z.key_low = r.key_low;
        z.key_high = r.key_high;
        z.vel_low = r.vel_low;
        z.vel_high = r.vel_high;
        z.root_key = r.root_key;
        z.tune_cents = f32::from(r.transpose) * 100.0 + r.tune_cents;
        z.volume_db = r.volume_db;
        z.pan = r.pan_percent / 100.0;
        z.loop_mode = match r.loop_mode {
            prowave_sfz::LoopMode::NoLoop => LoopMode::NoLoop,
            prowave_sfz::LoopMode::OneShot => LoopMode::OneShot,
            prowave_sfz::LoopMode::Continuous => LoopMode::Continuous,
            prowave_sfz::LoopMode::Sustain => LoopMode::Sustain,
        };
        z.loop_start = r.loop_start.map_or(0, |v| v as usize).min(len);
        z.loop_end = r
            .loop_end
            .map_or(len, |v| (v as usize).saturating_add(1))
            .min(len);
        z.attack = r.amp.attack;
        z.hold = r.amp.hold;
        z.decay = r.amp.decay;
        z.sustain = r.amp.sustain;
        z.release = r.amp.release;
        z.seq_length = r.seq_length;
        z.seq_position = r.seq_position;
        z.group = r.group;
        z.off_by = r.off_by;
        zones.push(z);
    }
    finish(zones, &loader, path)
}

fn zones_from_exs(path: &Path) -> Result<Vec<Zone>, String> {
    let instrument = prowave_exs::load(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let dir = path.parent().map_or_else(PathBuf::new, Path::to_path_buf);
    let mut loader = Loader::new();
    let mut zones = Vec::new();
    for z in &instrument.zones {
        let Some(sample) = z.sample.and_then(|i| instrument.samples.get(i)) else {
            continue;
        };
        if !sample.file_name_is_safe() {
            continue;
        }
        // EXS stores an absolute path of the machine it was made on: the file is looked for by
        // its name next to the instrument, then in a `Samples` folder there.
        let found = [
            dir.join(&sample.file_name),
            dir.join("Samples").join(&sample.file_name),
        ]
        .into_iter()
        .find(|p| p.is_file());
        let Some(data) = found.and_then(|p| loader.get(&p)) else {
            loader.missing += 1;
            continue;
        };
        let group = z.group.and_then(|g| instrument.groups.get(g));
        zones.push(exs_zone(z, group, data));
    }
    finish(zones, &loader, path)
}

/// One EXS zone as a Sampler zone (the sample is already loaded).
fn exs_zone(
    z: &prowave_exs::Zone,
    group: Option<&prowave_exs::Group>,
    sample: Arc<SampleData>,
) -> Zone {
    let len = sample.frames.len();
    let mut out = Zone::new(sample);
    out.key_low = z.key_low;
    out.key_high = z.key_high;
    out.vel_low = z.vel_low;
    out.vel_high = z.vel_high;
    out.root_key = z.root_key;
    out.tune_cents = f32::from(z.coarse_tuning) * 100.0 + f32::from(z.fine_tuning);
    out.volume_db = f32::from(z.volume_adjust);
    out.pan = f32::from(z.pan) / 64.0;
    // Zones that do not follow the keyboard's pitch play every key at the root's pitch.
    out.pitch_track = z.pitch_tracking;
    out.start = (z.sample_start as usize).min(len);
    out.end = if z.sample_end == 0 {
        len
    } else {
        (z.sample_end as usize).min(len)
    };
    if out.end <= out.start {
        out.end = len;
        out.start = 0;
    }
    out.loop_mode = if z.one_shot {
        LoopMode::OneShot
    } else if z.loop_enabled {
        if z.loop_play_to_end_on_release {
            LoopMode::Sustain
        } else {
            LoopMode::Continuous
        }
    } else {
        LoopMode::NoLoop
    };
    out.loop_start = (z.loop_start as usize).min(len);
    out.loop_end = if z.loop_end == 0 {
        out.end
    } else {
        (z.loop_end as usize).saturating_add(1).min(len)
    };
    // An exclusive group is a choke group: a new note cuts the others of its group.
    if let Some(g) = group {
        if g.exclusive > 0 {
            out.group = u32::from(g.exclusive);
            out.off_by = u32::from(g.exclusive);
        }
    }
    out.release = 0.02;
    out
}

/// The zones, or an error saying why there are none.
fn finish(zones: Vec<Zone>, loader: &Loader, path: &Path) -> Result<Vec<Zone>, String> {
    if zones.is_empty() {
        return Err(if loader.missing > 0 {
            format!(
                "{}: none of its {} samples could be loaded",
                path.display(),
                loader.missing
            )
        } else {
            format!("{}: the instrument has no samples", path.display())
        });
    }
    Ok(zones)
}

// ---------------------------------------------------------------------------------------------
// What went wrong, for the Inspector.

fn errors() -> MutexGuard<'static, HashMap<PathBuf, String>> {
    static ERRORS: OnceLock<Mutex<HashMap<PathBuf, String>>> = OnceLock::new();
    ERRORS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn set_error(path: &Path, error: Option<String>) {
    let mut map = errors();
    match error {
        Some(e) => {
            map.insert(path.to_path_buf(), e);
        }
        None => {
            map.remove(path);
        }
    }
}

/// Why the instrument at `path` could not be loaded the last time it was tried.
pub fn error(path: &Path) -> Option<String> {
    errors().get(path).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(frames: usize, level: f32) -> Vec<u8> {
        let data = frames as u32 * 4;
        let mut v = b"RIFF".to_vec();
        v.extend_from_slice(&(36 + data).to_le_bytes());
        v.extend_from_slice(b"WAVEfmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&3u16.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes());
        v.extend_from_slice(&48_000u32.to_le_bytes());
        v.extend_from_slice(&(48_000u32 * 4).to_le_bytes());
        v.extend_from_slice(&4u16.to_le_bytes());
        v.extend_from_slice(&32u16.to_le_bytes());
        v.extend_from_slice(b"data");
        v.extend_from_slice(&data.to_le_bytes());
        for _ in 0..frames {
            v.extend_from_slice(&level.to_le_bytes());
        }
        v
    }

    #[test]
    fn an_audio_file_becomes_one_zone_on_every_key_rooted_where_asked() {
        let dir = tempfile::tempdir().expect("temp dir");
        let file = dir.path().join("pad.wav");
        std::fs::write(&file, wav(4_800, 0.5)).expect("write");
        let zones = build_zones(&SamplerSource::Audio {
            path: file.clone(),
            root: 48,
            looped: true,
        })
        .expect("zones");
        assert_eq!(zones.len(), 1);
        let z = &zones[0];
        assert_eq!(
            (z.key_low, z.key_high, z.vel_low, z.vel_high),
            (0, 127, 0, 127)
        );
        assert_eq!(z.root_key, 48);
        assert_eq!(z.loop_mode, LoopMode::Continuous);
        let plain = build_zones(&SamplerSource::Audio {
            path: file,
            root: 200,
            looped: false,
        })
        .expect("zones");
        assert_eq!(
            plain[0].root_key, 127,
            "a root past the keyboard is the top key"
        );
        assert_eq!(plain[0].loop_mode, LoopMode::NoLoop);
        // A file that is not audio is an error, remembered for the Inspector.
        let junk = dir.path().join("junk.wav");
        std::fs::write(&junk, b"no").expect("write");
        let src = SamplerSource::Audio {
            path: junk.clone(),
            root: 60,
            looped: false,
        };
        assert!(build_zones(&src).is_err());
        assert!(error(&junk).is_some());
    }

    #[test]
    fn an_sfz_instrument_becomes_zones_with_its_ranges_loops_envelope_and_round_robin() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("a.wav"), wav(4_800, 0.4)).expect("write");
        std::fs::write(dir.path().join("b.wav"), wav(2_400, 0.2)).expect("write");
        let sfz = "<group> ampeg_attack=0.1 ampeg_release=0.5 seq_length=2\n\
                   <region> sample=a.wav lokey=36 hikey=47 pitch_keycenter=40 transpose=2 tune=10 \
                   volume=-3 pan=-50 loop_mode=loop_sustain loop_start=100 loop_end=999 seq_position=1\n\
                   <region> sample=a.wav lokey=36 hikey=47 pitch_keycenter=40 seq_position=2\n\
                   <region> sample=b.wav key=60 lovel=80 loop_mode=one_shot group=3 off_by=4\n\
                   <region> sample=*sine key=70\n\
                   <region> sample=missing.wav key=71\n";
        let path = dir.path().join("kit.sfz");
        std::fs::write(&path, sfz).expect("write");
        let zones = build_zones(&SamplerSource::Sfz(path)).expect("zones");
        assert_eq!(
            zones.len(),
            3,
            "the generator and the missing sample are skipped"
        );
        let z = &zones[0];
        assert_eq!((z.key_low, z.key_high, z.root_key), (36, 47, 40));
        assert_eq!(
            z.tune_cents, 210.0,
            "transpose 2 semitones and tune 10 cents"
        );
        assert_eq!((z.volume_db, z.pan), (-3.0, -0.5));
        assert_eq!(z.loop_mode, LoopMode::Sustain);
        assert_eq!(
            (z.loop_start, z.loop_end),
            (100, 1000),
            "SFZ's loop_end is the last frame"
        );
        assert_eq!((z.attack, z.release), (0.1, 0.5));
        assert_eq!((z.seq_length, z.seq_position), (2, 1));
        assert_eq!(zones[1].seq_position, 2);
        assert!(
            std::sync::Arc::ptr_eq(&zones[0].sample, &zones[1].sample),
            "one decode for a sample used twice"
        );
        let drum = &zones[2];
        assert_eq!(
            (drum.vel_low, drum.loop_mode, drum.group, drum.off_by),
            (80, LoopMode::OneShot, 3, 4)
        );
        // Nothing loadable is an error that says why.
        let empty = dir.path().join("empty.sfz");
        std::fs::write(&empty, "<region> sample=nothing.wav key=60\n").expect("write");
        let e = build_zones(&SamplerSource::Sfz(empty)).unwrap_err();
        assert!(e.contains("none of its"), "{e}");
        // An instrument that leaves its folder is refused by the reader.
        let sneaky = dir.path().join("sneaky.sfz");
        std::fs::write(&sneaky, "<region> sample=../../etc/passwd key=60\n").expect("write");
        assert!(build_zones(&SamplerSource::Sfz(sneaky)).is_err());
        assert!(build_zones(&SamplerSource::Sfz(dir.path().join("none.sfz"))).is_err());
    }

    fn exs_zone_literal() -> prowave_exs::Zone {
        prowave_exs::Zone {
            key_low: 36,
            key_high: 72,
            vel_low: 1,
            vel_high: 127,
            root_key: 48,
            fine_tuning: 12,
            coarse_tuning: -2,
            pan: 32,
            volume_adjust: -6,
            one_shot: false,
            pitch_tracking: true,
            reverse: false,
            sample_start: 100,
            sample_end: 0,
            loop_start: 10,
            loop_end: 50,
            loop_enabled: true,
            loop_equal_power: false,
            loop_play_to_end_on_release: false,
            loop_direction: prowave_exs::LoopDirection::Forward,
            group: Some(0),
            sample: Some(0),
        }
    }

    #[test]
    fn an_exs_zone_maps_its_tuning_pan_loop_trim_and_exclusive_group() {
        let sample = Arc::new(SampleData::new(vec![[0.1, 0.1]; 1000], 48_000));
        let group = prowave_exs::Group {
            volume: 0,
            pan: 0,
            polyphony: 0,
            exclusive: 5,
            vel_low: 0,
            vel_high: 127,
            release_trigger_time: 0,
            round_robin_position: None,
            start_note: 0,
            end_note: 127,
        };
        let z = exs_zone(&exs_zone_literal(), Some(&group), sample.clone());
        assert_eq!((z.key_low, z.key_high, z.root_key), (36, 72, 48));
        assert_eq!(z.tune_cents, -188.0, "-2 semitones and +12 cents");
        assert_eq!((z.volume_db, z.pan), (-6.0, 0.5));
        assert_eq!(
            (z.start, z.end),
            (100, 1000),
            "an end of 0 is the whole sample"
        );
        assert_eq!(z.loop_mode, LoopMode::Continuous);
        assert_eq!((z.loop_start, z.loop_end), (10, 51));
        assert_eq!((z.group, z.off_by), (5, 5));
        assert!(z.pitch_track);
        // One-shot and play-to-end-on-release; a bad trim falls back to the whole sample.
        let mut odd = exs_zone_literal();
        odd.one_shot = true;
        odd.sample_start = 900;
        odd.sample_end = 20;
        let o = exs_zone(&odd, None, sample.clone());
        assert_eq!(o.loop_mode, LoopMode::OneShot);
        assert_eq!((o.start, o.end), (0, 1000));
        assert_eq!((o.group, o.off_by), (0, 0));
        let mut tail = exs_zone_literal();
        tail.loop_play_to_end_on_release = true;
        assert_eq!(exs_zone(&tail, None, sample).loop_mode, LoopMode::Sustain);
    }

    #[test]
    fn a_missing_exs_is_an_error_and_names_come_from_the_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        assert!(build_zones(&SamplerSource::Exs(dir.path().join("none.exs"))).is_err());
        let junk = dir.path().join("junk.exs");
        std::fs::write(&junk, b"this is not an exs file at all").expect("write");
        assert!(build_zones(&SamplerSource::Exs(junk.clone())).is_err());
        let src = SamplerSource::Exs(junk);
        assert_eq!(src.name(), "junk.exs");
        assert_eq!(src.kind(), "exs");
        assert_eq!(SamplerSource::Sfz("/x/a.sfz".into()).kind(), "sfz");
    }

    #[test]
    fn an_sfz_instrument_plays_end_to_end_through_the_sampler_at_the_right_pitch() {
        use prowave_engine::sampler::{defaults, Sampler};
        let dir = tempfile::tempdir().expect("temp dir");
        // A 440 Hz sine, 1 second at 48 kHz, as the sample of the instrument.
        let mut data = Vec::new();
        for i in 0..48_000 {
            let v = (std::f32::consts::TAU * 440.0 * i as f32 / 48_000.0).sin() * 0.8;
            data.extend_from_slice(&v.to_le_bytes());
        }
        let mut w = wav(0, 0.0);
        w.truncate(44);
        w[4..8].copy_from_slice(&(36 + data.len() as u32).to_le_bytes());
        w[40..44].copy_from_slice(&(data.len() as u32).to_le_bytes());
        w.extend_from_slice(&data);
        std::fs::write(dir.path().join("tone.wav"), w).expect("write");
        let path = dir.path().join("tone.sfz");
        std::fs::write(
            &path,
            "<region> sample=tone.wav pitch_keycenter=69 lokey=0 hikey=127\n",
        )
        .expect("write");
        let zones = build_zones(&SamplerSource::Sfz(path)).expect("zones");
        let mut sampler = Sampler::new(zones, 48_000.0, defaults());
        sampler.note_on(81, 100); // an octave above the root
        let x: Vec<f32> = (0..24_000).map(|_| sampler.render().0).collect();
        // Count rising zero crossings: 880 Hz over 0.5 s is about 440 cycles.
        let cycles = x[2_000..]
            .windows(2)
            .filter(|w| w[0] <= 0.0 && w[1] > 0.0)
            .count() as f32
            / (22_000.0 / 48_000.0);
        assert!((cycles - 880.0).abs() < 12.0, "{cycles} Hz");
    }
}
