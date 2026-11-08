//! Hydrogen drum kits: a `drumkit.xml` and its WAV (or AIFF, FLAC, OGG) samples, turned into a
//! SoundFont so the engine plays them with the same player as any `.sf2` instrument.
//!
//! Hydrogen (<https://github.com/hydrogen-music>) is a free drum machine whose kits are a folder
//! with a `drumkit.xml` naming each instrument's MIDI note and its sample layers. Every layer
//! becomes a zone of the kit's sound: its instrument's note plays its sample, with the layer's
//! velocity range and gain.
//!
//! Reading, decoding and building allocate; call them off the audio thread.

use std::path::{Path, PathBuf};

use crate::sf2::{self, Preset, Sample, Zone};

/// The largest `drumkit.xml` accepted, in bytes.
const MAX_XML_BYTES: usize = 4 * 1024 * 1024;
/// The longest sample accepted, in seconds.
const MAX_SAMPLE_SECONDS: f32 = 30.0;
/// The deepest nesting accepted in the `drumkit.xml`.
const MAX_DEPTH: usize = 64;
/// The most sample layers a kit may have.
const MAX_ZONES: usize = 4096;
/// The most sample frames a kit may hold (about eight minutes at 48 kHz).
const MAX_TOTAL_FRAMES: usize = 48_000 * 60 * 8;
/// The first note a kit without `midi_out_note` numbers its instruments from (C1, Hydrogen's
/// first drum note).
const FIRST_DRUM_NOTE: u8 = 36;
/// The SoundFont bank of drum kits (General MIDI's percussion bank).
const DRUM_BANK: u16 = 128;

/// One sample layer of an instrument: its file, its velocity range (0 to 1) and its gain.
#[derive(Clone, Debug, PartialEq)]
pub struct Layer {
    pub file: String,
    pub velocity: (f32, f32),
    pub gain: f32,
}

/// One instrument of a kit: its name, the MIDI note that plays it and its sample layers.
#[derive(Clone, Debug, PartialEq)]
pub struct Instrument {
    pub name: String,
    pub note: u8,
    pub layers: Vec<Layer>,
}

/// A Hydrogen drum kit as its `drumkit.xml` describes it.
#[derive(Clone, Debug, PartialEq)]
pub struct Drumkit {
    pub name: String,
    pub instruments: Vec<Instrument>,
}

/// Whether `path` is a Hydrogen kit definition (`drumkit.xml`).
pub fn is_drumkit(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.eq_ignore_ascii_case("drumkit.xml"))
}

/// Parses a `drumkit.xml`. Malformed files are an error, never a panic.
pub fn parse(xml: &[u8]) -> Result<Drumkit, String> {
    if xml.len() > MAX_XML_BYTES {
        return Err(format!(
            "the drumkit.xml is larger than {} MB",
            MAX_XML_BYTES >> 20
        ));
    }
    let src = std::str::from_utf8(xml).map_err(|_| "the drumkit.xml is not UTF-8".to_string())?;
    let root = parse_xml(src)?;
    let name = child(&root, "drumkit_info")
        .and_then(|info| child(info, "name"))
        .map(text)
        .filter(|n| !n.is_empty())
        .as_deref()
        .unwrap_or("Hydrogen Drumkit")
        .to_string();

    let mut nodes = Vec::new();
    collect(&root, "instrument", &mut nodes);
    if nodes.is_empty() {
        return Err("the drumkit.xml has no <instrument>".to_string());
    }
    let mut instruments: Vec<Instrument> = Vec::new();
    for (index, node) in nodes.iter().enumerate() {
        let name = child(node, "name")
            .map(text)
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| format!("Instrument {}", index + 1));
        // Hydrogen 1.x names the note; older kits number their instruments from C1.
        let note = child(node, "midi_out_note")
            .and_then(|n| text(n).parse::<i32>().ok())
            .filter(|n| (0..=127).contains(n))
            .map_or_else(
                || (usize::from(FIRST_DRUM_NOTE) + index).min(127) as u8,
                |n| n as u8,
            );
        let mut layers = Vec::new();
        let mut layer_nodes = Vec::new();
        collect(node, "layer", &mut layer_nodes);
        for layer in layer_nodes {
            let Some(file) = child(layer, "filename").map(text) else {
                continue;
            };
            if file.is_empty() {
                continue;
            }
            let min = number(layer, "min").unwrap_or(0.0);
            let max = number(layer, "max").unwrap_or(1.0);
            let gain = number(layer, "gain").unwrap_or(1.0);
            layers.push(Layer {
                file: file.to_string(),
                velocity: (min.clamp(0.0, 1.0), max.clamp(0.0, 1.0)),
                gain: gain.clamp(0.0, 1.0),
            });
        }
        if layers.is_empty() {
            continue;
        }
        if instruments.iter().any(|i| i.note == note) {
            // Two instruments on one note would play together; the first wins, as in Hydrogen.
            continue;
        }
        instruments.push(Instrument { name, note, layers });
    }
    if instruments.is_empty() {
        return Err("the drumkit.xml names no sample".to_string());
    }
    Ok(Drumkit { name, instruments })
}

