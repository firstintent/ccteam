//! v0.8.18 柱1 — `GET /api/v1/hosts` host-keyed agent report.
//!
//! The host-first successor to the flat [`super::capabilities`] probe: a
//! machine is the主轴. Today one host (`local` = this machine); the host
//! axis (every session reserves `host`, `"local"` only so far) makes this
//! multi-machine-ready — future satellites each add a row, and a session
//! will carry which host it runs on. Per host we report hostname, os/arch,
//! and ccteam version. Per agent vendor we report whether it is installed
//! (plus its `--version`), whether the ccteam MCP server is registered, and
//! a `ready` / `needs_config` / `not_installed` status with a copy-paste
//! remediation hint.
//!
//! **The only writable endpoint** is `POST .../register-mcp`: ccteam
//! writing its OWN MCP server into the vendor config — the single allowed
//! write to a vendor footprint (red line: ccteam never writes a vendor
//! login / key and never installs a CLI from the web; it `execute`s
//! nothing else). It delegates to [`ccteam_core::mcp_register`], the same
//! idempotent seam `ccteam config` uses.
//!
//! Merged into the `/api/v1` [`OpenApiRouter`] (see [`super::openapi`]) so
//! the shared web-token gate applies for free.

use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use ccteam_harness::{CLAUDE_BIN_ENV, CODEX_BIN_ENV, GROK_BIN_ENV, OPENCODE_BIN_ENV};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::auth::{deny_non_admin, Identity};

/// The id of this machine — the single host until the v0.9 host axis adds
/// satellites.
pub const LOCAL_HOST: &str = "local";

/// A vendor ccteam can probe + register its MCP into. **Vendor-extensible**:
/// add a row to [`PROBE_SPECS`] to surface a new agent on the host page —
/// no other code changes. `bin_env` mirrors the `CCTEAM_*_BIN` overrides the
/// harness adapters honor, so a test points the probe at a fake script.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ProbeSpec {
    /// Stable vendor token (`claude` / `codex`) — matches `AgentVendor`'s
    /// lowercase serde form and what `POST .../sessions` accepts.
    pub vendor: &'static str,
    /// Harness id label (`claude-code` / `codex`).
    pub harness_id: &'static str,
    /// Env override for the binary path (`CCTEAM_CLAUDE_BIN` / `_CODEX_BIN`).
    pub bin_env: &'static str,
    /// Default binary name resolved on `PATH` when the env override is unset.
    pub default_bin: &'static str,
    /// Whether ccteam registers its MCP server into a persistent vendor config
    /// file for this vendor (claude `~/.claude.json` / codex `config.toml`).
    /// `false` for vendors with no config-file MCP seam (grok/ACP passes
    /// `mcpServers` per-session over the wire, so there is nothing to register
    /// on the host page): such a vendor never shows `needs_config` or a
    /// register CTA, and `register-mcp` rejects it with 400.
    pub mcp_registrable: bool,
}

/// The agent vendors surfaced on the host page. Extend here to add a vendor.
pub(crate) const PROBE_SPECS: &[ProbeSpec] = &[
    ProbeSpec {
        vendor: "claude",
        harness_id: "claude-code",
        bin_env: CLAUDE_BIN_ENV,
        default_bin: "claude",
        mcp_registrable: true,
    },
    ProbeSpec {
        vendor: "codex",
        harness_id: "codex",
        bin_env: CODEX_BIN_ENV,
        default_bin: "codex",
        mcp_registrable: true,
    },
    ProbeSpec {
        vendor: "grok",
        harness_id: "grok",
        bin_env: GROK_BIN_ENV,
        default_bin: "grok",
        // grok/ACP has no config-file MCP seam — nothing to register here.
        mcp_registrable: false,
    },
    ProbeSpec {
        vendor: "opencode",
        harness_id: "opencode",
        bin_env: OPENCODE_BIN_ENV,
        default_bin: "opencode",
        mcp_registrable: false,
    },
];

