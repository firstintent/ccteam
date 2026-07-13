//! Remote-host spawn gate + satellite exec-bridge proxy seam
//! (v0.8.24 Track D gate; v0.9.0 W3 F3 makes it a REAL execution proxy).
//!
//! Architecture (see `docs-local/versions/v0-9-0/tech-design.md` §4):
//! - Host registry (join + heartbeat) is the ops registration surface.
//! - Session spawn/rebuild with `host != local` is gated: offline / unknown
//!   host / terminal protocol / the satellite not having `slug` registered
//!   → readable error (session **not** created, and — critically for G10 —
//!   an existing remote session is **never** silently respawned locally).
//! - Stdio protocols (stream-json / acp) proxy to the satellite's
//!   `ccteam-exec.v1` bridge (`crate::remote_exec` re-export from
//!   `ccteam-harness`) once the gate passes. Production proxy dials the
//!   satellite's `GET /health` then hands back a [`RemoteExecTarget`]
//!   pointing at its `GET /ws/exec`; tests inject a [`RemoteHostProxy`]
//!   that fakes (or in-process-duplexes) the handoff.

use std::sync::Arc;

use anyhow::{bail, Result};
use async_trait::async_trait;
use ccteam_core::host_registry::{
    gate_remote_spawn, gate_remote_spawn_project, HostRegistry, DEFAULT_HEARTBEAT_TTL_SECS,
    LOCAL_HOST,
};
use ccteam_harness::{RemoteExecTarget, SessionProtocol};
use std::path::Path;

/// Decision after the host gate for a create/resume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteSpawnPlan {
    /// Run the adapter on this machine (host is local or empty).
    Local,
    /// Proxy to satellite (stdio only; host online + slug registered there).
    Remote {
        /// Satellite host id that will run the stdio session.
        host_id: String,
    },
}

/// What every spawn/rebuild call site needs after the host gate: the
/// normalized host id to stamp on the session, and — for a remote host —
/// the exec-bridge target to thread into `SpawnCtx::remote`. `remote:
/// None` with `host == "local"` is the overwhelming common case.
#[derive(Debug, Clone)]
pub struct HostTarget {
    /// Normalized host id (`"local"` or the satellite id) to stamp on the
    /// session.
    pub host: String,
    /// `Some` for an online, registered remote host — thread into
    /// `SpawnCtx::remote`. `None` for `host == "local"`.
    pub remote: Option<RemoteExecTarget>,
}

impl HostTarget {
    fn local() -> Self {
        Self {
            host: LOCAL_HOST.to_string(),
            remote: None,
        }
    }
}

/// Pluggable satellite exec-bridge proxy. Production default health-checks
/// `agent_url` then derives the `ws://…/ws/exec` target; tests install a
/// fake that skips the network hop (or points at an in-process satellite).
#[async_trait]
pub trait RemoteHostProxy: Send + Sync {
    /// Ensure a remote stdio session can be started on `host_id` and return
    /// the exec-bridge target to dial. Returning `Ok` means the main daemon
    /// may proceed to track the session; `Err` is a readable user-facing
    /// failure — the session must not be inserted (and, on a rebuild, must
    /// stay stopped rather than silently respawn locally — G10).
    async fn ensure_remote_spawn(
        &self,
        host_id: &str,
        protocol: SessionProtocol,
        agent_url: Option<&str>,
        agent_token: &str,
    ) -> Result<RemoteExecTarget>;
}

/// Rewrite an `http(s)://host:port` agent base URL into its
/// `ws(s)://host:port/ws/exec` exec-bridge counterpart.
pub fn agent_url_to_exec_ws(agent_url: &str) -> String {
    let trimmed = agent_url.trim().trim_end_matches('/');
    let ws_base = if let Some(rest) = trimmed.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = trimmed.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        // Already a ws(s):// URL (or schemeless) — pass through.
        trimmed.to_string()
    };
    format!("{ws_base}/ws/exec")
}

/// Production proxy: requires an `agent_url` and a successful HTTP health
/// probe (`GET {agent_url}/health` → 2xx), then hands back the exec-bridge
/// target (`ws://…/ws/exec` + the host's agent token). Failures are
/// readable; offline / unregistered-slug were already gated before this is
/// called.
pub struct HttpRemoteHostProxy;

