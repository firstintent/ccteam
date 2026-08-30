//! The MCP `status` body: which agents a project's host can hire, what the
//! team spent, and — only when asked for — model catalogs, per-vendor
//! install/budget rows and the user's routing notes.
//!
//! All PULL-only (the model asks; nothing is injected into any prompt) and all
//! JSON: the answer is data a caller reads, not a screen a human reads. The
//! human-facing panel lives in IM `/status` and the web console.
//!
//! **Tiered on purpose.** `brief` is what a caller almost always wants — can I
//! hire, and what has it cost — and it is a few hundred bytes. Everything else
//! (`models` / `vendors` / `routing` / `full`) is a `detail` away, so the
//! callers who never need it never pay for it.
//!
//! Honesty rules that survive every tier: only INSTALLED vendors appear in
//! `hire`; `auth` is always `unknown` (ccteam never probes vendor credential
//! files nor fakes `ready`); an offline satellite renders its LAST report
//! marked stale, never the local machine's capabilities; runtime and hub
//! catalogs stay separately labelled and advisory (never a spawn allowlist);
//! routing notes pass through verbatim.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ccteam_core::host_registry::{HostRecord, VendorAvailability};
use ccteam_core::{CcteamPaths, DEFAULT_HEARTBEAT_TTL_SECS, LOCAL_HOST};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use super::dispatch::McpCaller;
use crate::gateway::CallerCtx;

/// Resolve which project the `status` body is scoped to.
///
/// - **Ambient** (session principal): ALWAYS the authenticated caller's own
///   project (`ctx.slug`). Any self-reported `project_arg`/`caller_slug_arg`
///   is ignored — this is the security property that a session principal can
///   never query another project's host. A missing/failed principal → `Err`.
/// - **Admin** (the local mcp.sock admin-token tier — never an HTTP caller):
///   the explicit `project_arg`, else a supplied `caller_slug_arg` (nothing
///   ccteam ships injects one since the stdio forwarder was deleted), else
///   `None` (fleet caller with no bound project).
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

/// `status` for an enrolled binding that has authenticated but named no
/// workspace yet.
///
/// The generic ambient refusal ("caller could not be authenticated") was a lie
/// here — the credential verified fine, it simply carries no project — and it
/// sent a hand-started agent looking for a broken bearer (measured
/// 2026-08-31). Same shape as the answer `agent` / `agent_read` give it: what
/// is missing, how to supply it, and which slugs are reachable.
pub(crate) fn enrolled_unbound_status_note(reachable: &[String]) -> String {
    let mut note = "status: no project named yet — this MCP session is authenticated, but \
         project-scoped host, budget and routing details need a workspace. Name one on your next \
         `agent` call (`project:\"<slug>\"`)"
        .to_string();
    if reachable.is_empty() {
        note.push_str(
            "; no project is registered for this credential's owner yet — create one in the web \
             console first.",
        );
    } else {
        note.push_str(&format!("; reachable: {}.", reachable.join(", ")));
    }
    note
}

/// Cap for the routing-notes body (chars). A note beyond this keeps a 70/30
/// head-tail excerpt with a full-path pointer (aligns with the delegation
/// truncation family).
pub(crate) const ROUTING_NOTES_MAX_CHARS: usize = 4000;

/// Per-vendor cap on the model ids any catalog tier lists. Ids ride verbatim
/// (a truncated id is an unusable id); only the COUNT is bounded.
const CATALOG_IDS_PER_VENDOR: usize = 8;
const CATALOG_ALIASES_PER_VENDOR: usize = 8;

/// Which `status` tier the caller asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StatusDetail {
    Brief,
    Models,
    Vendors,
    Routing,
    Full,
}

