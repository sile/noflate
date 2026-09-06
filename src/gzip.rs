//! GZIP (RFC 1952) encoder and decoder.
//!
//! ```
//! # fn main() -> noflate::Result<()> {
//! let compressed = noflate::gzip::compress(b"hello")?;
//! assert_eq!(noflate::gzip::decompress(&compressed)?, b"hello");
//! # Ok(())
//! # }
//! ```
//!
//! See [`Encoder`] and [`Decoder`] for the streaming API.
//!
//! This module also provides [`Crc32`] and [`crc32`] for CRC-32 checksum
//! computation.

use alloc::borrow::Cow;
use alloc::format;
use alloc::vec::Vec;

use crate::buf::Buf;
pub use crate::crc32::{Crc32, crc32};
use crate::decode::Decoder as DeflateDecoder;
use crate::encode::{EncodeOptions, Encoder as DeflateEncoder};
use crate::error::{Error, Result};

const MAGIC1: u8 = 0x1F;
const MAGIC2: u8 = 0x8B;
const METHOD_DEFLATE: u8 = 8;

const FHCRC: u8 = 1 << 1;
const FEXTRA: u8 = 1 << 2;
const FNAME: u8 = 1 << 3;
const FCOMMENT: u8 = 1 << 4;

/// Compact the encoder output buffer once the consumed prefix exceeds this size.
///
/// See `encode::COMPACT_THRESHOLD` for the cost model — one `Vec::drain`
/// per `COMPACT_THRESHOLD` bytes consumed, zero cost under the threshold.
const COMPACT_THRESHOLD: usize = 1024 * 1024;

