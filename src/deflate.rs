//! Raw DEFLATE (RFC 1951) encoder and decoder.

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
    if !decoder.is_finished() {
        return Err(Error::InvalidData(
            "deflate stream ended before the final block".into(),
        ));
    }
    let out = decoder.output().to_vec();
    decoder.advance(out.len());
    Ok(out)
}
