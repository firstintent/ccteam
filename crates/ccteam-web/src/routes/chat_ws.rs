//! Browser chat WebSocket endpoint.
//!
//! This route is the web transport edge only. It accepts
//! `ccteam-chat.v1` frames, forwards neutral web-local messages into
//! the CLI-owned bridge, and renders bridge outbound messages back as
//! chat frames.

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query, State,
    },
    response::Response,
    routing::get,
    Extension, Router,
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;

use crate::auth::Identity;
use crate::chat_protocol::{
    now_unix_seconds, timestamp_id, ClientChatFrame, ServerChatFrame, SessionItem,
    WebChannelMessage, WebSendMessage, SUBPROTOCOL,
};
use crate::state::{AppState, ChatConns};

#[derive(Debug, Deserialize)]
struct ChatQuery {
    chat_id: Option<String>,
    user_id: Option<String>,
}

pub fn router() -> Router<AppState> {
    Router::new().route("/ws/chat", get(handle_chat_ws))
}

async fn handle_chat_ws(
    ws: WebSocketUpgrade,
    State(app): State<AppState>,
    identity: Option<Extension<Identity>>,
    Query(query): Query<ChatQuery>,
) -> Response {
    let identity = identity.map_or_else(Identity::admin, |Extension(i)| i);
    let (chat_id, user_id) = resolve_chat_identity(&app, &identity, query);
    ws.protocols([SUBPROTOCOL])
        .on_upgrade(move |socket| run(socket, app, identity, chat_id, user_id))
}

/// Cross-user fix (2026-07-28) — the socket's `(chat_id, user_id)` is the caller's IDENTITY, not a
/// client-chosen label. It keys everything downstream: the gateway `ChatKey`
/// (→ session ownership + ACL) and the outbound filter that decides which
/// socket receives a message. Taking it from the query string let any
/// authenticated caller connect as `chat_id=web-api` and read/drive the
/// admin's chat — the web twin of "an IM transport stamps the inbound channel,
/// the sender never picks it".
///
/// With auth OFF (loopback / `--no-auth`) there is exactly one local operator
/// and no identity to cross, so the query values still apply — that is also the
/// seam the CLI web-chat bridge's tests drive.
fn resolve_chat_identity(
    app: &AppState,
    identity: &Identity,
    query: ChatQuery,
) -> (String, String) {
    if app.auth.enabled {
        return (identity.web_chat_id(), identity.id.clone());
    }
    (
        query.chat_id.unwrap_or_else(|| "web-chat".to_string()),
        query.user_id.unwrap_or_else(|| "web-user".to_string()),
    )
}

async fn run(
    socket: WebSocket,
    app: AppState,
    identity: Identity,
    chat_id: String,
    user_id: String,
) {
    if let Err(err) = relay(socket, app, identity, chat_id.clone(), user_id).await {
        tracing::warn!(chat_id = %chat_id, error = %err, "chat_ws: relay loop exited");
    }
}

