//! JWT session claims for the agent ↔ server tunnel.
//!
//! This crate owns the canonical `Claims` shape used on the over/ACP
//! wire. Per `SPEC.md` § "Authentication", `Claims` carry only:
//!
//! - `sub`  — operator identity (admin) or agent_id (agent)
//! - `role` — `"admin"` or `"agent"`
//! - `user` — optional opaque user identifier (agent tokens only)
//! - `exp`  — expiry (Unix timestamp)
//! - `iss`  — issuer
//!
//! over/ACP intentionally has **no tier, plan, or entitlement claim**.
//! over/ACP is OSS and does not dictate billing models; deployments
//! that need per-user policy decisions should carry that state in
//! their own database keyed on `user`, not in the token.
//!
//! The mint/validate/peek helpers in this module are pure-function
//! conveniences; the server's `Authenticator` trait (in
//! `overacp-server::auth`) wraps them behind a swappable interface.

use chrono::Utc;
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::ProtocolError;

/// Default token lifetime, in seconds. Consumers may pass any TTL to
/// `mint_token`; this is just the recommended default.
pub const DEFAULT_TOKEN_TTL_SECS: i64 = 3600;

/// Role claim value for operator / admin tokens.
pub const ROLE_ADMIN: &str = "admin";

/// Role claim value for agent tokens.
pub const ROLE_AGENT: &str = "agent";

/// JWT claims carried in every over/ACP session token.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Claims {
    /// Subject. For admin tokens, this is the operator identity
    /// (UUID or service account). For agent tokens, this is the
    /// `agent_id` — the routing key on the WebSocket tunnel and
    /// REST surface.
    pub sub: Uuid,
    /// `"admin"` or `"agent"`. Decides which routes the token can
    /// hit.
    pub role: String,
    /// Optional opaque user identifier. Present only on agent
    /// tokens when the operator wants the broker to forward it.
    /// The broker itself never inspects this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<Uuid>,
    /// Expiration as a Unix timestamp (seconds since epoch).
    pub exp: i64,
    /// Issuer string. Must match what `validate_token` is told to
    /// expect for validation to succeed.
    pub iss: String,
}

impl Claims {
    /// Build an admin claim with the given TTL (in seconds from now).
    pub fn admin(sub: Uuid, ttl_secs: i64, issuer: impl Into<String>) -> Self {
        Self {
            sub,
            role: ROLE_ADMIN.to_string(),
            user: None,
            exp: Utc::now().timestamp() + ttl_secs,
            iss: issuer.into(),
        }
    }

    /// Build an agent claim scoped to `agent_id`, with optional user
    /// identifier and the given TTL (in seconds from now).
    pub fn agent(
        agent_id: Uuid,
        user: Option<Uuid>,
        ttl_secs: i64,
        issuer: impl Into<String>,
    ) -> Self {
        Self {
            sub: agent_id,
            role: ROLE_AGENT.to_string(),
            user,
            exp: Utc::now().timestamp() + ttl_secs,
            iss: issuer.into(),
        }
    }

    /// True if `role == "admin"`.
    pub fn is_admin(&self) -> bool {
        self.role == ROLE_ADMIN
    }

    /// True if `role == "agent"`.
    pub fn is_agent(&self) -> bool {
        self.role == ROLE_AGENT
    }
}

/// Mint a new session token. The claims carry their own `exp` and
/// `iss` — build them with [`Claims::admin`] or [`Claims::agent`].
pub fn mint_token(signing_key: &str, claims: &Claims) -> Result<String, ProtocolError> {
    let token = encode(
        &Header::default(),
        claims,
        &EncodingKey::from_secret(signing_key.as_bytes()),
    )?;
    Ok(token)
}

/// Validate a session token's signature, expiration, and issuer, and
/// return its claims. Also rejects tokens whose `role` is not one of
/// [`ROLE_ADMIN`] or [`ROLE_AGENT`].
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
    let claims = data.claims;
    if !claims.is_admin() && !claims.is_agent() {
        return Err(ProtocolError::InvalidRole(claims.role));
    }
    Ok(claims)
}

/// Decode the claims of a token **without verifying its signature,
/// expiration, or issuer**. Useful when the agent needs the `sub`
/// claim to build the tunnel URL before contacting the server (the
/// server still validates the token authoritatively on the way in).
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
