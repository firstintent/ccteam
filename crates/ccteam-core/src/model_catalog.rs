//! Advisory vendor model catalog cache.
//!
//! The authority lives in `ccteam-harness` because the adapters that capture
//! the vendor handshakes are below `ccteam-core` in the dependency graph.

pub use ccteam_harness::model_catalog::*;