async fn relay(
    socket: WebSocket,
    app: AppState,
    identity: Identity,
    chat_id: String,
    user_id: String,
) -> anyhow::Result<()> {
    let (mut tx, mut rx) = socket.split();

    // Order matters for the backlog/live handoff (P1-1):
    //   1. subscribe so we catch every live broadcast from here on,
    //   2. register this socket so the send path stops parking this
    //      recipient's messages in the backlog (a live socket exists),
    //   3. send the session list, then drain anything parked while the
    //      recipient had no socket. A message sent in the µs window
    //      between (1) and (2) may arrive both live and via the drain;
    //      that rare duplicate is preferred over the loss the old
    //      "backlog entry == delivery token" scheme caused with >1 tab.
    let mut outbound = app.chat_outbound.subscribe();
    let _conn = ConnGuard::enter(app.chat_conns.clone(), chat_id.clone()).await;
    let inbound = app.chat_inbound.clone();

    let sessions = ServerChatFrame::Sessions {
        items: session_items(&app, &identity),
    };
    send_frame(&mut tx, &sessions).await?;
    for message in take_backlog_for_target(&app, &chat_id).await {
        for frame in send_message_to_frames(message) {
            send_frame(&mut tx, &frame).await?;
        }
    }

    loop {
        tokio::select! {
            frame = rx.next() => match frame {
                Some(Ok(Message::Text(text))) => {
                    match serde_json::from_str::<ClientChatFrame>(&text) {
                        Ok(parsed) => {
                            forward_client_frame(parsed, &chat_id, &user_id, &inbound, &app, &identity, &mut tx)
                                .await?;
                        }
                        // P1-2: a single malformed frame must not tear
                        // down the whole chat socket. Log + keep going.
                        Err(err) => {
                            tracing::debug!(chat_id = %chat_id, error = %err, "chat_ws: ignoring malformed text frame");
                        }
                    }
                }
                Some(Ok(Message::Binary(data))) => {
                    match String::from_utf8(data.to_vec())
                        .ok()
                        .and_then(|text| serde_json::from_str::<ClientChatFrame>(&text).ok())
                    {
                        Some(parsed) => {
                            forward_client_frame(parsed, &chat_id, &user_id, &inbound, &app, &identity, &mut tx)
                                .await?;
                        }
                        None => {
                            tracing::debug!(chat_id = %chat_id, "chat_ws: ignoring malformed/non-utf8 binary frame");
                        }
                    }
                }
                Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => {}
                Some(Ok(Message::Close(_))) | None => break,
                Some(Err(err)) => {
                    tracing::debug!(error = %err, "chat_ws: socket recv error");
                    break;
                }
            },
            message = outbound.recv() => match message {
                // P1-1: deliver to every socket whose recipient matches —
                // multiple tabs on the same chat each get a copy. Delivery
                // is no longer gated on removing a backlog entry.
                Ok(message) => {
                    if message.recipient == chat_id {
                        for frame in send_message_to_frames(message) {
                            send_frame(&mut tx, &frame).await?;
                        }
                    }
                }
                Err(RecvError::Lagged(behind)) => {
                    send_frame(&mut tx, &ServerChatFrame::Lag { behind }).await?;
                }
                Err(RecvError::Closed) => break,
            },
        }
    }

    Ok(())
}

/// Translate a parsed client frame into bridge inbound messages and, for
/// focus switches, re-emit the (now-current) session list. Shared by the
/// text and binary receive arms so P1-2's resilience lives in one place.
async fn forward_client_frame<S>(
    parsed: ClientChatFrame,
    chat_id: &str,
    user_id: &str,
    inbound: &Option<mpsc::Sender<WebChannelMessage>>,
    app: &AppState,
    identity: &Identity,
    tx: &mut S,
) -> anyhow::Result<()>
where
    S: SinkExt<Message> + Unpin,
    <S as futures_util::Sink<Message>>::Error: std::error::Error + Send + Sync + 'static,
{
    let is_switch = matches!(parsed, ClientChatFrame::Switch { .. });
    let messages = frame_to_messages(parsed, chat_id, user_id);
    if let Some(inbound) = inbound {
        for message in messages {
            if inbound.send(message).await.is_err() {
                break;
            }
        }
    }
    if is_switch {
        let sessions = ServerChatFrame::Sessions {
            items: session_items(app, identity),
        };
        send_frame(tx, &sessions).await?;
    }
    Ok(())
}

/// RAII counter for live web-chat sockets per `chat_id`. The send path
/// (`web_chat_bridge`) reads this to decide live-broadcast vs backlog.
struct ConnGuard {
    conns: ChatConns,
    chat_id: String,
}

impl ConnGuard {
    async fn enter(conns: ChatConns, chat_id: String) -> Self {
        *conns.lock().await.entry(chat_id.clone()).or_insert(0) += 1;
        Self { conns, chat_id }
    }
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        // tokio's Mutex can't be locked synchronously here; hand the
        // decrement to the runtime. Keyed by chat_id, so a fast
        // reconnect (new increment before this runs) still nets correct.
        let conns = self.conns.clone();
        let chat_id = std::mem::take(&mut self.chat_id);
        tokio::spawn(async move {
            let mut guard = conns.lock().await;
            if let Some(count) = guard.get_mut(&chat_id) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    guard.remove(&chat_id);
                }
            }
        });
    }
}

async fn send_frame<S>(tx: &mut S, frame: &ServerChatFrame) -> anyhow::Result<()>
where
    S: SinkExt<Message> + Unpin,
    <S as futures_util::Sink<Message>>::Error: std::error::Error + Send + Sync + 'static,
{
    let payload = serde_json::to_string(frame)?;
    tx.send(Message::Text(payload.into())).await?;
    Ok(())
}

