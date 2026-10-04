//! Slack channel — Socket Mode inbound + Web API outbound, one Slack thread
//! per agent session.
//!
//! **Why Slack exists here (#19).** On a single-stream IM (Telegram) several
//! agent sessions share one bot conversation and their replies interleave.
//! Slack gives every conversation native threads, so this channel advertises
//! [`Channel::session_threads`] = `true`: every inbound [`ChannelMessage`]
//! carries the Slack thread it belongs to, the gateway scopes session focus
//! per thread, and every outbound reply for a session is posted back into that
//! session's thread.
//!
//! **Inbound = Socket Mode** (no public endpoint, same posture as Lark's WSS
//! long-connection): `apps.connections.open` with the app-level `xapp-` token
//! returns a one-shot `wss://` URL; the frame loop ACKs every envelope
//! *immediately* (Slack's 3 s budget) and hands it to an ordered processor, so
//! slow work (posting a slash-command anchor, downloading a file) never delays
//! an ACK and never reorders messages. Three envelope kinds are consumed:
//!
//! - `events_api` → `message` events (channels, private channels, DMs, group
//!   DMs). `app_mention` is deliberately ignored: it duplicates `message`.
//! - `slash_commands` → `/ccteam <text>`: the provider first posts a top-level
//!   ANCHOR message echoing the command, and the command runs inside the
//!   anchor's thread — so `/ccteam new codex` opens a fresh thread (session).
//! - `interactive` / `block_actions` → option-button clicks
//!   ([`ChoiceReply`]), threaded on the clicked message's thread.
//!
//! **Inbound invariant** (load-bearing for the gateway's per-thread focus):
//! every emitted [`ChannelMessage`] has `thread_ts = Some(..)` — a top-level
//! message threads on its own `ts`, a reply on its parent `thread_ts`, a slash
//! command on its anchor, a click on the clicked message's thread.
//!
//! **Allowlist** (`allowed_user_ids`, Slack `U…` ids) is fail-closed like
//! Lark: EMPTY = deny all, `"*"` = anyone. A rejected sender always leaves a
//! [`RejectedSenderProbe`] (the web binding flow reads it) but only gets the
//! one-time binding notice when the bot was addressed directly — a DM, an
//! @-mention, a slash command or a button click — never for ambient chatter
//! in a shared channel.
//!
//! **Outbound** posts Markdown through Slack's `markdown` block (standard
//! Markdown, unlike the legacy `mrkdwn` text dialect), with options rendered as
//! `actions` buttons; a block rejection retries once as plain `text`.

use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio_tungstenite::tungstenite::Message as WsMsg;

use crate::onboarding::{client_for_api_base, SLACK_API_BASE};
use crate::transport::{
    inbound_staging_dir, sanitize_attachment_name, AttachmentKind, Channel, ChannelAttachment,
    ChannelMessage, ChoiceReply, CommandSpec, MessageOption, OptionWeight, OutboundFile,
    RejectedSenderNotifier, RejectedSenderProbe, SendMessage,
};

/// Per-message ceiling in **UTF-16 code units**. Slack caps the cumulative
/// text of `markdown` blocks in one payload at 12 000 characters; a UTF-16
/// count is never smaller than the character count, so 10 000 units keeps a
/// split part safely under that cap with headroom for re-opened code fences.
/// The only home of the Slack length constant — the daemon reads it through
/// [`Channel::max_message_len`].
const SLACK_MAX_MESSAGE_UTF16: usize = 10_000;

/// Inbound attachment ceiling in bytes — mirrors Lark's ceiling so every
/// provider stages the same worst case to the shared filesystem.
const SLACK_MAX_ATTACHMENT_BYTES: u64 = 30 * 1024 * 1024;

/// Reaction name of the 👀 "received, processing" ack.
const SLACK_ACK_REACTION: &str = "eyes";

/// More options than this render as a dropdown instead of buttons.
const SLACK_MAX_BUTTONS: usize = 6;

/// Slack's cap on the options of one `static_select`.
const SLACK_SELECT_MAX_OPTIONS: usize = 100;

/// Slack rejects a button whose `plain_text` label exceeds 75 characters.
const SLACK_BUTTON_LABEL_MAX_CHARS: usize = 75;

/// Re-delivery dedupe memory (event ids + `(channel, ts)` + trigger ids).
/// Slack retries an un-ACKed event a handful of times within minutes, so a
/// few thousand recent keys is ample and keeps memory flat.
const SLACK_DEDUP_CAPACITY: usize = 2048;

/// Ordered envelope queue between the frame reader (which only ACKs and
/// enqueues) and the processor (which may post, download and backpressure).
const SLACK_ENVELOPE_QUEUE: usize = 256;

const RECONNECT_BACKOFF_MIN: Duration = Duration::from_secs(1);
const RECONNECT_BACKOFF_MAX: Duration = Duration::from_secs(60);

/// The client pings this often; any inbound frame refreshes liveness. A
/// half-open socket that stays silent past [`WS_IDLE_TIMEOUT`] is reopened.
const WS_PING_INTERVAL: Duration = Duration::from_secs(30);
const WS_IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// Upper bound on one `Retry-After` wait; a longer ask fails the call
/// instead of stalling the delivery path for minutes.
const RATE_LIMIT_MAX_WAIT: Duration = Duration::from_secs(30);

/// Web API error codes after which reconnecting cannot help: the credentials
/// (or Socket Mode itself) must be fixed by the operator, and a credentials
/// change rebuilds the channel anyway.
const FATAL_API_ERRORS: &[&str] = &[
    "invalid_auth",
    "not_authed",
    "account_inactive",
    "token_revoked",
    "token_expired",
    "not_allowed_token_type",
    "link_disabled",
];

/// A Slack Web API call that returned `ok: false` (or a Socket Mode
/// `link_disabled`). Kept typed so callers can branch on the error code.
#[derive(Debug)]
struct SlackApiError {
    method: String,
    code: String,
}

impl std::fmt::Display for SlackApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "slack {}: {}", self.method, self.code)
    }
}

impl std::error::Error for SlackApiError {}

/// The Slack error code carried by `err`, if it is a [`SlackApiError`].
fn api_error_code(err: &anyhow::Error) -> Option<&str> {
    err.downcast_ref::<SlackApiError>().map(|e| e.code.as_str())
}

/// Whether Slack refused the message's blocks (`invalid_blocks`,
/// `invalid_blocks_format`, `msg_blocks_too_long`, …) — the one failure a
/// plain-`text` resend can fix.
fn is_blocks_rejection(err: &anyhow::Error) -> bool {
    api_error_code(err).is_some_and(|code| code.contains("blocks"))
}

