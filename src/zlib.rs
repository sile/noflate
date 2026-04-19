//! ZLIB (RFC 1950) encoder and decoder.
//!
//! ```
//! let compressed = noflate::zlib::compress(b"hello").unwrap();
//! assert_eq!(noflate::zlib::decompress(&compressed).unwrap(), b"hello");
//! ```
//!
//! See [`Encoder`] and [`Decoder`] for the streaming API.
//!
//! This module also provides [`Adler32`] and [`adler32`] for Adler-32
//! checksum computation.

use alloc::borrow::Cow;
use alloc::format;
use alloc::vec::Vec;

pub use crate::adler32::{Adler32, adler32};
use crate::buf::Buf;
use crate::decode::Decoder as DeflateDecoder;
use crate::encode::{EncodeOptions, Encoder as DeflateEncoder};
use crate::error::{Error, Result};

/// RFC 1950 header byte: CMF = 0x78 (deflate, 32 KiB window).
const CMF: u8 = 0x78;
/// RFC 1950 FLG byte: default compression (FLEVEL=2), no dictionary.
/// The low 5 bits (FCHECK) are chosen so `(CMF * 256 + FLG) % 31 == 0`.
/// `0x78 * 256 + 0x9C = 30876 = 31 * 996`.
const FLG: u8 = 0x9C;

/// Streaming zlib decoder.
#[derive(Debug)]
pub struct Decoder {
    state: State,
    header: [u8; 2],
    header_filled: u8,
    deflate: DeflateDecoder,
    adler: Adler32,
    trailer: [u8; 4],
    trailer_filled: u8,
    finished: bool,
    output: Buf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Header,
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
    /// Create a zlib decoder positioned at the start of a stream.
    pub fn new() -> Self {
        Self {
            state: State::Header,
            header: [0; 2],
            header_filled: 0,
            deflate: DeflateDecoder::new(),
            adler: Adler32::new(),
            trailer: [0; 4],
            trailer_filled: 0,
            finished: false,
            output: Buf::new(),
        }
    }

