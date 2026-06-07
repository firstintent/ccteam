//! WebSocket PTY relay endpoints (backend-neutral).
//!
//! Two routes:
//!
//! - `GET /ws/{slug}/pty`           — project pane (tmux/rmux session name from
//!   `ProjectState::tmux_session`).
//! - `GET /ws/{slug}/{sid}/pty`     — **per-session** route. v0.8.8 B5: there is
//!   no project-level pane anymore (F1) — each session's pane is per-sid. The
//!   handler resolves `sid → {vendor, project, …}` via the live gateway
//!   ([`Gateway::session_resolve`](ccteam_im::gateway::Gateway::session_resolve))
//!   and targets the per-session pane: claude = `ccteam-chat-{slug}-{sid}`,
//!   codex = `ccteam-{slug}-{sid}` (see [`super::session_pane`]). No live
//!   gateway → 503 (standalone internal-web has no session map); a sid the
//!   gateway does not track → 404.
//!
//! ## Wire protocol (subprotocol `ccteam-pty.v1`)
//!
//! - **Server → client, binary frame**: pane output from the configured mux
//!   backend's `subscribe` stream (see the fidelity caveat below).
//! - **Server → client, text frame** (control): `{"type":"lag","behind":N}` —
//!   emitted once on broadcast lag; the stream continues from the latest
//!   available offset. The socket is NOT closed on lag.
//! - **Client → server, binary frame**: bytes forwarded to the backend's
//!   `send_text` (literal keystrokes; the browser xterm.js layer already sends
//!   control keys as their underlying bytes).
//! - **Client → server, text frame** (control):
//!   `{"type":"resize","cols":C,"rows":R}` — invokes the backend's `resize`.
//!
//! ## Fidelity caveat (v0.8.8 B5)
//!
//! Raw-byte faithfulness holds ONLY under the `tmux` backend
//! (`CCTEAM_MUX_BACKEND=tmux`, which streams real `pipe-pane` bytes). The
//! **default** backend is `rmux`, whose `subscribe` re-emits re-assembled lines
//! (strips `\r`, re-appends `\n`) — adequate for display + pattern-matching but
//! NOT byte-exact ANSI replay. So "looks exactly like a local terminal / full
//! raw ANSI" is a `tmux`-backend-only property; under default rmux it degrades
//! to line-text. TODO: a byte-faithful rmux capture shim (the ANSI gap noted in
//! `ccteam_harness::rmux_backend`'s `subscribe`).
//!
//! ## Auth
//!
//! The `auth_layer` middleware (mounted on the stateful router in
//! `lib::router_with_state`) runs **before** the `WebSocketUpgrade`
//! extractor. A missing / invalid token returns 401 with the same
//! `auth required` body as the rest of the app; the upgrade is
//! refused, not accepted-and-closed.
//!
//! ## Red lines (CLAUDE.md §三)
//!
//! - We never parse pane output for semantics. This is a pane byte/line relay,
//!   not a `capture-pane` scrape; orchestrator state comes from
//!   `progress.jsonl` alone.
//! - We never kill a session. The only teardown is stopping the pane relay
//!   (backend-side, e.g. `pipe-pane` stop under tmux) once the last subscriber
//!   drops.

use std::sync::Arc;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, State,
    },
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use ccteam_core::ProjectState;
// B5 — `default_backend()` returns `Arc<dyn PaneBackend>`; `send_text` /
// `resize` are callable on the trait object directly (vtable methods), so
// the `PaneBackend` / `ProcessBackend` traits need not be in scope. The
// previously-hardcoded `TmuxBackend` is no longer referenced.
use ccteam_harness::{default_backend, MuxSessionId};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::broadcast::error::RecvError;

use super::session_pane::{resolve_session_pane, PaneResolveError};
use crate::pty::Subscription;
use crate::state::AppState;

/// Subprotocol the browser asks for in `Sec-WebSocket-Protocol`.
pub const SUBPROTOCOL: &str = "ccteam-pty.v1";

/// Control message shape on the client → server text channel.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum ClientControl {
    Resize { cols: u16, rows: u16 },
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/ws/{slug}/pty", get(handle_project_ws))
        .route("/ws/{slug}/{sid}/pty", get(handle_session_ws))
}

async fn handle_project_ws(
    ws: WebSocketUpgrade,
    State(app): State<AppState>,
    Path(slug): Path<String>,
) -> Response {
    let state = match ProjectState::load(&app.paths.project_state(&slug)) {
        Ok(s) => s,
        Err(_) => return (StatusCode::NOT_FOUND, "project not found").into_response(),
    };
    let tmux_session = state.tmux_session.clone();
    if tmux_session.trim().is_empty() {
        return (StatusCode::NOT_FOUND, "project has no tmux session").into_response();
    }
    let key = slug.clone();
    upgrade(ws, app, key, tmux_session)
}

async fn handle_session_ws(
    ws: WebSocketUpgrade,
    State(app): State<AppState>,
    Path((slug, sid)): Path<(String, String)>,
) -> Response {
    // v0.8.8 B5 — F1 后【没有项目级 pane】:每会话 pane 是 per-sid。经 live
    // gateway 把 sid 解析成 {vendor, project, …},再按 vendor 构 per-session
    // pane 名(claude=ccteam-chat-{slug}-{sid} / codex=ccteam-{slug}-{sid})。
    // 无 gateway(standalone internal-web,无会话表)→ 503;sid 未知 → 404。
    // 不再退回 ProjectState::load + state.tmux_session(F1 后那个名字根本不
    // 对应任何活 pane,会 subscribe 空/秒断 → SPA 1s 重连循环)。
    let (tmux_session, _resolved) = match resolve_session_pane(&app, &sid).await {
        Ok(pair) => pair,
        Err(PaneResolveError::NoGateway) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "no live gateway: per-session terminal unavailable on standalone web",
            )
                .into_response()
        }
        Err(PaneResolveError::Unknown) => {
            return (StatusCode::NOT_FOUND, format!("unknown session: {sid}")).into_response()
        }
    };
    // FIFO/relay key 仍用 `{slug}/{sid}`(URL 寻址),pane 目标用解析出的名字。
    let key = format!("{slug}/{sid}");
    upgrade(ws, app, key, tmux_session)
}

