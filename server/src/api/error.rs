//! Handler-facing error type. Mapped to HTTP status codes by
//! the axum [`IntoResponse`] impl.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use thiserror::Error;

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
    /// An internal server error the client cannot fix by
    /// retrying differently (e.g. a crypto/mint failure). Mapped
    /// to HTTP 500.
    #[error("internal: {0}")]
    Internal(String),
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
            Self::Unauthorized(msg) => (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "error": "unauthorized", "message": msg })),
            )
                .into_response(),
            Self::ServiceUnavailable(msg) => (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "error": "service_unavailable", "message": msg })),
            )
                .into_response(),
            Self::Internal(msg) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "internal", "message": msg })),
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
    async fn not_found_maps_to_404() {
        let resp = ApiError::NotFound("x".into()).into_response();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"], "not_found");
    }

    #[tokio::test]
    async fn bad_request_maps_to_400() {
        let resp = ApiError::BadRequest("x".into()).into_response();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn unauthorized_maps_to_401() {
        let resp = ApiError::Unauthorized("nope".into()).into_response();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"], "unauthorized");
        assert_eq!(v["message"], "nope");
    }

    #[tokio::test]
    async fn service_unavailable_maps_to_503() {
        let resp = ApiError::ServiceUnavailable("queue full".into()).into_response();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"], "service_unavailable");
        assert_eq!(v["message"], "queue full");
    }

    #[tokio::test]
    async fn internal_maps_to_500() {
        let resp = ApiError::Internal("mint failed".into()).into_response();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"], "internal");
        assert_eq!(v["message"], "mint failed");
    }
}
