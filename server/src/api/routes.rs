//! axum routes for `/api/v1/compute/{providers,pools}`.
//!
//! Handlers parse the wire body into typed values up front
//! ([`PoolConfig`]) and from then on operate on those types — no
//! re-validation, no `Value` shape checks scattered through the
//! pipeline. Pool config is persisted via `SessionStore` as a
//! `serde_json::Value`; on the way out we echo it verbatim, which
//! preserves `${...}` secret references for GitOps round-trips
//! (design doc § 3.5).

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use serde_json::Value;

use crate::api::dto::{
    CreatePoolRequest, PoolConfigBody, PoolStatusResponse, PoolSummary, PoolView, ProviderInfo,
    ProvidersList, ValidationResult,
};
use crate::api::error::ApiError;
use crate::api::pool_config::PoolConfig;
use crate::api::providers::ProviderRegistry;
use crate::state::AppState;
use crate::store::{ComputePool, PoolStatus};

/// Mount the controlplane REST routes onto an axum router.
/// Designed to be `.merge`d into the binary's outer router so
/// `/healthz` and friends keep working.
pub fn router() -> Router<AppState> {
    Router::new()
        // § 3.1 — providers
        .route("/api/v1/compute/providers", get(list_providers))
        .route(
            "/api/v1/compute/providers/:provider_type",
            get(get_provider),
        )
        .route(
            "/api/v1/compute/providers/:provider_type/config/validate",
            post(validate_provider_config),
        )
        // § 3.2 — pools
        .route("/api/v1/compute/pools", get(list_pools).post(create_pool))
        .route(
            "/api/v1/compute/pools/:name",
            get(get_pool).delete(delete_pool),
        )
        .route(
            "/api/v1/compute/pools/:name/config",
            get(get_pool_config).put(replace_pool_config),
        )
        .route("/api/v1/compute/pools/:name/status", get(get_pool_status))
        .route("/api/v1/compute/pools/:name/pause", post(pause_pool))
        .route("/api/v1/compute/pools/:name/resume", post(resume_pool))
}

// ── § 3.1 — providers ───────────────────────────────────────────

async fn list_providers(State(s): State<AppState>) -> Json<ProvidersList> {
    Json(ProvidersList {
        providers: s.providers.list(),
    })
}

async fn get_provider(
    State(s): State<AppState>,
    Path(provider_type): Path<String>,
) -> Result<Json<ProviderInfo>, ApiError> {
    s.providers
        .get(&provider_type)
        .map(|p| Json(p.info()))
        .ok_or_else(|| ApiError::NotFound(format!("provider type '{provider_type}'")))
}

async fn validate_provider_config(
    State(s): State<AppState>,
    Path(provider_type): Path<String>,
    Json(body): Json<Value>,
) -> Result<Json<ValidationResult>, ApiError> {
    let plugin = s
        .providers
        .get(&provider_type)
        .ok_or_else(|| ApiError::NotFound(format!("provider type '{provider_type}'")))?;
    let raw_config = extract_config_field(&body)?;
    // Parse-don't-validate: shape errors and field errors are
    // distinct outcomes. A shape failure is a 400 (the body
    // wasn't a flat string map at all); a validate failure is a
    // structured 200 result with `valid: false` (Kafka Connect
    // semantics — `validate` is a query, not a mutation).
    let config = match PoolConfig::parse(raw_config) {
        Ok(c) => c,
        Err(e) => return Err(ApiError::MalformedConfig(e)),
    };
    if config.provider_class() != provider_type {
        return Err(ApiError::BadRequest(format!(
            "config 'provider.class' ({}) does not match URL provider type ({provider_type})",
            config.provider_class()
        )));
    }
    let errors = plugin.validate(&config);
    Ok(Json(ValidationResult {
        provider_type,
        valid: errors.is_empty(),
        errors,
    }))
}

// ── § 3.2 — pools ───────────────────────────────────────────────

async fn list_pools(State(s): State<AppState>) -> Result<Json<Vec<PoolSummary>>, ApiError> {
    let pools = s.store.list_pools().await?;
    Ok(Json(
        pools
            .into_iter()
            .map(|p| PoolSummary {
                name: p.name,
                provider_type: p.provider_type,
                status: p.status,
            })
            .collect(),
    ))
}