/// Streaming gzip decoder.
#[derive(Debug)]
pub struct Decoder {
    state: State,
    flags: u8,
    header_bytes: [u8; 10],
    header_filled: u8,
    extra_remaining: u32,
    extra_len_bytes_filled: u8,
    extra_len: [u8; 2],
    deflate: DeflateDecoder,
    crc: Crc32,
    size: u32,
    trailer: [u8; 8],
    trailer_filled: u8,
    finished: bool,
    output: Buf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    FixedHeader,
    Extra,
    ExtraLen,
    Name,
    Comment,
    Hcrc,
    Body,
    Trailer,
    Done,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder {
    /// Create a gzip decoder positioned at the start of a stream.
    pub fn new() -> Self {
        Self {
            state: State::FixedHeader,
            flags: 0,
            header_bytes: [0; 10],
            header_filled: 0,
            extra_remaining: 0,
            extra_len_bytes_filled: 0,
            extra_len: [0; 2],
            deflate: DeflateDecoder::new(),
            crc: Crc32::new(),
            size: 0,
            trailer: [0; 8],
            trailer_filled: 0,
            finished: false,
            output: Buf::new(),
        }
    }

    /// Append compressed bytes.
    pub fn feed(&mut self, data: &[u8]) -> Result<()> {
        let mut i = 0;
        while i < data.len() {
            match self.state {
                State::FixedHeader => {
                    let take = (10 - self.header_filled as usize).min(data.len() - i);
                    self.header_bytes
                        [self.header_filled as usize..self.header_filled as usize + take]
                        .copy_from_slice(&data[i..i + take]);
                    self.header_filled += take as u8;
                    i += take;
                    if self.header_filled == 10 {
                        self.parse_fixed_header()?;
                        self.advance_to_next_header_state();
                    }
                }
                State::Extra => {
                    if self.extra_len_bytes_filled < 2 {
                        let take = (2 - self.extra_len_bytes_filled as usize).min(data.len() - i);
                        self.extra_len[self.extra_len_bytes_filled as usize
                            ..self.extra_len_bytes_filled as usize + take]
                            .copy_from_slice(&data[i..i + take]);
                        self.extra_len_bytes_filled += take as u8;
                        i += take;
                        if self.extra_len_bytes_filled == 2 {
                            self.extra_remaining = u32::from(u16::from_le_bytes(self.extra_len));
                            self.state = State::ExtraLen;
                        }
                    } else {
                        self.state = State::ExtraLen;
                    }
                }
                State::ExtraLen => {
                    let take = (self.extra_remaining as usize).min(data.len() - i);
                    i += take;
                    self.extra_remaining -= take as u32;
                    if self.extra_remaining == 0 {
                        self.advance_past_extra();
                    }
                }
                State::Name => {
                    while i < data.len() {
                        let b = data[i];
                        i += 1;
                        if b == 0 {
                            self.advance_past_name();
                            break;
                        }
                    }
                }
                State::Comment => {
                    while i < data.len() {
                        let b = data[i];
                        i += 1;
                        if b == 0 {
                            self.advance_past_comment();
                            break;
                        }
                    }
                }
                State::Hcrc => {
                    // Skip 2 bytes of header CRC-16.
                    let skip = (2 - self.extra_len_bytes_filled as usize).min(data.len() - i);
                    i += skip;
                    self.extra_len_bytes_filled += skip as u8;
                    if self.extra_len_bytes_filled == 2 {
                        self.state = State::Body;
                        self.extra_len_bytes_filled = 0;
                    }
                }
                State::Body => {
                    self.deflate.feed(&data[i..])?;
                    i = data.len();
                    // Drain the deflate sub-stream, resuming with empty feeds
                    // after any internal output-cap yield, until the final
                    // block is consumed or it can make no further progress
                    // without more compressed input. The empty-feed progress
                    // check distinguishes a cap yield (resumable) from a step
                    // that genuinely needs more bytes (stop and wait): a
                    // `NeedMoreBytes` may leave unread input buffered even
                    // though decoding cannot proceed, so `remaining_input`
                    // alone cannot tell them apart.
                    loop {
                        let new_bytes = self.deflate.output();
                        self.crc.update(new_bytes);
                        self.size = self.size.wrapping_add(new_bytes.len() as u32);
                        self.output.feed(new_bytes);
                        let n = new_bytes.len();
                        self.deflate.advance(n);
                        if self.deflate.is_finished() {
                            self.state = State::Trailer;
                            self.consume_deflate_tail()?;
                            break;
                        }
                        let before_remaining = self.deflate.remaining_input().len();
                        self.deflate.feed(&[])?;
                        // A `feed` may produce the final block's output and
                        // finish in the same call; loop back to collect that
                        // output before transitioning to the trailer.
                        if self.deflate.output().is_empty()
                            && self.deflate.remaining_input().len() == before_remaining
                            && !self.deflate.is_finished()
                        {
                            // The sub-stream needs more compressed bytes;
                            // hand control back to the caller.
                            break;
                        }
                    }
                }
                State::Trailer => {
                    while i < data.len() && self.trailer_filled < 8 {
                        self.feed_trailer_byte(data[i])?;
                        i += 1;
                    }
                }
                State::Done => {
                    return Err(Error::InvalidData("bytes fed after gzip stream end".into()));
                }
            }
        }
        Ok(())
    }

    /// Borrow decompressed bytes not yet consumed.
    pub fn output(&self) -> &[u8] {
        self.output.get()
    }

    /// Mark `n` bytes of output as consumed.
    pub fn advance(&mut self, n: usize) {
        self.output.advance(n);
    }

    /// `true` once the trailer has been fully consumed and validated.
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    fn parse_fixed_header(&mut self) -> Result<()> {
        if self.header_bytes[0] != MAGIC1 || self.header_bytes[1] != MAGIC2 {
            return Err(Error::InvalidData("gzip magic bytes not found".into()));
        }
        if self.header_bytes[2] != METHOD_DEFLATE {
            return Err(Error::Unsupported(Cow::Owned(format!(
                "gzip compression method {} not supported (only deflate=8)",
                self.header_bytes[2]
            ))));
        }
        self.flags = self.header_bytes[3];
        // bytes 4..8 = MTIME, byte 8 = XFL, byte 9 = OS: ignored.
        Ok(())
    }

    fn advance_to_next_header_state(&mut self) {
        if self.flags & FEXTRA != 0 {
            self.state = State::Extra;
            self.extra_len_bytes_filled = 0;
        } else {
            self.advance_past_extra();
        }
    }

    fn advance_past_extra(&mut self) {
        if self.flags & FNAME != 0 {
            self.state = State::Name;
        } else {
            self.advance_past_name();
        }
    }

    fn advance_past_name(&mut self) {
        if self.flags & FCOMMENT != 0 {
            self.state = State::Comment;
        } else {
            self.advance_past_comment();
        }
    }

    fn advance_past_comment(&mut self) {
        if self.flags & FHCRC != 0 {
            self.state = State::Hcrc;
            self.extra_len_bytes_filled = 0;
        } else {
            self.state = State::Body;
        }
    }

    fn feed_trailer_byte(&mut self, byte: u8) -> Result<()> {
        self.trailer[self.trailer_filled as usize] = byte;
        self.trailer_filled += 1;
        if self.trailer_filled == 8 {
            let expected_crc = u32::from_le_bytes(
                self.trailer[0..4]
                    .try_into()
                    .expect("gzip trailer crc is 4 bytes"),
            );
            let expected_size = u32::from_le_bytes(
                self.trailer[4..8]
                    .try_into()
                    .expect("gzip trailer size is 4 bytes"),
            );
            if expected_crc != self.crc.value() {
                return Err(Error::InvalidData(
                    "crc-32 checksum mismatch in gzip trailer".into(),
                ));
            }
            if expected_size != self.size {
                return Err(Error::InvalidData("isize mismatch in gzip trailer".into()));
            }
            self.state = State::Done;
            self.finished = true;
        }
        Ok(())
    }

    fn consume_deflate_tail(&mut self) -> Result<()> {
        let tail = self.deflate.remaining_input();
        let tail_len = tail.len();
        let mut trailer_bytes = [0u8; 8];
        let copied = tail_len.min(trailer_bytes.len());
        trailer_bytes[..copied].copy_from_slice(&tail[..copied]);
        for &byte in &trailer_bytes[..copied] {
            self.feed_trailer_byte(byte)?;
        }
        if tail_len > trailer_bytes.len() {
            return Err(Error::InvalidData("bytes fed after gzip stream end".into()));
        }
        Ok(())
    }
}

/// Streaming gzip encoder.
#[derive(Debug)]
pub struct Encoder {
    deflate: DeflateEncoder,
    crc: Crc32,
    output: Vec<u8>,
    drained: usize,
    header_emitted: bool,
    finishing: bool,
    size: u32,
}

impl Default for Encoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Encoder {
    /// Create a gzip encoder with default options.
    pub fn new() -> Self {
        Self::with_options(EncodeOptions::new())
    }

