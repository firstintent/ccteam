//! v0.9.0 W3 (F3, tech-design §4.1/§4.2) — the satellite side of
//! `ccteam-exec.v1`. `ccteam host serve` embeds [`satellite_router`] (plus
//! its own heartbeat loop, owned by the CLI) so a machine can run vendor
//! CLIs on the main daemon's behalf.
//!
//! **The satellite is a protocol-blind byte pump** (red line: no
//! scraping, no interpretation): it never parses stream-json / ACP /
//! JSON-RPC, only bytes. Its own safety invariants (tech-design §4.2):
//!
//! 1. `vendor` → binary resolved from ITS OWN `CCTEAM_<VENDOR>_BIN` env or
//!    `PATH` default name — the wire NEVER carries a binary path.
//! 2. `slug` → cwd resolved from ITS OWN `~/.ccteam/config.yaml::projects[]`
//!    — an unregistered slug is rejected, never guessed.
//! 3. `files[].relpath` must resolve (lexically, no traversal) under
//!    `<project>/.ccteam/chat/<sid>/`.
//! 4. `env` — only `CCTEAM_*` keys are honored, merged over the
//!    satellite's own environment.
//! 5. `files[].content` may embed the literal token `{{DAEMON_URL}}`,
//!    substituted with this satellite's own `SatelliteSelf::daemon_url`.
//!
//! Bearer auth (`Authorization: Bearer <agent_token>`, `ct_eq`) gates
//! `GET /ws/exec` before the WS upgrade — a bad/missing token never even
//! reaches the exec handler.

use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use ccteam_core::{session_secret, CcteamPaths};
use ccteam_harness::{ExecExit, ExecFile, ExecSpec, ExecStarted, EXEC_SUBPROTOCOL};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

/// One vendor this satellite is willing to run + its `CCTEAM_*_BIN`
/// override / default `PATH` name. The wire `vendor` token is checked
/// against this allowlist — an unlisted vendor is rejected
/// (`vendor-not-allowed`), never run.
const ALLOWED_VENDORS: &[(&str, &str, &str)] = &[
    ("claude", ccteam_harness::CLAUDE_BIN_ENV, "claude"),
    ("codex", ccteam_harness::CODEX_BIN_ENV, "codex"),
    ("grok", ccteam_harness::GROK_BIN_ENV, "grok"),
    ("opencode", ccteam_harness::OPENCODE_BIN_ENV, "opencode"),
];

#[derive(Clone)]
struct SatelliteState {
    paths: Arc<CcteamPaths>,
    agent_token: Arc<String>,
    daemon_url: Arc<String>,
}

/// Build the satellite router: `GET /health` + `GET /ws/exec`. `paths` is
/// THIS machine's `CcteamPaths` (its own project registry); `agent_token`
/// is the bearer `GET /ws/exec` requires (the satellite's own
/// `SatelliteSelf::agent_token`); `daemon_url` is substituted for the
/// `{{DAEMON_URL}}` template token in shipped file content.
pub fn satellite_router(paths: CcteamPaths, agent_token: String, daemon_url: String) -> Router {
    let state = SatelliteState {
        paths: Arc::new(paths),
        agent_token: Arc::new(agent_token),
        daemon_url: Arc::new(daemon_url),
    };
    Router::new()
        .route("/health", get(handle_health))
        .route("/ws/exec", get(handle_ws_exec))
        .with_state(state)
}

async fn handle_health() -> impl IntoResponse {
    Json(json!({"status": "ok", "version": env!("CARGO_PKG_VERSION")}))
}

fn bearer_token(headers: &HeaderMap) -> String {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("")
        .trim()
        .trim_start_matches("ccteam:")
        .to_string()
}

async fn handle_ws_exec(
    State(state): State<SatelliteState>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let presented = bearer_token(&headers);
    if presented.is_empty() || !session_secret::ct_eq(&presented, &state.agent_token) {
        return (StatusCode::UNAUTHORIZED, "invalid or missing agent token").into_response();
    }
    ws.protocols([EXEC_SUBPROTOCOL])
        .on_upgrade(move |socket| run_exec_session(socket, state))
}

