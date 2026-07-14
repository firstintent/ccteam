//! v0.9.0 reverse-connection — the satellite CLIENT.
//!
//! A satellite exposes **no listener**. This module maintains one outbound
//! `ccteam-host.v1` control WebSocket to the main daemon
//! (`GET {daemon}/api/v1/hosts/channel`, bearer = this host's agent token)
//! and, on each `exec_open{nonce}` frame, dials back a fresh
//! `ccteam-exec.v1` WS (`GET {daemon}/api/v1/hosts/exec/{nonce}`) and runs
//! the protocol-blind exec engine (`ccteam_harness::run_exec_session`)
//! over it. All satellite traffic is outbound to the daemon's single web
//! port — only the daemon needs a reachable address.
//!
//! Embedded in every `ccteam start` (unified process: a node is a
//! satellite iff `ccteam host join` wrote `state/hosts/self.json`, which
//! is polled so a join after startup activates without a restart).
//!
//! Production-stability contract:
//! - reconnect with jittered exponential backoff (1s → 60s cap), reset
//!   after a connection that stayed up ≥ [`STABLE_AFTER`];
//! - periodic `report` frames (agents/projects/version, every
//!   `REPORT_PERIOD`) double as application-level keepalive;
//! - half-open detection: the daemon pings every `KEEPALIVE_PERIOD`; a
//!   channel with no inbound frame for `IDLE_TIMEOUT` is torn down and
//!   redialed;
//! - unknown control-channel ops are ignored (forward compatibility).

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use ccteam_core::{CcteamPaths, SatelliteSelf};
use ccteam_harness::{
    run_exec_session, SatelliteExecCtx, EXEC_SUBPROTOCOL, HOST_CHANNEL_SUBPROTOCOL, IDLE_TIMEOUT,
    REPORT_PERIOD,
};
use futures::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;

/// Re-check `state/hosts/self.json` this often while not joined.
const RECHECK: Duration = Duration::from_secs(30);

/// TCP/WS handshake timeout for both the control channel and dial-backs.
const DIAL_TIMEOUT: Duration = Duration::from_secs(10);

/// A connection that stayed up at least this long resets the backoff.
const STABLE_AFTER: Duration = Duration::from_secs(30);

/// Reconnect backoff cap.
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// Why the control channel ended.
enum ChannelEnd {
    /// Daemon closed / link dropped / half-open teardown — redial.
    Closed,
    /// Process shutdown — exit the client loop.
    Shutdown,
}

/// Rewrite the daemon's `http(s)://…` base URL to `ws(s)://…{path}`.
fn ws_url(daemon_url: &str, path: &str) -> String {
    let trimmed = daemon_url.trim().trim_end_matches('/');
    let base = if let Some(rest) = trimmed.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = trimmed.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        // Already ws(s):// (or schemeless) — pass through.
        trimmed.to_string()
    };
    format!("{base}{path}")
}

/// ±20% wall-clock jitter (no `rand` dependency for a scheduling nicety) —
/// keeps a fleet of satellites from sync-dialing a restarted daemon.
fn jittered(period: Duration) -> Duration {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let frac = (nanos % 4001) as i64 - 2000; // [-2000, 2000]
    let delta_ms = (period.as_millis() as i64 * frac) / 10_000;
    let total_ms = (period.as_millis() as i64 + delta_ms).max(100);
    Duration::from_millis(total_ms as u64)
}

/// Build the `{"op":"report", …}` control frame: this machine's agent
/// probe + its own registered projects — the fields the daemon-side gate
/// (`gate_remote_spawn_project`) and the hosts UI read.
async fn build_report(paths: &CcteamPaths) -> serde_json::Value {
    let agents = tokio::task::spawn_blocking(ccteam_core::probe_agents)
        .await
        .unwrap_or_default();
    let projects: Vec<ccteam_core::HostProjectReport> = ccteam_core::config::load(&paths.root)
        .map(|cfg| {
            cfg.projects
                .into_iter()
                .map(|p| ccteam_core::HostProjectReport {
                    slug: p.slug,
                    path: p.path.display().to_string(),
                })
                .collect()
        })
        .unwrap_or_default();
    serde_json::json!({
        "op": "report",
        "agents": agents,
        "projects": projects,
        "ccteam_version": env!("CARGO_PKG_VERSION"),
    })
}

/// Build an authenticated WS client request (bearer + subprotocol).
fn ws_request(
    url: &str,
    agent_token: &str,
    subprotocol: &'static str,
) -> Result<tokio_tungstenite::tungstenite::handshake::client::Request> {
    let mut request = url
        .into_client_request()
        .with_context(|| format!("invalid ws url: {url}"))?;
    request.headers_mut().insert(
        tokio_tungstenite::tungstenite::http::header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer ccteam:{agent_token}"))
            .context("agent token is not a valid header value")?,
    );
    request.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        HeaderValue::from_static(subprotocol),
    );
    Ok(request)
}