impl StatusDetail {
    /// Parse the `detail` argument; absent/empty = brief.
    pub(crate) fn parse(raw: Option<&str>) -> Result<Self, String> {
        match raw.map(str::trim).unwrap_or("") {
            "" | "brief" => Ok(Self::Brief),
            "models" => Ok(Self::Models),
            "vendors" => Ok(Self::Vendors),
            "routing" => Ok(Self::Routing),
            "full" => Ok(Self::Full),
            other => Err(format!(
                "status: invalid `detail` `{other}` (expected `brief` | `models` | `vendors` | `routing` | `full`)"
            )),
        }
    }

    fn wants_models(self) -> bool {
        matches!(self, Self::Models | Self::Full)
    }

    fn wants_vendors(self) -> bool {
        matches!(self, Self::Vendors | Self::Full)
    }

    fn wants_routing(self) -> bool {
        matches!(self, Self::Routing | Self::Full)
    }
}

/// Vendors ccteam bundles a price table for (`anthropic`/`openai`/`xai`).
/// Everything else is `unpriced` — a USD budget can't be metered, never $0.
fn vendor_is_priced(vendor: &str) -> bool {
    matches!(vendor, "claude" | "codex" | "grok")
}

// ── budget posture ───────────────────────────────────────────────────────

/// Per-vendor budget posture for the status body.
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

// ── gathered facts ─────────────────────────────────────────────────────────

/// Header facts for one status body (the project + its bound host).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PanelHeader {
    pub project: String,
    pub host: String,
    pub host_online: bool,
    /// When the availability snapshot was observed (RFC3339 / "just now").
    pub observed: String,
    /// True when the snapshot is a stale last-report (offline satellite).
    pub stale: bool,
    /// Optional one-line note (e.g. "no project resolved" / "host offline").
    pub note: Option<String>,
}

/// One vendor row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PanelRow {
    pub vendor: String,
    pub installed: bool,
    pub version: Option<String>,
    pub budget: BudgetState,
}

/// Everything one `status` call needs, gathered once (probes + fs reads —
/// BLOCKING, so callers run it on `spawn_blocking`). The tier builders below
/// are pure functions of this.
#[derive(Debug, Clone)]
pub(crate) struct StatusPanel {
    pub header: PanelHeader,
    pub rows: Vec<PanelRow>,
    /// The project's trailing-24h cost across all vendors.
    pub cost_24h_usd: f64,
    pub runtime: ccteam_core::model_catalog::ModelCatalog,
    pub routing: Option<RoutingFile>,
    /// The paths that WOULD have held routing notes, when none does.
    pub routing_missing: Vec<String>,
    /// `~/.ccteam` — the effort-ladder lookup root.
    pub root: PathBuf,
}

// ── tier builders (pure) ───────────────────────────────────────────────────

/// The default body: can I hire here, and what has it cost today.
pub(crate) fn brief_value(panel: &StatusPanel) -> Map<String, Value> {
    let mut body = Map::new();
    body.insert("project".into(), json!(panel.header.project));
    body.insert("host".into(), json!(panel.header.host));
    body.insert("cost_24h_usd".into(), json!(panel.cost_24h_usd));
    body.insert("hire".into(), json!(installed_vendors(&panel.rows)));
    // Only ever present when something is WRONG: a healthy local host spends
    // no bytes saying it is healthy.
    if !panel.header.host_online {
        body.insert("host_online".into(), json!(false));
    }
    if panel.header.stale {
        body.insert("stale".into(), json!(true));
    }
    let disabled: Vec<&str> = panel
        .rows
        .iter()
        .filter(|row| matches!(row.budget, BudgetState::Disabled { .. }))
        .map(|row| row.vendor.as_str())
        .collect();
    if !disabled.is_empty() {
        body.insert("budget_disabled".into(), json!(disabled));
    }
    body
}

/// Vendors INSTALLED on the bound host — the only ones a spawn can succeed on.
fn installed_vendors(rows: &[PanelRow]) -> Vec<&str> {
    rows.iter()
        .filter(|row| row.installed)
        .map(|row| row.vendor.as_str())
        .collect()
}

