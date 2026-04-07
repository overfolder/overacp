//! Pluggable workspace synchronization.
//!
//! 0.3 ships only `NoopSync`. Real implementations (GCS, rclone,
//! S3, ...) belong in separate crates per the SPEC.md "Open
//! questions" entry — the trait exists here so the supervisor can
//! call into them without growing per-backend dependencies.

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