/// Builds the SoundFont bytes of the kit defined by `xml_path`, reading the samples from the
/// same folder. Allocates and reads files; call off the audio thread.
pub fn soundfont_bytes(xml_path: &Path) -> Result<Vec<u8>, String> {
    let xml = std::fs::read(xml_path).map_err(|e| format!("{}: {e}", xml_path.display()))?;
    let kit = parse(&xml)?;
    let base = xml_path.parent().unwrap_or_else(|| Path::new("."));
    to_soundfont(&kit, base)
}

/// Builds the SoundFont bytes of `kit`, reading its sample files below `base`.
pub fn to_soundfont(kit: &Drumkit, base: &Path) -> Result<Vec<u8>, String> {
    let base = base
        .canonicalize()
        .map_err(|e| format!("{}: {e}", base.display()))?;
    let mut samples: Vec<Sample> = Vec::new();
    let mut zones: Vec<Zone> = Vec::new();
    let mut frames = 0usize;
    for instrument in &kit.instruments {
        for layer in &instrument.layers {
            if zones.len() >= MAX_ZONES {
                return Err(format!("the kit has more than {MAX_ZONES} sample layers"));
            }
            let path = sample_path(&base, &layer.file)?;
            let (pcm, rate) = decode(&path)?;
            frames = frames.saturating_add(pcm.len());
            if frames > MAX_TOTAL_FRAMES {
                return Err("the kit's samples are too long".to_string());
            }
            let vel_lo = (layer.velocity.0 * 127.0).round().clamp(1.0, 127.0) as u8;
            let vel_hi = (layer.velocity.1 * 127.0).round().clamp(1.0, 127.0) as u8;
            // The sample plays at its recorded pitch on its own note; a gain below one is an
            // attenuation (100 centibels to the decibel). A gain above one is left at 0 dB: the
            // player would clip it there anyway.
            let attenuation = if layer.gain < 1.0 {
                (-2000.0 * layer.gain.max(0.004).log10()).round() as i16
            } else {
                0
            };
            zones.push(Zone {
                sample: samples.len(),
                key_lo: instrument.note,
                key_hi: instrument.note,
                vel_lo,
                vel_hi: vel_hi.max(vel_lo),
                root_key: instrument.note,
                attenuation,
            });
            samples.push(Sample {
                name: format!("{} {}", instrument.name, layer.file),
                pcm,
                rate,
            });
        }
    }
    let presets = [Preset {
        name: &kit.name,
        bank: DRUM_BANK,
        program: 0,
        zones: &zones,
    }];
    Ok(sf2::build(&kit.name, &samples, &presets))
}

/// The path of a sample named by the kit: inside `base`, never an absolute path or a way out
/// of the kit folder.
fn sample_path(base: &Path, file: &str) -> Result<PathBuf, String> {
    let relative = Path::new(file);
    if relative.is_absolute()
        || relative
            .components()
            .any(|c| c == std::path::Component::ParentDir)
    {
        return Err(format!(
            "{file}: a sample must live inside the kit's folder"
        ));
    }
    let path = base.join(relative);
    let path = path.canonicalize().map_err(|e| format!("{file}: {e}"))?;
    if !path.starts_with(base) {
        return Err(format!(
            "{file}: a sample must live inside the kit's folder"
        ));
    }
    Ok(path)
}

