//! Streaming DEFLATE encoder.
//!
//! The caller feeds uncompressed bytes via [`Encoder::feed`], then calls
//! [`Encoder::finish`] to emit the final block. Compressed bytes are
//! borrowed via [`Encoder::output`] and acknowledged via [`Encoder::advance`].
//!
//! By default the encoder splits input into 64 KiB blocks, emitting each
//! as a fixed- or dynamic-Huffman block (or a sequence of stored sub-blocks)
//! during [`Encoder::feed`]. This keeps memory usage bounded for streaming
//! workloads. For one-shot compression use [`EncodeOptions::buffer_all_input`]
//! to gather all input into a single block.

use alloc::vec::Vec;
use core::cmp;
use core::mem;

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

/// Default maximum input bytes per DEFLATE block for streaming encoders.
const DEFAULT_MAX_BLOCK_INPUT: usize = 64 * 1024;

/// Compact the output buffer once the consumed prefix exceeds this size.
///
/// The encoder output has no back-reference requirement, so we drain the
/// consumed prefix entirely. Amortized cost is one `Vec::drain` per
/// `COMPACT_THRESHOLD` bytes consumed; callers that never reach the
/// threshold pay nothing.
const COMPACT_THRESHOLD: usize = 1024 * 1024;

/// Configurable parameters for the encoder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodeOptions {
    block_kind: BlockKind,
    max_block_input_bytes: Option<usize>,
}

impl EncodeOptions {
    /// Default options: dynamic Huffman blocks.
    pub fn new() -> Self {
        Self {
            block_kind: BlockKind::Dynamic,
            max_block_input_bytes: Some(DEFAULT_MAX_BLOCK_INPUT),
        }
    }

    /// Use fixed-Huffman blocks instead of dynamic.
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

    /// Buffer all input before compressing (one DEFLATE block).
    ///
    /// Disables the default 64 KiB block splitting. Best for one-shot
    /// compression where all input is available up front.
    #[must_use]
    pub fn buffer_all_input(mut self) -> Self {
        self.max_block_input_bytes = None;
        self
    }

    /// Set the maximum input bytes per DEFLATE block.
    ///
    /// When the encoder's internal buffer reaches this threshold during
    /// [`Encoder::feed`], it emits a non-final DEFLATE block and frees
    /// the buffered input. The default is 64 KiB.
    #[must_use]
    pub fn max_block_input_bytes(mut self, bytes: usize) -> Self {
        assert!(bytes > 0, "max_block_input_bytes must be positive");
        self.max_block_input_bytes = Some(bytes);
        self
    }
}

impl Default for EncodeOptions {
    fn default() -> Self {
        Self::new()
    }
}

/// Streaming DEFLATE encoder.
///
/// By default the encoder emits a non-final DEFLATE block each time the
/// buffered input reaches 64 KiB, keeping memory bounded for streaming
/// workloads. Call [`Encoder::finish`] to emit the final block.
#[derive(Debug)]
pub struct Encoder {
    options: EncodeOptions,
    input: Vec<u8>,
    output: Vec<u8>,
    matcher: MatchFinder,
    symbols: Vec<Lz77Code>,
    drained: usize,
    finishing: bool,
    bit_buffer: u64,
    bit_count: u8,
}

impl Default for Encoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Encoder {
    /// Create a DEFLATE encoder with default options.
    pub fn new() -> Self {
        Self::with_options(EncodeOptions::new())
    }

    /// Create a DEFLATE encoder with custom options.
    pub fn with_options(options: EncodeOptions) -> Self {
        Self {
            options,
            input: Vec::new(),
            output: Vec::new(),
            matcher: MatchFinder::new(),
            symbols: Vec::new(),
            drained: 0,
            finishing: false,
            bit_buffer: 0,
            bit_count: 0,
        }
    }

    /// Append uncompressed bytes.
    ///
    /// When the buffered input reaches the configured block size (default
    /// 64 KiB), a non-final DEFLATE block is emitted automatically.
    /// Calling `feed` after `finish` returns an error.
    pub fn feed(&mut self, data: &[u8]) -> Result<()> {
        if self.finishing {
            return Err(crate::error::Error::InvalidData(
                "bytes fed after encoder finish".into(),
            ));
        }
        self.input.extend_from_slice(data);
        if let Some(limit) = self.options.max_block_input_bytes {
            while self.input.len() >= limit {
                let tail = self.input.split_off(limit);
                let chunk = mem::replace(&mut self.input, tail);
                self.emit_block_chunk(&chunk, false)?;
            }
        }
        Ok(())
    }

