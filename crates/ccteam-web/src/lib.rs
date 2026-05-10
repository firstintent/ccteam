//! V0.3 M5.0 — `ccteam web` axum scaffold.
//!
//! Single entry point: [`serve`]. Wired up by `ccteam-cli` via
//! `commands::run_web` so the binary stays a thin protocol adapter.
//!
//! M5.0 scope (see `docs/v0-3/prd.md` §3 + `docs/v0-3/dev-plan.md` §2):
//!
//! - bind axum router on `opts.bind`
//! - install one route: `GET /health`
//! - graceful shutdown on Ctrl-C / SIGTERM
//! - print the actual bound `SocketAddr` to stdout so subprocess
//!   harnesses can read the port assigned by `127.0.0.1:0`
//!
//! Dashboard / SSE / write actions / token auth land in M5.1 / M5.2 /
//! M5.3. The [`ServeOpts`] shape is stable from M5.0 forward — M5.3
//! consumes `no_auth` / `token_file` exactly as defined here.

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result};
use axum::Router;
use tokio::net::TcpListener;

pub mod routes;

/// Knobs accepted by [`serve`]. Mirrors the `ccteam web` CLI flags
/// 1:1 so the CLI translation in `ccteam-cli::commands::run_web`
/// stays mechanical.
#[derive(Debug, Clone)]
pub struct ServeOpts {
    /// Address to bind. `127.0.0.1:0` ⇒ pick a free port (used by
    /// integration tests).
    pub bind: SocketAddr,
    /// Disable token auth on write endpoints. M5.3 will honor this;
    /// M5.0 has no write endpoints so the flag is currently a
    /// no-op record for ServeOpts shape stability.
    pub no_auth: bool,
    /// Custom path to read the auth token from. Default
    /// (`None`) means `~/.ccteam/web-token`. M5.3 consumes this.
    pub token_file: Option<PathBuf>,
}

/// Build the M5.0 router. Kept separate from [`serve`] so unit tests
/// can mount it without binding a real port.
pub fn router() -> Router {
    Router::new().merge(routes::router())
}

/// Start the web server. Binds, prints the bound address one line
/// to stdout (so subprocess harnesses can parse the port for the
/// `0` placeholder case), then serves until Ctrl-C / SIGTERM.
pub async fn serve(opts: ServeOpts) -> Result<()> {
    let listener = TcpListener::bind(opts.bind)
        .await
        .with_context(|| format!("bind {} for ccteam web", opts.bind))?;
    let local = listener
        .local_addr()
        .context("read local_addr after bind")?;

    // Subprocess-friendly bind announcement. Format is stable:
    // `ccteam web listening on http://<addr>`. The first line written
    // to stdout, flushed before serving so test harnesses can read
    // the port assigned to `:0`.
    println!("ccteam web listening on http://{local}");
    if opts.no_auth {
        eprintln!("ccteam web: --no-auth is set (M5.3 will honor this on write endpoints).");
    }
    if let Some(token) = &opts.token_file {
        eprintln!(
            "ccteam web: --token-file {} (M5.3 will honor this on write endpoints).",
            token.display(),
        );
    }
    tracing::info!(addr = %local, "ccteam web bound");

    let app = router();
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("axum serve loop terminated with error")?;
    Ok(())
}

/// Wait for Ctrl-C OR SIGTERM (unix only). Mirrors the orchestrator
/// daemon's `run_start` shutdown plumbing so behavior is consistent.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let ctrl_c = async {
            tokio::signal::ctrl_c().await.ok();
        };
        let sigterm = async {
            match signal(SignalKind::terminate()) {
                Ok(mut s) => {
                    s.recv().await;
                }
                Err(err) => {
                    tracing::warn!(
                        ?err,
                        "could not install SIGTERM handler; falling back to ctrl_c only"
                    );
                    // Sleep forever — the ctrl_c arm will still fire.
                    std::future::pending::<()>().await;
                }
            }
        };
        tokio::select! {
            _ = ctrl_c => tracing::info!("ccteam web: ctrl_c received"),
            _ = sigterm => tracing::info!("ccteam web: SIGTERM received"),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("ccteam web: ctrl_c received");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    #[tokio::test]
    async fn serve_health_endpoint_returns_ok_json() {
        let listener =
            TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 0))
                .await
                .unwrap();
        let addr = listener.local_addr().unwrap();
        let app = router();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        // Give the listener task a tick to begin polling.
        tokio::task::yield_now().await;

        let url = format!("http://{addr}/health");
        let resp = reqwest::get(&url).await.expect("GET /health");
        assert_eq!(resp.status(), 200);
        let json: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(json["status"], "ok");
        // Version is stamped from CARGO_PKG_VERSION at build time.
        assert!(
            json["version"]
                .as_str()
                .unwrap_or("")
                .chars()
                .next()
                .is_some(),
            "version must be a non-empty string",
        );

        server.abort();
    }

    #[test]
    fn serve_opts_shape_is_stable() {
        // M5.3 will read these three fields by name. If a future PR
        // renames any of them, this test compiles-fails — a deliberate
        // tripwire for M5.3 contract stability.
        let opts = ServeOpts {
            bind: "127.0.0.1:7331".parse().unwrap(),
            no_auth: false,
            token_file: None,
        };
        assert!(!opts.no_auth);
        assert!(opts.token_file.is_none());
        assert_eq!(opts.bind.port(), 7331);
    }
}
