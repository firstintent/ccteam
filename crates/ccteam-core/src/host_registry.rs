//! Multi-host registry (v0.8.24 Track D; reverse-connection since v0.9.0).
//!
//! Persists satellite hosts the main daemon has accepted via
//! `ccteam host join` / `POST /api/v1/hosts/join`. Local machine is
//! always implicit (`"local"`) and is never written here. Registration
//! state lives ONLY on the main daemon — a satellite keeps just its own
//! [`SatelliteSelf`] credentials and dials out (`ccteam-host.v1` control
//! channel); it exposes no listener and stores no peer registry.
//!
//! **Honest scope**: join-token + agent-token are an **ops registration
//! surface** (prevent accidental connect), not a security boundary.
//! Online/offline is TTL-based on the last `report` frame received over
//! the satellite's control channel (live-channel presence gates the
//! actual exec dial); terminal protocol is never multi-host. See
//! `docs/dev/tech-design.md` §2.7.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::session_secret;

/// Default host-id for the machine running the main daemon.
pub const LOCAL_HOST: &str = "local";

/// A host is offline when no control-channel `report` frame arrives within
/// this window (satellites report every ~25s — `REPORT_PERIOD` in
/// `ccteam-harness::host_channel`).
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

/// v0.9.0 W3 (G9) — one project the satellite has registered locally
/// (its OWN `~/.ccteam/config.yaml::projects[]`), reported at heartbeat so
/// the main daemon's remote-spawn gate can tell whether a slug is actually
/// usable on that host before proxying a spawn there.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HostProjectReport {
    pub slug: String,
    pub path: String,
}

/// One vendor this host can run + the env override / default binary name
/// used to resolve it. **The single source of truth for "which vendors
/// ccteam knows how to probe"** — shared by every consumer so a sixth
/// vendor is one edit here, never a scatter of parallel tables:
/// - the satellite report loop (`ccteam-web::satellite`, over the control
///   channel) via [`probe_agents`];
/// - `ccteam-web::routes::{hosts,capabilities}` (wrap [`probe_bin_cached`]
///   for the LOCAL admin host-detail / capability matrix, plus the
///   tool-surface state keyed off [`Self::tool_surface`]);
/// - the MCP `status` vendor panel + `session_spawn` availability discovery
///   (`ccteam-im`) via [`probe_availability`].
#[derive(Debug, Clone, Copy)]
pub struct AgentProbeSpec {
    /// Stable vendor token (`claude` / `codex`) — matches `AgentVendor`'s
    /// lowercase serde form and what `POST .../sessions` accepts.
    pub vendor: &'static str,
    /// Harness id label (`claude-code` / `codex`).
    pub harness_id: &'static str,
    /// Env override for the binary path (`CCTEAM_CLAUDE_BIN` / `_CODEX_BIN`).
    pub bin_env: &'static str,
    /// Default binary name resolved on `PATH` when the env override is unset.
    pub default_bin: &'static str,
    /// How ccteam exposes its eight tools to this vendor.
    pub tool_surface: ToolSurfaceMode,
    /// Admin one-click install/update recipe (VENDOR-INSTALL-1): the exact
    /// argv the daemon runs via `std::process::Command::new(argv[0])
    /// .args(&argv[1..])` — NEVER through a shell, and the package name is
    /// pinned here in the table, never taken from a request. `None` = no
    /// supported recipe (kimi/pi install manually, see
    /// `manual_install_url`).
    pub install_recipe: Option<&'static [&'static str]>,
    /// Manual-install docs link shown when there is no recipe. `None` for
    /// recipe-backed vendors (their recipe IS the guidance).
    pub manual_install_url: Option<&'static str>,
    /// VENDOR-QUOTA-1 — which subscription-quota probe this vendor has, if
    /// any (`ccteam_core::vendor_quota`; implementations in
    /// `ccteam_web::routes::vendor_quota`). `None` = no surface, the UI
    /// renders nothing (opencode/pi/dsh).
    pub quota_probe: Option<crate::vendor_quota::QuotaProbeKind>,
}

