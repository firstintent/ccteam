//! v0.9 T4 — Streamable HTTP MCP endpoint (`POST /mcp`).
//!
//! Stateless JSON mode: one JSON-RPC 2.0 message in → one JSON-RPC response
//! out via [`ccteam_im::mcp::McpDispatch`]. No SSE push, no `Mcp-Session-Id`.
//!
//! **Auth — self-gated, bearer-only.** This router mounts OUTSIDE the web
//! `auth_layer` (see `lib::router_with_state`): that layer only understands
//! the web token family (`ccteam:<hex>` + cookies) and would 401 a session
//! bearer before this handler ran — which silently downgraded every managed
//! session's A2A call to an admin fallback and dropped the delegation parent
//! (fixed v0.9.2). [`require_mcp_auth`] is the single gate; it accepts exactly
//! three principals from two bearer families:
//!
//! - admin web token `ccteam:<hex>` → [`McpAuth::Admin`] (owner front door;
//!   `session_spawn` is a root spawn by design)
//! - tenant web token `ccteam:<hex>` → [`McpAuth::User`] (project-scoped root
//!   caller; never promoted to admin or treated as a managed session)
//! - session principal `ccteam-sid:<sid>:<secret>` → Ambient with the FULL
//!   caller identity injected (the delegation-parent edge)
//!
//! A bearer is ALWAYS required — even when `AuthState.enabled == false`
//! (loopback / `--no-auth`): DNS-rebinding / local-script hardening; curated
//! per-session configs and external clients always hold a token. Cookies
//! never authenticate `/mcp`.

use axum::{
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use serde_json::{json, Value};

use crate::auth::{resolve_identity, TOKEN_PREFIX};
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
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let auth = match require_mcp_auth(&app, &headers).await {
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

    // Web-family bearer → Admin or project-scoped User. Session bearer
    // `ccteam-sid:<sid>:<secret>` → Ambient path with the FULL caller identity.
    // The managed-session fields are injected (_caller_sid/_caller_secret/_caller_role/
    // _caller_slug) so session_* principal auth matches the live session
    // (v0.9.0 W1 G4 — previously only role+secret were injected, so session_*
    // over HTTP failed closed with "no project scope").
    let (caller, req) = match auth {
        McpAuth::Admin => (ccteam_im::mcp::McpCaller::Admin, req),
        McpAuth::User { user_id } => (ccteam_im::mcp::McpCaller::User { user_id }, req),
        McpAuth::Session {
            sid,
            role,
            secret,
            slug,
        } => {
            inject_session_caller(&mut req, &sid, &role, &secret, &slug);
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
    User {
        user_id: String,
    },
    Session {
        sid: String,
        role: String,
        secret: String,
        slug: String,
    },
}

/// Enforce bearer always (this route mounts outside `auth_layer`, so this is
/// the ONLY gate). Accepts:
/// - admin/tenant web token `ccteam:<hex>` (resolved by the shared web family)
/// - session-scoped `ccteam-sid:<sid>:<secret>` (curated per-session MCP → Ambient)
async fn require_mcp_auth(app: &AppState, headers: &HeaderMap) -> Result<McpAuth, Response> {
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

    // Web token family — the LIVE admin token when the web gate is enabled
    // (single source with REST, including rotation) plus the tenant registry;
    // load the admin token from disk on loopback / --no-auth where AuthState
    // holds none.
    let expected = match app.auth.current_token() {
        Some(hex) => hex,
        None => match generate_or_load_token(&app.paths.web_token_path()) {
            Ok(hex) => hex,
            Err(err) => {
                tracing::error!(error = %err, "POST /mcp: failed to load web token");
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": "auth misconfigured: cannot load web token"})),
                )
                    .into_response());
            }
        },
    };
    let Some(bare) = raw.and_then(|presented| presented.strip_prefix(TOKEN_PREFIX)) else {
        return Err(unauthorized());
    };
    match resolve_identity(bare, &expected, &app.paths.users_dir()) {
        Some(identity) if identity.is_admin => Ok(McpAuth::Admin),
        Some(identity) => Ok(McpAuth::User {
            user_id: identity.id,
        }),
        None => Err(unauthorized()),
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
    // v0.9.0 W1 (F1/G4) — resolve the `(sid, secret)` PRINCIPAL to a CallerCtx
    // (server-side sid + slug + role). role/slug come from the matched session,
    // never the client; an empty secret / unknown sid returns None → 401.
    match guard.verify_session_principal(sid, secret) {
        Some(ctx) => Ok(McpAuth::Session {
            sid: ctx.sid,
            role: ctx.role,
            secret: secret.to_string(),
            slug: ctx.slug,
        }),
        None => Err(()),
    }
}

/// Inject the FULL caller identity (`_caller_sid` / `_caller_secret` /
/// `_caller_role` / `_caller_slug`) into a tools/call arguments object so the
/// Ambient session_* PRINCIPAL gate sees the curated session's identity. All
/// four are OVERWRITTEN (never trust a caller-supplied value); the daemon
/// re-verifies `(sid, secret)` and re-derives slug/role from CallerCtx.
fn inject_session_caller(req: &mut Value, sid: &str, role: &str, secret: &str, slug: &str) {
    let Some(params) = req.get_mut("params") else {
        return;
    };
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
        map.insert("_caller_sid".into(), json!(sid));
        map.insert("_caller_secret".into(), json!(secret));
        map.insert("_caller_role".into(), json!(role));
        map.insert("_caller_slug".into(), json!(slug));
        // The local-socket admin fallback arg must never ride in over HTTP —
        // this transport authenticates via bearer only (admin or sid).
        map.remove("_caller_admin_token");
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
