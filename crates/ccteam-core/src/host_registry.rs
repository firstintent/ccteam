//! Multi-host registry (v0.8.24 Track D).
//!
//! Persists satellite hosts the main daemon has accepted via
//! `ccteam host join` / `POST /api/v1/hosts/join`. Local machine is
//! always implicit (`"local"`) and is never written here.
//!
//! **Honest scope**: join-token + heartbeat are an **ops registration
//! surface** (prevent accidental connect), not a security boundary.
//! Online/offline is TTL-based on last heartbeat. Remote spawn for
//! stdio protocols only (terminal never multi-host) is gated on this
//! registry; see `docs/dev/tech-design.md` §2.7.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::session_secret;

/// Default host-id for the machine running the main daemon.
pub const LOCAL_HOST: &str = "local";

/// A host is offline when no heartbeat arrives within this window.
pub const DEFAULT_HEARTBEAT_TTL_SECS: u64 = 90;

/// Wire shape of one probe row the satellite reports at join / heartbeat.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HostAgentReport {
    pub vendor: String,
    pub installed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default)]
    pub status: String,
}

/// One registered satellite (never `"local"`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HostRecord {
    /// Stable host id (hostname-derived slug, unique in the registry).
    pub id: String,
    /// OS hostname as reported at join.
    pub hostname: String,
    pub os: String,
    pub arch: String,
    /// ccteam version the satellite is running.
    pub ccteam_version: String,
    /// Optional agent callback base URL for remote spawn proxy
    /// (e.g. `http://192.168.1.10:7332`). Absent ⇒ registry-only (ops
    /// visibility) until the satellite advertises an endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_url: Option<String>,
    /// Long-lived agent credential minted at join (for heartbeat +
    /// future proxy auth). Stored as bare hex (no `ccteam:` prefix).
    pub agent_token: String,
    /// Unix seconds of the last successful join or heartbeat.
    pub last_heartbeat_unix: u64,
    /// Agent matrix last reported by the satellite.
    #[serde(default)]
    pub agents: Vec<HostAgentReport>,
    /// RFC3339 join time.
    pub joined_at: String,
}

impl HostRecord {
    /// Whether the host is considered online given `now` and TTL.
    pub fn is_online_at(&self, now_unix: u64, ttl_secs: u64) -> bool {
        now_unix.saturating_sub(self.last_heartbeat_unix) <= ttl_secs
    }

    pub fn is_online(&self, ttl_secs: u64) -> bool {
        self.is_online_at(now_unix(), ttl_secs)
    }

    pub fn status_label(&self, ttl_secs: u64) -> &'static str {
        if self.is_online(ttl_secs) {
            "online"
        } else {
            "offline"
        }
    }
}

/// On-disk registry file shape.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct HostRegistry {
    #[serde(default)]
    pub hosts: BTreeMap<String, HostRecord>,
}