/// Where a vendor receives ccteam's tool surface. Adding a future vendor
/// without native MCP configuration requires one honest enum choice here;
/// generic config/status consumers then inherit the right behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolSurfaceMode {
    /// ccteam writes its own MCP entry to the vendor's global config.
    NativeMcpConfig,
    /// Only ccteam-managed sessions receive the runtime bridge.
    ManagedSessionBridge,
}

impl ToolSurfaceMode {
    pub const fn uses_native_mcp_config(self) -> bool {
        matches!(self, Self::NativeMcpConfig)
    }
}

/// Every vendor harness ccteam probes. Extend here to add one.
pub const AGENT_PROBE_SPECS: &[AgentProbeSpec] = &[
    AgentProbeSpec {
        vendor: "claude",
        harness_id: "claude-code",
        bin_env: ccteam_harness::CLAUDE_BIN_ENV,
        default_bin: "claude",
        tool_surface: ToolSurfaceMode::NativeMcpConfig,
        install_recipe: Some(&["npm", "install", "-g", "@anthropic-ai/claude-code@latest"]),
        manual_install_url: None,
        quota_probe: Some(crate::vendor_quota::QuotaProbeKind::ClaudeOauthUsage),
    },
    AgentProbeSpec {
        vendor: "codex",
        harness_id: "codex",
        bin_env: ccteam_harness::CODEX_BIN_ENV,
        default_bin: "codex",
        tool_surface: ToolSurfaceMode::NativeMcpConfig,
        install_recipe: Some(&["npm", "install", "-g", "@openai/codex@latest"]),
        manual_install_url: None,
        quota_probe: Some(crate::vendor_quota::QuotaProbeKind::CodexWhamUsage),
    },
    AgentProbeSpec {
        vendor: "grok",
        harness_id: "grok",
        bin_env: ccteam_harness::GROK_BIN_ENV,
        default_bin: "grok",
        tool_surface: ToolSurfaceMode::NativeMcpConfig,
        install_recipe: Some(&["npm", "install", "-g", "@xai-official/grok@latest"]),
        manual_install_url: None,
        quota_probe: Some(crate::vendor_quota::QuotaProbeKind::GrokBillingUnavailable),
    },
    AgentProbeSpec {
        vendor: "opencode",
        harness_id: "opencode",
        bin_env: ccteam_harness::OPENCODE_BIN_ENV,
        default_bin: "opencode",
        tool_surface: ToolSurfaceMode::NativeMcpConfig,
        install_recipe: Some(&["npm", "install", "-g", "opencode-ai@latest"]),
        manual_install_url: None,
        quota_probe: None,
    },
    AgentProbeSpec {
        vendor: "kimi",
        harness_id: "kimi",
        bin_env: ccteam_harness::KIMI_BIN_ENV,
        default_bin: "kimi",
        // Kimi has a config-file MCP seam: `$KIMI_CODE_HOME/mcp.json`.
        tool_surface: ToolSurfaceMode::NativeMcpConfig,
        install_recipe: None,
        manual_install_url: Some("https://moonshotai.github.io/kimi-code/"),
        quota_probe: Some(crate::vendor_quota::QuotaProbeKind::KimiManagedUsages),
    },
    AgentProbeSpec {
        vendor: "pi",
        harness_id: "pi",
        bin_env: ccteam_harness::PI_BIN_ENV,
        default_bin: "pi",
        tool_surface: ToolSurfaceMode::ManagedSessionBridge,
        install_recipe: None,
        manual_install_url: Some("https://pi.dev/"),
        quota_probe: None,
    },
    AgentProbeSpec {
        vendor: "dsh",
        harness_id: "dsh",
        bin_env: ccteam_harness::DSH_BIN_ENV,
        // The product CLI (`dsh`, whose web runtime ccteam connects to over
        // the plugin's unix socket), not a demo/dev binary.
        default_bin: "dsh",
        tool_surface: ToolSurfaceMode::ManagedSessionBridge,
        install_recipe: Some(&["npm", "install", "-g", "@deepseek-ai/dsh@latest"]),
        manual_install_url: None,
        quota_probe: None,
    },
];

