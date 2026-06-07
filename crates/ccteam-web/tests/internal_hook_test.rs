//! V0.6.1 F139 — `POST /internal/hook/:kind[/:action]` integration tests.
//!
//! These tests exercise the full axum + reqwest round-trip the
//! `~/.ccteam/hooks/hook.sh` script uses at production runtime:
//!
//! 1. `intercept-ask` → 200 + Claude-Code-deny JSON (the only hook that
//!    returns a structured decision).
//! 2. `progress-append/<event>` → 200 + `{}` with the side-effect
//!    (`<root>/progress/<slug>.jsonl` gets a line appended).
//! 3. unknown kind → 500 + `{ok:false,error:...}` so the script's
//!    fallback path can re-run through the CLI binary.
//! 4. bearer auth gates the route: a non-loopback auth-on AppState
//!    returns 401 without the header, 200 with it.
//!
//! `auth.enabled = false` by default (loopback bind heuristic) — the
//! production hook.sh only sends `Authorization: Bearer ccteam:<hex>`
//! when `~/.ccteam/web-token` exists.

use std::net::SocketAddr;

use ccteam_core::{bootstrap_project, disable_tool_surface_bootstrap_for_tests, CcteamPaths};
use ccteam_web::{router_with_state, AppState};
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::net::TcpListener;

fn fake_paths(root: &std::path::Path) -> CcteamPaths {
    CcteamPaths {
        root: root.join(".ccteam"),
        projects_root: root.join("projects"),
    }
}

async fn spawn(state: AppState) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router_with_state(state);
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    tokio::task::yield_now().await;
    addr
}

#[tokio::test]
async fn post_intercept_ask_returns_deny_decision_json() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let state = AppState::new(paths);
    let addr = spawn(state).await;

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/internal/hook/intercept-ask"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["hookSpecificOutput"]["permissionDecision"], "deny");
    assert_eq!(body["hookSpecificOutput"]["hookEventName"], "PreToolUse");
}

#[tokio::test]
async fn post_progress_append_returns_empty_object_and_writes_jsonl() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    disable_tool_surface_bootstrap_for_tests();
    bootstrap_project(&paths, "demo", "demo", "dev").unwrap();
    let progress = paths.progress_jsonl("demo");
    let cwd = paths.project_dir("demo");

    let state = AppState::new(paths);
    let addr = spawn(state).await;

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/internal/hook/progress-append/Stop"))
        .json(&json!({"cwd": cwd.display().to_string()}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body, json!({}));

    // Side effect: a Stop event line lives in the progress jsonl.
    let line = std::fs::read_to_string(&progress).expect("progress jsonl written");
    assert!(
        line.contains("\"event\":\"Stop\""),
        "expected Stop event, got: {line}",
    );
}

#[tokio::test]
async fn post_unknown_kind_returns_500_with_error_body() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let state = AppState::new(paths);
    let addr = spawn(state).await;

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/internal/hook/bogus"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 500);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["ok"], false);
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("unknown hook kind"),
        "expected unknown-kind error, got: {body}"
    );
}