/// Decodes a sample file to mono 16-bit PCM and its rate in Hz.
fn decode(path: &Path) -> Result<(Vec<i16>, u32), String> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::errors::Error as SymError;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            stream,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let mut format = probed.format;
    let track = format
        .default_track()
        .ok_or_else(|| format!("{}: no audio track", path.display()))?;
    let track_id = track.id;
    let rate = track
        .codec_params
        .sample_rate
        .ok_or_else(|| format!("{}: no sample rate", path.display()))?;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| format!("{}: {e}", path.display()))?;

    let limit = (rate as f32 * MAX_SAMPLE_SECONDS) as usize;
    let mut pcm: Vec<i16> = Vec::new();
    let mut buffer: Option<SampleBuffer<f32>> = None;
    let mut capacity = 0;
    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymError::ResetRequired) => break,
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            Err(SymError::DecodeError(_)) => continue,
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        if decoded.frames() == 0 {
            continue;
        }
        if buffer.is_none() || capacity < decoded.capacity() {
            capacity = decoded.capacity();
            buffer = Some(SampleBuffer::<f32>::new(capacity as u64, *decoded.spec()));
        }
        let Some(buffer) = buffer.as_mut() else {
            return Err(format!("{}: cannot decode", path.display()));
        };
        let channels = decoded.spec().channels.count().max(1);
        buffer.copy_interleaved_ref(decoded);
        for frame in buffer.samples().chunks(channels) {
            let sum: f32 = frame.iter().sum();
            let value = (sum / channels as f32).clamp(-1.0, 1.0);
            pcm.push((value * 32_767.0) as i16);
        }
        if pcm.len() > limit {
            pcm.truncate(limit);
            break;
        }
    }
    if pcm.is_empty() {
        return Err(format!("{}: the sample has no audio", path.display()));
    }
    Ok((pcm, rate))
}

// --- A very small XML reader: Hydrogen's files are simple, and this keeps the engine free of
// --- an XML dependency.

struct Node {
    name: String,
    text: String,
    children: Vec<Node>,
}

fn parse_xml(src: &str) -> Result<Node, String> {
    let mut stack = vec![Node {
        name: String::new(),
        text: String::new(),
        children: Vec::new(),
    }];
    let mut i = 0;
    while i < src.len() {
        let Some(rest) = src.get(i..) else {
            return Err("the drumkit.xml is not UTF-8".to_string());
        };
        if rest.starts_with("<!--") {
            let Some(end) = rest.find("-->") else {
                return Err("unterminated comment".to_string());
            };
            i += end + 3;
        } else if rest.starts_with("<?") {
            let Some(end) = rest.find("?>") else {
                return Err("unterminated declaration".to_string());
            };
            i += end + 2;
        } else if rest.starts_with("<!") {
            let Some(end) = rest.find('>') else {
                return Err("unterminated DOCTYPE".to_string());
            };
            i += end + 1;
        } else if rest.starts_with('<') {
            let Some(end) = rest.find('>') else {
                return Err("unterminated tag".to_string());
            };
            let inner = &rest[1..end];
            i += end + 1;
            if let Some(closing) = inner.strip_prefix('/') {
                let closing = closing.trim();
                let Some(node) = stack.pop() else {
                    return Err(format!("</{closing}> has no <{closing}>"));
                };
                if !node.name.eq_ignore_ascii_case(closing) {
                    return Err(format!("</{closing}> closes <{}>", node.name));
                }
                let Some(parent) = stack.last_mut() else {
                    return Err("a tag outside the root".to_string());
                };
                parent.children.push(node);
            } else {
                let self_closing = inner.trim_end().ends_with('/') || inner.trim_end().is_empty();
                let body = inner.trim_end().trim_end_matches('/');
                let name = body.split_whitespace().next().unwrap_or("");
                if name.is_empty() {
                    continue;
                }
                let node = Node {
                    name: name.to_string(),
                    text: String::new(),
                    children: Vec::new(),
                };
                if self_closing {
                    let Some(parent) = stack.last_mut() else {
                        return Err("a tag outside the root".to_string());
                    };
                    parent.children.push(node);
                } else {
                    if stack.len() >= MAX_DEPTH {
                        return Err("the drumkit.xml is nested too deeply".to_string());
                    }
                    stack.push(node);
                }
            }
        } else {
            let end = rest.find('<').map_or(src.len(), |j| i + j);
            let text = &src[i..end];
            if let Some(node) = stack.last_mut() {
                node.text.push_str(text);
            }
            i = end;
        }
    }
    if stack.len() != 1 {
        return Err(format!(
            "<{}> is not closed",
            stack.last().map_or("", |n| n.name.as_str())
        ));
    }
    let Some(document) = stack.pop() else {
        return Err("empty document".to_string());
    };
    document
        .children
        .into_iter()
        .next()
        .ok_or_else(|| "the drumkit.xml has no root element".to_string())
}

