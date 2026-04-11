//! `QuotaPolicy` hook — backs `quota/check` and `quota/update`.
//!
//! See `SPEC.md` § "The four hooks". The broker has no opinion on
//! tier, plan, or pricing; everything is decided by the operator.

use async_trait::async_trait;
use serde_json::Value;
use thiserror::Error;

use crate::auth::Claims;

/// Errors a `QuotaPolicy` can return.
#[derive(Debug, Error)]
pub enum QuotaError {
    /// The operator's quota layer hit an internal error.
    #[error("quota policy failed: {0}")]
    Internal(String),
}

/// Operator hook for `quota/check` and `quota/update`.
///
/// `check` is called once per turn before the agent does any
/// LLM/tool work. `record` is called after the turn completes with
/// whatever usage data the agent reports (token counts, billable
/// units, …). The `usage` payload is opaque to the broker.
#[async_trait]
pub trait QuotaPolicy: Send + Sync + 'static {
    /// Whether the caller is allowed to start a new turn.
    async fn check(&self, claims: &Claims) -> Result<bool, QuotaError>;

    /// Record usage from a completed turn.
    async fn record(&self, claims: &Claims, usage: Value) -> Result<(), QuotaError>;
}

/// Default `QuotaPolicy` for the reference server. Allows everything,
/// records nothing. The right pick for end-to-end demos and any
/// deployment that doesn't bill at all.
#[derive(Debug, Default, Clone, Copy)]
pub struct DefaultQuotaPolicy;

#[async_trait]
impl QuotaPolicy for DefaultQuotaPolicy {
    async fn check(&self, _claims: &Claims) -> Result<bool, QuotaError> {
        Ok(true)
    }

    async fn record(&self, _claims: &Claims, _usage: Value) -> Result<(), QuotaError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use uuid::Uuid;

    fn claims() -> Claims {
        Claims::agent(Uuid::new_v4(), None, 60, "test")
    }

    #[tokio::test]
    async fn check_always_allows() {
        let q = DefaultQuotaPolicy;
        assert!(q.check(&claims()).await.unwrap());
    }

    #[tokio::test]
    async fn record_is_a_noop() {
        let q = DefaultQuotaPolicy;
        q.record(&claims(), json!({ "input_tokens": 100 }))
            .await
            .unwrap();
    }
}
