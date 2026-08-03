//! Remote-host spawn gate + satellite exec proxy seam
//! (v0.8.24 Track D gate; v0.9.0 W3 F3 real execution; reverse-connection
//! since the network inversion — satellites dial in, no listener).
//!
//! Architecture (see `docs-local/versions/v0-9-0/tech-design.md` §4):
//! - Host registry (join + control-channel reports) is the ops
//!   registration surface; it lives ONLY on the main daemon.
//! - Session spawn/rebuild with `host != local` is gated: offline / unknown
//!   host / terminal protocol / the satellite not having `slug` registered
//!   → readable error (session **not** created, and — critically for G10 —
//!   an existing remote session is **never** silently respawned locally).
//! - Stdio protocols (stream-json / acp) proxy to the satellite through the
//!   daemon's [`HostChannelHub`]: the satellite keeps a live outbound
//!   `ccteam-host.v1` control channel; each spawn is an `exec_open`
//!   dial-back rendezvous (`ccteam-exec.v1` frames, re-exported from
//!   `ccteam-harness`). Production proxy ([`HubRemoteHostProxy`]) requires
//!   a live channel; tests inject a [`RemoteHostProxy`] that fakes (or
//!   in-process-duplexes) the handoff.

use std::sync::Arc;

use anyhow::{bail, Result};
use async_trait::async_trait;
use ccteam_core::host_registry::{
    gate_remote_spawn, gate_remote_spawn_project, HostRegistry, DEFAULT_HEARTBEAT_TTL_SECS,
    LOCAL_HOST,
};
use ccteam_harness::{
    AgentVendor, HostChannelHub, HostExecutionScope, RemoteExecTarget, SessionProtocol,
};
use std::path::Path;

/// Stable error returned when a v0.9.1 caller still supplies per-spawn host.
pub const HOST_SPAWN_PARAM_REMOVED: &str =
    "removed in v0.9.2: host is bound to the project; spawn into a project on that host instead";

/// Typed local-only rejection. Callers can downcast this through `anyhow`
/// without parsing display text, and no adapter/fallback is consulted.
#[derive(Debug, thiserror::Error)]
#[error("vendor `{vendor}` is local-only; project is bound to satellite host `{host}`; session was not created")]
pub struct RemoteVendorUnsupported {
    /// Lowercase vendor wire token.
    pub vendor: String,
    /// Satellite host bound to the project that rejected the local-only vendor.
    pub host: String,
}

/// Reject a vendor/host combination that ccteam cannot execute remotely.
/// Local and empty host ids are always accepted.
pub fn ensure_vendor_host_supported(vendor: AgentVendor, host: &str) -> Result<()> {
    if vendor.host_execution_scope() == HostExecutionScope::LocalOnly
        && !host.is_empty()
        && host != LOCAL_HOST
    {
        return Err(anyhow::Error::new(RemoteVendorUnsupported {
            vendor: vendor.wire_name().to_string(),
            host: host.to_string(),
        }));
    }
    Ok(())
}

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

/// Pluggable satellite exec proxy. Production requires a live reverse
/// control channel in the daemon's [`HostChannelHub`]; tests install a
/// fake that skips the liveness check (or wires an in-process hub).
#[async_trait]
pub trait RemoteHostProxy: Send + Sync {
    /// Ensure a remote stdio session can be started on `host_id` and return
    /// the exec target (host id + hub) to thread into `SpawnCtx::remote`.
    /// Returning `Ok` means the main daemon may proceed to track the
    /// session; `Err` is a readable user-facing failure — the session must
    /// not be inserted (and, on a rebuild, must stay stopped rather than
    /// silently respawn locally — G10).
    async fn ensure_remote_spawn(
        &self,
        host_id: &str,
        wire_slug: &str,
        protocol: SessionProtocol,
    ) -> Result<RemoteExecTarget>;
}

/// Production proxy: the satellite must have a LIVE `ccteam-host.v1`
/// control channel registered in the hub right now (instant, accurate
/// presence — no probe, no address: the satellite has no listener).
/// Failures are readable; offline-by-TTL / unregistered-slug were already
/// gated before this is called.
pub struct HubRemoteHostProxy {
    hub: Arc<HostChannelHub>,
}

