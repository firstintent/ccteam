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

#[derive(Serialize)]
pub struct DashboardRow {
    pub slug: String,
    pub team: String,
    pub kind: String,
    pub last_event_label: String,
    pub badge_class: &'static str,
    pub badge_label: &'static str,
    pub cost_label: String,
}

#[derive(Serialize)]
pub struct HarnessSnapshotView {
    pub model: String,
    pub context_used_pct: String,
    pub cost_usd_total: String,
    pub rate_limit_pct: String,
    pub captured_at: String,
}

#[derive(Serialize)]
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

#[derive(Serialize)]
pub struct OutboxRow {
    pub filename: String,
    pub kind: String,
    pub created_at: String,
    pub preview: String,
}