impl AgentProbeSpec {
    /// Look up a probe spec by its vendor token (`claude` / `codex` / …).
    pub fn by_vendor(vendor: &str) -> Option<&'static AgentProbeSpec> {
        AGENT_PROBE_SPECS.iter().find(|s| s.vendor == vendor)
    }

    /// Explains the `Pass`-with-no-config-file state a `ManagedSessionBridge`
    /// vendor always reports: unlike the five `NativeMcpConfig` vendors,
    /// there is no global config entry to point at, by design (Pi has no
    /// MCP seam at all; DSH's official ACP demo hard-rejects a non-empty
    /// `mcpServers`) — so this leads with *why* before the caveat, instead
    /// of reading like a warning bolted onto an otherwise-green row.
    pub fn tool_surface_notice(&self) -> Option<String> {
        if self.tool_surface != ToolSurfaceMode::ManagedSessionBridge {
            return None;
        }
        // DSH's tool face is a Cordis plugin, not a ccteam-owned bridge
        // extension (K2/K3) — the notice says so, and names the exact
        // install command instead of just the package (a plain-vendor DSH
        // session CAN unlock the tools; a plain-vendor Pi session cannot).
        if self.vendor == "dsh" {
            return Some(
                "DSH has no ccteam-writable config file: its ccteam surface is the \
                 `@ccteam/ccteam-client` plugin inside your DSH web runtime. ccteam preloads \
                 it for the runtimes it manages; for a `dsh web` you start yourself, use \
                 Register here (or `dsh plugin --profile web add @ccteam/ccteam-client`) and \
                 restart that instance."
                    .to_string(),
            );
        }
        let mut chars = self.vendor.chars();
        let display = chars
            .next()
            .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
            .unwrap_or_default();
        Some(format!(
            "{display} has no ccteam-writable config file: managed sessions get the ccteam \
             bridge automatically; a `{}` you start yourself does not, with no install step \
             to add it.",
            self.default_bin
        ))
    }
}

/// Resolve a vendor's binary path: the `CCTEAM_*_BIN` override, else the
/// `PATH` name — **except `dsh`**, which additionally falls back to a
/// cached `npx` copy (`ccteam_harness::resolve_dsh_default_bin` — DSH's own
/// resolution logic lives with its adapter; this crate already depends on
/// `ccteam-harness` for real, so it consumes rather than re-implements it).
/// The single resolver every probe call site shares.
pub fn resolve_bin(spec: &AgentProbeSpec) -> String {
    std::env::var(spec.bin_env).unwrap_or_else(|_| {
        if spec.vendor == "dsh" {
            ccteam_harness::resolve_dsh_default_bin()
        } else {
            spec.default_bin.to_string()
        }
    })
}

/// Whether a vendor binary is resolvable without executing it.
///
/// An explicit `CCTEAM_*_BIN` override is authoritative and counts when that
/// path exists. Otherwise the default binary name is scanned on `PATH`; Unix
/// candidates must be regular files with at least one executable bit. `dsh`
/// additionally counts a cached `npx` copy
/// (`ccteam_harness::find_cached_dsh_bin`).
pub fn bin_resolvable(spec: &AgentProbeSpec) -> bool {
    if let Some(path) = std::env::var_os(spec.bin_env) {
        return path_is_executable(std::path::Path::new(&path));
    }
    let on_path = std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| path_is_executable(&dir.join(spec.default_bin)))
    });
    if on_path {
        return true;
    }
    spec.vendor == "dsh" && ccteam_harness::find_cached_dsh_bin().is_some()
}

fn path_is_executable(path: &std::path::Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// One vendor's availability on THIS machine — the normalized snapshot the
/// MCP `status` panel and the `session_spawn` discovery error render from.
/// Deliberately carries only machine facts (`installed` + `--version`);
/// auth / budget / host-online are layered on by the panel from other
/// sources, never faked here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VendorAvailability {
    pub vendor: &'static str,
    pub harness_id: &'static str,
    pub installed: bool,
    pub version: Option<String>,
}

/// One cached probe result (`installed`, first `--version` line).
type ProbeCacheEntry = (bool, Option<String>);

