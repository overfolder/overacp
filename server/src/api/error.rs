//! Handler-facing error type. Mapped to HTTP status codes by
//! the axum [`IntoResponse`] impl.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use thiserror::Error;

use crate::api::dto::ValidationResult;
use crate::api::pool_config::PoolConfigParseError;
use crate::store::StoreError;

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("bad request: {0}")]
    BadRequest(String),
    /// Body parsed as JSON but did not match the flat-string-map
    /// shape required by `PoolConfig`.
    #[error("malformed pool config: {0}")]
    MalformedConfig(#[from] PoolConfigParseError),
    /// Body parsed as a `PoolConfig` but failed the provider's
    /// validate hook. Returns a structured `ValidationResult`.
    #[error("invalid config")]
    InvalidConfig(ValidationResult),
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            Self::NotFound(msg) => (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "not_found", "message": msg })),
            )
                .into_response(),
            Self::BadRequest(msg) => (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "bad_request", "message": msg })),
            )
                .into_response(),
            Self::MalformedConfig(e) => (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "bad_request", "message": e.to_string() })),
            )
                .into_response(),
            Self::InvalidConfig(result) => {
                (StatusCode::UNPROCESSABLE_ENTITY, Json(result)).into_response()
            }
            Self::Store(StoreError::NotFound) => (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "not_found" })),
            )
                .into_response(),
            Self::Store(StoreError::Conflict { what }) => (
                StatusCode::CONFLICT,
                Json(json!({ "error": "conflict", "message": what })),
            )
                .into_response(),
            Self::Store(StoreError::Backend(e)) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "backend", "message": e.to_string() })),
            )
                .into_response(),
        }
    }
}