    /// Append compressed bytes.
    pub fn feed(&mut self, data: &[u8]) -> Result<()> {
        let mut rest = data;
        while !rest.is_empty() {
            match self.state {
                State::Header => {
                    let take = (2 - self.header_filled as usize).min(rest.len());
                    self.header[self.header_filled as usize..self.header_filled as usize + take]
                        .copy_from_slice(&rest[..take]);
                    self.header_filled += take as u8;
                    rest = &rest[take..];
                    if self.header_filled == 2 {
                        validate_zlib_header(self.header)?;
                        self.state = State::Body;
                    }
                }
                State::Body => {
                    self.deflate.feed(rest)?;
                    let new_bytes = self.deflate.output();
                    self.adler.update(new_bytes);
                    self.output.feed(new_bytes);
                    let n = new_bytes.len();
                    self.deflate.advance(n);
                    if self.deflate.is_finished() {
                        self.state = State::Trailer;
                        rest = &[];
                        self.consume_deflate_tail()?;
                    } else {
                        rest = &[];
                    }
                }
                State::Trailer => {
                    let take = (4 - self.trailer_filled as usize).min(rest.len());
                    self.trailer[self.trailer_filled as usize..self.trailer_filled as usize + take]
                        .copy_from_slice(&rest[..take]);
                    self.trailer_filled += take as u8;
                    rest = &rest[take..];
                    if self.trailer_filled == 4 {
                        let expected = u32::from_be_bytes(self.trailer);
                        if expected != self.adler.value() {
                            return Err(Error::InvalidData(
                                "adler-32 checksum mismatch in zlib trailer".into(),
                            ));
                        }
                        self.state = State::Done;
                        self.finished = true;
                    }
                }
                State::Done => {
                    return Err(Error::InvalidData("bytes fed after zlib stream end".into()));
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

    /// `true` once the header + deflate stream + valid Adler-32 trailer
    /// have all been consumed.
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    fn feed_byte_in_trailer(&mut self, byte: u8) -> Result<()> {
        match self.state {
            State::Trailer => {
                self.trailer[self.trailer_filled as usize] = byte;
                self.trailer_filled += 1;
                if self.trailer_filled == 4 {
                    let expected = u32::from_be_bytes(self.trailer);
                    if expected != self.adler.value() {
                        return Err(Error::InvalidData(
                            "adler-32 checksum mismatch in zlib trailer".into(),
                        ));
                    }
                    self.state = State::Done;
                    self.finished = true;
                }
                Ok(())
            }
            State::Done => Err(Error::InvalidData("bytes fed after zlib stream end".into())),
            _ => unreachable!("feed_byte_in_trailer in unexpected state"),
        }
    }

    fn consume_deflate_tail(&mut self) -> Result<()> {
        let tail = self.deflate.remaining_input();
        let tail_len = tail.len();
        let mut trailer_bytes = [0u8; 4];
        let copied = tail_len.min(trailer_bytes.len());
        trailer_bytes[..copied].copy_from_slice(&tail[..copied]);
        for &byte in &trailer_bytes[..copied] {
            self.feed_byte_in_trailer(byte)?;
        }
        if tail_len > trailer_bytes.len() {
            return Err(Error::InvalidData("bytes fed after zlib stream end".into()));
        }
        Ok(())
    }
}

fn validate_zlib_header(header: [u8; 2]) -> Result<()> {
    let cmf = header[0];
    let flg = header[1];
    if (cmf & 0x0F) != 8 {
        return Err(Error::Unsupported(Cow::Owned(format!(
            "zlib compression method {} not supported (only deflate=8)",
            cmf & 0x0F
        ))));
    }
    if (cmf >> 4) > 7 {
        return Err(Error::Unsupported(Cow::Owned(format!(
            "zlib window size code {} not supported (max 7)",
            cmf >> 4
        ))));
    }
    if flg & 0x20 != 0 {
        return Err(Error::Unsupported(
            "zlib preset dictionary not supported".into(),
        ));
    }
    let combined = (u32::from(cmf) << 8) | u32::from(flg);
    if combined % 31 != 0 {
        return Err(Error::InvalidData(
            "zlib header FCHECK validation failed".into(),
        ));
    }
    Ok(())
}

/// Streaming zlib encoder.
#[derive(Debug)]
pub struct Encoder {
    deflate: DeflateEncoder,
    adler: Adler32,
    output: Vec<u8>,
    drained: usize,
    header_emitted: bool,
    finishing: bool,
}

impl Default for Encoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Encoder {
    /// Create a zlib encoder with default options.
    pub fn new() -> Self {
        Self::with_options(EncodeOptions::new())
    }

    /// Create a zlib encoder with custom options.
    pub fn with_options(options: EncodeOptions) -> Self {
        Self {
            deflate: DeflateEncoder::with_options(options),
            adler: Adler32::new(),
            output: Vec::new(),
            drained: 0,
            header_emitted: false,
            finishing: false,
        }
    }

    /// Append uncompressed bytes.
    pub fn feed(&mut self, data: &[u8]) -> Result<()> {
        if self.finishing {
            return Err(Error::InvalidData(
                "bytes fed after zlib encoder finish".into(),
            ));
        }
        if !self.header_emitted {
            self.output.push(CMF);
            self.output.push(FLG);
            self.header_emitted = true;
        }
        self.adler.update(data);
        self.deflate.feed(data)?;
        let produced = self.deflate.output().len();
        if produced > 0 {
            self.output.extend_from_slice(self.deflate.output());
            self.deflate.advance(produced);
        }
        Ok(())
    }

    /// Emit the final block and the Adler-32 trailer. Subsequent calls
    /// are a no-op.
    pub fn finish(&mut self) -> Result<()> {
        if self.finishing {
            return Ok(());
        }
        if !self.header_emitted {
            self.output.push(CMF);
            self.output.push(FLG);
            self.header_emitted = true;
        }
        self.deflate.finish()?;
        let produced = self.deflate.output().len();
        self.output.extend_from_slice(self.deflate.output());
        self.deflate.advance(produced);
        self.output
            .extend_from_slice(&self.adler.value().to_be_bytes());
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
    }

    /// `true` once `finish` has been called and all output consumed.
    pub fn is_finished(&self) -> bool {
        self.finishing && self.drained == self.output.len()
    }
}

/// One-shot: compress a slice into a new zlib stream.
pub fn compress(data: &[u8]) -> Result<Vec<u8>> {
    let mut e = Encoder::with_options(EncodeOptions::new().buffer_all_input());
    e.feed(data)?;
    e.finish()?;
    let out = e.output().to_vec();
    e.advance(out.len());
    Ok(out)
}

/// One-shot: decompress a zlib stream into a new `Vec<u8>`.
pub fn decompress(data: &[u8]) -> Result<Vec<u8>> {
    let mut d = Decoder::new();
    d.feed(data)?;
    if !d.is_finished() {
        return Err(Error::InvalidData(
            "zlib stream ended before the trailer".into(),
        ));
    }
    let out = d.output().to_vec();
    d.advance(out.len());
    Ok(out)
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::{Decoder, Encoder, compress, decompress};

    #[test]
    fn roundtrip_hello() {
        let original = b"Hello, zlib!";
        let compressed = compress(original).unwrap();
        assert_eq!(compressed[0], 0x78);
        assert_eq!(compressed[1], 0x9C);
        let decompressed = decompress(&compressed).unwrap();
        assert_eq!(decompressed, original);
    }

    #[test]
    fn roundtrip_empty() {
        let compressed = compress(b"").unwrap();
        assert_eq!(decompress(&compressed).unwrap(), b"");
    }

    #[test]
    fn roundtrip_various_sizes() {
        for len in [1usize, 7, 1024, 65_536] {
            let input: Vec<u8> = (0..len).map(|i| (i * 13 + 7) as u8).collect();
            let c = compress(&input).unwrap();
            assert_eq!(decompress(&c).unwrap(), input, "len={len}");
        }
    }

    #[test]
    fn incremental_feed_one_byte_at_a_time() {
        let compressed = compress(b"hello world zlib streaming").unwrap();
        let mut d = Decoder::new();
        for &byte in &compressed {
            d.feed(&[byte]).unwrap();
        }
        assert!(d.is_finished());
        let out = d.output().to_vec();
        d.advance(out.len());
        assert_eq!(out, b"hello world zlib streaming");
    }

    #[test]
    fn tampered_adler32_rejected() {
        let mut c = compress(b"the quick brown fox").unwrap();
        let last = c.len() - 1;
        c[last] ^= 0x01;
        assert!(decompress(&c).is_err());
    }

    #[test]
    fn header_fcheck_validated() {
        let mut c = compress(b"hello").unwrap();
        c[1] ^= 0x01;
        assert!(decompress(&c).is_err());
    }

    #[test]
    fn encoder_streaming_writes_to_output_buffer() {
        let mut e = Encoder::new();
        e.feed(b"streaming ").unwrap();
        e.feed(b"zlib ").unwrap();
        e.feed(b"test").unwrap();
        e.finish().unwrap();
        let out = e.output().to_vec();
        e.advance(out.len());
        assert!(e.is_finished());
        assert_eq!(decompress(&out).unwrap(), b"streaming zlib test");
    }

    #[test]
    fn rejects_bytes_after_finish() {
        let compressed = compress(b"x").unwrap();
        let mut d = Decoder::new();
        d.feed(&compressed).unwrap();
        assert!(d.is_finished());
        assert!(d.feed(b"y").is_err());
    }

    #[test]
    fn interoperates_with_flate2() {
        use std::io::{Read, Write};

        // Our output -> flate2's decoder.
        let ours = compress(b"noflate -> flate2").unwrap();
        let mut their_dec = flate2::read::ZlibDecoder::new(&ours[..]);
        let mut decoded = Vec::new();
        their_dec.read_to_end(&mut decoded).unwrap();
        assert_eq!(decoded, b"noflate -> flate2");

        // flate2's output -> our decoder.
        let mut their_enc =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        their_enc.write_all(b"flate2 -> noflate").unwrap();
        let theirs = their_enc.finish().unwrap();
        assert_eq!(decompress(&theirs).unwrap(), b"flate2 -> noflate");
    }
}
