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
//! **Writable endpoints**: `POST .../register-mcp` (ccteam writing its OWN
//! MCP server into the vendor config — the single allowed write to a vendor
//! footprint; it delegates to [`ccteam_core::mcp_register`], the same
//! idempotent seam `ccteam config` uses) and the VENDOR-INSTALL-1 admin
//! one-click install/update jobs under `.../vendors/{vendor}/install`
//! ([`super::vendor_install`] — recipe argv pinned in
//! `AgentProbeSpec::install_recipe`, executed shell-free as the daemon's OS
//! user; kimi/pi stay manual). ccteam still never writes a vendor login/key.
//!
//! Merged into the `/api/v1` [`OpenApiRouter`] (see [`super::openapi`]) so
//! the shared web-token gate applies for free.

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Extension, Json,
};
use ccteam_core::host_registry::{
    probe_bin_cached, resolve_bin, AgentProbeSpec, ToolSurfaceMode, AGENT_PROBE_SPECS,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::auth::{deny_non_admin, Identity};
use crate::state::AppState;

/// The id of this machine — the single host until the v0.9 host axis adds
/// satellites.
pub const LOCAL_HOST: &str = "local";

// The per-vendor probe spec registry (`AgentProbeSpec` / `AGENT_PROBE_SPECS`)
// now lives in `ccteam_core::host_registry` — the SINGLE source of truth
// shared by the satellite report loop, this host page, the capabilities
// matrix, and the MCP `status` panel. Adding a sixth vendor is one edit
// there; there is no parallel web-local table to keep in sync.

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
    /// Whether the ccteam MCP server is registered in this vendor's config.
    /// Always false for a managed-session bridge.
    pub mcp_registered: bool,
    /// `native_mcp_config` or `managed_session_bridge`.
    pub tool_surface: String,
    /// Honest explanation for non-native tool surfaces.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_surface_note: Option<String>,
    /// `ready` (installed + MCP registered, or installed with a managed
    /// session bridge) / `needs_config` (installed, MCP not
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
    /// Unix seconds of the last satellite heartbeat — the age anchor for the
    /// UI's "offline for N days" hint. `None` for `local` (always online).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_heartbeat_unix: Option<u64>,
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
    /// Whether `npm` is runnable on this machine — every VENDOR-INSTALL-1
    /// recipe is npm-based, so the SPA disables the one-click button with a
    /// hint when this is false. Only probed for `local` (always false for a
    /// satellite: its detail row renders no install CTA anyway).
    pub npm_available: bool,
    /// v0.9.0 W3 (G9) — projects registered on this host (its own
    /// `~/.ccteam/config.yaml::projects[]`, local read directly / satellite
    /// last reported at heartbeat). Drives the remote-spawn gate
    /// (`gate_remote_spawn_project`): a slug missing here cannot be
    /// spawned/rebuilt on this host.
    #[serde(default)]
    pub projects: Vec<HostProjectView>,
}

/// `GET /api/v1/hosts` response.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HostsResponse {
    pub hosts: Vec<HostSummary>,
}

/// v0.9.0 W3 (G9) — one project registered on a host, as surfaced by
/// `GET /api/v1/hosts/{host}`. Web-local schema view over
/// `ccteam_core::HostProjectReport` (which has no `utoipa` dependency).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HostProjectView {
    pub slug: String,
    pub path: String,
    /// Whether this host-local project has a daemon catalog binding.
    pub cataloged: bool,
    /// Daemon catalog slug for that binding.
    pub catalog_slug: Option<String>,
}

/// Is the ccteam MCP server registered in this vendor's config? Read-only,
/// best-effort (a missing / unreadable config reads as `false`).
fn mcp_registered(vendor: &str, ccteam_root: &std::path::Path) -> bool {
    match vendor {
        "claude" => ccteam_core::projects::resolve_claude_json_path()
            .map(|p| ccteam_core::mcp_register::claude_mcp_registered(&p))
            .unwrap_or(false),
        "codex" => ccteam_core::mcp_register::resolve_codex_config_path()
            .map(|p| ccteam_core::mcp_register::codex_mcp_registered(&p))
            .unwrap_or(false),
        "grok" => ccteam_core::mcp_register::resolve_grok_config_path()
            .map(|p| ccteam_core::mcp_register::grok_mcp_registered(&p))
            .unwrap_or(false),
        "opencode" => ccteam_core::mcp_register::resolve_opencode_config_path()
            .map(|p| ccteam_core::mcp_register::opencode_mcp_registered(&p))
            .unwrap_or(false),
        "kimi" => ccteam_core::mcp_register::resolve_kimi_config_path()
            .map(|p| ccteam_core::mcp_register::kimi_mcp_registered(&p))
            .unwrap_or(false),
        // DSH has no vendor MCP config file: "registered" means ccteam's
        // Cordis plugin (bundle + configured `ccteam-client` patch row) is
        // present in the operator's own `~/.dsh` web profile — exactly what
        // `register-mcp?vendor=dsh` writes. Read-only; a home we cannot
        // resolve or read is `false`.
        "dsh" => ccteam_harness::execution::dsh_acp::spawn_spec::dsh_home_for_identity(
            true,
            "",
            ccteam_root,
        )
        .map(|home| {
            ccteam_harness::execution::dsh_acp::materialize::dsh_client_registered_in_profile(
                &home,
                ccteam_harness::DSH_NATIVE_WEB_PROFILE,
            )
        })
        .unwrap_or(false),
        _ => false,
    }
}

