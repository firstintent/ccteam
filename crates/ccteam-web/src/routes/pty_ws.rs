//! V0.3.2 F56 — WebSocket PTY relay endpoints.
//!
//! Two routes:
//!
//! - `GET /ws/{slug}/pty`           — workflow / default project (tmux
//!   session from `ProjectState::tmux_session`).
//! - `GET /ws/{slug}/{sid}/pty`     — flex per-session (tmux session
//!   from `ProjectState::sessions[sid].tmux_session`).
//!
//! ## Wire protocol (subprotocol `ccteam-pty.v1`)
//!
//! - **Server → client, binary frame**: raw bytes captured from the
//!   tmux pane via `tmux pipe-pane` (one or more pane updates, no
//!   chunk boundary semantics).
//! - **Server → client, text frame** (control): `{"type":"lag",
//!   "behind":N}` — emitted once on broadcast lag; the stream continues
//!   from the latest available offset. The socket is NOT closed on
//!   lag.
//! - **Client → server, binary frame**: raw bytes piped to `tmux
//!   send-keys -l -- <bytes>`. `-l` (literal) keeps tmux from
//!   interpreting tokens like `Enter` or `C-c` as named keys — the
//!   browser xterm.js layer sends those as the underlying control
//!   bytes already.
//! - **Client → server, text frame** (control):
//!   `{"type":"resize","cols":C,"rows":R}` — invokes `tmux
//!   resize-window -t <session> -x C -y R`.
//!
//! ## Auth
//!
//! The `auth_layer` middleware (mounted on the stateful router in
//! `lib::router_with_state`) runs **before** the `WebSocketUpgrade`
//! extractor. A missing / invalid token returns 401 with the same
//! `auth required` body as the rest of the app; the upgrade is
//! refused, not accepted-and-closed.
//!
//! ## Red lines (CLAUDE.md §三 + PRD §6 §F56)
//!
//! - We never parse pane output for semantics. F56 is a raw byte
//!   relay; orchestrator state comes from `progress.jsonl` alone.
//! - We never invoke `tmux kill-session`. The only teardown F56 does
//!   is `tmux pipe-pane -t <session>:0.0` (stop, no command) once the
//!   last subscriber drops.

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
use ccteam_core::{ProjectState, TeamKind};
use ccteam_mux::{MuxBackend, MuxSessionId, TmuxBackend};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::broadcast::error::RecvError;

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
    let state = match ProjectState::load(&app.paths.project_state(&slug)) {
        Ok(s) => s,
        Err(_) => return (StatusCode::NOT_FOUND, "project not found").into_response(),
    };
    if state.team_kind != TeamKind::Flex {
        return (
            StatusCode::BAD_REQUEST,
            format!("project {slug} is not a flex project"),
        )
            .into_response();
    }
    let Some(record) = state.sessions.get(&sid) else {
        return (
            StatusCode::NOT_FOUND,
            format!("session not found: {slug}/{sid}"),
        )
            .into_response();
    };
    let tmux_session = record.tmux_session.clone();
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
    // V0.8 W1 — route through the MuxBackend trait. Note `TmuxBackend`
    // currently targets bare session-name (`-t <name>`) rather than the
    // legacy `<name>:0.0` form. The change is benign for CCTEAM-managed
    // single-window-single-pane sessions and removes an audit §4-B
    // base-index landmine.
    let backend = TmuxBackend::new();
    let id = MuxSessionId::new(tmux_session.to_string());
    backend.send_text(&id, s).await?;
    Ok(())
}

async fn resize_window(tmux_session: &str, cols: u16, rows: u16) -> anyhow::Result<()> {
    // V0.8 W1 — route through the MuxBackend trait.
    let backend = TmuxBackend::new();
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