async fn create_pool(
    State(s): State<AppState>,
    Json(req): Json<CreatePoolRequest>,
) -> Result<(StatusCode, Json<PoolView>), ApiError> {
    if req.name.is_empty() {
        return Err(ApiError::BadRequest("'name' must not be empty".into()));
    }
    let config = PoolConfig::parse(&req.config)?;
    let provider_type = config.provider_class().to_string();
    accept_or_reject(&s.providers, &provider_type, &config)?;

    let now = Utc::now();
    let pool = ComputePool {
        name: req.name,
        provider_type: provider_type.clone(),
        config_json: config.to_json_value(),
        status: PoolStatus::Active,
        created_at: now,
        updated_at: now,
    };
    s.store.create_pool(pool.clone()).await?;
    Ok((StatusCode::CREATED, Json(view_of(pool))))
}

async fn get_pool(
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<PoolView>, ApiError> {
    let pool = require_pool(&s, &name).await?;
    Ok(Json(view_of(pool)))
}

async fn delete_pool(
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> Result<StatusCode, ApiError> {
    s.store.delete_pool(&name).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn get_pool_config(
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<PoolConfigBody>, ApiError> {
    let pool = require_pool(&s, &name).await?;
    // Echo verbatim — secret refs stay in their `${...}` form.
    Ok(Json(PoolConfigBody {
        config: pool.config_json,
    }))
}

async fn replace_pool_config(
    State(s): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<PoolConfigBody>,
) -> Result<Json<PoolView>, ApiError> {
    let config = PoolConfig::parse(&body.config)?;
    // The config carries the provider class; we don't allow
    // switching a pool to a different provider via PUT — that's
    // a delete + create.
    let existing = require_pool(&s, &name).await?;
    if config.provider_class() != existing.provider_type {
        return Err(ApiError::BadRequest(format!(
            "cannot change pool 'provider.class' from '{}' to '{}'; delete and recreate",
            existing.provider_type,
            config.provider_class()
        )));
    }
    accept_or_reject(&s.providers, &existing.provider_type, &config)?;

    s.store
        .update_pool_config(&name, config.to_json_value())
        .await?;
    let updated = require_pool(&s, &name).await?;
    Ok(Json(view_of(updated)))
}

async fn get_pool_status(
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<PoolStatusResponse>, ApiError> {
    let pool = require_pool(&s, &name).await?;
    Ok(Json(PoolStatusResponse {
        name: pool.name,
        provider_type: pool.provider_type,
        state: pool.status,
    }))
}

async fn pause_pool(
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<PoolView>, ApiError> {
    s.store.set_pool_status(&name, PoolStatus::Paused).await?;
    Ok(Json(view_of(require_pool(&s, &name).await?)))
}

async fn resume_pool(
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<PoolView>, ApiError> {
    s.store.set_pool_status(&name, PoolStatus::Active).await?;
    Ok(Json(view_of(require_pool(&s, &name).await?)))
}

// ── helpers ──────────────────────────────────────────────────────

fn view_of(p: ComputePool) -> PoolView {
    PoolView {
        name: p.name,
        provider_type: p.provider_type,
        config: p.config_json,
        status: p.status,
        created_at: p.created_at,
        updated_at: p.updated_at,
    }
}

async fn require_pool(state: &AppState, name: &str) -> Result<ComputePool, ApiError> {
    state
        .store
        .get_pool(name)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("pool '{name}'")))
}

/// Look up the plugin and run its validate hook, mapping a
/// non-empty error list into [`ApiError::InvalidConfig`].
fn accept_or_reject(
    registry: &Arc<ProviderRegistry>,
    provider_type: &str,
    config: &PoolConfig,
) -> Result<(), ApiError> {
    let plugin = registry
        .get(provider_type)
        .ok_or_else(|| ApiError::BadRequest(format!("unknown provider.class '{provider_type}'")))?;
    let errors = plugin.validate(config);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(ApiError::InvalidConfig(ValidationResult {
            provider_type: provider_type.to_string(),
            valid: false,
            errors,
        }))
    }
}

/// Accept either `{"config": {...}}` or a bare `{...}` body for
/// the `validate` endpoint, mirroring Kafka Connect's tolerance.
fn extract_config_field(body: &Value) -> Result<&Value, ApiError> {
    match body {
        Value::Object(map) => match map.get("config") {
            Some(inner) => Ok(inner),
            None => Ok(body),
        },
        _ => Err(ApiError::BadRequest("expected a JSON object body".into())),
    }
}