/// Build one agent's health row (probe + MCP-registration check + tri-state).
fn agent_health(
    spec: &AgentProbeSpec,
    refresh: bool,
    ccteam_root: &std::path::Path,
) -> AgentHealth {
    let bin = resolve_bin(spec);
    let (installed, version) = probe_bin_cached(&bin, refresh);
    // `mcp_registered` answers per vendor (bridge vendors without a
    // registration surface answer `false`), so no tool-surface gate here.
    let registered = mcp_registered(spec.vendor, ccteam_root);
    let status = classify_status(installed, registered, spec.tool_surface);
    let hint: Option<String> = match status {
        // VENDOR-INSTALL-1 — recipe-backed vendors get the one-click pointer
        // (admin-only, backend-gated) with the exact manual command alongside;
        // kimi/pi stay manual with their docs link.
        "not_installed" => Some(match (spec.install_recipe, spec.manual_install_url) {
            (Some(recipe), _) => format!(
                "{} not found on PATH — admin: one-click Install here, or run `{}`; or set {}",
                spec.vendor,
                recipe.join(" "),
                spec.bin_env
            ),
            (None, Some(url)) => format!(
                "{} not found on PATH — install it manually ({}) or set {}",
                spec.vendor, url, spec.bin_env
            ),
            (None, None) => format!(
                "{} not found on PATH — install it or set {}",
                spec.vendor, spec.bin_env
            ),
        }),
        "needs_config" => Some(format!(
            "register the ccteam MCP server: POST /api/v1/hosts/{LOCAL_HOST}/register-mcp?vendor={}",
            spec.vendor
        )),
        _ => None,
    };
    AgentHealth {
        vendor: spec.vendor.to_string(),
        harness_id: spec.harness_id.to_string(),
        installed,
        version,
        bin,
        mcp_registered: registered,
        tool_surface: tool_surface_token(spec.tool_surface).to_string(),
        tool_surface_note: spec.tool_surface_notice(),
        status: status.to_string(),
        hint,
    }
}

/// The `ready | needs_config | not_installed` tri-state: `not_installed` when
/// the binary isn't runnable; `ready` when it is AND the ccteam MCP is
/// registered — OR tools arrive via a managed-session bridge, where an
/// installed binary is all there is to configure; `needs_config`
/// when installed but the MCP isn't registered yet (the one thing
/// `register-mcp` fixes).
fn classify_status(
    installed: bool,
    registered: bool,
    tool_surface: ToolSurfaceMode,
) -> &'static str {
    if !installed {
        "not_installed"
    } else if registered || tool_surface == ToolSurfaceMode::ManagedSessionBridge {
        "ready"
    } else {
        "needs_config"
    }
}

fn tool_surface_token(mode: ToolSurfaceMode) -> &'static str {
    match mode {
        ToolSurfaceMode::NativeMcpConfig => "native_mcp_config",
        ToolSurfaceMode::ManagedSessionBridge => "managed_session_bridge",
    }
}

/// Resolve this machine's hostname, defaulting to [`LOCAL_HOST`].
fn local_hostname() -> String {
    ccteam_core::host::read_hostname().unwrap_or_else(|| LOCAL_HOST.to_string())
}

