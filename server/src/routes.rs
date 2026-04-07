//! Top-level Axum router for `overacp-server`.

use std::sync::Arc;

use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use serde::Deserialize;
use tracing::warn;
use uuid::Uuid;

use crate::api::{compute_nodes_router, compute_router};
use crate::state::AppState;
use crate::tunnel::run::{run_tunnel, TunnelContext};

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/tunnel/:session_id", get(tunnel_upgrade))
        .merge(compute_router())
        .merge(compute_nodes_router())
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
    Path(session_id): Path<Uuid>,
    Query(query): Query<TunnelQuery>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.to_string())
        .or(query.token);

    let Some(token) = token else {
        return (StatusCode::UNAUTHORIZED, "missing token").into_response();
    };

    let claims = match state.authenticator.validate(&token) {
        Ok(c) => c,
        Err(e) => {
            warn!("invalid tunnel token: {e}");
            return (StatusCode::UNAUTHORIZED, "invalid token").into_response();
        }
    };

    if claims.conv != session_id {
        return (StatusCode::FORBIDDEN, "token conversation mismatch").into_response();
    }

    let ctx = Arc::new(TunnelContext {
        claims: claims.clone(),
        store: state.store.clone(),
        sessions: state.sessions.clone(),
        stream_broker: state.stream_broker.clone(),
    });

    ws.on_upgrade(move |socket| run_tunnel(socket, claims, ctx))
}