impl HostRegistry {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(path)
            .with_context(|| format!("read host registry {}", path.display()))?;
        let reg: Self = serde_json::from_str(&raw)
            .with_context(|| format!("parse host registry {}", path.display()))?;
        Ok(reg)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create host registry dir {}", parent.display()))?;
        }
        let body = serde_json::to_string_pretty(self).context("serialize host registry")?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, body.as_bytes())
            .with_context(|| format!("write host registry tmp {}", tmp.display()))?;
        fs::rename(&tmp, path)
            .with_context(|| format!("rename host registry into place {}", path.display()))?;
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<&HostRecord> {
        self.hosts.get(id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut HostRecord> {
        self.hosts.get_mut(id)
    }

    /// Find a host by its long-lived agent token (constant-time).
    pub fn by_agent_token(&self, token: &str) -> Option<&HostRecord> {
        self.hosts
            .values()
            .find(|h| session_secret::ct_eq(&h.agent_token, token))
    }

    pub fn list(&self) -> impl Iterator<Item = &HostRecord> {
        self.hosts.values()
    }

    /// Insert or replace a host record.
    pub fn upsert(&mut self, record: HostRecord) {
        self.hosts.insert(record.id.clone(), record);
    }
}

// ── join tokens ──────────────────────────────────────────────────────────────

/// One admin-minted join token (single-use or multi-use until revoked).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JoinToken {
    /// Bare hex secret (no prefix).
    pub token: String,
    /// RFC3339 mint time.
    pub minted_at: String,
    /// Optional human label (e.g. "laptop-mac").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// When true, the token is no longer accepted.
    #[serde(default)]
    pub revoked: bool,
    /// Optional max uses; `None` = unlimited until revoked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_uses: Option<u32>,
    #[serde(default)]
    pub uses: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct JoinTokenStore {
    #[serde(default)]
    pub tokens: Vec<JoinToken>,
}

impl JoinTokenStore {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(path)
            .with_context(|| format!("read join-token store {}", path.display()))?;
        let store: Self = serde_json::from_str(&raw)
            .with_context(|| format!("parse join-token store {}", path.display()))?;
        Ok(store)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create join-token dir {}", parent.display()))?;
            // secrets dir should be 0700 when we own it
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
            }
        }
        let body = serde_json::to_string_pretty(self).context("serialize join-token store")?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, body.as_bytes())
            .with_context(|| format!("write join-token tmp {}", tmp.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
        }
        fs::rename(&tmp, path)
            .with_context(|| format!("rename join-token store {}", path.display()))?;
        Ok(())
    }

    /// Mint a fresh join token and persist.
    pub fn mint(&mut self, label: Option<String>, max_uses: Option<u32>) -> &JoinToken {
        let tok = JoinToken {
            token: session_secret::mint(),
            minted_at: chrono_now_rfc3339(),
            label,
            revoked: false,
            max_uses,
            uses: 0,
        };
        self.tokens.push(tok);
        self.tokens.last().expect("just pushed")
    }

    /// Validate + consume one use of a join token. Returns Ok(()) when valid.
    pub fn consume(&mut self, presented: &str) -> Result<()> {
        let bare = strip_ccteam_prefix(presented);
        let tok = self
            .tokens
            .iter_mut()
            .find(|t| session_secret::ct_eq(&t.token, bare))
            .ok_or_else(|| anyhow!("invalid join token"))?;
        if tok.revoked {
            bail!("join token revoked");
        }
        if let Some(max) = tok.max_uses {
            if tok.uses >= max {
                bail!("join token exhausted");
            }
        }
        tok.uses = tok.uses.saturating_add(1);
        Ok(())
    }

    /// Constant-time membership check (does not consume).
    pub fn contains_valid(&self, presented: &str) -> bool {
        let bare = strip_ccteam_prefix(presented);
        self.tokens.iter().any(|t| {
            session_secret::ct_eq(&t.token, bare)
                && !t.revoked
                && t.max_uses.map(|m| t.uses < m).unwrap_or(true)
        })
    }
}

// ── join / heartbeat helpers ─────────────────────────────────────────────────

/// Request body for `POST /hosts/join` (and CLI `ccteam host join`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostJoinRequest {
    /// Join token minted by the main daemon admin.
    pub token: String,
    /// Preferred host id; empty/omit → derived from hostname.
    #[serde(default)]
    pub host_id: Option<String>,
    pub hostname: String,
    pub os: String,
    pub arch: String,
    pub ccteam_version: String,
    #[serde(default)]
    pub agent_url: Option<String>,
    #[serde(default)]
    pub agents: Vec<HostAgentReport>,
}

/// Response after a successful join.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostJoinResponse {
    pub host: String,
    pub agent_token: String,
    pub heartbeat_ttl_secs: u64,
}

/// Heartbeat body (`POST /hosts/{host}/heartbeat`).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HostHeartbeatRequest {
    /// Agent token minted at join (also accepted via Authorization bearer).
    #[serde(default)]
    pub agent_token: Option<String>,
    #[serde(default)]
    pub agents: Option<Vec<HostAgentReport>>,
    #[serde(default)]
    pub agent_url: Option<String>,
    #[serde(default)]
    pub ccteam_version: Option<String>,
}

/// Apply a join request against the registry + token store. Caller persists both.
pub fn apply_join(
    reg: &mut HostRegistry,
    tokens: &mut JoinTokenStore,
    req: &HostJoinRequest,
) -> Result<HostJoinResponse> {
    tokens.consume(&req.token)?;
    let id = normalize_host_id(
        req.host_id
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(&req.hostname),
    )?;
    if id == LOCAL_HOST {
        bail!("host id `{LOCAL_HOST}` is reserved for the main daemon machine");
    }
    let agent_token = session_secret::mint();
    let now = now_unix();
    let record = HostRecord {
        id: id.clone(),
        hostname: req.hostname.clone(),
        os: req.os.clone(),
        arch: req.arch.clone(),
        ccteam_version: req.ccteam_version.clone(),
        agent_url: req.agent_url.clone(),
        agent_token: agent_token.clone(),
        last_heartbeat_unix: now,
        agents: req.agents.clone(),
        joined_at: chrono_now_rfc3339(),
    };
    reg.upsert(record);
    Ok(HostJoinResponse {
        host: id,
        agent_token,
        heartbeat_ttl_secs: DEFAULT_HEARTBEAT_TTL_SECS,
    })
}

