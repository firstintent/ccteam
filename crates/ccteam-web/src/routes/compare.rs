//! `POST /api/v1/projects/{slug}/compare` — multi-vendor fan-out (v0.8.24 C2),
//! plus `GET .../compare/history` — past compare groups aggregated from
//! session meta (`compare_group`) and each member's first user turn.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::auth::Identity;
use crate::state::AppState;

use super::sessions_api::{no_gateway, project_not_visible};

/// Request body for compare.
#[derive(Debug, Deserialize, ToSchema)]
pub struct CompareRequest {
    /// The question / prompt to fan out (also accepts `question` alias).
    #[serde(alias = "question")]
    pub prompt: String,
    /// Optional vendor list (`claude`/`codex`/`grok`/`opencode`). Empty = all.
    #[serde(default)]
    pub vendors: Option<Vec<String>>,
    /// Optional timeout in seconds (default 300, max 600).
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

/// `POST /api/v1/projects/{slug}/compare`
#[utoipa::path(
    post,
    path = "/api/v1/projects/{slug}/compare",
    tag = "sessions",
    params(("slug" = String, Path, description = "Project slug")),
    request_body(content = CompareRequest, description = "Prompt + optional vendors/timeout"),
    responses(
        (status = 200, description = "Aggregated compare result", body = serde_json::Value),
        (status = 400, description = "Bad prompt / unknown vendor"),
        (status = 403, description = "Project not visible"),
        (status = 503, description = "No live gateway"),
    ),
)]
pub(crate) async fn handle_compare(
    State(app): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(slug): Path<String>,
    Json(body): Json<CompareRequest>,
) -> Response {
    let Some(gw) = app.gateway.as_ref() else {
        return no_gateway();
    };
    if !crate::routes::api_v1::can_see_project(&app, &identity, &slug) {
        return project_not_visible(&slug);
    }
    let prompt = body.prompt.trim().to_string();
    if prompt.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "prompt must not be empty"})),
        )
            .into_response();
    }
    let vendors =
        match ccteam_im::compare::parse_compare_vendors(body.vendors.as_deref().unwrap_or(&[])) {
            Ok(v) => v,
            Err(msg) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({"error": msg})),
                )
                    .into_response();
            }
        };
    let timeout_secs = body.timeout_secs.unwrap_or(300).clamp(5, 600);
    let timeout = std::time::Duration::from_secs(timeout_secs);

    let result = {
        let mut guard = gw.lock().await;
        guard
            .run_compare_for_web(identity.web_chat_id(), slug, prompt, vendors, timeout)
            .await
    };

    match result {
        Ok(r) => (
            StatusCode::OK,
            Json(serde_json::to_value(r).unwrap_or_default()),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

// ── compare history (v0.8.24 gap-fill) ───────────────────────────────────────

/// One member session of a past compare group.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CompareHistoryMember {
    /// Gateway session id — still `/use`-able to continue that answer.
    pub sid: String,
    /// Vendor token (`claude` / `codex` / `grok` / `opencode`).
    pub vendor: String,
    /// Accrued session cost USD (never a faked 0).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    /// Session title when set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// One past compare fan-out, aggregated from session meta.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CompareHistoryGroup {
    /// The shared `compare_group` id.
    pub group: String,
    /// Earliest member `created_at` (RFC3339) — when the compare ran.
    pub created_at: String,
    /// Question summary: the first non-empty user turn of any member
    /// (truncated ~200 chars); empty when no turn was mirrored.
    pub prompt: String,
    pub members: Vec<CompareHistoryMember>,
    /// Sum of known member costs (None when nothing priced).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_subtotal_usd: Option<f64>,
}

/// `GET .../compare/history` response.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CompareHistoryResponse {
    pub groups: Vec<CompareHistoryGroup>,
}

/// Truncate a prompt summary to ~`max` chars (char-safe).
fn truncate_prompt(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    let head: String = chars[..max].iter().collect();
    format!("{head}…")
}

/// Build the compare history from disk state (blocking: meta + turns reads).
/// Pure over `project_dir` so it is unit-testable without a server.
pub(crate) fn build_compare_history(project_dir: &std::path::Path) -> CompareHistoryResponse {
    use std::collections::BTreeMap;
    let metas = ccteam_harness::list_session_metas(project_dir);
    let mut by_group: BTreeMap<String, Vec<&ccteam_harness::SessionMeta>> = BTreeMap::new();
    for m in &metas {
        if let Some(g) = m.compare_group.as_deref().filter(|g| !g.is_empty()) {
            by_group.entry(g.to_string()).or_default().push(m);
        }
    }
    let mut groups: Vec<CompareHistoryGroup> = by_group
        .into_iter()
        .map(|(group, mut members)| {
            members.sort_by(|a, b| a.sid.cmp(&b.sid));
            let created_at = members
                .iter()
                .map(|m| m.created_at.clone())
                .min()
                .unwrap_or_default();
            // Question summary = first non-empty user turn of any member.
            let mut prompt = String::new();
            for m in &members {
                if let Ok(turns) =
                    ccteam_harness::execution::turns_mirror::read_all_turns(project_dir, &m.sid)
                {
                    if let Some(t) = turns.iter().find(|t| !t.user.trim().is_empty()) {
                        prompt = truncate_prompt(t.user.trim(), 200);
                        break;
                    }
                }
            }
            let rows: Vec<CompareHistoryMember> = members
                .iter()
                .map(|m| CompareHistoryMember {
                    sid: m.sid.clone(),
                    vendor: ccteam_im::compare::vendor_label(m.vendor).to_string(),
                    cost_usd: m.cost_usd,
                    title: m.title.clone(),
                })
                .collect();
            let mut subtotal = 0.0;
            let mut any = false;
            for r in &rows {
                if let Some(c) = r.cost_usd {
                    subtotal += c;
                    any = true;
                }
            }
            CompareHistoryGroup {
                group,
                created_at,
                prompt,
                members: rows,
                cost_subtotal_usd: any.then_some(subtotal),
            }
        })
        .collect();
    groups.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    CompareHistoryResponse { groups }
}

/// `GET /api/v1/projects/{slug}/compare/history` — past compare groups,
/// aggregated from session meta (`compare_group`). Members carry real sids
/// (each `/use`-able); the per-sid answers replay through the existing
/// session-history pipeline (`GET /api/v1/sessions/{sid}`).
#[utoipa::path(
    get,
    path = "/api/v1/projects/{slug}/compare/history",
    tag = "sessions",
    params(("slug" = String, Path, description = "Project slug")),
    responses(
        (status = 200, description = "Past compare groups (newest first)", body = CompareHistoryResponse),
        (status = 404, description = "Project not visible / unknown"),
    ),
)]
pub(crate) async fn handle_compare_history(
    State(app): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(slug): Path<String>,
) -> Response {
    if !crate::routes::api_v1::can_see_project(&app, &identity, &slug) {
        return project_not_visible(&slug);
    }
    let project_dir = app.paths.project_dir(&slug);
    match tokio::task::spawn_blocking(move || build_compare_history(&project_dir)).await {
        Ok(resp) => Json(resp).into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("worker: {err}")})),
        )
            .into_response(),
    }
}
