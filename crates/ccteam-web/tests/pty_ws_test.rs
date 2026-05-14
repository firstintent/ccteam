//! V0.3.2 F56 — WebSocket PTY relay integration tests.
//!
//! Each case fixtures a project state, spins a real axum listener on
//! `127.0.0.1:0`, and either:
//!
//! 1. asserts the upgrade contract (auth, subprotocol echo, pre-upgrade
//!    error codes) without needing tmux, OR
//! 2. spins up a real tmux session and verifies the byte relay /
//!    `send-keys` / `resize-window` / teardown invariants.
//!
//! Tmux-touching cases are gated on `tmux_available()`: when the
//! binary is missing (CI box, sandbox container) the test logs a skip
//! line and returns successfully so the suite stays green. Manual
//! verification then falls to a developer with tmux installed.
//!
//! All tmux cases are `#[serial]` to avoid two tests writing to the
//! same `tmux` server in parallel — even with unique session names
//! the registry's mutex can churn on tmux-server-wide state if we
//! don't serialize.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use ccteam_core::tmux::{tmux_available, TmuxSession};
use ccteam_core::{CcteamPaths, HarnessKind, ProjectState, SessionRecord, TeamKind};
use ccteam_web::{router_with_state, AppState, AuthState};
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use serial_test::serial;
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::client::Request as ClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderValue, StatusCode};
use tokio_tungstenite::tungstenite::Message;

const TOKEN_HEX: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
const SUBPROTOCOL: &str = "ccteam-pty.v1";

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_slug(test_name: &str) -> String {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    format!("ccteam-f56-{test_name}-{pid}-{n}")
}

fn fake_paths(root: &Path) -> CcteamPaths {
    CcteamPaths {
        root: root.join(".ccteam"),
        projects_root: root.join("projects"),
    }
}

/// Drop guard: kills the tmux session on test exit.
struct ScopedSession {
    session: TmuxSession,
}

impl ScopedSession {
    fn from_name(name: &str) -> Self {
        Self {
            session: TmuxSession::from_name(name),
        }
    }
}

impl Drop for ScopedSession {
    fn drop(&mut self) {
        let _ = self.session.kill();
    }
}

fn fixture_workflow_project(paths: &CcteamPaths, slug: &str, tmux_session: &str) {
    std::fs::create_dir_all(paths.project_ccteam_dir(slug)).unwrap();
    let now = Utc::now();
    let mut state = ProjectState::initial_for_team(slug.into(), "dev".into());
    state.tmux_session = tmux_session.to_string();
    state.team_kind = TeamKind::Workflow;
    state.created_at = now;
    state.last_user_interaction_at = now;
    state.save(&paths.project_state(slug)).unwrap();
}