    /// Create a gzip encoder with custom options.
    pub fn with_options(options: EncodeOptions) -> Self {
        Self {
            deflate: DeflateEncoder::with_options(options),
            crc: Crc32::new(),
            output: Vec::new(),
            drained: 0,
            header_emitted: false,
            finishing: false,
            size: 0,
        }
    }

    /// Append uncompressed bytes.
    pub fn feed(&mut self, data: &[u8]) -> Result<()> {
        if self.finishing {
            return Err(Error::InvalidData(
                "bytes fed after gzip encoder finish".into(),
            ));
        }
        self.ensure_header();
        self.crc.update(data);
        self.size = self.size.wrapping_add(data.len() as u32);
        self.deflate.feed(data)?;
        let produced = self.deflate.output().len();
        if produced > 0 {
            self.output.extend_from_slice(self.deflate.output());
            self.deflate.advance(produced);
        }
        Ok(())
    }

    /// Emit the final block and the gzip trailer. Subsequent calls are
    /// a no-op.
    pub fn finish(&mut self) -> Result<()> {
        if self.finishing {
            return Ok(());
        }
        self.ensure_header();
        self.deflate.finish()?;
        let produced = self.deflate.output().len();
        self.output.extend_from_slice(self.deflate.output());
        self.deflate.advance(produced);
        self.output
            .extend_from_slice(&self.crc.value().to_le_bytes());
        self.output.extend_from_slice(&self.size.to_le_bytes());
        self.finishing = true;
        Ok(())
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

    fn ensure_header(&mut self) {
        if self.header_emitted {
            return;
        }
        // Minimal gzip header: magic, method, flags=0, mtime=0, xfl=0, os=255.
        self.output
            .extend_from_slice(&[MAGIC1, MAGIC2, METHOD_DEFLATE, 0, 0, 0, 0, 0, 0, 255]);
        self.header_emitted = true;
    }
}

/// One-shot: compress a slice into a new gzip stream.
pub fn compress(data: &[u8]) -> Result<Vec<u8>> {
    let mut e = Encoder::with_options(EncodeOptions::new().buffer_all_input());
    e.feed(data)?;
    e.finish()?;
    let out = e.output().to_vec();
    e.advance(out.len());
    Ok(out)
}

/// One-shot: decompress a gzip stream into a new `Vec<u8>`.
pub fn decompress(data: &[u8]) -> Result<Vec<u8>> {
    let mut d = Decoder::new();
    d.feed(data)?;
    if !d.is_finished() {
        return Err(Error::InvalidData(
            "gzip stream ended before the trailer".into(),
        ));
    }
    let out = d.output().to_vec();
    d.advance(out.len());
    Ok(out)
}

#[cfg(test)]
mod tests {
    use alloc::vec;
    use alloc::vec::Vec;

    use super::{Decoder, compress, decompress};

