//! EXS24 reading: little- and big-endian files, zones linked to groups and samples, the flags,
//! and the limits and sample-name checks that untrusted instruments need.

use prowave_exs::{
    load, parse, ByteOrder, Error, LoopDirection, Sample, MAX_FILE_BYTES, MAX_ZONES,
};
use std::path::Path;

const LITTLE: [u32; 4] = [0x0000_0101, 0x0100_0101, 0x0200_0101, 0x0300_0101];
const BIG: [u32; 4] = [0x0101_0000, 0x0101_0001, 0x0101_0002, 0x0101_0003];
const LITTLE_NEW: [u32; 4] = [0x4000_0101, 0x4100_0101, 0x4200_0101, 0x4300_0101];
const PARAMETERS: u32 = 0x0400_0101;

#[derive(Clone, Copy)]
enum Order {
    Little,
    Big,
}

/// Builds an `.exs` file block by block, in the style the reader is expected to read.
struct Builder {
    bytes: Vec<u8>,
    order: Order,
    signatures: [u32; 4],
}

impl Builder {
    fn new(order: Order) -> Self {
        let signatures = match order {
            Order::Little => LITTLE,
            Order::Big => BIG,
        };
        Self {
            bytes: Vec::new(),
            order,
            signatures,
        }
    }

    fn put_u32(&self, out: &mut [u8], at: usize, value: u32) {
        let bytes = match self.order {
            Order::Little => value.to_le_bytes(),
            Order::Big => value.to_be_bytes(),
        };
        out[at..at + 4].copy_from_slice(&bytes);
    }

    fn block(&mut self, signature: u32, name: &str, data: &[u8]) {
        let mut header = [0u8; 84];
        self.put_u32(&mut header, 0, signature);
        self.put_u32(&mut header, 4, data.len() as u32);
        self.put_u32(&mut header, 8, 1);
        let name = name.as_bytes();
        let len = name.len().min(63);
        header[20..20 + len].copy_from_slice(&name[..len]);
        self.bytes.extend_from_slice(&header);
        self.bytes.extend_from_slice(data);
    }

    fn instrument(&mut self, name: &str) {
        let header = self.signatures[0];
        self.block(header, name, &[]);
    }

    fn zone(&mut self, spec: ZoneSpec) {
        let signature = self.signatures[1];
        let data = self.zone_data(spec);
        self.block(signature, "zone", &data);
    }

    fn group(&mut self, spec: GroupSpec) {
        let signature = self.signatures[2];
        let data = self.group_data(spec);
        self.block(signature, "group", &data);
    }

    fn sample(&mut self, spec: SampleSpec) {
        let signature = self.signatures[3];
        let data = self.sample_data(spec);
        self.block(signature, "sample", &data);
    }

    fn zone_data(&self, spec: ZoneSpec) -> Vec<u8> {
        let mut d = vec![0u8; 96];
        d[0] = spec.flags;
        d[1] = spec.root;
        d[2] = spec.fine as u8;
        d[3] = spec.pan as u8;
        d[4] = spec.volume as u8;
        d[6] = spec.key_low;
        d[7] = spec.key_high;
        d[9] = spec.vel_low;
        d[10] = spec.vel_high;
        self.put_u32(&mut d, 12, spec.sample_start);
        self.put_u32(&mut d, 16, spec.sample_end);
        self.put_u32(&mut d, 20, spec.loop_start);
        self.put_u32(&mut d, 24, spec.loop_end);
        d[33] = spec.loop_flags;
        d[34] = spec.loop_direction;
        d[80] = spec.coarse as u8;
        let group = spec.group.to_le_bytes();
        let group = match self.order {
            Order::Little => group,
            Order::Big => spec.group.to_be_bytes(),
        };
        d[88..92].copy_from_slice(&group);
        self.put_u32(&mut d, 92, spec.sample);
        d
    }

    fn group_data(&self, spec: GroupSpec) -> Vec<u8> {
        let mut d = vec![0u8; 92];
        d[0] = spec.volume;
        d[1] = spec.pan;
        d[2] = spec.polyphony;
        d[4] = spec.exclusive;
        d[5] = spec.vel_low;
        d[6] = spec.vel_high;
        let release = match self.order {
            Order::Little => spec.release_trigger_time.to_le_bytes(),
            Order::Big => spec.release_trigger_time.to_be_bytes(),
        };
        d[16..18].copy_from_slice(&release);
        let position = match self.order {
            Order::Little => spec.round_robin_position.to_le_bytes(),
            Order::Big => spec.round_robin_position.to_be_bytes(),
        };
        d[80..84].copy_from_slice(&position);
        d[88] = spec.start_note;
        d[89] = spec.end_note;
        d
    }