/// Probe every spec (off the async runtime — each shells out).
async fn probe_all_agents(refresh: bool, ccteam_root: std::path::PathBuf) -> Vec<AgentHealth> {
    tokio::task::spawn_blocking(move || {
        AGENT_PROBE_SPECS
            .iter()
            .map(|spec| agent_health(spec, refresh, &ccteam_root))
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_else(|err| {
        tracing::warn!(?err, "hosts: agent probe worker failed");
        Vec::new()
    })
}

/// Whether `npm` resolves and runs on THIS machine (the local host) — every
/// one-click install recipe is `npm install -g …`, so the SPA hides/disables
/// the button when this is false. Cached like the vendor probes.
async fn probe_npm_available(refresh: bool) -> bool {
    tokio::task::spawn_blocking(move || probe_bin_cached("npm", refresh).0)
        .await
        .unwrap_or(false)
}

/// `GET /api/v1/hosts` — list every host (today just this machine).
#[utoipa::path(
    get,
    path = "/api/v1/hosts",
    tag = "hosts",
    responses((status = 200, description = "Hosts ccteam drives (today one: `local`)", body = HostsResponse)),
)]
pub(crate) async fn handle_hosts(State(app): State<crate::state::AppState>) -> Response {
    let agents = probe_all_agents(false, app.paths.root.clone()).await;
    let agents_ready = agents.iter().filter(|a| a.status == "ready").count();
    let mut hosts = vec![HostSummary {
        host: LOCAL_HOST.to_string(),
        hostname: local_hostname(),
        is_local: true,
        status: "online".to_string(),
        agent_count: agents.len(),
        agents_ready,
        last_heartbeat_unix: None,
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
                last_heartbeat_unix: Some(h.last_heartbeat_unix),
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
    if host == LOCAL_HOST {
        let agents = probe_all_agents(q.refresh, app.paths.root.clone()).await;
        let npm_available = probe_npm_available(q.refresh).await;
        let projects = ccteam_core::config::load(&app.paths.root)
            .map(|cfg| {
                cfg.projects
                    .into_iter()
                    .filter(|p| p.host.is_empty() || p.host == LOCAL_HOST)
                    // Same ownership policy as `GET /api/v1/projects`: this
                    // surface must not hand a tenant the owner's catalog (slug
                    // + absolute path) just because it hangs off a host.
                    .filter(|p| crate::routes::api_v1::can_see_project(&app, &identity, &p.slug))
                    .map(|p| HostProjectView {
                        catalog_slug: Some(p.slug.clone()),
                        slug: p.slug,
                        path: p.path.display().to_string(),
                        cataloged: true,
                    })
                    .collect()
            })
            .unwrap_or_default();
        return Json(HostDetail {
            host: LOCAL_HOST.to_string(),
            hostname: local_hostname(),
            is_local: true,
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            ccteam_version: ccteam_core::VERSION.to_string(),
            agents,
            npm_available,
            projects,
        })
        .into_response();
    }
    match ccteam_core::HostRegistry::load(&app.paths.host_registry_path()) {
        Ok(reg) => match reg.get(&host) {
            Some(h) => {
                let catalog = ccteam_core::config::load(&app.paths.root).unwrap_or_default();
                let agents: Vec<AgentHealth> = h
                    .agents
                    .iter()
                    .map(|a| {
                        let spec = AgentProbeSpec::by_vendor(&a.vendor);
                        AgentHealth {
                            vendor: a.vendor.clone(),
                            harness_id: a.vendor.clone(),
                            installed: a.installed,
                            version: a.version.clone(),
                            bin: String::new(),
                            mcp_registered: false,
                            tool_surface: spec
                                .map(|spec| tool_surface_token(spec.tool_surface))
                                .unwrap_or("unknown")
                                .to_string(),
                            tool_surface_note: spec.and_then(AgentProbeSpec::tool_surface_notice),
                            status: a.status.clone(),
                            hint: None,
                        }
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
                    // Installs never target a satellite — the field is local-only.
                    npm_available: false,
                    projects: h
                        .projects
                        .iter()
                        .cloned()
                        .filter_map(|project| {
                            let catalog_slug = catalog
                                .projects
                                .iter()
                                .find(|entry| {
                                    entry.host == h.id
                                        && entry.remote_slug.as_deref()
                                            == Some(project.slug.as_str())
                                })
                                .map(|entry| entry.slug.clone());
                            // A CATALOGED project follows its catalog entry's
                            // ownership — that is the leak this filter closes.
                            // An un-cataloged one is nobody's yet, and it is
                            // precisely the candidate list `POST /projects/
                            // import` works from — a route that deliberately
                            // lets ANY identity claim one (it stamps the
                            // caller as owner). Hiding those from tenants
                            // would break importing from the UI, so they stay
                            // visible: no owner, nothing to leak.
                            let visible = match catalog_slug.as_deref() {
                                Some(slug) => {
                                    crate::routes::api_v1::can_see_project(&app, &identity, slug)
                                }
                                None => true,
                            };
                            visible.then(|| HostProjectView {
                                slug: project.slug,
                                path: project.path,
                                cataloged: catalog_slug.is_some(),
                                catalog_slug,
                            })
                        })
                        .collect(),
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

/// Query for `POST .../register-mcp` — optional vendor selector (one of the
/// vendors advertised by `AGENT_PROBE_SPECS`; omitted ⇒ register all).
#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
pub struct RegisterMcpQuery {
    /// Restrict to one registrable vendor; omit to register all.
    #[serde(default)]
    pub vendor: Option<String>,
}

/// `POST /api/v1/hosts/{host}/register-mcp` — register ccteam's OWN entry
/// into the vendor's config surface. For the five config-file vendors that is
/// the MCP server entry; for `?vendor=dsh` it is ccteam's plugin bundle +
/// patch row in the operator's `~/.dsh` web profile (which the human loads by
/// restarting their `dsh web`). **Idempotent** (merge, never clobber), and
/// only ccteam's own entries. ccteam never writes a vendor login/key; vendor
/// CLI installs live in the separate VENDOR-INSTALL-1 job surface
/// (`.../vendors/{vendor}/install`). 404 for a non-`local` host; 400 for an
/// unknown `vendor`; 500 if the vendor config or credentials cannot be
/// resolved.
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
        (status = 403, description = "Not the admin/owner"),
        (status = 404, description = "Unknown host"),
        (status = 500, description = "Cannot resolve vendor config or MCP credentials"),
    ),
)]
pub(crate) async fn handle_register_mcp(
    State(app): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(host): Path<String>,
    Query(q): Query<RegisterMcpQuery>,
) -> Response {
    // Writes the daemon's admin MCP credential into a vendor's GLOBAL config
    // on this machine — owner-scoped, never a tenant action.
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
        // DSH's "register ccteam" is not a vendor-config write: its tool and
        // transport surface is the `@ccteam/dsh-client` Cordis plugin. This
        // action merges ccteam's OWN bundle entry + `ccteam-client` patch row
        // (carrying this daemon's transportSocket/URL) into the operator's
        // real `~/.dsh` web profile — gate ①, idempotent, merge-only — so a
        // hand-started `dsh web` can serve hires after its owner restarts it.
        // Deliberately NOT part of register-all: it only takes effect after
        // the human restarts their own instance, so it stays an explicit ask.
        Some("dsh") => {
            return match app.dsh_web.runtime().register_operator_profile().await {
                Ok(profile_dir) => (
                    StatusCode::OK,
                    Json(serde_json::json!({
                        "registered": ["dsh"],
                        "paths": {"dsh": profile_dir},
                        "note": "restart your DSH web instance to load the ccteam plugin",
                    })),
                )
                    .into_response(),
                Err(e) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": format!("register the ccteam DSH plugin: {e}")})),
                )
                    .into_response(),
            };
        }
        // A valid vendor whose tools arrive through a managed-session bridge:
        // Reject explicitly rather than pretending a config file was written,
        // so the UI/API never presents a register action that does nothing.
        Some(v)
            if AGENT_PROBE_SPECS
                .iter()
                .any(|s| !s.tool_surface.uses_native_mcp_config() && s.vendor == v) =>
        {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": format!(
                        "vendor {v} has no config-file MCP registration. {}",
                        AgentProbeSpec::by_vendor(v)
                            .and_then(AgentProbeSpec::tool_surface_notice)
                            .unwrap_or_else(|| "Its tools are available only in managed sessions.".to_string())
                    )
                })),
            )
                .into_response();
        }
        Some(v) if AGENT_PROBE_SPECS.iter().any(|s| s.vendor == v) => Some(v.to_string()),
        Some(other) => {
            let expected = AGENT_PROBE_SPECS
                .iter()
                .map(|spec| spec.vendor)
                .collect::<Vec<_>>()
                .join("|");
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": format!("unknown vendor: {other} (expected {expected})")
                })),
            )
                .into_response();
        }
    };

    let paths = app.paths.clone();
    let mcp_http_url =
        ccteam_harness::execution::mcp_config::resolve_mcp_http_url(&app.paths.root.join("run"));
    let result = tokio::task::spawn_blocking(move || {
        register_mcp_blocking(want.as_deref(), &paths, &mcp_http_url)
    })
    .await;
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
///
/// The bearer is the machine-user ENROLLMENT credential, resolved once for the
/// whole batch. A vendor's global config is shared by every process that vendor
/// ever starts, so it must not carry the admin web token: that made every
/// hand-started agent the same caller, which is why their children mounted as
/// roots. `ensure_user_credential` is idempotent, so re-registering does not
/// invalidate the configs already written.
fn register_mcp_blocking(
    want: Option<&str>,
    paths: &ccteam_core::CcteamPaths,
    mcp_http_url: &str,
) -> anyhow::Result<std::collections::BTreeMap<String, String>> {
    let mut written = std::collections::BTreeMap::new();
    let do_vendor = |v: &str| want.is_none() || want == Some(v);
    let bearer = ccteam_core::enroll::ensure_user_credential(
        paths,
        &ccteam_core::identity::owner_tag(ccteam_core::identity::ADMIN_WEB_ID, true),
    )?
    .bearer();

    if do_vendor("claude") {
        let path = ccteam_core::projects::resolve_claude_json_path()?;
        ccteam_core::mcp_register::install_mcp_into(&path, mcp_http_url, &bearer)?;
        written.insert("claude".to_string(), path.display().to_string());
    }
    if do_vendor("codex") {
        let path = ccteam_core::mcp_register::resolve_codex_config_path()?;
        ccteam_core::mcp_register::install_codex_mcp_into(&path, mcp_http_url, &bearer)?;
        written.insert("codex".to_string(), path.display().to_string());
    }
    // v0.9.3 vendor symmetry — any vendor's main session can orchestrate.
    if do_vendor("grok") {
        let path = ccteam_core::mcp_register::resolve_grok_config_path()?;
        ccteam_core::mcp_register::install_grok_mcp_into(&path, mcp_http_url, &bearer)?;
        written.insert("grok".to_string(), path.display().to_string());
    }
    if do_vendor("opencode") {
        let path = ccteam_core::mcp_register::resolve_opencode_config_path()?;
        ccteam_core::mcp_register::install_opencode_mcp_into(&path, mcp_http_url, &bearer)?;
        written.insert("opencode".to_string(), path.display().to_string());
    }
    if do_vendor("kimi") {
        let path = ccteam_core::mcp_register::resolve_kimi_config_path()?;
        ccteam_core::mcp_register::install_kimi_mcp_into(&path, mcp_http_url, &bearer)?;
        written.insert("kimi".to_string(), path.display().to_string());
    }
    Ok(written)
}

