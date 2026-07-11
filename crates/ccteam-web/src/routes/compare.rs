//! `POST /api/v1/projects/{slug}/compare` — multi-vendor fan-out (v0.8.24 C2).

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use serde::Deserialize;
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
