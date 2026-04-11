//! End-to-end integration coverage for the broker REST surface
//! landed in Phase 4b.
//!
//! These tests build the full `router(state)` with every middleware
//! attached, mint real JWTs via `Authenticator::mint`, and drive
//! requests through `tower::ServiceExt::oneshot`. They verify:
//!
//! 1. Authorization rules: admin vs agent JWT, path `{id}` scoping,
//!    missing/invalid token handling.
//! 2. `POST /tokens`: admin-only minting, round-trips through
//!    `Authenticator::validate`.
//! 3. `POST /agents/{id}/messages`: inline delivery when the
//!    registry has a live entry, queue buffering otherwise, 503
//!    back-pressure on overflow.
//! 4. `GET /agents`, `GET /agents/{id}`, `DELETE /agents/{id}`:
//!    registry surface.
//! 5. `POST /agents/{id}/cancel`: no-op when disconnected.
//!
//! WebSocket upgrades are tested separately in `tunnel_auth` /
//! `dispatch_wireup`; here we stop at the HTTP layer.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use overacp_server::auth::{Authenticator, Claims};
use overacp_server::registry::{AgentEntry, MessageQueue};
use overacp_server::{router, AppState, StaticJwtAuthenticator};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tower::ServiceExt;
use uuid::Uuid;

const SIGNING_KEY: &str = "e2e-signing-key";
const ISSUER: &str = "overacp";

fn fresh_state() -> (AppState, Arc<dyn Authenticator>) {
    let auth: Arc<dyn Authenticator> =
        Arc::new(StaticJwtAuthenticator::new(SIGNING_KEY, ISSUER));
    let state = AppState::new(auth.clone());
    (state, auth)
}

fn state_with_queue_capacity(cap: usize) -> (AppState, Arc<dyn Authenticator>) {
    let (state, auth) = fresh_state();
    let state = AppState {
        message_queue: MessageQueue::new(cap),
        ..state
    };
    (state, auth)
}

fn mint_admin(auth: &Arc<dyn Authenticator>) -> String {
    auth.mint(&Claims::admin(Uuid::new_v4(), 300, ISSUER))
        .expect("mint admin")
}

fn mint_agent_for(auth: &Arc<dyn Authenticator>, agent_id: Uuid) -> String {
    auth.mint(&Claims::agent(agent_id, None, 300, ISSUER))
        .expect("mint agent")
}

/// Register a fake tunnel entry in the registry and return its
/// receiving channel. Mirrors the `run_tunnel` registration path.
fn register_fake(state: &AppState, agent_id: Uuid) -> mpsc::UnboundedReceiver<String> {
    let (tx, rx) = mpsc::unbounded_channel::<String>();
    let claims = Claims::agent(agent_id, None, 300, ISSUER);
    state.registry.register(agent_id, AgentEntry::new(tx, claims));
    rx
}

async fn oneshot(app: axum::Router, req: Request<Body>) -> (StatusCode, Value) {
    let res = app.oneshot(req).await.unwrap();
    let status = res.status();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body).unwrap_or(Value::Null)
    };
    (status, value)
}

fn bearer(req: Request<Body>, token: &str) -> Request<Body> {
    let (mut parts, body) = req.into_parts();
    parts
        .headers
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    Request::from_parts(parts, body)
}