/// Process-lifetime probe cache keyed by resolved binary path (not vendor,
/// so a test pointing `CCTEAM_*_BIN` at a fake script gets an independent
/// entry). A `refresh` probe bypasses + overwrites the entry — the manual
/// re-probe that breaks the daemon-lifetime cache when a vendor is installed
/// after the daemon started.
fn probe_cache() -> &'static std::sync::Mutex<BTreeMap<String, ProbeCacheEntry>> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<BTreeMap<String, ProbeCacheEntry>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(BTreeMap::new()))
}

/// Cached wrapper over [`probe_bin_version`]: probe `<bin> --version` once
/// per process (per resolved path), returning the cached result on
/// subsequent calls. `refresh` bypasses the cache and re-runs, overwriting
/// the entry. The satellite report loop calls the UNcached
/// [`probe_bin_version`] (fresh every beat); the daemon-resident consumers
/// (web host page, MCP status panel, spawn discovery) use this so the common
/// case never re-spawns a child.
pub fn probe_bin_cached(bin: &str, refresh: bool) -> (bool, Option<String>) {
    if !refresh {
        if let Ok(cache) = probe_cache().lock() {
            if let Some(hit) = cache.get(bin) {
                return hit.clone();
            }
        }
    }
    let result = probe_bin_version(bin);
    if let Ok(mut cache) = probe_cache().lock() {
        cache.insert(bin.to_string(), result.clone());
    }
    result
}

/// Probe every [`AGENT_PROBE_SPECS`] vendor on THIS machine (cached), folding
/// to the [`VendorAvailability`] snapshot. Blocking (shells out once per
/// vendor on a cold cache) — call via `spawn_blocking` from async. `refresh`
/// forces a re-probe of every vendor.
pub fn probe_availability(refresh: bool) -> Vec<VendorAvailability> {
    AGENT_PROBE_SPECS
        .iter()
        .map(|spec| {
            let bin = resolve_bin(spec);
            let (installed, version) = probe_bin_cached(&bin, refresh);
            VendorAvailability {
                vendor: spec.vendor,
                harness_id: spec.harness_id,
                installed,
                version,
            }
        })
        .collect()
}

/// Resolve + run `<bin> --version`; any spawn error (binary not on PATH)
/// folds to not-installed. The single shellout impl — both the web
/// host-detail probe (cached) and the satellite heartbeat loop (uncached,
/// one-shot per beat) reduce to this.
pub fn probe_bin_version(bin: &str) -> (bool, Option<String>) {
    match std::process::Command::new(bin)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
    {
        Ok(out) if out.status.success() => {
            let version = String::from_utf8_lossy(&out.stdout)
                .lines()
                .next()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            (true, version)
        }
        _ => (false, None),
    }
}

/// Probe every [`AGENT_PROBE_SPECS`] vendor on THIS machine and fold to the
/// wire [`HostAgentReport`] shape. Blocking (shells out per vendor) — call
/// via `spawn_blocking` from an async context (the satellite heartbeat
/// loop does). `status` is `ready` / `not_installed`: a satellite has no
/// MCP-registration concept (that is the `local`-only web host page's
/// concern), so it never reports `needs_config`.
pub fn probe_agents() -> Vec<HostAgentReport> {
    AGENT_PROBE_SPECS
        .iter()
        .map(|spec| {
            let bin = std::env::var(spec.bin_env).unwrap_or_else(|_| spec.default_bin.to_string());
            let (installed, version) = probe_bin_version(&bin);
            HostAgentReport {
                vendor: spec.vendor.to_string(),
                installed,
                version,
                status: if installed { "ready" } else { "not_installed" }.to_string(),
            }
        })
        .collect()
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
    /// Long-lived agent credential minted at join. The bearer for the
    /// satellite's outbound `ccteam-host.v1` control channel and its exec
    /// dial-backs (the satellite itself exposes NO listener — v0.9.0
    /// reverse connection). Stored as bare hex (no `ccteam:` prefix).
    pub agent_token: String,
    /// Unix seconds of the last successful join or control-channel report.
    pub last_heartbeat_unix: u64,
    /// Agent matrix last reported by the satellite.
    #[serde(default)]
    pub agents: Vec<HostAgentReport>,
    /// v0.9.0 W3 (G9) — projects the satellite has registered locally
    /// (its own `~/.ccteam/config.yaml::projects[]`), last reported at
    /// heartbeat. Empty until the first heartbeat with a non-`None`
    /// `projects` field lands.
    #[serde(default)]
    pub projects: Vec<HostProjectReport>,
    /// RFC3339 join time.
    pub joined_at: String,
}

