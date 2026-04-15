//! Streaming sans-io DEFLATE encoder.
//!
//! The caller feeds uncompressed bytes via [`Encoder::feed`], then calls
//! [`Encoder::finish`] to emit the final block. Compressed bytes are
//! borrowed via [`Encoder::output`] and acknowledged via [`Encoder::advance`].
//!
//! For simplicity the encoder buffers all input until `finish()` is called,
//! then emits the stream as a single fixed- or dynamic-Huffman block (or a
//! sequence of 0xFFFF-byte stored blocks). This trades some memory for a
//! straightforward implementation while still presenting the sans-io API.

use std::cmp;

use crate::bit::BitWriter;
use crate::error::Result;
use crate::huffman::{HuffmanEncoder, length_limited_code_lengths};
use crate::lz77::{Lz77Code, MatchFinder};
use crate::symbol::{
    BITWIDTH_CODE_ORDER, END_OF_BLOCK, MAX_BITS, MAX_STORED_BLOCK, distance_to_symbol,
    fixed_distance_code_lengths, fixed_literal_code_lengths, length_to_symbol,
};

/// Which DEFLATE block encoding strategy to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    Stored,
    Fixed,
    Dynamic,
}

/// Configurable parameters for the encoder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodeOptions {
    block_kind: BlockKind,
}

impl EncodeOptions {
    /// Default options: dynamic Huffman blocks.
    pub fn new() -> Self {
        Self {
            block_kind: BlockKind::Dynamic,
        }
    }

    /// Use a single fixed-Huffman block instead of dynamic.
    #[must_use]
    pub fn fixed_huffman(mut self) -> Self {
        self.block_kind = BlockKind::Fixed;
        self
    }

    /// Emit the stream as uncompressed stored blocks.
    #[must_use]
    pub fn stored(mut self) -> Self {
        self.block_kind = BlockKind::Stored;
        self
    }
}

impl Default for EncodeOptions {
    fn default() -> Self {
        Self::new()
    }
}

/// Streaming sans-io DEFLATE encoder.
///
/// The encoder accumulates fed bytes in an internal buffer and emits the
/// entire compressed stream on [`Encoder::finish`].
#[derive(Debug)]
pub struct Encoder {
    options: EncodeOptions,
    input: Vec<u8>,
    output: Vec<u8>,
    matcher: MatchFinder,
    drained: usize,
    finishing: bool,
}

impl Default for Encoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Encoder {
    /// Create an encoder with default options (dynamic Huffman blocks).
    pub fn new() -> Self {
        Self::with_options(EncodeOptions::new())
    }

    /// Create an encoder with custom options.
    pub fn with_options(options: EncodeOptions) -> Self {
        Self {
            options,
            input: Vec::new(),
            output: Vec::new(),
            matcher: MatchFinder::new(),
            drained: 0,
            finishing: false,
        }
    }

    /// Append uncompressed bytes to the pending input buffer.
    ///
    /// Calling `feed` after [`Encoder::finish`] returns an error.
    pub fn feed(&mut self, uncompressed: &[u8]) -> Result<()> {
        if self.finishing {
            return Err(crate::error::Error::InvalidData(
                "bytes fed after encoder finish".into(),
            ));
        }
        self.input.extend_from_slice(uncompressed);
        Ok(())
    }

    /// Emit the final DEFLATE block. Subsequent calls to `finish` are a
    /// no-op.
    pub fn finish(&mut self) -> Result<()> {
        if self.finishing {
            return Ok(());
        }
        self.finishing = true;
        match self.options.block_kind {
            BlockKind::Stored => self.emit_stored()?,
            BlockKind::Fixed => self.emit_fixed_block()?,
            BlockKind::Dynamic => self.emit_dynamic_block()?,
        }
        Ok(())
    }

    /// Borrow bytes of compressed output not yet consumed via
    /// [`Encoder::advance`].
    pub fn output(&self) -> &[u8] {
        &self.output[self.drained..]
    }

    /// Mark `n` bytes of output as consumed.
    pub fn advance(&mut self, n: usize) {
        assert!(
            n <= self.output.len() - self.drained,
            "advance past end of output: n={}, available={}",
            n,
            self.output.len() - self.drained,
        );
        self.drained += n;
    }

    /// `true` once [`Encoder::finish`] has been called and all emitted
    /// output bytes have been consumed via [`Encoder::advance`].
    pub fn is_finished(&self) -> bool {
        self.finishing && self.drained == self.output.len()
    }