    fn sample_data(&self, spec: SampleSpec) -> Vec<u8> {
        let mut d = vec![0u8; 592];
        self.put_u32(&mut d, 4, spec.length);
        self.put_u32(&mut d, 8, spec.sample_rate);
        self.put_u32(&mut d, 12, spec.bit_depth);
        self.put_u32(&mut d, 16, spec.channels);
        d[28..32].copy_from_slice(spec.file_type.as_bytes());
        let path = spec.file_path.as_bytes();
        let path_len = path.len().min(255);
        d[80..80 + path_len].copy_from_slice(&path[..path_len]);
        let name = spec.file_name.as_bytes();
        let name_len = name.len().min(255);
        d[336..336 + name_len].copy_from_slice(&name[..name_len]);
        d
    }
}

#[derive(Clone, Copy)]
struct ZoneSpec {
    flags: u8,
    root: u8,
    fine: i8,
    pan: i8,
    volume: i8,
    key_low: u8,
    key_high: u8,
    vel_low: u8,
    vel_high: u8,
    sample_start: u32,
    sample_end: u32,
    loop_start: u32,
    loop_end: u32,
    loop_flags: u8,
    loop_direction: u8,
    coarse: i8,
    group: i32,
    sample: u32,
}

impl Default for ZoneSpec {
    fn default() -> Self {
        Self {
            flags: 0,
            root: 60,
            fine: 0,
            pan: 0,
            volume: 0,
            key_low: 0,
            key_high: 127,
            vel_low: 0,
            vel_high: 127,
            sample_start: 0,
            sample_end: 0,
            loop_start: 0,
            loop_end: 0,
            loop_flags: 0,
            loop_direction: 0,
            coarse: 0,
            group: -1,
            sample: 0,
        }
    }
}

#[derive(Clone, Copy)]
struct GroupSpec {
    volume: u8,
    pan: u8,
    polyphony: u8,
    exclusive: u8,
    vel_low: u8,
    vel_high: u8,
    release_trigger_time: u16,
    round_robin_position: i32,
    start_note: u8,
    end_note: u8,
}

impl Default for GroupSpec {
    fn default() -> Self {
        Self {
            volume: 100,
            pan: 0,
            polyphony: 0,
            exclusive: 0,
            vel_low: 0,
            vel_high: 127,
            release_trigger_time: 0,
            round_robin_position: -1,
            start_note: 0,
            end_note: 127,
        }
    }
}

#[derive(Clone, Copy)]
struct SampleSpec {
    length: u32,
    sample_rate: u32,
    bit_depth: u32,
    channels: u32,
    file_type: &'static str,
    file_path: &'static str,
    file_name: &'static str,
}

impl Default for SampleSpec {
    fn default() -> Self {
        Self {
            length: 1000,
            sample_rate: 44_100,
            bit_depth: 24,
            channels: 2,
            file_type: "WAVE",
            file_path: "/Users/me/Samples/Kick.wav",
            file_name: "Kick.wav",
        }
    }
}

fn minimal(order: Order) -> Builder {
    let mut b = Builder::new(order);
    b.instrument("Test Kit");
    b.zone(ZoneSpec {
        key_low: 36,
        key_high: 48,
        root: 36,
        fine: -12,
        pan: 10,
        volume: -2,
        loop_flags: 0b011,
        loop_direction: 2,
        loop_start: 100,
        loop_end: 200,
        coarse: 1,
        group: 0,
        ..ZoneSpec::default()
    });
    b.group(GroupSpec {
        volume: 100,
        pan: 20,
        polyphony: 4,
        exclusive: 1,
        release_trigger_time: 500,
        start_note: 36,
        end_note: 48,
        ..GroupSpec::default()
    });
    b.sample(SampleSpec::default());
    b
}