/// Query for `DELETE /api/v1/hosts/{host}` — `?force=true` removes a host
/// even while it is online (heartbeating).
#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
pub struct HostRemoveQuery {
    /// Remove even if the host is currently online.
    #[serde(default)]
    pub force: bool,
}

/// `DELETE /api/v1/hosts/{host}` — deregister a satellite (drop its record
/// from the registry; a later `ccteam host join` re-adds it under a fresh
/// token). `local` can never be removed — it IS the main daemon machine.
/// An online host is refused unless `?force=true`, so an operator does not
/// accidentally drop a live satellite mid-session.
#[utoipa::path(
    delete,
    path = "/api/v1/hosts/{host}",
    tag = "hosts",
    params(
        ("host" = String, Path, description = "Host id to deregister (never `local`)"),
        HostRemoveQuery,
    ),
    responses(
        (status = 200, description = "Removed; `{host}`", body = serde_json::Value),
        (status = 400, description = "`host` is `local` (the main daemon machine)"),
        (status = 403, description = "Not the admin/owner"),
        (status = 404, description = "Unknown host"),
        (status = 409, description = "Host is online; retry with `?force=true`"),
        (status = 500, description = "Registry could not be loaded or saved"),
    ),
)]
pub(crate) async fn handle_host_remove(
    State(app): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(host): Path<String>,
    Query(q): Query<HostRemoveQuery>,
) -> Response {
    // Fleet membership is daemon-global state, not a project resource: only
    // the owner may drop a satellite (and with it its execution channel).
    if let Some(deny) = deny_non_admin(&identity) {
        return deny;
    }
    if host == LOCAL_HOST {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "local is the main daemon machine, not a registry entry"
            })),
        )
            .into_response();
    }
    let path = app.paths.host_registry_path();
    let mut reg = match ccteam_core::HostRegistry::load(&path) {
        Ok(r) => r,
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("{err}")})),
            )
                .into_response();
        }
    };
    if !q.force {
        if let Some(rec) = reg.get(&host) {
            if rec.status_label(ccteam_core::DEFAULT_HEARTBEAT_TTL_SECS) == "online" {
                return (
                    StatusCode::CONFLICT,
                    Json(serde_json::json!({
                        "error": format!(
                            "host {host} is online; pass ?force=true to remove a live satellite"
                        )
                    })),
                )
                    .into_response();
            }
        }
    }
    match reg.remove(&host) {
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": format!("unknown host: {host}")})),
        )
            .into_response(),
        Some(_) => {
            if let Err(err) = reg.save(&path) {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": format!("{err}")})),
                )
                    .into_response();
            }
            Json(serde_json::json!({"host": host})).into_response()
        }
    }
}

