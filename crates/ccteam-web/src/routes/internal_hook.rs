//! V0.6.1 F139 — `POST /internal/hook/:kind[/:action]`.
//!
//! Routes Claude Code hook firings through the long-running daemon
//! instead of paying the ~200 ms `ccteam` Rust binary start-up tax per
//! hook (4 hooks × ~1.5 turns/sec ⇒ user-visible 1+ s of chat
//! sluggishness). The wire shape is intentionally narrow: the script in
//! `~/.ccteam/hooks/hook.sh` packages the Claude Code hook stdin as the
//! POST body and forwards the subcommand `<kind>` (`progress-append` /
//! `load-context` / `intercept-ask` / `permission-request` /
//! `chat-progress`) + the optional `<action>` (the event-type arg for
//! `progress-append` / `chat-progress`) through the path. The handler
//! calls `ccteam_hooks::dispatch` — the same library entry the CLI uses —
//! and returns:
//!
//! - `{}` for fire-and-forget hooks (Claude Code treats empty / `{}`
//!   stdout as "allow with no notes"),
//! - the structured decision JSON for `intercept-ask` (the assistant
//!   sees the deny reason inline) and `permission-request` (the HITL
//!   allow/deny decision — this one blocks on the mcp.sock for up to the
//!   daemon's ~600s approval TTL, which is why the dispatch runs on the
//!   blocking pool below).
//!
//! Auth: this router sits under the same `auth_layer` middleware as the
//! rest of the stateful router. Loopback bind defaults to no auth (the
//! script still works without sending a token); non-loopback bind
//! generates `~/.ccteam/web-token` and the script reads that file +
//! sends `Authorization: Bearer ccteam:<hex>`.

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use serde_json::{json, Map, Value};

use crate::state::AppState;

/// F186 — HTTP header carrying the chat-mode bot role. Set by
/// `hook.sh` when `$CCTEAM_CHAT_ROLE` is in the firing claude pane's
/// env (F175 tmux env injection). The handler folds this into the
/// stdin payload's `role` field so `derive_role_from_payload` picks it
/// up via its existing stdin→env priority chain. Necessary because the
/// daemon process does not inherit claude's env (it runs as its own
/// process tree — only the cold-fallback CLI exec path can pick up the
/// env var by inheritance).
pub const HEADER_CHAT_ROLE: &str = "x-ccteam-role";
/// F186 — companion header for `CCTEAM_CHAT_SLUG`. Same mechanism as
/// `HEADER_CHAT_ROLE`; folded into stdin `slug` for any downstream
/// consumer that wants it.
pub const HEADER_CHAT_SLUG: &str = "x-ccteam-slug";
/// v0.8.8 F1 — companion header for `CCTEAM_CHAT_SID` (the ccteam session
/// `s<N>`). Same mechanism: `hook.sh` sets it from the firing pane's env;
/// folded into stdin `ccteam_sid` so the sid-keyed hook readers
/// (`chat_progress::derive_sid_from_payload`, `permission_request`,
/// `intercept_ask`) pick it up on the HTTP fast-path (the daemon process does
/// not inherit the pane env). NOT the Anthropic native session UUID.
pub const HEADER_CHAT_SID: &str = "x-ccteam-sid";

/// Build the `POST /internal/hook/:kind[/:action]` router.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/internal/hook/{kind}", post(handle_no_action))
        .route("/internal/hook/{kind}/{action}", post(handle_with_action))
}

async fn handle_no_action(
    State(app): State<AppState>,
    Path(kind): Path<String>,
    identity: Option<axum::Extension<crate::auth::Identity>>,
    headers: HeaderMap,
    body: Option<Json<Value>>,
) -> Response {
    let identity = identity.map(|axum::Extension(id)| id);
    dispatch(
        &app,
        &kind,
        None,
        identity.as_ref(),
        &headers,
        body.map(|Json(v)| v),
    )
    .await
}