/// Common upgrade path: echo the subprotocol if the client asked for
/// it, then hand off to the byte-relay loop.
fn upgrade(ws: WebSocketUpgrade, app: AppState, key: String, tmux_session: String) -> Response {
    ws.protocols([SUBPROTOCOL])
        .on_upgrade(move |socket| async move {
            if let Err(err) = run(socket, app, key.clone(), tmux_session.clone()).await {
                tracing::warn!(
                    key = %key,
                    tmux_session = %tmux_session,
                    error = %err,
                    "pty_ws: relay loop exited with error",
                );
            }
        })
}

async fn run(
    socket: WebSocket,
    app: AppState,
    key: String,
    tmux_session: String,
) -> anyhow::Result<()> {
    let subscription = app.pty.subscribe(&key, &tmux_session, &app.paths).await?;
    let tmux_session: Arc<str> = Arc::from(subscription.tmux_session().to_string());
    relay(socket, subscription, tmux_session).await;
    Ok(())
}

async fn relay(socket: WebSocket, mut sub: Subscription, tmux_session: Arc<str>) {
    let (mut tx, mut rx) = socket.split();

    loop {
        tokio::select! {
            // Server → client: forward pane bytes from the broadcast
            // channel. On lag, emit one text frame and keep going.
            event = sub.rx.recv() => match event {
                Ok(bytes) => {
                    if tx.send(Message::Binary(bytes.into())).await.is_err() {
                        break;
                    }
                }
                Err(RecvError::Lagged(n)) => {
                    let msg = format!(r#"{{"type":"lag","behind":{n}}}"#);
                    if tx.send(Message::Text(msg.into())).await.is_err() {
                        break;
                    }
                    // Continue from current offset; tokio rebuilds it
                    // for us on the next `recv`.
                }
                Err(RecvError::Closed) => {
                    // Sender dropped — subscription teardown is in
                    // progress. Close politely.
                    let _ = tx.send(Message::Close(None)).await;
                    break;
                }
            },
            // Client → server: route binary as keystrokes, text as
            // JSON control. Anything else is ignored.
            inbound = rx.next() => match inbound {
                Some(Ok(Message::Binary(data))) => {
                    if let Err(err) = send_keys(&tmux_session, &data).await {
                        tracing::warn!(error = %err, "pty_ws: send-keys failed");
                    }
                }
                Some(Ok(Message::Text(text))) => {
                    match serde_json::from_str::<ClientControl>(&text) {
                        Ok(ClientControl::Resize { cols, rows }) if cols > 0 && rows > 0 => {
                            if let Err(err) = resize_window(&tmux_session, cols, rows).await {
                                tracing::warn!(error = %err, "pty_ws: resize failed");
                            }
                        }
                        Ok(_) => {
                            // resize w/ zero dims — ignore.
                        }
                        Err(err) => {
                            tracing::debug!(error = %err, text = %text, "pty_ws: unknown control");
                        }
                    }
                }
                Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => {
                    // axum auto-replies to Ping; nothing to do.
                }
                Some(Ok(Message::Close(_))) | None => break,
                Some(Err(err)) => {
                    tracing::debug!(error = %err, "pty_ws: socket recv error");
                    break;
                }
            },
        }
    }
    // Subscription drops here, decrementing refcount + tearing down
    // pipe-pane on zero.
    drop(sub);
}

async fn send_keys(tmux_session: &str, bytes: &[u8]) -> anyhow::Result<()> {
    // xterm.js sends UTF-8 in practice; non-UTF-8 input is rejected
    // because the underlying tmux argv path does not survive
    // OsStr-level byte injection cleanly across all setups.
    let s = std::str::from_utf8(bytes).map_err(|_| {
        anyhow::anyhow!("send-keys: non-UTF-8 input rejected (would corrupt tmux argv)")
    })?;
    // v0.8.8 B5 — route through the configured mux backend (rmux-aware;
    // honors `CCTEAM_MUX_BACKEND`). Previously hardcoded `TmuxBackend::new()`,
    // which sent keystrokes to a tmux server that does not exist under the
    // default rmux backend, so input never reached the pane.
    let backend = default_backend();
    let id = MuxSessionId::new(tmux_session.to_string());
    backend.send_text(&id, s).await?;
    Ok(())
}

async fn resize_window(tmux_session: &str, cols: u16, rows: u16) -> anyhow::Result<()> {
    // v0.8.8 B5 — route through the configured mux backend (rmux-aware),
    // matching the subscribe + send paths. Was hardcoded `TmuxBackend::new()`.
    let backend = default_backend();
    let id = MuxSessionId::new(tmux_session.to_string());
    backend.resize(&id, cols, rows).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_control_parses_resize() {
        let v: ClientControl =
            serde_json::from_str(r#"{"type":"resize","cols":120,"rows":40}"#).unwrap();
        match v {
            ClientControl::Resize { cols, rows } => {
                assert_eq!(cols, 120);
                assert_eq!(rows, 40);
            }
        }
    }

    #[test]
    fn client_control_rejects_unknown_type() {
        let r: Result<ClientControl, _> =
            serde_json::from_str(r#"{"type":"shutdown","cols":1,"rows":1}"#);
        assert!(r.is_err());
    }
}
