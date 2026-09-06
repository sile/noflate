//! Raw DEFLATE (RFC 1951) encoder and decoder.
//!
//! ```
//! # fn main() -> noflate::Result<()> {
//! let compressed = noflate::deflate::compress(b"hello")?;
//! assert_eq!(noflate::deflate::decompress(&compressed)?, b"hello");
//! # Ok(())
//! # }
//! ```
//!
//! See [`Encoder`] and [`Decoder`] for the streaming API.

use alloc::vec::Vec;

use crate::error::{Error, Result};

pub use crate::decode::Decoder;
pub use crate::encode::{EncodeOptions, Encoder};

/// One-shot: compress a slice into a new DEFLATE stream.
pub fn compress(data: &[u8]) -> Result<Vec<u8>> {
    let mut encoder = Encoder::with_options(EncodeOptions::new().buffer_all_input());
    encoder.feed(data)?;
    encoder.finish()?;
    let out = encoder.output().to_vec();
    encoder.advance(out.len());
    Ok(out)
}

/// One-shot: decompress a DEFLATE stream into a new `Vec<u8>`.
///
/// Returns an error if the input is not a valid DEFLATE stream or ends
/// prematurely before the final block is consumed.
pub fn decompress(data: &[u8]) -> Result<Vec<u8>> {
    let mut decoder = Decoder::new();
    decoder.feed(data)?;
    let mut out = Vec::new();
    // `feed` may return before the final block is consumed once the decoder
    // has buffered up to its internal output cap; drain and resume until the
    // stream is complete. Resume with empty feeds, stopping only when an
    // empty feed makes no progress — that means the stream is truncated (the
    // decoder wants more compressed bytes that are not present).
    loop {
        let produced = decoder.output().to_vec();
        out.extend_from_slice(&produced);
        decoder.advance(produced.len());
        if decoder.is_finished() {
            break;
        }
        let before_remaining = decoder.remaining_input().len();
        decoder.feed(&[])?;
        // A `feed` may produce the final block's output and finish in the
        // same call. Do not break here; loop back to collect that output
        // before checking `is_finished`.
        if decoder.output().is_empty()
            && decoder.remaining_input().len() == before_remaining
            && !decoder.is_finished()
        {
            return Err(Error::InvalidData(
                "deflate stream ended before the final block".into(),
            ));
        }
    }
    Ok(out)
}
