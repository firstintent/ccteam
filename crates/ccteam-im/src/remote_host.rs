//! Remote-host spawn gate + satellite agent proxy seam (v0.8.24 Track D).
//!
//! Architecture (see `docs/dev/tech-design.md` §2.7):
//! - Host registry (join + heartbeat) is the ops registration surface.
//! - Session spawn with `host != local` is gated: offline → readable
//!   error (session **not** created); terminal protocol → hard reject.
//! - Stdio protocols (stream-json / acp) may proxy to a satellite agent
//!   when the host is online. Production proxy is HTTP to `agent_url`;
//!   tests inject a [`RemoteHostProxy`] that fakes the handoff.
//!
//! **Honest scope**: without a live satellite `agent_url`, an online
//! registered host is still "spawnable" only when a proxy is injected
//! (tests) or when production chooses to fail with a clear
//! "no agent endpoint" error. The default production path for an online
//! host with `agent_url` is a best-effort HTTP probe; full NDJSON
//! streaming proxy is the follow-on seam (transport already generic).

use std::sync::Arc;

use anyhow::{bail, Result};
use async_trait::async_trait;
use ccteam_core::host_registry::{
    gate_remote_spawn, HostRegistry, DEFAULT_HEARTBEAT_TTL_SECS, LOCAL_HOST,
};
use ccteam_harness::SessionProtocol;
use std::path::Path;

/// Decision after the host gate for a create/resume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteSpawnPlan {
    /// Run the adapter on this machine (host is local or empty).
    Local,
    /// Proxy to satellite (stdio only; host online).
    Remote {
        /// Satellite host id that will run the stdio session.
        host_id: String,
    },
}

/// Pluggable satellite agent proxy. Production default probes `agent_url`;
/// tests install a fake that records / succeeds without a real machine.
#[async_trait]
pub trait RemoteHostProxy: Send + Sync {
    /// Ensure a remote stdio session can be started on `host_id`.
    /// Returning Ok means the main daemon may proceed to track the session
    /// (either after a real remote handoff or a test fake). Err is a
    /// readable user-facing failure — the session must not be inserted.
    async fn ensure_remote_spawn(
        &self,
        host_id: &str,
        protocol: SessionProtocol,
        agent_url: Option<&str>,
    ) -> Result<()>;
}

/// Production proxy: requires an `agent_url` and a successful HTTP health
/// probe (`GET {agent_url}/health` → 2xx). Does **not** yet stream NDJSON
/// over the wire — that is the next seam on
/// `claude_stream_json::transport`. Failures are readable; offline was
/// already gated before this is called.
pub struct HttpRemoteHostProxy;

