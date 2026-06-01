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
    Router,
};
use ccteam_core::TeamKind;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::broadcast::error::RecvError;

use crate::chat_protocol::{
    now_unix_seconds, timestamp_id, ClientChatFrame, ServerChatFrame, SessionItem,
    WebChannelMessage, WebSendMessage, SUBPROTOCOL,
};
use crate::state::AppState;

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
    Query(query): Query<ChatQuery>,
) -> Response {
    let chat_id = query.chat_id.unwrap_or_else(|| "web-chat".to_string());
    let user_id = query.user_id.unwrap_or_else(|| "web-user".to_string());
    ws.protocols([SUBPROTOCOL])
        .on_upgrade(move |socket| run(socket, app, chat_id, user_id))
}

async fn run(socket: WebSocket, app: AppState, chat_id: String, user_id: String) {
    if let Err(err) = relay(socket, app, chat_id.clone(), user_id).await {
        tracing::warn!(chat_id = %chat_id, error = %err, "chat_ws: relay loop exited");
    }
}

async fn relay(
    socket: WebSocket,
    app: AppState,
    chat_id: String,
    user_id: String,
) -> anyhow::Result<()> {
    let (mut tx, mut rx) = socket.split();
    let sessions = ServerChatFrame::Sessions {
        items: session_items(&app),
    };
    send_frame(&mut tx, &sessions).await?;
    for message in take_backlog_for_target(&app, &chat_id).await {
        for frame in send_message_to_frames(message) {
            send_frame(&mut tx, &frame).await?;
        }
    }

    let inbound = app.chat_inbound.clone();
    let mut outbound = app.chat_outbound.subscribe();

    loop {
        tokio::select! {
            frame = rx.next() => match frame {
                Some(Ok(Message::Text(text))) => {
                    let parsed = serde_json::from_str::<ClientChatFrame>(&text)?;
                    let is_switch = matches!(parsed, ClientChatFrame::Switch { .. });
                    let messages = frame_to_messages(parsed, &chat_id, &user_id);
                    if let Some(inbound) = &inbound {
                        for message in messages {
                            if inbound.send(message).await.is_err() {
                                break;
                            }
                        }
                    }
                    if is_switch {
                        let sessions = ServerChatFrame::Sessions {
                            items: session_items(&app),
                        };
                        send_frame(&mut tx, &sessions).await?;
                    }
                }
                Some(Ok(Message::Binary(data))) => {
                    let text = String::from_utf8(data.to_vec())?;
                    let parsed = serde_json::from_str::<ClientChatFrame>(&text)?;
                    let is_switch = matches!(parsed, ClientChatFrame::Switch { .. });
                    let messages = frame_to_messages(parsed, &chat_id, &user_id);
                    if let Some(inbound) = &inbound {
                        for message in messages {
                            if inbound.send(message).await.is_err() {
                                break;
                            }
                        }
                    }
                    if is_switch {
                        let sessions = ServerChatFrame::Sessions {
                            items: session_items(&app),
                        };
                        send_frame(&mut tx, &sessions).await?;
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
                Ok(message) => {
                    if message.recipient == chat_id
                        && remove_backlog_message(&app, &message).await
                    {
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
                });
            }
            return messages;
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

async fn remove_backlog_message(app: &AppState, message: &WebSendMessage) -> bool {
    let mut guard = app.chat_backlog.lock().await;
    let Some(idx) = guard.iter().position(|entry| entry == message) else {
        return false;
    };
    guard.remove(idx);
    true
}

fn send_message_to_frames(message: WebSendMessage) -> Vec<ServerChatFrame> {
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
        let (Some(session), Some(project), Some(vendor), Some(_role)) =
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
            current: false,
        });
    }
    Some(items)
}

fn session_items(app: &AppState) -> Vec<SessionItem> {
    let Ok(projects) = ccteam_core::collect_projects(&app.paths) else {
        return Vec::new();
    };
    let mut items = Vec::new();
    for project in projects {
        let state = project.state;
        if state.team_kind == TeamKind::Flex {
            for (sid, record) in state.sessions {
                items.push(SessionItem {
                    project: state.slug.clone(),
                    session: Some(sid),
                    vendor: Some(format!("{:?}", record.harness).to_lowercase()),
                    current: false,
                });
            }
        } else {
            items.push(SessionItem {
                project: state.slug,
                session: None,
                vendor: None,
                current: false,
            });
        }
    }
    items
}
