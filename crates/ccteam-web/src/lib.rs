//! V0.3 web layer entry point.
//!
//! Single entry: [`serve`]. Wired up by `ccteam-cli` via
//! `commands::run_web` so the binary stays a thin protocol adapter.
//!
//! Ship state (per `docs/versions/v0-3/prd.md` §3 / §4 / §5 / §6):
//!
//! - **M5.0** — `GET /health` + bind / shutdown plumbing.
//! - **M5.1** — `GET /` dashboard, `GET /project/<slug>` detail
//!   page, `GET /assets/{file}` vendored static assets.
//! - **M5.2** — on-demand pane snapshots (`GET /api/<slug>/pane-snapshot.ansi`
//!   for xterm.js; the PNG `/screenshot/<slug>.png` fallback was removed
//!   2026-07-26 with the screenshot surface cull). Also
//!   shipped `GET /sse/all` + `GET /sse/project/<slug>` progress-file SSE
//!   streams (a `notify`-watcher broadcast) — **removed in v0.9.0 W4**
//!   (zero SPA consumers; superseded by the gateway-broadcast-backed
//!   `GET /api/v1/sessions/{sid}/events` (per-session) and
//!   `GET /api/v1/agents/events` (global team view)).
//! - **M5.3 (this PR)** — `POST /api/<slug>/{btw,inject_decision,
//!   pause,resume}` write actions backed by `ccteam_core::actions::*`,
//!   plus a token-auth gate (`auth_layer` middleware) on the entire
//!   stateful router. Loopback bind defaults to no auth; non-loopback
//!   bind generates `~/.ccteam/web-token` (mode 0600) and demands
//!   `Authorization: Bearer ccteam:<token>` on every request (or the
//!   matching `ccteam_token` cookie set by the URL shim).
//!
//! [`ServeOpts`] is stable from M5.0 forward and now adds
//! `no_auth_grace_secs` (test seam — `Some(0)` skips the 5 s
//! Ctrl-C window when `--no-auth` opts out on a non-loopback bind).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::{middleware::from_fn_with_state, Router};
use ccteam_core::CcteamPaths;
use tokio::net::TcpListener;
use tower_http::compression::CompressionLayer;

pub mod auth;
pub mod chat_protocol;
pub mod decisions;
pub mod dsh_web;
pub mod metrics;
pub mod pty;
pub mod queries;
// v0.8.22 P1 (review §3.1-3) — per-session SSE replay ring + live tap; see
// `ring.rs`'s module doc. `pub(crate)`: only `state.rs` (spawns the feeder)
// and `routes::sessions_api` (the SSE handler) need it.
mod ring;
pub mod routes;
// v0.9.0 reverse-connection — the satellite CLIENT (outbound
// `ccteam-host.v1` control channel + `ccteam-exec.v1` dial-backs),
// embedded in every `ccteam start`. Separate from `routes/`: it is not
// part of the `/api/v1` main-daemon surface at all.
pub mod satellite;
pub mod state;
pub mod status;
pub mod token;
pub mod views;

pub use auth::AuthState;
pub use state::{AppState, ChatConns, CHAT_BACKLOG_CAP};

/// Knobs accepted by [`serve`]. Mirrors the `ccteam web` CLI flags
/// 1:1 so the CLI translation in `ccteam-cli::commands::run_web`
/// stays mechanical.
#[derive(Debug, Clone)]
pub struct ServeOpts {
    /// Address to bind. Default (`0.0.0.0:7331`) reaches the LAN with
    /// auto-enabled token auth; loopback (`127.0.0.1:<port>`) skips
    /// auth. `127.0.0.1:0` ⇒ pick a free port (used by integration
    /// tests).
    pub bind: SocketAddr,
    /// Disable token auth on write endpoints. M5.3 honors this; if
    /// the bind is non-loopback the operator gets a stderr warning +
    /// a [`no_auth_grace_secs`]-second Ctrl-C window before serving.
    pub no_auth: bool,
    /// Custom path to read the auth token from. Default
    /// (`None`) means `~/.ccteam/web-token`.
    pub token_file: Option<PathBuf>,
    /// Companion-port DSH web reverse proxy bind. `Some(addr)` starts the
    /// second listener in this daemon process; `None` disables it and the REST
    /// status reports `state: "disabled"`.
    pub dsh_web_bind: Option<SocketAddr>,
    /// Test seam: how long to sleep (eprintln'ing the LAN-RCE warning
    /// banner first) when `no_auth = true` AND the bind is
    /// non-loopback. Production `serve()` callers pass `Some(5)` —
    /// integration tests pass `Some(0)` so the captured stderr can be
    /// asserted without taking 5 s per case.
    pub no_auth_grace_secs: Option<u64>,
}

