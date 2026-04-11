//! `POST /tokens` — admin-only endpoint for minting agent JWTs.
//!
//! Per `SPEC.md` § "Minting agent tokens":
//!
//! ```text
//! POST /tokens
//! Authorization: Bearer <admin-jwt>
//! Body: { "agent_id": "uuid", "user": "uuid"?, "ttl_secs": 2592000? }
//!
//! Response: { "token": "eyJ...", "claims": { "sub": "...", ... } }
//! ```
//!
//! The endpoint is a thin wrapper around `Authenticator::mint`; the
//! actual signing lives in [`crate::auth`]. Operators embedding the
//! server in-process can skip the HTTP round-trip and call
//! `Claims::agent(...)` + `authenticator.mint(...)` directly.

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::error::ApiError;
use crate::auth::Claims;
use crate::state::AppState;

/// Default agent JWT TTL: 30 days.
pub const DEFAULT_AGENT_TTL_SECS: i64 = 30 * 24 * 60 * 60;

pub fn router() -> Router<AppState> {
    Router::new().route("/tokens", post(mint_token))
}

#[derive(Debug, Clone, Deserialize)]
pub struct MintRequest {
    /// The agent's `sub` claim and routing key.
    pub agent_id: Uuid,
    /// Optional opaque user identifier forwarded to the agent via
    /// the `user` claim.
    #[serde(default)]
    pub user: Option<Uuid>,
    /// Lifetime of the minted token, in seconds. Defaults to 30
    /// days per `SPEC.md`.
    #[serde(default)]
    pub ttl_secs: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MintResponse {
    pub token: String,
    pub claims: Claims,
}

/// `POST /tokens` handler. The router layer is responsible for
/// gating this to admin JWTs before the handler runs.
async fn mint_token(
    State(state): State<AppState>,
    Json(req): Json<MintRequest>,
) -> Result<(StatusCode, Json<MintResponse>), ApiError> {
    let ttl = req.ttl_secs.unwrap_or(DEFAULT_AGENT_TTL_SECS);
    if ttl <= 0 {
        return Err(ApiError::BadRequest(
            "ttl_secs must be positive".into(),
        ));
    }

    // Read the issuer straight off the authenticator so the minted
    // token validates with the same instance that signed it,
    // regardless of which issuer string the operator configured.
    let issuer = state.authenticator.issuer().to_string();
    let claims = Claims::agent(req.agent_id, req.user, ttl, issuer);
    let token = state
        .authenticator
        .mint(&claims)
        .map_err(|e| ApiError::Internal(format!("mint failed: {e}")))?;

    Ok((
        StatusCode::CREATED,
        Json(MintResponse { token, claims }),
    ))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::api::default_registry;
    use crate::auth::StaticJwtAuthenticator;
    use crate::state::AppState;
    use crate::store::InMemoryStore;

    fn state_with_issuer(issuer: &'static str) -> AppState {
        AppState::new(
            Arc::new(InMemoryStore::new()),
            Arc::new(default_registry()),
            Arc::new(StaticJwtAuthenticator::new("test-key", issuer)),
        )
    }

    #[tokio::test]
    async fn mint_default_ttl_round_trip() {
        let state = state_with_issuer("overacp");
        let authenticator = state.authenticator.clone();

        let agent_id = Uuid::new_v4();
        let user = Some(Uuid::new_v4());

        let (status, Json(resp)) = mint_token(
            State(state),
            Json(MintRequest {
                agent_id,
                user,
                ttl_secs: None,
            }),
        )
        .await
        .expect("mint");

        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(resp.claims.sub, agent_id);
        assert!(resp.claims.is_agent());
        assert_eq!(resp.claims.user, user);

        // Round-trip: the minted token validates with the same
        // authenticator that issued it.
        let round_tripped = authenticator.validate(&resp.token).unwrap();
        assert_eq!(round_tripped.sub, agent_id);
        assert!(round_tripped.is_agent());
    }

    #[tokio::test]
    async fn mint_explicit_ttl_is_honoured() {
        let state = state_with_issuer("overacp");
        let (_status, Json(resp)) = mint_token(
            State(state),
            Json(MintRequest {
                agent_id: Uuid::new_v4(),
                user: None,
                ttl_secs: Some(120),
            }),
        )
        .await
        .unwrap();
        let remaining = resp.claims.exp - chrono::Utc::now().timestamp();
        assert!(
            (110..=125).contains(&remaining),
            "remaining = {remaining}"
        );
    }

    #[tokio::test]
    async fn mint_rejects_non_positive_ttl() {
        let state = state_with_issuer("overacp");
        let err = mint_token(
            State(state),
            Json(MintRequest {
                agent_id: Uuid::new_v4(),
                user: None,
                ttl_secs: Some(0),
            }),
        )
        .await
        .expect_err("zero ttl");
        assert!(matches!(err, ApiError::BadRequest(_)));
    }

    #[tokio::test]
    async fn mint_omits_user_when_not_supplied() {
        let state = state_with_issuer("overacp");
        let (_status, Json(resp)) = mint_token(
            State(state),
            Json(MintRequest {
                agent_id: Uuid::new_v4(),
                user: None,
                ttl_secs: None,
            }),
        )
        .await
        .unwrap();
        assert!(resp.claims.user.is_none());
    }
}
