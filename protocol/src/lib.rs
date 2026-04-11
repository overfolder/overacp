//! over/ACP wire protocol — pure types, no I/O.
//!
//! This crate defines the JSON-RPC method names, the request /
//! response / notification payload types, and the JWT session claims
//! used to authenticate the WebSocket tunnel between the over/ACP
//! broker and the agent process.
//!
//! See [`docs/design/protocol.md`](../docs/design/protocol.md) for
//! the full design doc.
//!
//! The crate is deliberately I/O-free. It depends only on `serde`,
//! `serde_json`, `jsonwebtoken`, `uuid`, `chrono`, and `thiserror`.
//! Anything that needs tokio, axum, or sqlx belongs in the server or
//! agent crates.

pub mod error;
pub mod jwt;
pub mod messages;
pub mod methods;

pub use error::ProtocolError;
pub use jwt::{
    mint_token, peek_claims_unverified, validate_token, Claims, DEFAULT_TOKEN_TTL_SECS, ROLE_ADMIN,
    ROLE_AGENT,
};
pub use messages::{
    Activity, Content, Heartbeat, InitializeRequest, InitializeResponse, Message,
    QuotaCheckRequest, QuotaCheckResponse, QuotaUpdateRequest, QuotaUpdateResponse, Role,
    SessionCancelParams, SessionMessageParams, TextDelta, ToolCallNotification,
    ToolResultNotification, TurnEndParams, Usage,
};
