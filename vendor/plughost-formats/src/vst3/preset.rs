//! Steinberg VST3 preset container. Validate structure before handing opaque state to a plugin.
use super::errors::Vst3Error;
use plughost_core::{MAX_PRESET_BYTES, PluginState};

const HEADER_LENGTH: usize = 48;
const CLASS_ID_LENGTH: usize = 32;
const FORMAT_VERSION: i32 = 1;
const COMPONENT_ID: &[u8; 4] = b"Comp";
const CONTROLLER_ID: &[u8; 4] = b"Cont";
const INFO_ID: &[u8; 4] = b"Info";

/// A `.vstpreset` container with independent processor, controller, and XML metadata chunks.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Preset {
    /// Target VST3 class ID as 32 hexadecimal digits.
    pub class_id: String,
    /// Opaque component state from the `Comp` chunk.
    pub component: Vec<u8>,
    /// Opaque controller state from the `Cont` chunk, which a plugin whose controller keeps no
    /// state of its own has none of.
    pub controller: Option<Vec<u8>>,
    /// Opaque XML metadata. Native/vendor attributes are preserved without reinterpretation.
    pub info: Vec<u8>,
}

impl Preset {
    /// Copies native state into a container with no XML annotations.
    #[must_use]
    pub fn from_state(state: &PluginState) -> Self {
        Self {
            class_id: state.id().class_id().to_string(),
            component: state.component().to_vec(),
            controller: state.controller().map(<[u8]>::to_vec),
            info: Vec::new(),
        }
    }

    /// Encodes a size-bounded container after validating its class ID and chunk lengths.
    pub fn to_bytes(&self) -> Result<Vec<u8>, crate::Error> {
        Ok(self.encode()?)
    }

    fn encode(&self) -> Result<Vec<u8>, Vst3Error> {
        if !valid_class(&self.class_id) {
            return Err(Vst3Error::Preset);
        }
        let mut length = HEADER_LENGTH + 8;
        let chunks: Vec<(&[u8; 4], &[u8])> = [
            (COMPONENT_ID, Some(&self.component[..])),
            (CONTROLLER_ID, self.controller.as_deref()),
            (INFO_ID, (!self.info.is_empty()).then_some(&self.info[..])),
        ]
        .into_iter()
        .filter_map(|(id, data)| Some((id, data?)))
        .collect();
        for (_, data) in &chunks {
            length = length
                .checked_add(data.len())
                .and_then(|n| n.checked_add(20))
                .ok_or(Vst3Error::Preset)?;
        }
        if length > MAX_PRESET_BYTES {
            return Err(Vst3Error::Preset);
        }
        let mut bytes = Vec::with_capacity(length);
        bytes.extend_from_slice(b"VST3");
        bytes.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        bytes.extend_from_slice(self.class_id.as_bytes());
        bytes.extend_from_slice(&[0; 8]);
        let mut entries = Vec::new();
        for (id, data) in chunks {
            entries.push((id, bytes.len(), data.len()));
            bytes.extend_from_slice(data);
        }
        let list = bytes.len();
        bytes[40..48].copy_from_slice(&(list as i64).to_le_bytes());
        bytes.extend_from_slice(b"List");
        bytes.extend_from_slice(&(entries.len() as i32).to_le_bytes());
        for (id, offset, size) in entries {
            bytes.extend_from_slice(id);
            bytes.extend_from_slice(&(offset as i64).to_le_bytes());
            bytes.extend_from_slice(&(size as i64).to_le_bytes());
        }
        Ok(bytes)
    }

    /// Validates the header, chunk table, offsets, lengths, and overlaps before copying state.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, crate::Error> {
        Ok(Self::decode(bytes)?)
    }

    fn decode(bytes: &[u8]) -> Result<Self, Vst3Error> {
        if bytes.len() > MAX_PRESET_BYTES
            || bytes.get(..4) != Some(b"VST3")
            || read_i32(bytes, 4)? != FORMAT_VERSION
        {
            return Err(Vst3Error::Preset);
        }
        let class_id = bytes
            .get(8..40)
            .and_then(|s| std::str::from_utf8(s).ok())
            .filter(|id| valid_class(id))
            .ok_or(Vst3Error::Preset)?
            .to_owned();
        let list = usize::try_from(read_i64(bytes, 40)?).map_err(|_| Vst3Error::Preset)?;
        let table = bytes
            .get(list..)
            .filter(|_| list >= HEADER_LENGTH)
            .ok_or(Vst3Error::Preset)?;
        if table.get(..4) != Some(b"List") {
            return Err(Vst3Error::Preset);
        }
        let count = usize::try_from(read_i32(table, 4)?).map_err(|_| Vst3Error::Preset)?;
        let table_length = count
            .checked_mul(20)
            .and_then(|n| n.checked_add(8))
            .ok_or(Vst3Error::Preset)?;
        if table_length > table.len() {
            return Err(Vst3Error::Preset);
        }
        let mut component = None;
        let mut controller = None;
        let mut info = None;
        let mut ranges = Vec::new();
        // Validate all entries and overlaps before allocating copies of opaque chunks.
        let mut entries = Vec::new();
        for entry in table[8..table_length].as_chunks::<20>().0 {
            let id = &entry[..4];
            let offset = usize::try_from(read_i64(entry, 4)?).map_err(|_| Vst3Error::Preset)?;
            let size = usize::try_from(read_i64(entry, 12)?).map_err(|_| Vst3Error::Preset)?;
            let end = offset
                .checked_add(size)
                .filter(|end| *end <= list)
                .ok_or(Vst3Error::Preset)?;
            if offset < HEADER_LENGTH {
                return Err(Vst3Error::Preset);
            }
            ranges.push((offset, end));
            entries.push((id, offset, end));
        }
        ranges.sort_unstable();
        let mut previous_end = HEADER_LENGTH;
        for (start, end) in ranges {
            if start != end && start < previous_end {
                return Err(Vst3Error::Preset);
            }
            previous_end = previous_end.max(end);
        }
        for (id, offset, end) in entries {
            let target = match id {
                b"Comp" => &mut component,
                b"Cont" => &mut controller,
                b"Info" => &mut info,
                _ => continue,
            };
            if target.is_some() {
                return Err(Vst3Error::Preset);
            }
            *target = Some(bytes[offset..end].to_vec());
        }
        Ok(Self {
            class_id,
            component: component.ok_or(Vst3Error::Preset)?,
            controller,
            info: info.unwrap_or_default(),
        })
    }
}