/// F186 — daemon process does not inherit the firing claude pane's
/// env (it runs in its own process tree), so `CCTEAM_CHAT_ROLE`
/// reaches it only via the `X-Ccteam-Role` HTTP header set by
/// `hook.sh`. Without this path the chat-mode SessionStart hook lands
/// `role=""` in progress.jsonl and the F176 active-session-id marker
/// is never written → F177 tail loop has no target jsonl → squad mode
/// silently breaks in production.
#[tokio::test]
async fn chat_progress_session_start_uses_role_header() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    disable_tool_surface_bootstrap_for_tests();
    bootstrap_project(&paths, "demo", "demo", "chat").unwrap();
    let progress = paths.progress_jsonl("demo");
    let cwd = paths.project_dir("demo");
    // v0.8.8 F1 — the marker dir is keyed by the ccteam session sid, which the
    // daemon HTTP path learns from the `x-ccteam-sid` header (folded into
    // stdin `ccteam_sid`).
    let marker = cwd.join(".ccteam/chat/s1/active-session-id");

    let state = AppState::new(paths);
    let addr = spawn(state).await;

    let resp = reqwest::Client::new()
        .post(format!(
            "http://{addr}/internal/hook/chat-progress/session-start"
        ))
        .header("x-ccteam-role", "alice")
        .header("x-ccteam-slug", "demo")
        .header("x-ccteam-sid", "s1")
        .json(&json!({
            "hook_event_name": "SessionStart",
            "session_id": "sid-f186",
            "transcript_path": "/tmp/whatever.jsonl",
            "cwd": cwd.display().to_string(),
            "source": "startup",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Progress line should carry `role=alice` even though the daemon
    // process itself has no `CCTEAM_CHAT_ROLE` in env.
    let line = std::fs::read_to_string(&progress).expect("progress jsonl written");
    assert!(
        line.contains("\"event\":\"chat_session_started\""),
        "expected chat_session_started, got: {line}",
    );
    assert!(
        line.contains("\"role\":\"alice\""),
        "expected role=alice from X-Ccteam-Role header injection, got: {line}",
    );

    // F176 active-session-id marker should land under the per-session sid dir
    // — this is the load-bearing side effect F186 protects (v0.8.8 F1 re-keys
    // it from role to sid). The marker *body* stays Anthropic's session UUID.
    let marker_body = std::fs::read_to_string(&marker).expect("active-session-id marker written");
    assert_eq!(marker_body.trim(), "sid-f186");
}

#[tokio::test]
async fn chat_progress_user_prompt_refreshes_marker() {
    // Regression: a missed SessionStart (or a mid-session transcript
    // rotation like /compact) used to strand the tail loop on a stale
    // jsonl, so the agent's reply was never read (silent bot despite a
    // healthy pane). user-prompt now also refreshes the active-session-id
    // marker, which self-heals it.
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    disable_tool_surface_bootstrap_for_tests();
    bootstrap_project(&paths, "demo", "demo", "chat").unwrap();
    let cwd = paths.project_dir("demo");
    // v0.8.8 F1 — sid-keyed marker dir; sid arrives via the x-ccteam-sid header.
    let marker = cwd.join(".ccteam/chat/s1/active-session-id");

    let state = AppState::new(paths);
    let addr = spawn(state).await;

    let resp = reqwest::Client::new()
        .post(format!(
            "http://{addr}/internal/hook/chat-progress/user-prompt"
        ))
        .header("x-ccteam-role", "alice")
        .header("x-ccteam-slug", "demo")
        .header("x-ccteam-sid", "s1")
        .json(&json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": "sid-up-heal",
            "cwd": cwd.display().to_string(),
            "prompt": "which project is this",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let marker_body =
        std::fs::read_to_string(&marker).expect("active-session-id marker written on user-prompt");
    assert_eq!(marker_body.trim(), "sid-up-heal");
}

/// F186 — payload-shipped `role` wins over header so we don't clobber
/// upstream-provided identity. Defensive: if Anthropic ever ships
/// `role` in the hook stdin, we honor it.
#[tokio::test]
async fn chat_progress_payload_role_overrides_header() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    disable_tool_surface_bootstrap_for_tests();
    bootstrap_project(&paths, "demo", "demo", "chat").unwrap();
    let progress = paths.progress_jsonl("demo");
    let cwd = paths.project_dir("demo");

    let state = AppState::new(paths);
    let addr = spawn(state).await;

    let resp = reqwest::Client::new()
        .post(format!(
            "http://{addr}/internal/hook/chat-progress/session-start"
        ))
        .header("x-ccteam-role", "header-role")
        .json(&json!({
            "hook_event_name": "SessionStart",
            "session_id": "sid-payload",
            "transcript_path": "/tmp/whatever.jsonl",
            "cwd": cwd.display().to_string(),
            "source": "startup",
            "role": "payload-role",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let line = std::fs::read_to_string(&progress).expect("progress jsonl written");
    assert!(
        line.contains("\"role\":\"payload-role\""),
        "payload role must win over header, got: {line}",
    );
}

#[tokio::test]
async fn auth_on_rejects_without_bearer_then_accepts_with_it() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let token = "deadbeefdeadbeefdeadbeefdeadbeef";
    let state = AppState::with_auth(paths, ccteam_web::auth::AuthState::enabled(token.into()));
    let addr = spawn(state).await;

    // Missing header ⇒ 401 (auth_layer gates the whole stateful
    // router; `/internal/hook/*` is wrapped along with everything else).
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/internal/hook/intercept-ask"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    // With the matching bearer ⇒ 200.
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/internal/hook/intercept-ask"))
        .header("authorization", format!("Bearer ccteam:{token}"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}