#[async_trait]
impl RemoteHostProxy for HttpRemoteHostProxy {
    async fn ensure_remote_spawn(
        &self,
        host_id: &str,
        protocol: SessionProtocol,
        agent_url: Option<&str>,
        agent_token: &str,
    ) -> Result<RemoteExecTarget> {
        if protocol.is_terminal() {
            bail!(
                "terminal protocol cannot run on remote host `{host_id}` \
                 (multi-host supports stdio only)"
            );
        }
        let Some(url) = agent_url.filter(|u| !u.is_empty()) else {
            bail!(
                "host `{host_id}` is online but has no satellite agent endpoint \
                 (agent_url); re-join with --agent-url or wait for a heartbeat that \
                 advertises one"
            );
        };
        let health = format!("{}/health", url.trim_end_matches('/'));
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(3))
            .build()
            .map_err(|e| anyhow::anyhow!("remote host http client: {e}"))?;
        match client.get(&health).send().await {
            Ok(resp) if resp.status().is_success() => Ok(RemoteExecTarget {
                exec_ws_url: agent_url_to_exec_ws(url),
                agent_token: agent_token.to_string(),
            }),
            Ok(resp) => bail!(
                "host `{host_id}` agent at {url} returned HTTP {}; session was not created",
                resp.status()
            ),
            Err(e) => {
                bail!("host `{host_id}` agent unreachable at {url}: {e}; session was not created")
            }
        }
    }
}

/// Test / in-process proxy. Records the last host id for assertions;
/// `exec_target` lets a test point the returned [`RemoteExecTarget`] at an
/// in-process fake satellite (e.g. a `tokio::net::TcpListener` bound to
/// `127.0.0.1:0` speaking real `ccteam-exec.v1`) instead of deriving one
/// from the registry's (often dummy) `agent_url` — the "in-process duplex
/// mode" gateway e2e tests need.
#[derive(Default)]
pub struct FakeRemoteHostProxy {
    /// Last host id passed to [`RemoteHostProxy::ensure_remote_spawn`].
    pub last_host: std::sync::Mutex<Option<String>>,
    /// When true, `ensure_remote_spawn` returns an error.
    pub fail: std::sync::Mutex<bool>,
    /// Override the returned target (bypassing `agent_url` derivation).
    pub exec_target: std::sync::Mutex<Option<RemoteExecTarget>>,
}

#[async_trait]
impl RemoteHostProxy for FakeRemoteHostProxy {
    async fn ensure_remote_spawn(
        &self,
        host_id: &str,
        protocol: SessionProtocol,
        agent_url: Option<&str>,
        agent_token: &str,
    ) -> Result<RemoteExecTarget> {
        if protocol.is_terminal() {
            bail!("terminal protocol cannot run on remote host `{host_id}`");
        }
        if *self.fail.lock().unwrap() {
            bail!("host `{host_id}` proxy failed (fake); session was not created");
        }
        *self.last_host.lock().unwrap() = Some(host_id.to_string());
        if let Some(target) = self.exec_target.lock().unwrap().clone() {
            return Ok(target);
        }
        let url = agent_url
            .filter(|u| !u.is_empty())
            .unwrap_or("http://127.0.0.1:0");
        Ok(RemoteExecTarget {
            exec_ws_url: agent_url_to_exec_ws(url),
            agent_token: agent_token.to_string(),
        })
    }
}

/// Load registry from a ccteam home root and plan a spawn for `host`.
/// Gates offline / terminal-on-remote / unknown host (NOT the per-project
/// registration — that additionally needs `slug`, gated by
/// [`prepare_host_for_spawn`] once the registry is loaded).
pub fn plan_spawn_host(
    ccteam_root: &Path,
    host: &str,
    protocol: SessionProtocol,
) -> Result<RemoteSpawnPlan> {
    let host = if host.is_empty() { LOCAL_HOST } else { host };
    if host == LOCAL_HOST {
        return Ok(RemoteSpawnPlan::Local);
    }
    let reg_path = ccteam_core::host_registry::registry_path_in(ccteam_root);
    let reg = HostRegistry::load(&reg_path)?;
    gate_remote_spawn(
        &reg,
        host,
        protocol.is_terminal(),
        DEFAULT_HEARTBEAT_TTL_SECS,
    )?;
    Ok(RemoteSpawnPlan::Remote {
        host_id: host.to_string(),
    })
}

