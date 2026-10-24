//! Smart Tempo on files: the tempo and onsets of a WAV on disk, and a file that went missing.
use prowave_engine::AudioFile;
use std::io::Write;
use std::path::Path;

const RATE: u32 = 44_100;

/// A stereo 16-bit WAV with a noise burst on every beat of `bpm`.
fn write_clicks(path: &Path, bpm: f64, seconds: f64) -> std::io::Result<()> {
    let frames = (seconds * f64::from(RATE)) as usize;
    let data_len = (frames * 4) as u32;
    let mut b = Vec::with_capacity(44 + data_len as usize);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data_len).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&RATE.to_le_bytes());
    b.extend_from_slice(&(RATE * 4).to_le_bytes());
    b.extend_from_slice(&4u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data_len.to_le_bytes());
    let beat = (60.0 / bpm * f64::from(RATE)) as usize;
    let mut seed = 1u32;
    for f in 0..frames {
        let into = (f + beat - 9000) % beat;
        let v = if into < 500 {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let n = (seed >> 16) as i32 - 32_768;
            (n as f32 * 0.5 * (-(into as f32) / 120.0).exp()) as i16
        } else {
            0
        };
        b.extend_from_slice(&v.to_le_bytes());
        b.extend_from_slice(&v.to_le_bytes());
    }
    std::fs::File::create(path)?.write_all(&b)
}

#[test]
fn the_tempo_of_a_file_is_found() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    for bpm in [90.0, 124.0, 160.0] {
        let path = dir.path().join(format!("clicks{bpm}.wav"));
        write_clicks(&path, bpm, 16.0)?;
        let found = AudioFile::open(&path)?.analyze()?;
        let tempo = found.tempo.ok_or("no tempo")?;
        assert!((tempo.bpm - bpm).abs() < 1.0, "{bpm}: {}", tempo.bpm);
        assert!(found.onsets.len() >= (16.0 * bpm / 60.0) as usize - 2);
        assert!(found.onsets.windows(2).all(|w| w[0] < w[1]), "in order");
    }
    Ok(())
}

#[test]
fn a_file_that_cannot_be_read_is_an_error_not_a_panic() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("c.wav");
    write_clicks(&path, 120.0, 4.0)?;
    let file = AudioFile::open(&path)?;
    std::fs::remove_file(&path)?;
    assert!(file.analyze().is_err());
    Ok(())
}