fn valid_class(id: &str) -> bool {
    id.len() == CLASS_ID_LENGTH && id.bytes().all(|b| b.is_ascii_hexdigit())
}
fn read_i32(bytes: &[u8], at: usize) -> Result<i32, Vst3Error> {
    let end = at.checked_add(4).ok_or(Vst3Error::Preset)?;
    let raw = bytes
        .get(at..end)
        .and_then(|s| s.try_into().ok())
        .ok_or(Vst3Error::Preset)?;
    Ok(i32::from_le_bytes(raw))
}
fn read_i64(bytes: &[u8], at: usize) -> Result<i64, Vst3Error> {
    let end = at.checked_add(8).ok_or(Vst3Error::Preset)?;
    let raw = bytes
        .get(at..end)
        .and_then(|s| s.try_into().ok())
        .ok_or(Vst3Error::Preset)?;
    Ok(i64::from_le_bytes(raw))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preset(controller: &[u8]) -> Preset {
        Preset {
            class_id: "84E8DE5F92554F5396FAE4133C935A18".to_owned(),
            component: vec![1, 2, 3, 4, 5],
            controller: (!controller.is_empty()).then(|| controller.to_vec()),
            info: Vec::new(),
        }
    }

    #[test]
    fn written_files_read_back() {
        // An empty controller chunk is kept apart from none.
        for controller in [Some(vec![9u8, 8, 7]), Some(Vec::new()), None] {
            let preset = Preset {
                controller,
                ..preset(&[])
            };
            assert_eq!(
                Preset::from_bytes(&preset.to_bytes().unwrap()).unwrap(),
                preset
            );
        }
    }

    #[test]
    fn layout_matches_the_sdk_format() {
        let bytes = preset(&[9]).to_bytes().unwrap();
        assert_eq!(&bytes[..4], b"VST3");
        assert_eq!(&bytes[8..40], b"84E8DE5F92554F5396FAE4133C935A18");
        let list = i64::from_le_bytes(bytes[40..48].try_into().unwrap()) as usize;
        assert_eq!(list, 48 + 5 + 1);
        assert_eq!(&bytes[list..list + 4], b"List");
        assert_eq!(&bytes[list + 8..list + 12], b"Comp");
        assert_eq!(&bytes[48..53], &[1, 2, 3, 4, 5]);
    }

    #[test]
    fn truncated_or_foreign_files_are_rejected() {
        let bytes = preset(&[]).to_bytes().unwrap();
        assert!(matches!(
            Preset::decode(&bytes[..bytes.len() - 3]),
            Err(Vst3Error::Preset)
        ));
        assert!(matches!(
            Preset::decode(b"RIFF0000"),
            Err(Vst3Error::Preset)
        ));
    }

    #[test]
    fn metadata_is_preserved_and_invalid_identity_cannot_be_written() {
        let mut preset = preset(&[9]);
        preset.info = "<MetaInfo><Attribute id=\"Name\" value=\"프리셋\"/></MetaInfo>"
            .as_bytes()
            .to_vec();
        assert_eq!(
            Preset::from_bytes(&preset.to_bytes().unwrap()).unwrap(),
            preset
        );
        for id in [
            "",
            "84E8DE5F92554F5396FAE4133C935A1Z",
            "84E8DE5F92554F5396FAE4133C935A18extra",
        ] {
            preset.class_id = id.into();
            assert!(matches!(preset.encode(), Err(Vst3Error::Preset)));
        }
    }

    #[test]
    fn malformed_headers_and_chunk_tables_are_rejected() {
        let bytes = preset(&[9]).to_bytes().unwrap();
        let list = read_i64(&bytes, 40).unwrap() as usize;
        let cases = [
            (4, 2i32.to_le_bytes().to_vec()),
            (8, vec![b'Z']),
            (40, (-1i64).to_le_bytes().to_vec()),
            (40, i64::MAX.to_le_bytes().to_vec()),
            (list + 4, (-1i32).to_le_bytes().to_vec()),
            (list + 4, i32::MAX.to_le_bytes().to_vec()),
            (list + 28, b"Comp".to_vec()), // duplicate component
            (list + 32, 48i64.to_le_bytes().to_vec()), // overlapping controller
            (list + 12, 0i64.to_le_bytes().to_vec()), // header data
            (list + 12, (list as i64).to_le_bytes().to_vec()), // table data
            (list + 20, i64::MAX.to_le_bytes().to_vec()),
            (list + 20, (-1i64).to_le_bytes().to_vec()),
        ];
        for (offset, replacement) in cases {
            let mut malformed = bytes.clone();
            malformed[offset..offset + replacement.len()].copy_from_slice(&replacement);
            assert!(
                matches!(Preset::decode(&malformed), Err(Vst3Error::Preset)),
                "offset {offset}"
            );
        }
        for length in 0..bytes.len() {
            assert!(
                Preset::from_bytes(&bytes[..length]).is_err(),
                "length {length}"
            );
        }
    }
}