#[test]
fn reads_a_little_endian_instrument() {
    let instrument = parse(&minimal(Order::Little).bytes).expect("parse");
    assert_eq!(instrument.name, "Test Kit");
    assert_eq!(instrument.byte_order, ByteOrder::Little);

    let zone = &instrument.zones[0];
    assert_eq!((zone.key_low, zone.key_high), (36, 48));
    assert_eq!((zone.vel_low, zone.vel_high), (0, 127));
    assert_eq!(zone.root_key, 36);
    assert_eq!(zone.fine_tuning, -12);
    assert_eq!(zone.pan, 10);
    assert_eq!(zone.volume_adjust, -2);
    assert_eq!(zone.coarse_tuning, 1);
    assert!(zone.loop_enabled);
    assert!(zone.loop_equal_power);
    assert!(!zone.loop_play_to_end_on_release);
    assert_eq!(zone.loop_direction, LoopDirection::PingPong);
    assert_eq!((zone.loop_start, zone.loop_end), (100, 200));
    assert_eq!(zone.group, Some(0));
    assert_eq!(zone.sample, Some(0));

    let group = &instrument.groups[0];
    assert_eq!(group.volume, 100);
    assert_eq!(group.pan, 20);
    assert_eq!(group.polyphony, 4);
    assert_eq!(group.exclusive, 1);
    assert_eq!(group.release_trigger_time, 500);
    assert_eq!(group.round_robin_position, None);
    assert_eq!((group.start_note, group.end_note), (36, 48));

    let sample = &instrument.samples[0];
    assert_eq!(sample.length, 1000);
    assert_eq!(sample.sample_rate, 44_100);
    assert_eq!(sample.bit_depth, 24);
    assert_eq!(sample.channels, 2);
    assert_eq!(sample.file_type, "WAVE");
    assert_eq!(sample.file_path, "/Users/me/Samples/Kick.wav");
    assert_eq!(sample.file_name, "Kick.wav");
    assert!(sample.file_name_is_safe());
}

#[test]
fn reads_a_big_endian_instrument() {
    let instrument = parse(&minimal(Order::Big).bytes).expect("parse");
    assert_eq!(instrument.byte_order, ByteOrder::Big);
    assert_eq!(instrument.name, "Test Kit");
    let zone = &instrument.zones[0];
    assert_eq!((zone.key_low, zone.key_high), (36, 48));
    assert_eq!(zone.loop_direction, LoopDirection::PingPong);
    assert_eq!(zone.group, Some(0));
    assert_eq!(zone.sample, Some(0));
    assert_eq!(instrument.samples[0].file_name, "Kick.wav");
    assert_eq!(instrument.samples[0].length, 1000);
}

#[test]
fn reads_the_new_style_signatures() {
    let mut b = Builder::new(Order::Little);
    b.signatures = LITTLE_NEW;
    b.instrument("Test Kit");
    b.zone(ZoneSpec::default());
    let instrument = parse(&b.bytes).expect("parse");
    assert_eq!(instrument.name, "Test Kit");
    assert_eq!(instrument.zones.len(), 1);
}

#[test]
fn zone_links_out_of_range_are_dropped() {
    let mut b = Builder::new(Order::Little);
    b.instrument("Links");
    b.zone(ZoneSpec {
        group: 5,
        sample: 9,
        ..ZoneSpec::default()
    });
    let instrument = parse(&b.bytes).expect("parse");
    assert_eq!(instrument.zones[0].group, None);
    assert_eq!(instrument.zones[0].sample, None);
}

#[test]
fn zone_and_loop_flags_are_decoded() {
    let mut b = Builder::new(Order::Little);
    b.instrument("Flags");
    // bit 0 one-shot, bit 1 `!pitch` (so pitch tracking is off), bit 2 reverse.
    b.zone(ZoneSpec {
        flags: 0b111,
        loop_flags: 0b100,
        ..ZoneSpec::default()
    });
    b.zone(ZoneSpec {
        flags: 0,
        loop_flags: 0,
        ..ZoneSpec::default()
    });
    let instrument = parse(&b.bytes).expect("parse");

    let first = &instrument.zones[0];
    assert!(first.one_shot);
    assert!(!first.pitch_tracking);
    assert!(first.reverse);
    assert!(!first.loop_enabled);
    assert!(first.loop_play_to_end_on_release);

    let second = &instrument.zones[1];
    assert!(!second.one_shot);
    assert!(second.pitch_tracking);
    assert!(!second.reverse);
    assert!(!second.loop_enabled);
}

