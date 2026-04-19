//! Error and `Result` types for DEFLATE encoding and decoding.

use alloc::borrow::Cow;

/// Errors returned by the encoder and decoder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The input is malformed or violates the expected format.
    InvalidData(Cow<'static, str>),
    /// The input uses a feature that this library does not support.
    Unsupported(Cow<'static, str>),
}

/// Convenience alias for `Result<T, Error>`.
pub type Result<T> = core::result::Result<T, Error>;

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidData(message) => write!(f, "invalid data: {message}"),
            Self::Unsupported(message) => write!(f, "unsupported: {message}"),
        }
    }
}

impl core::error::Error for Error {}