/// Full gate + proxy ensure for a **create** (fresh spawn). Returns the
/// [`HostTarget`] to stamp on the session / thread into `SpawnCtx::remote`.
pub async fn prepare_host_for_spawn(
    ccteam_root: Option<&Path>,
    host: &str,
    slug: &str,
    protocol: SessionProtocol,
    proxy: Option<&Arc<dyn RemoteHostProxy>>,
) -> Result<HostTarget> {
    let host = if host.is_empty() {
        LOCAL_HOST.to_string()
    } else {
        host.to_string()
    };
    if host == LOCAL_HOST {
        return Ok(HostTarget::local());
    }
    let Some(root) = ccteam_root else {
        bail!(
            "remote host `{host}` requested but daemon has no ccteam home \
             (host registry unavailable); session was not created"
        );
    };
    let plan = plan_spawn_host(root, &host, protocol)?;
    match plan {
        RemoteSpawnPlan::Local => Ok(HostTarget::local()),
        RemoteSpawnPlan::Remote { host_id } => {
            let reg = HostRegistry::load(&ccteam_core::host_registry::registry_path_in(root))?;
            let rec = reg
                .get(&host_id)
                .ok_or_else(|| anyhow::anyhow!("unknown host: {host_id}"))?;
            // v0.9.0 W3 (G9) — the satellite must have ITS OWN copy of this
            // project registered (last reported at heartbeat), else a spawn
            // there would fail `unknown-slug` at the exec bridge anyway —
            // fail here with a more actionable message.
            gate_remote_spawn_project(&reg, &host_id, slug)?;
            let target = if let Some(proxy) = proxy {
                proxy
                    .ensure_remote_spawn(
                        &host_id,
                        protocol,
                        rec.agent_url.as_deref(),
                        &rec.agent_token,
                    )
                    .await?
            } else {
                HttpRemoteHostProxy
                    .ensure_remote_spawn(
                        &host_id,
                        protocol,
                        rec.agent_url.as_deref(),
                        &rec.agent_token,
                    )
                    .await?
            };
            Ok(HostTarget {
                host: host_id,
                remote: Some(target),
            })
        }
    }
}