/// Resolve `vendor` against [`ALLOWED_VENDORS`]. `None` ⇒ not allowed.
fn resolve_vendor_bin(vendor: &str) -> Option<String> {
    ALLOWED_VENDORS
        .iter()
        .find(|(v, _, _)| *v == vendor)
        .map(|(_, env, default)| std::env::var(env).unwrap_or_else(|_| (*default).to_string()))
}

/// Resolve `slug` against THIS machine's OWN project registry
/// (`~/.ccteam/config.yaml::projects[]`). `None` ⇒ unregistered — never a
/// guessed fallback path (unlike `CcteamPaths::project_dir`, which is
/// deliberately lenient for the local daemon's own use).
fn resolve_project_dir(paths: &CcteamPaths, slug: &str) -> Option<PathBuf> {
    let cfg = ccteam_core::config::load(&paths.root).ok()?;
    cfg.projects
        .into_iter()
        .find(|p| p.slug == slug)
        .map(|p| p.path)
}

/// Lexically resolve `relpath` against `cwd` and require the result to
/// fall under `confine_under` (both already-absolute). Rejects absolute
/// relpaths and any `..` component — no traversal, and no reliance on
/// `canonicalize()` (the file may not exist yet).
fn confined_join(cwd: &Path, relpath: &str, confine_under: &Path) -> Option<PathBuf> {
    let rel = Path::new(relpath);
    let mut normalized = PathBuf::new();
    for comp in rel.components() {
        match comp {
            Component::Normal(s) => normalized.push(s),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if normalized.as_os_str().is_empty() {
        return None;
    }
    let full = cwd.join(&normalized);
    full.starts_with(confine_under).then_some(full)
}

/// Materialize `files` under `<cwd>/.ccteam/chat/<sid>/`, substituting the
/// `{{DAEMON_URL}}` template token. Returns `Err(readable message)` on the
/// first violation — nothing is left half-written on a reject (each file
/// is validated before any write starts).
fn write_confined_files(
    cwd: &Path,
    sid: &str,
    files: &[ExecFile],
    daemon_url: &str,
) -> Result<(), String> {
    let confine_under = cwd.join(".ccteam").join("chat").join(sid);
    let mut resolved = Vec::with_capacity(files.len());
    for f in files {
        let dest = confined_join(cwd, &f.relpath, &confine_under)
            .ok_or_else(|| format!("relpath escapes .ccteam/chat/{sid}/: {}", f.relpath))?;
        resolved.push((
            dest,
            f.content.replace(ExecSpec::DAEMON_URL_TOKEN, daemon_url),
        ));
    }
    for (dest, content) in resolved {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create {}: {e}", parent.display()))?;
        }
        std::fs::write(&dest, content.as_bytes())
            .map_err(|e| format!("write {}: {e}", dest.display()))?;
    }
    Ok(())
}

async fn send_started(
    socket: &mut WebSocket,
    ok: bool,
    pid: Option<u32>,
    code: &str,
    message: &str,
) {
    let body = ExecStarted {
        ok,
        pid,
        code: (!code.is_empty()).then(|| code.to_string()),
        message: (!message.is_empty()).then(|| message.to_string()),
    };
    if let Ok(json) = serde_json::to_string(&body) {
        let _ = socket.send(Message::Text(json.into())).await;
    }
}

/// The whole exec-bridge session lifecycle for one WS connection: read
/// [`ExecSpec`] → validate (vendor/slug/files) → spawn → bidirectional
/// byte bridge → [`ExecExit`] tail. Never panics on a malformed/hostile
/// peer — every failure path sends a readable [`ExecStarted`] rejection
/// (or silently drops a connection that never sent a valid spec at all).
async fn run_exec_session(mut socket: WebSocket, state: SatelliteState) {
    let spec: ExecSpec = match socket.recv().await {
        Some(Ok(Message::Text(t))) => match serde_json::from_str(&t) {
            Ok(s) => s,
            Err(e) => {
                send_started(
                    &mut socket,
                    false,
                    None,
                    "bad-spec",
                    &format!("invalid ExecSpec: {e}"),
                )
                .await;
                return;
            }
        },
        _ => return, // peer vanished / sent garbage before any spec — nothing to reject with.
    };

    let Some(bin) = resolve_vendor_bin(&spec.vendor) else {
        send_started(
            &mut socket,
            false,
            None,
            "vendor-not-allowed",
            &format!(
                "vendor `{}` is not on this satellite's allowlist",
                spec.vendor
            ),
        )
        .await;
        return;
    };

    let Some(cwd) = resolve_project_dir(&state.paths, &spec.slug) else {
        send_started(
            &mut socket,
            false,
            None,
            "unknown-slug",
            &format!(
                "project `{}` is not registered on this host; run `ccteam init` here first",
                spec.slug
            ),
        )
        .await;
        return;
    };

    if let Err(msg) = write_confined_files(&cwd, &spec.sid, &spec.files, &state.daemon_url) {
        send_started(&mut socket, false, None, "bad-spec", &msg).await;
        return;
    }

    let mut cmd = Command::new(&bin);
    cmd.args(&spec.args)
        .current_dir(&cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // Env allowlist (§4.2 invariant ④): only CCTEAM_* keys, merged OVER
    // the satellite's own environment (never a raw passthrough of the
    // wire env, and never clobbering PATH/HOME/etc).
    for (k, v) in &spec.env {
        if k.starts_with("CCTEAM_") {
            cmd.env(k, v);
        }
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            send_started(
                &mut socket,
                false,
                None,
                "spawn-failed",
                &format!("{bin}: {e}"),
            )
            .await;
            return;
        }
    };
    let pid = child.id();
    send_started(&mut socket, true, pid, "", "").await;

    let Some(mut stdout) = child.stdout.take() else {
        return;
    };
    let Some(mut stdin) = child.stdin.take() else {
        return;
    };
    if let Some(mut stderr) = child.stderr.take() {
        // stderr → tracing only, NEVER the wire (red line: no terminal
        // scraping surfaced to the caller; this is diagnostic-only).
        tokio::spawn(async move {
            let mut buf = Vec::new();
            let _ = stderr.read_to_end(&mut buf).await;
            if !buf.is_empty() {
                tracing::warn!(
                    stderr = %String::from_utf8_lossy(&buf),
                    "ccteam-exec.v1: child stderr (diagnostic only, never streamed)"
                );
            }
        });
    }

    let mut buf = [0u8; 8192];
    let exit_status = loop {
        tokio::select! {
            n = stdout.read(&mut buf) => {
                match n {
                    Ok(0) => break child.wait().await.ok(),
                    Ok(n) => {
                        if socket.send(Message::Binary(buf[..n].to_vec().into())).await.is_err() {
                            break None;
                        }
                    }
                    Err(_) => break None,
                }
            }
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Binary(b))) => {
                        if stdin.write_all(&b).await.is_err() {
                            break None;
                        }
                    }
                    Some(Ok(Message::Text(t))) if t.contains("stdin_close") => {
                        let _ = stdin.shutdown().await;
                    }
                    Some(Ok(Message::Text(_) | Message::Ping(_) | Message::Pong(_))) => {}
                    Some(Ok(Message::Close(_))) | None => break None,
                    Some(Err(_)) => break None,
                }
            }
            status = child.wait() => {
                break status.ok();
            }
        }
    };

    // WS dropped mid-flight or an IO error broke the loop before the
    // child exited on its own — kill it (red line honesty: WS-side
    // grandchildren of `bin` are NOT killed, a documented simplification;
    // vendor `--resume` makes the next connect's context whole again).
    let exit_status = match exit_status {
        Some(s) => Some(s),
        None => {
            let _ = child.start_kill();
            child.wait().await.ok()
        }
    };
    let ev = build_exec_exit(exit_status);
    if let Ok(json) = serde_json::to_string(&ev) {
        let _ = socket.send(Message::Text(json.into())).await;
    }
}

