//! LSB-first bit I/O for DEFLATE streams (RFC 1951).
//!
//! [`BitReader`] reads from a borrowed byte slice; callers snapshot its
//! position before a parse step and [`BitReader::restore`] if the step
//! cannot be completed because more input is needed.
//!
//! [`BitWriter`] appends bits to a caller-owned `Vec<u8>`.

use crate::error::{Error, Result};

/// Snapshot of a [`BitReader`]'s position, returned from
/// [`BitReader::snapshot`] and replayed via [`BitReader::restore`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct BitReaderState {
    byte_index: usize,
    bit_buffer: u64,
    bit_count: u8,
}

/// LSB-first bit reader over a borrowed byte slice.
#[derive(Debug)]
pub(crate) struct BitReader<'a> {
    input: &'a [u8],
    byte_index: usize,
    bit_buffer: u64,
    bit_count: u8,
}

impl<'a> BitReader<'a> {
    #[cfg(test)]
    pub(crate) fn new(input: &'a [u8]) -> Self {
        Self::new_seeded(input, 0, 0)
    }

    /// Create a reader seeded with buffered bits left over from a previous
    /// call. `bit_count` may exceed 8 if whole buffered bytes were still
    /// unread.
    pub(crate) fn new_seeded(input: &'a [u8], bit_buffer: u64, bit_count: u8) -> Self {
        Self {
            input,
            byte_index: 0,
            bit_buffer,
            bit_count,
        }
    }

    /// Total bits still available for reading: the unread buffered bits
    /// plus the bits in the remaining bytes of the input slice.
    pub(crate) fn available_bits(&self) -> usize {
        self.bit_count as usize + (self.input.len() - self.byte_index) * 8
    }

    /// Buffered bits not yet consumed.
    pub(crate) fn residual_bit_buffer(&self) -> u64 {
        self.bit_buffer
    }

    /// Count of buffered bits not yet consumed.
    pub(crate) fn residual_bit_count(&self) -> u8 {
        self.bit_count
    }

    pub(crate) fn read_bit(&mut self) -> Result<bool> {
        Ok(self.read_bits(1)? != 0)
    }

    pub(crate) fn read_bits(&mut self, bit_count: u8) -> Result<u16> {
        let bits = self.peek_bits(bit_count)?;
        self.skip_bits(bit_count)?;
        Ok(bits)
    }

    pub(crate) fn peek_bits(&mut self, bit_count: u8) -> Result<u16> {
        while self.bit_count < bit_count {
            let Some(&next) = self.input.get(self.byte_index) else {
                return Err(Error::InvalidData(
                    "unexpected end of deflate stream".into(),
                ));
            };
            self.bit_buffer |= u64::from(next) << self.bit_count;
            self.bit_count += 8;
            self.byte_index += 1;
        }
        Ok((self.bit_buffer & ((1u64 << bit_count) - 1)) as u16)
    }

    pub(crate) fn skip_bits(&mut self, bit_count: u8) -> Result<()> {
        if self.bit_count < bit_count {
            self.peek_bits(bit_count)?;
        }
        self.bit_buffer >>= bit_count;
        self.bit_count -= bit_count;
        Ok(())
    }

    /// Discard any buffered fractional byte so the next read is aligned.
    pub(crate) fn align_to_byte(&mut self) {
        let extra = self.bit_count % 8;
        self.bit_buffer >>= extra;
        self.bit_count -= extra;
    }

    /// Read `len` whole bytes. Requires the reader to be byte-aligned;
    /// call [`BitReader::align_to_byte`] first.
    pub(crate) fn read_bytes(&mut self, len: usize) -> Result<&'a [u8]> {
        debug_assert_eq!(self.bit_count % 8, 0);
        // Unwind any buffered whole bytes before indexing into `input`.
        let buffered_bytes = (self.bit_count / 8) as usize;
        let start = self.byte_index - buffered_bytes;
        self.bit_buffer = 0;
        self.bit_count = 0;
        let end = start + len;
        let Some(bytes) = self.input.get(start..end) else {
            return Err(Error::InvalidData(
                "unexpected end of deflate stream".into(),
            ));
        };
        self.byte_index = end;
        Ok(bytes)
    }

    pub(crate) fn snapshot(&self) -> BitReaderState {
        BitReaderState {
            byte_index: self.byte_index,
            bit_buffer: self.bit_buffer,
            bit_count: self.bit_count,
        }
    }

    pub(crate) fn restore(&mut self, state: BitReaderState) {
        self.byte_index = state.byte_index;
        self.bit_buffer = state.bit_buffer;
        self.bit_count = state.bit_count;
    }

    /// Bytes that have been pulled from the input slice into the bit
    /// buffer. Once these bytes are "committed" by advancing the outer
    /// input buffer, the remaining unread bits survive in
    /// [`BitReader::residual_bit_buffer`] and the outer caller reseeds
    /// a fresh reader from those values next time around.
    pub(crate) fn committed_bytes(&self) -> usize {
        self.byte_index
    }
}