    /// Emit the final block. Subsequent calls are a no-op.
    pub fn finish(&mut self) -> Result<()> {
        if self.finishing {
            return Ok(());
        }
        self.finishing = true;
        let chunk = mem::take(&mut self.input);
        self.emit_block_chunk(&chunk, true)?;
        Ok(())
    }

    /// Flush pending input as a non-final block and append a sync flush
    /// marker so the stream ends on a byte boundary with the 4-byte trailer
    /// `0x00 0x00 0xFF 0xFF` (an empty stored block with BFINAL=0).
    ///
    /// The stream remains continuable: subsequent [`Encoder::feed`] calls
    /// append further blocks, and [`Encoder::finish`] still emits the
    /// final block.
    ///
    /// This method leaves the 4-byte trailer in [`Encoder::output`]. For
    /// WebSocket `permessage-deflate` (RFC 7692), callers typically send the
    /// output *without* that trailer and append it back before decoding:
    ///
    /// ```rust
    /// let mut enc = noflate::deflate::Encoder::new();
    /// enc.feed(b"hello")?;
    /// enc.sync_flush()?;
    ///
    /// let mut frame = enc.output().to_vec();
    /// enc.advance(frame.len());
    /// assert!(frame.ends_with(&[0x00, 0x00, 0xFF, 0xFF]));
    /// frame.truncate(frame.len() - 4); // strip per RFC 7692
    ///
    /// let mut dec = noflate::deflate::Decoder::new();
    /// dec.feed(&frame)?;
    /// dec.feed(&[0x00, 0x00, 0xFF, 0xFF])?;
    /// let out = dec.output().to_vec();
    /// dec.advance(out.len());
    /// assert_eq!(out, b"hello");
    /// # Ok::<(), noflate::Error>(())
    /// ```
    ///
    /// Intended for protocols that frame messages over a single DEFLATE
    /// stream, such as WebSocket `permessage-deflate` (RFC 7692). Returns an
    /// error if called after [`Encoder::finish`].
    pub fn sync_flush(&mut self) -> Result<()> {
        if self.finishing {
            return Err(crate::error::Error::InvalidData(
                "sync_flush called after encoder finish".into(),
            ));
        }
        if !self.input.is_empty() {
            let chunk = mem::take(&mut self.input);
            self.emit_block_chunk(&chunk, false)?;
        }
        self.emit_stored_chunk(&[], false)
    }

    /// Drop the LZ77 sliding window so subsequent blocks encode no
    /// back-references into input fed before this call.
    ///
    /// Intended for `permessage-deflate` senders that negotiated
    /// `server_no_context_takeover` or `client_no_context_takeover`
    /// (RFC 7692 §7.1.1): call after [`Encoder::sync_flush`] at each
    /// message boundary. Does not touch pending output, options, or the
    /// bit buffer.
    pub fn reset_history(&mut self) {
        self.matcher = MatchFinder::new();
    }