async fn handle_with_action(
    State(app): State<AppState>,
    Path((kind, action)): Path<(String, String)>,
    identity: Option<axum::Extension<crate::auth::Identity>>,
    headers: HeaderMap,
    body: Option<Json<Value>>,
) -> Response {
    let identity = identity.map(|axum::Extension(id)| id);
    dispatch(
        &app,
        &kind,
        Some(&action),
        identity.as_ref(),
        &headers,
        body.map(|Json(v)| v),
    )
    .await
}

/// F186 — inject `X-Ccteam-Role` / `X-Ccteam-Slug` request headers into
/// the stdin payload as `role` / `slug` fields, only when the payload
/// doesn't already carry them. Defensive against future payload-shipped
/// role/slug from Anthropic (don't overwrite an upstream-provided
/// value). Promotes `Value::Null` (empty body) to `Value::Object` so
/// the field insertion is safe — direct `stdin["role"] = ...` indexing
/// on `Value::Null` panics. Non-object payloads (an array / scalar
/// stdin would be ill-formed for a hook anyway) are left alone.
fn inject_headers(mut stdin: Value, headers: &HeaderMap) -> Value {
    let role_hdr = headers
        .get(HEADER_CHAT_ROLE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let slug_hdr = headers
        .get(HEADER_CHAT_SLUG)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    // v0.8.8 F1 — fold the ccteam session sid header into stdin `ccteam_sid`.
    let sid_hdr = headers
        .get(HEADER_CHAT_SID)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    if role_hdr.is_none() && slug_hdr.is_none() && sid_hdr.is_none() {
        return stdin;
    }
    if matches!(stdin, Value::Null) {
        stdin = Value::Object(Map::new());
    }
    if let Value::Object(map) = &mut stdin {
        if let Some(role) = role_hdr {
            if !role.is_empty() && !map.contains_key("role") {
                map.insert("role".into(), Value::String(role));
            }
        }
        if let Some(slug) = slug_hdr {
            if !slug.is_empty() && !map.contains_key("slug") {
                map.insert("slug".into(), Value::String(slug));
            }
        }
        if let Some(sid) = sid_hdr {
            if !sid.is_empty() && !map.contains_key("ccteam_sid") {
                map.insert("ccteam_sid".into(), Value::String(sid));
            }
        }
    }
    stdin
}

/// Shared invocation path. `None` body is normalized to `Value::Null`
/// so handlers that don't actually read stdin (today: `intercept-ask`)
/// still work when the client sent zero bytes.
async fn dispatch(
    app: &AppState,
    kind: &str,
    action: Option<&str>,
    identity: Option<&crate::auth::Identity>,
    headers: &HeaderMap,
    body: Option<Value>,
) -> Response {
    let t0 = std::time::Instant::now();
    let stdin = inject_headers(body.unwrap_or(Value::Null), headers);
    // `flow-run` names its target project IN THE BODY, so the URL-shaped ACL
    // choke point (`auth::project_acl_layer`) cannot see it — gate it here
    // with the same ownership policy and the same non-disclosing 404. Missing
    // `Identity` extension mirrors `project_acl_layer`: a loopback / open-mode
    // daemon runs without auth and that posture IS the single-user admin
    // (checker s523 R1: without this, any authenticated tenant could append
    // envelope rows to another tenant's journal).
    if kind == "flow-run" {
        if let Some(slug) = stdin.get("project").and_then(|v| v.as_str()) {
            let caller = identity
                .cloned()
                .unwrap_or_else(crate::auth::Identity::admin);
            if !crate::routes::api_v1::can_see_project(app, &caller, slug) {
                return (
                    StatusCode::NOT_FOUND,
                    Json(json!({"error": format!("project not found: {slug}")})),
                )
                    .into_response();
            }
        }
        // A body without `project` falls through: the hooks-library validator
        // rejects it with its own "missing `project`" error.
    }
    // Try to pull session id / role from the Claude Code hook stdin
    // payload — useful for joining hook latency rows to other stages.
    // Best-effort: any field missing → empty string. Owned (not borrowed from
    // `stdin`) so `stdin` can move into the blocking task below.
    let session_id = stdin
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let cwd = stdin
        .get("cwd")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    // (v0.8.5 S1) `ccteam_hooks::dispatch` does blocking socket I/O — the
    // `intercept-ask` hook waits up to SOCKET_READ_TIMEOUT_SECS on the daemon
    // mcp.sock for the user's answer. Run it on the blocking pool so it never
    // stalls the (single-threaded) daemon runtime; otherwise the mcp.sock task
    // that writes the answer can't be scheduled and D6 self-deadlocks until
    // timeout. (The cold CLI hook path is a short-lived subprocess that blocks
    // harmlessly.)
    let paths = app.paths.clone();
    let kind_owned = kind.to_string();
    let action_owned = action.map(str::to_string);
    let result = match tokio::task::spawn_blocking(move || {
        ccteam_hooks::dispatch(&paths, &kind_owned, action_owned.as_deref(), &stdin)
    })
    .await
    {
        Ok(r) => r,
        Err(join_err) => {
            tracing::warn!(
                event = "latency",
                stage = "hook.recv.err",
                kind = %kind,
                action = action.unwrap_or(""),
                elapsed_ms = t0.elapsed().as_millis() as u64,
                error = %join_err,
                "latency hook.recv (dispatch task panicked)"
            );
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(
                    json!({"ok": false, "error": format!("hook dispatch task panicked: {join_err}")}),
                ),
            )
                .into_response();
        }
    };
    match result {
        Ok(Some(decision)) => {
            tracing::info!(
                event = "latency",
                stage = "hook.recv",
                kind = %kind,
                action = action.unwrap_or(""),
                session_id = %session_id,
                cwd = %cwd,
                elapsed_ms = t0.elapsed().as_millis() as u64,
                decision = true,
                "latency hook.recv"
            );
            Json(decision).into_response()
        }
        Ok(None) => {
            tracing::info!(
                event = "latency",
                stage = "hook.recv",
                kind = %kind,
                action = action.unwrap_or(""),
                session_id = %session_id,
                cwd = %cwd,
                elapsed_ms = t0.elapsed().as_millis() as u64,
                decision = false,
                "latency hook.recv"
            );
            // 204 No Content — an EMPTY body, not `{}`. `hook.sh`'s curl
            // silences stderr but NOT stdout, so any response body prints to
            // the hook's stdout; for a SessionStart / UserPromptSubmit hook
            // that stdout lands in the model's context. An empty 204 keeps a
            // non-decision hook's stdout provably ZERO. (The `Ok(Some)` HITL
            // arm above still returns its decision JSON — that body MUST reach
            // claude, so we do NOT silence the curl itself.)
            StatusCode::NO_CONTENT.into_response()
        }
        Err(err) => {
            tracing::warn!(
                event = "latency",
                stage = "hook.recv.err",
                kind = %kind,
                action = action.unwrap_or(""),
                elapsed_ms = t0.elapsed().as_millis() as u64,
                error = %err,
                "latency hook.recv (failed)"
            );
            // 5xx so the script's fallback branch fires through the CLI;
            // the CLI re-runs `dispatch` and surfaces the same error on
            // stderr where the user can see it.
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"ok": false, "error": format!("{err:#}")})),
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    // Unit-level tests live in the `internal_hook_test.rs` integration
    // file (full `axum::serve` + reqwest round-trip) so the dependency
    // surface mirrors the rest of the routes/ tests in this crate
    // (`actions_test.rs`, `dashboard_test.rs`, ...).
    #[test]
    fn router_compiles() {
        // smoke — keeps the inline test module non-empty so
        // `cargo test --lib -p ccteam-web` exercises the route build.
        let _: axum::Router<crate::state::AppState> = super::router();
    }
}
