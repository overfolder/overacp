//! Bundled `ComputeProvider` implementations.
//!
//! Each submodule is one provider; they all live in this crate so the
//! controlplane only depends on `overacp-compute-core` and picks
//! providers via cargo features (future) or direct construction.

pub mod local;

pub use local::{LocalProvider, PROVIDER_TYPE as LOCAL_PROVIDER_TYPE};