impl Default for ServeOpts {
    fn default() -> Self {
        Self {
            // V0.4.2: default to the unspecified bind so host
            // deployments are LAN-reachable out of the box. Auth is
            // automatically enabled on non-loopback (see `serve()`'s
            // auth heuristic table), so token-on-disk is the gate.
            bind: "0.0.0.0:7331"
                .parse()
                .expect("hardcoded unspecified parses"),
            no_auth: false,
            token_file: None,
            dsh_web_bind: Some(
                "0.0.0.0:7332"
                    .parse()
                    .expect("hardcoded DSH web companion bind parses"),
            ),
            no_auth_grace_secs: Some(5),
        }
    }
}

/// Build the router with a freshly resolved `CcteamPaths`. Used by
/// `serve`; tests call [`router_with_state`] directly to inject a
/// tempdir-backed `AppState`.
pub fn router() -> Result<Router> {
    let paths = CcteamPaths::from_env().context("resolve CcteamPaths from env for ccteam web")?;
    Ok(router_with_state(AppState::new(paths)))
}

/// Build the router with an explicit `AppState`. Tests use this so
/// each test owns its own `tempdir`-backed paths.
///
/// The auth layer wraps the **stateful** router (everything except
/// `/health`). When `state.auth.enabled = false` the layer is a
/// pass-through; when enabled it gates every route on a valid
/// `Authorization: Bearer ccteam:<token>` header or the matching
/// `ccteam_token` cookie set via the URL shim (see
/// `auth::auth_layer`).
pub fn router_with_state(state: AppState) -> Router {
    // Start only after composition has had a chance to replace the standalone
    // projection with the gateway-owned Arc. This avoids hydrating the same
    // large journals twice during daemon startup.
    state.progress_projection.start_hydration();
    let stateful = routes::stateful_router()
        // v0.8.18 档1 — project-ownership ACL for every `/projects/{slug}/...`
        // route. Layered INSIDE `auth_layer` (which is added after it, so it
        // wraps this) → auth injects `Identity` first, then this gate reads it.
        .layer(from_fn_with_state(state.clone(), auth::project_acl_layer))
        .layer(from_fn_with_state(state.clone(), auth::auth_layer))
        .with_state(state.clone());
    // `POST /mcp` mounts OUTSIDE auth_layer: it authenticates itself (a managed
    // session's principal `ccteam-sid:<sid>:<secret>`, or an enrolled client's
    // `ccteam-enroll:<id>:<secret>` plus the `Mcp-Session-Id` issued at
    // `initialize`) and enforces a bearer even when the web gate is disabled —
    // strictly no weaker than the layer it left. Behind auth_layer — which only
    // understands the web-token family — the session bearer was 401'd before the
    // handler ran, so managed sessions lost their Ambient identity and every A2A
    // spawn came out rootless (the v0.9.2 "delegation parent lost over HTTP"
    // fix). The web token is not a credential HERE at all: it names a browser
    // session and authenticates `/api/v1/**`, never the MCP data plane.
    let mcp = routes::mcp::router().with_state(state);
    Router::new()
        .merge(routes::stateless_router())
        .merge(mcp)
        .merge(stateful)
        .layer(CompressionLayer::new().gzip(true).br(true))
        .layer(axum::middleware::from_fn(metrics::record_request_latency))
}

/// Standalone `ccteam web` entry. Calls [`serve_with_shutdown`] with
/// the default Ctrl-C / SIGTERM signal-based shutdown.
///
/// Auth heuristic (PRD §6.2.4):
///
/// | bind             | --no-auth | enabled | token             |
/// |------------------|-----------|---------|-------------------|
/// | loopback         | false     | false   | not generated     |
/// | loopback         | true      | false   | not generated     |
/// | non-loopback     | false     | **true**| generated or read |
/// | non-loopback     | true      | false   | not generated     |
///
/// On the non-loopback `--no-auth` path we eprintln a LAN-wide RCE
/// banner and sleep [`ServeOpts::no_auth_grace_secs`] seconds (test
/// seam: pass `Some(0)` to skip in integration tests) so the operator
/// has a window to Ctrl-C out.
pub async fn serve(opts: ServeOpts) -> Result<()> {
    serve_with_shutdown(opts, shutdown_signal()).await
}

