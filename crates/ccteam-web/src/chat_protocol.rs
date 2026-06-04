//! Browser chat WebSocket protocol.
//!
//! The web crate deliberately keeps these as web-local wire structs so
//! `ccteam-web` does not depend on `ccteam-im`. The CLI gateway wiring
//! translates these neutral shapes to/from the IM transport types.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Subprotocol the browser asks for in `Sec-WebSocket-Protocol`.
pub const SUBPROTOCOL: &str = "ccteam-chat.v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientChatFrame {
    Text {
        content: String,
        id: Option<String>,
    },
    Switch {
        project: Option<String>,
        session: Option<String>,
    },
    Attach {
        name: String,
        data: String,
    },
    /// A chip click answering a `Choice` prompt (v0.8.5 D3). `data` is the
    /// opaque `"{token}:{idx}"` payload carried by the chip.
    Choice {
        data: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerChatFrame {
    TurnStarted {
        session: String,
        vendor: String,
    },
    AssistantDelta {
        text: String,
    },
    Tool {
        name: String,
        summary: String,
    },
    Reply {
        content: String,
    },
    TurnDone {
        session: String,
    },
    Sessions {
        items: Vec<SessionItem>,
    },
    Lag {
        behind: u64,
    },
    /// A choice prompt rendered as clickable chips (v0.8.5 D3). A chip click
    /// comes back as [`ClientChatFrame::Choice`].
    Choice {
        token: String,
        title: String,
        options: Vec<WebMessageOption>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionItem {
    pub project: String,
    pub session: Option<String>,
    pub vendor: Option<String>,
    /// Agent role (`reviewer`, `api`, …). Derived from the chat-session
    /// tmux name (`ccteam-chat-<slug>-<role>`) for the disk path, or the
    /// `/sessions` reply for the gateway path. `None` for workflow
    /// projects and sessions whose role can't be resolved.
    pub role: Option<String>,
    pub current: bool,
}

/// Web-local mirror of `ccteam_im::transport::MessageOption` (v0.8.5 D3).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebMessageOption {
    /// Opaque callback payload, `"{token}:{idx}"`.
    pub data: String,
    /// Button / chip label.
    pub label: String,
}

/// Web-local mirror of `ccteam_im::transport::ChannelMessage`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebChannelMessage {
    pub id: String,
    pub sender: String,
    pub reply_target: String,
    pub content: String,
    pub channel: String,
    pub timestamp: u64,
    pub thread_ts: Option<String>,
    /// Set when this inbound event is a chip click (v0.8.5 D3): the opaque
    /// `"{token}:{idx}"` payload. `None` for ordinary text.
    #[serde(default)]
    pub selection: Option<String>,
}

/// Web-local mirror of `ccteam_im::transport::SendMessage`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebSendMessage {
    pub content: String,
    pub recipient: String,
    pub subject: Option<String>,
    pub thread_ts: Option<String>,
    /// Selectable options rendered as chips (v0.8.5 D3). Empty ⇒ ordinary
    /// message.
    #[serde(default)]
    pub options: Vec<WebMessageOption>,
}

impl WebSendMessage {
    pub fn new(content: impl Into<String>, recipient: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            recipient: recipient.into(),
            subject: None,
            thread_ts: None,
            options: Vec::new(),
        }
    }
}

pub fn now_unix_seconds() -> u64 {
    Utc::now().timestamp().max(0) as u64
}

pub fn timestamp_id(prefix: &str, when: DateTime<Utc>, content: &str) -> String {
    format!(
        "{prefix}-{}-{}",
        when.timestamp_millis(),
        content_hash(content)
    )
}

fn content_hash(content: &str) -> u64 {
    content.bytes().fold(0_u64, |acc, byte| {
        acc.wrapping_mul(31).wrapping_add(byte as u64)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip_client(frame: ClientChatFrame) {
        let wire = serde_json::to_string(&frame).unwrap();
        let parsed: ClientChatFrame = serde_json::from_str(&wire).unwrap();
        assert_eq!(parsed, frame);
    }

    fn round_trip_server(frame: ServerChatFrame) {
        let wire = serde_json::to_string(&frame).unwrap();
        let parsed: ServerChatFrame = serde_json::from_str(&wire).unwrap();
        assert_eq!(parsed, frame);
    }

    #[test]
    fn chat_frame_client_variants_round_trip() {
        round_trip_client(ClientChatFrame::Text {
            content: "hi".into(),
            id: Some("m1".into()),
        });
        round_trip_client(ClientChatFrame::Switch {
            project: Some("dev-demo".into()),
            session: Some("s1".into()),
        });
        round_trip_client(ClientChatFrame::Attach {
            name: "note.txt".into(),
            data: "aGVsbG8=".into(),
        });
        round_trip_client(ClientChatFrame::Choice {
            data: "tok:1".into(),
        });
    }

    #[test]
    fn chat_frame_server_variants_round_trip() {
        round_trip_server(ServerChatFrame::TurnStarted {
            session: "s1".into(),
            vendor: "claude".into(),
        });
        round_trip_server(ServerChatFrame::AssistantDelta { text: "ok".into() });
        round_trip_server(ServerChatFrame::Tool {
            name: "Read".into(),
            summary: "opened file".into(),
        });
        round_trip_server(ServerChatFrame::Reply {
            content: "done".into(),
        });
        round_trip_server(ServerChatFrame::TurnDone {
            session: "s1".into(),
        });
        round_trip_server(ServerChatFrame::Sessions {
            items: vec![SessionItem {
                project: "dev-demo".into(),
                session: Some("s1".into()),
                vendor: Some("codex".into()),
                role: Some("reviewer".into()),
                current: true,
            }],
        });
        round_trip_server(ServerChatFrame::Lag { behind: 3 });
        round_trip_server(ServerChatFrame::Choice {
            token: "tok".into(),
            title: "Pick".into(),
            options: vec![WebMessageOption {
                data: "tok:0".into(),
                label: "A".into(),
            }],
        });
    }

    #[test]
    fn chat_frame_subprotocol_constant_is_stable() {
        assert_eq!(SUBPROTOCOL, "ccteam-chat.v1");
    }
}