/// One agent's health on a host.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AgentHealth {
    /// Vendor token (`claude` / `codex`).
    pub vendor: String,
    /// Harness id (`claude-code` / `codex`).
    pub harness_id: String,
    /// Whether `<bin> --version` ran and exited 0.
    pub installed: bool,
    /// First line of `<bin> --version` (e.g. `"claude 1.2.3"`); `null` when
    /// not installed.
    pub version: Option<String>,
    /// Resolved binary path the probe ran (env override or `PATH` name).
    pub bin: String,
    /// Whether the ccteam MCP server is registered in this vendor's config
    /// (`~/.claude.json` / Codex `config.toml`). The single thing
    /// `register-mcp` can flip. Always `false` when [`Self::mcp_registrable`]
    /// is `false` (no config-file seam to register into).
    pub mcp_registered: bool,
    /// Whether config-file MCP registration applies to this vendor at all
    /// (`false` for grok/ACP — MCP rides the session protocol, not a config).
    /// Drives whether the register CTA is offered.
    pub mcp_registrable: bool,
    /// `ready` (installed + MCP registered, or installed for a
    /// non-registrable vendor) / `needs_config` (installed, MCP not
    /// registered) / `not_installed`.
    pub status: String,
    /// Copy-paste remediation when not `ready`; `null` when ready.
    pub hint: Option<String>,
}

/// Collection row for `GET /api/v1/hosts`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HostSummary {
    /// Host id (`local` or registered satellite id).
    pub host: String,
    /// OS hostname (or `local` when unresolved).
    pub hostname: String,
    /// `true` for this machine.
    pub is_local: bool,
    /// `online` | `offline` (local is always online).
    #[serde(default = "default_online")]
    pub status: String,
    /// Number of agent vendors probed / last reported.
    pub agent_count: usize,
    /// How many agents are `ready`.
    pub agents_ready: usize,
}

#[allow(dead_code)]
fn default_online() -> String {
    "online".to_string()
}

/// Detail for `GET /api/v1/hosts/{host}`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HostDetail {
    pub host: String,
    pub hostname: String,
    pub is_local: bool,
    /// `std::env::consts::OS` (`linux` / `macos`).
    pub os: String,
    /// `std::env::consts::ARCH` (`x86_64` / `aarch64`).
    pub arch: String,
    /// ccteam build version driving this host.
    pub ccteam_version: String,
    pub agents: Vec<AgentHealth>,
}

/// `GET /api/v1/hosts` response.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HostsResponse {
    pub hosts: Vec<HostSummary>,
}

/// Result of one `<bin> --version` probe.
#[derive(Debug, Clone)]
pub(crate) struct ProbeResult {
    pub installed: bool,
    pub version: Option<String>,
}

/// Process-lifetime probe cache keyed by resolved binary path. Keyed by
/// path (not vendor) so a test pointing `CCTEAM_*_BIN` at a fake script gets
/// an independent entry. A `refresh` probe bypasses + overwrites the entry —
/// the manual re-probe that breaks the daemon-lifetime cache (a vendor
/// installed after the daemon started flips `installed` without a restart).
fn probe_cache() -> &'static Mutex<HashMap<String, ProbeResult>> {
    static CACHE: OnceLock<Mutex<HashMap<String, ProbeResult>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Resolve a vendor's binary path: `CCTEAM_*_BIN` override, else the `PATH`
/// name.
pub(crate) fn resolve_bin(spec: &ProbeSpec) -> String {
    std::env::var(spec.bin_env).unwrap_or_else(|_| spec.default_bin.to_string())
}

/// Probe `<bin> --version`: capture exit status + the first stdout line as
/// the version string. Cached by path; `refresh` bypasses the cache and
/// re-runs. Any spawn error (binary not on PATH) folds to
/// `{installed:false, version:None}`. This is the SINGLE probe impl —
/// [`super::capabilities`] reuses it (no second `--version` shell-out path).
pub(crate) fn probe_bin(bin: &str, refresh: bool) -> ProbeResult {
    if !refresh {
        if let Ok(cache) = probe_cache().lock() {
            if let Some(hit) = cache.get(bin) {
                return hit.clone();
            }
        }
    }
    let result = match Command::new(bin)
        .arg("--version")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    {
        Ok(out) if out.status.success() => {
            let version = String::from_utf8_lossy(&out.stdout)
                .lines()
                .next()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            ProbeResult {
                installed: true,
                version,
            }
        }
        _ => ProbeResult {
            installed: false,
            version: None,
        },
    };
    if let Ok(mut cache) = probe_cache().lock() {
        cache.insert(bin.to_string(), result.clone());
    }
    result
}