impl HubRemoteHostProxy {
    /// Wrap the daemon's shared [`HostChannelHub`] (the same instance the
    /// web WS handlers register satellite connections into).
    pub fn new(hub: Arc<HostChannelHub>) -> Self {
        Self { hub }
    }
}

#[async_trait]
impl RemoteHostProxy for HubRemoteHostProxy {
    async fn ensure_remote_spawn(
        &self,
        host_id: &str,
        wire_slug: &str,
        protocol: SessionProtocol,
    ) -> Result<RemoteExecTarget> {
        if protocol.is_terminal() {
            bail!(
                "terminal protocol cannot run on remote host `{host_id}` \
                 (multi-host supports stdio only)"
            );
        }
        if !self.hub.is_connected(host_id) {
            bail!(
                "host `{host_id}` has no live control channel to this daemon \
                 (satellite disconnected?); session was not created"
            );
        }
        Ok(RemoteExecTarget {
            host_id: host_id.to_string(),
            wire_slug: wire_slug.to_string(),
            hub: self.hub.clone(),
        })
    }
}

/// Test / in-process proxy. Records the last host id for assertions;
/// `exec_target` lets a test point the returned [`RemoteExecTarget`] at an
/// in-process hub with a fake satellite attached (speaking real
/// `ccteam-exec.v1` over `ExecBridge` halves) — the "in-process duplex
/// mode" gateway e2e tests need. When unset, a dead hub is returned (fine
/// for gate-only tests whose adapter never dials).
#[derive(Default)]
pub struct FakeRemoteHostProxy {
    /// Last host id passed to [`RemoteHostProxy::ensure_remote_spawn`].
    pub last_host: std::sync::Mutex<Option<String>>,
    /// When true, `ensure_remote_spawn` returns an error.
    pub fail: std::sync::Mutex<bool>,
    /// Override the returned target (e.g. a hub with an in-process fake
    /// satellite registered).
    pub exec_target: std::sync::Mutex<Option<RemoteExecTarget>>,
}

