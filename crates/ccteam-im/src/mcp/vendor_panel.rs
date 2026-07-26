//! v0.10 T1 — the MCP `status` vendor panel + routing-notes transport.
//!
//! Three honest layers appended to the daemon-aware `status` tool output,
//! all PULL-only (the model asks; nothing is injected into any prompt):
//!
//! - **Vendor panel** — for the caller project's *bound host*, one line per
//!   vendor: `installed` + `version` (from the shared `AGENT_PROBE_SPECS`
//!   probe), `auth=unknown` (the honest default — ccteam never probes vendor
//!   credential files nor fakes `ready`), and a `budget` posture derived from
//!   ccteam's own cost ledger + configured caps. The local host is probed
//!   live; a satellite host is rendered from its last control-channel report
//!   (offline → `host_online=false, stale=true`, last snapshot, NEVER the
//!   local machine's capabilities substituted).
//! - **Routing notes** — the user's advisory markdown
//!   (`<project>/.ccteam/routing.md`, else `~/.ccteam/routing.md`),
//!   wrapped with `source`/`sha256`/`updated_at`/`truncated` and capped;
//!   ccteam passes it through verbatim and never parses/interprets it.
//!
//! Pure renderers (unit-tested) + blocking gather helpers (probe/read fs, run
//! off the async runtime via `spawn_blocking`).

use std::collections::BTreeMap;
use std::path::Path;

use ccteam_core::host_registry::{HostRecord, VendorAvailability};
use ccteam_core::{CcteamPaths, DEFAULT_HEARTBEAT_TTL_SECS, LOCAL_HOST};
use sha2::{Digest, Sha256};

use super::dispatch::McpCaller;
use crate::gateway::CallerCtx;

/// Resolve which project the `status` vendor panel is scoped to.
///
/// - **Ambient** (session principal): ALWAYS the authenticated caller's own
///   project (`ctx.slug`). Any self-reported `project_arg`/`caller_slug_arg`
///   is ignored — this is the security property that a session principal can
///   never query another project's host. A missing/failed principal → `Err`.
/// - **Admin** (HTTP `/mcp` or the local main-session token): the explicit
///   `project_arg`, else the cwd-resolved `caller_slug_arg` the forwarder
///   injected, else `None` (fleet caller with no bound project → the panel
///   falls back to the local host with a note).
pub(crate) fn resolve_status_project(
    caller: McpCaller,
    project_arg: Option<&str>,
    caller_slug_arg: Option<&str>,
    ctx: Option<&CallerCtx>,
) -> Result<Option<String>, String> {
    match caller {
        McpCaller::Ambient => match ctx {
            Some(ctx) => Ok(Some(ctx.slug.clone())),
            None => Err(
                "status: caller could not be authenticated (no live session holds the \
                         presented (sid, secret) principal); the vendor panel is scoped to your \
                         own project, so it is withheld"
                    .to_string(),
            ),
        },
        McpCaller::Admin => {
            let pick = project_arg
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .or_else(|| caller_slug_arg.map(str::trim).filter(|s| !s.is_empty()));
            Ok(pick.map(str::to_string))
        }
        McpCaller::User { .. } => {
            Err("status: tenant project scope must be resolved by the dispatch ACL".to_string())
        }
    }
}

/// Cap for the routing-notes body (chars). A note beyond this keeps a 70/30
/// head-tail excerpt with a full-path pointer (aligns with the delegation
/// truncation family). ~4k chars keeps the whole `status` output ~1–2k tokens.
pub(crate) const ROUTING_NOTES_MAX_CHARS: usize = 4000;

/// Per-vendor caps keep the full status response within its one-screen budget.
const CATALOG_IDS_PER_VENDOR: usize = 4;
const CATALOG_ALIASES_PER_VENDOR: usize = 4;
const CATALOG_VENDOR_LIMIT: usize = 5;
const CATALOG_TOKEN_CHARS: usize = 32;

/// Vendors ccteam bundles a price table for (`anthropic`/`openai`/`xai`).
/// Everything else is `unpriced` — a USD budget can't be metered, never $0.
fn vendor_is_priced(vendor: &str) -> bool {
    matches!(vendor, "claude" | "codex" | "grok")
}

// ── budget posture ───────────────────────────────────────────────────────

/// Per-vendor budget posture for the status panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BudgetState {
    /// A cost cap is configured and the 24h spend is under it.
    Ok,
    /// The 24h spend reached/exceeded the cap; `approx_hours` = approximate
    /// hours until the rolling window clears enough to resume (assumes even
    /// spend over the 24h window — advisory).
    Disabled { approx_hours: u32 },
    /// No bundled price table for this vendor → a USD budget is meaningless.
    Unpriced,
    /// No cost cap configured for this vendor.
    NotConfigured,
}

impl BudgetState {
    pub(crate) fn render(&self) -> String {
        match self {
            BudgetState::Ok => "ok".to_string(),
            BudgetState::Disabled { approx_hours } => format!("disabled(~{approx_hours}h)"),
            BudgetState::Unpriced => "unpriced".to_string(),
            BudgetState::NotConfigured => "not_configured".to_string(),
        }
    }
}