/// Is the ccteam MCP server registered in this vendor's config? Read-only,
/// best-effort (a missing / unreadable config reads as `false`).
fn mcp_registered(vendor: &str) -> bool {
    match vendor {
        "claude" => ccteam_core::projects::resolve_claude_json_path()
            .map(|p| ccteam_core::mcp_register::claude_mcp_registered(&p))
            .unwrap_or(false),
        "codex" => ccteam_core::mcp_register::resolve_codex_config_path()
            .map(|p| ccteam_core::mcp_register::codex_mcp_registered(&p))
            .unwrap_or(false),
        // Grok MCP registration not wired in MVP (L18: best-effort, non-blocking).
        "grok" => false,
        "opencode" => false,
        _ => false,
    }
}

/// Build one agent's health row (probe + MCP-registration check + tri-state).
fn agent_health(spec: &ProbeSpec, refresh: bool) -> AgentHealth {
    let bin = resolve_bin(spec);
    let probe = probe_bin(&bin, refresh);
    // A non-registrable vendor has no config-file seam → never "registered".
    let registered = spec.mcp_registrable && mcp_registered(spec.vendor);
    let status = classify_status(probe.installed, registered, spec.mcp_registrable);
    let hint: Option<String> = match status {
        "not_installed" => Some(format!(
            "{} not found on PATH — install it (or set {}); ccteam never installs a CLI for you",
            spec.vendor, spec.bin_env
        )),
        "needs_config" => Some(format!(
            "register the ccteam MCP server: POST /api/v1/hosts/{LOCAL_HOST}/register-mcp?vendor={}",
            spec.vendor
        )),
        _ => None,
    };
    AgentHealth {
        vendor: spec.vendor.to_string(),
        harness_id: spec.harness_id.to_string(),
        installed: probe.installed,
        version: probe.version,
        bin,
        mcp_registered: registered,
        mcp_registrable: spec.mcp_registrable,
        status: status.to_string(),
        hint,
    }
}

/// The `ready | needs_config | not_installed` tri-state: `not_installed` when
/// the binary isn't runnable; `ready` when it is AND the ccteam MCP is
/// registered — OR the vendor has no config-file MCP seam (`!registrable`),
/// where an installed binary is all there is to configure; `needs_config`
/// when installed but the MCP isn't registered yet (the one thing
/// `register-mcp` fixes).
fn classify_status(installed: bool, registered: bool, registrable: bool) -> &'static str {
    if !installed {
        "not_installed"
    } else if registered || !registrable {
        "ready"
    } else {
        "needs_config"
    }
}

/// Resolve this machine's hostname, defaulting to [`LOCAL_HOST`].
fn local_hostname() -> String {
    ccteam_core::host::read_hostname().unwrap_or_else(|| LOCAL_HOST.to_string())
}

/// Probe every spec (off the async runtime — each shells out).
async fn probe_all_agents(refresh: bool) -> Vec<AgentHealth> {
    tokio::task::spawn_blocking(move || {
        PROBE_SPECS
            .iter()
            .map(|spec| agent_health(spec, refresh))
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_else(|err| {
        tracing::warn!(?err, "hosts: agent probe worker failed");
        Vec::new()
    })
}

/// `GET /api/v1/hosts` — list every host (today just this machine).
#[utoipa::path(
    get,
    path = "/api/v1/hosts",
    tag = "hosts",
    responses((status = 200, description = "Hosts ccteam drives (today one: `local`)", body = HostsResponse)),
)]
pub(crate) async fn handle_hosts(
    State(app): State<crate::state::AppState>,
    Extension(identity): Extension<Identity>,
) -> Response {
    // v0.8.18 档1 — the host-keyed agent report is an operator/admin surface.
    if let Some(deny) = deny_non_admin(&identity) {
        return deny;
    }
    let agents = probe_all_agents(false).await;
    let agents_ready = agents.iter().filter(|a| a.status == "ready").count();
    let mut hosts = vec![HostSummary {
        host: LOCAL_HOST.to_string(),
        hostname: local_hostname(),
        is_local: true,
        status: "online".to_string(),
        agent_count: agents.len(),
        agents_ready,
    }];
    // v0.8.24 Track D — registered satellites from the host registry.
    if let Ok(reg) = ccteam_core::HostRegistry::load(&app.paths.host_registry_path()) {
        for h in reg.list() {
            let ready = h.agents.iter().filter(|a| a.status == "ready").count();
            hosts.push(HostSummary {
                host: h.id.clone(),
                hostname: h.hostname.clone(),
                is_local: false,
                status: h
                    .status_label(ccteam_core::DEFAULT_HEARTBEAT_TTL_SECS)
                    .to_string(),
                agent_count: h.agents.len().max(1),
                agents_ready: ready,
            });
        }
    }
    Json(HostsResponse { hosts }).into_response()
}

