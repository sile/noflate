//! Streaming sans-io DEFLATE decoder.
//!
//! The caller feeds compressed bytes via [`Decoder::feed`] and pulls
//! decompressed bytes back out via [`Decoder::output`] + [`Decoder::advance`].
//! The decoder runs its internal state machine as far as possible on each
//! `feed` call and waits for more input when a step needs more bits.
//! "Need more bytes" is a no-op return from `feed`, not an error.

use std::borrow::Cow;

use crate::bit::BitReader;
use crate::buf::Buf;
use crate::error::{Error, Result};
use crate::huffman::HuffmanDecoder;
use crate::symbol::{
    BITWIDTH_CODE_ORDER, DISTANCE_TABLE, END_OF_BLOCK, LENGTH_TABLE, fixed_distance_code_lengths,
    fixed_literal_code_lengths,
};

/// Streaming sans-io DEFLATE decoder.
#[derive(Debug)]
pub struct Decoder {
    input: Buf,
    output: Vec<u8>,
    drained: usize,
    state: DecodeState,
    pending_bit_buffer: u64,
    pending_bit_count: u8,
    finished: bool,
}

#[derive(Debug)]
enum DecodeState {
    BlockHeader,
    StoredAlignAndLen {
        is_final: bool,
    },
    StoredBody {
        remaining: u16,
        is_final: bool,
    },
    DynamicHeader {
        is_final: bool,
    },
    DynamicBitwidthTable {
        is_final: bool,
        hlit: u16,
        hdist: u16,
        hclen: u8,
        order_idx: u8,
        code_lengths: [u8; 19],
    },
    DynamicCodeLengths {
        is_final: bool,
        hlit: u16,
        hdist: u16,
        bitwidth_decoder: HuffmanDecoder,
        all_code_lengths: Vec<u8>,
        target_len: usize,
    },
    SymbolLoop {
        is_final: bool,
        literal: HuffmanDecoder,
        distance: HuffmanDecoder,
    },
    Finished,
    /// Placeholder used while transitioning via `std::mem::replace`. Never
    /// left in this state between step calls.
    Transient,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder {
    /// Create a new decoder positioned at the start of a DEFLATE stream.
    pub fn new() -> Self {
        Self {
            input: Buf::new(),
            output: Vec::new(),
            drained: 0,
            state: DecodeState::BlockHeader,
            pending_bit_buffer: 0,
            pending_bit_count: 0,
            finished: false,
        }
    }

    /// Append compressed bytes and advance the decoder as far as possible.
    ///
    /// Returns an error only for genuine stream errors. Running out of
    /// input is not an error: the call returns `Ok(())` and the decoder
    /// waits for more bytes.
    pub fn feed(&mut self, compressed: &[u8]) -> Result<()> {
        if self.finished && !compressed.is_empty() {
            return Err(Error::InvalidData(
                "bytes fed after deflate stream end".into(),
            ));
        }
        self.input.feed(compressed);
        self.drive()
    }

    /// Borrow decompressed bytes that have been produced but not yet
    /// consumed via [`Decoder::advance`].
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

    /// `true` once the final block's EOB has been decoded. Additional
    /// bytes fed after this will cause `Error::InvalidData`.
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    fn drive(&mut self) -> Result<()> {
        let (consumed, residual_buffer, residual_count, finished) = {
            let Self {
                input,
                output,
                state,
                pending_bit_buffer,
                pending_bit_count,
                ..
            } = self;
            let mut reader = BitReader::new_seeded(
                input.get(),
                *pending_bit_buffer,
                *pending_bit_count,
            );
            let mut finished = false;
            loop {
                match step(&mut reader, state, output)? {
                    StepOutcome::Progress => continue,
                    StepOutcome::NeedMoreBytes => break,
                    StepOutcome::Finished => {
                        finished = true;
                        break;
                    }
                }
            }
            (
                reader.committed_bytes(),
                reader.residual_bit_buffer(),
                reader.residual_bit_count(),
                finished,
            )
        };
        self.input.advance(consumed);
        self.pending_bit_buffer = residual_buffer;
        self.pending_bit_count = residual_count;
        if finished {
            self.finished = true;
        }
        Ok(())
    }
}

fn step(
    reader: &mut BitReader<'_>,
    state: &mut DecodeState,
    output: &mut Vec<u8>,
) -> Result<StepOutcome> {
    let current = std::mem::replace(state, DecodeState::Transient);
    match current {
        DecodeState::BlockHeader => step_block_header(reader, state),
        DecodeState::StoredAlignAndLen { is_final } => {
            step_stored_align_and_len(reader, state, is_final)
        }
        DecodeState::StoredBody {
            remaining,
            is_final,
        } => step_stored_body(reader, state, output, remaining, is_final),
        DecodeState::DynamicHeader { is_final } => step_dynamic_header(reader, state, is_final),
        DecodeState::DynamicBitwidthTable {
            is_final,
            hlit,
            hdist,
            hclen,
            order_idx,
            code_lengths,
        } => step_dynamic_bitwidth_table(
            reader,
            state,
            is_final,
            hlit,
            hdist,
            hclen,
            order_idx,
            code_lengths,
        ),
        DecodeState::DynamicCodeLengths {
            is_final,
            hlit,
            hdist,
            bitwidth_decoder,
            all_code_lengths,
            target_len,
        } => step_dynamic_code_lengths(
            reader,
            state,
            is_final,
            hlit,
            hdist,
            bitwidth_decoder,
            all_code_lengths,
            target_len,
        ),
        DecodeState::SymbolLoop {
            is_final,
            literal,
            distance,
        } => step_symbol_loop(reader, state, output, is_final, literal, distance),
        DecodeState::Finished => {
            *state = DecodeState::Finished;
            Ok(StepOutcome::Finished)
        }
        DecodeState::Transient => unreachable!("decoder left in transient state"),
    }
}

fn step_block_header(
    reader: &mut BitReader<'_>,
    state: &mut DecodeState,
) -> Result<StepOutcome> {
    let snap = reader.snapshot();
    if reader.available_bits() < 3 {
        *state = DecodeState::BlockHeader;
        reader.restore(snap);
        return Ok(StepOutcome::NeedMoreBytes);
    }
    let is_final = reader.read_bit()?;
    let block_type = reader.read_bits(2)?;
    match block_type {
        0b00 => {
            *state = DecodeState::StoredAlignAndLen { is_final };
        }
        0b01 => {
            let literal = HuffmanDecoder::from_code_lengths(
                &fixed_literal_code_lengths(),
                None,
                Some(END_OF_BLOCK),
            )?;
            let distance = HuffmanDecoder::from_code_lengths(
                &fixed_distance_code_lengths(),
                Some(7),
                None,
            )?;
            *state = DecodeState::SymbolLoop {
                is_final,
                literal,
                distance,
            };
        }
        0b10 => {
            *state = DecodeState::DynamicHeader { is_final };
        }
        _ => {
            return Err(Error::InvalidData("reserved DEFLATE block type".into()));
        }
    }
    Ok(StepOutcome::Progress)
}

fn step_stored_align_and_len(
    reader: &mut BitReader<'_>,
    state: &mut DecodeState,
    is_final: bool,
) -> Result<StepOutcome> {
    let snap = reader.snapshot();
    let residual = reader.residual_bit_count() % 8;
    let required_bits = residual as usize + 32;
    if reader.available_bits() < required_bits {
        *state = DecodeState::StoredAlignAndLen { is_final };
        reader.restore(snap);
        return Ok(StepOutcome::NeedMoreBytes);
    }
    reader.align_to_byte();
    let bytes = match reader.read_bytes(4) {
        Ok(b) => b,
        Err(_) => {
            reader.restore(snap);
            *state = DecodeState::StoredAlignAndLen { is_final };
            return Ok(StepOutcome::NeedMoreBytes);
        }
    };
    let len = u16::from_le_bytes([bytes[0], bytes[1]]);
    let nlen = u16::from_le_bytes([bytes[2], bytes[3]]);
    if !len != nlen {
        return Err(Error::InvalidData(Cow::Owned(format!(
            "LEN={len} is not the one's complement of NLEN={nlen}"
        ))));
    }
    *state = DecodeState::StoredBody {
        remaining: len,
        is_final,
    };
    Ok(StepOutcome::Progress)
}

fn step_stored_body(
    reader: &mut BitReader<'_>,
    state: &mut DecodeState,
    output: &mut Vec<u8>,
    remaining: u16,
    is_final: bool,
) -> Result<StepOutcome> {
    if remaining == 0 {
        if is_final {
            *state = DecodeState::Finished;
            return Ok(StepOutcome::Finished);
        }
        *state = DecodeState::BlockHeader;
        return Ok(StepOutcome::Progress);
    }
    let available = reader.available_bits() / 8;
    if available == 0 {
        *state = DecodeState::StoredBody {
            remaining,
            is_final,
        };
        return Ok(StepOutcome::NeedMoreBytes);
    }
    let take = available.min(remaining as usize);
    let bytes = reader.read_bytes(take)?;
    output.extend_from_slice(bytes);
    let new_remaining = remaining - take as u16;
    *state = DecodeState::StoredBody {
        remaining: new_remaining,
        is_final,
    };
    Ok(StepOutcome::Progress)
}

fn step_dynamic_header(
    reader: &mut BitReader<'_>,
    state: &mut DecodeState,
    is_final: bool,
) -> Result<StepOutcome> {
    let snap = reader.snapshot();
    if reader.available_bits() < 14 {
        *state = DecodeState::DynamicHeader { is_final };
        reader.restore(snap);
        return Ok(StepOutcome::NeedMoreBytes);
    }
    let hlit = reader.read_bits(5)? + 257;
    let hdist = reader.read_bits(5)? + 1;
    let hclen = reader.read_bits(4)? as u8 + 4;
    if hdist as usize > DISTANCE_TABLE.len() {
        return Err(Error::InvalidData(Cow::Owned(format!(
            "HDIST is too large: {hdist}"
        ))));
    }
    *state = DecodeState::DynamicBitwidthTable {
        is_final,
        hlit,
        hdist,
        hclen,
        order_idx: 0,
        code_lengths: [0u8; 19],
    };
    Ok(StepOutcome::Progress)
}

#[allow(clippy::too_many_arguments)]
fn step_dynamic_bitwidth_table(
    reader: &mut BitReader<'_>,
    state: &mut DecodeState,
    is_final: bool,
    hlit: u16,
    hdist: u16,
    hclen: u8,
    mut order_idx: u8,
    mut code_lengths: [u8; 19],
) -> Result<StepOutcome> {
    while order_idx < hclen {
        let snap = reader.snapshot();
        if reader.available_bits() < 3 {
            *state = DecodeState::DynamicBitwidthTable {
                is_final,
                hlit,
                hdist,
                hclen,
                order_idx,
                code_lengths,
            };
            reader.restore(snap);
            return Ok(StepOutcome::NeedMoreBytes);
        }
        let width = reader.read_bits(3)? as u8;
        let slot = BITWIDTH_CODE_ORDER[order_idx as usize];
        code_lengths[slot] = width;
        order_idx += 1;
    }
    let bitwidth_decoder = HuffmanDecoder::from_code_lengths(&code_lengths, Some(1), None)?;
    let target_len = hlit as usize + hdist as usize;
    *state = DecodeState::DynamicCodeLengths {
        is_final,
        hlit,
        hdist,
        bitwidth_decoder,
        all_code_lengths: Vec::with_capacity(target_len),
        target_len,
    };
    Ok(StepOutcome::Progress)
}

#[allow(clippy::too_many_arguments)]
fn step_dynamic_code_lengths(
    reader: &mut BitReader<'_>,
    state: &mut DecodeState,
    is_final: bool,
    hlit: u16,
    hdist: u16,
    bitwidth_decoder: HuffmanDecoder,
    mut all_code_lengths: Vec<u8>,
    target_len: usize,
) -> Result<StepOutcome> {
    while all_code_lengths.len() < target_len {
        let snap = reader.snapshot();
        // No conservative pre-check here: each RLE element may consume as
        // little as 1 bit (a width-1 code with no extras), so we rely on
        // per-read EOF rollback below.
        let code = match bitwidth_decoder.decode(reader) {
            Ok(v) => v,
            Err(e) if is_eof_error(&e) => {
                reader.restore(snap);
                *state = DecodeState::DynamicCodeLengths {
                    is_final,
                    hlit,
                    hdist,
                    bitwidth_decoder,
                    all_code_lengths,
                    target_len,
                };
                return Ok(StepOutcome::NeedMoreBytes);
            }
            Err(e) => return Err(e),
        };
        match code {
            0..=15 => all_code_lengths.push(code as u8),
            16 => {
                let repeat = match reader.read_bits(2) {
                    Ok(v) => v + 3,
                    Err(e) if is_eof_error(&e) => {
                        reader.restore(snap);
                        *state = DecodeState::DynamicCodeLengths {
                            is_final,
                            hlit,
                            hdist,
                            bitwidth_decoder,
                            all_code_lengths,
                            target_len,
                        };
                        return Ok(StepOutcome::NeedMoreBytes);
                    }
                    Err(e) => return Err(e),
                };
                let Some(&last) = all_code_lengths.last() else {
                    return Err(Error::InvalidData(
                        "repeat code 16 without a previous code".into(),
                    ));
                };
                all_code_lengths.extend(std::iter::repeat_n(last, repeat as usize));
            }
            17 => {
                let repeat = match reader.read_bits(3) {
                    Ok(v) => v + 3,
                    Err(e) if is_eof_error(&e) => {
                        reader.restore(snap);
                        *state = DecodeState::DynamicCodeLengths {
                            is_final,
                            hlit,
                            hdist,
                            bitwidth_decoder,
                            all_code_lengths,
                            target_len,
                        };
                        return Ok(StepOutcome::NeedMoreBytes);
                    }
                    Err(e) => return Err(e),
                };
                all_code_lengths.extend(std::iter::repeat_n(0, repeat as usize));
            }
            18 => {
                let repeat = match reader.read_bits(7) {
                    Ok(v) => v + 11,
                    Err(e) if is_eof_error(&e) => {
                        reader.restore(snap);
                        *state = DecodeState::DynamicCodeLengths {
                            is_final,
                            hlit,
                            hdist,
                            bitwidth_decoder,
                            all_code_lengths,
                            target_len,
                        };
                        return Ok(StepOutcome::NeedMoreBytes);
                    }
                    Err(e) => return Err(e),
                };
                all_code_lengths.extend(std::iter::repeat_n(0, repeat as usize));
            }
            _ => {
                return Err(Error::InvalidData(Cow::Owned(format!(
                    "invalid code length symbol: {code}"
                ))));
            }
        }
        if all_code_lengths.len() > target_len {
            return Err(Error::InvalidData(
                "dynamic huffman code lengths exceed the announced table size".into(),
            ));
        }
    }
    let literal_lengths = &all_code_lengths[..hlit as usize];
    let distance_lengths = &all_code_lengths[hlit as usize..hlit as usize + hdist as usize];
    let literal = HuffmanDecoder::from_code_lengths(literal_lengths, None, Some(END_OF_BLOCK))?;
    let distance = HuffmanDecoder::from_code_lengths(
        distance_lengths,
        Some(literal.safely_peek_bits()),
        None,
    )?;
    *state = DecodeState::SymbolLoop {
        is_final,
        literal,
        distance,
    };
    Ok(StepOutcome::Progress)
}

fn step_symbol_loop(
    reader: &mut BitReader<'_>,
    state: &mut DecodeState,
    output: &mut Vec<u8>,
    is_final: bool,
    literal: HuffmanDecoder,
    distance: HuffmanDecoder,
) -> Result<StepOutcome> {
    loop {
        let snap = reader.snapshot();
        if reader.available_bits() < literal.safely_peek_bits() as usize {
            *state = DecodeState::SymbolLoop {
                is_final,
                literal,
                distance,
            };
            reader.restore(snap);
            return Ok(StepOutcome::NeedMoreBytes);
        }
        let symbol = match literal.decode(reader) {
            Ok(s) => s,
            Err(e) if is_eof_error(&e) => {
                reader.restore(snap);
                *state = DecodeState::SymbolLoop {
                    is_final,
                    literal,
                    distance,
                };
                return Ok(StepOutcome::NeedMoreBytes);
            }
            Err(e) => return Err(e),
        };
        match symbol {
            0..=255 => output.push(symbol as u8),
            END_OF_BLOCK => {
                if is_final {
                    *state = DecodeState::Finished;
                    return Ok(StepOutcome::Finished);
                }
                *state = DecodeState::BlockHeader;
                return Ok(StepOutcome::Progress);
            }
            257..=285 => {
                let (base_length, length_extra_bits) = LENGTH_TABLE[(symbol - 257) as usize];
                let length_extra = if length_extra_bits == 0 {
                    0
                } else {
                    match reader.read_bits(length_extra_bits) {
                        Ok(v) => v,
                        Err(e) if is_eof_error(&e) => {
                            reader.restore(snap);
                            *state = DecodeState::SymbolLoop {
                                is_final,
                                literal,
                                distance,
                            };
                            return Ok(StepOutcome::NeedMoreBytes);
                        }
                        Err(e) => return Err(e),
                    }
                };
                let length = base_length + length_extra;
                let distance_symbol = match distance.decode(reader) {
                    Ok(s) => s,
                    Err(e) if is_eof_error(&e) => {
                        reader.restore(snap);
                        *state = DecodeState::SymbolLoop {
                            is_final,
                            literal,
                            distance,
                        };
                        return Ok(StepOutcome::NeedMoreBytes);
                    }
                    Err(e) => return Err(e),
                };
                let Some(&(base_distance, dist_extra_bits)) =
                    DISTANCE_TABLE.get(distance_symbol as usize)
                else {
                    return Err(Error::InvalidData(Cow::Owned(format!(
                        "invalid distance symbol: {distance_symbol}"
                    ))));
                };
                let dist_extra = if dist_extra_bits == 0 {
                    0
                } else {
                    match reader.read_bits(dist_extra_bits) {
                        Ok(v) => v,
                        Err(e) if is_eof_error(&e) => {
                            reader.restore(snap);
                            *state = DecodeState::SymbolLoop {
                                is_final,
                                literal,
                                distance,
                            };
                            return Ok(StepOutcome::NeedMoreBytes);
                        }
                        Err(e) => return Err(e),
                    }
                };
                let full_distance = (base_distance + dist_extra) as usize;
                copy_from_distance(output, full_distance, length as usize)?;
            }
            286 | 287 => {
                return Err(Error::InvalidData(Cow::Owned(format!(
                    "literal/length symbol {symbol} must not appear in compressed data"
                ))));
            }
            _ => unreachable!("literal/length symbol out of range: {symbol}"),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum StepOutcome {
    Progress,
    NeedMoreBytes,
    Finished,
}

fn is_eof_error(e: &Error) -> bool {
    matches!(e, Error::InvalidData(msg) if msg.as_ref() == "unexpected end of deflate stream")
}

fn copy_from_distance(output: &mut Vec<u8>, distance: usize, length: usize) -> Result<()> {
    if distance == 0 || distance > output.len() {
        return Err(Error::InvalidData(Cow::Owned(format!(
            "too long backward reference: output_len={}, distance={}",
            output.len(),
            distance
        ))));
    }
    let start = output.len() - distance;
    if distance >= length {
        output.extend_from_within(start..start + length);
    } else {
        // Overlapping: the pattern at the tail is `distance` bytes wide
        // initially and grows by whatever we emit each iteration. We
        // exploit that by doubling: each iteration copies up to the full
        // current tail, giving O(log(length / distance)) extend calls
        // instead of O(length / distance).
        output.reserve(length);
        let mut emitted = 0usize;
        while emitted < length {
            let tail_len = distance + emitted;
            let take = tail_len.min(length - emitted);
            let src_start = output.len() - tail_len;
            output.extend_from_within(src_start..src_start + take);
            emitted += take;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::Decoder;

    fn decompress_once(input: &[u8]) -> Vec<u8> {
        let mut d = Decoder::new();
        d.feed(input).expect("feed");
        assert!(d.is_finished(), "stream did not finish");
        let out = d.output().to_vec();
        d.advance(out.len());
        out
    }

    #[test]
    fn decode_known_fixed_block() {
        let input = [243, 72, 205, 201, 201, 87, 8, 207, 47, 202, 73, 81, 4, 0];
        assert_eq!(decompress_once(&input), b"Hello World!");
    }

    #[test]
    fn decode_known_raw_block() {
        let input = [
            1, 12, 0, 243, 255, 72, 101, 108, 108, 111, 32, 87, 111, 114, 108, 100, 33,
        ];
        assert_eq!(decompress_once(&input), b"Hello World!");
    }

    #[test]
    fn decode_known_dynamic_block() {
        let input = [75, 76, 42, 74, 76, 78, 76, 73, 4, 82, 10, 137, 216, 217, 0];
        assert_eq!(decompress_once(&input), b"abracadabra abracadabra abracadabra");
    }

    #[test]
    fn reserved_block_type_errors() {
        let input = [0x07];
        let mut d = Decoder::new();
        assert!(d.feed(&input).is_err());
    }

    #[test]
    fn byte_by_byte_feed_matches_whole_at_once() {
        let input = [243, 72, 205, 201, 201, 87, 8, 207, 47, 202, 73, 81, 4, 0];
        let mut d = Decoder::new();
        for &byte in &input {
            d.feed(&[byte]).expect("feed");
        }
        assert!(d.is_finished());
        let out = d.output().to_vec();
        d.advance(out.len());
        assert_eq!(out, b"Hello World!");
    }
}