/// Classify a vendor's budget posture. `priced` = the vendor has a bundled
/// price table; `cap` = its configured 24h USD cap (`None`/`≤0` = not
/// configured); `spend_24h` = its trailing-24h spend from the cost ledger.
pub(crate) fn classify_budget(priced: bool, cap: Option<f64>, spend_24h: f64) -> BudgetState {
    if !priced {
        return BudgetState::Unpriced;
    }
    match cap {
        None => BudgetState::NotConfigured,
        Some(cap) if cap <= 0.0 => BudgetState::NotConfigured,
        Some(cap) => {
            if spend_24h >= cap {
                // Assuming even spend across the rolling window, the trailing
                // sum drops back under `cap` once the overage ages out.
                let ratio = if spend_24h > 0.0 {
                    cap / spend_24h
                } else {
                    1.0
                };
                let hours = (24.0 * (1.0 - ratio)).ceil().clamp(1.0, 24.0) as u32;
                BudgetState::Disabled {
                    approx_hours: hours,
                }
            } else {
                BudgetState::Ok
            }
        }
    }
}

// ── panel rendering (pure) ─────────────────────────────────────────────────

/// Header facts for one panel (the project + its bound host).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PanelHeader {
    pub project: String,
    pub host: String,
    pub host_online: bool,
    /// When the availability snapshot was observed (RFC3339 / "just now").
    pub observed: String,
    /// True when the snapshot is a stale last-report (offline satellite).
    pub stale: bool,
    /// Optional one-line note (e.g. "no project resolved" / "host unknown").
    pub note: Option<String>,
}

/// One vendor row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PanelRow {
    pub vendor: String,
    pub installed: bool,
    pub version: Option<String>,
    /// Reserved: a ccteam-ledger `last_session_ok` timestamp when cheaply
    /// derivable. `None` → render the bare honest `auth=unknown`.
    pub last_session_ok: Option<String>,
    pub budget: BudgetState,
}

/// Render the vendor panel: a header line + one aligned line per vendor.
/// Not-installed vendors render `installed=no` only (auth/budget are moot).
pub(crate) fn render_panel(header: &PanelHeader, rows: &[PanelRow]) -> String {
    let mut out = format!(
        "vendors (project={}, bound host={}, host_online={}, observed={}, stale={}):",
        header.project, header.host, header.host_online, header.observed, header.stale,
    );
    if let Some(note) = &header.note {
        out.push_str(&format!("\n  note: {note}"));
    }
    if rows.is_empty() {
        out.push_str("\n  (no vendor snapshot available for this host)");
        return out;
    }
    let vendor_w = rows
        .iter()
        .map(|r| r.vendor.len())
        .max()
        .unwrap_or(6)
        .max(6);
    for row in rows {
        if !row.installed {
            out.push_str(&format!("\n  {:<vendor_w$}  installed=no", row.vendor));
            continue;
        }
        let ver = row
            .version
            .as_deref()
            .map(|v| format!(" {v}"))
            .unwrap_or_default();
        let installed_seg = format!("installed=yes{ver}");
        let auth = match &row.last_session_ok {
            Some(ts) => format!("auth=unknown(last_session_ok {ts})"),
            None => "auth=unknown".to_string(),
        };
        out.push_str(&format!(
            "\n  {:<vendor_w$}  {:<28}  {}  budget={}",
            row.vendor,
            installed_seg,
            auth,
            row.budget.render(),
        ));
    }
    out
}

// ── advisory model catalogs (pure) ─────────────────────────────────────────

fn compact_token(value: &str) -> String {
    let mut out: String = value.chars().take(CATALOG_TOKEN_CHARS).collect();
    if value.chars().count() > CATALOG_TOKEN_CHARS {
        out.push('…');
    }
    out
}

fn compact_list(values: &[String], limit: usize) -> String {
    let mut rendered: Vec<String> = values
        .iter()
        .take(limit)
        .map(|value| compact_token(value))
        .collect();
    if values.len() > limit {
        rendered.push(format!("… +{}", values.len() - limit));
    }
    rendered.join(", ")
}

fn compact_timestamp(value: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|ts| {
            ts.with_timezone(&chrono::Utc)
                .format("%Y-%m-%dT%H:%MZ")
                .to_string()
        })
        .unwrap_or_else(|_| compact_token(value))
}

