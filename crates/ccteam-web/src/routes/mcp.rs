//! v0.9 T4 — Streamable HTTP MCP endpoint (`POST /mcp`).
//!
//! Stateless JSON mode: one JSON-RPC 2.0 message in → one JSON-RPC response
//! out via [`ccteam_im::mcp::McpDispatch`]. No SSE push, no `Mcp-Session-Id`.
//!
//! **Auth.** Bearer is ALWAYS required — even when `AuthState.enabled == false`
//! (loopback / `--no-auth`). Rationale: DNS-rebinding / local-script hardening;
//! curated per-session configs and external clients always hold a token.
//!
//! // Future (comment-only reserve — do NOT implement yet): a session-scoped
//! // bearer of the form `ccteam-sid:<sid>:<secret>` may be accepted once the
//! // owner decides. Match seam lives next to the admin-token check in
//! // [`require_mcp_bearer`].

use axum::{
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Extension, Json, Router,
};
use serde_json::{json, Value};

use crate::auth::{validate_bearer, Identity, PresentedToken};
use crate::state::AppState;
use crate::token::generate_or_load_token;

/// Mount `POST|GET|DELETE /mcp`.
pub fn router() -> Router<AppState> {
    Router::new().route(
        "/mcp",
        post(handle_post)
            .get(method_not_allowed)
            .delete(method_not_allowed),
    )
}

/// Stateless server: no SSE stream, no session id — reject non-POST.
async fn method_not_allowed() -> Response {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        Json(json!({
            "error": "method not allowed: MCP HTTP is POST-only (stateless JSON mode; no SSE / Mcp-Session-Id)"
        })),
    )
        .into_response()
}

/// `POST /mcp` — body = one JSON-RPC 2.0 message.
async fn handle_post(
    State(app): State<AppState>,
    headers: HeaderMap,
    // Injected by auth_layer when auth is enabled (absent on the loopback
    // no-auth path, which inserts Identity::admin but no PresentedToken).
    identity: Option<Extension<Identity>>,
    presented: Option<Extension<PresentedToken>>,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    if let Some(resp) = require_mcp_bearer(&app, &headers, identity, presented) {
        return resp;
    }

    let req = match body {
        Ok(Json(v)) => v,
        Err(err) => {
            // JSON-RPC-over-HTTP: parse errors are JSON-RPC -32700 with HTTP 200
            // (same convention as the mcp.sock line handler).
            return (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/json")],
                json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": {
                        "code": -32700,
                        "message": format!("parse error: {err}"),
                    },
                })
                .to_string(),
            )
                .into_response();
        }
    };

    let dispatch = app.mcp_dispatch();
    match dispatch.dispatch(req).await {
        Some(response) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            response.to_string(),
        )
            .into_response(),
        // Notifications (e.g. notifications/initialized) → 202 empty.
        None => StatusCode::ACCEPTED.into_response(),
    }
}

/// Enforce bearer always. Returns `Some(401)` when the request must be rejected.
///
/// - **Auth enabled**: `auth_layer` already 401'd missing/bad tokens. Additionally
///   require the [`PresentedToken`] extension (token was actually presented) and
///   an admin [`Identity`] (MCP is admin-gated, not tenant-shared).
/// - **Auth disabled**: validate `Authorization: Bearer ccteam:<hex>` yourself
///   against [`generate_or_load_token`](`paths.web_token_path()`) with the
///   constant-time [`validate_bearer`].
///
/// // Future seam (NOT implemented): accept `ccteam-sid:<sid>:<secret>` as a
/// // session-scoped bearer once the owner decides — branch beside the admin
/// // checks below.
fn require_mcp_bearer(
    app: &AppState,
    headers: &HeaderMap,
    identity: Option<Extension<Identity>>,
    presented: Option<Extension<PresentedToken>>,
) -> Option<Response> {
    let unauthorized = || {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "auth required: Authorization: Bearer ccteam:<hex>"})),
        )
            .into_response()
    };

    if app.auth.enabled {
        // auth_layer already rejected invalid credentials. Require that a token
        // was presented and resolved to admin (tenant tokens are 401 for /mcp).
        //
        // Future: match `ccteam-sid:<sid>:<secret>` here for session-scoped MCP.
        let is_admin = identity
            .as_ref()
            .map(|Extension(id)| id.is_admin)
            .unwrap_or(false);
        if presented.is_none() || !is_admin {
            return Some(unauthorized());
        }
        // Cookie-presented admin tokens also set PresentedToken — accepted per
        // "query-param/cookie flows are harmless if present". MCP clients send
        // Authorization: Bearer.
        None
    } else {
        // Auth off (loopback / --no-auth): still demand the on-disk web token.
        //
        // Future: match `ccteam-sid:<sid>:<secret>` here for session-scoped MCP.
        let expected = match generate_or_load_token(&app.paths.web_token_path()) {
            Ok(hex) => hex,
            Err(err) => {
                tracing::error!(error = %err, "POST /mcp: failed to load web token");
                return Some(
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({"error": "auth misconfigured: cannot load web token"})),
                    )
                        .into_response(),
                );
            }
        };
        let presented = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_bearer_value);
        match presented {
            Some(p) if validate_bearer(p, &expected) => None,
            _ => Some(unauthorized()),
        }
    }
}

/// Chomp `Bearer ` → the wire token (`ccteam:<hex>`).
fn parse_bearer_value(value: &str) -> Option<&str> {
    let rest = value.strip_prefix("Bearer ")?;
    let trimmed = rest.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}