    /// Borrow encoded bytes not yet consumed.
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
        if self.drained >= COMPACT_THRESHOLD {
            self.output.drain(..self.drained);
            self.drained = 0;
        }
    }

    /// `true` once `finish` has been called and all output consumed.
    pub fn is_finished(&self) -> bool {
        self.finishing && self.drained == self.output.len()
    }

    fn emit_block_chunk(&mut self, chunk: &[u8], is_final: bool) -> Result<()> {
        match self.options.block_kind {
            BlockKind::Stored => self.emit_stored_chunk(chunk, is_final),
            BlockKind::Fixed => self.emit_fixed_block_chunk(chunk, is_final),
            BlockKind::Dynamic => self.emit_dynamic_block_chunk(chunk, is_final),
        }
    }

    fn emit_stored_chunk(&mut self, chunk: &[u8], is_final: bool) -> Result<()> {
        let total = chunk.len();
        if total == 0 {
            {
                let mut w =
                    BitWriter::new_seeded(&mut self.output, self.bit_buffer, self.bit_count);
                w.write_bit(is_final);
                w.write_bits(2, 0b00);
                w.align_to_byte();
            }
            self.bit_buffer = 0;
            self.bit_count = 0;
            self.output.extend_from_slice(&[0, 0, 0xFF, 0xFF]);
            return Ok(());
        }
        let mut offset = 0usize;
        while offset < total {
            let sub_len = cmp::min(MAX_STORED_BLOCK, total - offset);
            let is_last = offset + sub_len == total;
            {
                let mut w =
                    BitWriter::new_seeded(&mut self.output, self.bit_buffer, self.bit_count);
                w.write_bit(is_final && is_last);
                w.write_bits(2, 0b00);
                w.align_to_byte();
            }
            self.bit_buffer = 0;
            self.bit_count = 0;
            let len = sub_len as u16;
            let nlen = !len;
            self.output.extend_from_slice(&len.to_le_bytes());
            self.output.extend_from_slice(&nlen.to_le_bytes());
            self.output
                .extend_from_slice(&chunk[offset..offset + sub_len]);
            offset += sub_len;
        }
        Ok(())
    }

    fn emit_fixed_block_chunk(&mut self, chunk: &[u8], is_final: bool) -> Result<()> {
        self.matcher.fill_symbols(chunk, &mut self.symbols);
        let literal_lengths = fixed_literal_code_lengths();
        let distance_lengths = fixed_distance_code_lengths();
        let literal_encoder = HuffmanEncoder::from_code_lengths(&literal_lengths)?;
        let distance_encoder = HuffmanEncoder::from_code_lengths(&distance_lengths)?;

        let mut w = BitWriter::new_seeded(&mut self.output, self.bit_buffer, self.bit_count);
        w.write_bit(is_final);
        w.write_bits(2, 0b01);
        write_symbols(&mut w, &self.symbols, &literal_encoder, &distance_encoder);
        if is_final {
            w.finish();
            self.bit_buffer = 0;
            self.bit_count = 0;
        } else {
            (self.bit_buffer, self.bit_count) = w.bit_state();
        }
        Ok(())
    }

    fn emit_dynamic_block_chunk(&mut self, chunk: &[u8], is_final: bool) -> Result<()> {
        self.matcher.fill_symbols(chunk, &mut self.symbols);
        let mut literal_frequencies = [0usize; 286];
        let mut distance_frequencies = [0usize; 30];
        let mut has_distance = false;
        for symbol in &self.symbols {
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

        let mut w = BitWriter::new_seeded(&mut self.output, self.bit_buffer, self.bit_count);
        w.write_bit(is_final);
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
        write_symbols(&mut w, &self.symbols, &literal_encoder, &distance_encoder);
        if is_final {
            w.finish();
            self.bit_buffer = 0;
            self.bit_count = 0;
        } else {
            (self.bit_buffer, self.bit_count) = w.bit_state();
        }
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
    use alloc::vec;
    use alloc::vec::Vec;

    use super::{EncodeOptions, Encoder};
    use crate::deflate::{Decoder, decompress};

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

    #[test]
    fn feed_produces_output_during_block_split() {
        let input = vec![b'a'; 128 * 1024];
        let mut e = Encoder::new();
        e.feed(&input).unwrap();
        assert!(
            !e.output().is_empty(),
            "expected intermediate output from block splitting"
        );
        e.finish().unwrap();
        let out = e.output().to_vec();
        e.advance(out.len());
        assert!(e.is_finished());
        assert_eq!(decompress(&out).unwrap(), input);
    }

    #[test]
    fn buffer_all_input_no_intermediate_output() {
        let input = vec![b'a'; 128 * 1024];
        let mut e = Encoder::with_options(EncodeOptions::new().buffer_all_input());
        e.feed(&input).unwrap();
        assert!(
            e.output().is_empty(),
            "buffer_all_input should not produce output during feed"
        );
        e.finish().unwrap();
        let out = e.output().to_vec();
        e.advance(out.len());
        assert!(e.is_finished());
        assert_eq!(decompress(&out).unwrap(), input);
    }

    #[test]
    fn streaming_roundtrip_large_input() {
        let input: Vec<u8> = (0..150_000).map(|i| (i * 37 + 13) as u8).collect();
        for opts in [
            EncodeOptions::new(),
            EncodeOptions::new().fixed_huffman(),
            EncodeOptions::new().stored(),
        ] {
            let compressed = compress_with(opts.clone(), &input);
            assert_eq!(decompress(&compressed).unwrap(), input);
        }
    }

    #[test]
    fn sync_flush_marker_is_empty_stored_block() {
        let mut e = Encoder::new();
        e.sync_flush().unwrap();
        // From bit state (0, 0): write_bit(false) + write_bits(2, 0b00)
        // + align_to_byte pads out to one 0x00 byte; then the 4-byte
        // trailer is appended literally.
        assert_eq!(e.output(), &[0x00, 0x00, 0x00, 0xFF, 0xFF]);
    }

    #[test]
    fn sync_flush_then_finish_roundtrip() {
        for opts in [
            EncodeOptions::new(),
            EncodeOptions::new().fixed_huffman(),
            EncodeOptions::new().stored(),
        ] {
            let mut e = Encoder::with_options(opts);
            e.feed(b"hello ").unwrap();
            e.sync_flush().unwrap();
            e.feed(b"world").unwrap();
            e.finish().unwrap();
            let out = e.output().to_vec();
            e.advance(out.len());
            assert!(e.is_finished());
            assert_eq!(decompress(&out).unwrap(), b"hello world");
        }
    }

    #[test]
    fn permessage_deflate_style_framing_roundtrip() {
        // Mirror the RFC 7692 sender/receiver pattern: strip the 4-byte
        // trailer per message, re-append on the receiver, decode streaming.
        let messages: &[&[u8]] = &[
            b"the quick brown fox",
            b"jumps over the lazy dog",
            b"hello again",
        ];
        let mut e = Encoder::new();
        let mut wire: Vec<Vec<u8>> = Vec::new();
        for msg in messages {
            e.feed(msg).unwrap();
            e.sync_flush().unwrap();
            let mut frame = e.output().to_vec();
            e.advance(frame.len());
            assert!(frame.ends_with(&[0x00, 0x00, 0xFF, 0xFF]));
            frame.truncate(frame.len() - 4);
            wire.push(frame);
        }
        let mut d = Decoder::new();
        for frame in &wire {
            d.feed(frame).unwrap();
            d.feed(&[0x00, 0x00, 0xFF, 0xFF]).unwrap();
        }
        let decoded = d.output().to_vec();
        let expected: Vec<u8> = messages.iter().flat_map(|m| m.iter().copied()).collect();
        assert_eq!(decoded, expected);
    }

    #[test]
    fn reset_history_drops_backrefs_to_prior_input() {
        // With a cross-message-repeating payload, reset_history must
        // produce the same bytes as a fresh encoder — the LZ77 matcher
        // cannot see the first message's bytes anymore.
        let payload = b"abcdefghijklmnopqrstuvwxyz0123456789";

        let mut fresh = Encoder::new();
        fresh.feed(payload).unwrap();
        fresh.sync_flush().unwrap();
        let baseline = fresh.output().to_vec();

        let mut e = Encoder::new();
        e.feed(payload).unwrap();
        e.sync_flush().unwrap();
        let first_len = e.output().len();
        e.advance(first_len);
        e.reset_history();
        e.feed(payload).unwrap();
        e.sync_flush().unwrap();
        let second = e.output().to_vec();
        e.advance(second.len());
        assert_eq!(second, baseline);
    }

    #[test]
    fn sync_flush_after_finish_errors() {
        let mut e = Encoder::new();
        e.feed(b"data").unwrap();
        e.finish().unwrap();
        assert!(e.sync_flush().is_err());
    }

    #[test]
    fn advance_compacts_output_buffer() {
        // Regression for https://github.com/sile/noflate/issues/1: the
        // encoder's output Vec must drop consumed bytes so memory does
        // not grow without bound during large streaming encodes.
        let mut e = Encoder::with_options(EncodeOptions::new().stored());
        let chunk = vec![b'x'; 64 * 1024];
        let mut total_consumed = 0usize;
        let mut max_internal = 0usize;
        // Drive ~10 MiB through the encoder, draining after each feed.
        for _ in 0..160 {
            e.feed(&chunk).unwrap();
            let out = e.output().to_vec();
            total_consumed += out.len();
            e.advance(out.len());
            max_internal = max_internal.max(e.output.len());
        }
        e.finish().unwrap();
        let tail = e.output().to_vec();
        total_consumed += tail.len();
        e.advance(tail.len());
        assert!(e.is_finished());
        assert!(total_consumed > 10 * 1024 * 1024);
        assert!(
            max_internal < 2 * 1024 * 1024,
            "internal output buffer grew to {max_internal} bytes"
        );
    }
}
