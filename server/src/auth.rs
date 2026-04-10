//! Authentication for the over/ACP broker.
//!
//! Today there is exactly one impl: a static-JWT validator + minter
//! backed by a shared HS256 signing key. The trait exists so deployments
//! can swap in something fancier (JWKS, mTLS, …) later without touching
//! the tunnel code.
//!
//! Per `SPEC.md` § "Authentication", the wire `Claims` carry only:
//!
//! - `sub`  — operator identity (admin) or agent_id (agent)
//! - `role` — `"admin"` or `"agent"`
//! - `user` — optional opaque user identifier (agent tokens only)
//! - `exp`  — expiry (Unix timestamp)
//! - `iss`  — issuer
//!
//! There is no `tier`, `plan`, or entitlement claim — over/ACP is OSS
//! and does not encode billing or identity hierarchy.

use chrono::Utc;
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    /// Subject. For admin tokens, this is the operator identity (UUID
    /// or service account). For agent tokens, this is the `agent_id`
    /// — the routing key on the WebSocket tunnel and REST surface.
    pub sub: Uuid,
    /// `"admin"` or `"agent"`. Decides which routes the token can hit.
    pub role: String,
    /// Optional opaque user identifier. Present only on agent tokens
    /// when the operator wants the broker to forward it. The broker
    /// itself never inspects this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<Uuid>,
    /// Expiration (Unix timestamp).
    pub exp: i64,
    /// Issuer.
    pub iss: String,
}

impl Claims {
    pub const ROLE_ADMIN: &'static str = "admin";
    pub const ROLE_AGENT: &'static str = "agent";

    /// Build an admin claim with the given TTL (in seconds from now).
    pub fn admin(sub: Uuid, ttl_secs: i64, issuer: impl Into<String>) -> Self {
        Self {
            sub,
            role: Self::ROLE_ADMIN.to_string(),
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
            role: Self::ROLE_AGENT.to_string(),
            user,
            exp: Utc::now().timestamp() + ttl_secs,
            iss: issuer.into(),
        }
    }

    pub fn is_admin(&self) -> bool {
        self.role == Self::ROLE_ADMIN
    }

    pub fn is_agent(&self) -> bool {
        self.role == Self::ROLE_AGENT
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("invalid token: {0}")]
    Invalid(String),
}

pub trait Authenticator: Send + Sync + 'static {
    /// Decode + verify a token. Verifies signature, issuer, expiry,
    /// and that `role` is one of the known values.
    fn validate(&self, token: &str) -> Result<Claims, AuthError>;

    /// Encode + sign claims into a JWT. Used by `POST /tokens` and by
    /// operators that embed the server in-process.
    fn mint(&self, claims: &Claims) -> Result<String, AuthError>;
}

pub struct StaticJwtAuthenticator {
    signing_key: String,
    issuer: String,
}

impl StaticJwtAuthenticator {
    pub fn new(signing_key: impl Into<String>, issuer: impl Into<String>) -> Self {
        Self {
            signing_key: signing_key.into(),
            issuer: issuer.into(),
        }
    }

    /// Issuer that newly-minted tokens will carry.
    pub fn issuer(&self) -> &str {
        &self.issuer
    }
}

impl Authenticator for StaticJwtAuthenticator {
    fn validate(&self, token: &str) -> Result<Claims, AuthError> {
        let mut validation = Validation::default();
        validation.set_issuer(&[&self.issuer]);
        let data = decode::<Claims>(
            token,
            &DecodingKey::from_secret(self.signing_key.as_bytes()),
            &validation,
        )
        .map_err(|e| AuthError::Invalid(e.to_string()))?;
        let claims = data.claims;
        match claims.role.as_str() {
            Claims::ROLE_ADMIN | Claims::ROLE_AGENT => Ok(claims),
            other => Err(AuthError::Invalid(format!("invalid role: {other}"))),
        }
    }

    fn mint(&self, claims: &Claims) -> Result<String, AuthError> {
        encode(
            &Header::default(),
            claims,
            &EncodingKey::from_secret(self.signing_key.as_bytes()),
        )
        .map_err(|e| AuthError::Invalid(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make() -> StaticJwtAuthenticator {
        StaticJwtAuthenticator::new("k", "overacp")
    }

    #[test]
    fn admin_round_trip() {
        let auth = make();
        let claims = Claims::admin(Uuid::new_v4(), 60, "overacp");
        let token = auth.mint(&claims).expect("mint admin");
        let decoded = auth.validate(&token).expect("validate admin");
        assert_eq!(decoded.sub, claims.sub);
        assert_eq!(decoded.role, "admin");
        assert!(decoded.is_admin());
        assert!(!decoded.is_agent());
        assert!(decoded.user.is_none());
    }

    #[test]
    fn agent_round_trip_with_user() {
        let auth = make();
        let agent_id = Uuid::new_v4();
        let user = Uuid::new_v4();
        let claims = Claims::agent(agent_id, Some(user), 60, "overacp");
        let token = auth.mint(&claims).expect("mint agent");
        let decoded = auth.validate(&token).expect("validate agent");
        assert_eq!(decoded.sub, agent_id);
        assert_eq!(decoded.role, "agent");
        assert!(decoded.is_agent());
        assert_eq!(decoded.user, Some(user));
    }

    #[test]
    fn agent_round_trip_without_user() {
        let auth = make();
        let claims = Claims::agent(Uuid::new_v4(), None, 60, "overacp");
        let token = auth.mint(&claims).expect("mint");
        let decoded = auth.validate(&token).expect("validate");
        assert!(decoded.user.is_none());
    }

    #[test]
    fn rejects_wrong_issuer() {
        let auth = make();
        let claims = Claims::agent(Uuid::new_v4(), None, 60, "other");
        let token = auth.mint(&claims).expect("mint");
        assert!(auth.validate(&token).is_err());
    }

    #[test]
    fn rejects_wrong_key() {
        let signing = StaticJwtAuthenticator::new("k1", "overacp");
        let other = StaticJwtAuthenticator::new("k2", "overacp");
        let token = signing
            .mint(&Claims::agent(Uuid::new_v4(), None, 60, "overacp"))
            .expect("mint");
        assert!(other.validate(&token).is_err());
    }

    #[test]
    fn rejects_expired_token() {
        let auth = make();
        // jsonwebtoken's default leeway is 60s; go well past it.
        let claims = Claims::agent(Uuid::new_v4(), None, -3600, "overacp");
        let token = auth.mint(&claims).expect("mint");
        assert!(auth.validate(&token).is_err());
    }

    #[test]
    fn rejects_invalid_role() {
        let auth = make();
        // Hand-roll a token with a role that isn't admin/agent.
        let bad = Claims {
            sub: Uuid::new_v4(),
            role: "superuser".into(),
            user: None,
            exp: Utc::now().timestamp() + 60,
            iss: "overacp".into(),
        };
        let token = auth.mint(&bad).expect("mint");
        let err = auth.validate(&token).expect_err("should reject");
        let AuthError::Invalid(msg) = err;
        assert!(msg.contains("invalid role"), "msg = {msg}");
    }

    #[test]
    fn issuer_accessor_returns_configured_issuer() {
        let auth = StaticJwtAuthenticator::new("k", "my-issuer");
        assert_eq!(auth.issuer(), "my-issuer");
    }
}
