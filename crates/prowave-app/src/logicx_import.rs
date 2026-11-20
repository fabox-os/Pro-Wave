//! Logic Pro `.logicx` metadata preview: File > Import > LogicX Project (Preview)…
//!
//! `prowave-logicx-import` validates the bundle and reads its metadata. It does not decode Logic's
//! closed `ProjectData` arrangement yet, so this entry deliberately creates no tracks: it reports
//! the project's tempo, key, signature, sample rate and alternative in the status line, and
//! applies the tempo and time signature to an empty project as a starting point.

use crate::model::Project;
use std::path::Path;

/// Whether a dropped or command-line path names a Logic project bundle.
pub fn is_supported_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("logicx"))
}

/// Reads a `.logicx` package and returns a status line describing what Logic wrote.
pub fn preview(p: &mut Project, path: &Path) -> Result<String, String> {
    let info = prowave_logicx_import::read(path).map_err(|e| e.to_string())?;
    if p.tracks.iter().all(|t| t.regions.is_empty()) {
        if info.tempo.is_finite() && info.tempo > 0.0 {
            p.tempo = info.tempo.clamp(
                f64::from(crate::model::MIN_BPM),
                f64::from(crate::model::MAX_BPM),
            ) as f32;
            p.tempo_changes.clear();
        }
        if let Some(sig) = info.time_signature.as_deref().and_then(parse_signature) {
            p.sig = sig;
            p.sig_changes.clear();
        }
    }
    let key = info.key_signature.as_deref().unwrap_or("unknown");
    let signature = info.time_signature.as_deref().unwrap_or("unknown");
    let rate = info
        .sample_rate
        .map(|r| format!("{} Hz", r.round() as u64))
        .unwrap_or_else(|| "unknown rate".into());
    Ok(format!(
        "LogicX {} (Preview): {} BPM, {}, {}, {}, {} channels, alternative {}, {} bytes of ProjectData; arrangement not imported",
        info.name,
        info.tempo.round() as u64,
        key,
        signature,
        rate,
        info.channels.unwrap_or(0),
        info.alternative,
        info.project_data_bytes,
    ))
}

/// Logic writes signatures as `4/4` or `12/8`.
fn parse_signature(text: &str) -> Option<(u8, u8)> {
    let (num, den) = text.trim().split_once('/')?;
    let num = num.trim().parse::<u8>().ok()?;
    let den = den.trim().parse::<u8>().ok()?;
    (num > 0 && den > 0).then_some((num, den))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn reads_metadata_without_making_tracks() {
        const HEADER: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
        <!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
        "#;
        const MAGIC: [u8; 4] = [0x23, 0x47, 0xC0, 0xAB];
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "prowave-logicx-test-{}-{nanos}",
            std::process::id()
        ));
        let bundle = dir.join("Song.logicx");
        let alt = bundle.join("Alternatives/000");
        fs::create_dir_all(&alt).expect("the alternative directory");
        fs::create_dir_all(bundle.join("Resources")).expect("the resources directory");
        fs::write(
            alt.join("MetaData.plist"),
            format!(
                "{HEADER}<plist version=\"1.0\"><dict><key>tempo</key><real>96</real><key>keySignature</key><string>C Major</string><key>timeSignature</key><string>3/4</string><key>sampleRate</key><real>48000</real><key>channelCount</key><integer>2</integer></dict></plist>"
            ),
        )
        .expect("MetaData.plist");
        fs::write(
            bundle.join("Resources/ProjectInformation.plist"),
            format!(
                "{HEADER}<plist version=\"1.0\"><dict><key>activeAlternative</key><integer>0</integer><key>logicVersion</key><string>11.0</string></dict></plist>"
            ),
        )
        .expect("ProjectInformation.plist");
        fs::write(alt.join("ProjectData"), [MAGIC, [0; 4]].concat()).expect("ProjectData");

        let mut p = Project::empty();
        let status = preview(&mut p, &bundle).expect("preview the project");
        let _ = fs::remove_dir_all(&dir);

        assert!(status.contains("Song (Preview)"));
        assert!(status.contains("arrangement not imported"));
        assert_eq!(p.tempo, 96.0);
        assert_eq!(p.sig, (3, 4));
        assert!(p.tracks.iter().all(|t| t.regions.is_empty()));
    }

    #[test]
    fn signatures_are_parsed_defensively() {
        assert_eq!(parse_signature("4/4"), Some((4, 4)));
        assert_eq!(parse_signature(" 12 / 8 "), Some((12, 8)));
        assert_eq!(parse_signature("4"), None);
        assert_eq!(parse_signature("0/4"), None);
        assert_eq!(parse_signature("4/0"), None);
    }
}