#[test]
fn a_parameter_block_is_skipped() {
    let mut b = minimal(Order::Little);
    b.block(PARAMETERS, "parameters", &[0u8; 128]);
    let instrument = parse(&b.bytes).expect("parse");
    assert_eq!(instrument.zones.len(), 1);
    assert_eq!(instrument.samples.len(), 1);
}

#[test]
fn a_file_that_is_not_an_exs_instrument_is_refused() {
    assert!(matches!(
        parse(b"not an instrument at all"),
        Err(Error::NotExs)
    ));
    assert!(matches!(parse(&[]), Err(Error::NotExs)));

    // A zone block first, without a header block.
    let mut b = Builder::new(Order::Little);
    let data = b.zone_data(ZoneSpec::default());
    b.block(LITTLE[1], "zone", &data);
    assert!(matches!(parse(&b.bytes), Err(Error::NotExs)));
}

#[test]
fn truncated_blocks_are_refused() {
    let mut b = minimal(Order::Little);
    b.bytes.truncate(b.bytes.len() - 10);
    assert!(matches!(parse(&b.bytes), Err(Error::Truncated)));

    // A zone block shorter than its documented 96 bytes.
    let mut short = Builder::new(Order::Little);
    short.instrument("Short");
    short.block(LITTLE[1], "zone", &[0u8; 95]);
    assert!(matches!(parse(&short.bytes), Err(Error::Truncated)));
}

#[test]
fn files_larger_than_the_limit_are_refused() {
    let huge = vec![0u8; MAX_FILE_BYTES + 1];
    assert!(matches!(
        parse(&huge),
        Err(Error::TooLarge {
            limit: MAX_FILE_BYTES
        })
    ));

    let Ok(dir) = tempfile::tempdir() else {
        panic!("a temporary directory");
    };
    let path = dir.path().join("huge.exs");
    if let Err(e) = std::fs::write(&path, &huge) {
        panic!("writing the oversized instrument: {e}");
    }
    assert!(matches!(
        load(Path::new(&path)),
        Err(Error::TooLarge {
            limit: MAX_FILE_BYTES
        })
    ));
}

#[test]
fn the_zone_count_is_capped() {
    let mut b = Builder::new(Order::Little);
    b.instrument("Too many");
    for _ in 0..=MAX_ZONES {
        b.zone(ZoneSpec::default());
    }
    assert!(matches!(
        parse(&b.bytes),
        Err(Error::TooManyZones { limit: MAX_ZONES })
    ));
}

#[test]
fn non_utf8_names_are_lossy_not_fatal() {
    let mut b = Builder::new(Order::Little);
    let mut header = [0u8; 84];
    b.put_u32(&mut header, 0, LITTLE[0]);
    b.put_u32(&mut header, 4, 0);
    b.put_u32(&mut header, 8, 1);
    header[20..25].copy_from_slice(&[b'A', 0xff, b'B', 0, 0]);
    b.bytes.extend_from_slice(&header);
    let instrument = parse(&b.bytes).expect("parse");
    assert_eq!(instrument.name, "A\u{fffd}B");
}

#[test]
fn sample_file_names_must_be_a_single_name() {
    let sample = |name: &str| Sample {
        name: "Kick".into(),
        length: 1,
        sample_rate: 44_100,
        bit_depth: 16,
        channels: 1,
        file_type: "WAVE".into(),
        file_path: "/tmp/Kick.wav".into(),
        file_name: name.into(),
    };
    for safe in ["Kick.wav", "kick_1.aif", "snare-2.wav"] {
        assert!(sample(safe).file_name_is_safe(), "{safe}");
    }
    for unsafe_name in ["", ".", "..", "../Kick.wav", "a/b.wav", "a\\b.wav"] {
        assert!(!sample(unsafe_name).file_name_is_safe(), "{unsafe_name}");
    }
}

#[test]
fn reads_a_file() {
    let Ok(dir) = tempfile::tempdir() else {
        panic!("a temporary directory");
    };
    let path = dir.path().join("Kit.exs");
    if let Err(e) = std::fs::write(&path, &minimal(Order::Little).bytes) {
        panic!("writing the instrument: {e}");
    }
    let instrument = load(Path::new(&path)).expect("load");
    assert_eq!(instrument.name, "Test Kit");
    assert_eq!(instrument.zones.len(), 1);
}
