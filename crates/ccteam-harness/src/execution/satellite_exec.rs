//! The satellite side of `ccteam-exec.v1` — a **protocol-blind byte pump**
//! (red line: no scraping, no interpretation): it never parses stream-json /
//! ACP / JSON-RPC, only bytes. v0.9.0 network inversion moved this engine
//! out of the old `ccteam host serve` WS *server* into a transport-generic
//! form: the satellite now *dials back* to the daemon and runs this engine
//! over the resulting client socket (`ccteam-web::satellite`), and
//! in-process tests run it straight over an [`ExecBridge`] half — same
//! bounds a split tungstenite socket satisfies, no WS required.
//!
//! Safety invariants (unchanged from the listener era, tech-design §4.2):
//!
//! 1. `vendor` → binary resolved from THIS machine's `CCTEAM_<VENDOR>_BIN`
//!    env or `PATH` default name — the wire NEVER carries a binary path.
//! 2. `slug` → cwd resolved by the caller-supplied [`SatelliteExecCtx::resolve_project_dir`]
//!    (this machine's own project registry) — an unregistered slug is
//!    rejected, never guessed.
//! 3. `files[].relpath` must resolve (lexically, no traversal) under
//!    `<project>/.ccteam/chat/<sid>/`.
//! 4. `env` — only `CCTEAM_*` keys are honored, merged over this machine's
//!    own environment.
//! 5. `files[].content` may embed the literal token `{{DAEMON_URL}}`,
//!    substituted with this satellite's own daemon URL.
//!
//! Production stability: the pump treats a link with no inbound frame for
//! [`super::host_channel::IDLE_TIMEOUT`] as half-open — the child is killed
//! and the session ends readable (the daemon side sees EOF → next spawn
//! re-gates + `--resume`s). The daemon side pings every
//! [`super::host_channel::KEEPALIVE_PERIOD`], so a healthy link never
//! trips this.

use std::path::{Component, Path, PathBuf};
use std::process::Stdio;

use futures::{Sink, SinkExt, Stream, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};

use super::host_channel::IDLE_TIMEOUT;
use super::remote_exec::{ExecExit, ExecFile, ExecSpec, ExecStarted};

/// One vendor this satellite is willing to run + its `CCTEAM_*_BIN`
/// override / default `PATH` name. The wire `vendor` token is checked
/// against this allowlist — an unlisted vendor is rejected
/// (`vendor-not-allowed`), never run.
const ALLOWED_VENDORS: &[(&str, &str, &str)] = &[
    ("claude", crate::CLAUDE_BIN_ENV, "claude"),
    ("codex", crate::CODEX_BIN_ENV, "codex"),
    ("grok", crate::GROK_BIN_ENV, "grok"),
    ("opencode", crate::OPENCODE_BIN_ENV, "opencode"),
];

/// How long the engine waits for the daemon's [`ExecSpec`] first frame
/// after the exec link is up.
const SPEC_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Everything the engine needs from its host process: how to resolve a
/// project slug to a cwd on THIS machine, and this machine's daemon URL for
/// the `{{DAEMON_URL}}` file-content substitution.
pub struct SatelliteExecCtx<'a> {
    pub daemon_url: &'a str,
    pub resolve_project_dir: &'a (dyn Fn(&str) -> Option<PathBuf> + Send + Sync),
}