/// The first child of `node` named `name`, ignoring case.
fn child<'a>(node: &'a Node, name: &str) -> Option<&'a Node> {
    node.children
        .iter()
        .find(|c| c.name.eq_ignore_ascii_case(name))
}

/// Every descendant of `node` named `name`, in document order.
fn collect<'a>(node: &'a Node, name: &str, out: &mut Vec<&'a Node>) {
    collect_to(node, name, 0, out);
}

fn collect_to<'a>(node: &'a Node, name: &str, depth: usize, out: &mut Vec<&'a Node>) {
    if depth > MAX_DEPTH {
        return;
    }
    for child in &node.children {
        if child.name.eq_ignore_ascii_case(name) {
            out.push(child);
        }
        collect_to(child, name, depth + 1, out);
    }
}

/// The text of `node` without its whitespace, with the XML entities Hydrogen writes undone.
fn text(node: &Node) -> String {
    let raw = node.text.trim();
    if raw.contains('&') {
        raw.replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&amp;", "&")
    } else {
        raw.to_string()
    }
}

/// The number in the child `name` of `node`, if any.
fn number(node: &Node, name: &str) -> Option<f32> {
    child(node, name)
        .map(text)
        .and_then(|t| t.parse::<f32>().ok())
        .filter(|n| n.is_finite())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::soundfont::{SfPlayer, SoundFontBank};

    /// A 16-bit mono PCM WAV of `hz` for `seconds` at `rate`.
    fn wav(rate: u32, hz: f32, seconds: f32) -> Vec<u8> {
        let frames = (rate as f32 * seconds) as u32;
        let mut data = Vec::new();
        for i in 0..frames {
            let t = i as f32 / rate as f32;
            let value = (f32::sin(std::f32::consts::TAU * hz * t) * 12_000.0) as i16;
            data.extend_from_slice(&value.to_le_bytes());
        }
        let mut out = b"RIFF".to_vec();
        out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * 2).to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&data);
        out
    }

    const KIT: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE drumkit SYSTEM "drumkit.dtd">
<drumkit version="1.0">
  <drumkit_info>
    <name>Test Kit</name>
    <author>Pro Wave</author>
  </drumkit_info>
  <instrumentList>
    <instrument>
      <id>0</id>
      <name>Kick</name>
      <layer>
        <filename>kick.wav</filename>
        <min>0.0</min>
        <max>0.6</max>
        <gain>0.5</gain>
      </layer>
      <layer>
        <filename>kick-hard.wav</filename>
        <min>0.6</min>
        <max>1.0</max>
      </layer>
      <midi_out_note>36</midi_out_note>
    </instrument>
    <instrument>
      <id>1</id>
      <name>Snare</name>
      <layer>
        <filename>snare.wav</filename>
        <min>0.0</min>
        <max>1.0</max>
      </layer>
      <midi_out_note>38</midi_out_note>
    </instrument>
  </instrumentList>