/// LSB-first bit writer that appends to a caller-owned `Vec<u8>`.
#[derive(Debug)]
pub(crate) struct BitWriter<'a> {
    output: &'a mut Vec<u8>,
    bit_buffer: u64,
    bit_count: u8,
}

impl<'a> BitWriter<'a> {
    pub(crate) fn new(output: &'a mut Vec<u8>) -> Self {
        Self {
            output,
            bit_buffer: 0,
            bit_count: 0,
        }
    }

    pub(crate) fn write_bit(&mut self, bit: bool) {
        self.write_bits(1, u16::from(bit));
    }

    pub(crate) fn write_bits(&mut self, bit_count: u8, bits: u16) {
        debug_assert!(bit_count <= 16);
        self.bit_buffer |= u64::from(bits) << self.bit_count;
        self.bit_count += bit_count;
        while self.bit_count >= 8 {
            self.output.push(self.bit_buffer as u8);
            self.bit_buffer >>= 8;
            self.bit_count -= 8;
        }
    }

    /// Zero-pad the current partial byte, if any, and push it to the output.
    pub(crate) fn align_to_byte(&mut self) {
        if self.bit_count > 0 {
            self.output.push(self.bit_buffer as u8);
            self.bit_buffer = 0;
            self.bit_count = 0;
        }
    }

    /// Equivalent to calling [`BitWriter::align_to_byte`] and dropping the
    /// writer.
    pub(crate) fn finish(mut self) {
        self.align_to_byte();
    }
}

#[cfg(test)]
mod tests {
    use super::{BitReader, BitWriter};

    #[test]
    fn writer_basic() {
        let mut out = Vec::new();
        let mut w = BitWriter::new(&mut out);
        w.write_bits(3, 0b101);
        w.write_bits(5, 0b10011);
        w.finish();
        // Bits LSB-first: first write fills bits 0..3 (0b101), then bits
        // 3..8 (0b10011), combining to byte 0b10011_101 = 0x9D.
        assert_eq!(out, vec![0x9D]);
    }

    #[test]
    fn writer_multibyte() {
        let mut out = Vec::new();
        let mut w = BitWriter::new(&mut out);
        w.write_bits(16, 0xABCD);
        w.finish();
        assert_eq!(out, vec![0xCD, 0xAB]);
    }

    #[test]
    fn reader_roundtrip_with_writer() {
        let mut out = Vec::new();
        let mut w = BitWriter::new(&mut out);
        w.write_bits(1, 1);
        w.write_bits(3, 0b010);
        w.write_bits(5, 0b11001);
        w.write_bits(7, 0b0110110);
        w.finish();

        let mut r = BitReader::new(&out);
        assert_eq!(r.read_bits(1).unwrap(), 1);
        assert_eq!(r.read_bits(3).unwrap(), 0b010);
        assert_eq!(r.read_bits(5).unwrap(), 0b11001);
        assert_eq!(r.read_bits(7).unwrap(), 0b0110110);
    }

    #[test]
    fn reader_eof_errors() {
        let mut r = BitReader::new(&[0x01]);
        r.read_bits(8).unwrap();
        assert!(r.read_bits(1).is_err());
    }

    #[test]
    fn reader_snapshot_restore() {
        let data = [0xAB, 0xCD, 0xEF];
        let mut r = BitReader::new(&data);
        let snap = r.snapshot();
        assert_eq!(r.read_bits(4).unwrap(), 0xB);
        assert_eq!(r.read_bits(4).unwrap(), 0xA);
        r.restore(snap);
        assert_eq!(r.read_bits(8).unwrap(), 0xAB);
        assert_eq!(r.read_bits(8).unwrap(), 0xCD);
    }

    #[test]
    fn align_to_byte_discards_fraction() {
        let data = [0x34, 0x12];
        let mut r = BitReader::new(&data);
        r.read_bits(4).unwrap();
        r.align_to_byte();
        assert_eq!(r.read_bytes(1).unwrap(), &[0x12]);
    }

    #[test]
    fn committed_bytes_counts_loaded_bytes() {
        let data = [0xAA, 0xBB, 0xCC];
        let mut r = BitReader::new(&data);
        assert_eq!(r.committed_bytes(), 0);
        r.read_bits(4).unwrap();
        // Byte 0 was loaded into the buffer even though only 4 bits were
        // consumed; the outer caller commits the whole byte and keeps the
        // remaining 4 bits via `residual_bit_buffer`.
        assert_eq!(r.committed_bytes(), 1);
        r.read_bits(4).unwrap();
        assert_eq!(r.committed_bytes(), 1);
        r.read_bits(1).unwrap();
        assert_eq!(r.committed_bytes(), 2);
    }

    #[test]
    fn available_bits() {
        let data = [0x00, 0x00];
        let mut r = BitReader::new(&data);
        assert_eq!(r.available_bits(), 16);
        r.read_bits(3).unwrap();
        assert_eq!(r.available_bits(), 13);
    }
}