#[cfg(unix)]
fn build_exec_exit(status: Option<std::process::ExitStatus>) -> ExecExit {
    use std::os::unix::process::ExitStatusExt;
    match status {
        Some(s) => ExecExit {
            exit: s.code(),
            signal: s.signal().map(|n| n.to_string()),
        },
        None => ExecExit::default(),
    }
}

#[cfg(not(unix))]
fn build_exec_exit(status: Option<std::process::ExitStatus>) -> ExecExit {
    ExecExit {
        exit: status.and_then(|s| s.code()),
        signal: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn resolve_vendor_bin_rejects_unlisted_vendor() {
        assert!(resolve_vendor_bin("gemini").is_none());
        assert!(resolve_vendor_bin("claude").is_some());
    }

    #[test]
    fn resolve_project_dir_none_for_unregistered_slug() {
        let tmp = TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        std::fs::create_dir_all(&paths.root).unwrap();
        assert!(resolve_project_dir(&paths, "demo").is_none());

        ccteam_core::config::upsert_project(
            &paths.root,
            ccteam_core::config::ProjectEntry {
                slug: "demo".into(),
                path: tmp.path().join("projects/demo"),
                team: "dev".into(),
                installed_at: chrono::Utc::now(),
            },
        )
        .unwrap();
        assert_eq!(
            resolve_project_dir(&paths, "demo"),
            Some(tmp.path().join("projects/demo"))
        );
        assert!(resolve_project_dir(&paths, "other").is_none());
    }

    #[test]
    fn confined_join_rejects_traversal_and_absolute() {
        let cwd = Path::new("/home/sat/projects/demo");
        let confine = cwd.join(".ccteam/chat/s7");
        assert!(confined_join(cwd, "../../etc/passwd", &confine).is_none());
        assert!(confined_join(cwd, "/etc/passwd", &confine).is_none());
        assert!(confined_join(cwd, ".ccteam/chat/s7/../s8/mcp.json", &confine).is_none());
        assert_eq!(
            confined_join(cwd, ".ccteam/chat/s7/mcp.json", &confine),
            Some(cwd.join(".ccteam/chat/s7/mcp.json"))
        );
    }

    #[test]
    fn write_confined_files_substitutes_daemon_url_token() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path().join("demo");
        std::fs::create_dir_all(&cwd).unwrap();
        let files = vec![ExecFile {
            relpath: ".ccteam/chat/s7/mcp.json".into(),
            content: format!(r#"{{"url":"{}/mcp"}}"#, ExecSpec::DAEMON_URL_TOKEN),
        }];
        write_confined_files(&cwd, "s7", &files, "http://127.0.0.1:7331").unwrap();
        let written = std::fs::read_to_string(cwd.join(".ccteam/chat/s7/mcp.json")).unwrap();
        assert_eq!(written, r#"{"url":"http://127.0.0.1:7331/mcp"}"#);
    }

    #[test]
    fn write_confined_files_rejects_traversal_and_writes_nothing() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path().join("demo");
        std::fs::create_dir_all(&cwd).unwrap();
        let files = vec![
            ExecFile {
                relpath: ".ccteam/chat/s7/mcp.json".into(),
                content: "ok".into(),
            },
            ExecFile {
                relpath: "../../../etc/passwd".into(),
                content: "pwned".into(),
            },
        ];
        let err = write_confined_files(&cwd, "s7", &files, "http://x").unwrap_err();
        assert!(err.contains("escapes"), "got: {err}");
        // Nothing partially written — validated before any write starts.
        assert!(!cwd.join(".ccteam/chat/s7/mcp.json").exists());
    }
}