</drumkit>
"#;

    /// A folder with the samples and a `drumkit.xml` holding `xml`.
    fn kit_dir(xml: &str, files: &[(&str, Vec<u8>)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        for (name, bytes) in files {
            std::fs::write(dir.path().join(name), bytes).unwrap_or_else(|e| panic!("{e}"));
        }
        std::fs::write(dir.path().join("drumkit.xml"), xml).unwrap_or_else(|e| panic!("{e}"));
        dir
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
    fn a_kit_names_its_instruments_layers_and_notes() {
        let kit = parse(KIT.as_bytes()).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(kit.name, "Test Kit");
        let notes: Vec<u8> = kit.instruments.iter().map(|i| i.note).collect();
        assert_eq!(notes, [36, 38]);
        assert_eq!(kit.instruments[0].name, "Kick");
        assert_eq!(kit.instruments[0].layers.len(), 2);
        assert_eq!(kit.instruments[0].layers[0].file, "kick.wav");
        assert_eq!(kit.instruments[0].layers[0].velocity, (0.0, 0.6));
        assert_eq!(kit.instruments[0].layers[0].gain, 0.5);
        assert_eq!(kit.instruments[1].layers[0].velocity, (0.0, 1.0));
    }

    #[test]
    fn a_kit_without_notes_numbers_its_instruments_from_c1() {
        let xml = "<drumkit><instrumentList>\
            <instrument><name>a</name><layer><filename>a.wav</filename></layer></instrument>\
            <instrument><name>b</name><layer><filename>b.wav</filename></layer></instrument>\
            </instrumentList></drumkit>";
        let kit = parse(xml.as_bytes()).unwrap_or_else(|e| panic!("{e}"));
        let notes: Vec<u8> = kit.instruments.iter().map(|i| i.note).collect();
        assert_eq!(notes, [36, 37]);
        assert_eq!(kit.name, "Hydrogen Drumkit");
    }

    #[test]
    fn the_kit_plays_the_sample_of_the_note_that_is_hit() {
        let dir = kit_dir(
            KIT,
            &[
                ("kick.wav", wav(44_100, 220.0, 1.0)),
                ("kick-hard.wav", wav(44_100, 220.0, 1.0)),
                ("snare.wav", wav(44_100, 880.0, 1.0)),
            ],
        );
        let bytes =
            soundfont_bytes(&dir.path().join("drumkit.xml")).unwrap_or_else(|e| panic!("{e}"));
        let bank = SoundFontBank::load(&bytes).unwrap_or_else(|e| panic!("{e}"));
        let Some(preset) = bank.presets().iter().find(|p| p.bank == 128).cloned() else {
            panic!("the kit is a drum preset");
        };
        assert_eq!(preset.name, "Test Kit");
        assert!(preset.is_drums());

        let mut player = SfPlayer::new(&bank, &preset, 48_000.0).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(energy(&mut player, 1_000), 0.0, "silent before a note");
        player.note_on(36, 100);
        assert!(energy(&mut player, 4_800) > 1.0, "the kick sounds");

        let mut other = SfPlayer::new(&bank, &preset, 48_000.0).unwrap_or_else(|e| panic!("{e}"));
        other.note_on(37, 100);
        assert_eq!(
            energy(&mut other, 4_800),
            0.0,
            "a note with no sample is silent"
        );
    }

    #[test]
    fn broken_kits_are_errors_not_panics() {
        assert!(parse(b"").is_err());
        assert!(parse(b"not xml at all").is_err());
        assert!(
            parse(b"<drumkit><instrumentList>").is_err(),
            "unclosed tags"
        );
        assert!(
            parse(b"<drumkit><instrumentList></instrumentList></drumkit>").is_err(),
            "no instrument"
        );
        let only_kick = kit_dir(KIT, &[("kick.wav", wav(44_100, 220.0, 0.1))]);
        assert!(
            soundfont_bytes(&only_kick.path().join("drumkit.xml")).is_err(),
            "a missing sample is an error"
        );
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let outside = Drumkit {
            name: "Bad".to_string(),
            instruments: vec![Instrument {
                name: "Evil".to_string(),
                note: 36,
                layers: vec![Layer {
                    file: "../outside.wav".to_string(),
                    velocity: (0.0, 1.0),
                    gain: 1.0,
                }],
            }],
        };
        assert!(
            to_soundfont(&outside, dir.path()).is_err(),
            "a sample outside the kit folder is refused"
        );
    }

    #[test]
    fn only_a_drumkit_xml_is_a_kit() {
        assert!(is_drumkit(Path::new("GMRockKit/drumkit.xml")));
        assert!(is_drumkit(Path::new("GMRockKit/DrumKit.XML")));
        assert!(!is_drumkit(Path::new("GMRockKit/kick.wav")));
        assert!(!is_drumkit(Path::new("GeneralMIDI.sf2")));
    }
}
