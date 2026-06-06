//! v0.8.6 W5b ResDisk — `GET /api/v1/capabilities`.
//!
//! Reports the harness vendors ccteam can drive, each annotated with an
//! `available` flag from a PATH probe of the vendor binary
//! (`claude --version` / `codex --version`). The web SPA uses this to
//! grey out a "create session with vendor X" affordance when the binary
//! isn't installed.
//!
//! Shape:
//!
//! ```json
//! {
//!   "harnesses": [
//!     {"id": "claude-code", "vendor": "claude", "available": true,  "providers": []},
//!     {"id": "codex",       "vendor": "codex",  "available": false, "providers": []}
//!   ]
//! }
//! ```
//!
//! `providers` is reserved (per-vendor model/provider enumeration) and
//! ships as an empty array for now.
//!
//! The probe runs `<bin> --version` via `spawn_blocking` (it shells out)
//! and the boolean result is **cached for the daemon's lifetime** keyed
//! by binary path — installing a vendor binary after the daemon starts
//! requires a daemon restart to flip `available`, which is acceptable for
//! a single-user dev tool (and avoids re-spawning a child per request).
//! Auth: merged into [`super::stateful_router`], so the existing
//! `auth_layer` gate applies for free.

use std::collections::HashMap;
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use axum::{response::IntoResponse, routing::get, Json, Router};
use ccteam_harness::{CLAUDE_BIN_ENV, CODEX_BIN_ENV};
use serde::Serialize;

use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/api/v1/capabilities", get(handle_capabilities))
}

/// One harness entry in the capabilities response.
#[derive(Debug, Clone, Serialize)]
pub struct HarnessCapability {
    /// Stable harness id (`claude-code` / `codex`).
    pub id: &'static str,
    /// Vendor token matching [`ccteam_harness::AgentVendor`]'s lowercase
    /// serde form (`claude` / `codex`) — the same string
    /// `POST .../sessions` accepts.
    pub vendor: &'static str,
    /// Whether the vendor binary is on PATH (or its `CCTEAM_*_BIN`
    /// override resolves to an executable).
    pub available: bool,
    /// Reserved for per-vendor provider/model enumeration; empty for now.
    pub providers: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CapabilitiesResponse {
    pub harnesses: Vec<HarnessCapability>,
}

async fn handle_capabilities() -> impl IntoResponse {
    // Two cheap blocking probes; run them off the async runtime. Each is
    // cached, so the common case never spawns a child at all.
    let claude_bin = claude_bin();
    let codex_bin = codex_bin();
    let probe = tokio::task::spawn_blocking(move || {
        (probe_available(&claude_bin), probe_available(&codex_bin))
    })
    .await;
    let (claude_ok, codex_ok) = match probe {
        Ok(pair) => pair,
        Err(err) => {
            // spawn_blocking panicked/cancelled — degrade to "unavailable"
            // rather than 500; the SPA just shows both vendors disabled.
            tracing::warn!(?err, "capabilities probe worker failed");
            (false, false)
        }
    };
    Json(CapabilitiesResponse {
        harnesses: vec![
            HarnessCapability {
                id: "claude-code",
                vendor: "claude",
                available: claude_ok,
                providers: Vec::new(),
            },
            HarnessCapability {
                id: "codex",
                vendor: "codex",
                available: codex_ok,
                providers: Vec::new(),
            },
        ],
    })
}

/// Resolve the `claude` binary path, honoring `CCTEAM_CLAUDE_BIN` (same
/// override the harness adapters use), defaulting to `claude` on PATH.
fn claude_bin() -> String {
    std::env::var(CLAUDE_BIN_ENV).unwrap_or_else(|_| "claude".to_string())
}

/// Resolve the `codex` binary path, honoring `CCTEAM_CODEX_BIN`.
fn codex_bin() -> String {
    std::env::var(CODEX_BIN_ENV).unwrap_or_else(|_| "codex".to_string())
}

/// Process-lifetime cache of `<bin> --version` success, keyed by the
/// resolved binary path string. Keyed by path (not vendor) so a test
/// flipping `CCTEAM_*_BIN` to a fake script gets an independent cache
/// entry rather than colliding with the real-binary result.
fn probe_cache() -> &'static Mutex<HashMap<String, bool>> {
    static CACHE: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Best-effort "is this binary runnable" probe: `<bin> --version` exiting
/// 0. Cached for the process lifetime. Any spawn error (binary not on
/// PATH) or non-zero exit folds to `false`.
fn probe_available(bin: &str) -> bool {
    if let Ok(cache) = probe_cache().lock() {
        if let Some(&hit) = cache.get(bin) {
            return hit;
        }
    }
    let ok = Command::new(bin)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if let Ok(mut cache) = probe_cache().lock() {
        cache.insert(bin.to_string(), ok);
    }
    ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_available_false_for_missing_binary() {
        // A path that cannot exist → spawn error → false.
        assert!(!probe_available("/nonexistent/ccteam-fake-binary-zzz"));
    }

    #[test]
    fn probe_available_true_for_true_binary() {
        // `/bin/true --version` exits 0 on Linux (GNU coreutils accepts
        // and ignores it); use it as a stand-in for a runnable vendor.
        if std::path::Path::new("/bin/true").exists() {
            assert!(probe_available("/bin/true"));
        }
    }

    #[test]
    fn probe_cache_is_keyed_by_path() {
        // Two distinct paths get independent cache entries; a second call
        // on the same path returns the cached value without re-spawning
        // (we can only assert it stays consistent here).
        let a = probe_available("/nonexistent/aaa");
        let b = probe_available("/nonexistent/aaa");
        assert_eq!(a, b);
        assert!(!a);
    }
}