/// v0.9.0 W3 (G10, safety-critical) — re-gate a REBUILD/resume/`/role`
/// switch whose persisted `meta.host` (or live `GatewaySession.host`) is
/// non-local. `host == "local"` (or empty) is a zero-cost no-op — the
/// overwhelming common case never touches the registry or network.
///
/// This is the single choke point every rebuild path must call before its
/// slow spawn: online + registered → `Some(RemoteExecTarget)` to thread
/// into `SpawnCtx::remote`; offline / unregistered / unknown → `Err`
/// (readable), and the caller MUST leave the session stopped rather than
/// fall back to a local respawn (the red line this closes).
pub async fn regate_remote_host(
    ccteam_root: Option<&Path>,
    host: &str,
    slug: &str,
    protocol: SessionProtocol,
    proxy: Option<&Arc<dyn RemoteHostProxy>>,
) -> Result<Option<RemoteExecTarget>> {
    if host.is_empty() || host == LOCAL_HOST {
        return Ok(None);
    }
    let target = prepare_host_for_spawn(ccteam_root, host, slug, protocol, proxy).await?;
    Ok(target.remote)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ccteam_core::host_registry::{now_unix, HostRecord};
    use tempfile::TempDir;

    fn registered_sat(tmp: &TempDir, agent_url: &str, last_heartbeat_unix: u64) {
        let mut reg = HostRegistry::default();
        reg.upsert(HostRecord {
            id: "sat".into(),
            hostname: "sat".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
            ccteam_version: "0.9.0".into(),
            agent_url: Some(agent_url.into()),
            agent_token: "t".into(),
            last_heartbeat_unix,
            agents: vec![],
            projects: vec![ccteam_core::HostProjectReport {
                slug: "demo".into(),
                path: "/home/sat/projects/demo".into(),
            }],
            joined_at: chrono::Utc::now().to_rfc3339(),
        });
        reg.save(&ccteam_core::host_registry::registry_path_in(tmp.path()))
            .unwrap();
    }

    #[tokio::test]
    async fn offline_host_errors_without_proxy_call() {
        let tmp = TempDir::new().unwrap();
        registered_sat(
            &tmp,
            "http://127.0.0.1:9",
            now_unix().saturating_sub(10_000),
        );
        let fake = Arc::new(FakeRemoteHostProxy::default());
        let proxy: Arc<dyn RemoteHostProxy> = fake.clone();
        let err = prepare_host_for_spawn(
            Some(tmp.path()),
            "sat",
            "demo",
            SessionProtocol::StreamJson,
            Some(&proxy),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("offline"));
        assert!(fake.last_host.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn online_host_calls_fake_proxy_and_returns_remote_target() {
        let tmp = TempDir::new().unwrap();
        registered_sat(&tmp, "http://127.0.0.1:9", now_unix());
        let fake = Arc::new(FakeRemoteHostProxy::default());
        let proxy: Arc<dyn RemoteHostProxy> = fake.clone();
        let target = prepare_host_for_spawn(
            Some(tmp.path()),
            "sat",
            "demo",
            SessionProtocol::StreamJson,
            Some(&proxy),
        )
        .await
        .unwrap();
        assert_eq!(target.host, "sat");
        assert_eq!(fake.last_host.lock().unwrap().as_deref(), Some("sat"));
        let remote = target
            .remote
            .expect("online host must yield a remote target");
        assert_eq!(remote.exec_ws_url, "ws://127.0.0.1:9/ws/exec");
        assert_eq!(remote.agent_token, "t");
    }

    #[tokio::test]
    async fn unregistered_slug_rejects_even_when_host_online() {
        let tmp = TempDir::new().unwrap();
        // Registered host, but its `projects` list does not include "other".
        registered_sat(&tmp, "http://127.0.0.1:9", now_unix());
        let fake = Arc::new(FakeRemoteHostProxy::default());
        let proxy: Arc<dyn RemoteHostProxy> = fake.clone();
        let err = prepare_host_for_spawn(
            Some(tmp.path()),
            "sat",
            "other",
            SessionProtocol::StreamJson,
            Some(&proxy),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("not registered"), "got: {err}");
        // The proxy (which would dial the satellite) must never be called.
        assert!(fake.last_host.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn regate_remote_host_is_a_zero_cost_noop_for_local() {
        assert!(
            regate_remote_host(None, "local", "demo", SessionProtocol::StreamJson, None)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            regate_remote_host(None, "", "demo", SessionProtocol::StreamJson, None)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn regate_remote_host_offline_errors_readable() {
        let tmp = TempDir::new().unwrap();
        registered_sat(
            &tmp,
            "http://127.0.0.1:9",
            now_unix().saturating_sub(10_000),
        );
        let fake = Arc::new(FakeRemoteHostProxy::default());
        let proxy: Arc<dyn RemoteHostProxy> = fake;
        let err = regate_remote_host(
            Some(tmp.path()),
            "sat",
            "demo",
            SessionProtocol::StreamJson,
            Some(&proxy),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("offline"));
    }

    #[test]
    fn agent_url_to_exec_ws_rewrites_scheme_and_path() {
        assert_eq!(
            agent_url_to_exec_ws("http://192.168.1.10:7332"),
            "ws://192.168.1.10:7332/ws/exec"
        );
        assert_eq!(
            agent_url_to_exec_ws("https://sat.example.com/"),
            "wss://sat.example.com/ws/exec"
        );
    }

    /// Production [`HttpRemoteHostProxy`] must actually reach the satellite's
    /// exec bridge after a healthy probe — it no longer fails closed with a
    /// "not implemented" error (v0.9.0 W3 retires the v0.8.24 placeholder).
    #[tokio::test]
    async fn http_proxy_healthy_satellite_returns_exec_target() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf).await;
                let _ = sock
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                    .await;
            }
        });
        let url = format!("http://{addr}");
        let target = HttpRemoteHostProxy
            .ensure_remote_spawn("sat", SessionProtocol::StreamJson, Some(&url), "tok")
            .await
            .unwrap();
        assert_eq!(target.exec_ws_url, format!("ws://{addr}/ws/exec"));
        assert_eq!(target.agent_token, "tok");
        server.abort();
    }
}
