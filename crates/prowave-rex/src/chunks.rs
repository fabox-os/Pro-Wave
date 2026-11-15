//! The IFF-style container REX2 files are wrapped in.
//!
//! A chunk is a four-byte ASCII id, a big-endian `u32` payload size, the payload, and a pad
//! byte when the size is odd. `CAT ` (and `FORM`) chunks start their payload with a four-byte
//! type (`REX2` at the top level) and then hold more chunks. The file is treated as untrusted
//! input: every size is checked, the nesting depth and the number of chunks are capped.

/// The deepest `CAT` nesting accepted.
const MAX_DEPTH: usize = 8;
/// The most chunks accepted in one file.
const MAX_CHUNKS: usize = 8192;

/// One chunk of the file, with its payload.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Chunk<'a> {
    pub id: [u8; 4],
    pub data: &'a [u8],
}

/// Every chunk of the file, flat, in the order it appears (the container chunks included).
pub(crate) fn walk<'a>(data: &'a [u8]) -> Result<Vec<Chunk<'a>>, String> {
    let mut out = Vec::new();
    walk_into(data, 0, &mut out)?;
    Ok(out)
}

fn walk_into<'a>(data: &'a [u8], depth: usize, out: &mut Vec<Chunk<'a>>) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Err(format!("chunks nested deeper than {MAX_DEPTH}"));
    }
    let mut i = 0usize;
    while i + 8 <= data.len() {
        let id = [data[i], data[i + 1], data[i + 2], data[i + 3]];
        let size =
            u32::from_be_bytes([data[i + 4], data[i + 5], data[i + 6], data[i + 7]]) as usize;
        let start = i + 8;
        let end = start
            .checked_add(size)
            .ok_or_else(|| "a chunk size overflows".to_string())?;
        if end > data.len() {
            return Err(format!(
                "chunk `{}` claims {size} bytes but only {} are left",
                chunk_name(id),
                data.len() - start
            ));
        }
        if out.len() >= MAX_CHUNKS {
            return Err(format!("more than {MAX_CHUNKS} chunks"));
        }
        let payload = &data[start..end];
        out.push(Chunk { id, data: payload });
        if &id == b"CAT " || &id == b"FORM" {
            if payload.len() < 4 {
                return Err(format!(
                    "`{}` chunk is shorter than its type",
                    chunk_name(id)
                ));
            }
            walk_into(&payload[4..], depth + 1, out)?;
        }
        // Chunks are padded to an even offset; the pad byte is not part of the size.
        i = end + (size & 1);
    }
    // Trailing bytes that cannot hold a chunk header are ignored: some writers pad the file.
    Ok(())
}

/// The first chunk called `id`, if there is one.
pub(crate) fn find<'a, 'b>(chunks: &'b [Chunk<'a>], id: &[u8; 4]) -> Option<&'b Chunk<'a>> {
    chunks.iter().find(|c| &c.id == id)
}

fn chunk_name(id: [u8; 4]) -> String {
    String::from_utf8_lossy(&id).trim_end().to_string()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn chunk(id: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(id);
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        out.extend_from_slice(payload);
        if payload.len() % 2 == 1 {
            out.push(0);
        }
        out
    }

    #[test]
    fn finds_nested_chunks_and_skips_padding() {
        let mut inner = Vec::new();
        inner.extend_from_slice(b"REX2");
        inner.extend_from_slice(&chunk(b"GLOB", &[1, 2, 3]));
        let file = chunk(b"CAT ", &inner);

        let chunks = walk(&file).unwrap();
        assert_eq!(chunks.len(), 2);
        let glob = find(&chunks, b"GLOB").unwrap();
        assert_eq!(glob.data, &[1, 2, 3]);
    }

    #[test]
    fn refuses_a_chunk_that_runs_past_the_end() {
        let mut file = Vec::new();
        file.extend_from_slice(b"CAT ");
        file.extend_from_slice(&u32::MAX.to_be_bytes());
        assert!(walk(&file).is_err());
    }

    #[test]
    fn refuses_deep_nesting() {
        let mut data = b"REX2".to_vec();
        for _ in 0..(MAX_DEPTH + 2) {
            data = chunk(b"CAT ", &data);
        }
        assert!(walk(&data).is_err());
    }
}