/// Query for `GET /api/v1/hosts/{host}` — `?refresh=true` forces a re-probe.
#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
pub struct HostDetailQuery {
    /// Bypass the probe cache and re-run `<bin> --version` (manual re-probe).
    #[serde(default)]
    pub refresh: bool,
}

/// `GET /api/v1/hosts/{host}` — one host's full agent report. 404 for any
/// host other than `local` (no satellites yet).
#[utoipa::path(
    get,
    path = "/api/v1/hosts/{host}",
    tag = "hosts",
    params(
        ("host" = String, Path, description = "Host id (`local`)"),
        HostDetailQuery,
    ),
    responses(
        (status = 200, description = "Host detail `{host, hostname, os, arch, ccteam_version, agents[]}`", body = HostDetail),
        (status = 404, description = "Unknown host (only `local` exists today)"),
    ),
)]
pub(crate) async fn handle_host_detail(
    State(app): State<crate::state::AppState>,
    Extension(identity): Extension<Identity>,
    Path(host): Path<String>,
    Query(q): Query<HostDetailQuery>,
) -> Response {
    if let Some(deny) = deny_non_admin(&identity) {
        return deny;
    }
    if host == LOCAL_HOST {
        let agents = probe_all_agents(q.refresh).await;
        return Json(HostDetail {
            host: LOCAL_HOST.to_string(),
            hostname: local_hostname(),
            is_local: true,
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            ccteam_version: ccteam_core::VERSION.to_string(),
            agents,
        })
        .into_response();
    }
    match ccteam_core::HostRegistry::load(&app.paths.host_registry_path()) {
        Ok(reg) => match reg.get(&host) {
            Some(h) => {
                let agents: Vec<AgentHealth> = h
                    .agents
                    .iter()
                    .map(|a| AgentHealth {
                        vendor: a.vendor.clone(),
                        harness_id: a.vendor.clone(),
                        installed: a.installed,
                        version: a.version.clone(),
                        bin: String::new(),
                        mcp_registered: false,
                        mcp_registrable: false,
                        status: a.status.clone(),
                        hint: None,
                    })
                    .collect();
                Json(HostDetail {
                    host: h.id.clone(),
                    hostname: h.hostname.clone(),
                    is_local: false,
                    os: h.os.clone(),
                    arch: h.arch.clone(),
                    ccteam_version: h.ccteam_version.clone(),
                    agents,
                })
                .into_response()
            }
            None => (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": format!("unknown host: {host}")})),
            )
                .into_response(),
        },
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("{err}")})),
        )
            .into_response(),
    }
}

/// Query for `POST .../register-mcp` — optional `?vendor=claude|codex`
/// (omitted ⇒ register every vendor).
#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
pub struct RegisterMcpQuery {
    /// Restrict to one vendor (`claude` / `codex`); omit to register all.
    #[serde(default)]
    pub vendor: Option<String>,
}

