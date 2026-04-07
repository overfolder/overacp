//! REST surface for the controlplane.
//!
//! Implements `docs/design/controlplane.md` § 3.1 (compute
//! providers) and § 3.2 (compute pools). Nodes (§ 3.3), agents
//! (§ 3.4), and the SSE/cancel surfaces (§ 3.5) land in
//! follow-ups.
//!
//! Module layout:
//! - [`pool_config`] — typed pool config (parse-don't-validate).
//! - [`providers`]   — server-side `ProviderPlugin` shim around
//!   `compute::ComputeProvider` plus a registry.
//! - [`dto`]         — wire types for the REST surface.
//! - [`error`]       — `ApiError` + axum `IntoResponse` mapping.
//! - [`routes`]      — handlers and the `Router` constructor.

pub mod dto;
pub mod error;
pub mod pool_config;
pub mod providers;
pub mod routes;

pub use dto::{
    CreatePoolRequest, PoolConfigBody, PoolStatusResponse, PoolSummary, PoolView, ProviderInfo,
    ProvidersList, ValidationFieldError, ValidationResult,
};
pub use error::ApiError;
pub use pool_config::{is_secret_ref, PoolConfig, PoolConfigParseError, PROVIDER_CLASS_KEY};
pub use providers::{
    default_registry, LocalProcessPlugin, MorphPlugin, ProviderPlugin, ProviderRegistry,
};
pub use routes::router as compute_router;