    fn emit_stored(&mut self) -> Result<()> {
        let total = self.input.len();
        if total == 0 {
            {
                let mut w = BitWriter::new(&mut self.output);
                w.write_bit(true);
                w.write_bits(2, 0b00);
                w.align_to_byte();
            }
            self.output.extend_from_slice(&[0, 0, 0xFF, 0xFF]);
            return Ok(());
        }
        let mut offset = 0usize;
        while offset < total {
            let chunk_len = cmp::min(MAX_STORED_BLOCK, total - offset);
            let is_final = offset + chunk_len == total;
            {
                let mut w = BitWriter::new(&mut self.output);
                w.write_bit(is_final);
                w.write_bits(2, 0b00);
                w.align_to_byte();
            }
            let len = chunk_len as u16;
            let nlen = !len;
            self.output.extend_from_slice(&len.to_le_bytes());
            self.output.extend_from_slice(&nlen.to_le_bytes());
            self.output
                .extend_from_slice(&self.input[offset..offset + chunk_len]);
            offset += chunk_len;
        }
        Ok(())
    }

    fn emit_fixed_block(&mut self) -> Result<()> {
        let symbols = self.matcher.symbols(&self.input);
        let literal_lengths = fixed_literal_code_lengths();
        let distance_lengths = fixed_distance_code_lengths();
        let literal_encoder = HuffmanEncoder::from_code_lengths(&literal_lengths)?;
        let distance_encoder = HuffmanEncoder::from_code_lengths(&distance_lengths)?;

        let mut w = BitWriter::new(&mut self.output);
        w.write_bit(true);
        w.write_bits(2, 0b01);
        write_symbols(&mut w, &symbols, &literal_encoder, &distance_encoder);
        w.finish();
        Ok(())
    }

    fn emit_dynamic_block(&mut self) -> Result<()> {
        let symbols = self.matcher.symbols(&self.input);
        let mut literal_frequencies = [0usize; 286];
        let mut distance_frequencies = [0usize; 30];
        let mut has_distance = false;
        for symbol in &symbols {
            match *symbol {
                Lz77Code::Literal(byte) => literal_frequencies[byte as usize] += 1,
                Lz77Code::Pointer { length, distance } => {
                    let len_sym = length_to_symbol(length as u16);
                    literal_frequencies[len_sym.code as usize] += 1;
                    let dist_sym = distance_to_symbol(distance as u16);
                    distance_frequencies[dist_sym.code as usize] += 1;
                    has_distance = true;
                }
            }
        }
        literal_frequencies[END_OF_BLOCK as usize] = 1;
        if !has_distance {
            distance_frequencies[0] = 1;
        }

        let literal_lengths = length_limited_code_lengths(&literal_frequencies, MAX_BITS);
        let distance_lengths = length_limited_code_lengths(&distance_frequencies, MAX_BITS);
        let literal_encoder = HuffmanEncoder::from_code_lengths(&literal_lengths)?;
        let distance_encoder = HuffmanEncoder::from_code_lengths(&distance_lengths)?;

        let literal_code_count = cmp::max(
            257,
            literal_encoder.used_max_symbol().unwrap_or(0) as usize + 1,
        );
        let distance_code_count = cmp::max(
            1,
            distance_encoder.used_max_symbol().unwrap_or(0) as usize + 1,
        );

        let bitwidth_codes = build_bitwidth_codes(
            &literal_encoder,
            literal_code_count,
            &distance_encoder,
            distance_code_count,
        );
        let mut bitwidth_frequencies = [0usize; 19];
        for &(code, _, _) in &bitwidth_codes {
            bitwidth_frequencies[code as usize] += 1;
        }
        let bitwidth_lengths = length_limited_code_lengths(&bitwidth_frequencies, 7);
        let bitwidth_encoder = HuffmanEncoder::from_code_lengths(&bitwidth_lengths)?;
        let bitwidth_code_count = cmp::max(
            4,
            BITWIDTH_CODE_ORDER
                .iter()
                .rposition(|&index| bitwidth_encoder.code_width(index as u16) > 0)
                .map_or(0, |index| index + 1),
        );

        let mut w = BitWriter::new(&mut self.output);
        w.write_bit(true);
        w.write_bits(2, 0b10);
        w.write_bits(5, (literal_code_count - 257) as u16);
        w.write_bits(5, (distance_code_count - 1) as u16);
        w.write_bits(4, (bitwidth_code_count - 4) as u16);
        for &index in BITWIDTH_CODE_ORDER.iter().take(bitwidth_code_count) {
            w.write_bits(3, u16::from(bitwidth_encoder.code_width(index as u16)));
        }
        for &(code, extra_bits, extra) in &bitwidth_codes {
            bitwidth_encoder.encode(&mut w, u16::from(code));
            if extra_bits > 0 {
                w.write_bits(extra_bits, u16::from(extra));
            }
        }
        write_symbols(&mut w, &symbols, &literal_encoder, &distance_encoder);
        w.finish();
        Ok(())
    }
}