/// The resident satellite client: poll for join credentials, keep the
/// control channel up (backoff + jitter + stable-reset), dispatch exec
/// dial-backs. Runs until `shutdown` flips.
pub async fn run_satellite_client(
    paths: CcteamPaths,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    let self_path = SatelliteSelf::path_in(&paths.root);
    let mut consecutive_failures: u32 = 0;
    loop {
        if *shutdown.borrow() {
            return;
        }
        // (Re)load credentials each attempt: a fresh `host join` (new
        // daemon URL / token) takes effect on the next dial, not a restart.
        let me = match SatelliteSelf::load(&self_path) {
            Ok(me) => me,
            Err(_) => {
                // Not joined (yet) — keep watching.
                tokio::select! {
                    _ = shutdown.changed() => return,
                    _ = tokio::time::sleep(RECHECK) => continue,
                }
            }
        };
        let started = tokio::time::Instant::now();
        match run_control_channel(&paths, &me, &mut shutdown).await {
            Ok(ChannelEnd::Shutdown) => return,
            Ok(ChannelEnd::Closed) => {
                tracing::info!(host = %me.host, daemon = %me.daemon_url, "control channel closed; redialing");
            }
            Err(err) => {
                tracing::warn!(host = %me.host, daemon = %me.daemon_url, error = %err, "control channel failed");
            }
        }
        if started.elapsed() >= STABLE_AFTER {
            consecutive_failures = 0;
        } else {
            consecutive_failures = consecutive_failures.saturating_add(1);
        }
        let backoff = Duration::from_secs(1u64 << consecutive_failures.min(6)).min(MAX_BACKOFF);
        tokio::select! {
            _ = shutdown.changed() => return,
            _ = tokio::time::sleep(jittered(backoff)) => {}
        }
    }
}

/// One control-channel connection lifetime.
async fn run_control_channel(
    paths: &CcteamPaths,
    me: &SatelliteSelf,
    shutdown: &mut tokio::sync::watch::Receiver<bool>,
) -> Result<ChannelEnd> {
    let url = ws_url(&me.daemon_url, "/api/v1/hosts/channel");
    let request = ws_request(&url, &me.agent_token, HOST_CHANNEL_SUBPROTOCOL)?;
    let (ws, _resp) = tokio::time::timeout(DIAL_TIMEOUT, tokio_tungstenite::connect_async(request))
        .await
        .map_err(|_| anyhow::anyhow!("ccteam-host.v1 dial to {url} timed out"))?
        .with_context(|| format!("ccteam-host.v1 dial to {url} failed"))?;
    tracing::info!(host = %me.host, daemon = %me.daemon_url, "control channel connected");
    let (mut sink, mut stream) = ws.split();

    // Immediate report on connect (instant presence + fresh projects).
    let first = build_report(paths).await;
    sink.send(Message::Text(first.to_string()))
        .await
        .context("send initial report")?;

    let mut report_tick = tokio::time::interval(REPORT_PERIOD);
    report_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    report_tick.reset(); // the initial report above covers the first period
    let mut idle_tick = tokio::time::interval(IDLE_TIMEOUT / 3);
    idle_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_rx = tokio::time::Instant::now();

    loop {
        tokio::select! {
            msg = stream.next() => match msg {
                Some(Ok(Message::Text(t))) => {
                    last_rx = tokio::time::Instant::now();
                    handle_control_frame(paths, me, &t);
                }
                Some(Ok(Message::Close(_))) | None => return Ok(ChannelEnd::Closed),
                Some(Ok(_)) => {
                    // Ping/Pong/Binary — liveness only (tungstenite
                    // auto-answers pings while the stream is polled).
                    last_rx = tokio::time::Instant::now();
                }
                Some(Err(e)) => return Err(e).context("control channel read"),
            },
            _ = report_tick.tick() => {
                let report = build_report(paths).await;
                sink.send(Message::Text(report.to_string()))
                    .await
                    .context("send report")?;
            }
            _ = idle_tick.tick() => {
                if last_rx.elapsed() > IDLE_TIMEOUT {
                    anyhow::bail!(
                        "control channel idle past {}s (half-open link)",
                        IDLE_TIMEOUT.as_secs()
                    );
                }
            }
            _ = shutdown.changed() => {
                let _ = sink.send(Message::Close(None)).await;
                return Ok(ChannelEnd::Shutdown);
            }
        }
    }
}

