//! REST surface for the broker.
//!
//! - [`agents`] — the `/agents/...` routes (messages, stream,
//!   cancel, describe, list, disconnect).
//! - [`tokens`] — `POST /tokens` admin-only agent JWT minting.
//! - [`error`] — shared `ApiError` type and its `IntoResponse`
//!   mapping.

pub mod agents;
pub mod error;
pub mod tokens;

pub use error::ApiError;
pub use tokens::router as tokens_router;
