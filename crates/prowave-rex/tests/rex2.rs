//! End-to-end tests: a file written by `prowave_rex::write` is read back by
//! `RexFile::parse`, and damaged input is refused or reported without a panic.

use prowave_rex::write::{self, RexData};
use prowave_rex::RexFile;

fn tone(frames: usize, freq: f32, rate: u32) -> Vec<i16> {
    (0..frames)
        .map(|i| {
            let t = i as f32 / rate as f32;
            ((t * freq * std::f32::consts::TAU).sin() * 20000.0) as i16
        })
        .collect()
}

#[test]
fn a_mono_loop_round_trips() {
    let samples = tone(8_000, 220.0, 44_100);
    let mut data = RexData::new(44_100, 1, 92.5, samples.clone());
    data.original_tempo = 93.25;
    data.slices = vec![(0, 2_000), (2_000, 2_000), (4_000, 4_000)];
    let bytes = write::write(&data).expect("the loop should write");

    assert!(RexFile::is_rex(&bytes));
    let rex = RexFile::parse(&bytes).expect("the loop should read back");
    assert_eq!(rex.channels, 1);
    assert_eq!(rex.sample_rate, 44_100);
    assert_eq!(rex.bit_depth, 16);
    assert_eq!(rex.frame_count(), samples.len());
    assert_eq!(rex.tempo, Some(92.5));
    assert_eq!(rex.original_tempo, Some(93.25));
    assert!(rex.warnings.is_empty(), "{:?}", rex.warnings);

    for (i, expected) in samples.iter().enumerate() {
        let got = rex.sample(i, 0);
        assert_eq!(got, f32::from(*expected) / 32768.0, "frame {i}");
    }

    assert_eq!(rex.slices.len(), 3);
    assert_eq!(
        rex.slices[0],
        prowave_rex::RexSlice {
            start: 0,
            length: 2_000,
            muted: false,
            locked: false,
            selected: false,
        }
    );
    assert_eq!(rex.slice_range(2), Some((4_000, 8_000)));

    let first = rex.slice_samples(0);
    assert_eq!(first.len(), 2_000);
    assert_eq!(first[7], rex.sample(7, 0));

    let beats = rex.slice_beat(1, 15_360).expect("the loop region is known");
    // The second slice starts a quarter of the way through the waveform.
    assert!((beats - 1.0).abs() < 0.01, "beat position {beats}");
}

#[test]
fn a_stereo_loop_round_trips() {
    let mut samples = Vec::new();
    for i in 0..4_000 {
        let t = i as f32 / 48_000.0;
        samples.push(((t * 330.0 * std::f32::consts::TAU).sin() * 15000.0) as i16);
        samples.push(((t * 440.0 * std::f32::consts::TAU).cos() * 9000.0) as i16);
    }
    let data = RexData::new(48_000, 2, 120.0, samples.clone());
    let bytes = write::write(&data).unwrap();
    let rex = RexFile::parse(&bytes).unwrap();

    assert_eq!(rex.channels, 2);
    assert_eq!(rex.sample_rate, 48_000);
    assert_eq!(rex.frame_count(), 4_000);
    for frame in 0..4_000 {
        assert_eq!(
            rex.sample(frame, 0),
            f32::from(samples[frame * 2]) / 32768.0
        );
        assert_eq!(
            rex.sample(frame, 1),
            f32::from(samples[frame * 2 + 1]) / 32768.0
        );
    }
    assert!((rex.duration_seconds() - 4_000.0 / 48_000.0).abs() < 1e-9);
}

#[test]
fn a_loop_with_no_slices_uses_the_whole_waveform() {
    let data = RexData::new(44_100, 1, 100.0, tone(1_000, 110.0, 44_100));
    let rex = RexFile::parse(&write::write(&data).unwrap()).unwrap();
    assert!(rex.slices.is_empty());
    assert_eq!(rex.frame_count(), 1_000);
}

#[test]
fn a_transient_marker_is_kept_out_of_the_slices() {
    let mut data = RexData::new(44_100, 1, 100.0, tone(2_000, 110.0, 44_100));
    data.slices = vec![(0, 1_000)];
    let mut bytes = write::write(&data).unwrap();
    // Turn the one real slice into a marker: length 1 is the marker encoding.
    let at = find(&bytes, b"SLCE").expect("the slice chunk is there");
    bytes[at + 8 + 4..at + 8 + 8].copy_from_slice(&1u32.to_be_bytes());
    let rex = RexFile::parse(&bytes).unwrap();
    assert!(rex.slices.is_empty());
    assert_eq!(rex.markers, vec![0]);
    assert!(rex.warnings.iter().any(|w| w.contains("markers")));
}

#[test]
fn damaged_files_are_refused_or_reported_without_panicking() {
    let samples = tone(3_000, 300.0, 44_100);
    let mut data = RexData::new(44_100, 1, 100.0, samples);
    data.slices = vec![(0, 1_500)];
    let bytes = write::write(&data).unwrap();

    // Not a REX2 file at all.
    assert!(RexFile::parse(b"hello, this is not a loop").is_err());
    assert!(!RexFile::is_rex(b"RIFF...."));
    // Every prefix either parses or says why.
    for cut in (0..bytes.len()).step_by(97) {
        let _ = RexFile::parse(&bytes[..cut]);
    }
    // Random bytes, including headers that claim to be a REX2 file.
    let mut seed = 0x1234_5678u32;
    for _ in 0..2_000 {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let len = (seed as usize >> 12) % 512;
        let mut junk: Vec<u8> = (0..len)
            .map(|i| (i as u32).wrapping_mul(seed) as u8)
            .collect();
        if junk.len() >= 12 {
            junk[0..4].copy_from_slice(b"CAT ");
            junk[8..12].copy_from_slice(b"REX2");
        }
        let _ = RexFile::parse(&junk);
    }
}

#[test]
fn writing_refuses_bad_input() {
    assert!(write::write(&RexData::new(44_100, 3, 120.0, vec![0; 10])).is_err());
    assert!(write::write(&RexData::new(0, 1, 120.0, vec![0; 10])).is_err());
    assert!(write::write(&RexData::new(44_100, 1, 0.0, vec![0; 10])).is_err());
    assert!(write::write(&RexData::new(44_100, 1, 120.0, vec![])).is_err());
    let mut data = RexData::new(44_100, 1, 120.0, vec![0; 10]);
    data.slices = vec![(5, 10)];
    assert!(write::write(&data).is_err());
}

/// The payload offset of the first chunk with `id`.
fn find(bytes: &[u8], id: &[u8; 4]) -> Option<usize> {
    let mut i = 0;
    while i + 8 <= bytes.len() {
        if &bytes[i..i + 4] == id {
            return Some(i);
        }
        i += 1;
    }
    None
}
