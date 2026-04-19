#![no_std]
#![warn(missing_docs)]
#![forbid(unsafe_code)]

//! A zero-dependency DEFLATE (RFC 1951), gzip (RFC 1952), and zlib
//! (RFC 1950) encoder and decoder.
//!
//! # Design
//!
//! This crate follows a *sans-io* design: the library performs no I/O
//! itself. Callers drive encoding and decoding by feeding bytes in
//! (`feed`) and consuming bytes out (`output` / `advance`). This makes
//! the library usable with any I/O strategy — synchronous, async, or
//! embedded — without runtime dependencies.
//!
//! # Examples
//!
//! One-shot compression and decompression:
//!
//! ```
//! let input = b"Hello, DEFLATE!";
//! let compressed = noflate::deflate::compress(input).unwrap();
//! let decompressed = noflate::deflate::decompress(&compressed).unwrap();
//! assert_eq!(decompressed, input);
//! ```
//!
//! Streaming decoder:
//!
//! ```
//! # let compressed = noflate::deflate::compress(b"hello").unwrap();
//! let mut decoder = noflate::deflate::Decoder::new();
//! decoder.feed(&compressed).unwrap();
//! let out = decoder.output().to_vec();
//! decoder.advance(out.len());
//! assert!(decoder.is_finished());
//! assert_eq!(out, b"hello");
//! ```

extern crate alloc;

#[cfg(test)]
extern crate std;

mod adler32;
mod bit;
mod buf;
mod crc32;
mod decode;
pub mod deflate;
mod encode;
mod error;
pub mod gzip;
mod huffman;
mod lz77;
mod symbol;
pub mod zlib;

pub use adler32::{Adler32, adler32};
pub use crc32::{Crc32, crc32};
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

impl Format {
    /// Detect the compression format from the first bytes of `data`.
    ///
    /// Returns `None` if `data` is shorter than 2 bytes.
    ///
    /// Detection order:
    /// 1. **Gzip** — magic bytes `0x1F 0x8B`.
    /// 2. **Zlib** — CM=8, CINFO≤7, and the FCHECK checksum passes.
    /// 3. **Deflate** — fallback for anything else.
    pub fn detect(data: &[u8]) -> Option<Format> {
        if data.len() < 2 {
            return None;
        }

        if data[0] == 0x1F && data[1] == 0x8B {
            return Some(Format::Gzip);
        }

        let cmf = data[0];
        let flg = data[1];
        if (cmf & 0x0F) == 8
            && (cmf >> 4) <= 7
            && (u16::from(cmf) * 256 + u16::from(flg)) % 31 == 0
        {
            return Some(Format::Zlib);
        }

        Some(Format::Deflate)
    }
}