/// Embedded entry: serve the web UI until `shutdown` resolves. Used by
/// `ccteam start` to host the web server in the same process as the
/// orchestrator daemon (V0.4.1 simplification: one binary, one
/// terminal, one shutdown signal). The standalone `ccteam web` command
/// is a thin wrapper that supplies the default signal-based shutdown.
pub async fn serve_with_shutdown<F>(opts: ServeOpts, shutdown: F) -> Result<()>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    serve_with_state_factory_and_shutdown(opts, AppState::with_auth, None, shutdown).await
}

/// Embedded entry with caller-supplied state construction. `ccteam start`
/// uses this to install the web-chat bridge while preserving the same bind
/// and auth behavior as [`serve_with_shutdown`].
///
/// `dsh_runtime` is the daemon-wide DSH process manager: `ccteam start` builds
/// ONE in its composition root and hands it in here, so web and every other
/// consumer drive the same `dsh web` process per identity. `None` (standalone
/// `ccteam web`) builds a private one. Either way THIS function is what
/// configures it, once the bind is known — the daemon-owned instance is created
/// long before any port exists.
pub async fn serve_with_state_factory_and_shutdown<F, B>(
    opts: ServeOpts,
    build_state: B,
    dsh_runtime: Option<std::sync::Arc<ccteam_harness::DshRuntimeManager>>,
    shutdown: F,
) -> Result<()>
where
    F: std::future::Future<Output = ()> + Send + 'static,
    B: FnOnce(CcteamPaths, AuthState) -> AppState,
{
    let paths = CcteamPaths::from_env().context("resolve CcteamPaths from env for ccteam web")?;

    // Older builds could leave DSH children behind when the daemon was
    // SIGKILLed. Sweep only init-parented processes carrying a DSH_HOME under
    // this resolved ccteam runtime before accepting new work. New children are
    // also protected by Linux PDEATHSIG at both DSH spawn sites.
    ccteam_harness::sweep_legacy_dsh_orphans(&paths.root).await;

    let listener = TcpListener::bind(opts.bind)
        .await
        .with_context(|| format!("bind {} for ccteam web", opts.bind))?;
    let local = listener
        .local_addr()
        .context("read local_addr after bind")?;
    let non_loopback = !auth::is_loopback(&local);

    // Decide auth state from the bind heuristic.
    let auth_state = if !non_loopback {
        if opts.no_auth {
            eprintln!("ccteam web: --no-auth on loopback is the implicit default (no-op).");
        }
        AuthState::disabled()
    } else if opts.no_auth {
        eprintln!();
        eprintln!(
            "\x1b[1;31mWARNING:\x1b[0m --no-auth on non-loopback bind = LAN-wide RCE on bypassPermissions sessions.",
        );
        eprintln!(
            "Press Ctrl-C within {}s to abort.",
            opts.no_auth_grace_secs.unwrap_or(5)
        );
        eprintln!();
        let grace = opts.no_auth_grace_secs.unwrap_or(5);
        if grace > 0 {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(grace)) => {},
                _ = tokio::signal::ctrl_c() => {
                    eprintln!("ccteam web: aborted during --no-auth grace window.");
                    return Ok(());
                }
            }
        }
        AuthState::disabled()
    } else {
        let token_path = opts
            .token_file
            .clone()
            .unwrap_or_else(|| token::default_token_path(&paths));
        let token_existed = token_path.exists();
        let hex = token::generate_or_load_token(&token_path)
            .with_context(|| format!("load or generate token at {}", token_path.display()))?;
        if !token_existed {
            eprintln!(
                "ccteam web: generated new auth token at {}",
                token_path.display()
            );
        }
        // Echo to stderr (PRD §6.2.4) so the operator can paste it into
        // a browser. Using stderr (not stdout) keeps the subprocess
        // harness in tests free to parse the bind line on stdout
        // unambiguously.
        eprintln!("ccteam web: auth token: ccteam:{hex}");
        eprintln!(
            "ccteam web: reset token via:  rm {} && restart",
            token_path.display(),
        );
        AuthState::enabled(hex)
    };

    // Subprocess-friendly bind announcement. Format is stable:
    // `ccteam web listening on http://<addr>`. First line on stdout,
    // flushed before serving so test harnesses can parse the assigned
    // port when `bind = :0`.
    println!("ccteam web listening on http://{local}");
    tracing::info!(addr = %local, auth_enabled = auth_state.enabled, "ccteam web bound");

    let companion_listener = if let Some(bind) = opts.dsh_web_bind {
        Some(
            TcpListener::bind(bind)
                .await
                .with_context(|| format!("bind {bind} for ccteam DSH web companion"))?,
        )
    } else {
        None
    };
    let companion_local = companion_listener
        .as_ref()
        .map(TcpListener::local_addr)
        .transpose()
        .context("read DSH web companion local_addr after bind")?;

    let runtime = dsh_runtime.unwrap_or_else(|| dsh_web::new_runtime_manager(paths.root.clone()));
    runtime.configure(ccteam_harness::DshRuntimeConfig {
        enabled: companion_local.is_some(),
        daemon_url: format!("http://127.0.0.1:{}", local.port()),
        attach_url: None,
    });
    let supervisor = std::sync::Arc::new(dsh_web::DshWebSupervisor::new(runtime));
    if let Some(addr) = companion_local {
        supervisor.set_companion_addr(addr);
        println!("ccteam DSH web companion listening on http://{addr}");
        tracing::info!(addr = %addr, "ccteam DSH web companion bound");
    }

    let state = build_state(paths, auth_state).with_dsh_web(std::sync::Arc::clone(&supervisor));
    let app = router_with_state(state.clone());
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let shutdown_task = tokio::spawn(async move {
        shutdown.await;
        let _ = shutdown_tx.send(true);
    });
    let companion_handle = companion_listener.map(|listener| {
        let mut rx = shutdown_rx.clone();
        let app = dsh_web::companion_router()
            .with_state(state)
            .into_make_service_with_connect_info::<std::net::SocketAddr>();
        tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = rx.changed().await;
                })
                .await
        })
    });
    let mut main_rx = shutdown_rx.clone();
    // `connect_info` carries the TCP peer into request extensions — the fact
    // `/mcp` provenance auth resolves a loopback caller's process from. A
    // router served without it (tests, embedded uses) still works; provenance
    // is simply skipped there.
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        let _ = main_rx.changed().await;
    })
    .await
    .context("axum serve loop terminated with error")?;
    let _ = shutdown_task.await;
    if let Some(handle) = companion_handle {
        handle
            .await
            .context("join DSH web companion listener task")?
            .context("DSH web companion serve loop terminated with error")?;
    }
    supervisor.shutdown_all().await;
    Ok(())
}

