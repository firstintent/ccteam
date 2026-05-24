//! V0.6.1 F139 — `POST /internal/hook/:kind[/:action]`.
//!
//! Routes Claude Code hook firings through the long-running daemon
//! instead of paying the ~200 ms `ccteam` Rust binary start-up tax per
//! hook (4 hooks × ~1.5 turns/sec ⇒ user-visible 1+ s of chat
//! sluggishness). The wire shape is intentionally narrow: the script in
//! `~/.ccteam/hooks/hook.sh` packages the Claude Code hook stdin as the
//! POST body and forwards the subcommand `<kind>` (`progress-append` /
//! `load-context` / `intercept-ask` / `chat-progress`) + the optional
//! `<action>` (the event-type arg for `progress-append` /
//! `chat-progress`) through the path. The handler calls
//! `ccteam_hooks::dispatch` — the same library entry the CLI uses —
//! and returns:
//!
//! - `{}` for fire-and-forget hooks (Claude Code treats empty / `{}`
//!   stdout as "allow with no notes"),
//! - the structured decision JSON for `intercept-ask` (the assistant
//!   sees the deny reason inline).
//!
//! Auth: this router sits under the same `auth_layer` middleware as the
//! rest of the stateful router. Loopback bind defaults to no auth (the
//! script still works without sending a token); non-loopback bind
//! generates `~/.ccteam/web-token` and the script reads that file +
//! sends `Authorization: Bearer ccteam:<hex>`.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use serde_json::{json, Value};

use crate::state::AppState;

/// Build the `POST /internal/hook/:kind[/:action]` router.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/internal/hook/{kind}", post(handle_no_action))
        .route("/internal/hook/{kind}/{action}", post(handle_with_action))
}

async fn handle_no_action(
    State(app): State<AppState>,
    Path(kind): Path<String>,
    body: Option<Json<Value>>,
) -> Response {
    dispatch(&app, &kind, None, body.map(|Json(v)| v))
}

async fn handle_with_action(
    State(app): State<AppState>,
    Path((kind, action)): Path<(String, String)>,
    body: Option<Json<Value>>,
) -> Response {
    dispatch(&app, &kind, Some(&action), body.map(|Json(v)| v))
}

/// Shared invocation path. `None` body is normalized to `Value::Null`
/// so handlers that don't actually read stdin (today: `intercept-ask`)
/// still work when the client sent zero bytes.
fn dispatch(app: &AppState, kind: &str, action: Option<&str>, body: Option<Value>) -> Response {
    let t0 = std::time::Instant::now();
    let stdin = body.unwrap_or(Value::Null);
    // Try to pull session id / role from the Claude Code hook stdin
    // payload — useful for joining hook latency rows to other stages.
    // Best-effort: any field missing → empty string.
    let session_id = stdin
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let cwd = stdin.get("cwd").and_then(|v| v.as_str()).unwrap_or("");
    match ccteam_hooks::dispatch(&app.paths, kind, action, &stdin) {
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
            Json(json!({})).into_response()
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
