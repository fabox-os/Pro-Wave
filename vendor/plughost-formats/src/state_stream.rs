//! Bounded accumulation for native state writers. A native plugin may ignore a failed write;
//! callers must check the sticky failure before accepting its reported save result.

use std::io::{self, Write};

pub(crate) struct StateWriter {
    bytes: Vec<u8>,
    limit: usize,
    exceeded: bool,
}

impl StateWriter {
    pub fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
            exceeded: false,
        }
    }

    pub fn exceeded(&self) -> bool {
        self.exceeded
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

impl Write for StateWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.exceeded || bytes.len() > self.limit - self.bytes.len() {
            self.exceeded = true;
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                plughost_core::InputError::StateSize,
            ));
        }
        // Avoid geometric Vec growth exceeding the declared payload budget near its limit.
        if self.bytes.len() + bytes.len() > self.bytes.capacity()
            && self.bytes.capacity() > self.limit / 2
        {
            self.bytes.reserve_exact(self.limit - self.bytes.len());
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_stream_rejects_overflow_during_write_and_keeps_failure_sticky() {
        // Exercise the actual CLAP stream callback adapter, not a substitute plugin behavior.
        let mut writer = StateWriter::new(1024);
        {
            let mut stream = clack_host::stream::OutputStream::from_writer(&mut writer);
            stream.write_all(&[0x5A; 1024]).unwrap();
            assert!(stream.write_all(&[1]).is_err());
            assert!(stream.write_all(&[2]).is_err());
        }
        assert!(writer.exceeded());
        assert_eq!(writer.into_bytes(), vec![0x5A; 1024]);
    }

    #[test]
    fn native_stream_transfers_large_state_in_chunks_without_a_second_output_copy() {
        let mut writer = StateWriter::new(8 << 20);
        {
            let mut stream = clack_host::stream::OutputStream::from_writer(&mut writer);
            for byte in 0..128u8 {
                stream.write_all(&[byte; 65536]).unwrap();
            }
        }
        assert!(!writer.exceeded());
        let bytes = writer.into_bytes();
        assert_eq!(bytes.len(), 8 << 20);
        for (index, chunk) in bytes.as_chunks::<65536>().0.iter().enumerate() {
            assert!(chunk.iter().all(|&byte| byte == index as u8));
        }
    }
}