/// Apply a heartbeat. Returns the updated record or an error.
pub fn apply_heartbeat(
    reg: &mut HostRegistry,
    host_id: &str,
    agent_token: &str,
    req: &HostHeartbeatRequest,
) -> Result<HostRecord> {
    if host_id == LOCAL_HOST {
        bail!("heartbeat is only for registered satellite hosts, not `{LOCAL_HOST}`");
    }
    let bare = strip_ccteam_prefix(agent_token);
    let host = reg
        .get_mut(host_id)
        .ok_or_else(|| anyhow!("unknown host: {host_id}"))?;
    if !session_secret::ct_eq(&host.agent_token, bare) {
        bail!("invalid agent token for host {host_id}");
    }
    host.last_heartbeat_unix = now_unix();
    if let Some(agents) = &req.agents {
        host.agents = agents.clone();
    }
    if let Some(url) = &req.agent_url {
        host.agent_url = if url.is_empty() {
            None
        } else {
            Some(url.clone())
        };
    }
    if let Some(ver) = &req.ccteam_version {
        if !ver.is_empty() {
            host.ccteam_version = ver.clone();
        }
    }
    Ok(host.clone())
}

/// Gate a session spawn against the host registry.
///
/// - `local` / empty → always ok
/// - terminal protocol on remote → hard reject (red line)
/// - unknown host → reject
/// - offline host → reject (session must NOT be created / deleted)
/// - online remote → ok (caller then proxies stdio spawn)
pub fn gate_remote_spawn(
    reg: &HostRegistry,
    host: &str,
    protocol_is_terminal: bool,
    ttl_secs: u64,
) -> Result<()> {
    let host = if host.is_empty() { LOCAL_HOST } else { host };
    if host == LOCAL_HOST {
        return Ok(());
    }
    if protocol_is_terminal {
        bail!(
            "terminal protocol cannot run on remote host `{host}` \
             (multi-host supports stdio protocols only: stream-json / acp)"
        );
    }
    let Some(rec) = reg.get(host) else {
        bail!("unknown host: {host}");
    };
    if !rec.is_online(ttl_secs) {
        bail!(
            "host `{host}` is offline (last heartbeat {}s ago; ttl {ttl_secs}s); \
             session was not created",
            now_unix().saturating_sub(rec.last_heartbeat_unix)
        );
    }
    Ok(())
}

/// Derive a stable host id from a hostname (slugify + refuse empty).
pub fn normalize_host_id(raw: &str) -> Result<String> {
    let s = raw.trim().to_ascii_lowercase();
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
            out.push(c);
        } else if (c == '.' || c == ' ') && !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        bail!("host id empty after normalize");
    }
    if out == LOCAL_HOST {
        bail!("host id `{LOCAL_HOST}` is reserved");
    }
    Ok(out)
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_secs()
}

fn chrono_now_rfc3339() -> String {
    // Avoid pulling chrono into this helper path shape if possible — but
    // the crate already depends on chrono. Use it for RFC3339 consistency
    // with SessionMeta timestamps.
    chrono::Utc::now().to_rfc3339()
}

fn strip_ccteam_prefix(presented: &str) -> &str {
    presented
        .strip_prefix("ccteam:")
        .unwrap_or(presented)
        .trim()
}

/// Path helpers relative to a ccteam home root.
pub fn registry_path_in(root: &Path) -> PathBuf {
    root.join("state").join("hosts").join("registry.json")
}

pub fn join_tokens_path_in(root: &Path) -> PathBuf {
    root.join("secrets").join("host-join-tokens.json")
}

/// Satellite-side self credentials after a successful join
/// (`~/.ccteam/state/hosts/self.json` on the satellite).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SatelliteSelf {
    pub daemon_url: String,
    pub host: String,
    pub agent_token: String,
    pub heartbeat_ttl_secs: u64,
    pub joined_at: String,
}

impl SatelliteSelf {
    pub fn path_in(root: &Path) -> PathBuf {
        root.join("state").join("hosts").join("self.json")
    }