fn frame_to_messages(
    frame: ClientChatFrame,
    chat_id: &str,
    user_id: &str,
) -> Vec<WebChannelMessage> {
    let now = chrono::Utc::now();
    let content = match frame {
        ClientChatFrame::Text { content, id } => {
            return vec![WebChannelMessage {
                id: id.unwrap_or_else(|| timestamp_id("web-in", now, &content)),
                sender: user_id.to_string(),
                reply_target: chat_id.to_string(),
                content,
                channel: "web".to_string(),
                timestamp: now_unix_seconds(),
                thread_ts: None,
                selection: None,
            }];
        }
        ClientChatFrame::Switch { project, session } => {
            let mut messages = Vec::new();
            if let Some(project) = project {
                messages.push(WebChannelMessage {
                    id: timestamp_id("web-in", now, &project),
                    sender: user_id.to_string(),
                    reply_target: chat_id.to_string(),
                    content: format!("/cd {project}"),
                    channel: "web".to_string(),
                    timestamp: now_unix_seconds(),
                    thread_ts: None,
                    selection: None,
                });
            }
            if let Some(session) = session {
                messages.push(WebChannelMessage {
                    id: timestamp_id("web-in", now, &session),
                    sender: user_id.to_string(),
                    reply_target: chat_id.to_string(),
                    content: format!("/use {session}"),
                    channel: "web".to_string(),
                    timestamp: now_unix_seconds(),
                    thread_ts: None,
                    selection: None,
                });
            }
            return messages;
        }
        ClientChatFrame::Choice { data } => {
            // A chip click is a selection, not text (v0.8.5 D3).
            return vec![WebChannelMessage {
                id: timestamp_id("web-in", now, &data),
                sender: user_id.to_string(),
                reply_target: chat_id.to_string(),
                content: String::new(),
                channel: "web".to_string(),
                timestamp: now_unix_seconds(),
                thread_ts: None,
                selection: Some(data),
            }];
        }
        ClientChatFrame::Attach { name, data } => format!("/attach {name}\n{data}"),
    };
    vec![WebChannelMessage {
        id: timestamp_id("web-in", now, &content),
        sender: user_id.to_string(),
        reply_target: chat_id.to_string(),
        content,
        channel: "web".to_string(),
        timestamp: now_unix_seconds(),
        thread_ts: None,
        selection: None,
    }]
}

async fn take_backlog_for_target(app: &AppState, target: &str) -> Vec<WebSendMessage> {
    let mut guard = app.chat_backlog.lock().await;
    let mut matched = Vec::new();
    let mut idx = 0;
    while idx < guard.len() {
        if guard[idx].recipient == target {
            matched.push(guard.remove(idx));
        } else {
            idx += 1;
        }
    }
    matched
}

fn send_message_to_frames(message: WebSendMessage) -> Vec<ServerChatFrame> {
    // v0.8.5 D3: an options-bearing message is a choice prompt → chips. The
    // token is shared across options (embedded in each `"{token}:{idx}"`);
    // derive it from the first option.
    if !message.options.is_empty() {
        let token = message
            .options
            .first()
            .and_then(|o| o.data.split_once(':').map(|(t, _)| t.to_string()))
            .unwrap_or_default();
        return vec![ServerChatFrame::Choice {
            token,
            title: message.content,
            options: message.options,
        }];
    }
    let mut frames = vec![ServerChatFrame::Reply {
        content: message.content.clone(),
    }];
    if let Some(items) = parse_sessions_reply(&message.content) {
        frames.push(ServerChatFrame::Sessions { items });
    }
    frames
}

fn parse_sessions_reply(content: &str) -> Option<Vec<SessionItem>> {
    if content == "no sessions" {
        return Some(Vec::new());
    }
    let mut items = Vec::new();
    for line in content.lines() {
        let mut parts = line.splitn(4, ':');
        let (Some(session), Some(project), Some(vendor), Some(role)) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return None;
        };
        if !session.starts_with('s') {
            return None;
        }
        items.push(SessionItem {
            project: project.to_string(),
            session: Some(session.to_string()),
            vendor: Some(vendor.to_ascii_lowercase()),
            role: Some(role.to_string()),
            current: false,
        });
    }
    Some(items)
}

/// The project picker this socket opens with. Cross-user fix (2026-07-28) — filtered through the
/// SAME ownership policy as `GET /api/v1/projects`; it used to hand every
/// connected chat the full project list of the box, leaking other users'
/// project names.
fn session_items(app: &AppState, identity: &Identity) -> Vec<SessionItem> {
    let Ok(projects) = ccteam_core::collect_projects(&app.paths) else {
        return Vec::new();
    };
    let mut items = Vec::new();
    for project in projects {
        let state = project.state;
        if !identity.can_see_owner(state.owner.as_deref()) {
            continue;
        }
        items.push(SessionItem {
            project: state.slug,
            session: None,
            vendor: None,
            role: None,
            current: false,
        });
    }
    items
}
