//! JWT session claims for the agent ↔ server tunnel.
//!
//! Tokens are short-lived (default 1 hour), scoped to a single
//! conversation, and accepted by both the WebSocket tunnel and the
//! LLM proxy. The signing key, the issuer string, and the TTL are all
//! parameters of `mint_token` / `validate_token` — this crate bakes no
//! product-specific values into the wire format.

use chrono::Utc;
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::ProtocolError;

/// Default token lifetime, in seconds. Consumers may pass any TTL to
/// `mint_token`; this is just the recommended default.
pub const DEFAULT_TOKEN_TTL_SECS: i64 = 3600;

/// JWT claims carried in every over/ACP session token.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Claims {
    /// Agent identity (subject).
    pub sub: Uuid,
    /// User identity.
    pub user: Uuid,
    /// Conversation ID this token is scoped to.
    pub conv: Uuid,
    /// User tier (e.g. "free", "paid", "premium"). Free-form string;
    /// the protocol does not assign meaning beyond round-tripping it.
    pub tier: String,
    /// Expiration as a Unix timestamp (seconds since epoch).
    pub exp: i64,
    /// Issuer string. Must match what `validate_token` is told to
    /// expect for validation to succeed.
    pub iss: String,
}

/// Mint a new session token.
///
/// The caller chooses the issuer string and the TTL. To use the
/// recommended one-hour lifetime pass [`DEFAULT_TOKEN_TTL_SECS`].
pub fn mint_token(
    signing_key: &str,
    issuer: &str,
    ttl_secs: i64,
    agent_id: Uuid,
    user_id: Uuid,
    conversation_id: Uuid,
    tier: &str,
) -> Result<String, ProtocolError> {
    let now = Utc::now().timestamp();
    let claims = Claims {
        sub: agent_id,
        user: user_id,
        conv: conversation_id,
        tier: tier.to_string(),
        exp: now + ttl_secs,
        iss: issuer.to_string(),
    };

    let token = encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(signing_key.as_bytes()),
    )?;
    Ok(token)
}

/// Validate a session token's signature, expiration, and issuer, and
/// return its claims.
pub fn validate_token(
    signing_key: &str,
    issuer: &str,
    token: &str,
) -> Result<Claims, ProtocolError> {
    let mut validation = Validation::default();
    validation.set_issuer(&[issuer]);

    let data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(signing_key.as_bytes()),
        &validation,
    )?;
    Ok(data.claims)
}

/// Decode the claims of a token **without verifying its signature,
/// expiration, or issuer**. Useful when the agent needs the `conv`
/// claim to build the tunnel URL before contacting the server (the
/// server still validates the token on the way in).
///
/// Never use the returned claims for any authorization decision.
pub fn peek_claims_unverified(token: &str) -> Result<Claims, ProtocolError> {
    let mut validation = Validation::default();
    validation.insecure_disable_signature_validation();
    validation.validate_exp = false;
    validation.validate_nbf = false;
    // No issuer requirement.
    validation.required_spec_claims.clear();

    // Use a dummy key — signature validation is disabled.
    let data = decode::<Claims>(token, &DecodingKey::from_secret(b"unused"), &validation)?;
    Ok(data.claims)
}
