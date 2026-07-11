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
    let auth = match require_mcp_auth(&app, &headers, identity, presented).await {
        Ok(a) => a,
        Err(resp) => return resp,
    };

    let mut req = match body {
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

    // Admin bearer → owner's front door (session tools skip cto gate).
    // Session bearer `ccteam-sid:<sid>:<secret>` → Ambient path with injected
    // _caller_role/_caller_secret so session_* auth matches the live session.
    let (caller, req) = match auth {
        McpAuth::Admin => (ccteam_im::mcp::McpCaller::Admin, req),
        McpAuth::Session { role, secret } => {
            inject_session_caller(&mut req, &role, &secret);
            (ccteam_im::mcp::McpCaller::Ambient, req)
        }
    };
    let dispatch = app.mcp_dispatch();
    match dispatch.dispatch_as(req, caller).await {
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

/// Who authenticated against `POST /mcp`.
enum McpAuth {
    Admin,
    Session { role: String, secret: String },
}

/// Enforce bearer always. Accepts:
/// - admin web token `ccteam:<hex>` (owner front door → [`McpCaller::Admin`])
/// - session-scoped `ccteam-sid:<sid>:<secret>` (curated per-session MCP → Ambient)
async fn require_mcp_auth(
    app: &AppState,
    headers: &HeaderMap,
    identity: Option<Extension<Identity>>,
    presented: Option<Extension<PresentedToken>>,
) -> Result<McpAuth, Response> {
    let unauthorized = || {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "error": "auth required: Authorization: Bearer ccteam:<hex> | ccteam-sid:<sid>:<secret>"
            })),
        )
            .into_response()
    };

    let raw = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_bearer_value);

    // Session-scoped bearer first (works regardless of AuthState.enabled).
    if let Some(tok) = raw {
        if let Some((sid, secret)) = parse_session_bearer(tok) {
            return verify_session_bearer(app, &sid, &secret)
                .await
                .map_err(|_| unauthorized());
        }
    }

    if app.auth.enabled {
        let is_admin = identity
            .as_ref()
            .map(|Extension(id)| id.is_admin)
            .unwrap_or(false);
        if presented.is_none() || !is_admin {
            return Err(unauthorized());
        }
        Ok(McpAuth::Admin)
    } else {
        let expected = match generate_or_load_token(&app.paths.web_token_path()) {
            Ok(hex) => hex,
            Err(err) => {
                tracing::error!(error = %err, "POST /mcp: failed to load web token");
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": "auth misconfigured: cannot load web token"})),
                )
                    .into_response());
            }
        };
        match raw {
            Some(p) if validate_bearer(p, &expected) => Ok(McpAuth::Admin),
            _ => Err(unauthorized()),
        }
    }
}

/// Parse `ccteam-sid:<sid>:<secret>` → (sid, secret).
fn parse_session_bearer(tok: &str) -> Option<(String, String)> {
    let rest = tok.strip_prefix("ccteam-sid:")?;
    let (sid, secret) = rest.split_once(':')?;
    if sid.is_empty() || secret.is_empty() {
        return None;
    }
    Some((sid.to_string(), secret.to_string()))
}

async fn verify_session_bearer(app: &AppState, sid: &str, secret: &str) -> Result<McpAuth, ()> {
    let Some(gw) = app.gateway.as_ref() else {
        return Err(());
    };
    let guard = gw.lock().await;
    // Reuse gateway secret map: any live session with matching secret.
    // Role is taken from the matched session (not client-supplied).
    if let Some((role, ok)) = guard.session_role_if_secret_matches(sid, secret) {
        if ok {
            return Ok(McpAuth::Session {
                role,
                secret: secret.to_string(),
            });
        }
    }
    Err(())
}

/// Inject `_caller_role` / `_caller_secret` into a tools/call arguments object
/// so Ambient session_* auth sees the curated session's identity.
fn inject_session_caller(req: &mut Value, role: &str, secret: &str) {
    let Some(params) = req.get_mut("params") else {
        return;
    };
    let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
    if !name.contains("session_") && !name.ends_with("session_list") {
        // Still inject for all tools/call — harmless for non-session tools.
    }
    let args = params.as_object_mut().and_then(|m| m.get_mut("arguments"));
    let args = match args {
        Some(a) => a,
        None => {
            if let Some(obj) = params.as_object_mut() {
                obj.insert("arguments".into(), json!({}));
                obj.get_mut("arguments").unwrap()
            } else {
                return;
            }
        }
    };
    if let Some(map) = args.as_object_mut() {
        map.insert("_caller_role".into(), json!(role));
        map.insert("_caller_secret".into(), json!(secret));
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
