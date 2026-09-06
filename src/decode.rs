//! Streaming DEFLATE decoder.
//!
//! The caller feeds compressed bytes via [`Decoder::feed`] and pulls
//! decompressed bytes back out via [`Decoder::output`] + [`Decoder::advance`].
//! The decoder runs its internal state machine as far as possible on each
//! `feed` call and waits for more input when a step needs more bits.
//! "Need more bytes" is a no-op return from `feed`, not an error.
//!
//! To keep memory bounded, `feed` may also return early once the decoder has
//! buffered [`MAX_INTERNAL_BUFFER`] decompressed bytes that the caller has
//! not yet drained — a single block can expand arbitrarily ([RFC 1951] has
//! no per-block maximum), and without this cap the whole block would be
//! buffered before control returned. Drain via `output` / `advance`, then
//! call `feed` again (with remaining input, or an empty slice once the
//! compressed data is exhausted) to resume decoding.
//!
//! [RFC 1951]: https://tools.ietf.org/html/rfc1951

use alloc::borrow::Cow;
use alloc::format;
use alloc::vec::Vec;

use crate::bit::BitReader;
use crate::buf::Buf;
use crate::error::{Error, Result};
use crate::huffman::HuffmanDecoder;
use crate::symbol::{
    BITWIDTH_CODE_ORDER, DISTANCE_TABLE, END_OF_BLOCK, LENGTH_TABLE, WINDOW_SIZE,
    fixed_distance_code_lengths, fixed_literal_code_lengths,
};

/// Compact the output buffer once it exceeds this size.
///
/// Amortized cost is one `copy_within` of at most `WINDOW_SIZE` + unconsumed
/// bytes per `COMPACT_THRESHOLD` bytes decoded — negligible compared to the
/// decoding work itself. Smaller streams never hit the threshold, so the
/// common case pays nothing.
const COMPACT_THRESHOLD: usize = 1024 * 1024;

/// Maximum number of decompressed bytes the decoder buffers before yielding
/// control back to the caller.
///
/// A single DEFLATE block can expand to an arbitrarily large size (RFC 1951
/// has no per-block maximum). Without this cap, one highly-compressible
/// block would be fully expanded into the internal output buffer before the
/// caller could drain it via [`Decoder::output`] / [`Decoder::advance`],
/// allowing memory to grow without bound. Once the unread output reaches this
/// threshold the decoder stops and returns from [`Decoder::feed`]; callers
/// drain the buffered bytes and call `feed` again (with remaining input, or
/// an empty slice once the compressed data is exhausted) to resume.
///
/// The cap is checked between symbols, so the buffer may overshoot by up to
/// one LZ77 match (`MAX_MATCH`, 258 bytes) before control returns.
const MAX_INTERNAL_BUFFER: usize = 64 * 1024;