/// `models` — what each vendor's own handshake reported, plus ccteam's effort
/// ladder for it. Advisory, never an allowlist: any model id rides to the
/// vendor verbatim and the vendor decides.
pub(crate) fn models_value(panel: &StatusPanel) -> Value {
    let mut vendors: BTreeMap<String, Value> = BTreeMap::new();
    let candidates: std::collections::BTreeSet<String> = panel
        .rows
        .iter()
        .filter(|row| row.installed)
        .map(|row| row.vendor.clone())
        .chain(panel.runtime.0.keys().cloned())
        .collect();
    for vendor in candidates {
        let entry = panel.runtime.0.get(&vendor);
        let ids: Vec<String> = entry
            .map(|entry| {
                entry
                    .models
                    .iter()
                    .take(CATALOG_IDS_PER_VENDOR)
                    .map(|model| model.id.clone())
                    .collect()
            })
            .unwrap_or_default();
        let efforts = ccteam_core::model_catalog::supported_efforts_in(&panel.root, &vendor);
        if ids.is_empty() && efforts.is_empty() {
            continue;
        }
        let mut row = Map::new();
        row.insert("ids".into(), json!(ids));
        if !efforts.is_empty() {
            row.insert("efforts".into(), json!(efforts));
        }
        // Provenance: an observation is dated so staleness is the reader's
        // call; no observation says nothing rather than passing ccteam's
        // pinned fallback off as a vendor fact.
        if let Some(entry) = entry.filter(|entry| !entry.models.is_empty()) {
            row.insert("seen".into(), json!(compact_timestamp(&entry.observed_at)));
        }
        vendors.insert(vendor, Value::Object(row));
    }
    json!(vendors)
}

/// `hub` — the second, SEPARATELY LABELLED catalog source (never merged with
/// the runtime one; a hub default is not an observation). `None` when the hub
/// catalog is unavailable.
pub(crate) fn hub_value(hub: &crate::hub::HubModelsState) -> Option<Value> {
    let crate::hub::HubModelsState::Available(snapshot) = hub else {
        return None;
    };
    let mut body = Map::new();
    body.insert(
        "revision".into(),
        json!(snapshot.revision.chars().take(7).collect::<String>()),
    );
    if snapshot.stale {
        body.insert("stale".into(), json!(true));
    }
    for (vendor, entry) in &snapshot.catalog.vendors {
        let mut row = Map::new();
        if let Some(default) = entry.default.as_deref() {
            row.insert("default".into(), json!(default));
        }
        row.insert(
            "ids".into(),
            json!(entry
                .models
                .iter()
                .take(CATALOG_IDS_PER_VENDOR)
                .map(|model| model.id.clone())
                .collect::<Vec<_>>()),
        );
        let aliases: Map<String, Value> = entry
            .models
            .iter()
            .flat_map(|model| {
                model
                    .aliases
                    .iter()
                    .map(|alias| (alias.clone(), json!(model.id)))
            })
            .take(CATALOG_ALIASES_PER_VENDOR)
            .collect();
        if !aliases.is_empty() {
            row.insert("aliases".into(), Value::Object(aliases));
        }
        body.insert(vendor.clone(), Value::Object(row));
    }
    Some(Value::Object(body))
}

/// `vendors` — one row per vendor on the bound host.
pub(crate) fn vendors_value(panel: &StatusPanel) -> Value {
    json!(panel
        .rows
        .iter()
        .map(|row| {
            let mut out = Map::new();
            out.insert("vendor".into(), json!(row.vendor));
            out.insert("installed".into(), json!(row.installed));
            if let Some(version) = row.version.as_deref() {
                out.insert("version".into(), json!(version));
            }
            // Honest by construction: ccteam never reads a vendor's credential
            // store, so it never claims a vendor is authenticated.
            out.insert("auth".into(), json!("unknown"));
            out.insert("budget".into(), json!(row.budget.render()));
            Value::Object(out)
        })
        .collect::<Vec<_>>())
}