// ── v0.8.24 Track D — join-token / join / heartbeat ───────────────────────────

/// Body for `POST /api/v1/hosts/join-token`.
#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct MintJoinTokenForm {
    #[serde(default)]
    pub label: Option<String>,
    /// Optional max uses; omit = unlimited until revoked.
    #[serde(default)]
    pub max_uses: Option<u32>,
}

/// `POST /api/v1/hosts/join-token` — mint a join token for satellites.
#[utoipa::path(
    post,
    path = "/api/v1/hosts/join-token",
    tag = "hosts",
    request_body = MintJoinTokenForm,
    responses(
        (status = 201, description = "Minted; `{token, label?, max_uses?, command}`", body = serde_json::Value),
        (status = 403, description = "Not the admin/owner"),
    ),
)]
pub(crate) async fn handle_mint_join_token(
    State(app): State<crate::state::AppState>,
    Extension(identity): Extension<Identity>,
    Json(form): Json<MintJoinTokenForm>,
) -> Response {
    // A join token attaches a MACHINE to this daemon (and lets it run agent
    // sessions): a fleet credential, owner-only to mint.
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

/// `GET /api/v1/hosts/join-token` — read the newest still-valid join
/// token (unrevoked, not exhausted). `token: null` when none exists — the UI
/// then offers the mint action (`POST` on this path). **Read-only**: never
/// mints.
#[utoipa::path(
    get,
    path = "/api/v1/hosts/join-token",
    tag = "hosts",
    responses(
        (status = 200, description = "Newest valid token or `{token: null}`; `{token, label?, minted_at?, max_uses?, uses, command}`", body = serde_json::Value),
        (status = 403, description = "Not the admin/owner"),
    ),
)]
pub(crate) async fn handle_get_join_token(
    State(app): State<crate::state::AppState>,
    Extension(identity): Extension<Identity>,
) -> Response {
    // Reads the token itself — same credential as minting one, same gate.
    if let Some(deny) = deny_non_admin(&identity) {
        return deny;
    }
    let path = app.paths.host_join_tokens_path();
    let store = match ccteam_core::JoinTokenStore::load(&path) {
        Ok(s) => s,
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("{err}")})),
            )
                .into_response();
        }
    };
    let newest_valid = store
        .tokens
        .iter()
        .rev()
        .find(|t| !t.revoked && t.max_uses.map(|m| t.uses < m).unwrap_or(true));
    match newest_valid {
        Some(t) => Json(serde_json::json!({
            "token": t.token,
            "label": t.label,
            "minted_at": t.minted_at,
            "max_uses": t.max_uses,
            "uses": t.uses,
            "command": format!("ccteam host join --daemon <daemon-url> --token {}", t.token),
        }))
        .into_response(),
        None => Json(serde_json::json!({ "token": serde_json::Value::Null })).into_response(),
    }
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

