#![no_std]
#![warn(missing_docs)]
#![forbid(unsafe_code)]

//! A zero-dependency DEFLATE (RFC 1951), gzip (RFC 1952), and zlib
//! (RFC 1950) encoder and decoder.
//!
//! - `no_std` (requires only `alloc`)
//! - No `unsafe` code
//! - Sans-io: callers drive encoding and decoding via `feed` / `output` /
//!   `advance`, with no I/O performed by the library itself
//! - WebSocket `permessage-deflate` (RFC 7692) support via
//!   [`deflate::Encoder::sync_flush`] and [`deflate::Encoder::reset_history`]
//!
//! # Examples
//!
//! One-shot compression and decompression:
//!
//! ```
//! # fn main() -> noflate::Result<()> {
//! let input = b"Hello, DEFLATE!";
//! let compressed = noflate::deflate::compress(input)?;
//! let decompressed = noflate::deflate::decompress(&compressed)?;
//! assert_eq!(decompressed, input);
//! # Ok(())
//! # }
//! ```
//!
//! Streaming encoder:
//!
//! ```
//! # fn main() -> noflate::Result<()> {
//! let mut encoder = noflate::deflate::Encoder::new();
//! encoder.feed(b"Hello, ")?;
//! encoder.feed(b"world!")?;
//! encoder.finish()?;
//! let compressed = encoder.output().to_vec();
//! encoder.advance(compressed.len());
//! assert_eq!(
//!     noflate::deflate::decompress(&compressed)?,
//!     b"Hello, world!",
//! );
//! # Ok(())
//! # }
//! ```
//!
//! Streaming decoder:
//!
//! ```
//! # fn main() -> noflate::Result<()> {
//! # let compressed = noflate::deflate::compress(b"hello")?;
//! let mut decoder = noflate::deflate::Decoder::new();
//! decoder.feed(&compressed)?;
//! let out = decoder.output().to_vec();
//! decoder.advance(out.len());
//! assert!(decoder.is_finished());
//! assert_eq!(out, b"hello");
//! # Ok(())
//! # }
//! ```
//!
//! The [`gzip`] and [`zlib`] modules provide the same API shape for their
//! respective container formats.
//!
//! [`Format::detect`] identifies the format of a compressed stream:
//!
//! ```
//! # fn main() -> noflate::Result<()> {
//! let data = noflate::gzip::compress(b"hello")?;
//! assert_eq!(noflate::Format::detect(&data), Some(noflate::Format::Gzip));
//! # Ok(())
//! # }
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
        if (cmf & 0x0F) == 8 && (cmf >> 4) <= 7 && (u16::from(cmf) * 256 + u16::from(flg)) % 31 == 0
        {
            return Some(Format::Zlib);
        }

        Some(Format::Deflate)
    }
}