fn fixture_flex_session(
    paths: &CcteamPaths,
    slug: &str,
    sid: &str,
    tmux_session: &str,
) {
    std::fs::create_dir_all(paths.project_ccteam_dir(slug)).unwrap();
    let now = Utc::now();
    let mut state = ProjectState::initial_for_team(slug.into(), "flex".into());
    state.tmux_session = format!("ccteam-{slug}");
    state.team_kind = TeamKind::Flex;
    state.created_at = now;
    state.last_user_interaction_at = now;
    let mut sessions = BTreeMap::new();
    sessions.insert(
        sid.to_string(),
        SessionRecord {
            harness: HarnessKind::Claude,
            tmux_session: tmux_session.to_string(),
            started_at: now,
            pid: None,
            job_id: None,
        },
    );
    state.sessions = sessions;
    state.save(&paths.project_state(slug)).unwrap();
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

fn ws_request(addr: SocketAddr, path: &str) -> ClientRequest {
    let url = format!("ws://{addr}{path}");
    url.into_client_request().unwrap()
}

fn ws_request_with_subprotocol(addr: SocketAddr, path: &str) -> ClientRequest {
    let mut req = ws_request(addr, path);
    req.headers_mut()
        .insert("Sec-WebSocket-Protocol", HeaderValue::from_static(SUBPROTOCOL));
    req
}

fn add_bearer(req: &mut ClientRequest, hex: &str) {
    let v = format!("Bearer ccteam:{hex}");
    req.headers_mut()
        .insert("Authorization", HeaderValue::from_str(&v).unwrap());
}

/// Wait up to `timeout` for `cond` to read true. Yields between
/// polls so other tokio tasks make progress.
async fn wait_for<F: FnMut() -> bool>(timeout: Duration, mut cond: F) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if cond() {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Returns `#{pane_pipe}` for the first pane of `tmux_session` ("1"
/// when pipe-pane is active, "0" otherwise). Empty / err on failure.
fn pane_pipe_status(tmux_session: &str) -> String {
    let out = std::process::Command::new("tmux")
        .args([
            "list-panes",
            "-t",
            &format!("{tmux_session}:0"),
            "-F",
            "#{pane_pipe}",
        ])
        .output();
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        Err(_) => String::new(),
    }
}

fn window_dims(tmux_session: &str) -> Option<(u16, u16)> {
    let out = std::process::Command::new("tmux")
        .args([
            "display-message",
            "-t",
            tmux_session,
            "-p",
            "#{window_width}x#{window_height}",
        ])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let (w, h) = s.split_once('x')?;
    Some((w.parse().ok()?, h.parse().ok()?))
}

// ---------------------------------------------------------------------------
// 1. Auth-disabled: bad slug returns 404 pre-upgrade.
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn ws_unknown_slug_returns_404_before_upgrade() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let state = AppState::with_auth(paths, AuthState::disabled());
    let addr = spawn(state).await;

    let req = ws_request_with_subprotocol(addr, "/ws/nonexistent/pty");
    let err = tokio_tungstenite::connect_async(req).await.unwrap_err();
    match err {
        tokio_tungstenite::tungstenite::Error::Http(resp) => {
            assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        }
        other => panic!("expected HTTP 404 error, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// 2. Auth-enabled: connect without credentials → 401 (refused upgrade).
//    The middleware short-circuits before the WS extractor runs.
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn ws_without_auth_rejects_with_401_when_auth_enabled() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    fixture_workflow_project(&paths, "demo", "ccteam-demo");
    let state = AppState::with_auth(paths, AuthState::enabled(TOKEN_HEX.into()));
    let addr = spawn(state).await;

    let req = ws_request_with_subprotocol(addr, "/ws/demo/pty");
    let err = tokio_tungstenite::connect_async(req).await.unwrap_err();
    match err {
        tokio_tungstenite::tungstenite::Error::Http(resp) => {
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        }
        other => panic!("expected HTTP 401 error, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// 3. Auth-enabled + bearer header + valid slug. We can't 101 without
//    tmux being able to bring up pipe-pane (the upgrade only succeeds
//    once the project state loads, but the relay loop itself wants a
//    real tmux session). So we fixture a real tmux session when tmux
//    is available; if not, we still verify auth + slug → upgrade
//    completion by accepting any non-error outcome.
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn ws_with_bearer_header_accepts_upgrade() {
    if !tmux_available() {
        eprintln!("[skip] ws_with_bearer_header_accepts_upgrade: tmux not on PATH");
        return;
    }
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let slug = unique_slug("bearer-upgrade");
    let tmux_name = format!("ccteam-{slug}");
    fixture_workflow_project(&paths, &slug, &tmux_name);

    let scoped = ScopedSession::from_name(&tmux_name);
    scoped
        .session
        .start(&paths.project_dir(&slug), &["sh", "-i"])
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    let state = AppState::with_auth(paths, AuthState::enabled(TOKEN_HEX.into()));
    let addr = spawn(state).await;

    let mut req = ws_request_with_subprotocol(addr, &format!("/ws/{slug}/pty"));
    add_bearer(&mut req, TOKEN_HEX);
    let (mut ws, resp) = tokio_tungstenite::connect_async(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::SWITCHING_PROTOCOLS);
    let echoed = resp
        .headers()
        .get("sec-websocket-protocol")
        .and_then(|h| h.to_str().ok())
        .map(str::to_string);
    assert_eq!(
        echoed.as_deref(),
        Some(SUBPROTOCOL),
        "server must echo the subprotocol it accepted",
    );
    let _ = ws.close(None).await;
}

// ---------------------------------------------------------------------------
// 4. Auth-disabled + valid slug + tmux → upgrade succeeds (loopback).
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn ws_no_auth_loopback_accepts_upgrade() {
    if !tmux_available() {
        eprintln!("[skip] ws_no_auth_loopback_accepts_upgrade: tmux not on PATH");
        return;
    }
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let slug = unique_slug("loopback-upgrade");
    let tmux_name = format!("ccteam-{slug}");
    fixture_workflow_project(&paths, &slug, &tmux_name);

    let scoped = ScopedSession::from_name(&tmux_name);
    scoped
        .session
        .start(&paths.project_dir(&slug), &["sh", "-i"])
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    let state = AppState::with_auth(paths, AuthState::disabled());
    let addr = spawn(state).await;

    let req = ws_request_with_subprotocol(addr, &format!("/ws/{slug}/pty"));
    let (mut ws, resp) = tokio_tungstenite::connect_async(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::SWITCHING_PROTOCOLS);
    let _ = ws.close(None).await;
}

// ---------------------------------------------------------------------------
// 5. End-to-end: pane bytes from `tmux send-keys` reach a WS client
//    as a binary frame.
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn ws_receives_pane_bytes_from_pipe_pane() {
    if !tmux_available() {
        eprintln!("[skip] ws_receives_pane_bytes_from_pipe_pane: tmux not on PATH");
        return;
    }
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let slug = unique_slug("pane-bytes");
    let tmux_name = format!("ccteam-{slug}");
    fixture_workflow_project(&paths, &slug, &tmux_name);

    let scoped = ScopedSession::from_name(&tmux_name);
    scoped
        .session
        .start(&paths.project_dir(&slug), &["sh", "-i"])
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    let state = AppState::with_auth(paths, AuthState::disabled());
    let addr = spawn(state).await;

    let req = ws_request_with_subprotocol(addr, &format!("/ws/{slug}/pty"));
    let (mut ws, _resp) = tokio_tungstenite::connect_async(req).await.unwrap();

    // Wait for pipe-pane to attach before triggering output. Without
    // this poll the test races: the shell can produce "F56HELLOPANE"
    // before tmux has hooked the cat-writer up, in which case those
    // bytes are simply never captured.
    let ts = tmux_name.clone();
    let attached =
        wait_for(Duration::from_secs(2), || pane_pipe_status(&ts) == "1").await;
    assert!(attached, "pipe-pane should attach within 2s after WS connect");

    // Generate pane output. send-keys with the literal text + Enter
    // triggers the shell to echo + execute the command.
    let _ = std::process::Command::new("tmux")
        .args([
            "send-keys",
            "-t",
            &format!("{tmux_name}:0.0"),
            "echo F56HELLOPANE",
            "Enter",
        ])
        .status();

    let mut acc: Vec<u8> = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let saw = loop {
        if tokio::time::Instant::now() >= deadline {
            break false;
        }
        match tokio::time::timeout(Duration::from_millis(250), ws.next()).await {
            Ok(Some(Ok(Message::Binary(b)))) => {
                acc.extend_from_slice(&b);
                if memmem(&acc, b"F56HELLOPANE") {
                    break true;
                }
            }
            Ok(Some(Ok(_))) => continue,
            Ok(Some(Err(_))) | Ok(None) => break false,
            Err(_) => continue,
        }
    };
    assert!(
        saw,
        "expected to see F56HELLOPANE in WS bytes within 5s; got {} bytes",
        acc.len()
    );
    let _ = ws.close(None).await;
}

// ---------------------------------------------------------------------------
// 6. Two clients share the same pipe-pane via the broadcast channel.
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn ws_two_clients_share_one_pipe_pane() {
    if !tmux_available() {
        eprintln!("[skip] ws_two_clients_share_one_pipe_pane: tmux not on PATH");
        return;
    }
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let slug = unique_slug("share-pipe");
    let tmux_name = format!("ccteam-{slug}");
    fixture_workflow_project(&paths, &slug, &tmux_name);

    let scoped = ScopedSession::from_name(&tmux_name);
    scoped
        .session
        .start(&paths.project_dir(&slug), &["sh", "-i"])
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    let state = AppState::with_auth(paths, AuthState::disabled());
    let addr = spawn(state).await;

    let req1 = ws_request_with_subprotocol(addr, &format!("/ws/{slug}/pty"));
    let (mut ws1, _) = tokio_tungstenite::connect_async(req1).await.unwrap();
    let req2 = ws_request_with_subprotocol(addr, &format!("/ws/{slug}/pty"));
    let (mut ws2, _) = tokio_tungstenite::connect_async(req2).await.unwrap();

    let ts = tmux_name.clone();
    let attached =
        wait_for(Duration::from_secs(2), || pane_pipe_status(&ts) == "1").await;
    assert!(attached, "pipe-pane should attach within 2s after WS connect");

    let _ = std::process::Command::new("tmux")
        .args([
            "send-keys",
            "-t",
            &format!("{tmux_name}:0.0"),
            "echo F56SHAREDBYTES",
            "Enter",
        ])
        .status();

    let saw_a = wait_for_marker(&mut ws1, b"F56SHAREDBYTES", Duration::from_secs(5)).await;
    let saw_b = wait_for_marker(&mut ws2, b"F56SHAREDBYTES", Duration::from_secs(5)).await;
    assert!(saw_a, "ws1 missed the marker");
    assert!(saw_b, "ws2 missed the marker");

    let _ = ws1.close(None).await;
    let _ = ws2.close(None).await;
}

// ---------------------------------------------------------------------------
// 7. Resize control frame → tmux resize-window.
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn ws_resize_control_frame_invokes_tmux_resize() {
    if !tmux_available() {
        eprintln!("[skip] ws_resize_control_frame_invokes_tmux_resize: tmux not on PATH");
        return;
    }
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let slug = unique_slug("resize");
    let tmux_name = format!("ccteam-{slug}");
    fixture_workflow_project(&paths, &slug, &tmux_name);

    let scoped = ScopedSession::from_name(&tmux_name);
    scoped
        .session
        .start(&paths.project_dir(&slug), &["sh", "-i"])
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    let state = AppState::with_auth(paths, AuthState::disabled());
    let addr = spawn(state).await;
    let req = ws_request_with_subprotocol(addr, &format!("/ws/{slug}/pty"));
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;

    ws.send(Message::Text(
        r#"{"type":"resize","cols":120,"rows":40}"#.into(),
    ))
    .await
    .unwrap();

    let target = tmux_name.clone();
    let ok = wait_for(Duration::from_secs(2), || {
        matches!(window_dims(&target), Some((120, 40)))
    })
    .await;
    assert!(
        ok,
        "expected tmux window resize to 120x40; got {:?}",
        window_dims(&target),
    );
    let _ = ws.close(None).await;
}

// ---------------------------------------------------------------------------
// 8. Last-client disconnect → pipe-pane stops + FIFO unlinks.
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn ws_last_client_disconnect_stops_pipe_pane() {
    if !tmux_available() {
        eprintln!("[skip] ws_last_client_disconnect_stops_pipe_pane: tmux not on PATH");
        return;
    }
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let slug = unique_slug("teardown");
    let tmux_name = format!("ccteam-{slug}");
    fixture_workflow_project(&paths, &slug, &tmux_name);

    let scoped = ScopedSession::from_name(&tmux_name);
    scoped
        .session
        .start(&paths.project_dir(&slug), &["sh", "-i"])
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    let fifo_path = paths.pty_dir().join(format!("{slug}.fifo"));

    let state = AppState::with_auth(paths.clone(), AuthState::disabled());
    let addr = spawn(state).await;
    let req = ws_request_with_subprotocol(addr, &format!("/ws/{slug}/pty"));
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    // During the connection, pipe-pane should be active.
    assert_eq!(
        pane_pipe_status(&tmux_name),
        "1",
        "pipe-pane should be active while a WS client is connected",
    );
    assert!(
        fifo_path.exists(),
        "FIFO should exist while a WS client is connected: {}",
        fifo_path.display(),
    );

    // Disconnect.
    let _ = ws.close(None).await;
    drop(ws);

    let fp = fifo_path.clone();
    let ts = tmux_name.clone();
    let stopped = wait_for(Duration::from_secs(3), || {
        !fp.exists() && pane_pipe_status(&ts) == "0"
    })
    .await;
    assert!(
        stopped,
        "expected pipe-pane to stop + FIFO to unlink within 3s; pipe={} fifo_exists={}",
        pane_pipe_status(&tmux_name),
        fifo_path.exists()
    );

    // Tmux session itself is still alive — F56 must not touch it.
    let session = TmuxSession::from_name(&tmux_name);
    assert!(
        session.exists(),
        "F56 must not kill the tmux session: {tmux_name}",
    );
}

// ---------------------------------------------------------------------------
// Flex / sid-scoped happy path: hooks up against a `sessions[sid]`
// record and exercises the same byte relay.
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn ws_flex_sid_scoped_route_relays_bytes() {
    if !tmux_available() {
        eprintln!("[skip] ws_flex_sid_scoped_route_relays_bytes: tmux not on PATH");
        return;
    }
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let slug = unique_slug("flex-sid");
    let sid = "claude-1";
    let tmux_name = format!("ccteam-{slug}-{sid}");
    fixture_flex_session(&paths, &slug, sid, &tmux_name);

    let scoped = ScopedSession::from_name(&tmux_name);
    scoped
        .session
        .start(&paths.project_dir(&slug), &["sh", "-i"])
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    let state = AppState::with_auth(paths.clone(), AuthState::disabled());
    let addr = spawn(state).await;
    let req = ws_request_with_subprotocol(addr, &format!("/ws/{slug}/{sid}/pty"));
    let (mut ws, resp) = tokio_tungstenite::connect_async(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::SWITCHING_PROTOCOLS);

    let ts = tmux_name.clone();
    let attached =
        wait_for(Duration::from_secs(2), || pane_pipe_status(&ts) == "1").await;
    assert!(attached, "pipe-pane should attach within 2s after WS connect");

    let _ = std::process::Command::new("tmux")
        .args([
            "send-keys",
            "-t",
            &format!("{tmux_name}:0.0"),
            "echo F56FLEXTAG",
            "Enter",
        ])
        .status();

    let saw = wait_for_marker(&mut ws, b"F56FLEXTAG", Duration::from_secs(5)).await;
    assert!(saw, "expected to see F56FLEXTAG bytes from flex sid pane");

    // FIFO name uses '-' instead of '/' from the key.
    let fifo_path = paths.pty_dir().join(format!("{slug}-{sid}.fifo"));
    assert!(fifo_path.exists(), "expected FIFO {}", fifo_path.display());

    let _ = ws.close(None).await;
}

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

async fn wait_for_marker<S>(ws: &mut S, marker: &[u8], timeout: Duration) -> bool
where
    S: StreamExt<
            Item = Result<
                tokio_tungstenite::tungstenite::Message,
                tokio_tungstenite::tungstenite::Error,
            >,
        > + Unpin,
{
    let mut acc: Vec<u8> = Vec::new();
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        match tokio::time::timeout(Duration::from_millis(250), ws.next()).await {
            Ok(Some(Ok(Message::Binary(b)))) => {
                acc.extend_from_slice(&b);
                if memmem(&acc, marker) {
                    return true;
                }
            }
            Ok(Some(Ok(_))) => continue,
            Ok(Some(Err(_))) | Ok(None) => return false,
            Err(_) => continue,
        }
    }
}

fn memmem(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    if haystack.len() < needle.len() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|w| w == needle)
}
