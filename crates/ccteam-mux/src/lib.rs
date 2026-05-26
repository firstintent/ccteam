//! ccteam-mux — unified mux abstraction for mode 1 / 2 / 3a / 3b child supervision.
//!
//! V0.8 W0 spike: this crate is a placeholder. The real `MuxBackend`
//! trait + `InProcBackend` / `TmuxBackend` / `RmuxBackend` impls land
//! in W1 per `docs/research/embedded-mux-unified-architecture.md` §四.
//!
//! For now the crate exists to:
//! 1. pin `rmux-sdk` / `rmux-client` / `rmux-server` / `rmux-proto` 0.3
//!    in the workspace dep graph so any rmux semver drift surfaces at
//!    workspace `cargo build` time (the `rmux_types_compile_link` test
//!    in `tests/smoke_rmux_sdk.rs` is the canary)
//! 2. carry the `#[ignore]` end-to-end smoke test that pairs ccteam
//!    against a real `rmux` daemon (`cargo test -p ccteam-mux
//!    smoke_rmux_sdk -- --ignored --nocapture`)
//!
//! The W0 spike intentionally avoids adding the `MuxBackend` trait
//! here — adding the trait without a real impl behind it would force
//! ccteam-core / ccteam-cli callers to convert in the same wave, which
//! is W1 scope per the design doc wave plan.

/// Spike-only placeholder. Removed in W1 when the `MuxBackend` trait
/// lands and this crate exports something real.
pub fn placeholder() {}