// ── v0.9.0 reverse-connection — control channel + exec dial-back ─────────────
//
// The satellite exposes NO listener. It dials `GET /api/v1/hosts/channel`
// (bearer = agent token → identity `host:<id>` via `resolve_host_token`)
// and keeps that WS up; `report` frames riding it replace the retired
// `POST …/heartbeat` endpoint. Remote spawns push `exec_open{nonce}` down
// the channel and the satellite dials back `GET /api/v1/hosts/exec/{nonce}`
// — paired here into the `HostChannelHub` rendezvous the spawn awaits.

/// Resolve the satellite host id for a channel/dial-back request.
///
/// Fast path: the shared auth layer already mapped the agent-token bearer
/// to identity `host:<id>`. Fallback (auth disabled on a loopback bind —
/// the layer stamps everyone `admin` without reading the header): resolve
/// the `Authorization` bearer against the registry directly, exactly like
/// the old standalone satellite listener did. No valid token ⇒ reject.
fn resolve_channel_host(
    app: &crate::state::AppState,
    identity: &Identity,
    headers: &HeaderMap,
) -> Result<String, Box<Response>> {
    if let Some(id) = identity.id.strip_prefix("host:") {
        if !id.is_empty() {
            return Ok(id.to_string());
        }
    }
    let bearer = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("")
        .trim()
        .trim_start_matches("ccteam:");
    if !bearer.is_empty() {
        if let Ok(reg) = ccteam_core::HostRegistry::load(&app.paths.host_registry_path()) {
            if let Some(h) = reg.by_agent_token(bearer) {
                return Ok(h.id.clone());
            }
        }
    }
    Err(Box::new(
        (
            StatusCode::FORBIDDEN,
            "satellite agent-token bearer required",
        )
            .into_response(),
    ))
}

/// Apply a satellite report to the persisted registry (best-effort: a
/// registry IO failure is logged, never breaks the live channel).
fn apply_report_persist(
    app: &crate::state::AppState,
    host_id: &str,
    report: &ccteam_core::HostReport,
) {
    let reg_path = app.paths.host_registry_path();
    let result = ccteam_core::HostRegistry::load(&reg_path).and_then(|mut reg| {
        ccteam_core::apply_report(&mut reg, host_id, report)?;
        reg.save(&reg_path)
    });
    if let Err(err) = result {
        tracing::warn!(host = %host_id, error = %err, "host channel: report persist failed");
    }
}

/// One inbound Text frame on the control channel. Unknown `op`s are
/// ignored (forward compatibility — an older daemon must not kill the
/// channel over a frame a newer satellite added).
fn handle_channel_frame(app: &crate::state::AppState, host_id: &str, raw: &str) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) else {
        tracing::debug!(host = %host_id, "host channel: non-JSON frame ignored");
        return;
    };
    match v.get("op").and_then(|o| o.as_str()) {
        Some("report") => match serde_json::from_value::<ccteam_core::HostReport>(v.clone()) {
            Ok(rep) => apply_report_persist(app, host_id, &rep),
            Err(err) => {
                tracing::debug!(host = %host_id, error = %err, "host channel: bad report frame")
            }
        },
        Some("project_init_result") => {
            let Some(nonce) = v.get("nonce").and_then(|n| n.as_str()) else {
                tracing::debug!(host = %host_id, "host channel: project_init_result missing nonce");
                return;
            };
            match serde_json::from_value::<ccteam_harness::ProjectInitResult>(v.clone()) {
                Ok(result) => {
                    if let Err(err) = app.host_hub.complete_project_init(nonce, host_id, result) {
                        tracing::debug!(host = %host_id, error = %err, "host channel: stale project_init_result ignored");
                    }
                }
                Err(err) => {
                    tracing::debug!(host = %host_id, error = %err, "host channel: bad project_init_result frame")
                }
            }
        }
        other => {
            tracing::debug!(host = %host_id, op = ?other, "host channel: unknown op ignored")
        }
    }
}

/// `GET /api/v1/hosts/channel` — the satellite's persistent reverse control
/// channel (`ccteam-host.v1`). Not OpenAPI-documented (WS). Auth: the
/// shared bearer gate already resolved the agent token to `host:<id>`.
pub(crate) async fn handle_host_channel(
    State(app): State<crate::state::AppState>,
    Extension(identity): Extension<Identity>,
    headers: HeaderMap,
    ws: axum::extract::ws::WebSocketUpgrade,
) -> Response {
    let host_id = match resolve_channel_host(&app, &identity, &headers) {
        Ok(h) => h,
        Err(deny) => return *deny,
    };
    ws.protocols([ccteam_harness::HOST_CHANNEL_SUBPROTOCOL])
        .on_upgrade(move |socket| run_host_channel(socket, app, host_id))
}