fn write_symbols(
    w: &mut BitWriter<'_>,
    symbols: &[Lz77Code],
    literal_encoder: &HuffmanEncoder,
    distance_encoder: &HuffmanEncoder,
) {
    for symbol in symbols {
        match *symbol {
            Lz77Code::Literal(byte) => {
                literal_encoder.encode(w, u16::from(byte));
            }
            Lz77Code::Pointer { length, distance } => {
                let len_sym = length_to_symbol(length as u16);
                literal_encoder.encode(w, len_sym.code);
                if let Some((bits, extra)) = len_sym.extra {
                    w.write_bits(bits, extra);
                }
                let dist_sym = distance_to_symbol(distance as u16);
                distance_encoder.encode(w, dist_sym.code);
                if let Some((bits, extra)) = dist_sym.extra {
                    w.write_bits(bits, extra);
                }
            }
        }
    }
    literal_encoder.encode(w, END_OF_BLOCK);
}

fn build_bitwidth_codes(
    literal: &HuffmanEncoder,
    literal_code_count: usize,
    distance: &HuffmanEncoder,
    distance_code_count: usize,
) -> Vec<(u8, u8, u8)> {
    #[derive(Debug)]
    struct RunLength {
        value: u8,
        count: usize,
    }

    let mut run_lengths = Vec::<RunLength>::new();
    for width in (0..literal_code_count)
        .map(|symbol| literal.code_width(symbol as u16))
        .chain((0..distance_code_count).map(|symbol| distance.code_width(symbol as u16)))
    {
        if run_lengths.last().is_some_and(|run| run.value == width) {
            run_lengths
                .last_mut()
                .expect("run_lengths is non-empty after the last() check")
                .count += 1;
        } else {
            run_lengths.push(RunLength {
                value: width,
                count: 1,
            });
        }
    }

    let mut codes = Vec::new();
    for run in run_lengths {
        if run.value == 0 {
            let mut count = run.count;
            while count >= 11 {
                let amount = cmp::min(138, count) as u8;
                codes.push((18, 7, amount - 11));
                count -= amount as usize;
            }
            if count >= 3 {
                codes.push((17, 3, count as u8 - 3));
                count = 0;
            }
            for _ in 0..count {
                codes.push((0, 0, 0));
            }
        } else {
            codes.push((run.value, 0, 0));
            let mut count = run.count - 1;
            while count >= 3 {
                let amount = cmp::min(6, count) as u8;
                codes.push((16, 2, amount - 3));
                count -= amount as usize;
            }
            for _ in 0..count {
                codes.push((run.value, 0, 0));
            }
        }
    }
    codes
}

#[cfg(test)]
mod tests {
    use super::{EncodeOptions, Encoder};
    use crate::decompress;

    fn compress_with(opts: EncodeOptions, input: &[u8]) -> Vec<u8> {
        let mut e = Encoder::with_options(opts);
        e.feed(input).unwrap();
        e.finish().unwrap();
        assert!(e.is_finished() || !e.output().is_empty());
        let out = e.output().to_vec();
        e.advance(out.len());
        assert!(e.is_finished());
        out
    }

    #[test]
    fn dynamic_roundtrip() {
        let input = b"banana banana banana banana";
        let compressed = compress_with(EncodeOptions::new(), input);
        assert_eq!(decompress(&compressed).unwrap(), input);
    }

    #[test]
    fn fixed_roundtrip() {
        let input = b"hello hello hello";
        let compressed = compress_with(EncodeOptions::new().fixed_huffman(), input);
        assert_eq!(decompress(&compressed).unwrap(), input);
    }

    #[test]
    fn stored_roundtrip() {
        let input = b"this is stored data";
        let compressed = compress_with(EncodeOptions::new().stored(), input);
        assert_eq!(decompress(&compressed).unwrap(), input);
    }

    #[test]
    fn empty_roundtrip() {
        for opts in [
            EncodeOptions::new(),
            EncodeOptions::new().fixed_huffman(),
            EncodeOptions::new().stored(),
        ] {
            let compressed = compress_with(opts, b"");
            assert_eq!(decompress(&compressed).unwrap(), b"");
        }
    }

    #[test]
    fn large_repetitive_compresses() {
        let input = vec![b'a'; 2048];
        let compressed = compress_with(EncodeOptions::new(), &input);
        assert!(compressed.len() < 64);
        assert_eq!(decompress(&compressed).unwrap(), input);
    }

    #[test]
    fn stored_splits_at_0xffff() {
        let input = vec![b'x'; 0xFFFF + 10];
        let compressed = compress_with(EncodeOptions::new().stored(), &input);
        assert_eq!(decompress(&compressed).unwrap(), input);
    }
}