/// `POST /api/v1/hosts/{host}/register-mcp` — register ccteam's OWN MCP
/// server into the vendor config(s). **Idempotent** (merge, never clobber)
/// and the ONLY write this surface performs. ccteam executes nothing else:
/// it never writes a vendor login/key and never installs a CLI. 404 for a
/// non-`local` host; 400 for an unknown `vendor`; 500 if the binary path
/// can't be resolved.
#[utoipa::path(
    post,
    path = "/api/v1/hosts/{host}/register-mcp",
    tag = "hosts",
    params(
        ("host" = String, Path, description = "Host id (`local`)"),
        RegisterMcpQuery,
    ),
    responses(
        (status = 200, description = "Registered; `{registered:[vendor], paths:{vendor:path}}`", body = serde_json::Value),
        (status = 400, description = "Unknown vendor"),
        (status = 404, description = "Unknown host"),
        (status = 500, description = "Cannot resolve the ccteam binary path"),
    ),
)]
pub(crate) async fn handle_register_mcp(
    Extension(identity): Extension<Identity>,
    Path(host): Path<String>,
    Query(q): Query<RegisterMcpQuery>,
) -> Response {
    if let Some(deny) = deny_non_admin(&identity) {
        return deny;
    }
    if host != LOCAL_HOST {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": format!("unknown host: {host}")})),
        )
            .into_response();
    }
    let want: Option<String> = match q.vendor.as_deref().map(str::trim) {
        None | Some("") => None,
        // A valid vendor that has no config-file MCP seam (grok/ACP): reject
        // explicitly rather than silently no-op, so the UI/API never presents
        // a register action that does nothing.
        Some(v)
            if PROBE_SPECS
                .iter()
                .any(|s| s.vendor == v && !s.mcp_registrable) =>
        {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": format!(
                        "vendor {v} has no config-file MCP registration (its MCP rides the session protocol)"
                    )
                })),
            )
                .into_response();
        }
        Some(v) if PROBE_SPECS.iter().any(|s| s.vendor == v) => Some(v.to_string()),
        Some(other) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": format!("unknown vendor: {other} (expected claude|codex)")
                })),
            )
                .into_response();
        }
    };

    let result = tokio::task::spawn_blocking(move || register_mcp_blocking(want.as_deref())).await;
    match result {
        Ok(Ok(paths)) => Json(serde_json::json!({
            "registered": paths.keys().collect::<Vec<_>>(),
            "paths": paths,
        }))
        .into_response(),
        Ok(Err(err)) => {
            tracing::warn!(%err, "register-mcp failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("{err}")})),
            )
                .into_response()
        }
        Err(err) => {
            tracing::warn!(?err, "register-mcp worker panicked");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "register-mcp worker failed"})),
            )
                .into_response()
        }
    }
}

/// Do the (blocking) MCP registration for the requested vendor(s). Returns a
/// `vendor → written-config-path` map. `want = None` registers every vendor.
fn register_mcp_blocking(
    want: Option<&str>,
) -> anyhow::Result<std::collections::BTreeMap<String, String>> {
    let bin = ccteam_core::current_ccteam_bin()?;
    let mut written = std::collections::BTreeMap::new();
    let do_vendor = |v: &str| want.is_none() || want == Some(v);

    if do_vendor("claude") {
        let path = ccteam_core::projects::resolve_claude_json_path()?;
        ccteam_core::mcp_register::install_mcp_into(&path, &bin)?;
        written.insert("claude".to_string(), path.display().to_string());
    }
    if do_vendor("codex") {
        let path = ccteam_core::mcp_register::resolve_codex_config_path()?;
        ccteam_core::mcp_register::install_codex_mcp_into(&path, &bin)?;
        written.insert("codex".to_string(), path.display().to_string());
    }
    Ok(written)
}

// ── v0.8.24 Track D — join-token / join / heartbeat ───────────────────────────

/// Body for `POST /api/v1/hosts/join-token` (admin mint).
#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct MintJoinTokenForm {
    #[serde(default)]
    pub label: Option<String>,
    /// Optional max uses; omit = unlimited until revoked.
    #[serde(default)]
    pub max_uses: Option<u32>,
}