/// Render the runtime and hub catalogs as explicitly separate advisory
/// sources. The third source — user aliases/preferences — remains the
/// separately-labelled routing-notes block immediately below this one.
pub(crate) fn render_catalog(
    runtime: &ccteam_core::model_catalog::ModelCatalog,
    hub: &crate::hub::HubModelsState,
) -> String {
    let mut out =
        String::from("catalog (advisory, never a spawn allowlist; sources kept separate):");
    let runtime_rows: Vec<_> = runtime
        .0
        .iter()
        .filter(|(_, entry)| !entry.models.is_empty())
        .collect();
    if runtime_rows.is_empty() {
        out.push_str("\n  runtime: unavailable (no vendor catalog observed yet)");
    } else {
        for (vendor, entry) in runtime_rows.iter().take(CATALOG_VENDOR_LIMIT) {
            let ids: Vec<String> = entry.models.iter().map(|model| model.id.clone()).collect();
            out.push_str(&format!(
                "\n  runtime(last-seen {}): {}=[{}]",
                compact_timestamp(&entry.observed_at),
                compact_token(vendor),
                compact_list(&ids, CATALOG_IDS_PER_VENDOR),
            ));
        }
        if runtime_rows.len() > CATALOG_VENDOR_LIMIT {
            out.push_str(&format!(
                "\n  runtime: … +{} vendors",
                runtime_rows.len() - CATALOG_VENDOR_LIMIT
            ));
        }
    }

    match hub {
        crate::hub::HubModelsState::Unavailable => out.push_str("\n  hub: unavailable"),
        crate::hub::HubModelsState::Available(snapshot) => {
            let revision: String = snapshot.revision.chars().take(7).collect();
            let stale = if snapshot.stale { ", stale=true" } else { "" };
            if snapshot.catalog.vendors.is_empty() {
                out.push_str(&format!(
                    "\n  hub(models.json@{revision}{stale}): no vendor entries"
                ));
            } else {
                for (vendor, entry) in snapshot.catalog.vendors.iter().take(CATALOG_VENDOR_LIMIT) {
                    let ids: Vec<String> =
                        entry.models.iter().map(|model| model.id.clone()).collect();
                    let aliases: Vec<String> = entry
                        .models
                        .iter()
                        .flat_map(|model| {
                            model.aliases.iter().map(|alias| {
                                format!("{}={}", compact_token(alias), compact_token(&model.id))
                            })
                        })
                        .collect();
                    let default = entry.default.as_deref().unwrap_or("unspecified");
                    let aliases = if aliases.is_empty() {
                        "none".to_string()
                    } else {
                        compact_list(&aliases, CATALOG_ALIASES_PER_VENDOR)
                    };
                    out.push_str(&format!(
                        "\n  hub(models.json@{revision}{stale}): {}: default={}; ids=[{}]; aliases {}",
                        compact_token(vendor),
                        compact_token(default),
                        compact_list(&ids, CATALOG_IDS_PER_VENDOR),
                        aliases,
                    ));
                }
                if snapshot.catalog.vendors.len() > CATALOG_VENDOR_LIMIT {
                    out.push_str(&format!(
                        "\n  hub(models.json@{revision}{stale}): … +{} vendors",
                        snapshot.catalog.vendors.len() - CATALOG_VENDOR_LIMIT
                    ));
                }
            }
        }
    }
    out
}

// ── routing notes (pure) ────────────────────────────────────────────────────

/// A routing-notes file found on disk.
#[derive(Debug, Clone)]
pub(crate) struct RoutingFile {
    /// Absolute source path (the full-path pointer given on truncation).
    pub path: String,
    /// Raw bytes (rendered verbatim, capped; never parsed).
    pub bytes: Vec<u8>,
    /// RFC3339 file mtime (or "" when unavailable).
    pub updated_at: String,
}

/// Lower-hex sha256 (no `hex` crate; mirrors `hub::sha256_hex`).
fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        out.push(char::from_digit((b >> 4) as u32, 16).unwrap());
        out.push(char::from_digit((b & 0x0f) as u32, 16).unwrap());
    }
    out
}

/// Render the routing-notes section. `found = None` → a single honest pointer
/// line telling the planner it can create one. The content is wrapped with
/// `source`/`sha256`/`updated_at`/`truncated` then the (capped) raw markdown;
/// ccteam never parses it.
pub(crate) fn render_routing_notes(found: Option<&RoutingFile>) -> String {
    let Some(file) = found else {
        return String::from(
            "routing notes: none configured — create ~/.ccteam/routing.md \
             (or the project-specific <project>/.ccteam/routing.md) with your \
             vendor/model routing preferences; ccteam passes it through verbatim (advisory user \
             text, never parsed). No hub default guide is wired yet — just be explicit.",
        );
    };
    let sha = sha256_hex(&file.bytes);
    let text = String::from_utf8_lossy(&file.bytes);
    let path = file.path.clone();
    let bounded =
        crate::delegation::truncate_head_tail_with_marker(&text, ROUTING_NOTES_MAX_CHARS, |n| {
            format!("\n…[{n} chars omitted — full note at {path}]…\n")
        });
    format!(
        "routing notes (source={} sha256={} updated_at={} truncated={}) (advisory user text):\n{}",
        file.path, sha, file.updated_at, bounded.truncated, bounded.text,
    )
}

// ── spawn-failure discovery (pure) ─────────────────────────────────────────

