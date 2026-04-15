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
