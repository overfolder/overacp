//! Operator-supplied hooks that the broker delegates to.
//!
//! The broker is a stateless router. Everything that requires
//! product-specific knowledge — what conversation to bootstrap with,
//! what tools are available, whether a request is within quota — is
//! decided by an operator-supplied trait impl. This module defines
//! the four hook contracts:
//!
//! - [`BootProvider`] — hands the agent its system prompt + history
//!   on `initialize`.
//! - [`ToolHost`]     — backs `tools/list` / `tools/call`.
//! - [`QuotaPolicy`]  — backs `quota/check` / `quota/update`.
//! - `Authenticator`  — lives in [`crate::auth`] (it predates the
//!   hooks module and the JWT-validation surface fits there
//!   naturally).
//!
//! Each hook ships with a stub default implementation, all named
//! `Default*` for consistency. They let the reference server boot
//! and the end-to-end demo work without an operator stack:
//!
//! - [`DefaultBootProvider`] returns an empty bootstrap.
//! - [`DefaultToolHost`] reports no tools.
//! - [`DefaultQuotaPolicy`] permits everything.

pub mod boot;
pub mod quota;
pub mod tools;

pub use boot::{BootError, BootProvider, DefaultBootProvider};
pub use quota::{DefaultQuotaPolicy, QuotaError, QuotaPolicy};
pub use tools::{DefaultToolHost, ToolError, ToolHost};