/// `POST /api/v1/hosts/join-token` — admin mints a join token for satellites.
#[utoipa::path(
    post,
    path = "/api/v1/hosts/join-token",
    tag = "hosts",
    request_body = MintJoinTokenForm,
    responses(
        (status = 201, description = "Minted; `{token, label?, max_uses?, command}`", body = serde_json::Value),
        (status = 403, description = "Non-admin"),
    ),
)]
pub(crate) async fn handle_mint_join_token(
    State(app): State<crate::state::AppState>,
    Extension(identity): Extension<Identity>,
    Json(form): Json<MintJoinTokenForm>,
) -> Response {
    if let Some(deny) = deny_non_admin(&identity) {
        return deny;
    }
    let path = app.paths.host_join_tokens_path();
    let mut store = match ccteam_core::JoinTokenStore::load(&path) {
        Ok(s) => s,
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("{err}")})),
            )
                .into_response();
        }
    };
    let tok = store.mint(form.label.clone(), form.max_uses).token.clone();
    if let Err(err) = store.save(&path) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("{err}")})),
        )
            .into_response();
    }
    let command = format!("ccteam host join --daemon <daemon-url> --token {tok}");
    (
        StatusCode::CREATED,
        Json(serde_json::json!({
            "token": tok,
            "label": form.label,
            "max_uses": form.max_uses,
            "command": command,
        })),
    )
        .into_response()
}

/// `POST /api/v1/hosts/join` — satellite registers with a join token.
#[utoipa::path(
    post,
    path = "/api/v1/hosts/join",
    tag = "hosts",
    request_body = serde_json::Value,
    responses(
        (status = 201, description = "Joined; `{host, agent_token, heartbeat_ttl_secs}`", body = serde_json::Value),
        (status = 400, description = "Invalid / exhausted join token"),
        (status = 403, description = "Caller not admin and not join-token bearer"),
    ),
)]
pub(crate) async fn handle_host_join(
    State(app): State<crate::state::AppState>,
    Extension(identity): Extension<Identity>,
    Json(req): Json<ccteam_core::HostJoinRequest>,
) -> Response {
    // Admin OR join-token identity may call this; body token is always validated.
    let ok_caller = identity.is_admin || identity.id == "host-join";
    if !ok_caller {
        if let Some(deny) = deny_non_admin(&identity) {
            return deny;
        }
    }
    let reg_path = app.paths.host_registry_path();
    let tok_path = app.paths.host_join_tokens_path();
    let mut reg = match ccteam_core::HostRegistry::load(&reg_path) {
        Ok(r) => r,
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("{err}")})),
            )
                .into_response();
        }
    };
    let mut tokens = match ccteam_core::JoinTokenStore::load(&tok_path) {
        Ok(t) => t,
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("{err}")})),
            )
                .into_response();
        }
    };
    match ccteam_core::apply_join(&mut reg, &mut tokens, &req) {
        Ok(resp) => {
            if let Err(err) = reg.save(&reg_path).and_then(|_| tokens.save(&tok_path)) {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": format!("{err}")})),
                )
                    .into_response();
            }
            (StatusCode::CREATED, Json(resp)).into_response()
        }
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": format!("{err}")})),
        )
            .into_response(),
    }
}

