//! Handler-facing error type. Mapped to HTTP status codes by
//! the axum [`IntoResponse`] impl.

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use thiserror::Error;

use overacp_compute_core::ProviderError;

use crate::api::dto::ValidationResult;
use crate::api::pool_config::PoolConfigParseError;
use crate::store::StoreError;

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("unauthorized: {0}")]
    Unauthorized(String),
    /// Back-pressure: a downstream resource is temporarily
    /// unavailable. Mapped to HTTP 503.
    #[error("service unavailable: {0}")]
    ServiceUnavailable(String),
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
    #[error(transparent)]
    Provider(#[from] ProviderError),
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
            Self::Unauthorized(msg) => {
                let mut resp = (
                    StatusCode::UNAUTHORIZED,
                    Json(json!({ "error": "unauthorized", "message": msg })),
                )
                    .into_response();
                resp.headers_mut().insert(
                    header::WWW_AUTHENTICATE,
                    HeaderValue::from_static("Basic realm=\"overacp\""),
                );
                resp
            }
            Self::ServiceUnavailable(msg) => (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "error": "service_unavailable", "message": msg })),
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
            Self::Store(StoreError::NotFound) => {
                (StatusCode::NOT_FOUND, Json(json!({ "error": "not_found" }))).into_response()
            }
            Self::Store(StoreError::Conflict { what }) => (
                StatusCode::CONFLICT,
                Json(json!({ "error": "conflict", "message": what })),
            )
                .into_response(),
            Self::Provider(ProviderError::NotFound(id)) => (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "not_found", "message": format!("node '{id}'") })),
            )
                .into_response(),
            Self::Provider(ProviderError::Timeout) => (
                StatusCode::GATEWAY_TIMEOUT,
                Json(json!({ "error": "timeout" })),
            )
                .into_response(),
            Self::Provider(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "provider", "message": e.to_string() })),
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

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    #[tokio::test]
    async fn unauthorized_sets_status_and_www_authenticate() {
        let resp = ApiError::Unauthorized("nope".into()).into_response();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            resp.headers().get(header::WWW_AUTHENTICATE).unwrap(),
            "Basic realm=\"overacp\""
        );
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"], "unauthorized");
        assert_eq!(v["message"], "nope");
    }
}