impl HostRecord {
    /// Whether `slug` is registered as a project on this host (per its last
    /// heartbeat report). Used by the remote-spawn gate (G9/G10): an
    /// online-but-unregistered slug must fail readable, never silently
    /// spawn/rebuild on the main daemon.
    pub fn has_project(&self, slug: &str) -> bool {
        self.projects.iter().any(|p| p.slug == slug)
    }
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
            // v0.9.0 W3 (G13) — the registry holds every satellite's
            // long-lived agent_token; harden the directory like
            // `JoinTokenStore` does.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
            }
        }
        let body = serde_json::to_string_pretty(self).context("serialize host registry")?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, body.as_bytes())
            .with_context(|| format!("write host registry tmp {}", tmp.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
        }
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

    /// Remove a host record (deregistration). Returns the removed record, or
    /// `None` if unknown. Caller persists via `save`.
    pub fn remove(&mut self, id: &str) -> Option<HostRecord> {
        self.hosts.remove(id)
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
    pub agents: Vec<HostAgentReport>,
}

/// Response after a successful join.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostJoinResponse {
    pub host: String,
    pub agent_token: String,
    pub heartbeat_ttl_secs: u64,
}

/// Satellite status report — the body of the `{"op":"report", …}` frame a
/// satellite pushes over its `ccteam-host.v1` control channel every
/// `REPORT_PERIOD` (and immediately on connect). Auth is the channel's
/// bearer (already verified before any report is applied) — no in-body
/// token.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HostReport {
    #[serde(default)]
    pub agents: Option<Vec<HostAgentReport>>,
    #[serde(default)]
    pub ccteam_version: Option<String>,
    /// v0.9.0 W3 (G9) — the satellite's own registered projects
    /// (`[{slug, path}]`). `None` leaves the last-known list untouched;
    /// `Some` (including `Some(vec![])`) replaces it.
    #[serde(default)]
    pub projects: Option<Vec<HostProjectReport>>,
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
        agent_token: agent_token.clone(),
        last_heartbeat_unix: now,
        agents: req.agents.clone(),
        projects: vec![],
        joined_at: chrono_now_rfc3339(),
    };
    reg.upsert(record);
    Ok(HostJoinResponse {
        host: id,
        agent_token,
        heartbeat_ttl_secs: DEFAULT_HEARTBEAT_TTL_SECS,
    })
}

