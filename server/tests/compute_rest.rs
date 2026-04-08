//! End-to-end JSON round-trip tests for `/compute/...`.
//! Drives the axum router via `tower::ServiceExt::oneshot` so the
//! tests don't need a real socket.

use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use overacp_server::api::{
    default_registry, PoolStatusResponse, PoolSummary, PoolView, ProviderInfo, ProvidersList,
    ValidationResult,
};
use overacp_server::{compute_router, AppState, InMemoryStore, PoolStatus, StaticJwtAuthenticator};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use tower::ServiceExt;

const FIXTURE_CREATE: &str = include_str!("fixtures/morph_pool_create.json");
const FIXTURE_VALIDATE: &str = include_str!("fixtures/morph_validate.json");

fn app() -> Router {
    let state = AppState::new(
        Arc::new(InMemoryStore::new()),
        Arc::new(default_registry()),
        Arc::new(StaticJwtAuthenticator::new("test-key", "overacp")),
    );
    compute_router().with_state(state)
}

async fn send(app: &Router, method: &str, uri: &str, body: Option<&str>) -> (StatusCode, Bytes) {
    let mut req = Request::builder().method(method).uri(uri);
    if body.is_some() {
        req = req.header("content-type", "application/json");
    }
    let req = req
        .body(Body::from(body.map(str::to_owned).unwrap_or_default()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, bytes)
}

fn parse<T: DeserializeOwned>(b: &Bytes) -> T {
    serde_json::from_slice(b)
        .unwrap_or_else(|e| panic!("decode failed: {e}\nbody: {}", String::from_utf8_lossy(b)))
}

// ── § 3.1 — providers ────────────────────────────────────────────

#[tokio::test]
async fn lists_compiled_in_providers() {
    let app = app();
    let (status, body) = send(&app, "GET", "/compute/providers", None).await;
    assert_eq!(status, StatusCode::OK);
    let list: ProvidersList = parse(&body);
    let names: Vec<_> = list
        .providers
        .iter()
        .map(|p| p.provider_type.as_str())
        .collect();
    assert!(names.contains(&"morph"));
    assert!(names.contains(&"local-process"));
}

#[tokio::test]
async fn describes_a_single_provider_and_404s_for_unknown() {
    let app = app();
    let (status, body) = send(&app, "GET", "/compute/providers/morph", None).await;
    assert_eq!(status, StatusCode::OK);
    let info: ProviderInfo = parse(&body);
    assert_eq!(info.provider_type, "morph");
    assert!(!info.supports_multi_agent_nodes);
    assert!(info.supports_node_reuse);

    let (status, body) = send(&app, "GET", "/compute/providers/local-process", None).await;
    assert_eq!(status, StatusCode::OK);
    let local: ProviderInfo = parse(&body);
    assert!(!local.supports_multi_agent_nodes);
    assert!(!local.supports_node_reuse);

    let (status, _) = send(&app, "GET", "/compute/providers/nope", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn validate_endpoint_accepts_kafka_connect_style_body() {
    let app = app();
    let (status, body) = send(
        &app,
        "POST",
        "/compute/providers/morph/config/validate",
        Some(FIXTURE_VALIDATE),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let result: ValidationResult = parse(&body);
    assert_eq!(result.provider_type, "morph");
    assert!(
        result.valid,
        "expected valid, got errors: {:?}",
        result.errors
    );
}

#[tokio::test]
async fn validate_reports_missing_required_keys() {
    let app = app();
    let bad = json!({
        "config": { "provider.class": "morph" }
    })
    .to_string();
    let (status, body) = send(
        &app,
        "POST",
        "/compute/providers/morph/config/validate",
        Some(&bad),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let result: ValidationResult = parse(&body);
    assert!(!result.valid);
    let keys: Vec<_> = result.errors.iter().map(|e| e.key.as_str()).collect();
    assert!(keys.contains(&"morph.api_key"));
    assert!(keys.contains(&"default.image"));
}

#[tokio::test]
async fn validate_treats_secret_refs_as_present_for_numeric_keys() {
    let app = app();
    let body_in = json!({
        "config": {
            "provider.class": "morph",
            "morph.api_key": "${env:K}",
            "default.image": "img",
            "max_nodes": "${env:MAX_NODES}"
        }
    })
    .to_string();
    let (status, body) = send(
        &app,
        "POST",
        "/compute/providers/morph/config/validate",
        Some(&body_in),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let result: ValidationResult = parse(&body);
    assert!(
        result.valid,
        "secret-ref numerics should be opaque-but-valid: {:?}",
        result.errors
    );
}

#[tokio::test]
async fn validate_accepts_bare_object_body_without_config_wrapper() {
    // Kafka Connect's `validate` accepts a bare config object;
    // we mirror that. The fixture wraps in `{"config": ...}`,
    // this test exercises the unwrapped form explicitly.
    let app = app();
    let bare = json!({
        "provider.class": "morph",
        "morph.api_key": "${env:K}",
        "default.image": "img"
    })
    .to_string();
    let (status, body) = send(
        &app,
        "POST",
        "/compute/providers/morph/config/validate",
        Some(&bare),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let result: ValidationResult = parse(&body);
    assert!(result.valid, "errors: {:?}", result.errors);
}

#[tokio::test]
async fn validate_400s_when_body_isnt_a_flat_string_map() {
    let app = app();
    let bad = json!({ "config": { "provider.class": "morph", "n": 1 } }).to_string();
    let (status, _) = send(
        &app,
        "POST",
        "/compute/providers/morph/config/validate",
        Some(&bad),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

// ── § 3.2 — pools ────────────────────────────────────────────────

#[tokio::test]
async fn create_get_round_trips_secret_refs_unchanged() {
    let app = app();

    // Create.
    let (status, body) = send(&app, "POST", "/compute/pools", Some(FIXTURE_CREATE)).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "body: {}",
        String::from_utf8_lossy(&body)
    );
    let created: PoolView = parse(&body);
    assert_eq!(created.name, "morph-prod");
    assert_eq!(created.provider_type, "morph");
    assert_eq!(created.status, PoolStatus::Active);
    assert_eq!(
        created.config.get("morph.api_key").and_then(Value::as_str),
        Some("${env:MORPH_API_KEY}"),
        "POST response must echo the unresolved secret reference"
    );

    // GET full pool.
    let (status, body) = send(&app, "GET", "/compute/pools/morph-prod", None).await;
    assert_eq!(status, StatusCode::OK);
    let got: PoolView = parse(&body);
    assert_eq!(got.config, created.config);

    // GET .../config — same shape, secret refs preserved verbatim.
    let (status, body) = send(&app, "GET", "/compute/pools/morph-prod/config", None).await;
    assert_eq!(status, StatusCode::OK);
    let cfg: serde_json::Value = parse(&body);
    let echoed = cfg.get("config").unwrap();

    // The original fixture's `config` block must equal what we
    // get back. That's the GitOps round-trip guarantee from
    // § 3.5 of the design doc.
    let original: Value = serde_json::from_str(FIXTURE_CREATE).unwrap();
    let original_config = original.get("config").unwrap();
    assert_eq!(echoed, original_config);
}

#[tokio::test]
async fn list_summaries_include_provider_type() {
    let app = app();
    send(&app, "POST", "/compute/pools", Some(FIXTURE_CREATE)).await;

    let (status, body) = send(&app, "GET", "/compute/pools", None).await;
    assert_eq!(status, StatusCode::OK);
    let list: Vec<PoolSummary> = parse(&body);
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].name, "morph-prod");
    assert_eq!(list[0].provider_type, "morph");
}

#[tokio::test]
async fn duplicate_create_is_409() {
    let app = app();
    send(&app, "POST", "/compute/pools", Some(FIXTURE_CREATE)).await;
    let (status, _) = send(&app, "POST", "/compute/pools", Some(FIXTURE_CREATE)).await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn create_with_invalid_config_is_422_with_structured_errors() {
    let app = app();
    let body = json!({
        "name": "broken",
        "config": { "provider.class": "morph" }
    })
    .to_string();
    let (status, body) = send(&app, "POST", "/compute/pools", Some(&body)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let result: ValidationResult = parse(&body);
    assert!(!result.valid);
    assert!(!result.errors.is_empty());
}

#[tokio::test]
async fn create_with_malformed_config_is_400() {
    let app = app();
    let body = json!({
        "name": "broken",
        "config": { "provider.class": "morph", "n": 1 }
    })
    .to_string();
    let (status, _) = send(&app, "POST", "/compute/pools", Some(&body)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn put_config_replaces_and_round_trips() {
    let app = app();
    send(&app, "POST", "/compute/pools", Some(FIXTURE_CREATE)).await;

    let new_cfg = json!({
        "config": {
            "provider.class": "morph",
            "morph.api_key": "${env:OTHER_KEY}",
            "default.image": "ghcr.io/example:v2",
            "max_nodes": "10"
        }
    })
    .to_string();
    let (status, body) = send(
        &app,
        "PUT",
        "/compute/pools/morph-prod/config",
        Some(&new_cfg),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "body: {}",
        String::from_utf8_lossy(&body)
    );
    let updated: PoolView = parse(&body);
    assert_eq!(
        updated.config.get("morph.api_key").and_then(Value::as_str),
        Some("${env:OTHER_KEY}")
    );
    assert_eq!(
        updated.config.get("default.image").and_then(Value::as_str),
        Some("ghcr.io/example:v2")
    );
    assert!(updated.updated_at >= updated.created_at);
}

#[tokio::test]
async fn put_config_rejects_provider_class_change() {
    let app = app();
    send(&app, "POST", "/compute/pools", Some(FIXTURE_CREATE)).await;
    let body = json!({
        "config": { "provider.class": "local-process" }
    })
    .to_string();
    let (status, _) = send(&app, "PUT", "/compute/pools/morph-prod/config", Some(&body)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn pause_resume_updates_status() {
    let app = app();
    send(&app, "POST", "/compute/pools", Some(FIXTURE_CREATE)).await;

    let (status, body) = send(&app, "POST", "/compute/pools/morph-prod/pause", Some("{}")).await;
    assert_eq!(status, StatusCode::OK);
    let pool: PoolView = parse(&body);
    assert_eq!(pool.status, PoolStatus::Paused);

    let (status, body) = send(&app, "GET", "/compute/pools/morph-prod/status", None).await;
    assert_eq!(status, StatusCode::OK);
    let s: PoolStatusResponse = parse(&body);
    assert_eq!(s.state, PoolStatus::Paused);
    assert_eq!(s.provider_type, "morph");

    let (status, body) = send(&app, "POST", "/compute/pools/morph-prod/resume", Some("{}")).await;
    assert_eq!(status, StatusCode::OK);
    let pool: PoolView = parse(&body);
    assert_eq!(pool.status, PoolStatus::Active);
}

// ── § 3.2.1 — capability flags (multi_agent_nodes, node_reuse) ───

/// Helper: minimal valid morph create body with extra capability keys.
fn morph_create_with(extra: serde_json::Map<String, Value>) -> String {
    let mut config = serde_json::Map::new();
    config.insert("provider.class".into(), json!("morph"));
    config.insert("morph.api_key".into(), json!("${env:K}"));
    config.insert("default.image".into(), json!("img"));
    for (k, v) in extra {
        config.insert(k, v);
    }
    json!({ "name": "morph-prod", "config": config }).to_string()
}

fn local_create_with(extra: serde_json::Map<String, Value>) -> String {
    let mut config = serde_json::Map::new();
    config.insert("provider.class".into(), json!("local-process"));
    for (k, v) in extra {
        config.insert(k, v);
    }
    json!({ "name": "lp", "config": config }).to_string()
}

#[tokio::test]
async fn create_pool_defaults_capability_flags_to_provider_support() {
    // Neither key set → falls through to provider defaults, must succeed.
    let app = app();
    let body = morph_create_with(serde_json::Map::new());
    let (status, body) = send(&app, "POST", "/compute/pools", Some(&body)).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "body: {}",
        String::from_utf8_lossy(&body)
    );
}

#[tokio::test]
async fn create_pool_accepts_restricting_a_supported_flag_to_false() {
    // morph supports node_reuse=true; restricting to false is allowed.
    let app = app();
    let mut extra = serde_json::Map::new();
    extra.insert("node_reuse".into(), json!("false"));
    extra.insert("multi_agent_nodes".into(), json!("false"));
    let body = morph_create_with(extra);
    let (status, body) = send(&app, "POST", "/compute/pools", Some(&body)).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "body: {}",
        String::from_utf8_lossy(&body)
    );
}

#[tokio::test]
async fn create_pool_accepts_enabling_a_flag_the_provider_supports() {
    // morph supports node_reuse=true.
    let app = app();
    let mut extra = serde_json::Map::new();
    extra.insert("node_reuse".into(), json!("true"));
    let body = morph_create_with(extra);
    let (status, _) = send(&app, "POST", "/compute/pools", Some(&body)).await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn create_pool_rejects_enabling_multi_agent_nodes_when_unsupported() {
    // morph does NOT advertise multi_agent_nodes → 422.
    let app = app();
    let mut extra = serde_json::Map::new();
    extra.insert("multi_agent_nodes".into(), json!("true"));
    let body = morph_create_with(extra);
    let (status, body) = send(&app, "POST", "/compute/pools", Some(&body)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let result: ValidationResult = parse(&body);
    assert!(!result.valid);
    let offending: Vec<_> = result
        .errors
        .iter()
        .filter(|e| e.key == "multi_agent_nodes")
        .collect();
    assert_eq!(
        offending.len(),
        1,
        "expected one error keyed at multi_agent_nodes, got {:?}",
        result.errors
    );
}

#[tokio::test]
async fn create_pool_rejects_enabling_node_reuse_when_unsupported() {
    // local-process advertises neither flag → enabling node_reuse is 422.
    let app = app();
    let mut extra = serde_json::Map::new();
    extra.insert("node_reuse".into(), json!("true"));
    let body = local_create_with(extra);
    let (status, body) = send(&app, "POST", "/compute/pools", Some(&body)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let result: ValidationResult = parse(&body);
    assert!(result.errors.iter().any(|e| e.key == "node_reuse"));
}

#[tokio::test]
async fn create_pool_accepts_reserved_unenforced_keys() {
    // max_nodes and idle_ttl_s are reserved-but-unenforced in 0.4.
    let app = app();
    let mut extra = serde_json::Map::new();
    extra.insert("max_nodes".into(), json!("50"));
    extra.insert("idle_ttl_s".into(), json!("1800"));
    let body = morph_create_with(extra);
    let (status, _) = send(&app, "POST", "/compute/pools", Some(&body)).await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn delete_pool_removes_it() {
    let app = app();
    send(&app, "POST", "/compute/pools", Some(FIXTURE_CREATE)).await;

    let (status, _) = send(&app, "DELETE", "/compute/pools/morph-prod", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _) = send(&app, "GET", "/compute/pools/morph-prod", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
