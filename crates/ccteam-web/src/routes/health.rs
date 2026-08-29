//! `GET /health` — liveness probe **and** daemon identity.
//!
//! Liveness was the original job (M5.0 verification scripts and the
//! integration-test subprocess harness poll it to know the server is up).
//! v0.10.5 adds the identity a client needs BEFORE it trusts this daemon:
//! a DSH plugin that finds a ccteam already running must decide "is this
//! *my* engine?" — same `$CCTEAM_HOME`, same version — and attach, rather
//! than start a second one against a different home.
//!
//! Body: `{status, version, build?, home, pid, web_bind, dsh_web_bind?,
//! uptime_secs}`. The endpoint stays OUTSIDE the auth layer: a client that
//! cannot yet authenticate still has to be able to ask "who are you?".
//! Nothing here is a secret — pid/home/binds are already visible to any
//! process under this uid, which is the same trust boundary the daemon
//! documents everywhere else (AGENTS.md §三).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use axum::{extract::State, routing::get, Json, Router};
use serde_json::{json, Value};

use ccteam_core::CcteamPaths;

/// The identity facts `/health` reports. Resolved once at daemon start
/// (the binds are only known after `TcpListener::bind`, so `serve` builds
/// the bound one and hands it to the router).
#[derive(Debug, Clone)]
pub struct HealthIdentity {
    /// Canonical `$CCTEAM_HOME` — the same rule as
    /// [`CcteamPaths::from_env`], symlinks resolved so a client comparing
    /// its own resolution against ours cannot differ over `~/.ccteam` vs
    /// `/home/u/.ccteam`.
    home: PathBuf,
    /// Git commit of the running binary, when the build recorded one.
    /// Only the binary crate's `build.rs` knows it, so it is plumbed in
    /// through `ServeOpts` rather than re-derived here.
    build: Option<String>,
    /// Address the web console is actually bound to (post-bind, so `:0`
    /// reports the assigned port). `None` only when the router was built
    /// without serving — the library/test path, never a live daemon.
    web_bind: Option<String>,
    /// Companion-port DSH web proxy bind; `None` when disabled.
    dsh_web_bind: Option<String>,
    started_at: Instant,
}

impl HealthIdentity {
    /// Identity for a router that is not (yet) serving: home + pid are
    /// real, the binds are unknown. Used by `AppState::new` so every
    /// router has a valid identity without inventing an address.
    pub fn unbound(paths: &CcteamPaths) -> Self {
        Self {
            home: canonical_home(&paths.root),
            build: None,
            web_bind: None,
            dsh_web_bind: None,
            started_at: Instant::now(),
        }
    }

    /// Identity for the served daemon: the actual bound addresses plus the
    /// binary's build id.
    pub fn bound(
        paths: &CcteamPaths,
        build: Option<String>,
        web_bind: String,
        dsh_web_bind: Option<String>,
    ) -> Self {
        Self {
            home: canonical_home(&paths.root),
            build,
            web_bind: Some(web_bind),
            dsh_web_bind,
            started_at: Instant::now(),
        }
    }

    /// The JSON body, so the shape has ONE home and the handler is a
    /// one-liner (and `ccteam daemon status --json` can be checked against
    /// the same field set).
    pub fn body(&self) -> Value {
        json!({
            "status": "ok",
            "version": env!("CARGO_PKG_VERSION"),
            "build": self.build,
            "home": self.home.display().to_string(),
            "pid": std::process::id(),
            "web_bind": self.web_bind,
            "dsh_web_bind": self.dsh_web_bind,
            "uptime_secs": self.started_at.elapsed().as_secs(),
        })
    }
}

/// Resolve `$CCTEAM_HOME` to an absolute, symlink-free path. Falls back to
/// the path as-given when it does not exist yet (a daemon booting into a
/// fresh home must still answer `/health`).
fn canonical_home(root: &Path) -> PathBuf {
    std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf())
}

/// Build the `GET /health` router.
pub fn router(identity: Arc<HealthIdentity>) -> Router {
    Router::new()
        .route("/health", get(handle_health))
        .with_state(identity)
}

async fn handle_health(State(identity): State<Arc<HealthIdentity>>) -> Json<Value> {
    Json(identity.body())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(root: PathBuf) -> CcteamPaths {
        CcteamPaths {
            projects_root: root.join("projects"),
            root,
        }
    }

    #[tokio::test]
    async fn handle_health_body_shape() {
        // Direct handler call — the in-process axum + reqwest path is
        // exercised by `crate::tests::serve_health_endpoint_returns_ok_json`
        // (lib.rs); here we just assert the body shape so a churn in
        // the JSON contract is loud.
        let tmp = tempfile::TempDir::new().unwrap();
        let identity = Arc::new(HealthIdentity::unbound(&paths(tmp.path().to_path_buf())));
        let Json(body) = handle_health(State(identity)).await;
        assert_eq!(body["status"], "ok");
        assert!(body["version"].as_str().is_some_and(|s| !s.is_empty()));
    }

    #[test]
    fn bound_identity_reports_home_pid_binds_and_uptime() {
        // The plugin's attach decision reads all of these; a missing one
        // is the difference between "attach" and "start a second daemon".
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let identity = HealthIdentity::bound(
            &paths(home.clone()),
            Some("abc1234".to_string()),
            "127.0.0.1:7331".to_string(),
            Some("127.0.0.1:7332".to_string()),
        );
        let body = identity.body();
        assert_eq!(body["status"], "ok");
        assert_eq!(body["build"], "abc1234");
        assert_eq!(
            body["home"].as_str().unwrap(),
            std::fs::canonicalize(&home).unwrap().display().to_string(),
            "home must be the canonical CCTEAM_HOME so a client's own \
             resolution compares equal"
        );
        assert_eq!(body["pid"].as_u64().unwrap(), std::process::id() as u64);
        assert_eq!(body["web_bind"], "127.0.0.1:7331");
        assert_eq!(body["dsh_web_bind"], "127.0.0.1:7332");
        assert!(body["uptime_secs"].is_u64());
        assert!(body["version"].as_str().is_some_and(|s| !s.is_empty()));
    }

    #[test]
    fn unbound_identity_reports_null_binds_rather_than_inventing_one() {
        let tmp = tempfile::TempDir::new().unwrap();
        let identity = HealthIdentity::unbound(&paths(tmp.path().join("absent")));
        let body = identity.body();
        assert!(body["web_bind"].is_null());
        assert!(body["dsh_web_bind"].is_null());
        assert!(body["build"].is_null());
        // A home that does not exist yet still reports the path as-given.
        assert!(body["home"].as_str().unwrap().ends_with("absent"));
    }
}