/// Streaming DEFLATE decoder.
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
    /// Create a DEFLATE decoder positioned at the start of a stream.
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

    /// Append compressed bytes.
    ///
    /// Returns an error only for genuine stream errors. Running out of
    /// input is not an error: the call returns `Ok(())` and the decoder
    /// waits for more bytes.
    ///
    /// The call may also return `Ok(())` before the stream is finished once
    /// [`MAX_INTERNAL_BUFFER`] decompressed bytes have been buffered and not
    /// yet drained. Drain them via [`Decoder::output`] / [`Decoder::advance`],
    /// then call `feed` again (with remaining input, or `&[]` once the
    /// compressed data is exhausted) to resume. A `feed` call is therefore
    /// not guaranteed to consume the entire stream; callers that need a
    /// complete stream must drain in a loop until [`Decoder::is_finished`].
    pub fn feed(&mut self, data: &[u8]) -> Result<()> {
        if self.finished && !data.is_empty() {
            return Err(Error::InvalidData(
                "bytes fed after deflate stream end".into(),
            ));
        }
        self.input.feed(data);
        self.drive()
    }

    /// Borrow decompressed bytes not yet consumed.
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
        self.maybe_compact();
    }

    /// Drop consumed bytes from the front of the output buffer while
    /// preserving the LZ77 sliding window required for back-references.
    ///
    /// `copy_from_distance` uses `output.len() - distance` (relative
    /// indexing), so shrinking the front keeps all back-references valid
    /// as long as the last [`WINDOW_SIZE`] bytes are retained.
    fn maybe_compact(&mut self) {
        if self.output.len() < COMPACT_THRESHOLD {
            return;
        }
        let window_start = self.output.len().saturating_sub(WINDOW_SIZE);
        let keep_from = self.drained.min(window_start);
        if keep_from == 0 {
            return;
        }
        self.output.copy_within(keep_from.., 0);
        self.output.truncate(self.output.len() - keep_from);
        self.drained -= keep_from;
    }

    /// `true` once the final block's EOB has been decoded. Additional
    /// bytes fed after this will cause `Error::InvalidData`.
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// Bytes fed to `feed` that the decoder did not consume.
    ///
    /// Non-empty after the final block when the input contained trailing
    /// bytes (e.g. a container trailer).
    pub fn remaining_input(&self) -> &[u8] {
        self.input.get()
    }

    fn drive(&mut self) -> Result<()> {
        let (consumed, residual_buffer, residual_count, finished) = {
            let Self {
                input,
                output,
                drained,
                state,
                pending_bit_buffer,
                pending_bit_count,
                ..
            } = self;
            let mut reader =
                BitReader::new_seeded(input.get(), *pending_bit_buffer, *pending_bit_count);
            let mut finished = false;
            loop {
                match step(&mut reader, state, output, *drained)? {
                    StepOutcome::Progress => continue,
                    StepOutcome::NeedMoreBytes => break,
                    StepOutcome::Yield => break,
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
    drained: usize,
) -> Result<StepOutcome> {
    let current = core::mem::replace(state, DecodeState::Transient);
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
        } => step_symbol_loop(reader, state, output, drained, is_final, literal, distance),
        DecodeState::Finished => {
            *state = DecodeState::Finished;
            Ok(StepOutcome::Finished)
        }
        DecodeState::Transient => unreachable!("decoder left in transient state"),
    }
}

fn step_block_header(reader: &mut BitReader<'_>, state: &mut DecodeState) -> Result<StepOutcome> {
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
            let distance =
                HuffmanDecoder::from_code_lengths(&fixed_distance_code_lengths(), Some(7), None)?;
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
                all_code_lengths.extend(core::iter::repeat_n(last, repeat as usize));
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
                all_code_lengths.extend(core::iter::repeat_n(0, repeat as usize));
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
                all_code_lengths.extend(core::iter::repeat_n(0, repeat as usize));
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
    drained: usize,
    is_final: bool,
    literal: HuffmanDecoder,
    distance: HuffmanDecoder,
) -> Result<StepOutcome> {
    loop {
        // Yield to the caller once the unread output reaches the internal
        // cap, so a single arbitrarily-large block cannot grow the buffer
        // without bound. The Huffman decoders live in the preserved
        // `SymbolLoop` state, so decoding resumes seamlessly on the next
        // `feed`. Unlike `NeedMoreBytes`, the just-decoded symbol's bits are
        // left consumed (no snapshot restore): control returns to the caller
        // with the produced bytes ready to drain.
        if output.len() - drained >= MAX_INTERNAL_BUFFER {
            *state = DecodeState::SymbolLoop {
                is_final,
                literal,
                distance,
            };
            return Ok(StepOutcome::Yield);
        }
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
    /// The output buffer reached [`MAX_INTERNAL_BUFFER`] and control should
    /// return to the caller so it can drain the produced bytes. Unlike
    /// `NeedMoreBytes`, no input is awaited and no snapshot is restored.
    Yield,
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
    use alloc::vec::Vec;

    use super::Decoder;

    fn decompress_once(input: &[u8]) -> Vec<u8> {
        let mut d = Decoder::new();
        d.feed(input).expect("feed");
        assert!(d.is_finished(), "stream did not finish");
        let out = d.output().to_vec();
        d.advance(out.len());
        out
    }

    /// Drain a decoder to completion, resuming via empty `feed` calls after
    /// any internal output-cap yield. Appends all produced bytes to `out`.
    ///
    /// Test inputs are always complete streams, so a run that stalls with no
    /// progress indicates a regression and is surfaced as a panic rather
    /// than a hang.
    fn drain_until_finished(d: &mut Decoder, out: &mut Vec<u8>) {
        loop {
            let produced = d.output().to_vec();
            out.extend_from_slice(&produced);
            d.advance(produced.len());
            if d.is_finished() {
                break;
            }
            // Resume after an output-cap yield. The remaining compressed
            // input is buffered in the decoder, so an empty feed continues
            // decoding.
            let before_remaining = d.remaining_input().len();
            d.feed(&[]).expect("resume feed");
            // A resume may produce the final block and finish at once while
            // leaving no new output; that is still progress (the stream is
            // complete), so only flag a true stall (would-be truncated input).
            assert!(
                !d.output().is_empty()
                    || d.remaining_input().len() != before_remaining
                    || d.is_finished(),
                "drain_until_finished stalled: no progress on a complete stream",
            );
        }
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
        assert_eq!(
            decompress_once(&input),
            b"abracadabra abracadabra abracadabra"
        );
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

    #[test]
    fn advance_compacts_output_buffer() {
        // Regression for https://github.com/sile/noflate/issues/1: the output
        // buffer must not grow without bound when the caller streams the
        // decoded bytes out via feed/output/advance. Compress ~10 MiB and
        // decode it in chunks, draining after each chunk; the internal
        // output buffer should stay capped near the compaction threshold
        // plus the LZ77 window rather than holding all 10 MiB.
        use crate::encode::{EncodeOptions, Encoder};

        let payload: alloc::vec::Vec<u8> =
            (0..10 * 1024 * 1024).map(|i| (i * 37 + 13) as u8).collect();
        let mut e = Encoder::with_options(EncodeOptions::new().buffer_all_input());
        e.feed(&payload).unwrap();
        e.finish().unwrap();
        let compressed = e.output().to_vec();

        let mut d = Decoder::new();
        let mut decoded = alloc::vec::Vec::with_capacity(payload.len());
        let mut max_internal = 0usize;
        for chunk in compressed.chunks(64 * 1024) {
            d.feed(chunk).unwrap();
            let produced = d.output().to_vec();
            decoded.extend_from_slice(&produced);
            d.advance(produced.len());
            // Inspect the unread buffer length through the public surface:
            // after advance, output().len() is (total - drained), which is
            // bounded by the internal cap plus one symbol's overshoot.
            max_internal = max_internal.max(d.output().len());
        }
        // A `feed` may return before the final block when the internal cap
        // is reached; drain the remainder with empty resumes.
        drain_until_finished(&mut d, &mut decoded);
        assert!(d.is_finished());
        assert_eq!(decoded, payload);
        // The unread buffer observed by the caller must stay well under the
        // total decoded size (10 MiB): a single feed must not buffer a whole
        // block.
        assert!(
            max_internal <= super::MAX_INTERNAL_BUFFER + 258,
            "internal output buffer grew to {max_internal} bytes"
        );
    }

    #[test]
    fn back_reference_correct_across_compaction() {
        // Build a stream whose back-references span the compaction boundary.
        // The payload is a 3 MiB sequence followed by an exact copy of the
        // last 16 KiB — that inner copy becomes a back-reference spanning
        // data that will have been compacted away from the front.
        use crate::encode::{EncodeOptions, Encoder};

        let unit: alloc::vec::Vec<u8> = (0..16 * 1024).map(|i| (i * 31 + 7) as u8).collect();
        let mut payload = alloc::vec::Vec::new();
        for _ in 0..192 {
            // 192 * 16 KiB = 3 MiB of varying data
            payload.extend_from_slice(&unit);
        }
        // Final block that should LZ77-match the immediately-prior unit.
        payload.extend_from_slice(&unit);

        let mut e = Encoder::with_options(EncodeOptions::new().buffer_all_input());
        e.feed(&payload).unwrap();
        e.finish().unwrap();
        let compressed = e.output().to_vec();

        let mut d = Decoder::new();
        let mut decoded = alloc::vec::Vec::with_capacity(payload.len());
        // Drain in small chunks so compaction runs many times.
        for chunk in compressed.chunks(32 * 1024) {
            d.feed(chunk).unwrap();
            let produced = d.output().to_vec();
            decoded.extend_from_slice(&produced);
            d.advance(produced.len());
        }
        drain_until_finished(&mut d, &mut decoded);
        assert!(d.is_finished());
        assert_eq!(decoded, payload);
    }

    #[test]
    fn single_block_does_not_buffer_whole_output() {
        // Regression: a single DEFLATE block can expand arbitrarily (RFC 1951
        // has no per-block maximum). A one-shot encode of 8 MiB of zeros
        // yields one block; without the internal output cap a single `feed`
        // would buffer the entire 8 MiB expansion before the caller could
        // drain it.
        use crate::encode::{EncodeOptions, Encoder};

        let payload = alloc::vec![0u8; 8 * 1024 * 1024];
        let mut e = Encoder::with_options(EncodeOptions::new().buffer_all_input());
        e.feed(&payload).unwrap();
        e.finish().unwrap();
        let compressed = e.output().to_vec();

        let mut d = Decoder::new();
        d.feed(&compressed).expect("feed");
        // The first feed must stop once the internal cap is reached, not
        // buffer the whole 8 MiB expansion.
        let first_len = d.output().len();
        assert!(
            first_len <= super::MAX_INTERNAL_BUFFER + 258,
            "single feed buffered {first_len} bytes of one block",
        );
        let mut decoded = alloc::vec::Vec::new();
        drain_until_finished(&mut d, &mut decoded);
        assert_eq!(decoded, payload);
    }
}
