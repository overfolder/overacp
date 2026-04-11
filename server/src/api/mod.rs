//! REST surface for the broker.
//!
//! Two sets of endpoints live under this module during the ongoing
//! refactor:
//!
//! - **Broker-shaped** (Phase 4b): [`agents`] and [`tokens`]. The
//!   new `POST /tokens`, `GET /agents`, `GET /agents/{id}`,
//!   `DELETE /agents/{id}`, `POST /agents/{id}/messages`,
//!   `GET /agents/{id}/stream`, and `POST /agents/{id}/cancel`
//!   routes defined in `SPEC.md`.
//! - **Legacy controlplane** (awaiting removal in Phase 5):
//!   [`pool_config`], [`providers`], [`dto`], [`nodes`],
//!   [`routes`]. The `/compute/*` REST surface from the superseded
//!   controlplane architecture. Still mounted behind HTTP Basic
//!   auth so existing integrations keep working until Phase 5.
//!
//! [`error`] is shared: `ApiError` is the single handler-facing
//! error type for everything in this module.

pub mod agents;
pub mod dto;
pub mod error;
pub mod nodes;
pub mod pool_config;
pub mod providers;
pub mod routes;
pub mod tokens;

pub use tokens::router as tokens_router;
pub use dto::{
    CreatePoolRequest, PoolConfigBody, PoolStatusResponse, PoolSummary, PoolView, ProviderInfo,
    ProvidersList, ValidationFieldError, ValidationResult,
};
pub use error::ApiError;
pub use nodes::router as compute_nodes_router;
pub use pool_config::{is_secret_ref, PoolConfig, PoolConfigParseError, PROVIDER_CLASS_KEY};
pub use providers::{
    default_registry, LocalProcessPlugin, MorphPlugin, ProviderPlugin, ProviderRegistry,
};
pub use routes::router as compute_router;