/// One inbound control frame. Unknown ops are ignored (forward compat).
fn handle_control_frame(paths: &CcteamPaths, me: &SatelliteSelf, raw: &str) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) else {
        tracing::debug!("satellite: non-JSON control frame ignored");
        return;
    };
    match v.get("op").and_then(|o| o.as_str()) {
        Some("exec_open") => {
            let Some(nonce) = v.get("nonce").and_then(|n| n.as_str()) else {
                tracing::warn!("satellite: exec_open without nonce ignored");
                return;
            };
            let sid = v
                .get("sid")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            tracing::info!(sid = %sid, "satellite: exec_open — dialing back");
            let paths = paths.clone();
            let me = me.clone();
            let nonce = nonce.to_string();
            tokio::spawn(async move {
                if let Err(err) = run_exec_dialback(paths, me, &nonce).await {
                    tracing::warn!(sid = %sid, error = %err, "satellite: exec dial-back failed");
                }
            });
        }
        other => {
            tracing::debug!(op = ?other, "satellite: unknown control op ignored (forward-compat)")
        }
    }
}

/// Dial back one `ccteam-exec.v1` WS for `nonce` and run the exec engine
/// over it. The engine owns the whole session lifecycle (spec → spawn →
/// pump → exit tail); this function only supplies transport + context.
async fn run_exec_dialback(paths: CcteamPaths, me: SatelliteSelf, nonce: &str) -> Result<()> {
    let url = ws_url(&me.daemon_url, &format!("/api/v1/hosts/exec/{nonce}"));
    let request = ws_request(&url, &me.agent_token, EXEC_SUBPROTOCOL)?;
    let (ws, _resp) = tokio::time::timeout(DIAL_TIMEOUT, tokio_tungstenite::connect_async(request))
        .await
        .map_err(|_| anyhow::anyhow!("ccteam-exec.v1 dial-back timed out"))?
        .context("ccteam-exec.v1 dial-back failed")?;
    let (sink, stream) = ws.split();
    let root = paths.root.clone();
    let resolver = move |slug: &str| -> Option<PathBuf> {
        // THIS machine's own project registry — an unregistered slug is
        // rejected by the engine, never guessed.
        ccteam_core::config::load(&root)
            .ok()?
            .projects
            .into_iter()
            .find(|p| p.slug == slug)
            .map(|p| p.path)
    };
    let ctx = SatelliteExecCtx {
        daemon_url: &me.daemon_url,
        resolve_project_dir: &resolver,
    };
    run_exec_session(stream, sink, &ctx).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_url_rewrites_scheme_and_appends_path() {
        assert_eq!(
            ws_url("http://192.168.1.10:7331", "/api/v1/hosts/channel"),
            "ws://192.168.1.10:7331/api/v1/hosts/channel"
        );
        assert_eq!(
            ws_url("https://daemon.example.com/", "/api/v1/hosts/exec/abc"),
            "wss://daemon.example.com/api/v1/hosts/exec/abc"
        );
        assert_eq!(ws_url("ws://127.0.0.1:7331", "/x"), "ws://127.0.0.1:7331/x");
    }

    #[test]
    fn jittered_stays_within_20_percent_plus_floor() {
        let base = Duration::from_secs(10);
        for _ in 0..32 {
            let j = jittered(base);
            assert!(j >= Duration::from_secs(8), "got {j:?}");
            assert!(j <= Duration::from_secs(12), "got {j:?}");
        }
    }

    #[tokio::test]
    async fn build_report_carries_op_and_version() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let report = build_report(&paths).await;
        assert_eq!(report["op"], "report");
        assert_eq!(report["ccteam_version"], env!("CARGO_PKG_VERSION"));
        assert!(report["projects"].is_array());
        // Parses into the wire type the daemon applies (minus the op tag).
        let parsed: ccteam_core::HostReport = serde_json::from_value(report).unwrap();
        assert!(parsed.ccteam_version.is_some());
    }

    #[test]
    fn ws_request_carries_bearer_and_subprotocol() {
        let req = ws_request(
            "ws://127.0.0.1:7331/api/v1/hosts/channel",
            "deadbeef",
            HOST_CHANNEL_SUBPROTOCOL,
        )
        .unwrap();
        assert_eq!(
            req.headers()
                .get("authorization")
                .and_then(|v| v.to_str().ok()),
            Some("Bearer ccteam:deadbeef")
        );
        assert_eq!(
            req.headers()
                .get("sec-websocket-protocol")
                .and_then(|v| v.to_str().ok()),
            Some(HOST_CHANNEL_SUBPROTOCOL)
        );
    }
}
