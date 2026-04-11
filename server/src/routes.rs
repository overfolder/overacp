//! Top-level Axum router for `overacp-server`.

use std::sync::Arc;

use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::from_fn_with_state;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use serde::Deserialize;
use tracing::warn;
use uuid::Uuid;

use crate::api::{agents_router, compute_nodes_router, compute_router};
use crate::auth::{Authenticator, Claims};
use crate::basic_auth::require_basic_auth;
use crate::state::AppState;
use crate::tunnel::run::{run_tunnel, TunnelContext};

pub fn router(state: AppState) -> Router {
    // Control-plane sub-routers (operator/orchestrator-facing). These
    // sit behind HTTP Basic auth — see `basic_auth::require_basic_auth`.
    // The compute-pool surface is from the superseded controlplane
    // design and will be removed in Phase 5 of the broker refactor.
    let control_plane = Router::new()
        .merge(compute_router())
        .merge(compute_nodes_router())
        .route_layer(from_fn_with_state(state.clone(), require_basic_auth));

    Router::new()
        .route("/healthz", get(healthz))
        .route("/tunnel/:agent_id", get(tunnel_upgrade))
        // Agent-facing REST adapters (§ 3.5) — JWT only, no Basic auth.
        .merge(agents_router())
        .merge(control_plane)
        .with_state(state)
}

async fn healthz() -> &'static str {
    "ok"
}

#[derive(Debug, Deserialize)]
struct TunnelQuery {
    token: Option<String>,
}

async fn tunnel_upgrade(
    ws: WebSocketUpgrade,
    Path(agent_id): Path<Uuid>,
    Query(query): Query<TunnelQuery>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let token = extract_bearer_token(&headers, query.token);
    let claims = match authorize_tunnel(&*state.authenticator, agent_id, token) {
        Ok(c) => c,
        Err(rej) => return rej.into_response(),
    };

    let ctx = Arc::new(TunnelContext {
        claims: claims.clone(),
        store: state.store.clone(),
        sessions: state.sessions.clone(),
        registry: state.registry.clone(),
        message_queue: state.message_queue.clone(),
        stream_broker: state.stream_broker.clone(),
        boot_provider: state.boot_provider.clone(),
        tool_host: state.tool_host.clone(),
        quota_policy: state.quota_policy.clone(),
    });

    ws.on_upgrade(move |socket| run_tunnel(socket, claims, ctx))
}

/// Pull a Bearer token off either the `Authorization` header or the
/// `?token=` query string. Header wins if both are present.
fn extract_bearer_token(headers: &HeaderMap, query_token: Option<String>) -> Option<String> {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.to_string())
        .or(query_token)
}

/// Rejection produced by `authorize_tunnel`. Mapped to a response in
/// the handler. The variants are deliberately fine-grained so the
/// tests can distinguish them.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TunnelAuthRejection {
    MissingToken,
    InvalidToken,
    WrongRole,
    AgentMismatch,
}

impl IntoResponse for TunnelAuthRejection {
    fn into_response(self) -> Response {
        match self {
            Self::MissingToken => (StatusCode::UNAUTHORIZED, "missing token").into_response(),
            Self::InvalidToken => (StatusCode::UNAUTHORIZED, "invalid token").into_response(),
            Self::WrongRole => (StatusCode::FORBIDDEN, "agent role required").into_response(),
            Self::AgentMismatch => {
                (StatusCode::FORBIDDEN, "token agent mismatch").into_response()
            }
        }
    }
}

/// Run the JWT auth gate for `/tunnel/:agent_id`. Pulled out of the
/// handler so it can be unit-tested without dragging axum's
/// `WebSocketUpgrade` extractor (which short-circuits in test
/// harnesses that don't carry a real hyper upgrade extension).
pub(crate) fn authorize_tunnel(
    authenticator: &dyn Authenticator,
    agent_id: Uuid,
    token: Option<String>,
) -> Result<Claims, TunnelAuthRejection> {
    let Some(token) = token else {
        return Err(TunnelAuthRejection::MissingToken);
    };
    let claims = authenticator.validate(&token).map_err(|e| {
        warn!("invalid tunnel token: {e}");
        TunnelAuthRejection::InvalidToken
    })?;
    if !claims.is_agent() {
        return Err(TunnelAuthRejection::WrongRole);
    }
    if claims.sub != agent_id {
        return Err(TunnelAuthRejection::AgentMismatch);
    }
    Ok(claims)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::StaticJwtAuthenticator;

    fn auth() -> StaticJwtAuthenticator {
        StaticJwtAuthenticator::new("k", "overacp")
    }

    #[test]
    fn missing_token_is_rejected() {
        let result = authorize_tunnel(&auth(), Uuid::new_v4(), None);
        assert_eq!(result.unwrap_err(), TunnelAuthRejection::MissingToken);
    }

    #[test]
    fn malformed_token_is_rejected() {
        let result = authorize_tunnel(&auth(), Uuid::new_v4(), Some("not-a-jwt".into()));
        assert_eq!(result.unwrap_err(), TunnelAuthRejection::InvalidToken);
    }

    #[test]
    fn admin_token_is_rejected() {
        let a = auth();
        let token = a
            .mint(&Claims::admin(Uuid::new_v4(), 60, "overacp"))
            .unwrap();
        let result = authorize_tunnel(&a, Uuid::new_v4(), Some(token));
        assert_eq!(result.unwrap_err(), TunnelAuthRejection::WrongRole);
    }

    #[test]
    fn agent_token_with_wrong_sub_is_rejected() {
        let a = auth();
        let token = a
            .mint(&Claims::agent(Uuid::new_v4(), None, 60, "overacp"))
            .unwrap();
        let result = authorize_tunnel(&a, Uuid::new_v4(), Some(token));
        assert_eq!(result.unwrap_err(), TunnelAuthRejection::AgentMismatch);
    }

    #[test]
    fn agent_token_with_matching_sub_passes() {
        let a = auth();
        let agent_id = Uuid::new_v4();
        let token = a
            .mint(&Claims::agent(agent_id, None, 60, "overacp"))
            .unwrap();
        let claims = authorize_tunnel(&a, agent_id, Some(token)).expect("authorized");
        assert_eq!(claims.sub, agent_id);
        assert!(claims.is_agent());
    }

    #[test]
    fn extracts_bearer_from_header() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer abc123".parse().unwrap());
        assert_eq!(
            extract_bearer_token(&headers, None),
            Some("abc123".to_string())
        );
    }

    #[test]
    fn extracts_token_from_query_when_no_header() {
        let headers = HeaderMap::new();
        assert_eq!(
            extract_bearer_token(&headers, Some("from-query".into())),
            Some("from-query".to_string())
        );
    }

    #[test]
    fn header_wins_over_query() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer from-header".parse().unwrap());
        assert_eq!(
            extract_bearer_token(&headers, Some("from-query".into())),
            Some("from-header".to_string())
        );
    }
}