#[async_trait]
impl RemoteHostProxy for FakeRemoteHostProxy {
    async fn ensure_remote_spawn(
        &self,
        host_id: &str,
        wire_slug: &str,
        protocol: SessionProtocol,
    ) -> Result<RemoteExecTarget> {
        if protocol.is_terminal() {
            bail!("terminal protocol cannot run on remote host `{host_id}`");
        }
        if *self.fail.lock().unwrap() {
            bail!("host `{host_id}` proxy failed (fake); session was not created");
        }
        *self.last_host.lock().unwrap() = Some(host_id.to_string());
        if let Some(mut target) = self.exec_target.lock().unwrap().clone() {
            target.wire_slug = wire_slug.to_string();
            return Ok(target);
        }
        Ok(RemoteExecTarget {
            host_id: host_id.to_string(),
            wire_slug: wire_slug.to_string(),
            hub: Arc::new(HostChannelHub::default()),
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
    vendor: AgentVendor,
    protocol: SessionProtocol,
    proxy: Option<&Arc<dyn RemoteHostProxy>>,
) -> Result<HostTarget> {
    let host = if host.is_empty() {
        LOCAL_HOST.to_string()
    } else {
        host.to_string()
    };
    ensure_vendor_host_supported(vendor, &host)?;
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
            // v0.9.0 W3 (G9) — the satellite must have ITS OWN copy of this
            // project registered (last reported over the control channel),
            // else a spawn there would fail `unknown-slug` at the exec
            // engine anyway — fail here with a more actionable message.
            gate_remote_spawn_project(&reg, &host_id, slug)?;
            let Some(proxy) = proxy else {
                // Reverse-connection model: remote exec REQUIRES the
                // daemon's host-channel hub (wired by `ccteam start`). A
                // gateway without one cannot reach any satellite.
                bail!(
                    "remote host `{host_id}` requested but this daemon has no \
                     host-channel hub (web/API disabled?); session was not created"
                );
            };
            let target = proxy.ensure_remote_spawn(&host_id, slug, protocol).await?;
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
    vendor: AgentVendor,
    protocol: SessionProtocol,
    proxy: Option<&Arc<dyn RemoteHostProxy>>,
) -> Result<Option<RemoteExecTarget>> {
    if host.is_empty() || host == LOCAL_HOST {
        return Ok(None);
    }
    let target = prepare_host_for_spawn(ccteam_root, host, slug, vendor, protocol, proxy).await?;
    Ok(target.remote)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ccteam_core::host_registry::{now_unix, HostRecord};
    use tempfile::TempDir;

    fn registered_sat(tmp: &TempDir, last_heartbeat_unix: u64) {
        let mut reg = HostRegistry::default();
        reg.upsert(HostRecord {
            id: "sat".into(),
            hostname: "sat".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
            ccteam_version: "0.9.0".into(),
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
        registered_sat(&tmp, now_unix().saturating_sub(10_000));
        let fake = Arc::new(FakeRemoteHostProxy::default());
        let proxy: Arc<dyn RemoteHostProxy> = fake.clone();
        let err = prepare_host_for_spawn(
            Some(tmp.path()),
            "sat",
            "demo",
            AgentVendor::Claude,
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
        registered_sat(&tmp, now_unix());
        let fake = Arc::new(FakeRemoteHostProxy::default());
        let proxy: Arc<dyn RemoteHostProxy> = fake.clone();
        let target = prepare_host_for_spawn(
            Some(tmp.path()),
            "sat",
            "demo",
            AgentVendor::Claude,
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
        assert_eq!(remote.host_id, "sat");
        assert_eq!(remote.wire_slug, "demo");
    }

    #[tokio::test]
    async fn unregistered_slug_rejects_even_when_host_online() {
        let tmp = TempDir::new().unwrap();
        // Registered host, but its `projects` list does not include "other".
        registered_sat(&tmp, now_unix());
        let fake = Arc::new(FakeRemoteHostProxy::default());
        let proxy: Arc<dyn RemoteHostProxy> = fake.clone();
        let err = prepare_host_for_spawn(
            Some(tmp.path()),
            "sat",
            "other",
            AgentVendor::Claude,
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
        assert!(regate_remote_host(
            None,
            "local",
            "demo",
            AgentVendor::Claude,
            SessionProtocol::StreamJson,
            None,
        )
        .await
        .unwrap()
        .is_none());
        assert!(regate_remote_host(
            None,
            "",
            "demo",
            AgentVendor::Claude,
            SessionProtocol::StreamJson,
            None,
        )
        .await
        .unwrap()
        .is_none());
    }

    #[tokio::test]
    async fn regate_remote_host_offline_errors_readable() {
        let tmp = TempDir::new().unwrap();
        registered_sat(&tmp, now_unix().saturating_sub(10_000));
        let fake = Arc::new(FakeRemoteHostProxy::default());
        let proxy: Arc<dyn RemoteHostProxy> = fake;
        let err = regate_remote_host(
            Some(tmp.path()),
            "sat",
            "demo",
            AgentVendor::Claude,
            SessionProtocol::StreamJson,
            Some(&proxy),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("offline"));
    }

    #[tokio::test]
    async fn pi_remote_spawn_is_typed_and_never_calls_the_proxy() {
        let fake = Arc::new(FakeRemoteHostProxy::default());
        let proxy: Arc<dyn RemoteHostProxy> = fake.clone();
        let err = prepare_host_for_spawn(
            None,
            "sat",
            "demo",
            AgentVendor::Pi,
            SessionProtocol::StreamJson,
            Some(&proxy),
        )
        .await
        .unwrap_err();
        let typed = err.downcast_ref::<RemoteVendorUnsupported>().unwrap();
        assert_eq!(typed.vendor, "pi");
        assert_eq!(typed.host, "sat");
        assert!(fake.last_host.lock().unwrap().is_none());
    }

    /// Production [`HubRemoteHostProxy`]: a live control channel in the hub
    /// yields a target; no channel is a readable failure (the satellite has
    /// no listener to probe — presence IS the channel).
    #[tokio::test]
    async fn hub_proxy_requires_a_live_control_channel() {
        let hub = Arc::new(HostChannelHub::default());
        let proxy = HubRemoteHostProxy::new(hub.clone());

        let err = proxy
            .ensure_remote_spawn("sat", "demo", SessionProtocol::StreamJson)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("no live control channel"), "got: {err}");

        let _reg = hub.register("sat");
        let target = proxy
            .ensure_remote_spawn("sat", "demo", SessionProtocol::StreamJson)
            .await
            .unwrap();
        assert_eq!(target.host_id, "sat");

        let err = proxy
            .ensure_remote_spawn("sat", "demo", SessionProtocol::Terminal)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("terminal"), "got: {err}");
    }
}
