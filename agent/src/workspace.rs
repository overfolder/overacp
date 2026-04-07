//! Pluggable workspace synchronization.
//!
//! Real synchronizer implementations (GCS, rclone, S3, ...) belong
//! in separate crates so this crate doesn't grow per-backend
//! dependencies. The trait lives here so the supervisor can call
//! into any of them through a single uniform interface.

use anyhow::Result;

/// A workspace synchronizer.
///
/// Implementations move files between the session workspace and an
/// external store (cloud bucket, NFS, ...). The supervisor calls
/// `pull` once before spawning the child agent and `push` once after
/// the bridge exits.
#[allow(async_fn_in_trait)]
pub trait WorkspaceSync {
    /// Hydrate the workspace from the external store.
    async fn pull(&self) -> Result<()>;
    /// Persist the workspace back to the external store.
    async fn push(&self) -> Result<()>;
}

/// Default no-op implementation. Used when no external sync is
/// configured (e.g. local development against a bind-mounted
/// workspace).
pub struct NoopSync;

impl WorkspaceSync for NoopSync {
    async fn pull(&self) -> Result<()> {
        Ok(())
    }
    async fn push(&self) -> Result<()> {
        Ok(())
    }
}
