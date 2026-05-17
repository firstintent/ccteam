//! V0.5.0 F96 — JSON API for Agent Teams.
//!
//! Read-only mirror of Anthropic's `~/.claude/teams/<>/` SoT. Every
//! handler in this file parses files under `state.claude_home` and
//! returns JSON. No file under the Anthropic root is ever written —
//! `state.claude_home` is `Arc<PathBuf>` to make accidental
//! mutation harder.
//!
//! Endpoints (mount at `/api/v1/teams[/...]`):
//!
//! - `GET /api/v1/teams` — array of every team dir under
//!   `<claude_home>/teams/` (`TeamListEntry` shape).
//! - `GET /api/v1/teams/{name}` — single team detail with the parsed
//!   `config`, `TaskCounts`, and the last few non-idle messages.
//! - `GET /api/v1/teams/{name}/tasks` — Kanban data.
//! - `GET /api/v1/teams/{name}/inbox?teammate=<n>&since=<ts>` —
//!   message stream; `teammate` omitted ⇒ merged across all inboxes.
//! - `GET /api/v1/teams/{name}/member/{teammate}/definition` —
//!   404 for ad-hoc, 200 with parsed frontmatter+body for
//!   definition-backed members. `definition_missing: true` when the
//!   member is definition-backed but the `.md` file isn't anywhere
//!   on disk (PRD §F96 acceptance #4 — surface the warning).

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};

use crate::state::AppState;
use crate::teams::discovery::{discover_teams, load_team_config, MemberView};
use crate::teams::inbox::{filter_since, load_all_inboxes, load_inbox, recent_preview};
use crate::teams::subagent_resolver::{resolve_definition, AgentDefinition};
use crate::teams::tasks::{load_tasks, TaskCounts};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/teams", get(handle_list))
        .route("/api/v1/teams/{name}", get(handle_detail))
        .route("/api/v1/teams/{name}/tasks", get(handle_tasks))
        .route("/api/v1/teams/{name}/inbox", get(handle_inbox))
        .route(
            "/api/v1/teams/{name}/member/{teammate}/definition",
            get(handle_definition),
        )
}

#[derive(Debug, Deserialize)]
pub struct InboxQuery {
    #[serde(default)]
    pub teammate: Option<String>,
    #[serde(default)]
    pub since: Option<String>,
}

/// Composite payload returned by `GET /api/v1/teams/{name}`.
#[derive(Debug, Serialize)]
pub struct TeamDetailResponse {
    pub config: crate::teams::discovery::TeamConfig,
    pub task_count: TaskCounts,
    /// Last ~5 non-idle messages across every teammate (merged).
    pub recent_messages: Vec<crate::teams::inbox::InboxMessage>,
}

/// Wire payload returned by the definition endpoint. `Some(def)` for
/// resolved file; `definition_missing=true` for definition-backed but
/// not-on-disk (warning banner trigger).
#[derive(Debug, Serialize)]
pub struct DefinitionResponse {
    pub agent_type: String,
    pub teammate: String,
    pub definition: Option<AgentDefinition>,
    pub definition_missing: bool,
}

async fn handle_list(State(app): State<AppState>) -> impl IntoResponse {
    match discover_teams(&app.claude_home) {
        Ok(entries) => Json(entries).into_response(),
        Err(err) => {
            tracing::error!(?err, "GET /api/v1/teams failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("{err}")})),
            )
                .into_response()
        }
    }
}

async fn handle_detail(State(app): State<AppState>, Path(name): Path<String>) -> impl IntoResponse {
    let cfg = match load_team_config(&app.claude_home, &name) {
        Ok(c) => c,
        Err(err) => {
            return team_not_found_or_500(&name, err);
        }
    };
    let tasks = load_tasks(&app.claude_home, &name).unwrap_or_default();
    let task_count = TaskCounts::from(&tasks);
    let merged = load_all_inboxes(&app.claude_home, &name).unwrap_or_default();
    let recent = recent_preview(&merged, 5);
    Json(TeamDetailResponse {
        config: cfg,
        task_count,
        recent_messages: recent,
    })
    .into_response()
}

async fn handle_tasks(State(app): State<AppState>, Path(name): Path<String>) -> impl IntoResponse {
    // 404 only when neither config nor tasks dir exists — a team with
    // zero tasks is valid.
    let team_dir = app.claude_home.join("teams").join(&name);
    if !team_dir.exists() {
        return team_404(&name);
    }
    match load_tasks(&app.claude_home, &name) {
        Ok(tasks) => Json(tasks).into_response(),
        Err(err) => {
            tracing::error!(team = %name, ?err, "load_tasks failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("{err}")})),
            )
                .into_response()
        }
    }
}

async fn handle_inbox(
    State(app): State<AppState>,
    Path(name): Path<String>,
    Query(q): Query<InboxQuery>,
) -> impl IntoResponse {
    let team_dir = app.claude_home.join("teams").join(&name);
    if !team_dir.exists() {
        return team_404(&name);
    }
    let msgs_result = match &q.teammate {
        Some(t) if !t.is_empty() => load_inbox(&app.claude_home, &name, t),
        _ => load_all_inboxes(&app.claude_home, &name),
    };
    match msgs_result {
        Ok(msgs) => Json(filter_since(msgs, q.since.as_deref())).into_response(),
        Err(err) => {
            tracing::error!(team = %name, ?err, "load_inbox failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("{err}")})),
            )
                .into_response()
        }
    }
}

async fn handle_definition(
    State(app): State<AppState>,
    Path((name, teammate)): Path<(String, String)>,
) -> impl IntoResponse {
    let cfg = match load_team_config(&app.claude_home, &name) {
        Ok(c) => c,
        Err(err) => return team_not_found_or_500(&name, err),
    };
    let Some(member) = cfg.members.iter().find(|m| m.name == teammate) else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": format!("teammate not found: {teammate} in {name}")
            })),
        )
            .into_response();
    };
    if !member.definition_backed {
        // PRD §F96 — ad-hoc returns 404 with a guidance hint, NOT the
        // inline prompt (callers can fetch `members[i].prompt` via the
        // detail endpoint).
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": "ad-hoc teammate — no `.md` definition file; prompt is inline in /api/v1/teams/<name> members[i].prompt",
                "ad_hoc": true,
            })),
        )
            .into_response();
    }
    let agent_type = member.agent_type.clone().unwrap_or_default();
    let cwd = member.cwd.as_deref().map(std::path::Path::new);
    let resolved = resolve_definition(&app.claude_home, cwd, &agent_type);
    let missing = resolved.is_none();
    Json(DefinitionResponse {
        agent_type,
        teammate,
        definition: resolved,
        definition_missing: missing,
    })
    .into_response()
}

fn team_not_found_or_500(name: &str, err: anyhow::Error) -> axum::response::Response {
    // The most common case is "team dir missing entirely" — surface
    // 404. Anything else is server-side (parse / IO).
    let chain = format!("{err:#}");
    if chain.contains("No such file") || chain.contains("os error 2") {
        return team_404(name);
    }
    tracing::error!(team = %name, ?err, "team load failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({"error": chain})),
    )
        .into_response()
}

fn team_404(name: &str) -> axum::response::Response {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({"error": format!("team not found: {name}")})),
    )
        .into_response()
}

// Silence "unused" lint while MemberView is re-exported for tests and
// the SPA-side type generation — the field set is wire-stable.
#[allow(dead_code)]
fn _wire_anchor(_a: MemberView) {}
