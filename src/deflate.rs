//! Raw DEFLATE (RFC 1951) encoder and decoder.

use alloc::vec::Vec;

use crate::error::{Error, Result};

pub use crate::decode::Decoder;
pub use crate::encode::{EncodeOptions, Encoder};

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
    let mut encoder = Encoder::with_options(EncodeOptions::new().buffer_all_input());
    encoder.feed(uncompressed)?;
    encoder.finish()?;
    let out = encoder.output().to_vec();
    encoder.advance(out.len());
    Ok(out)
}