/// The `session_spawn` discovery error for a vendor that is not installed on
/// the project's bound host. Lists the installed vendors on THAT host (from
/// the same snapshot) + freshness, and keeps model ids advisory (a fresh
/// install can retry). Never a local fallback; never blocks on auth.
pub(crate) fn spawn_unavailable_message(
    vendor: &str,
    host: &str,
    installed_vendors: &[String],
    freshness: &str,
) -> String {
    let installed = if installed_vendors.is_empty() {
        "none".to_string()
    } else {
        installed_vendors.join(", ")
    };
    format!(
        "session_spawn: vendor `{vendor}` is not installed on host `{host}` \
         (installed there: {installed}; observed {freshness}). Spawn one of the installed \
         vendors, or install `{vendor}` on that host and retry — ccteam never installs a CLI \
         for you. Model ids stay advisory (ccteam does not validate them), so a genuinely fresh \
         install can just retry."
    )
}

// ── gather helpers (blocking: probe / read fs) ──────────────────────────────

/// Read routing notes for an optional project: project-owned first
/// (`<project>/.ccteam/routing.md`), then the global `~/.ccteam/routing.md`.
/// A fleet-level caller (`slug = None`) reads only the global file. The two
/// files are alternatives, never merged.
pub(crate) fn read_routing_file(paths: &CcteamPaths, slug: Option<&str>) -> Option<RoutingFile> {
    let project_specific = slug.map(|slug| paths.project_routing_notes(slug));
    let global = paths.global_routing_notes();
    let path = project_specific
        .filter(|path| path.is_file())
        .or_else(|| global.is_file().then_some(global))?;
    let bytes = std::fs::read(&path).ok()?;
    let updated_at = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok()
        .map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339())
        .unwrap_or_default();
    Some(RoutingFile {
        path: path.display().to_string(),
        bytes,
        updated_at,
    })
}

/// Minimal view over a project `workflow.yaml` to read its per-vendor budget
/// caps without pulling `ccteam-flow` in (matches the on-disk `budgets_v060`
/// key + the web `/status` route's reader).
#[derive(Debug, Default, serde::Deserialize)]
struct WorkflowBudgetView {
    #[serde(default)]
    budgets_v060: Option<ccteam_cost::Budgets>,
}

/// Read a project's per-vendor budget caps from its `workflow.yaml` (nested
/// `.ccteam/` first, then the project root — the `ccteam_core` precedence).
/// Any miss → `None` (a project without budgets contributes no caps).
pub(crate) fn budgets_for_project(project_dir: &Path) -> Option<ccteam_cost::Budgets> {
    let nested = project_dir.join(".ccteam").join("workflow.yaml");
    let direct = project_dir.join("workflow.yaml");
    let path = if nested.exists() { nested } else { direct };
    let raw = std::fs::read_to_string(&path).ok()?;
    let view: WorkflowBudgetView = serde_yaml::from_str(&raw).ok()?;
    view.budgets_v060
}

/// Cost cap for a vendor from an optional `Budgets` (wire-name keyed).
fn vendor_cap(budgets: Option<&ccteam_cost::Budgets>, vendor: &str) -> Option<f64> {
    let budgets = budgets?;
    let v = match vendor {
        "claude" => ccteam_cost::Vendor::Claude,
        "codex" => ccteam_cost::Vendor::Codex,
        "grok" => ccteam_cost::Vendor::Grok,
        "opencode" => ccteam_cost::Vendor::Opencode,
        "kimi" => ccteam_cost::Vendor::Kimi,
        _ => return None,
    };
    budgets.cap_for(v).max_cost_usd_per_24h
}

/// Build the per-vendor budget row for `vendor` given the project's caps +
/// its trailing-24h per-vendor spend.
fn budget_row(
    vendor: &str,
    budgets: Option<&ccteam_cost::Budgets>,
    spend_24h: &BTreeMap<String, f64>,
) -> BudgetState {
    let priced = vendor_is_priced(vendor);
    let cap = vendor_cap(budgets, vendor);
    let spend = spend_24h.get(vendor).copied().unwrap_or(0.0);
    classify_budget(priced, cap, spend)
}

/// Local-host vendor rows: live (cached) probe + budget posture.
fn local_rows(
    availability: &[VendorAvailability],
    budgets: Option<&ccteam_cost::Budgets>,
    spend_24h: &BTreeMap<String, f64>,
) -> Vec<PanelRow> {
    availability
        .iter()
        .map(|a| PanelRow {
            vendor: a.vendor.to_string(),
            installed: a.installed,
            version: a.version.clone(),
            last_session_ok: None,
            budget: budget_row(a.vendor, budgets, spend_24h),
        })
        .collect()
}

/// Satellite-host vendor rows: from the host's LAST control-channel report
/// (never the local machine's probe). Budget posture still comes from the
/// project's caps + the daemon's own cost ledger (recorded under the catalog
/// slug regardless of execution host).
fn satellite_rows(
    rec: &HostRecord,
    budgets: Option<&ccteam_cost::Budgets>,
    spend_24h: &BTreeMap<String, f64>,
) -> Vec<PanelRow> {
    rec.agents
        .iter()
        .map(|a| PanelRow {
            vendor: a.vendor.clone(),
            installed: a.installed,
            version: a.version.clone(),
            last_session_ok: None,
            budget: budget_row(&a.vendor, budgets, spend_24h),
        })
        .collect()
}