    #[test]
    fn roundtrip_hello() {
        let original = b"Hello, gzip!";
        let compressed = compress(original).expect("compress failed");
        assert_eq!(&compressed[..3], &[0x1F, 0x8B, 8]);
        let decompressed = decompress(&compressed).expect("decompress failed");
        assert_eq!(decompressed, original);
    }

    #[test]
    fn roundtrip_empty() {
        let compressed = compress(b"").expect("compress failed");
        assert_eq!(decompress(&compressed).expect("decompress failed"), b"");
    }

    #[test]
    fn roundtrip_various_sizes() {
        // `75_536` spans two 64 KiB blocks with a partial final block, so the
        // deflate sub-stream finishes mid-block after an output-cap yield;
        // the container must collect that final partial output rather than
        // truncating it.
        for len in [1usize, 7, 1024, 65_536, 75_536, 262_144] {
            let input: Vec<u8> = (0..len).map(|i| (i * 13 + 7) as u8).collect();
            let c = compress(&input).expect("compress failed");
            assert_eq!(
                decompress(&c).expect("decompress failed"),
                input,
                "len={len}"
            );
        }
    }

    #[test]
    fn incremental_feed_one_byte_at_a_time() {
        let compressed = compress(b"gzip streaming test").expect("compress failed");
        let mut d = Decoder::new();
        for &byte in &compressed {
            d.feed(&[byte]).expect("feed failed");
        }
        assert!(d.is_finished());
        let out = d.output().to_vec();
        d.advance(out.len());
        assert_eq!(out, b"gzip streaming test");
    }

    #[test]
    fn tampered_crc_rejected() {
        let mut c = compress(b"the quick brown fox").expect("compress failed");
        let i = c.len() - 8;
        c[i] ^= 0x01;
        assert!(decompress(&c).is_err());
    }

    #[test]
    fn tampered_size_rejected() {
        let mut c = compress(b"the quick brown fox").expect("compress failed");
        let i = c.len() - 1;
        c[i] ^= 0x01;
        assert!(decompress(&c).is_err());
    }

    #[test]
    fn bad_magic_rejected() {
        let mut c = compress(b"hello").expect("compress failed");
        c[0] ^= 0x01;
        assert!(decompress(&c).is_err());
    }

    #[test]
    fn interoperates_with_flate2() {
        use std::io::{Read, Write};

        // Our output -> flate2's decoder.
        let ours = compress(b"noflate -> flate2 gzip").expect("compress failed");
        let mut their_dec = flate2::read::GzDecoder::new(&ours[..]);
        let mut decoded = Vec::new();
        their_dec
            .read_to_end(&mut decoded)
            .expect("read_to_end failed");
        assert_eq!(decoded, b"noflate -> flate2 gzip");

        // flate2's output -> our decoder.
        let mut their_enc =
            flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        their_enc
            .write_all(b"flate2 -> noflate gzip")
            .expect("write_all failed");
        let theirs = their_enc.finish().expect("finish failed");
        assert_eq!(
            decompress(&theirs).expect("decompress failed"),
            b"flate2 -> noflate gzip"
        );
    }

    #[test]
    fn decodes_header_with_fname() {
        // Build a gzip with FNAME flag and name "hi\0".
        let body = compress(b"xyz").expect("compress failed");
        let mut patched = Vec::new();
        patched.extend_from_slice(&[MAGIC1, MAGIC2, 8, super::FNAME, 0, 0, 0, 0, 0, 255]);
        patched.extend_from_slice(b"hi\0");
        patched.extend_from_slice(&body[10..]);
        assert_eq!(decompress(&patched).expect("decompress failed"), b"xyz");
    }

    use super::{MAGIC1, MAGIC2};

    #[test]
    fn advance_compacts_output_buffer() {
        // Regression for https://github.com/sile/noflate/issues/1.
        use super::{EncodeOptions, Encoder};
        let mut e = Encoder::with_options(EncodeOptions::new().stored());
        let chunk = vec![b'x'; 64 * 1024];
        let mut total = 0usize;
        let mut max_internal = 0usize;
        for _ in 0..160 {
            e.feed(&chunk).expect("feed failed");
            let out = e.output().to_vec();
            total += out.len();
            e.advance(out.len());
            max_internal = max_internal.max(e.output.len());
        }
        e.finish().expect("finish failed");
        let tail = e.output().to_vec();
        total += tail.len();
        e.advance(tail.len());
        assert!(e.is_finished());
        assert!(total > 10 * 1024 * 1024);
        assert!(
            max_internal < 2 * 1024 * 1024,
            "internal output buffer grew to {max_internal} bytes"
        );
    }
}
