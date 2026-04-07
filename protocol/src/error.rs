//! Protocol-level errors.

use jsonwebtoken::errors::Error as JwtError;
use thiserror::Error;

/// Errors that can occur while encoding, decoding, or validating
/// over/ACP protocol payloads.
#[derive(Debug, Error)]
pub enum ProtocolError {
    /// JWT minting, validation, signature, expiry, or issuer mismatch.
    #[error("jwt: {0}")]
    Jwt(#[from] JwtError),

    /// JSON serialization or deserialization failure.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),

    /// A token was structurally malformed (not three dot-separated segments).
    #[error("malformed jwt: {0}")]
    Malformed(&'static str),
}