/// `POST /api/v1/hosts/{host}/heartbeat` — satellite keepalive.
#[utoipa::path(
    post,
    path = "/api/v1/hosts/{host}/heartbeat",
    tag = "hosts",
    params(("host" = String, Path, description = "Registered host id")),
    request_body = serde_json::Value,
    responses(
        (status = 200, description = "Heartbeat accepted; `{host, status}`"),
        (status = 401, description = "Bad agent token"),
        (status = 404, description = "Unknown host"),
    ),
)]
pub(crate) async fn handle_host_heartbeat(
    State(app): State<crate::state::AppState>,
    Extension(identity): Extension<Identity>,
    Path(host): Path<String>,
    Json(req): Json<ccteam_core::HostHeartbeatRequest>,
) -> Response {
    // Agent token from body, or (when identity is host:<id>) the stored
    // registry token for that host. Auth layer already accepted the bearer.
    let reg_path = app.paths.host_registry_path();
    let mut reg = match ccteam_core::HostRegistry::load(&reg_path) {
        Ok(r) => r,
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("{err}")})),
            )
                .into_response();
        }
    };
    let agent_token = req.agent_token.clone().unwrap_or_default();
    let token = if !agent_token.is_empty() {
        agent_token
    } else if identity.id == format!("host:{host}") || identity.is_admin {
        reg.get(&host)
            .map(|h| h.agent_token.clone())
            .unwrap_or_default()
    } else {
        String::new()
    };
    if token.is_empty() {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "agent token required"})),
        )
            .into_response();
    }
    match ccteam_core::apply_heartbeat(&mut reg, &host, &token, &req) {
        Ok(rec) => {
            if let Err(err) = reg.save(&reg_path) {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": format!("{err}")})),
                )
                    .into_response();
            }
            Json(serde_json::json!({
                "host": rec.id,
                "status": rec.status_label(ccteam_core::DEFAULT_HEARTBEAT_TTL_SECS),
                "last_heartbeat_unix": rec.last_heartbeat_unix,
            }))
            .into_response()
        }
        Err(err) => {
            let msg = err.to_string();
            let code = if msg.contains("unknown host") {
                StatusCode::NOT_FOUND
            } else if msg.contains("invalid agent") {
                StatusCode::UNAUTHORIZED
            } else {
                StatusCode::BAD_REQUEST
            };
            (code, Json(serde_json::json!({"error": msg}))).into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_bin_missing_binary_is_not_installed() {
        let r = probe_bin("/nonexistent/ccteam-fake-binary-zzz", true);
        assert!(!r.installed);
        assert!(r.version.is_none());
    }

    #[test]
    fn probe_bin_true_binary_is_installed() {
        // `/bin/true --version` exits 0 (GNU coreutils ignores the flag) — a
        // stand-in for a runnable vendor binary.
        if std::path::Path::new("/bin/true").exists() {
            let r = probe_bin("/bin/true", true);
            assert!(r.installed);
        }
    }

    #[test]
    fn classify_status_covers_the_tri_state() {
        // not installed dominates regardless of registration.
        assert_eq!(classify_status(false, false, true), "not_installed");
        assert_eq!(classify_status(false, true, true), "not_installed");
        // installed but MCP not registered → needs_config (register-mcp fixes it).
        assert_eq!(classify_status(true, false, true), "needs_config");
        // installed + MCP registered → ready (the acceptance: claude ready).
        assert_eq!(classify_status(true, true, true), "ready");
    }

    #[test]
    fn classify_status_non_registrable_vendor_is_ready_when_installed() {
        // grok/ACP has no config-file MCP seam: an installed binary is all
        // there is to configure, so it reads `ready` (never `needs_config`)
        // and offers no register CTA.
        assert_eq!(classify_status(true, false, false), "ready");
        assert_eq!(classify_status(false, false, false), "not_installed");
    }

    #[test]
    fn grok_spec_is_not_mcp_registrable() {
        let grok = PROBE_SPECS.iter().find(|s| s.vendor == "grok").unwrap();
        assert!(!grok.mcp_registrable);
        let claude = PROBE_SPECS.iter().find(|s| s.vendor == "claude").unwrap();
        assert!(claude.mcp_registrable);
    }

    #[test]
    fn agent_health_status_is_not_installed_for_missing_bin() {
        let spec = ProbeSpec {
            vendor: "claude",
            harness_id: "claude-code",
            // Point at a path that cannot exist so the probe fails regardless
            // of the host's real claude install.
            bin_env: "CCTEAM_TEST_UNSET_BIN_ENV_ZZZ",
            default_bin: "/nonexistent/ccteam-fake-zzz",
            mcp_registrable: true,
        };
        let h = agent_health(&spec, true);
        assert!(!h.installed);
        assert_eq!(h.status, "not_installed");
        assert!(h.hint.is_some());
    }

    #[test]
    fn register_mcp_query_rejects_unknown_vendor_token() {
        // The handler validates against PROBE_SPECS; assert the membership
        // check that gates the 400.
        assert!(PROBE_SPECS.iter().any(|s| s.vendor == "claude"));
        assert!(PROBE_SPECS.iter().any(|s| s.vendor == "codex"));
        assert!(PROBE_SPECS.iter().any(|s| s.vendor == "grok"));
        assert!(PROBE_SPECS.iter().any(|s| s.vendor == "opencode"));
        assert!(!PROBE_SPECS.iter().any(|s| s.vendor == "gemini"));
    }
}