fn json_post(uri: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn empty_get(uri: &str) -> Request<Body> {
    Request::builder()
        .method(Method::GET)
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

fn empty_delete(uri: &str) -> Request<Body> {
    Request::builder()
        .method(Method::DELETE)
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

// ══════════════════════════════════════════════════════════════
//  Healthz smoke
// ══════════════════════════════════════════════════════════════

#[tokio::test]
async fn healthz_is_always_reachable() {
    let (state, _) = fresh_state();
    let app = router(state);
    let res = app
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

// ══════════════════════════════════════════════════════════════
//  POST /tokens (admin only)
// ══════════════════════════════════════════════════════════════

#[tokio::test]
async fn mint_token_as_admin_round_trip() {
    let (state, auth) = fresh_state();
    let admin_jwt = mint_admin(&auth);
    let app = router(state);

    let agent_id = Uuid::new_v4();
    let req = bearer(
        json_post("/tokens", json!({ "agent_id": agent_id, "ttl_secs": 60 })),
        &admin_jwt,
    );
    let (status, body) = oneshot(app, req).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["claims"]["sub"], agent_id.to_string());
    assert_eq!(body["claims"]["role"], "agent");

    // The minted token validates against the same authenticator.
    let token = body["token"].as_str().unwrap();
    let claims = auth.validate(token).unwrap();
    assert_eq!(claims.sub, agent_id);
}

#[tokio::test]
async fn mint_token_requires_admin_token() {
    let (state, auth) = fresh_state();
    let app = router(state);

    // No token at all.
    let (status, _) = oneshot(
        app.clone(),
        json_post("/tokens", json!({ "agent_id": Uuid::new_v4() })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Agent token is not sufficient.
    let agent_jwt = mint_agent_for(&auth, Uuid::new_v4());
    let (status, _) = oneshot(
        app,
        bearer(
            json_post("/tokens", json!({ "agent_id": Uuid::new_v4() })),
            &agent_jwt,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

// ══════════════════════════════════════════════════════════════
//  GET /agents, GET /agents/{id}, DELETE /agents/{id}
// ══════════════════════════════════════════════════════════════

#[tokio::test]
async fn list_agents_requires_admin() {
    let (state, auth) = fresh_state();
    let app = router(state);

    // Agent token — 403 because /agents has no {id} the token can
    // be scoped to.
    let agent_jwt = mint_agent_for(&auth, Uuid::new_v4());
    let (status, _) = oneshot(app.clone(), bearer(empty_get("/agents"), &agent_jwt)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Admin token — OK.
    let admin_jwt = mint_admin(&auth);
    let (status, body) = oneshot(app, bearer(empty_get("/agents"), &admin_jwt)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["agents"], json!([]));
}

#[tokio::test]
async fn describe_agent_is_admin_only_per_spec() {
    // Per SPEC.md § "Route authorization", GET /agents/{id} is
    // admin-only even if the agent JWT's `sub` matches. Web
    // frontends that want to read their own agent's state should
    // hit the agent-scoped streaming endpoints instead.
    let (state, auth) = fresh_state();
    let agent_id = Uuid::new_v4();
    let _rx = register_fake(&state, agent_id);
    let app = router(state);

    // Admin can describe any agent.
    let admin_jwt = mint_admin(&auth);
    let (status, body) = oneshot(
        app.clone(),
        bearer(empty_get(&format!("/agents/{agent_id}")), &admin_jwt),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["agent_id"], agent_id.to_string());
    assert_eq!(body["connected"], true);

    // Agent tokens are rejected, even when `sub == id`.
    let own_jwt = mint_agent_for(&auth, agent_id);
    let (status, _) = oneshot(
        app.clone(),
        bearer(empty_get(&format!("/agents/{agent_id}")), &own_jwt),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Different-agent tokens are also rejected (same forbidden).
    let other_jwt = mint_agent_for(&auth, Uuid::new_v4());
    let (status, _) = oneshot(
        app,
        bearer(empty_get(&format!("/agents/{agent_id}")), &other_jwt),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn describe_unknown_agent_returns_404_for_admin() {
    let (state, auth) = fresh_state();
    let app = router(state);
    let admin_jwt = mint_admin(&auth);
    let (status, _) = oneshot(
        app,
        bearer(empty_get(&format!("/agents/{}", Uuid::new_v4())), &admin_jwt),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_agent_admin_only_and_disconnects() {
    let (state, auth) = fresh_state();
    let agent_id = Uuid::new_v4();
    let _rx = register_fake(&state, agent_id);

    let admin_jwt = mint_admin(&auth);
    let app = router(state.clone());

    let (status, _) = oneshot(
        app,
        bearer(empty_delete(&format!("/agents/{agent_id}")), &admin_jwt),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert!(!state.registry.is_connected(agent_id));
}

#[tokio::test]
async fn delete_agent_rejects_agent_tokens() {
    // Even the agent whose `sub` matches the path cannot
    // force-disconnect itself — DELETE /agents/{id} is admin-only
    // per SPEC.md § "Route authorization".
    let (state, auth) = fresh_state();
    let agent_id = Uuid::new_v4();
    let _rx = register_fake(&state, agent_id);
    let app = router(state.clone());

    // Own token → 403.
    let own_jwt = mint_agent_for(&auth, agent_id);
    let (status, _) = oneshot(
        app.clone(),
        bearer(empty_delete(&format!("/agents/{agent_id}")), &own_jwt),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Different-agent token → also 403.
    let other_jwt = mint_agent_for(&auth, Uuid::new_v4());
    let (status, _) = oneshot(
        app,
        bearer(empty_delete(&format!("/agents/{agent_id}")), &other_jwt),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // And the registry entry is still alive — neither attempt
    // took effect.
    assert!(state.registry.is_connected(agent_id));
}

// ══════════════════════════════════════════════════════════════
//  POST /agents/{id}/messages
// ══════════════════════════════════════════════════════════════

#[tokio::test]
async fn send_message_live_delivery_via_admin_token() {
    let (state, auth) = fresh_state();
    let agent_id = Uuid::new_v4();
    let mut rx = register_fake(&state, agent_id);
    let admin_jwt = mint_admin(&auth);
    let app = router(state);

    let req = bearer(
        json_post(
            &format!("/agents/{agent_id}/messages"),
            json!({ "role": "user", "content": "hi" }),
        ),
        &admin_jwt,
    );
    let (status, body) = oneshot(app, req).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body["delivery"], "live");

    let frame = rx.recv().await.expect("frame");
    let parsed: Value = serde_json::from_str(&frame).unwrap();
    assert_eq!(parsed["method"], "session/message");
    assert_eq!(parsed["params"]["content"], "hi");
}

#[tokio::test]
async fn send_message_queues_when_agent_disconnected() {
    let (state, auth) = fresh_state();
    let agent_id = Uuid::new_v4();
    // NB: no registry entry — agent is "offline".
    let admin_jwt = mint_admin(&auth);
    let app = router(state.clone());

    let req = bearer(
        json_post(
            &format!("/agents/{agent_id}/messages"),
            json!({ "role": "user", "content": "pending" }),
        ),
        &admin_jwt,
    );
    let (status, body) = oneshot(app, req).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body["delivery"], "queued");
    assert_eq!(state.message_queue.len(agent_id), 1);
}

#[tokio::test]
async fn send_message_returns_503_on_queue_overflow() {
    let (state, auth) = state_with_queue_capacity(1);
    let agent_id = Uuid::new_v4();
    let admin_jwt = mint_admin(&auth);
    let app = router(state);

    // First push buffers.
    let (status, _) = oneshot(
        app.clone(),
        bearer(
            json_post(
                &format!("/agents/{agent_id}/messages"),
                json!({ "role": "user", "content": "a" }),
            ),
            &admin_jwt,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    // Second push overflows.
    let (status, body) = oneshot(
        app,
        bearer(
            json_post(
                &format!("/agents/{agent_id}/messages"),
                json!({ "role": "user", "content": "b" }),
            ),
            &admin_jwt,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"], "service_unavailable");
}

#[tokio::test]
async fn send_message_agent_token_scoped_to_own_id() {
    let (state, auth) = fresh_state();
    let agent_id = Uuid::new_v4();
    let _rx = register_fake(&state, agent_id);
    let app = router(state);

    // Own token works.
    let own_jwt = mint_agent_for(&auth, agent_id);
    let (status, _) = oneshot(
        app.clone(),
        bearer(
            json_post(
                &format!("/agents/{agent_id}/messages"),
                json!({ "role": "user", "content": "ok" }),
            ),
            &own_jwt,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    // Different agent's token is rejected.
    let other_jwt = mint_agent_for(&auth, Uuid::new_v4());
    let (status, _) = oneshot(
        app,
        bearer(
            json_post(
                &format!("/agents/{agent_id}/messages"),
                json!({ "role": "user", "content": "nope" }),
            ),
            &other_jwt,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

// ══════════════════════════════════════════════════════════════
//  POST /agents/{id}/cancel
// ══════════════════════════════════════════════════════════════

#[tokio::test]
async fn cancel_when_disconnected_returns_202() {
    let (state, auth) = fresh_state();
    let admin_jwt = mint_admin(&auth);
    let app = router(state);
    let (status, _) = oneshot(
        app,
        bearer(
            json_post(&format!("/agents/{}/cancel", Uuid::new_v4()), json!({})),
            &admin_jwt,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
}

#[tokio::test]
async fn cancel_agent_token_scoped_to_own_id() {
    let (state, auth) = fresh_state();
    let agent_id = Uuid::new_v4();
    let _rx = register_fake(&state, agent_id);
    let app = router(state);

    // Own token → 202.
    let own_jwt = mint_agent_for(&auth, agent_id);
    let (status, _) = oneshot(
        app.clone(),
        bearer(
            json_post(&format!("/agents/{agent_id}/cancel"), json!({})),
            &own_jwt,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    // Different-agent token → 403.
    let other_jwt = mint_agent_for(&auth, Uuid::new_v4());
    let (status, _) = oneshot(
        app,
        bearer(
            json_post(&format!("/agents/{agent_id}/cancel"), json!({})),
            &other_jwt,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn cancel_when_connected_sends_session_cancel() {
    let (state, auth) = fresh_state();
    let agent_id = Uuid::new_v4();
    let mut rx = register_fake(&state, agent_id);
    let admin_jwt = mint_admin(&auth);
    let app = router(state);
    let (status, _) = oneshot(
        app,
        bearer(
            json_post(&format!("/agents/{agent_id}/cancel"), json!({})),
            &admin_jwt,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let frame = rx.recv().await.unwrap();
    let parsed: Value = serde_json::from_str(&frame).unwrap();
    assert_eq!(parsed["method"], "session/cancel");
}

// ══════════════════════════════════════════════════════════════
//  GET /agents/{id}/stream — auth scoping
// ══════════════════════════════════════════════════════════════

#[tokio::test]
async fn stream_is_reachable_by_admin_and_scoped_agent() {
    let (state, auth) = fresh_state();
    let agent_id = Uuid::new_v4();
    let app = router(state);

    // Admin token — any agent_id is fine.
    let admin_jwt = mint_admin(&auth);
    let res = app
        .clone()
        .oneshot(bearer(
            empty_get(&format!("/agents/{agent_id}/stream")),
            &admin_jwt,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // Agent token with matching sub.
    let own_jwt = mint_agent_for(&auth, agent_id);
    let res = app
        .clone()
        .oneshot(bearer(
            empty_get(&format!("/agents/{agent_id}/stream")),
            &own_jwt,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // Agent token with mismatched sub.
    let other_jwt = mint_agent_for(&auth, Uuid::new_v4());
    let res = app
        .oneshot(bearer(
            empty_get(&format!("/agents/{agent_id}/stream")),
            &other_jwt,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}

// ══════════════════════════════════════════════════════════════
//  Auth edge cases
// ══════════════════════════════════════════════════════════════

#[tokio::test]
async fn malformed_bearer_is_401() {
    let (state, _) = fresh_state();
    let app = router(state);
    let req = Request::builder()
        .uri("/agents")
        .header("authorization", "Bearer not-a-jwt")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn missing_bearer_is_401() {
    let (state, _) = fresh_state();
    let app = router(state);
    let res = app
        .oneshot(empty_get("/agents"))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}