/// Wait for Ctrl-C OR SIGTERM (unix only). Mirrors the orchestrator
/// daemon's `run_start` shutdown plumbing so behavior is consistent.
pub async fn shutdown_signal() {
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

    fn fake_state() -> AppState {
        let tmp = tempfile::tempdir().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join(".ccteam"),
            projects_root: tmp.path().join("projects"),
        };
        // tmp is dropped at end of scope but AppState only uses paths
        // by value; for a test that just hits /health this is fine.
        std::mem::forget(tmp);
        AppState::new(paths)
    }

    #[tokio::test]
    async fn serve_health_endpoint_returns_ok_json() {
        let listener =
            TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 0))
                .await
                .unwrap();
        let addr = listener.local_addr().unwrap();
        let app = router_with_state(fake_state());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        // Give the listener task a tick to begin polling.
        tokio::task::yield_now().await;

        let url = format!("http://{addr}/health");
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let resp = client.get(&url).send().await.expect("GET /health");
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
        // M5.3 reads `bind` / `no_auth` / `token_file` /
        // `no_auth_grace_secs` by name. If a future PR renames any of
        // them, this test compiles-fails — a deliberate tripwire for
        // contract stability.
        let opts = ServeOpts {
            bind: "127.0.0.1:7331".parse().unwrap(),
            no_auth: false,
            token_file: None,
            dsh_web_bind: Some("127.0.0.1:7332".parse().unwrap()),
            no_auth_grace_secs: Some(5),
        };
        assert!(!opts.no_auth);
        assert!(opts.token_file.is_none());
        assert_eq!(opts.bind.port(), 7331);
        assert_eq!(opts.dsh_web_bind.unwrap().port(), 7332);
        assert_eq!(opts.no_auth_grace_secs, Some(5));
        // V0.4.2: default bind is unspecified (0.0.0.0) so host
        // deployments reach the LAN by default. Auth-on stance is
        // preserved (no_auth defaults to false) — the unspecified
        // bind triggers the auth-enabled branch in `serve()`.
        let d = ServeOpts::default();
        assert!(d.bind.ip().is_unspecified(), "default bind is 0.0.0.0");
        assert!(!d.bind.ip().is_loopback(), "default bind is not loopback");
        assert!(!d.no_auth, "default keeps auth on");
        assert_eq!(d.dsh_web_bind.unwrap().port(), 7332);
        assert_eq!(d.no_auth_grace_secs, Some(5));
    }
}
