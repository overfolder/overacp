//! Top-level Axum router for `overacp-server`.
//!
//! Wire-up of the REST surface documented in `SPEC.md` and
//! `docs/design/protocol.md`. Authentication is JWT-based (Bearer
//! token in the `Authorization` header, or `?token=...` for the
//! WebSocket upgrade path). Two roles:
//!
//! - `admin` — full access to every endpoint, plus the ability to
//!   mint agent tokens via `POST /tokens`.
//! - `agent` — scoped to a single `agent_id` (the token's `sub`).
//!   Can hold the matching `/tunnel/:agent_id` WebSocket and call
//!   the agent-scoped REST endpoints for that same id.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{from_fn_with_state, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use serde::Deserialize;
use tracing::warn;
use uuid::Uuid;

use crate::api::{agents, tokens_router};
use crate::auth::{Authenticator, Claims};
use crate::state::AppState;
use crate::tunnel::run::{run_tunnel, TunnelContext};

pub fn router(state: AppState) -> Router {
    // Routes split by authorization model (per SPEC.md § "Route
    // authorization"):
    //
    // - `admin_only` — admin JWTs only. Registry listing, describe,
    //   force-disconnect, and agent-token minting.
    // - `agent_scoped` — admin JWTs OR an agent JWT whose `sub`
    //   matches the `{id}` path segment. The streaming surface the
    //   operator's web frontend holds an agent token for.
    let admin_only = Router::new()
        .merge(agents::admin_router())
        .merge(tokens_router())
        .route_layer(from_fn_with_state(state.clone(), require_admin));

    let agent_scoped = agents::agent_scoped_router()
        .route_layer(from_fn_with_state(state.clone(), require_agent_or_admin));

    Router::new()
        .route("/healthz", get(healthz))
        .route("/tunnel/{agent_id}", get(tunnel_upgrade))
        .merge(admin_only)
        .merge(agent_scoped)
        .with_state(state)
}

async fn healthz() -> &'static str {
    "ok"
}

// ═══════════════════════════════════════════════════════════════
//  JWT auth middleware — the broker-shaped REST surface.
// ═══════════════════════════════════════════════════════════════

/// Extract the Bearer token from the request, validate it via the
/// authenticator, return the `Claims` or a boxed error response.
/// Boxed so the `Err` variant stays small (axum's `Response` is
/// large and clippy flags `result_large_err` otherwise).
fn validate_bearer(state: &AppState, headers: &HeaderMap) -> Result<Claims, Box<Response>> {
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let Some(token) = token else {
        return Err(Box::new(
            (StatusCode::UNAUTHORIZED, "missing bearer token").into_response(),
        ));
    };
    state.authenticator.validate(token).map_err(|e| {
        warn!("invalid rest token: {e}");
        Box::new((StatusCode::UNAUTHORIZED, "invalid token").into_response())
    })
}

/// Middleware: require an admin JWT. Used for `POST /tokens` and
/// any other operator-only endpoint.
async fn require_admin(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Request<Body>,
    next: Next,
) -> Response {
    let claims = match validate_bearer(&state, &headers) {
        Ok(c) => c,
        Err(resp) => return *resp,
    };
    if !claims.is_admin() {
        return (StatusCode::FORBIDDEN, "admin role required").into_response();
    }
    next.run(request).await
}

/// Middleware: require either an admin JWT or an agent JWT whose
/// `sub` matches the `{id}` path segment. Used for the agent-scoped
/// streaming routes: `POST /agents/{id}/messages`,
/// `GET /agents/{id}/stream`, and `POST /agents/{id}/cancel`.
/// Admin-only routes (`GET /agents`, `GET /agents/{id}`,
/// `DELETE /agents/{id}`, `POST /tokens`) go through
/// [`require_admin`] instead.
async fn require_agent_or_admin(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Request<Body>,
    next: Next,
) -> Response {
    let claims = match validate_bearer(&state, &headers) {
        Ok(c) => c,
        Err(resp) => return *resp,
    };
    if claims.is_admin() {
        return next.run(request).await;
    }
    if !claims.is_agent() {
        return (StatusCode::FORBIDDEN, "admin or agent role required").into_response();
    }
    // Agent tokens: `sub` must match the `{id}` path param. Every
    // route wired through this middleware is an agent-scoped
    // route (`/agents/{id}/messages`, `/agents/{id}/stream`,
    // `/agents/{id}/cancel`), so `path_agent_id` is always `Some`
    // — the `None` arm exists only to make the function total
    // rather than panic if a future route accidentally hangs a
    // non-scoped path off this middleware.
    match path_agent_id(request.uri().path()) {
        Some(id) if id == claims.sub => next.run(request).await,
        Some(_) => (StatusCode::FORBIDDEN, "token agent mismatch").into_response(),
        None => (StatusCode::FORBIDDEN, "admin role required for this route").into_response(),
    }
}

/// Pull the `{id}` segment out of an `/agents/{id}[/...]` path.
/// Returns `None` for `/agents` (the list route) or anything that
/// doesn't match the shape.
fn path_agent_id(path: &str) -> Option<Uuid> {
    let rest = path.strip_prefix("/agents/")?;
    let id_part = rest.split('/').next()?;
    Uuid::parse_str(id_part).ok()
}

// ═══════════════════════════════════════════════════════════════
//  WebSocket tunnel upgrade.
// ═══════════════════════════════════════════════════════════════

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
            Self::AgentMismatch => (StatusCode::FORBIDDEN, "token agent mismatch").into_response(),
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

    // ── path_agent_id ──

    #[test]
    fn path_agent_id_extracts_uuid() {
        let id = Uuid::new_v4();
        let path = format!("/agents/{id}/messages");
        assert_eq!(path_agent_id(&path), Some(id));
    }

    #[test]
    fn path_agent_id_matches_bare_agent_route() {
        let id = Uuid::new_v4();
        assert_eq!(path_agent_id(&format!("/agents/{id}")), Some(id));
    }

    #[test]
    fn path_agent_id_returns_none_for_list_route() {
        assert_eq!(path_agent_id("/agents"), None);
    }

    #[test]
    fn path_agent_id_returns_none_for_unrelated_route() {
        assert_eq!(path_agent_id("/tokens"), None);
        assert_eq!(path_agent_id("/healthz"), None);
    }

    #[test]
    fn path_agent_id_returns_none_for_non_uuid() {
        assert_eq!(path_agent_id("/agents/not-a-uuid/messages"), None);
    }

    // ── TunnelAuthRejection::into_response matrix ──

    #[test]
    fn rejection_missing_token_into_response_is_401() {
        let resp = TunnelAuthRejection::MissingToken.into_response();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn rejection_invalid_token_into_response_is_401() {
        let resp = TunnelAuthRejection::InvalidToken.into_response();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn rejection_wrong_role_into_response_is_403() {
        let resp = TunnelAuthRejection::WrongRole.into_response();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn rejection_agent_mismatch_into_response_is_403() {
        let resp = TunnelAuthRejection::AgentMismatch.into_response();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }
}
