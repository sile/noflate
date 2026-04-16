#![no_std]
#![forbid(unsafe_code)]

//! A zero-dependency, sans-io DEFLATE (RFC 1951) encoder and decoder.
//!
//! # Design
//!
//! The library owns its internal input and output buffers. Callers drive
//! encoding and decoding by feeding bytes in and consuming bytes out.
//!
//! # Examples
//!
//! One-shot compression and decompression:
//!
//! ```
//! let input = b"Hello, DEFLATE!";
//! let compressed = noflate::compress(input).unwrap();
//! let decompressed = noflate::decompress(&compressed).unwrap();
//! assert_eq!(decompressed, input);
//! ```
//!
//! Streaming decoder:
//!
//! ```
//! # let compressed = noflate::compress(b"hello").unwrap();
//! let mut decoder = noflate::Decoder::new();
//! decoder.feed(&compressed).unwrap();
//! let out = decoder.output().to_vec();
//! decoder.advance(out.len());
//! assert!(decoder.is_finished());
//! assert_eq!(out, b"hello");
//! ```

extern crate alloc;

#[cfg(test)]
extern crate std;

use alloc::vec::Vec;

mod adler32;
mod bit;
mod buf;
mod crc32;
mod decode;
mod encode;
mod error;
pub mod gzip;
mod huffman;
mod lz77;
mod symbol;
pub mod zlib;

pub use adler32::{Adler32, adler32};
pub use crc32::{Crc32, crc32};
pub use decode::Decoder;
pub use encode::{EncodeOptions, Encoder};
pub use error::{Error, Result};

/// The detected compression format of a byte stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    /// Raw DEFLATE (RFC 1951).
    Deflate,
    /// ZLIB container (RFC 1950).
    Zlib,
    /// GZIP container (RFC 1952).
    Gzip,
}

/// Inspect the first bytes of `data` and return the detected compression format.
///
/// Returns `None` if `data` is shorter than 2 bytes.
///
/// Detection order:
/// 1. **Gzip** — magic bytes `0x1F 0x8B`.
/// 2. **Zlib** — CM=8, CINFO≤7, and the FCHECK checksum passes.
/// 3. **Deflate** — fallback for anything else.
pub fn inspect(data: &[u8]) -> Option<Format> {
    if data.len() < 2 {
        return None;
    }

    // Gzip magic (RFC 1952).
    if data[0] == 0x1F && data[1] == 0x8B {
        return Some(Format::Gzip);
    }

    // Zlib header (RFC 1950).
    let cmf = data[0];
    let flg = data[1];
    if (cmf & 0x0F) == 8 && (cmf >> 4) <= 7 && (u16::from(cmf) * 256 + u16::from(flg)) % 31 == 0 {
        return Some(Format::Zlib);
    }

    Some(Format::Deflate)
}

/// Decompress a complete DEFLATE stream into a new `Vec<u8>`.
///
/// Returns an error if the input is not a valid DEFLATE stream or ends
/// prematurely before the final block is consumed.
pub fn decompress(compressed: &[u8]) -> Result<Vec<u8>> {
    let mut decoder = Decoder::new();
    decoder.feed(compressed)?;
    if !decoder.is_finished() {
        return Err(Error::InvalidData(
            "deflate stream ended before the final block".into(),
        ));
    }
    let out = decoder.output().to_vec();
    decoder.advance(out.len());
    Ok(out)
}

/// Compress a byte slice into a new DEFLATE stream with default options.
pub fn compress(uncompressed: &[u8]) -> Result<Vec<u8>> {
    let mut encoder = Encoder::new();
    encoder.feed(uncompressed)?;
    encoder.finish()?;
    let out = encoder.output().to_vec();
    encoder.advance(out.len());
    Ok(out)
}