/// Resolve `vendor` against [`ALLOWED_VENDORS`]. `None` ⇒ not allowed.
fn resolve_vendor_bin(vendor: &str) -> Option<String> {
    ALLOWED_VENDORS
        .iter()
        .find(|(v, _, _)| *v == vendor)
        .map(|(_, env, default)| std::env::var(env).unwrap_or_else(|_| (*default).to_string()))
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

async fn send_started<K>(sink: &mut K, ok: bool, pid: Option<u32>, code: &str, message: &str)
where
    K: Sink<Message, Error = WsError> + Unpin,
{
    let body = ExecStarted {
        ok,
        pid,
        code: (!code.is_empty()).then(|| code.to_string()),
        message: (!message.is_empty()).then(|| message.to_string()),
    };
    if let Ok(json) = serde_json::to_string(&body) {
        let _ = sink.send(Message::Text(json)).await;
    }
}

/// The whole exec session lifecycle over one established link: read
/// [`ExecSpec`] → validate (vendor/slug/files) → spawn → bidirectional
/// byte bridge → [`ExecExit`] tail. Never panics on a malformed/hostile
/// peer — every failure path sends a readable [`ExecStarted`] rejection
/// (or silently drops a link that never sent a valid spec at all).
pub async fn run_exec_session<S, K>(mut stream: S, mut sink: K, ctx: &SatelliteExecCtx<'_>)
where
    S: Stream<Item = Result<Message, WsError>> + Unpin,
    K: Sink<Message, Error = WsError> + Unpin,
{
    let first = tokio::time::timeout(SPEC_TIMEOUT, async {
        loop {
            match stream.next().await {
                Some(Ok(Message::Text(t))) => return Some(t),
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
                _ => return None,
            }
        }
    })
    .await
    .ok()
    .flatten();
    let spec: ExecSpec = match first {
        Some(t) => match serde_json::from_str(&t) {
            Ok(s) => s,
            Err(e) => {
                send_started(
                    &mut sink,
                    false,
                    None,
                    "bad-spec",
                    &format!("invalid ExecSpec: {e}"),
                )
                .await;
                return;
            }
        },
        // Peer vanished / sent garbage / never sent a spec — nothing to
        // reject with.
        None => return,
    };

    let Some(bin) = resolve_vendor_bin(&spec.vendor) else {
        send_started(
            &mut sink,
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

    let Some(cwd) = (ctx.resolve_project_dir)(&spec.slug) else {
        send_started(
            &mut sink,
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

    if let Err(msg) = write_confined_files(&cwd, &spec.sid, &spec.files, ctx.daemon_url) {
        send_started(&mut sink, false, None, "bad-spec", &msg).await;
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
    // this machine's own environment (never a raw passthrough of the wire
    // env, and never clobbering PATH/HOME/etc).
    for (k, v) in &spec.env {
        if k.starts_with("CCTEAM_") {
            cmd.env(k, v);
        }
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            send_started(
                &mut sink,
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
    send_started(&mut sink, true, pid, "", "").await;

    let Some(mut stdout) = child.stdout.take() else {
        return;
    };
    // `Option` so `stdin_close` can DROP the handle: tokio's
    // `ChildStdin::shutdown()` only flushes (the fd closes on drop), so a
    // drop is the only real half-close — without it the child never sees
    // stdin EOF and every teardown degenerates into the kill path.
    let mut stdin = child.stdin.take();
    if stdin.is_none() {
        return;
    }
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
    let mut last_rx = tokio::time::Instant::now();
    let mut idle_check = tokio::time::interval(IDLE_TIMEOUT / 3);
    idle_check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let exit_status = loop {
        tokio::select! {
            n = stdout.read(&mut buf) => {
                match n {
                    Ok(0) => break child.wait().await.ok(),
                    Ok(n) => {
                        if sink.send(Message::Binary(buf[..n].to_vec())).await.is_err() {
                            break None;
                        }
                    }
                    Err(_) => break None,
                }
            }
            msg = stream.next() => {
                match msg {
                    Some(Ok(Message::Binary(b))) => {
                        last_rx = tokio::time::Instant::now();
                        match stdin.as_mut() {
                            Some(s) => {
                                if s.write_all(&b).await.is_err() {
                                    break None;
                                }
                            }
                            None => break None, // payload after half-close: protocol error
                        }
                    }
                    Some(Ok(Message::Text(t))) if t.contains("stdin_close") => {
                        last_rx = tokio::time::Instant::now();
                        if let Some(mut s) = stdin.take() {
                            let _ = s.flush().await;
                            // drop(s) closes the fd → child sees stdin EOF.
                        }
                    }
                    Some(Ok(Message::Text(_) | Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => {
                        last_rx = tokio::time::Instant::now();
                    }
                    Some(Ok(Message::Close(_))) | None => break None,
                    Some(Err(_)) => break None,
                }
            }
            _ = idle_check.tick() => {
                // Half-open link detection: the daemon pings every
                // KEEPALIVE_PERIOD, so a healthy link always shows inbound
                // frames. A silent link this long is dead — kill the child
                // (the daemon-side EOF → re-gate + `--resume` recovers).
                if last_rx.elapsed() > IDLE_TIMEOUT {
                    tracing::warn!(
                        sid = %spec.sid,
                        "ccteam-exec.v1: exec link idle past {}s — treating as half-open, ending session",
                        IDLE_TIMEOUT.as_secs()
                    );
                    break None;
                }
            }
            status = child.wait() => {
                break status.ok();
            }
        }
    };

    // Link dropped mid-flight or an IO error broke the loop before the
    // child exited on its own — kill it (red line honesty: grandchildren
    // of `bin` are NOT killed, a documented simplification; vendor
    // `--resume` makes the next connect's context whole again).
    let exit_status = match exit_status {
        Some(s) => Some(s),
        None => {
            let _ = child.start_kill();
            child.wait().await.ok()
        }
    };
    let ev = build_exec_exit(exit_status);
    if let Ok(json) = serde_json::to_string(&ev) {
        let _ = sink.send(Message::Text(json)).await;
    }
    let _ = sink.close().await;
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

    // NOTE: the full engine round trip (fake vendor via `CCTEAM_CLAUDE_BIN`)
    // is an INTEGRATION test (`tests/satellite_exec_e2e.rs`) — it mutates
    // process env, which lib tests must never do (CLAUDE.md §六).

    #[tokio::test]
    async fn engine_rejects_unknown_slug_readable() {
        use crate::execution::host_channel::ExecBridge;
        let resolver = |_slug: &str| -> Option<PathBuf> { None };
        let (daemon_half, sat_half) = ExecBridge::pair();
        let engine = tokio::spawn(async move {
            let (stream, sink) = sat_half.into_io();
            let ctx = SatelliteExecCtx {
                daemon_url: "http://x",
                resolve_project_dir: &resolver,
            };
            run_exec_session(stream, sink, &ctx).await;
        });
        let spec = ExecSpec::new("claude", "nope", "s7", "stream-json");
        daemon_half
            .tx
            .send(Message::Text(serde_json::to_string(&spec).unwrap()))
            .await
            .unwrap();
        let mut rx = daemon_half.rx;
        let started: ExecStarted = match rx.recv().await {
            Some(Message::Text(t)) => serde_json::from_str(&t).unwrap(),
            other => panic!("expected ExecStarted, got {other:?}"),
        };
        assert!(!started.ok);
        assert_eq!(started.code.as_deref(), Some("unknown-slug"));
        engine.await.unwrap();
    }
}