/// Apply a control-channel `report` frame. The caller has ALREADY
/// authenticated the channel's agent-token bearer to `host_id` — no in-body
/// token to re-verify. Returns the updated record or an error.
pub fn apply_report(reg: &mut HostRegistry, host_id: &str, req: &HostReport) -> Result<HostRecord> {
    if host_id == LOCAL_HOST {
        bail!("reports are only for registered satellite hosts, not `{LOCAL_HOST}`");
    }
    let host = reg
        .get_mut(host_id)
        .ok_or_else(|| anyhow!("unknown host: {host_id}"))?;
    host.last_heartbeat_unix = now_unix();
    if let Some(agents) = &req.agents {
        host.agents = agents.clone();
    }
    if let Some(projects) = &req.projects {
        host.projects = projects.clone();
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

/// v0.9.0 W3 (G9) — gate a remote spawn/rebuild against the satellite's
/// OWN registered project set (last reported at heartbeat). `local` /
/// empty host always passes (nothing to check). An online, registered
/// host that has never reported `slug` in its `projects` list fails
/// readable — the session must not be created/rebuilt.
///
/// Callers run [`gate_remote_spawn`] first (offline / terminal / unknown
/// host) and only reach here once that has already passed.
pub fn gate_remote_spawn_project(reg: &HostRegistry, host: &str, slug: &str) -> Result<()> {
    let host = if host.is_empty() { LOCAL_HOST } else { host };
    if host == LOCAL_HOST {
        return Ok(());
    }
    let Some(rec) = reg.get(host) else {
        bail!("unknown host: {host}");
    };
    if !rec.has_project(slug) {
        bail!(
            "project `{slug}` is not registered on host `{host}`; run `ccteam init` \
             for it there (or open it in that satellite's `~/.ccteam/config.yaml`) \
             and wait for the next heartbeat, then retry"
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
                agents: vec![],
            },
        );
        assert!(err.is_err());

        // Report over the (pre-authed) control channel.
        let mut reg3 = HostRegistry::load(&reg_path).unwrap();
        let updated = apply_report(
            &mut reg3,
            "lab-mac",
            &HostReport {
                agents: None,
                ccteam_version: Some("0.8.24-next".into()),
                projects: None,
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
            agent_token: "abc".into(),
            last_heartbeat_unix: now_unix().saturating_sub(10_000),
            agents: vec![],
            projects: vec![],
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
            agent_token: "t".into(),
            last_heartbeat_unix: now_unix(),
            agents: vec![],
            projects: vec![],
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
    fn remove_deregisters_a_known_host_and_is_none_for_unknown() {
        let mut reg = HostRegistry::default();
        reg.upsert(HostRecord {
            id: "sat".into(),
            hostname: "sat".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
            ccteam_version: "0.9.11".into(),
            agent_token: "t".into(),
            last_heartbeat_unix: now_unix(),
            agents: vec![],
            projects: vec![],
            joined_at: chrono_now_rfc3339(),
        });
        assert!(reg.get("sat").is_some());

        let removed = reg.remove("sat").expect("known host removed");
        assert_eq!(removed.id, "sat");
        assert!(reg.get("sat").is_none());

        // Removing an unknown id is a no-op that returns None.
        assert!(reg.remove("sat").is_none());
        assert!(reg.remove("never-existed").is_none());
    }

    #[test]
    fn normalize_host_id_slugifies() {
        assert_eq!(normalize_host_id("Lab.Mac.local").unwrap(), "lab-mac-local");
        assert!(normalize_host_id("local").is_err());
    }

    fn sat_with_projects(projects: Vec<HostProjectReport>) -> HostRegistry {
        let mut reg = HostRegistry::default();
        reg.upsert(HostRecord {
            id: "sat".into(),
            hostname: "sat".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
            ccteam_version: "0.9.0".into(),
            agent_token: "t".into(),
            last_heartbeat_unix: now_unix(),
            agents: vec![],
            projects,
            joined_at: chrono_now_rfc3339(),
        });
        reg
    }

    #[test]
    fn gate_remote_spawn_project_passes_for_registered_slug() {
        let reg = sat_with_projects(vec![HostProjectReport {
            slug: "demo".into(),
            path: "/home/sat/projects/demo".into(),
        }]);
        gate_remote_spawn_project(&reg, "sat", "demo").unwrap();
    }

    #[test]
    fn gate_remote_spawn_project_rejects_unregistered_slug() {
        let reg = sat_with_projects(vec![HostProjectReport {
            slug: "other".into(),
            path: "/home/sat/projects/other".into(),
        }]);
        let err = gate_remote_spawn_project(&reg, "sat", "demo").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("not registered"), "got: {msg}");
        assert!(msg.contains("ccteam init"), "got: {msg}");
    }

    #[test]
    fn gate_remote_spawn_project_local_always_passes() {
        let reg = HostRegistry::default();
        gate_remote_spawn_project(&reg, "local", "anything").unwrap();
        gate_remote_spawn_project(&reg, "", "anything").unwrap();
    }

    #[test]
    fn report_merges_projects_when_present() {
        let tmp = TempDir::new().unwrap();
        let reg_path = registry_path_in(tmp.path());
        let mut reg = sat_with_projects(vec![]);
        reg.save(&reg_path).unwrap();

        let updated = apply_report(
            &mut reg,
            "sat",
            &HostReport {
                agents: None,
                ccteam_version: None,
                projects: Some(vec![HostProjectReport {
                    slug: "demo".into(),
                    path: "/home/sat/projects/demo".into(),
                }]),
            },
        )
        .unwrap();
        assert!(updated.has_project("demo"));

        // `projects: None` on a later report must NOT wipe the list.
        let updated2 = apply_report(
            &mut reg,
            "sat",
            &HostReport {
                agents: None,
                ccteam_version: None,
                projects: None,
            },
        )
        .unwrap();
        assert!(updated2.has_project("demo"));
    }

    #[cfg(unix)]
    #[test]
    fn registry_save_hardens_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        let reg_path = registry_path_in(tmp.path());
        let reg = sat_with_projects(vec![]);
        reg.save(&reg_path).unwrap();
        let file_mode = fs::metadata(&reg_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(file_mode, 0o600, "registry.json must be 0600");
        let dir_mode = fs::metadata(reg_path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700, "state/hosts/ dir must be 0700");
    }

    #[test]
    fn probe_bin_version_missing_binary_is_not_installed() {
        // No env mutation (CLAUDE.md §六 pitfall) — probe a path directly.
        let (installed, version) = probe_bin_version("/nonexistent/ccteam-fake-zzz");
        assert!(!installed);
        assert!(version.is_none());
    }

    #[test]
    fn agent_probe_specs_covers_seven_vendors() {
        let vendors: Vec<&str> = AGENT_PROBE_SPECS.iter().map(|s| s.vendor).collect();
        assert_eq!(
            vendors,
            vec!["claude", "codex", "grok", "opencode", "kimi", "pi", "dsh"]
        );
    }

    #[test]
    fn agent_probe_specs_carry_harness_id_and_tool_surface_mode() {
        // The web host page + capabilities matrix now read these off the
        // shared spec (no parallel table). Native vendors share one mode;
        // managed Pi names the bridge mode explicitly.
        let by = |v: &str| AgentProbeSpec::by_vendor(v).unwrap();
        assert_eq!(by("claude").harness_id, "claude-code");
        assert_eq!(by("pi").harness_id, "pi");
        assert_eq!(by("pi").tool_surface, ToolSurfaceMode::ManagedSessionBridge);
        assert_eq!(by("dsh").harness_id, "dsh");
        assert_eq!(
            by("dsh").tool_surface,
            ToolSurfaceMode::ManagedSessionBridge
        );
        assert_eq!(
            by("dsh").tool_surface_notice().as_deref(),
            Some(
                "DSH has no ccteam-writable config file: its ccteam surface is the \
                 `@ccteam/ccteam-client` plugin inside your DSH web runtime. ccteam preloads \
                 it for the runtimes it manages; for a `dsh web` you start yourself, use \
                 Register here (or `dsh plugin --profile web add @ccteam/ccteam-client`) and \
                 restart that instance."
            )
        );
        for spec in AGENT_PROBE_SPECS {
            match spec.tool_surface {
                ToolSurfaceMode::NativeMcpConfig => {
                    assert!(spec.tool_surface_notice().is_none());
                }
                ToolSurfaceMode::ManagedSessionBridge => {
                    let notice = spec.tool_surface_notice().unwrap();
                    assert!(notice.contains(spec.default_bin), "{spec:?}: {notice}");
                }
            }
        }
        let future = AgentProbeSpec {
            vendor: "future",
            harness_id: "future",
            bin_env: "CCTEAM_FUTURE_BIN",
            default_bin: "future",
            tool_surface: ToolSurfaceMode::ManagedSessionBridge,
            install_recipe: None,
            manual_install_url: None,
            quota_probe: None,
        };
        assert_eq!(
            future.tool_surface_notice().as_deref(),
            Some(
                "Future has no ccteam-writable config file: managed sessions get the ccteam \
                 bridge automatically; a `future` you start yourself does not, with no install \
                 step to add it."
            )
        );
        assert!(AgentProbeSpec::by_vendor("gemini").is_none());
    }

    #[test]
    fn path_resolver_requires_a_file_and_unix_exec_bit() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("vendor-bin");
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&path).unwrap().permissions();
            permissions.set_mode(0o644);
            std::fs::set_permissions(&path, permissions.clone()).unwrap();
            assert!(!path_is_executable(&path));
            permissions.set_mode(0o755);
            std::fs::set_permissions(&path, permissions).unwrap();
        }
        assert!(path_is_executable(&path));
        assert!(!path_is_executable(tmp.path()));
        assert!(!path_is_executable(&tmp.path().join("missing")));
    }

    #[test]
    fn probe_bin_cached_missing_binary_is_not_installed() {
        // A path that cannot exist → not installed, both cold + cached read.
        let cold = probe_bin_cached("/nonexistent/ccteam-fake-cache-zzz", true);
        assert!(!cold.0 && cold.1.is_none());
        let warm = probe_bin_cached("/nonexistent/ccteam-fake-cache-zzz", false);
        assert_eq!(cold, warm);
    }

    #[test]
    fn probe_availability_returns_one_row_per_spec() {
        // No env mutation: whatever the host has installed, the SHAPE holds —
        // one snapshot row per probe spec, vendor tokens in registry order.
        let avail = probe_availability(true);
        assert_eq!(avail.len(), AGENT_PROBE_SPECS.len());
        let vendors: Vec<&str> = avail.iter().map(|a| a.vendor).collect();
        assert_eq!(
            vendors,
            vec!["claude", "codex", "grok", "opencode", "kimi", "pi", "dsh"]
        );
        // harness_id is carried through from the spec.
        assert_eq!(avail[0].harness_id, "claude-code");
    }

    #[test]
    fn install_recipes_are_exact_argv_and_shell_free() {
        // The npm-backed vendors pin their exact recipe argv in the table.
        let expected: &[(&str, &[&str])] = &[
            (
                "claude",
                &["npm", "install", "-g", "@anthropic-ai/claude-code@latest"],
            ),
            ("codex", &["npm", "install", "-g", "@openai/codex@latest"]),
            (
                "grok",
                &["npm", "install", "-g", "@xai-official/grok@latest"],
            ),
            ("opencode", &["npm", "install", "-g", "opencode-ai@latest"]),
            ("dsh", &["npm", "install", "-g", "@deepseek-ai/dsh@latest"]),
        ];
        for (vendor, argv) in expected {
            let spec = AgentProbeSpec::by_vendor(vendor).unwrap();
            assert_eq!(spec.install_recipe, Some(*argv), "{vendor}");
            assert!(spec.manual_install_url.is_none(), "{vendor}");
        }
        // Every recipe executes `npm` directly as argv[0] (no shell wrapper)
        // and every token is a bare word — no metacharacter a shell could
        // reinterpret, so exec-without-shell is semantics-preserving.
        for spec in AGENT_PROBE_SPECS {
            let Some(argv) = spec.install_recipe else {
                continue;
            };
            assert_eq!(argv[0], "npm", "{spec:?}");
            assert!(argv.len() >= 2, "{spec:?}");
            for token in argv {
                assert!(
                    token
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "-._/@".contains(c)),
                    "{spec:?}: unsafe token {token:?}"
                );
            }
        }
    }

    #[test]
    fn install_recipe_absent_means_manual_with_a_link() {
        // kimi/pi have no supported recipe: the UI keeps manual guidance and
        // the API 400s the install endpoint. The link must be present so the
        // hint can point at real docs.
        for vendor in ["kimi", "pi"] {
            let spec = AgentProbeSpec::by_vendor(vendor).unwrap();
            assert!(spec.install_recipe.is_none(), "{vendor}");
            let url = spec
                .manual_install_url
                .unwrap_or_else(|| panic!("{vendor}"));
            assert!(url.starts_with("https://"), "{vendor}: {url}");
        }
        // Recipe-backed vendors never carry a manual link (one guidance home).
        for spec in AGENT_PROBE_SPECS {
            assert_eq!(
                spec.install_recipe.is_some(),
                spec.manual_install_url.is_none(),
                "{spec:?}"
            );
        }
    }
}