/// Unix seconds → RFC3339.
fn unix_to_rfc3339(secs: u64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(secs as i64, 0)
        .map(|t| t.to_rfc3339())
        .unwrap_or_default()
}

/// Render the whole appended section (vendor panel + routing notes) for a
/// resolved project slug. `slug = None` → no project resolved (admin/local
/// caller outside any registered project): render the LOCAL host with a note,
/// and the global routing notes. BLOCKING (probes + fs reads).
pub(crate) fn render_section(
    paths: &CcteamPaths,
    slug: Option<&str>,
    hub: &crate::hub::HubModelsState,
) -> String {
    let (header, rows) = match slug {
        Some(slug) => build_project_panel(paths, slug),
        None => build_local_panel(paths, None, Some("no project resolved — showing the local host; pass `project` or run inside a registered project directory".to_string())),
    };
    let notes = read_routing_file(paths, slug);
    let runtime = ccteam_core::model_catalog::load_model_catalog_in(&paths.root);
    format!(
        "{}{}\n\n{}\n\n{}",
        render_panel(&header, &rows),
        render_recipes(&rows),
        render_catalog(&runtime, hub),
        render_routing_notes(notes.as_ref()),
    )
}

/// MCP-BEACON-1 — one `session_spawn` recipe line per INSTALLED vendor, so
/// discovery → execution is a single hop (the external-agent complaint: the
/// panel said grok exists but not how to call it). Static text assembled
/// from the probe rows — no model, no routing opinion beyond what the spawn
/// tool description already states. Empty when nothing is installed (the
/// panel rows already say so).
fn render_recipes(rows: &[PanelRow]) -> String {
    let mut lines: Vec<String> = Vec::new();
    for row in rows.iter().filter(|r| r.installed) {
        let recipe = match row.vendor.as_str() {
            "grok" => {
                "session_spawn{vendor:\"grok\", task:\"…\", wait_seconds:120} — fast live web/X search, inline answer"
            }
            "claude" => "session_spawn{vendor:\"claude\", task:\"…\"} — coding agent for repo work",
            "codex" => {
                "session_spawn{vendor:\"codex\", task:\"…\"} — coding agent (long grinds; async + ONE completion notification)"
            }
            "kimi" => "session_spawn{vendor:\"kimi\", task:\"…\"}",
            "opencode" => "session_spawn{vendor:\"opencode\", task:\"…\"}",
            _ => continue,
        };
        lines.push(format!("  {recipe}"));
    }
    if lines.is_empty() {
        return String::new();
    }
    format!(
        "\nrecipes (installed vendors):\n{}\n  then: session_collect{{sid, tail:true}} reads the final answer · session_dispatch{{sid, task}} sends follow-ups",
        lines.join("\n")
    )
}

/// Panel for a resolved project: local vs satellite by its catalog host
/// binding.
fn build_project_panel(paths: &CcteamPaths, slug: &str) -> (PanelHeader, Vec<PanelRow>) {
    let entry = ccteam_core::config::lookup_project(&paths.root, slug)
        .ok()
        .flatten();
    let host = entry
        .as_ref()
        .map(|e| {
            if e.host.trim().is_empty() {
                LOCAL_HOST.to_string()
            } else {
                e.host.clone()
            }
        })
        .unwrap_or_else(|| LOCAL_HOST.to_string());
    let project_dir = entry.as_ref().map(|e| e.path.clone());
    let budgets = project_dir.as_deref().and_then(budgets_for_project);
    let spend_24h = ccteam_core::queries::cost_summary(slug, &paths.progress_jsonl(slug), paths)
        .map(|s| s.cost_24h_by_vendor)
        .unwrap_or_default();

    if host == LOCAL_HOST {
        let availability = ccteam_core::host_registry::probe_availability(false);
        let header = PanelHeader {
            project: slug.to_string(),
            host,
            host_online: true,
            observed: "just now".to_string(),
            stale: false,
            note: None,
        };
        (
            header,
            local_rows(&availability, budgets.as_ref(), &spend_24h),
        )
    } else {
        satellite_panel(paths, slug, &host, budgets.as_ref(), &spend_24h)
    }
}