/// The `note` line for the `vendors` / `full` tiers: the header's own caveat
/// (offline satellite, unregistered host, no project) plus every bridge notice
/// for a vendor whose tool surface ccteam cannot write a config file for.
pub(crate) fn vendors_note(panel: &StatusPanel) -> Option<String> {
    let mut notes: Vec<String> = Vec::new();
    if let Some(note) = panel.header.note.as_deref() {
        notes.push(note.to_string());
    }
    notes.extend(
        panel
            .rows
            .iter()
            .filter_map(|row| ccteam_core::host_registry::AgentProbeSpec::by_vendor(&row.vendor))
            .filter_map(ccteam_core::host_registry::AgentProbeSpec::tool_surface_notice),
    );
    (!notes.is_empty()).then(|| notes.join(" "))
}

/// `routing` — the user's advisory markdown, verbatim (capped) with its
/// provenance, or the paths where one could be created.
pub(crate) fn routing_value(panel: &StatusPanel) -> Value {
    let Some(file) = panel.routing.as_ref() else {
        return json!({ "missing": panel.routing_missing });
    };
    let sha = sha256_hex(&file.bytes);
    let text = String::from_utf8_lossy(&file.bytes);
    let path = file.path.clone();
    let bounded =
        crate::delegation::truncate_head_tail_with_marker(&text, ROUTING_NOTES_MAX_CHARS, |n| {
            format!("\n…[{n} chars omitted — full note at {path}]…\n")
        });
    json!({
        "source": file.path,
        "sha256": sha,
        "updated_at": file.updated_at,
        "truncated": bounded.truncated,
        "text": bounded.text,
    })
}

/// Compose the whole `status` body for one tier.
pub(crate) fn status_value(
    panel: &StatusPanel,
    hub: &crate::hub::HubModelsState,
    detail: StatusDetail,
) -> Map<String, Value> {
    let mut body = brief_value(panel);
    if detail.wants_models() {
        body.insert("models".into(), models_value(panel));
        if let Some(hub) = hub_value(hub) {
            body.insert("hub".into(), hub);
        }
    }
    if detail.wants_vendors() {
        body.insert("vendors".into(), vendors_value(panel));
        body.insert("observed".into(), json!(panel.header.observed));
        if let Some(note) = vendors_note(panel) {
            body.insert("note".into(), json!(note));
        }
    }
    if detail.wants_routing() {
        body.insert("routing".into(), routing_value(panel));
    }
    body
}

fn compact_timestamp(value: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|ts| {
            ts.with_timezone(&chrono::Utc)
                .format("%Y-%m-%dT%H:%MZ")
                .to_string()
        })
        .unwrap_or_else(|_| value.to_string())
}

// ── routing notes ───────────────────────────────────────────────────────────

/// A routing-notes file found on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
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

// ── spawn-failure discovery (pure) ─────────────────────────────────────────

/// The `agent` discovery error for a vendor that is not installed on the
/// project's bound host. Lists the installed vendors on THAT host (from the
/// same snapshot) + freshness, and keeps model ids advisory (a fresh install
/// can retry). Never a local fallback; never blocks on auth.
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
        "agent: vendor `{vendor}` is not installed on host `{host}` \
         (installed there: {installed}; observed {freshness}). Hire one of the installed \
         vendors, or install `{vendor}` on that host and retry — the admin can one-click \
         install npm-packaged vendors (claude/codex/grok/opencode/dsh) from the Ops & Hosts \
         web page; kimi/pi install manually. Model ids stay advisory (ccteam does not \
         validate them), so a genuinely fresh install can just retry."
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

/// The paths a caller could create routing notes at, most specific first.
fn routing_candidates(paths: &CcteamPaths, slug: Option<&str>) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(slug) = slug {
        out.push(paths.project_routing_notes(slug).display().to_string());
    }
    out.push(paths.global_routing_notes().display().to_string());
    out
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
        "pi" => ccteam_cost::Vendor::Pi,
        "dsh" => ccteam_cost::Vendor::Dsh,
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

