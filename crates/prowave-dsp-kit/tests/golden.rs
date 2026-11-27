//! The port against the original: every case of `golden/cases.txt` is run through a [`Chain`]
//! built from the same command-line words, and the result is compared with what the original
//! `dsp` program wrote (`golden/gen.sh` regenerates the files; it needs the original's source).
//!
//! The original writes 32-bit floats and clips them to -1..1 in its output stage, so the
//! comparison clips the same way and allows what the float rounding costs.

use prowave_dsp_kit::{Chain, Stream};
use std::path::PathBuf;

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

fn read_f32(path: &std::path::Path) -> Vec<f64> {
    let bytes = std::fs::read(path).unwrap_or_default();
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| f64::from(f32::from_le_bytes(*c)))
        .collect()
}

#[test]
fn every_case_matches_the_original_program_sample_by_sample() {
    let input = read_f32(&dir().join("input.f32"));
    assert_eq!(input.len(), 4800 * 2);
    // `long_*` cases use a longer signal with note-like bursts (for the decoders).
    let long_input = read_f32(&dir().join("long.f32"));
    assert_eq!(long_input.len(), 28800 * 2);
    // `spaced_*` cases use a signal whose channels are out of step (for the multiband decoder).
    let spaced_input = read_f32(&dir().join("spaced.f32"));
    assert_eq!(spaced_input.len(), 48000 * 2);
    let cases = std::fs::read_to_string(dir().join("cases.txt")).unwrap_or_default();
    let mut checked = 0;
    let mut problems = Vec::new();
    for line in cases
        .lines()
        .filter(|l| !l.starts_with('#') && l.contains('|'))
    {
        let mut fields = line.splitn(3, '|');
        let name = fields.next().unwrap_or_default().trim();
        let fs: u32 = fields
            .next()
            .unwrap_or_default()
            .trim()
            .parse()
            .unwrap_or(48_000);
        let words = fields.next().unwrap_or_default().trim();
        // Every case starts like a run of the original program: with fresh random sequences.
        prowave_dsp_kit::reset_random_seeds();
        // `GOLDEN_ONLY=name` runs just one case.
        if std::env::var("GOLDEN_ONLY").is_ok_and(|only| only != name) {
            continue;
        }
        let argv: Vec<&str> = words.split_whitespace().collect();
        let want = read_f32(&dir().join(format!("{name}.f32")));
        let mut chain = match Chain::from_args(Stream { fs, channels: 2 }, &argv) {
            Ok(c) => c,
            Err(e) => {
                problems.push(format!("{name}: {e}"));
                continue;
            }
        };
        let out = chain.process(if name.starts_with("long_") {
            &long_input
        } else if name.starts_with("spaced_") {
            &spaced_input
        } else {
            &input
        });
        // `stats_*` cases: the tables the original printed.
        if let Ok(text) = std::fs::read_to_string(dir().join(format!("{name}.txt"))) {
            let got_text: String = chain
                .reports()
                .into_iter()
                .filter_map(|r| match r {
                    prowave_dsp_kit::Report::Stats(s) => Some(s.text),
                    _ => None,
                })
                .collect();
            if got_text != text {
                problems.push(format!(
                    "{name}: statistics differ:\n{got_text}\n--- original ---\n{text}"
                ));
            }
        }
        let got: Vec<f64> = out.iter().map(|v| v.clamp(-1.0, 1.0)).collect();
        if got.len() != want.len() {
            problems.push(format!(
                "{name}: {} samples, original {}",
                got.len(),
                want.len()
            ));
            continue;
        }
        let worst = got
            .iter()
            .zip(&want)
            .map(|(a, b)| (a - b).abs() / (1.0 + b.abs()))
            .fold(0.0f64, f64::max);
        // The multiband decoder's event decisions hinge on comparisons that the Windows math
        // library, which rounds `exp`/`pow`/`sin` a little differently from the one the
        // fixtures were made with, can flip by a sample; the output then differs by a few
        // thousandths. Linux and macOS match to the last digit.
        let tol = if cfg!(windows) && name.contains("matrix4_mb") {
            2e-2
        } else {
            2e-6
        };
        if worst > tol {
            problems.push(format!("{name}: differs from the original by {worst:e}"));
        }
        checked += 1;
    }
    assert!(problems.is_empty(), "{problems:#?}");
    assert!(
        checked >= 100 || std::env::var("GOLDEN_ONLY").is_ok(),
        "only {checked} cases"
    );
}

#[test]
fn the_block_size_does_not_change_the_sound() {
    // The original processes 2048 frames at a time; any other block size must give the same
    // samples because the filters keep their memory between blocks.
    let input = read_f32(&dir().join("input.f32"));
    let s = Stream {
        fs: 48_000,
        channels: 2,
    };
    let words = [
        "lowpass",
        "1k",
        "0.7q",
        "eq",
        "300",
        "1q",
        "6",
        "st2ms",
        "sinc",
        "-c",
        "2k",
        "fir_p",
        "-a",
        "coefs:0.1,0.5,0.9,0.5,0.1",
    ];
    let mut a = Chain::from_args(s, &words).unwrap_or_else(|e| panic!("{e}"));
    let mut b = Chain::from_args(s, &words).unwrap_or_else(|e| panic!("{e}"));
    b.set_block_frames(37);
    assert_eq!(a.run(&input), b.run(&input));
}