/// The bot's own identity from `auth.test`: its user id, used to drop its
/// own messages and to strip `<@BOT>` mentions. (Its `bot_id` needs no
/// keeping — every message carrying any `bot_id` is dropped, ours included.)
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct BotIdentity {
    user_id: String,
}

/// One Socket Mode frame.
#[derive(Debug, PartialEq)]
enum Frame {
    Hello,
    Disconnect(String),
    Envelope(Envelope),
    Other,
}

/// An envelope that must be ACKed by echoing its `envelope_id`.
#[derive(Debug, Clone, PartialEq)]
struct Envelope {
    envelope_id: String,
    kind: String,
    payload: Value,
}

fn parse_frame(text: &str) -> Frame {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return Frame::Other;
    };
    let kind = v.get("type").and_then(Value::as_str).unwrap_or("");
    if let Some(id) = v
        .get("envelope_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        return Frame::Envelope(Envelope {
            envelope_id: id.to_string(),
            kind: kind.to_string(),
            payload: v.get("payload").cloned().unwrap_or(Value::Null),
        });
    }
    match kind {
        "hello" => Frame::Hello,
        "disconnect" => Frame::Disconnect(
            v.get("reason")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        ),
        _ => Frame::Other,
    }
}

/// How one socket session ended.
#[derive(Debug, PartialEq, Eq)]
enum SocketEnd {
    /// The gateway receiver is gone — stop listening.
    Closed,
    /// Slack asked us to move (`refresh_requested` / `warning`): reopen now.
    Refresh,
    /// The socket dropped. `healthy` = it got as far as `hello`, so the
    /// reconnect backoff restarts from its minimum.
    Dropped { healthy: bool },
}

/// A file attached to an inbound message, still to be downloaded.
#[derive(Debug, Clone, PartialEq)]
struct PendingFile {
    id: String,
    name: String,
    mimetype: Option<String>,
    size: Option<u64>,
    url: String,
}

/// One inbound Slack `message` event after decode, before the allowlist,
/// dedupe and file download (which need `&self`).
#[derive(Debug, Clone, PartialEq)]
struct DecodedMessage {
    user: String,
    channel: String,
    channel_type: String,
    ts: String,
    /// Resolved thread: the event's `thread_ts`, else its own `ts`.
    thread_ts: String,
    text: String,
    mentions_bot: bool,
    files: Vec<PendingFile>,
}

impl DecodedMessage {
    fn into_channel_message(self, channel_name: &str) -> ChannelMessage {
        ChannelMessage {
            timestamp: ts_secs(&self.ts),
            id: self.ts,
            sender: self.user,
            reply_target: self.channel,
            content: self.text,
            channel: channel_name.to_string(),
            thread_ts: Some(self.thread_ts),
            attachments: Vec::new(),
            selection: None,
        }
    }
}

fn str_at<'a>(v: &'a Value, pointer: &str) -> Option<&'a str> {
    v.pointer(pointer)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

/// Decode a Socket Mode `events_api` `message` event. Returns `None` for
/// anything the bot must not act on: other event types, edits/joins/other
/// subtypes, bot traffic (its own or another app's), empty bodies.
fn decode_message_event(event: &Value, bot: &BotIdentity) -> Option<DecodedMessage> {
    if event.get("type").and_then(Value::as_str) != Some("message") {
        return None;
    }
    let channel_type = event
        .get("channel_type")
        .and_then(Value::as_str)
        .unwrap_or("");
    if !matches!(channel_type, "" | "channel" | "group" | "im" | "mpim") {
        return None;
    }
    match event.get("subtype").and_then(Value::as_str) {
        None | Some("file_share") | Some("thread_broadcast") => {}
        Some(_) => return None,
    }
    if str_at(event, "/bot_id").is_some() {
        return None;
    }
    let user = str_at(event, "/user")?;
    if !bot.user_id.is_empty() && user == bot.user_id {
        return None;
    }
    let channel = str_at(event, "/channel")?;
    let ts = str_at(event, "/ts")?;
    let thread_ts = str_at(event, "/thread_ts").unwrap_or(ts);

    let raw = event.get("text").and_then(Value::as_str).unwrap_or("");
    let (stripped, mentions_bot) = strip_bot_mention(raw, &bot.user_id);
    // Strip the mention BEFORE unescaping, so a literal `&lt;@BOT&gt;` the
    // user typed stays text. `trim` (not just `trim_end`) is what lets
    // ` /status` — typed with a leading space to get past Slack's own slash
    // interception — reach the gateway as the `/status` command.
    let text = command_from_sigil(unescape_entities(&stripped).trim().to_string());

    let files: Vec<PendingFile> = event
        .get("files")
        .and_then(Value::as_array)
        .map(|files| files.iter().filter_map(decode_file).collect())
        .unwrap_or_default();
    if text.is_empty() && files.is_empty() {
        return None;
    }
    Some(DecodedMessage {
        user: user.to_string(),
        channel: channel.to_string(),
        channel_type: channel_type.to_string(),
        ts: ts.to_string(),
        thread_ts: thread_ts.to_string(),
        text,
        mentions_bot,
        files,
    })
}

/// A downloadable file from `event.files[]`; tombstoned / external files
/// without a private download URL are skipped.
fn decode_file(file: &Value) -> Option<PendingFile> {
    let url = str_at(file, "/url_private_download").or_else(|| str_at(file, "/url_private"))?;
    let id = str_at(file, "/id").unwrap_or("file");
    let name = str_at(file, "/name")
        .or_else(|| str_at(file, "/title"))
        .unwrap_or("file");
    Some(PendingFile {
        id: id.to_string(),
        name: name.to_string(),
        mimetype: str_at(file, "/mimetype").map(str::to_string),
        size: file.get("size").and_then(Value::as_u64),
        url: url.to_string(),
    })
}