/// Local-host panel with no bound project (admin/fleet caller).
fn build_local_panel(
    paths: &CcteamPaths,
    slug: Option<&str>,
    note: Option<String>,
) -> (PanelHeader, Vec<PanelRow>) {
    let availability = ccteam_core::host_registry::probe_availability(false);
    let (budgets, spend_24h) = match slug {
        Some(slug) => {
            let entry = ccteam_core::config::lookup_project(&paths.root, slug)
                .ok()
                .flatten();
            let budgets = entry
                .as_ref()
                .map(|e| e.path.clone())
                .as_deref()
                .and_then(budgets_for_project);
            let spend =
                ccteam_core::queries::cost_summary(slug, &paths.progress_jsonl(slug), paths)
                    .map(|s| s.cost_24h_by_vendor)
                    .unwrap_or_default();
            (budgets, spend)
        }
        None => (None, BTreeMap::new()),
    };
    let header = PanelHeader {
        project: slug.unwrap_or("(none)").to_string(),
        host: LOCAL_HOST.to_string(),
        host_online: true,
        observed: "just now".to_string(),
        stale: false,
        note,
    };
    (
        header,
        local_rows(&availability, budgets.as_ref(), &spend_24h),
    )
}

/// Satellite-host panel: render from the last control-channel report; offline
/// / unknown → `host_online=false, stale=true` (never the local probe).
fn satellite_panel(
    paths: &CcteamPaths,
    slug: &str,
    host: &str,
    budgets: Option<&ccteam_cost::Budgets>,
    spend_24h: &BTreeMap<String, f64>,
) -> (PanelHeader, Vec<PanelRow>) {
    let rec = ccteam_core::HostRegistry::load(&paths.host_registry_path())
        .ok()
        .and_then(|reg| reg.get(host).cloned());
    match rec {
        Some(rec) => {
            let online = rec.is_online(DEFAULT_HEARTBEAT_TTL_SECS);
            let header = PanelHeader {
                project: slug.to_string(),
                host: host.to_string(),
                host_online: online,
                observed: unix_to_rfc3339(rec.last_heartbeat_unix),
                stale: !online,
                note: (!online).then(|| {
                    format!("host `{host}` is offline — showing its last report; NOT the local machine's capabilities")
                }),
            };
            (header, satellite_rows(&rec, budgets, spend_24h))
        }
        None => {
            let header = PanelHeader {
                project: slug.to_string(),
                host: host.to_string(),
                host_online: false,
                observed: "never".to_string(),
                stale: true,
                note: Some(format!(
                    "host `{host}` is not registered — no report yet; NOT substituting local capabilities"
                )),
            };
            (header, Vec::new())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header() -> PanelHeader {
        PanelHeader {
            project: "demo".to_string(),
            host: "local".to_string(),
            host_online: true,
            observed: "just now".to_string(),
            stale: false,
            note: None,
        }
    }

    #[test]
    fn budget_unpriced_for_vendor_without_table() {
        // Opencode/Kimi have no bundled table → unpriced regardless of cap.
        assert_eq!(
            classify_budget(false, Some(5.0), 0.0),
            BudgetState::Unpriced
        );
        assert_eq!(classify_budget(false, None, 99.0), BudgetState::Unpriced);
    }

    #[test]
    fn budget_not_configured_when_no_cap() {
        assert_eq!(classify_budget(true, None, 3.0), BudgetState::NotConfigured);
        assert_eq!(
            classify_budget(true, Some(0.0), 3.0),
            BudgetState::NotConfigured
        );
    }

    #[test]
    fn budget_ok_and_disabled_across_the_cap() {
        assert_eq!(classify_budget(true, Some(10.0), 4.0), BudgetState::Ok);
        // At/over the cap → disabled with an approximate recovery window.
        match classify_budget(true, Some(2.0), 8.0) {
            BudgetState::Disabled { approx_hours } => {
                assert!((1..=24).contains(&approx_hours), "hours {approx_hours}");
            }
            other => panic!("expected disabled, got {other:?}"),
        }
        assert_eq!(
            classify_budget(true, Some(5.0), 5.0),
            BudgetState::Disabled { approx_hours: 1 },
            "spend == cap trips disabled (window just cleared)"
        );
    }

    /// MCP-BEACON-1 — recipes list INSTALLED vendors only (one spawn
    /// one-liner each + the collect/dispatch footer); nothing installed →
    /// empty string (no dangling header).
    #[test]
    fn recipes_render_installed_vendors_only() {
        let row = |vendor: &str, installed: bool| PanelRow {
            vendor: vendor.to_string(),
            installed,
            version: None,
            last_session_ok: None,
            budget: BudgetState::NotConfigured,
        };
        let out = render_recipes(&[
            row("claude", true),
            row("codex", false),
            row("grok", true),
            row("kimi", false),
            row("opencode", false),
        ]);
        assert!(out.contains("recipes (installed vendors):"), "{out}");
        assert!(
            out.contains("session_spawn{vendor:\"grok\", task:\"…\", wait_seconds:120}"),
            "{out}"
        );
        assert!(out.contains("session_spawn{vendor:\"claude\""), "{out}");
        assert!(!out.contains("vendor:\"codex\""), "{out}");
        assert!(!out.contains("vendor:\"kimi\""), "{out}");
        assert!(out.contains("session_collect{sid, tail:true}"), "{out}");

        assert_eq!(render_recipes(&[row("claude", false)]), "");
        assert_eq!(render_recipes(&[]), "");
    }

    #[test]
    fn panel_renders_installed_and_missing_rows() {
        let rows = vec![
            PanelRow {
                vendor: "claude".to_string(),
                installed: true,
                version: Some("claude 1.2.3".to_string()),
                last_session_ok: None,
                budget: BudgetState::Ok,
            },
            PanelRow {
                vendor: "codex".to_string(),
                installed: true,
                version: None,
                last_session_ok: None,
                budget: BudgetState::Disabled { approx_hours: 3 },
            },
            PanelRow {
                vendor: "grok".to_string(),
                installed: false,
                version: None,
                last_session_ok: None,
                budget: BudgetState::NotConfigured,
            },
            PanelRow {
                vendor: "kimi".to_string(),
                installed: true,
                version: Some("kimi 0.1".to_string()),
                last_session_ok: None,
                budget: BudgetState::Unpriced,
            },
        ];
        let out = render_panel(&header(), &rows);
        assert!(out.contains("vendors (project=demo, bound host=local, host_online=true"));
        assert!(out.contains("claude") && out.contains("installed=yes claude 1.2.3"));
        assert!(out.contains("auth=unknown"));
        assert!(out.contains("budget=ok"));
        assert!(out.contains("budget=disabled(~3h)"));
        // A not-installed vendor shows only installed=no (no auth/budget noise).
        let grok_line = out.lines().find(|l| l.contains("grok")).unwrap();
        assert!(grok_line.contains("installed=no"));
        assert!(!grok_line.contains("auth="));
        // Unpriced vendor renders unpriced, never $0.
        assert!(out.contains("budget=unpriced"));
        // NEVER fakes ready.
        assert!(!out.contains("auth=ready"));
    }

    #[test]
    fn panel_offline_satellite_note_renders() {
        let mut h = header();
        h.host = "sat-lab".to_string();
        h.host_online = false;
        h.stale = true;
        h.note = Some("host `sat-lab` is offline — showing its last report".to_string());
        let out = render_panel(&h, &[]);
        assert!(out.contains("host_online=false"));
        assert!(out.contains("stale=true"));
        assert!(out.contains("offline"));
        assert!(out.contains("no vendor snapshot available"));
    }

    #[test]
    fn catalog_absent_renders_two_honest_source_lines() {
        let out = render_catalog(
            &ccteam_core::model_catalog::ModelCatalog::default(),
            &crate::hub::HubModelsState::Unavailable,
        );
        assert!(out.contains("advisory, never a spawn allowlist"));
        assert!(out.contains("runtime: unavailable"));
        assert!(out.contains("hub: unavailable"));
    }

    #[test]
    fn catalog_present_keeps_runtime_and_hub_separate_and_bounded() {
        let runtime = ccteam_core::model_catalog::ModelCatalog(BTreeMap::from([(
            "codex".to_string(),
            ccteam_core::model_catalog::VendorModelCatalog {
                observed_at: "2026-07-19T10:30:00Z".to_string(),
                source: "codex model/list".to_string(),
                models: ["a", "b", "c", "d", "e", "f"]
                    .iter()
                    .map(|id| ccteam_core::model_catalog::CatalogModel {
                        id: (*id).to_string(),
                        display_name: None,
                        efforts: Vec::new(),
                    })
                    .collect(),
            },
        )]));
        let hub = crate::hub::HubModelsState::Available(crate::hub::HubModelsSnapshot {
            catalog: crate::hub::HubModelsCatalog {
                schema: "ccteam.models/v1".to_string(),
                updated_at: "2026-07-20T00:00:00Z".to_string(),
                vendors: BTreeMap::from([(
                    "claude".to_string(),
                    crate::hub::HubVendorModels {
                        default: Some("sonnet".to_string()),
                        models: vec![crate::hub::HubModel {
                            id: "opus".to_string(),
                            display_name: Some("Claude Opus".to_string()),
                            aliases: vec!["deep".to_string(), "refactor".to_string()],
                            context_window: Some(200_000),
                        }],
                    },
                )]),
            },
            revision: "abcdef0123456789".to_string(),
            stale: true,
        });

        let out = render_catalog(&runtime, &hub);
        assert!(out.contains("runtime(last-seen 2026-07-19T10:30Z): codex=[a, b, c, d, … +2]"));
        assert!(out.contains("hub(models.json@abcdef0, stale=true): claude:"));
        assert!(out.contains("default=sonnet"));
        assert!(out.contains("aliases deep=opus, refactor=opus"));
        assert_eq!(out.matches("runtime(last-seen").count(), 1);
        assert_eq!(out.matches("hub(models.json@").count(), 1);
    }

    #[test]
    fn routing_notes_missing_gives_pointer() {
        let out = render_routing_notes(None);
        assert!(out.contains("none configured"));
        assert!(out.contains("~/.ccteam/routing.md"));
        assert!(out.contains("<project>/.ccteam/routing.md"));
        assert!(out.contains("never parsed"));
    }

    #[test]
    fn routing_notes_prefer_project_file_then_global_and_ignore_retired_path() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let global = paths.root.join("routing.md");
        let retired = paths.root.join("routing").join("projects").join("demo.md");
        let project = paths
            .projects_root
            .join("demo")
            .join(".ccteam")
            .join("routing.md");
        std::fs::create_dir_all(global.parent().unwrap()).unwrap();
        std::fs::create_dir_all(retired.parent().unwrap()).unwrap();
        std::fs::write(&global, "global").unwrap();
        std::fs::write(&retired, "retired").unwrap();

        let found = read_routing_file(&paths, Some("demo")).unwrap();
        assert_eq!(found.path, global.display().to_string());
        assert_eq!(found.bytes, b"global");

        std::fs::create_dir_all(project.parent().unwrap()).unwrap();
        std::fs::write(&project, "project").unwrap();
        let found = read_routing_file(&paths, Some("demo")).unwrap();
        assert_eq!(found.path, project.display().to_string());
        assert_eq!(found.bytes, b"project");

        let fleet_found = read_routing_file(&paths, None).unwrap();
        assert_eq!(fleet_found.path, global.display().to_string());
        assert_eq!(fleet_found.bytes, b"global");
    }

    #[test]
    fn routing_notes_wrapper_carries_source_sha_and_untruncated() {
        let file = RoutingFile {
            path: "/home/u/.ccteam/routing.md".to_string(),
            bytes: b"# Routing\nUI -> fable\nrefactor -> opus\n".to_vec(),
            updated_at: "2026-07-21T00:00:00+00:00".to_string(),
        };
        let out = render_routing_notes(Some(&file));
        assert!(out.contains("source=/home/u/.ccteam/routing.md"));
        assert!(out.contains("sha256="));
        assert!(out.contains("updated_at=2026-07-21T00:00:00+00:00"));
        assert!(out.contains("truncated=false"));
        assert!(out.contains("UI -> fable"));
    }

    #[test]
    fn routing_notes_truncates_long_body_with_pointer() {
        let big = "x".repeat(ROUTING_NOTES_MAX_CHARS * 3);
        let file = RoutingFile {
            path: "/home/u/.ccteam/routing.md".to_string(),
            bytes: big.into_bytes(),
            updated_at: String::new(),
        };
        let out = render_routing_notes(Some(&file));
        assert!(out.contains("truncated=true"));
        assert!(out.contains("chars omitted — full note at /home/u/.ccteam/routing.md"));
        // The whole section stays bounded (header + cap + marker), not 3x cap.
        assert!(out.chars().count() < ROUTING_NOTES_MAX_CHARS + 500);
    }

    #[test]
    fn spawn_unavailable_lists_installed_set_and_stays_advisory() {
        let msg = spawn_unavailable_message(
            "grok",
            "local",
            &["claude".to_string(), "codex".to_string()],
            "just now",
        );
        assert!(msg.contains("vendor `grok` is not installed on host `local`"));
        assert!(msg.contains("installed there: claude, codex"));
        assert!(msg.contains("observed just now"));
        assert!(msg.contains("advisory"));
        // Never a local fallback; never installs.
        assert!(msg.contains("never installs a CLI"));
    }

    #[test]
    fn spawn_unavailable_handles_empty_installed_set() {
        let msg = spawn_unavailable_message("codex", "sat-lab", &[], "42s ago");
        assert!(msg.contains("installed there: none"));
        assert!(msg.contains("observed 42s ago"));
    }

    fn ctx(slug: &str) -> CallerCtx {
        CallerCtx {
            sid: "s3".to_string(),
            slug: slug.to_string(),
            role: String::new(),
            depth: 1,
        }
    }

    #[test]
    fn ambient_caller_is_pinned_to_own_project_ignoring_self_report() {
        // A session principal is scoped to its OWN project: even a lying
        // caller (project="victim" / _caller_slug="victim") resolves to the
        // authenticated ctx.slug — it can NEVER query another project's host.
        let got = resolve_status_project(
            McpCaller::Ambient,
            Some("victim"),
            Some("victim"),
            Some(&ctx("mine")),
        )
        .unwrap();
        assert_eq!(got.as_deref(), Some("mine"));
    }

    #[test]
    fn ambient_caller_without_principal_is_withheld() {
        // No valid principal → error (panel withheld, no cross-project leak).
        let err =
            resolve_status_project(McpCaller::Ambient, Some("victim"), None, None).unwrap_err();
        assert!(err.contains("scoped to your"));
    }

    #[test]
    fn admin_caller_prefers_explicit_project_then_cwd_slug() {
        assert_eq!(
            resolve_status_project(McpCaller::Admin, Some("chosen"), Some("cwd"), None)
                .unwrap()
                .as_deref(),
            Some("chosen"),
        );
        assert_eq!(
            resolve_status_project(McpCaller::Admin, None, Some("cwd"), None)
                .unwrap()
                .as_deref(),
            Some("cwd"),
        );
        // Blank args → no project (fleet caller; panel shows the local host).
        assert_eq!(
            resolve_status_project(McpCaller::Admin, Some("  "), None, None).unwrap(),
            None,
        );
    }
}
