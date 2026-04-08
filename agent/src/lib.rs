//! `overacp-agent` — supervisor for a child agent process speaking
//! over/ACP on stdio.
//!
//! 0.4 milestone: this crate currently exposes only the boot
//! contract (env-driven configuration) defined in
//! `docs/design/protocol.md` § 2.4. The WebSocket supervisor and
//! stdio bridge will land in subsequent commits.

pub mod config;

pub use config::{BootConfig, ConfigError};
