//! `ExtractError` (Extractors.swift:3-20): why a file went into the index unreadable, or out of
//! it entirely. The description strings are what `index-service` reports and what the app shows.

use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ExtractError {
    #[error("unreadable: {why}")]
    Unreadable { why: String },
    #[error("small image {width}×{height}")]
    TooSmall { width: i64, height: i64 },
    #[error("password-protected")]
    Locked,
    #[error("{why}")]
    Empty { why: String },
    /// Not for the index after all, once read (code a program made, or holding a private key).
    #[error("{why}")]
    LeftOut { why: String },
}
