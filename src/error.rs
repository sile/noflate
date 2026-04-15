//! Error and `Result` types for DEFLATE encoding and decoding.

use std::borrow::Cow;

/// Errors returned by the encoder and decoder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The input violates the DEFLATE specification.
    InvalidData(Cow<'static, str>),
    /// The input uses a DEFLATE feature that this library does not support.
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