async fn run_host_channel(
    mut socket: axum::extract::ws::WebSocket,
    app: crate::state::AppState,
    host_id: String,
) {
    use axum::extract::ws::Message as AxMessage;
    use ccteam_harness::{HubCtrlMsg, IDLE_TIMEOUT, KEEPALIVE_PERIOD};

    let mut reg = app.host_hub.register(&host_id);
    tracing::info!(host = %host_id, "host channel connected");
    // Presence is immediate: bump the registry on connect, not just on the
    // first periodic report.
    apply_report_persist(&app, &host_id, &ccteam_core::HostReport::default());

    let mut last_rx = tokio::time::Instant::now();
    let mut ping = tokio::time::interval(KEEPALIVE_PERIOD);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            msg = socket.recv() => match msg {
                Some(Ok(AxMessage::Text(t))) => {
                    last_rx = tokio::time::Instant::now();
                    handle_channel_frame(&app, &host_id, t.as_str());
                }
                Some(Ok(AxMessage::Close(_))) | None => break,
                Some(Ok(_)) => {
                    // Ping/Pong/Binary — liveness only (axum auto-pongs).
                    last_rx = tokio::time::Instant::now();
                }
                Some(Err(_)) => break,
            },
            ctrl = reg.ctrl_rx.recv() => match ctrl {
                Some(HubCtrlMsg::ExecOpen { nonce, sid }) => {
                    let frame = serde_json::json!({
                        "op": "exec_open",
                        "nonce": nonce,
                        "sid": sid,
                    });
                    if socket.send(AxMessage::Text(frame.to_string().into())).await.is_err() {
                        break;
                    }
                }
                Some(HubCtrlMsg::ProjectInit { nonce, path, slug }) => {
                    let frame = serde_json::json!({
                        "op": "project_init",
                        "nonce": nonce,
                        "path": path,
                        "slug": slug,
                    });
                    if socket.send(AxMessage::Text(frame.to_string().into())).await.is_err() {
                        break;
                    }
                }
                None => break,
            },
            _ = reg.kicked.cancelled() => {
                // A newer connection for this host registered — this one is
                // a stale ghost (NAT rebind); close and exit WITHOUT
                // unregistering the replacement (generation check below).
                let _ = socket.send(AxMessage::Close(None)).await;
                break;
            }
            _ = ping.tick() => {
                if last_rx.elapsed() > IDLE_TIMEOUT {
                    tracing::warn!(host = %host_id, "host channel idle past {}s — dropping half-open link", IDLE_TIMEOUT.as_secs());
                    break;
                }
                if socket.send(AxMessage::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
            }
        }
    }
    app.host_hub.unregister(&host_id, reg.generation);
    tracing::info!(host = %host_id, "host channel disconnected");
}

/// `GET /api/v1/hosts/exec/{nonce}` — a satellite's exec dial-back
/// (`ccteam-exec.v1`). Claims the single-use, host-bound nonce from the
/// hub, then pumps WS frames ↔ the paired [`ccteam_harness::ExecBridge`]
/// half until either side ends.
pub(crate) async fn handle_host_exec_dialback(
    State(app): State<crate::state::AppState>,
    Extension(identity): Extension<Identity>,
    Path(nonce): Path<String>,
    headers: HeaderMap,
    ws: axum::extract::ws::WebSocketUpgrade,
) -> Response {
    let host_id = match resolve_channel_host(&app, &identity, &headers) {
        Ok(h) => h,
        Err(deny) => return *deny,
    };
    let slot = match app.host_hub.claim_exec(&nonce, &host_id) {
        Ok(s) => s,
        Err(err) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": format!("{err}")})),
            )
                .into_response();
        }
    };
    ws.protocols([ccteam_harness::EXEC_SUBPROTOCOL])
        .on_upgrade(move |socket| pump_exec_dialback(socket, slot, host_id))
}

