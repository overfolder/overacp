//! Authentication for the over/ACP tunnel.
//!
//! Today there is exactly one impl: a static-JWT validator backed by a
//! shared HS256 signing key. The trait exists so deployments can swap
//! in something fancier (JWKS, mTLS, …) later without touching the
//! tunnel code. Per `docs/design/protocol.md` § 2.1, the wire `Claims`
//! deliberately omit any tier/plan/entitlement field.

use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    /// Agent identity (subject).
    pub sub: Uuid,
    /// User identity.
    pub user: Uuid,
    /// Conversation ID this token is scoped to.
    pub conv: Uuid,
    /// Expiration (Unix timestamp).
    pub exp: i64,
    /// Issuer.
    pub iss: String,
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("invalid token: {0}")]
    Invalid(String),
    #[error("mint failed: {0}")]
    Mint(String),
}

pub trait Authenticator: Send + Sync + 'static {
    fn validate(&self, token: &str) -> Result<Claims, AuthError>;

    /// Mint a fresh JWT for an agent. `sub` is the agent identity,
    /// `user`/`conv` are echoed into the claims, `exp` is a Unix
    /// timestamp. Per `docs/design/protocol.md` § 2.4 the
    /// recommended TTL for the agent token is 30 days.
    fn mint_agent_token(
        &self,
        sub: Uuid,
        user: Uuid,
        conv: Uuid,
        exp: i64,
    ) -> Result<String, AuthError>;
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
        Ok(data.claims)
    }

    fn mint_agent_token(
        &self,
        sub: Uuid,
        user: Uuid,
        conv: Uuid,
        exp: i64,
    ) -> Result<String, AuthError> {
        let claims = Claims {
            sub,
            user,
            conv,
            exp,
            iss: self.issuer.clone(),
        };
        encode(
            &Header::default(),
            &claims,
            &EncodingKey::from_secret(self.signing_key.as_bytes()),
        )
        .map_err(|e| AuthError::Mint(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{encode, EncodingKey, Header};

    fn mint(key: &str, issuer: &str, conv: Uuid, exp: i64) -> String {
        let claims = Claims {
            sub: Uuid::new_v4(),
            user: Uuid::new_v4(),
            conv,
            exp,
            iss: issuer.to_string(),
        };
        encode(
            &Header::default(),
            &claims,
            &EncodingKey::from_secret(key.as_bytes()),
        )
        .unwrap()
    }

    #[test]
    fn validates_a_good_token() {
        let auth = StaticJwtAuthenticator::new("k", "overacp");
        let conv = Uuid::new_v4();
        let token = mint("k", "overacp", conv, chrono::Utc::now().timestamp() + 60);
        let claims = auth.validate(&token).unwrap();
        assert_eq!(claims.conv, conv);
    }

    #[test]
    fn rejects_wrong_issuer() {
        let auth = StaticJwtAuthenticator::new("k", "overacp");
        let token = mint(
            "k",
            "other",
            Uuid::new_v4(),
            chrono::Utc::now().timestamp() + 60,
        );
        assert!(auth.validate(&token).is_err());
    }

    #[test]
    fn rejects_wrong_key() {
        let auth = StaticJwtAuthenticator::new("k1", "overacp");
        let token = mint(
            "k2",
            "overacp",
            Uuid::new_v4(),
            chrono::Utc::now().timestamp() + 60,
        );
        assert!(auth.validate(&token).is_err());
    }
}