#[async_trait]
impl RemoteHostProxy for HttpRemoteHostProxy {
    async fn ensure_remote_spawn(
        &self,
        host_id: &str,
        protocol: SessionProtocol,
        agent_url: Option<&str>,
    ) -> Result<()> {
        if protocol.is_terminal() {
            bail!(
                "terminal protocol cannot run on remote host `{host_id}` \
                 (multi-host supports stdio only)"
            );
        }
        let Some(url) = agent_url.filter(|u| !u.is_empty()) else {
            bail!(
                "host `{host_id}` is online but has no satellite agent endpoint \
                 (agent_url); re-join with --agent-url or wait for heartbeat that \
                 advertises one"
            );
        };
        let health = format!("{}/health", url.trim_end_matches('/'));
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(3))
            .build()
            .map_err(|e| anyhow::anyhow!("remote host http client: {e}"))?;
        match client.get(&health).send().await {
            Ok(resp) if resp.status().is_success() => Ok(()),
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

/// Test / in-process proxy that always succeeds for online hosts (no HTTP).
/// Records the last host id for assertions.
#[derive(Default)]
pub struct FakeRemoteHostProxy {
    /// Last host id passed to [`RemoteHostProxy::ensure_remote_spawn`].
    pub last_host: std::sync::Mutex<Option<String>>,
    /// When true, `ensure_remote_spawn` returns an error.
    pub fail: std::sync::Mutex<bool>,
}

#[async_trait]
impl RemoteHostProxy for FakeRemoteHostProxy {
    async fn ensure_remote_spawn(
        &self,
        host_id: &str,
        protocol: SessionProtocol,
        _agent_url: Option<&str>,
    ) -> Result<()> {
        if protocol.is_terminal() {
            bail!("terminal protocol cannot run on remote host `{host_id}`");
        }
        if *self.fail.lock().unwrap() {
            bail!("host `{host_id}` proxy failed (fake); session was not created");
        }
        *self.last_host.lock().unwrap() = Some(host_id.to_string());
        Ok(())
    }
}

/// Load registry from a ccteam home root and plan a spawn for `host`.
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

/// Full gate + proxy ensure. Returns the normalized host id to stamp on
/// the session (`"local"` or the satellite id).
pub async fn prepare_host_for_spawn(
    ccteam_root: Option<&Path>,
    host: &str,
    protocol: SessionProtocol,
    proxy: Option<&Arc<dyn RemoteHostProxy>>,
) -> Result<String> {
    let host = if host.is_empty() {
        LOCAL_HOST.to_string()
    } else {
        host.to_string()
    };
    if host == LOCAL_HOST {
        return Ok(LOCAL_HOST.to_string());
    }
    let Some(root) = ccteam_root else {
        bail!(
            "remote host `{host}` requested but daemon has no ccteam home \
             (host registry unavailable); session was not created"
        );
    };
    let plan = plan_spawn_host(root, &host, protocol)?;
    match plan {
        RemoteSpawnPlan::Local => Ok(LOCAL_HOST.to_string()),
        RemoteSpawnPlan::Remote { host_id } => {
            let reg = HostRegistry::load(&ccteam_core::host_registry::registry_path_in(root))?;
            let rec = reg
                .get(&host_id)
                .ok_or_else(|| anyhow::anyhow!("unknown host: {host_id}"))?;
            if let Some(proxy) = proxy {
                proxy
                    .ensure_remote_spawn(&host_id, protocol, rec.agent_url.as_deref())
                    .await?;
            } else {
                // Default production proxy.
                HttpRemoteHostProxy
                    .ensure_remote_spawn(&host_id, protocol, rec.agent_url.as_deref())
                    .await?;
            }
            Ok(host_id)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ccteam_core::host_registry::{now_unix, HostRecord};
    use tempfile::TempDir;

    #[tokio::test]
    async fn offline_host_errors_without_proxy_call() {
        let tmp = TempDir::new().unwrap();
        let mut reg = HostRegistry::default();
        reg.upsert(HostRecord {
            id: "sat".into(),
            hostname: "sat".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
            ccteam_version: "0.8.24".into(),
            agent_url: Some("http://127.0.0.1:9".into()),
            agent_token: "t".into(),
            last_heartbeat_unix: now_unix().saturating_sub(10_000),
            agents: vec![],
            joined_at: chrono::Utc::now().to_rfc3339(),
        });
        reg.save(&ccteam_core::host_registry::registry_path_in(tmp.path()))
            .unwrap();
        let fake = Arc::new(FakeRemoteHostProxy::default());
        let proxy: Arc<dyn RemoteHostProxy> = fake.clone();
        let err = prepare_host_for_spawn(
            Some(tmp.path()),
            "sat",
            SessionProtocol::StreamJson,
            Some(&proxy),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("offline"));
        assert!(fake.last_host.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn online_host_calls_fake_proxy() {
        let tmp = TempDir::new().unwrap();
        let mut reg = HostRegistry::default();
        reg.upsert(HostRecord {
            id: "sat".into(),
            hostname: "sat".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
            ccteam_version: "0.8.24".into(),
            agent_url: Some("http://127.0.0.1:9".into()),
            agent_token: "t".into(),
            last_heartbeat_unix: now_unix(),
            agents: vec![],
            joined_at: chrono::Utc::now().to_rfc3339(),
        });
        reg.save(&ccteam_core::host_registry::registry_path_in(tmp.path()))
            .unwrap();
        let fake = Arc::new(FakeRemoteHostProxy::default());
        let proxy: Arc<dyn RemoteHostProxy> = fake.clone();
        let host = prepare_host_for_spawn(
            Some(tmp.path()),
            "sat",
            SessionProtocol::StreamJson,
            Some(&proxy),
        )
        .await
        .unwrap();
        assert_eq!(host, "sat");
        assert_eq!(fake.last_host.lock().unwrap().as_deref(), Some("sat"));
    }
}