/// Gather every fact a `status` call can need for a resolved project slug.
/// `slug = None` → no project resolved (admin/local caller outside any
/// registered project): the LOCAL host with a note, and the global routing
/// notes. BLOCKING (probes + fs reads).
pub(crate) fn gather_status_panel(paths: &CcteamPaths, slug: Option<&str>) -> StatusPanel {
    let (header, rows) = match slug {
        Some(slug) => build_project_panel(paths, slug),
        None => build_local_panel(
            paths,
            None,
            Some(
                "no project resolved — showing the local host; pass `project` or run inside a \
                 registered project directory"
                    .to_string(),
            ),
        ),
    };
    let cost_24h_usd = slug
        .map(|slug| {
            crate::progress_projection::ProgressProjection::new(paths.clone())
                .project_snapshot(slug)
                .cost
                .cost_24h_usd
        })
        .unwrap_or(0.0);
    StatusPanel {
        header,
        rows,
        cost_24h_usd,
        runtime: ccteam_core::model_catalog::load_model_catalog_in(&paths.root),
        routing: read_routing_file(paths, slug),
        routing_missing: routing_candidates(paths, slug),
        root: paths.root.clone(),
    }
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
    let spend_24h = crate::progress_projection::ProgressProjection::new(paths.clone())
        .project_snapshot(slug)
        .cost
        .cost_24h_by_vendor;

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
            let spend = crate::progress_projection::ProgressProjection::new(paths.clone())
                .project_snapshot(slug)
                .cost
                .cost_24h_by_vendor;
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

    fn panel_row(vendor: &str, installed: bool) -> PanelRow {
        PanelRow {
            vendor: vendor.to_string(),
            installed,
            version: None,
            budget: BudgetState::NotConfigured,
        }
    }

    fn panel(rows: Vec<PanelRow>) -> StatusPanel {
        StatusPanel {
            header: header(),
            rows,
            cost_24h_usd: 1.42,
            runtime: ccteam_core::model_catalog::ModelCatalog::default(),
            routing: None,
            routing_missing: vec![
                "/p/.ccteam/routing.md".to_string(),
                "/h/.ccteam/routing.md".to_string(),
            ],
            root: PathBuf::from("/nonexistent-root"),
        }
    }

    #[test]
    fn budget_unpriced_for_vendor_without_table() {
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

    #[test]
    fn detail_parses_every_tier_and_rejects_strangers() {
        assert_eq!(StatusDetail::parse(None).unwrap(), StatusDetail::Brief);
        assert_eq!(StatusDetail::parse(Some("")).unwrap(), StatusDetail::Brief);
        assert_eq!(
            StatusDetail::parse(Some("models")).unwrap(),
            StatusDetail::Models
        );
        assert_eq!(
            StatusDetail::parse(Some(" full ")).unwrap(),
            StatusDetail::Full
        );
        assert!(StatusDetail::parse(Some("everything")).is_err());
    }

    /// G2 — the default body is a few hundred bytes and lists only what a
    /// spawn can actually succeed on.
    #[test]
    fn brief_lists_installed_vendors_only_and_stays_small() {
        let panel = panel(vec![
            panel_row("claude", true),
            panel_row("codex", true),
            panel_row("grok", false),
        ]);
        let body = Value::Object(brief_value(&panel));
        assert_eq!(body["hire"], json!(["claude", "codex"]));
        assert_eq!(body["project"], "demo");
        assert_eq!(body["host"], "local");
        assert_eq!(body["cost_24h_usd"], 1.42);
        // A healthy local host spends no bytes saying so.
        assert!(body.get("host_online").is_none());
        assert!(body.get("stale").is_none());
        assert!(body.get("budget_disabled").is_none());
        let bytes = serde_json::to_string(&body).unwrap().len();
        assert!(bytes <= 300, "status brief is {bytes} B: {body}");
    }

    #[test]
    fn brief_names_disabled_budgets_only_when_some_vendor_is_over() {
        let mut panel = panel(vec![panel_row("claude", true), panel_row("codex", true)]);
        panel.rows[1].budget = BudgetState::Disabled { approx_hours: 3 };
        let body = Value::Object(brief_value(&panel));
        assert_eq!(body["budget_disabled"], json!(["codex"]));
    }

    /// An offline satellite reports ITS last snapshot, marked, and never the
    /// local machine's abilities.
    #[test]
    fn brief_marks_an_offline_satellite_and_keeps_its_own_rows() {
        let mut panel = panel(vec![panel_row("claude", true)]);
        panel.header.host = "sat-lab".to_string();
        panel.header.host_online = false;
        panel.header.stale = true;
        panel.header.note = Some("host `sat-lab` is offline — showing its last report".into());
        let body = Value::Object(brief_value(&panel));
        assert_eq!(body["host"], "sat-lab");
        assert_eq!(body["host_online"], json!(false));
        assert_eq!(body["stale"], json!(true));
        assert_eq!(body["hire"], json!(["claude"]));
    }

    #[test]
    fn vendors_tier_is_honest_about_auth_and_carries_bridge_notes() {
        let mut panel = panel(vec![panel_row("claude", true), panel_row("pi", true)]);
        panel.rows[0].version = Some("claude 1.2.3".into());
        panel.rows[0].budget = BudgetState::Ok;
        panel.rows[1].budget = BudgetState::Unpriced;
        let body = Value::Object(status_value(
            &panel,
            &crate::hub::HubModelsState::Unavailable,
            StatusDetail::Vendors,
        ));
        let rows = body["vendors"].as_array().unwrap();
        assert_eq!(rows[0]["vendor"], "claude");
        assert_eq!(rows[0]["installed"], json!(true));
        assert_eq!(rows[0]["version"], "claude 1.2.3");
        assert_eq!(rows[0]["auth"], "unknown");
        assert_eq!(rows[0]["budget"], "ok");
        assert_eq!(rows[1]["budget"], "unpriced", "never a faked $0");
        assert_eq!(body["observed"], "just now");
        let note = body["note"].as_str().unwrap();
        let expected = ccteam_core::host_registry::AgentProbeSpec::by_vendor("pi")
            .and_then(ccteam_core::host_registry::AgentProbeSpec::tool_surface_notice)
            .unwrap();
        assert_eq!(note, expected);
        // Never claims readiness it has not observed.
        assert!(!body.to_string().contains("\"ready\""));
    }

    #[test]
    fn vendors_note_leads_with_the_header_caveat() {
        let mut panel = panel(vec![panel_row("claude", true)]);
        panel.header.note = Some("host `sat-lab` is offline".into());
        assert_eq!(
            vendors_note(&panel).as_deref(),
            Some("host `sat-lab` is offline")
        );
    }

    #[test]
    fn models_tier_keeps_runtime_and_hub_separate() {
        let mut panel = panel(vec![panel_row("codex", true)]);
        panel.runtime = ccteam_core::model_catalog::ModelCatalog(BTreeMap::from([(
            "codex".to_string(),
            ccteam_core::model_catalog::VendorModelCatalog {
                observed_at: "2026-07-19T10:30:00Z".to_string(),
                source: "codex model/list".to_string(),
                models: ["a", "b", "c", "d", "e", "f", "g", "h", "i"]
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
        let body = Value::Object(status_value(&panel, &hub, StatusDetail::Models));
        // Runtime: ids verbatim (a truncated model id is unusable), count capped.
        let ids = body["models"]["codex"]["ids"].as_array().unwrap();
        assert_eq!(ids.len(), CATALOG_IDS_PER_VENDOR);
        assert_eq!(ids[0], "a");
        assert_eq!(body["models"]["codex"]["seen"], "2026-07-19T10:30Z");
        // Hub: its own labelled block, never merged into `models`.
        assert_eq!(body["hub"]["revision"], "abcdef0");
        assert_eq!(body["hub"]["stale"], json!(true));
        assert_eq!(body["hub"]["claude"]["default"], "sonnet");
        assert_eq!(body["hub"]["claude"]["aliases"]["deep"], "opus");
        assert!(body["models"].get("claude").is_none());
    }

    #[test]
    fn models_tier_omits_hub_when_unavailable() {
        let panel = panel(vec![panel_row("claude", true)]);
        let body = Value::Object(status_value(
            &panel,
            &crate::hub::HubModelsState::Unavailable,
            StatusDetail::Models,
        ));
        assert!(body.get("hub").is_none());
    }

    #[test]
    fn routing_tier_reports_the_paths_it_looked_at_when_none_exists() {
        let panel = panel(vec![]);
        let body = Value::Object(status_value(
            &panel,
            &crate::hub::HubModelsState::Unavailable,
            StatusDetail::Routing,
        ));
        assert_eq!(
            body["routing"]["missing"],
            json!(["/p/.ccteam/routing.md", "/h/.ccteam/routing.md"])
        );
    }

    #[test]
    fn routing_tier_passes_the_note_through_verbatim_with_provenance() {
        let mut panel = panel(vec![]);
        panel.routing = Some(RoutingFile {
            path: "/home/u/.ccteam/routing.md".to_string(),
            bytes: b"# Routing\nUI -> fable\nrefactor -> opus\n".to_vec(),
            updated_at: "2026-07-21T00:00:00+00:00".to_string(),
        });
        let routing = routing_value(&panel);
        assert_eq!(routing["source"], "/home/u/.ccteam/routing.md");
        assert!(routing["sha256"].as_str().unwrap().len() == 64);
        assert_eq!(routing["updated_at"], "2026-07-21T00:00:00+00:00");
        assert_eq!(routing["truncated"], json!(false));
        assert_eq!(
            routing["text"],
            "# Routing\nUI -> fable\nrefactor -> opus\n"
        );
    }

    #[test]
    fn routing_tier_truncates_a_long_note_with_a_pointer() {
        let mut panel = panel(vec![]);
        panel.routing = Some(RoutingFile {
            path: "/home/u/.ccteam/routing.md".to_string(),
            bytes: "x".repeat(ROUTING_NOTES_MAX_CHARS * 3).into_bytes(),
            updated_at: String::new(),
        });
        let routing = routing_value(&panel);
        assert_eq!(routing["truncated"], json!(true));
        let text = routing["text"].as_str().unwrap();
        assert!(text.contains("chars omitted — full note at /home/u/.ccteam/routing.md"));
        assert_eq!(text.chars().count(), ROUTING_NOTES_MAX_CHARS);
    }

    #[test]
    fn full_tier_carries_every_section() {
        let panel = panel(vec![panel_row("claude", true)]);
        let body = Value::Object(status_value(
            &panel,
            &crate::hub::HubModelsState::Unavailable,
            StatusDetail::Full,
        ));
        for key in ["project", "host", "hire", "models", "vendors", "routing"] {
            assert!(body.get(key).is_some(), "full body must carry `{key}`");
        }
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

        // The candidate list a `missing` answer reports, most specific first.
        assert_eq!(
            routing_candidates(&paths, Some("demo")),
            vec![project.display().to_string(), global.display().to_string()]
        );
    }

    #[test]
    fn spawn_unavailable_lists_installed_set_and_stays_advisory() {
        let msg = spawn_unavailable_message(
            "grok",
            "local",
            &["claude".to_string(), "codex".to_string()],
            "just now",
        );
        assert!(msg.starts_with("agent: "), "{msg}");
        assert!(msg.contains("vendor `grok` is not installed on host `local`"));
        assert!(msg.contains("installed there: claude, codex"));
        assert!(msg.contains("observed just now"));
        assert!(msg.contains("advisory"));
        // Never a local fallback; the admin one-click install is the pointer.
        assert!(msg.contains("one-click install"));
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
        assert_eq!(
            resolve_status_project(McpCaller::Admin, Some("  "), None, None).unwrap(),
            None,
        );
    }
}