/// Remove every `<@BOT>` / `<@BOT|label>` token. Returns the remaining text
/// and whether the bot was mentioned.
fn strip_bot_mention(text: &str, bot_user_id: &str) -> (String, bool) {
    if bot_user_id.is_empty() {
        return (text.to_string(), false);
    }
    let needle = format!("<@{bot_user_id}");
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let mut found = false;
    while let Some(pos) = rest.find(&needle) {
        let after = &rest[pos + needle.len()..];
        let end = match after.chars().next() {
            Some('>') => Some(1),
            Some('|') => after.find('>').map(|i| i + 1),
            _ => None,
        };
        match end {
            Some(end) => {
                out.push_str(&rest[..pos]);
                rest = &after[end..];
                found = true;
            }
            None => {
                out.push_str(&rest[..pos + needle.len()]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    (out, found)
}

/// Undo Slack's three control-character escapes (`&amp;` last, so an
/// escaped `&amp;lt;` decodes to the literal `&lt;` the user typed).
fn unescape_entities(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// Escape user text for Slack's `text` field.
fn escape_entities(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// A `slash_commands` envelope payload.
#[derive(Debug, Clone, PartialEq)]
struct SlashCommand {
    user_id: String,
    channel_id: String,
    command: String,
    text: String,
    response_url: String,
    trigger_id: String,
}

fn decode_slash(payload: &Value) -> Option<SlashCommand> {
    Some(SlashCommand {
        user_id: str_at(payload, "/user_id")?.to_string(),
        channel_id: str_at(payload, "/channel_id")?.to_string(),
        command: str_at(payload, "/command")?.to_string(),
        text: payload
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        response_url: str_at(payload, "/response_url").unwrap_or("").to_string(),
        trigger_id: str_at(payload, "/trigger_id").unwrap_or("").to_string(),
    })
}

/// The gateway command a `/ccteam <text>` invocation stands for:
/// `new codex` → `/new codex`, empty → `/help`.
fn slash_content(text: &str) -> String {
    let text = unescape_entities(text).trim().to_string();
    if text.is_empty() {
        "/help".to_string()
    } else if text.starts_with('/') {
        text
    } else {
        format!("/{text}")
    }
}

/// The character a Slack user starts a ccteam command with. Slack keeps `/`
/// for itself — it swallows any message that starts with one, and an app's
/// own slash command cannot run inside a thread at all — so `!status`,
/// `!model`, `!compact` stand in for `/status` & co. everywhere on Slack, in a
/// session's thread included.
const COMMAND_SIGIL: char = '!';

/// Agent (vendor) commands ccteam's replies name by example; rewritten to
/// `!name` alongside ccteam's own registered commands.
const AGENT_COMMANDS_NAMED: &[&str] = &["model", "compact", "clear", "effort", "goal"];

/// `!word …` → `/word …` (a letter must follow, so `!!!` or `! wow` stay
/// prose); anything else unchanged.
fn command_from_sigil(text: String) -> String {
    let mut chars = text.chars();
    if chars.next() == Some(COMMAND_SIGIL) && chars.next().is_some_and(|c| c.is_ascii_alphabetic())
    {
        format!("/{}", &text[COMMAND_SIGIL.len_utf8()..])
    } else {
        text
    }
}

/// Rewrite references to ccteam's own commands (`names`, without the `/`)
/// from `/name` to `!name`, so a hint like `→ /status` names what a Slack user
/// can actually type. Only a `/` that starts a token is touched, and only when
/// the whole word is one of `names` (`/home/stop`, `/status.json`, `/stopped`
/// stay as they are); fenced code blocks are left verbatim.
fn rewrite_command_sigils(text: &str, names: &[String]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_fence = false;
    for line in text.split_inclusive('\n') {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            out.push_str(line);
        } else if in_fence {
            out.push_str(line);
        } else {
            rewrite_line_sigils(line, names, &mut out);
        }
    }
    out
}

fn rewrite_line_sigils(line: &str, names: &[String], out: &mut String) {
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let starts_token = i == 0
            || chars[i - 1].is_whitespace()
            || matches!(
                chars[i - 1],
                '(' | '[' | '`' | '"' | '\'' | '「' | '“' | '：' | ':'
            );
        if c == '/' && starts_token {
            let mut j = i + 1;
            while j < chars.len() && (chars[j].is_ascii_alphanumeric() || chars[j] == '_') {
                j += 1;
            }
            let word: String = chars[i + 1..j].iter().collect();
            let whole_word = match chars.get(j) {
                None => true,
                Some('/') | Some('-') => false,
                Some('.') => !chars.get(j + 1).is_some_and(|n| n.is_alphanumeric()),
                Some(_) => true,
            };
            if whole_word && names.contains(&word) {
                out.push(COMMAND_SIGIL);
                out.push_str(&word);
                i = j;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
}

/// Text of the top-level anchor message a slash command threads under —
/// it echoes the command so the thread reads as "who asked for what".
fn anchor_text(command: &str, text: &str, user_id: &str) -> String {
    let text = unescape_entities(text).trim().to_string();
    let invocation = if text.is_empty() {
        command.to_string()
    } else {
        format!("{command} {text}")
    };
    format!("`{}` · <@{user_id}>", escape_entities(&invocation))
}

/// A `block_actions` click on one of our option buttons.
#[derive(Debug, Clone, PartialEq)]
struct ButtonClick {
    user_id: String,
    channel_id: String,
    message_ts: String,
    thread_ts: String,
    value: String,
    trigger_id: String,
}

fn decode_block_actions(payload: &Value) -> Option<ButtonClick> {
    if payload.get("type").and_then(Value::as_str) != Some("block_actions") {
        return None;
    }
    let user_id = str_at(payload, "/user/id")?;
    // A button carries `value`; a dropdown pick carries the chosen option's.
    let value = str_at(payload, "/actions/0/value")
        .or_else(|| str_at(payload, "/actions/0/selected_option/value"))?;
    let channel_id =
        str_at(payload, "/channel/id").or_else(|| str_at(payload, "/container/channel_id"))?;
    let message_ts =
        str_at(payload, "/container/message_ts").or_else(|| str_at(payload, "/message/ts"))?;
    let thread_ts = str_at(payload, "/container/thread_ts")
        .or_else(|| str_at(payload, "/message/thread_ts"))
        .unwrap_or(message_ts);
    Some(ButtonClick {
        user_id: user_id.to_string(),
        channel_id: channel_id.to_string(),
        message_ts: message_ts.to_string(),
        thread_ts: thread_ts.to_string(),
        value: value.to_string(),
        trigger_id: str_at(payload, "/trigger_id").unwrap_or("").to_string(),
    })
}

/// Blocks for one outbound message: a `markdown` block carrying `content`,
/// then the options — as one row of buttons when there are a few, as a dropdown when there are more than
/// [`SLACK_MAX_BUTTONS`] (a project list or a model × effort picker would
/// otherwise bury the thread under a wall of buttons). Either way the
/// option's opaque `data` comes back verbatim on click; `action_id`s are
/// unique within the message as Slack requires.
fn message_blocks(content: &str, options: &[MessageOption]) -> Vec<Value> {
    let mut blocks = Vec::new();
    if !content.is_empty() {
        blocks.push(json!({ "type": "markdown", "text": content }));
    }
    if options.len() > SLACK_MAX_BUTTONS {
        for (index, chunk) in options.chunks(SLACK_SELECT_MAX_OPTIONS).enumerate() {
            let choices: Vec<Value> = chunk
                .iter()
                .map(|option| {
                    let label: String = option
                        .label
                        .trim()
                        .chars()
                        .take(SLACK_BUTTON_LABEL_MAX_CHARS)
                        .collect();
                    json!({
                        "text": { "type": "plain_text", "text": label, "emoji": true },
                        "value": option.data,
                    })
                })
                .collect();
            blocks.push(json!({
                "type": "actions",
                "elements": [{
                    "type": "static_select",
                    "action_id": format!("ccteam_select_{index}"),
                    "placeholder": { "type": "plain_text", "text": "选择…", "emoji": true },
                    "options": choices,
                }],
            }));
        }
        return blocks;
    }
    if options.is_empty() {
        return blocks;
    }
    let buttons: Vec<Value> = options
        .iter()
        .enumerate()
        .map(|(index, option)| {
            let label: String = option
                .label
                .chars()
                .take(SLACK_BUTTON_LABEL_MAX_CHARS)
                .collect();
            let label = if label.trim().is_empty() {
                format!("{}", index + 1)
            } else {
                label
            };
            let mut button = json!({
                "type": "button",
                "text": { "type": "plain_text", "text": label, "emoji": true },
                "value": option.data,
                "action_id": format!("ccteam_opt_{index}"),
            });
            // Slack cannot size a button: a main action gets the highlighted
            // style, one that is easy to regret asks before it acts.
            match option.weight {
                OptionWeight::Primary => button["style"] = json!("primary"),
                OptionWeight::Minor => {
                    button["confirm"] = json!({
                        "title": { "type": "plain_text", "text": "确认" },
                        "text": { "type": "plain_text", "text": format!("{} — 确定吗?", label.trim()) },
                        "confirm": { "type": "plain_text", "text": "确定" },
                        "deny": { "type": "plain_text", "text": "取消" },
                    });
                }
                OptionWeight::Normal => {}
            }
            button
        })
        .collect();
    blocks.push(json!({ "type": "actions", "elements": buttons }));
    blocks
}

/// `chat.postMessage` body. `rich` = Markdown block + buttons; plain = the
/// `text` field only (the fallback when Slack rejects the blocks — the
/// gateway already puts a numbered option list in `content`).
fn post_body(
    channel: &str,
    thread_ts: Option<&str>,
    content: &str,
    options: &[MessageOption],
    rich: bool,
) -> Value {
    let mut body = json!({ "channel": channel, "text": content, "unfurl_links": false });
    if let Some(thread_ts) = thread_ts.filter(|t| !t.is_empty()) {
        body["thread_ts"] = json!(thread_ts);
    }
    if rich {
        let blocks = message_blocks(content, options);
        if !blocks.is_empty() {
            body["blocks"] = Value::Array(blocks);
        }
    }
    body
}

/// `chat.update` body. The plain variant clears the blocks explicitly —
/// omitting `blocks` would keep the message's previous ones.
fn update_body(channel: &str, ts: &str, content: &str, rich: bool) -> Value {
    let blocks = if rich {
        message_blocks(content, &[])
    } else {
        Vec::new()
    };
    json!({ "channel": channel, "ts": ts, "text": content, "blocks": blocks })
}

/// Seconds part of a Slack `ts` (`"1712345678.000100"`), else wall clock.
fn ts_secs(ts: &str) -> u64 {
    ts.split('.')
        .next()
        .and_then(|secs| secs.parse::<u64>().ok())
        .unwrap_or_else(now_secs)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// `Retry-After` (seconds) of a 429, capped at [`RATE_LIMIT_MAX_WAIT`].
fn retry_after(headers: &reqwest::header::HeaderMap) -> Duration {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(1))
        .min(RATE_LIMIT_MAX_WAIT)
}

/// Bounded FIFO memory of seen delivery keys.
#[derive(Debug, Default)]
struct SeenKeys {
    order: VecDeque<String>,
    set: HashSet<String>,
}

impl SeenKeys {
    /// `true` when none of `keys` was seen before; records all of them.
    fn first_sighting(&mut self, keys: &[String]) -> bool {
        if keys.iter().any(|key| self.set.contains(key)) {
            return false;
        }
        for key in keys {
            if self.set.insert(key.clone()) {
                self.order.push_back(key.clone());
            }
        }
        while self.order.len() > SLACK_DEDUP_CAPACITY {
            if let Some(oldest) = self.order.pop_front() {
                self.set.remove(&oldest);
            }
        }
        true
    }
}

/// Request body of one Web API call.
enum ApiBody<'a> {
    Empty,
    Json(&'a Value),
    Form(&'a [(&'a str, String)]),
}

/// Explanation posted (ephemerally) when a slash command cannot open its
/// anchor thread — typically a channel or DM the bot is not a member of.
fn anchor_failure_notice(err: &anyhow::Error) -> String {
    let code = api_error_code(err).unwrap_or("request_failed");
    format!(
        "ccteam 无法在这里开线程({code})。请先把 bot 邀请进此频道(`/invite @<bot>`),\
         或在与 bot 的私聊里使用它的斜杠命令。"
    )
}

/// Slack channel (Socket Mode inbound, Web API outbound).
pub struct SlackChannel {
    bot_token: String,
    app_token: String,
    allowed_users: Vec<String>,
    api_base: String,
    http: reqwest::Client,
    /// `auth.test` result, fetched once per listener (re-fetched only if a
    /// previous attempt failed).
    bot: RwLock<Option<BotIdentity>>,
    seen: Mutex<SeenKeys>,
    /// Where downloaded inbound files are staged.
    staging_dir: PathBuf,
    /// Probe path for rejected senders that are recorded but NOT notified
    /// (ambient channel chatter); the notified path goes through
    /// [`RejectedSenderNotifier`], which appends the same probe itself.
    probe_path: Option<PathBuf>,
    rejected_senders: RejectedSenderNotifier,
    /// ccteam's own command names (no `/`), as the daemon registers them —
    /// what [`rewrite_command_sigils`] turns into `!name` on the way out.
    command_names: RwLock<Vec<String>>,
    name: String,
}

impl SlackChannel {
    /// Build with the `xoxb-` bot token (Web API), the `xapp-` app-level
    /// token (Socket Mode, `connections:write`) and the provider allowlist of
    /// Slack user ids (`U…`; empty = deny all, `"*"` = anyone).
    pub fn new(bot_token: String, app_token: String, allowed_user_ids: Vec<String>) -> Self {
        Self {
            bot_token,
            app_token,
            allowed_users: allowed_user_ids,
            api_base: SLACK_API_BASE.to_string(),
            http: Self::http_for(SLACK_API_BASE),
            bot: RwLock::new(None),
            seen: Mutex::new(SeenKeys::default()),
            staging_dir: inbound_staging_dir(),
            probe_path: None,
            rejected_senders: RejectedSenderNotifier::default(),
            command_names: RwLock::new(Vec::new()),
            name: "slack".to_string(),
        }
    }

    /// [`rewrite_command_sigils`] with the registered names; `None` when
    /// nothing changes.
    async fn localize_commands(&self, text: &str) -> Option<String> {
        if !text.contains('/') {
            return None;
        }
        let mut names = self.command_names.read().await.clone();
        if names.is_empty() {
            return None;
        }
        // The agents' own commands a reply most often names (`/help` points
        // at `/model` and `/compact`) — typed with `!` on Slack just the same.
        names.extend(AGENT_COMMANDS_NAMED.iter().map(|n| n.to_string()));
        let out = rewrite_command_sigils(text, &names);
        (out != text).then_some(out)
    }

    fn http_for(api_base: &str) -> reqwest::Client {
        client_for_api_base(api_base, Duration::from_secs(30)).expect("reqwest client")
    }

    /// Override the channel-map key (a per-tenant bot's `"slack@<tenant>"`).
    pub fn with_name(mut self, name: String) -> Self {
        self.name = name;
        self
    }

    /// Record rejected senders to this JSONL path (the web binding flow
    /// reads it). Unset in tests / standalone use.
    pub fn with_probe_path(mut self, path: PathBuf) -> Self {
        self.rejected_senders = RejectedSenderNotifier::with_probe_path(path.clone());
        self.probe_path = Some(path);
        self
    }

    /// Point the Web API (and therefore the Socket Mode URL, which
    /// `apps.connections.open` hands out) at another base — a local mock in
    /// tests. A loopback base also bypasses any configured HTTP proxy.
    pub fn with_api_base(mut self, api_base: impl Into<String>) -> Self {
        self.api_base = api_base.into().trim_end_matches('/').to_string();
        self.http = Self::http_for(&self.api_base);
        self
    }

    /// Stage inbound files under `dir` instead of the daemon's shared
    /// inbound staging directory.
    pub fn with_staging_dir(mut self, dir: PathBuf) -> Self {
        self.staging_dir = dir;
        self
    }

    /// Provider-layer allowlist: empty = deny all, `"*"` = anyone.
    fn is_user_allowed(&self, user_id: &str) -> bool {
        self.allowed_users
            .iter()
            .any(|allowed| allowed == "*" || allowed == user_id)
    }

    // ── Web API ─────────────────────────────────────────────────────────

    /// Send a request, honouring one HTTP 429 `Retry-After` and retrying
    /// exactly once.
    async fn send_rate_limited(
        &self,
        build: impl Fn() -> reqwest::RequestBuilder,
    ) -> anyhow::Result<reqwest::Response> {
        let resp = build().send().await?;
        if resp.status() != reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Ok(resp);
        }
        let wait = retry_after(resp.headers());
        tracing::warn!(
            wait_ms = wait.as_millis() as u64,
            "slack: rate limited; retrying once"
        );
        tokio::time::sleep(wait).await;
        Ok(build().send().await?)
    }

    /// Call one Web API method with `token`; `ok: false` becomes a typed
    /// [`SlackApiError`].
    async fn call(&self, method: &str, token: &str, body: ApiBody<'_>) -> anyhow::Result<Value> {
        let url = format!("{}/{method}", self.api_base);
        let build = || {
            let req = self.http.post(&url).bearer_auth(token);
            match &body {
                ApiBody::Empty => req,
                ApiBody::Json(value) => req
                    .header(
                        reqwest::header::CONTENT_TYPE,
                        "application/json; charset=utf-8",
                    )
                    .body(value.to_string()),
                ApiBody::Form(fields) => req.form(fields),
            }
        };
        let resp = self.send_rate_limited(build).await?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        let v: Value = serde_json::from_str(&text)
            .with_context(|| format!("slack {method}: HTTP {status} with a non-JSON body"))?;
        if v.get("ok").and_then(Value::as_bool) == Some(true) {
            return Ok(v);
        }
        Err(SlackApiError {
            method: method.to_string(),
            code: v
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("unknown_error")
                .to_string(),
        }
        .into())
    }

    async fn call_bot(&self, method: &str, body: ApiBody<'_>) -> anyhow::Result<Value> {
        self.call(method, &self.bot_token, body).await
    }

    /// `auth.test` → the bot identity, cached after the first success.
    async fn ensure_identity(&self) -> anyhow::Result<BotIdentity> {
        if let Some(identity) = self.bot.read().await.clone() {
            return Ok(identity);
        }
        let v = self.call_bot("auth.test", ApiBody::Empty).await?;
        let identity = BotIdentity {
            user_id: str_at(&v, "/user_id").unwrap_or("").to_string(),
        };
        *self.bot.write().await = Some(identity.clone());
        Ok(identity)
    }

    /// Post one message; on a block rejection retry once as plain text.
    async fn post_message(
        &self,
        channel: &str,
        thread_ts: Option<&str>,
        content: &str,
        options: &[MessageOption],
    ) -> anyhow::Result<Option<String>> {
        let rich = post_body(channel, thread_ts, content, options, true);
        let v = match self
            .call_bot("chat.postMessage", ApiBody::Json(&rich))
            .await
        {
            Ok(v) => v,
            Err(err) if is_blocks_rejection(&err) => {
                tracing::warn!(error = %err, "slack: blocks rejected; resending as plain text");
                let plain = post_body(channel, thread_ts, content, options, false);
                self.call_bot("chat.postMessage", ApiBody::Json(&plain))
                    .await?
            }
            Err(err) => return Err(err),
        };
        Ok(str_at(&v, "/ts").map(str::to_string))
    }

    /// Post plain `text` (Slack's own `mrkdwn`, where `<@U…>` renders as a
    /// mention) — used for the slash-command anchor.
    async fn post_plain(&self, channel: &str, text: &str) -> anyhow::Result<Option<String>> {
        let body = post_body(channel, None, text, &[], false);
        let v = self
            .call_bot("chat.postMessage", ApiBody::Json(&body))
            .await?;
        Ok(str_at(&v, "/ts").map(str::to_string))
    }

    /// Files go through Slack's external-upload flow
    /// (`files.getUploadURLExternal` → POST bytes → `files.completeUploadExternal`).
    ///
    /// Choice: a non-empty `content` (or any options) is posted FIRST as its
    /// own Markdown message in the same thread, then every file is shared
    /// with only its own caption as `initial_comment`. Keeping the text out
    /// of `initial_comment` keeps Markdown rendering and buttons, and avoids
    /// that field's tighter limits. Returns the text message's `ts` (an
    /// upload has no message `ts` until Slack finishes sharing it).
    async fn send_with_attachments(&self, message: &SendMessage) -> anyhow::Result<Option<String>> {
        let thread_ts = message.thread_ts.as_deref();
        let mut first = None;
        if !message.content.is_empty() || !message.options.is_empty() {
            first = self
                .post_message(
                    &message.recipient,
                    thread_ts,
                    &message.content,
                    &message.options,
                )
                .await?;
        }
        for attachment in &message.attachments {
            self.upload_file(&message.recipient, thread_ts, attachment)
                .await?;
        }
        Ok(first)
    }

    async fn upload_file(
        &self,
        channel: &str,
        thread_ts: Option<&str>,
        attachment: &OutboundFile,
    ) -> anyhow::Result<()> {
        let bytes = tokio::fs::read(&attachment.path)
            .await
            .with_context(|| format!("read outbound file {}", attachment.path))?;
        let file_name = std::path::Path::new(&attachment.path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_string();
        let ticket = self
            .call_bot(
                "files.getUploadURLExternal",
                ApiBody::Form(&[
                    ("filename", file_name.clone()),
                    ("length", bytes.len().to_string()),
                ]),
            )
            .await?;
        let upload_url = str_at(&ticket, "/upload_url")
            .context("slack files.getUploadURLExternal: no upload_url")?;
        let file_id = str_at(&ticket, "/file_id")
            .context("slack files.getUploadURLExternal: no file_id")?
            .to_string();
        // The upload URL is pre-signed: it takes the raw bytes, no token.
        let resp = self
            .send_rate_limited(|| {
                self.http
                    .post(upload_url)
                    .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
                    .body(bytes.clone())
            })
            .await?;
        if !resp.status().is_success() {
            anyhow::bail!("slack file upload {file_name} → HTTP {}", resp.status());
        }
        let mut form = vec![
            (
                "files",
                json!([{ "id": file_id, "title": file_name }]).to_string(),
            ),
            ("channel_id", channel.to_string()),
        ];
        if let Some(thread_ts) = thread_ts.filter(|t| !t.is_empty()) {
            form.push(("thread_ts", thread_ts.to_string()));
        }
        if let Some(caption) = attachment.caption.as_ref().filter(|c| !c.is_empty()) {
            form.push(("initial_comment", caption.clone()));
        }
        self.call_bot("files.completeUploadExternal", ApiBody::Form(&form))
            .await?;
        Ok(())
    }

    /// Download one inbound file with the bot token and stage it. `Ok(None)`
    /// = over the size ceiling (rejected, not an error).
    async fn stage_file(
        &self,
        ts: &str,
        file: &PendingFile,
    ) -> anyhow::Result<Option<ChannelAttachment>> {
        if file
            .size
            .is_some_and(|size| size > SLACK_MAX_ATTACHMENT_BYTES)
        {
            return Ok(None);
        }
        let resp = self
            .send_rate_limited(|| self.http.get(&file.url).bearer_auth(&self.bot_token))
            .await?;
        let status = resp.status();
        if !status.is_success() {
            anyhow::bail!("slack file download {} → HTTP {status}", file.id);
        }
        // Without `files:read` Slack answers 200 with its HTML sign-in page;
        // staging that as the user's file would mislead the agent.
        let served_html = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| ct.starts_with("text/html"));
        let expects_html = file
            .mimetype
            .as_deref()
            .is_some_and(|m| m.starts_with("text/html"));
        if served_html && !expects_html {
            anyhow::bail!(
                "slack file download {} returned an HTML page (is the files:read scope granted?)",
                file.id
            );
        }
        let bytes = resp.bytes().await?;
        if bytes.len() as u64 > SLACK_MAX_ATTACHMENT_BYTES {
            return Ok(None);
        }
        let safe_name = sanitize_attachment_name(&file.name);
        tokio::fs::create_dir_all(&self.staging_dir).await?;
        let dest = self.staging_dir.join(sanitize_attachment_name(&format!(
            "slack-{ts}-{}-{safe_name}",
            file.id
        )));
        tokio::fs::write(&dest, &bytes).await?;
        let is_image = file
            .mimetype
            .as_deref()
            .is_some_and(|m| m.starts_with("image/"));
        Ok(Some(ChannelAttachment {
            kind: if is_image {
                AttachmentKind::Image
            } else {
                AttachmentKind::File
            },
            file_name: safe_name,
            local_path: dest.to_string_lossy().into_owned(),
            mime: file.mimetype.clone(),
            size: Some(bytes.len() as u64),
        }))
    }

    /// Answer a slash command privately through its `response_url`.
    async fn respond_ephemeral(&self, response_url: &str, text: &str) {
        if response_url.is_empty() {
            return;
        }
        let body = json!({ "response_type": "ephemeral", "text": text });
        if let Err(err) = self
            .send_rate_limited(|| self.http.post(response_url).json(&body))
            .await
        {
            tracing::warn!(error = %err, "slack: ephemeral slash-command reply failed");
        }
    }

    // ── Inbound ─────────────────────────────────────────────────────────

    /// Fail-closed rejection. Every rejected event leaves a probe; only a
    /// sender who addressed the bot (`notify`) gets the one-time notice.
    async fn reject_sender(
        &self,
        sender_id: &str,
        chat_id: &str,
        message_id: &str,
        timestamp: u64,
        notify: bool,
    ) {
        let probe = RejectedSenderProbe {
            channel: self.name.clone(),
            sender_id: sender_id.to_string(),
            chat_id: chat_id.to_string(),
            message_id: message_id.to_string(),
            timestamp,
        };
        if notify {
            self.rejected_senders.record_and_notify(self, probe).await;
            return;
        }
        tracing::debug!(
            channel = %self.name,
            sender_id = %sender_id,
            "slack: dropping ambient message from a sender outside the allowlist"
        );
        if let Some(path) = self.probe_path.as_ref() {
            probe.append_to(path).await;
        }
    }

    async fn first_sighting(&self, keys: &[String]) -> bool {
        self.seen.lock().await.first_sighting(keys)
    }

    /// Turn one ACKed envelope into the message the gateway should see.
    async fn handle_envelope(&self, envelope: &Envelope) -> Option<ChannelMessage> {
        match envelope.kind.as_str() {
            "events_api" => self.handle_event(&envelope.payload).await,
            "slash_commands" => self.handle_slash(&envelope.payload).await,
            "interactive" => self.handle_interactive(&envelope.payload).await,
            other => {
                tracing::debug!(kind = %other, "slack: ignoring envelope kind");
                None
            }
        }
    }

    async fn handle_event(&self, payload: &Value) -> Option<ChannelMessage> {
        let event = payload.get("event")?;
        if event.get("type").and_then(Value::as_str) != Some("message") {
            return None;
        }
        let bot = match self.ensure_identity().await {
            Ok(bot) => bot,
            Err(err) => {
                tracing::warn!(error = %err, "slack: auth.test failed; dropping event");
                return None;
            }
        };
        let decoded = decode_message_event(event, &bot)?;
        let mut keys = vec![format!("msg:{}:{}", decoded.channel, decoded.ts)];
        if let Some(event_id) = str_at(payload, "/event_id") {
            keys.push(format!("event:{event_id}"));
        }
        if !self.first_sighting(&keys).await {
            tracing::debug!(ts = %decoded.ts, "slack: duplicate delivery dropped");
            return None;
        }
        if !self.is_user_allowed(&decoded.user) {
            let notify = decoded.channel_type == "im" || decoded.mentions_bot;
            self.reject_sender(
                &decoded.user,
                &decoded.channel,
                &decoded.ts,
                ts_secs(&decoded.ts),
                notify,
            )
            .await;
            return None;
        }
        let files = decoded.files.clone();
        let mut message = decoded.into_channel_message(&self.name);
        for file in &files {
            match self.stage_file(&message.id, file).await {
                Ok(Some(attachment)) => message.attachments.push(attachment),
                Ok(None) => {
                    let mb = SLACK_MAX_ATTACHMENT_BYTES / (1024 * 1024);
                    self.notify_in_thread(
                        &message,
                        format!("⚠️ 附件 {} 超过 {mb}MB 上限,已拒收", file.name),
                    )
                    .await;
                }
                Err(err) => {
                    tracing::warn!(file = %file.id, error = %err, "slack: attachment download failed");
                    self.notify_in_thread(&message, format!("⚠️ 附件 {} 下载失败", file.name))
                        .await;
                }
            }
        }
        if message.content.is_empty() && message.attachments.is_empty() {
            return None;
        }
        Some(message)
    }

    async fn notify_in_thread(&self, message: &ChannelMessage, text: String) {
        let notice = SendMessage::new(text, message.reply_target.clone())
            .in_thread(message.thread_ts.clone());
        if let Err(err) = self.send(&notice).await {
            tracing::warn!(error = %err, "slack: attachment notice failed");
        }
    }

    async fn handle_slash(&self, payload: &Value) -> Option<ChannelMessage> {
        let command = decode_slash(payload)?;
        // No name check: Socket Mode only delivers this app's own slash
        // commands, and each app is named its own (`/ccteam`, `/cct2`, …) —
        // see `onboarding::slack_slash_command`.
        if !command.trigger_id.is_empty()
            && !self
                .first_sighting(&[format!("trigger:{}", command.trigger_id)])
                .await
        {
            return None;
        }
        if !self.is_user_allowed(&command.user_id) {
            self.reject_sender(
                &command.user_id,
                &command.channel_id,
                &command.trigger_id,
                now_secs(),
                true,
            )
            .await;
            return None;
        }
        let anchor = anchor_text(&command.command, &command.text, &command.user_id);
        let anchor_ts = match self.post_plain(&command.channel_id, &anchor).await {
            Ok(Some(ts)) => ts,
            Ok(None) => {
                let err = anyhow::anyhow!("chat.postMessage returned no ts");
                self.respond_ephemeral(&command.response_url, &anchor_failure_notice(&err))
                    .await;
                return None;
            }
            Err(err) => {
                tracing::warn!(
                    channel = %command.channel_id,
                    error = %err,
                    "slack: slash-command anchor post failed"
                );
                self.respond_ephemeral(&command.response_url, &anchor_failure_notice(&err))
                    .await;
                return None;
            }
        };
        Some(ChannelMessage {
            timestamp: ts_secs(&anchor_ts),
            id: anchor_ts.clone(),
            sender: command.user_id,
            reply_target: command.channel_id,
            content: slash_content(&command.text),
            channel: self.name.clone(),
            thread_ts: Some(anchor_ts),
            attachments: Vec::new(),
            selection: None,
        })
    }

    async fn handle_interactive(&self, payload: &Value) -> Option<ChannelMessage> {
        let click = decode_block_actions(payload)?;
        if !click.trigger_id.is_empty()
            && !self
                .first_sighting(&[format!("trigger:{}", click.trigger_id)])
                .await
        {
            return None;
        }
        if !self.is_user_allowed(&click.user_id) {
            self.reject_sender(
                &click.user_id,
                &click.channel_id,
                &click.message_ts,
                now_secs(),
                true,
            )
            .await;
            return None;
        }
        Some(ChannelMessage {
            id: click.message_ts,
            sender: click.user_id,
            reply_target: click.channel_id,
            content: String::new(),
            channel: self.name.clone(),
            timestamp: now_secs(),
            thread_ts: Some(click.thread_ts),
            attachments: Vec::new(),
            selection: Some(ChoiceReply { data: click.value }),
        })
    }

    /// Ordered consumer of ACKed envelopes. Returns when the gateway's
    /// receiver is gone.
    async fn process_envelopes(
        &self,
        mut work: mpsc::Receiver<Envelope>,
        tx: &mpsc::Sender<ChannelMessage>,
    ) {
        while let Some(envelope) = work.recv().await {
            if let Some(message) = self.handle_envelope(&envelope).await {
                if tx.send(message).await.is_err() {
                    return;
                }
            }
        }
    }

    /// Reconnect loop around [`Self::run_socket`] with capped exponential
    /// backoff. `Ok(())` once the gateway's receiver is gone; `Err` only for
    /// failures reconnecting cannot fix ([`FATAL_API_ERRORS`]).
    async fn connect_loop(
        &self,
        work: mpsc::Sender<Envelope>,
        tx: &mpsc::Sender<ChannelMessage>,
    ) -> anyhow::Result<()> {
        let mut backoff = RECONNECT_BACKOFF_MIN;
        loop {
            if tx.is_closed() {
                return Ok(());
            }
            match self.run_socket(&work, tx).await {
                Ok(SocketEnd::Closed) => return Ok(()),
                Ok(SocketEnd::Refresh) => {
                    tracing::info!("slack: Socket Mode asked for a reconnect");
                    backoff = RECONNECT_BACKOFF_MIN;
                    continue;
                }
                Ok(SocketEnd::Dropped { healthy }) => {
                    if healthy {
                        backoff = RECONNECT_BACKOFF_MIN;
                    }
                    tracing::warn!(
                        backoff_ms = backoff.as_millis() as u64,
                        "slack: socket dropped; reconnecting"
                    );
                }
                Err(err) => {
                    if api_error_code(&err).is_some_and(|code| FATAL_API_ERRORS.contains(&code)) {
                        tracing::error!(error = %err, "slack: listener stopped — fix the Slack app/credentials");
                        return Err(err);
                    }
                    tracing::warn!(
                        error = %err,
                        backoff_ms = backoff.as_millis() as u64,
                        "slack: socket setup failed; retrying"
                    );
                }
            }
            tokio::select! {
                _ = tokio::time::sleep(backoff) => {}
                _ = tx.closed() => return Ok(()),
            }
            backoff = (backoff * 2).min(RECONNECT_BACKOFF_MAX);
        }
    }

    /// One Socket Mode connection: open, then read frames — ACK each envelope
    /// first, then enqueue it for the ordered processor.
    async fn run_socket(
        &self,
        work: &mpsc::Sender<Envelope>,
        tx: &mpsc::Sender<ChannelMessage>,
    ) -> anyhow::Result<SocketEnd> {
        self.ensure_identity().await?;
        let opened = self
            .call("apps.connections.open", &self.app_token, ApiBody::Empty)
            .await?;
        let url = str_at(&opened, "/url").context("slack apps.connections.open: no url")?;
        let (socket, _) = tokio_tungstenite::connect_async(url)
            .await
            .context("slack: Socket Mode connect")?;
        let (mut write, mut read) = socket.split();
        tracing::info!(channel = %self.name, "slack: Socket Mode connected");

        let mut healthy = false;
        let mut last_recv = Instant::now();
        let mut ping = tokio::time::interval(WS_PING_INTERVAL);
        ping.tick().await;
        loop {
            tokio::select! {
                _ = tx.closed() => return Ok(SocketEnd::Closed),
                _ = ping.tick() => {
                    if last_recv.elapsed() > WS_IDLE_TIMEOUT {
                        tracing::warn!("slack: socket silent too long");
                        return Ok(SocketEnd::Dropped { healthy });
                    }
                    if write.send(WsMsg::Ping(Vec::new())).await.is_err() {
                        return Ok(SocketEnd::Dropped { healthy });
                    }
                }
                frame = read.next() => {
                    let frame = match frame {
                        Some(Ok(frame)) => frame,
                        Some(Err(err)) => {
                            tracing::warn!(error = %err, "slack: socket read error");
                            return Ok(SocketEnd::Dropped { healthy });
                        }
                        None => return Ok(SocketEnd::Dropped { healthy }),
                    };
                    last_recv = Instant::now();
                    let text = match frame {
                        WsMsg::Text(text) => text,
                        WsMsg::Ping(data) => {
                            let _ = write.send(WsMsg::Pong(data)).await;
                            continue;
                        }
                        WsMsg::Close(_) => return Ok(SocketEnd::Dropped { healthy }),
                        _ => continue,
                    };
                    match parse_frame(&text) {
                        Frame::Hello => {
                            healthy = true;
                            tracing::debug!("slack: Socket Mode hello");
                        }
                        Frame::Disconnect(reason) if reason == "link_disabled" => {
                            return Err(SlackApiError {
                                method: "socket_mode".to_string(),
                                code: reason,
                            }
                            .into());
                        }
                        Frame::Disconnect(reason) => {
                            tracing::info!(reason = %reason, "slack: Socket Mode disconnect");
                            return Ok(SocketEnd::Refresh);
                        }
                        Frame::Envelope(envelope) => {
                            // ACK before any slow work: Slack re-delivers an
                            // envelope that is not ACKed within 3 s.
                            let ack = json!({ "envelope_id": envelope.envelope_id }).to_string();
                            if write.send(WsMsg::Text(ack)).await.is_err() {
                                return Ok(SocketEnd::Dropped { healthy });
                            }
                            if work.send(envelope).await.is_err() {
                                return Ok(SocketEnd::Closed);
                            }
                        }
                        Frame::Other => {}
                    }
                }
            }
        }
    }
}

#[async_trait]
impl Channel for SlackChannel {
    fn name(&self) -> &str {
        &self.name
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<Option<String>> {
        let localized;
        let message = match self.localize_commands(&message.content).await {
            Some(content) => {
                localized = SendMessage {
                    content,
                    ..message.clone()
                };
                &localized
            }
            None => message,
        };
        if !message.attachments.is_empty() {
            return self.send_with_attachments(message).await;
        }
        self.post_message(
            &message.recipient,
            message.thread_ts.as_deref(),
            &message.content,
            &message.options,
        )
        .await
    }

    async fn listen(&self, tx: mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        // The reader ACKs and enqueues; the processor handles envelopes in
        // order. Both run on this task, so a reconnect never loses queued
        // (already ACKed) envelopes, and either side ending ends the listener.
        let (work_tx, work_rx) = mpsc::channel::<Envelope>(SLACK_ENVELOPE_QUEUE);
        tokio::select! {
            res = self.connect_loop(work_tx, &tx) => res,
            () = self.process_envelopes(work_rx, &tx) => Ok(()),
        }
    }

    async fn health_check(&self) -> bool {
        self.call_bot("auth.test", ApiBody::Empty).await.is_ok()
    }

    fn max_message_len(&self) -> Option<usize> {
        Some(SLACK_MAX_MESSAGE_UTF16)
    }

    fn session_threads(&self) -> bool {
        true
    }

    /// Slack has no command menu to fill; it keeps the names so replies can
    /// point at `!name` instead of a `/name` Slack would swallow.
    async fn register_commands(&self, cmds: &[CommandSpec]) -> anyhow::Result<()> {
        *self.command_names.write().await = cmds
            .iter()
            .map(|c| c.name.trim_start_matches('/').to_string())
            .filter(|n| !n.is_empty())
            .collect();
        Ok(())
    }

    fn native_buttons(&self) -> bool {
        true
    }

    async fn edit_message(
        &self,
        recipient: &str,
        message_id: &str,
        content: &str,
    ) -> anyhow::Result<Option<String>> {
        let localized = self.localize_commands(content).await;
        let content = localized.as_deref().unwrap_or(content);
        let rich = update_body(recipient, message_id, content, true);
        match self.call_bot("chat.update", ApiBody::Json(&rich)).await {
            Ok(_) => {}
            Err(err) if is_blocks_rejection(&err) => {
                let plain = update_body(recipient, message_id, content, false);
                self.call_bot("chat.update", ApiBody::Json(&plain)).await?;
            }
            Err(err) => return Err(err),
        }
        Ok(Some(message_id.to_string()))
    }

    async fn add_reaction(
        &self,
        chat_id: &str,
        message_id: &str,
    ) -> anyhow::Result<Option<String>> {
        let body =
            json!({ "channel": chat_id, "timestamp": message_id, "name": SLACK_ACK_REACTION });
        match self.call_bot("reactions.add", ApiBody::Json(&body)).await {
            Ok(_) => Ok(None),
            Err(err) if api_error_code(&err) == Some("already_reacted") => Ok(None),
            Err(err) => Err(err),
        }
    }

    async fn remove_reaction(
        &self,
        chat_id: &str,
        message_id: &str,
        _handle: Option<&str>,
    ) -> anyhow::Result<()> {
        let body =
            json!({ "channel": chat_id, "timestamp": message_id, "name": SLACK_ACK_REACTION });
        match self
            .call_bot("reactions.remove", ApiBody::Json(&body))
            .await
        {
            Ok(_) => Ok(()),
            Err(err) if api_error_code(&err) == Some("no_reaction") => Ok(()),
            Err(err) => Err(err),
        }
    }
}

#[cfg(test)]
#[path = "slack_tests.rs"]
mod tests;