async fn pump_exec_dialback(
    mut socket: axum::extract::ws::WebSocket,
    slot: tokio::sync::oneshot::Sender<ccteam_harness::ExecBridge>,
    host_id: String,
) {
    use axum::extract::ws::Message as AxMessage;
    use ccteam_harness::{ExecBridge, IDLE_TIMEOUT, KEEPALIVE_PERIOD};
    use tokio_tungstenite::tungstenite::Message as TMessage;

    let (mine, theirs) = ExecBridge::pair();
    if slot.send(theirs).is_err() {
        // The opener gave up (dial-back arrived after its timeout).
        tracing::warn!(host = %host_id, "exec dial-back arrived after the opener timed out");
        let _ = socket.send(AxMessage::Close(None)).await;
        return;
    }
    let ExecBridge {
        tx: to_daemon,
        rx: mut from_daemon,
    } = mine;

    let mut last_rx = tokio::time::Instant::now();
    let mut ping = tokio::time::interval(KEEPALIVE_PERIOD);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            msg = socket.recv() => match msg {
                Some(Ok(am)) => {
                    last_rx = tokio::time::Instant::now();
                    let forward = match am {
                        AxMessage::Text(t) => Some(TMessage::Text(t.as_str().to_string())),
                        AxMessage::Binary(b) => Some(TMessage::Binary(b.to_vec())),
                        // Keepalive frames stay on this hop (axum
                        // auto-pongs); the bridge carries payload only.
                        AxMessage::Ping(_) | AxMessage::Pong(_) => None,
                        AxMessage::Close(_) => break,
                    };
                    if let Some(m) = forward {
                        if to_daemon.send(m).await.is_err() {
                            break; // spawn side dropped the transport
                        }
                    }
                }
                Some(Err(_)) | None => break,
            },
            out = from_daemon.recv() => match out {
                Some(m) => {
                    let am = match m {
                        TMessage::Text(t) => AxMessage::Text(t.into()),
                        TMessage::Binary(b) => AxMessage::Binary(b.into()),
                        TMessage::Close(_) => {
                            let _ = socket.send(AxMessage::Close(None)).await;
                            break;
                        }
                        // Ping/Pong/Frame never ride the bridge.
                        _ => continue,
                    };
                    if socket.send(am).await.is_err() {
                        break;
                    }
                }
                None => {
                    // Daemon-side transport closed (writer shutdown / drop)
                    // — mirror it as a WS close so the satellite ends the
                    // child exactly like the direct-dial era did.
                    let _ = socket.send(AxMessage::Close(None)).await;
                    break;
                }
            },
            _ = ping.tick() => {
                if last_rx.elapsed() > IDLE_TIMEOUT {
                    tracing::warn!(host = %host_id, "exec link idle past {}s — dropping half-open link", IDLE_TIMEOUT.as_secs());
                    break;
                }
                if socket.send(AxMessage::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
            }
        }
    }
    // Dropping `to_daemon`/`from_daemon` EOFs the spawn-side reader.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_bin_missing_binary_is_not_installed() {
        let (installed, version) = probe_bin_cached("/nonexistent/ccteam-fake-binary-zzz", true);
        assert!(!installed);
        assert!(version.is_none());
    }

    #[test]
    fn probe_bin_true_binary_is_installed() {
        // `/bin/true --version` exits 0 (GNU coreutils ignores the flag) — a
        // stand-in for a runnable vendor binary.
        if std::path::Path::new("/bin/true").exists() {
            let (installed, _) = probe_bin_cached("/bin/true", true);
            assert!(installed);
        }
    }

    #[test]
    fn classify_status_covers_the_tri_state() {
        // not installed dominates regardless of registration.
        assert_eq!(
            classify_status(false, false, ToolSurfaceMode::NativeMcpConfig),
            "not_installed"
        );
        assert_eq!(
            classify_status(false, true, ToolSurfaceMode::NativeMcpConfig),
            "not_installed"
        );
        // installed but MCP not registered → needs_config (register-mcp fixes it).
        assert_eq!(
            classify_status(true, false, ToolSurfaceMode::NativeMcpConfig),
            "needs_config"
        );
        // installed + MCP registered → ready (the acceptance: claude ready).
        assert_eq!(
            classify_status(true, true, ToolSurfaceMode::NativeMcpConfig),
            "ready"
        );
    }

    #[test]
    fn classify_status_non_registrable_vendor_is_ready_when_installed() {
        // Managed-session bridges are ready without a global config write.
        assert_eq!(
            classify_status(true, false, ToolSurfaceMode::ManagedSessionBridge),
            "ready"
        );
        assert_eq!(
            classify_status(false, false, ToolSurfaceMode::ManagedSessionBridge),
            "not_installed"
        );
    }

    #[test]
    fn probe_specs_declare_their_tool_surface_mode() {
        assert_eq!(AGENT_PROBE_SPECS.len(), 7);
        assert_eq!(
            AgentProbeSpec::by_vendor("pi").unwrap().tool_surface,
            ToolSurfaceMode::ManagedSessionBridge
        );
        assert!(AGENT_PROBE_SPECS
            .iter()
            .any(|spec| spec.tool_surface == ToolSurfaceMode::NativeMcpConfig));
        assert!(AGENT_PROBE_SPECS
            .iter()
            .any(|spec| spec.tool_surface == ToolSurfaceMode::ManagedSessionBridge));
        for spec in AGENT_PROBE_SPECS {
            assert_eq!(
                spec.tool_surface_notice().is_some(),
                spec.tool_surface == ToolSurfaceMode::ManagedSessionBridge,
                "{spec:?}"
            );
        }
    }

    #[test]
    fn agent_health_status_is_not_installed_for_missing_bin() {
        let spec = AgentProbeSpec {
            vendor: "claude",
            harness_id: "claude-code",
            // Point at a path that cannot exist so the probe fails regardless
            // of the host's real claude install.
            bin_env: "CCTEAM_TEST_UNSET_BIN_ENV_ZZZ",
            default_bin: "/nonexistent/ccteam-fake-zzz",
            tool_surface: ToolSurfaceMode::NativeMcpConfig,
            install_recipe: None,
            manual_install_url: None,
            quota_probe: None,
        };
        let h = agent_health(
            &spec,
            true,
            std::path::Path::new("/nonexistent/ccteam-home-zzz"),
        );
        assert!(!h.installed);
        assert_eq!(h.status, "not_installed");
        assert!(h.hint.is_some());
    }

    #[test]
    fn register_mcp_query_rejects_unknown_vendor_token() {
        // The handler validates against AGENT_PROBE_SPECS; assert the membership
        // check that gates the 400.
        assert!(AGENT_PROBE_SPECS.iter().any(|s| s.vendor == "claude"));
        assert!(AGENT_PROBE_SPECS.iter().any(|s| s.vendor == "codex"));
        assert!(AGENT_PROBE_SPECS.iter().any(|s| s.vendor == "grok"));
        assert!(AGENT_PROBE_SPECS.iter().any(|s| s.vendor == "opencode"));
        assert!(!AGENT_PROBE_SPECS.iter().any(|s| s.vendor == "gemini"));
    }
}