    pub fn load(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("read satellite self {}", path.display()))?;
        Ok(serde_json::from_str(&raw)?)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let body = serde_json::to_string_pretty(self)?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, body.as_bytes())?;
        fs::rename(&tmp, path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn join_registers_and_heartbeat_keeps_online() {
        let tmp = TempDir::new().unwrap();
        let reg_path = registry_path_in(tmp.path());
        let tok_path = join_tokens_path_in(tmp.path());
        let mut tokens = JoinTokenStore::default();
        let minted = tokens.mint(Some("lab".into()), Some(1)).token.clone();
        tokens.save(&tok_path).unwrap();

        let mut reg = HostRegistry::default();
        let resp = apply_join(
            &mut reg,
            &mut tokens,
            &HostJoinRequest {
                token: minted,
                host_id: Some("lab-mac".into()),
                hostname: "lab-mac.local".into(),
                os: "macos".into(),
                arch: "aarch64".into(),
                ccteam_version: "0.8.24".into(),
                agent_url: Some("http://10.0.0.2:7332".into()),
                agents: vec![HostAgentReport {
                    vendor: "claude".into(),
                    installed: true,
                    version: Some("1.0".into()),
                    status: "ready".into(),
                }],
            },
        )
        .unwrap();
        reg.save(&reg_path).unwrap();
        tokens.save(&tok_path).unwrap();

        assert_eq!(resp.host, "lab-mac");
        let loaded = HostRegistry::load(&reg_path).unwrap();
        let h = loaded.get("lab-mac").unwrap();
        assert!(h.is_online(DEFAULT_HEARTBEAT_TTL_SECS));
        assert_eq!(h.agents.len(), 1);

        // Exhausted token cannot join again.
        let mut tokens2 = JoinTokenStore::load(&tok_path).unwrap();
        let mut reg2 = HostRegistry::load(&reg_path).unwrap();
        let err = apply_join(
            &mut reg2,
            &mut tokens2,
            &HostJoinRequest {
                token: resp.agent_token.clone(), // wrong kind of token
                host_id: None,
                hostname: "x".into(),
                os: "linux".into(),
                arch: "x86_64".into(),
                ccteam_version: "0".into(),
                agent_url: None,
                agents: vec![],
            },
        );
        assert!(err.is_err());

        // Heartbeat with agent token.
        let mut reg3 = HostRegistry::load(&reg_path).unwrap();
        let updated = apply_heartbeat(
            &mut reg3,
            "lab-mac",
            &resp.agent_token,
            &HostHeartbeatRequest {
                agent_token: Some(resp.agent_token.clone()),
                agents: None,
                agent_url: None,
                ccteam_version: Some("0.8.24-next".into()),
            },
        )
        .unwrap();
        assert_eq!(updated.ccteam_version, "0.8.24-next");
        reg3.save(&reg_path).unwrap();
    }

    #[test]
    fn offline_gate_rejects_without_deleting_host() {
        let mut reg = HostRegistry::default();
        reg.upsert(HostRecord {
            id: "dead".into(),
            hostname: "dead".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
            ccteam_version: "0.8.24".into(),
            agent_url: None,
            agent_token: "abc".into(),
            last_heartbeat_unix: now_unix().saturating_sub(10_000),
            agents: vec![],
            joined_at: chrono_now_rfc3339(),
        });
        let err = gate_remote_spawn(&reg, "dead", false, DEFAULT_HEARTBEAT_TTL_SECS).unwrap_err();
        assert!(
            err.to_string().contains("offline"),
            "expected offline error, got {err}"
        );
        // Host still in registry.
        assert!(reg.get("dead").is_some());
    }

    #[test]
    fn terminal_protocol_rejected_on_remote() {
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
            joined_at: chrono_now_rfc3339(),
        });
        let err = gate_remote_spawn(&reg, "sat", true, DEFAULT_HEARTBEAT_TTL_SECS).unwrap_err();
        assert!(err.to_string().contains("terminal"));
    }

    #[test]
    fn local_always_passes_gate() {
        let reg = HostRegistry::default();
        gate_remote_spawn(&reg, "local", true, DEFAULT_HEARTBEAT_TTL_SECS).unwrap();
        gate_remote_spawn(&reg, "", false, DEFAULT_HEARTBEAT_TTL_SECS).unwrap();
    }

    #[test]
    fn normalize_host_id_slugifies() {
        assert_eq!(normalize_host_id("Lab.Mac.local").unwrap(), "lab-mac-local");
        assert!(normalize_host_id("local").is_err());
    }
}
