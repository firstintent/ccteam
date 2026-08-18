//! V0.3.2 F59 — JSON DTO collection (was askama template structs).
//!
//! V0.3 M5.1 originally defined three `#[derive(Template)]` structs
//! here (`DashboardTemplate`, `ProjectTemplate`, `SessionTemplate`)
//! plus a generic `HtmlTemplate<T>` axum wrapper. V0.3.2 F59 retired
//! the htmx UI; the three top-level structs and the `HtmlTemplate`
//! wrapper were dropped along with `templates/{dashboard,project,
//! session}.html`. `templates/base.html` is kept as a minimal askama
//! SSR fallback per the PRD; askama stays in `Cargo.toml` for the
//! same reason (no live `#[derive(Template)]` references it today).
//!
//! What lives on: the **data** structs (`DashboardRow`, `EventRow`,
//! `OutboxRow`, `HarnessSnapshotView`) which the JSON API
//! (`routes::api_v1`) doc-comments still reference. These derive
//! `Serialize` so they encode straight into the F52 JSON contract
//! without churn.
//!
//! V0.4.0 F67: the legacy `current_phase` field on [`DashboardRow`]
//! is dropped — phase machinery was retired in F60 and the field
//! decayed into the empty string. The SPA's dashboard column moved
//! to the F67 `workflow_summary` shape (rendered downstream of
//! `WorkflowSummary` in `api_v1::ProjectSummary` / consumed by F68).

use serde::Serialize;
use utoipa::ToSchema;

#[derive(Serialize, ToSchema)]
pub struct DashboardRow {
    /// Monotonic revision of the projection snapshot backing this response.
    /// Repeated on every row to keep the long-standing top-level array wire
    /// shape additive; the HTTP ETag carries it even when the array is empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u64>,
    pub slug: String,
    /// The project's real working-tree path. The SPA shows it to disambiguate
    /// an auto-appended slug (demo2 vs demo): the dir is unambiguous.
    pub path: String,
    /// Project-bound execution host (`local` or a satellite id).
    pub host: String,
    /// Local is always online; satellites use the host-registry TTL.
    pub host_online: bool,
    pub team: String,
    pub kind: String,
    pub last_event_label: String,
    pub badge_class: &'static str,
    pub badge_label: &'static str,
    pub cost_label: String,
    /// True for an ORPHANED registration: the slug is in `config.yaml` but its
    /// `.ccteam/state.json` is gone (the dir was removed out-of-band, or an
    /// init half-failed). `collect_projects` skips these, so `GET /projects`
    /// surfaces them ONLY to the admin (owner is unknowable → tenants fail
    /// closed) so they can be deregistered/cleaned from the web. Healthy rows
    /// are `false`.
    #[serde(default)]
    pub broken: bool,
    /// v0.8.24 Q7 — current git branch of the working tree (read-only,
    /// best-effort from `.git/HEAD`); `None` when not a git repo → the SPA
    /// hides the branch dimension.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_branch: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct HarnessSnapshotView {
    pub model: String,
    pub context_used_pct: String,
    pub cost_usd_total: String,
    pub rate_limit_pct: String,
    pub captured_at: String,
}

#[derive(Serialize, ToSchema)]
pub struct EventRow {
    pub ts: String,
    pub event: String,
    pub detail: String,
    /// Hook payload (`PreToolUse` / `PostToolUse`) — preserved so the
    /// SPA's events panel can render tool-use rows the same way for
    /// REST-seeded and SSE-pushed events.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cmd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct OutboxRow {
    pub filename: String,
    pub kind: String,
    pub created_at: String,
    pub preview: String,
}
