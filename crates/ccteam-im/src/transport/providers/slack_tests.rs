//! Slack provider tests. Every network peer is local: an in-test HTTP/1.1
//! responder stands in for the Web API (`with_api_base`) and a plain-TCP
//! WebSocket server stands in for Socket Mode (its `ws://` URL is what the
//! mock `apps.connections.open` hands out). No test touches real Slack, the
//! real `~/.ccteam`, or the process env (staging + probes go to tempdirs).

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message as WsMsg;
use tokio_tungstenite::WebSocketStream;

use super::*;
use crate::transport::OutboundFileKind;

const WAIT: Duration = Duration::from_secs(10);

// ─────────────────────────────────────────────────────────────────────────────
// Local Web API mock
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct Req {
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Req {
    /// Web API method name (`/chat.postMessage` → `chat.postMessage`).
    fn method(&self) -> &str {
        self.path.trim_start_matches('/')
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    fn form(&self) -> HashMap<String, String> {
        String::from_utf8_lossy(&self.body)
            .split('&')
            .filter(|pair| !pair.is_empty())
            .filter_map(|pair| {
                let (k, v) = pair.split_once('=')?;
                Some((url_decode(k), url_decode(v)))
            })
            .collect()
    }
}

fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

struct Resp {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

fn json_resp(v: Value) -> Resp {
    Resp {
        status: 200,
        headers: vec![("Content-Type".into(), "application/json".into())],
        body: v.to_string().into_bytes(),
    }
}

fn ok(extra: Value) -> Resp {
    let mut v = json!({ "ok": true });
    if let (Some(map), Some(extra)) = (v.as_object_mut(), extra.as_object()) {
        for (k, val) in extra {
            map.insert(k.clone(), val.clone());
        }
    }
    json_resp(v)
}

fn api_err(code: &str) -> Resp {
    json_resp(json!({ "ok": false, "error": code }))
}

type BoxFut = Pin<Box<dyn Future<Output = Resp> + Send>>;
type Handler = Arc<dyn Fn(Req) -> BoxFut + Send + Sync>;

struct MockHttp {
    base: String,
    log: Arc<std::sync::Mutex<Vec<Req>>>,
}

impl MockHttp {
    async fn start<F>(handler: F) -> Self
    where
        F: Fn(Req) -> BoxFut + Send + Sync + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handler: Handler = Arc::new(handler);
        let log: Arc<std::sync::Mutex<Vec<Req>>> = Arc::default();
        let accept_log = log.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(serve_conn(stream, handler.clone(), accept_log.clone()));
            }
        });
        Self {
            base: format!("http://{addr}"),
            log,
        }
    }

    async fn start_sync<F>(f: F) -> Self
    where
        F: Fn(&Req) -> Resp + Send + Sync + 'static,
    {
        let f = Arc::new(f);
        Self::start(move |req| {
            let f = f.clone();
            Box::pin(async move { f(&req) })
        })
        .await
    }

    fn requests(&self) -> Vec<Req> {
        self.log.lock().unwrap().clone()
    }

    fn calls(&self, method: &str) -> Vec<Req> {
        self.requests()
            .into_iter()
            .filter(|r| r.method() == method)
            .collect()
    }
}

async fn serve_conn(mut stream: TcpStream, handler: Handler, log: Arc<std::sync::Mutex<Vec<Req>>>) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or("");
    let path = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or("/")
        .split('?')
        .next()
        .unwrap_or("/")
        .to_string();
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| {
            let (k, v) = line.split_once(':')?;
            Some((k.trim().to_string(), v.trim().to_string()))
        })
        .collect();
    let content_length = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = buf[header_end..].to_vec();
    while body.len() < content_length {
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => body.extend_from_slice(&chunk[..n]),
        }
    }
    let req = Req {
        path,
        headers,
        body,
    };
    log.lock().unwrap().push(req.clone());
    let resp = handler(req).await;
    let mut out = format!(
        "HTTP/1.1 {} X\r\nContent-Length: {}\r\nConnection: close\r\n",
        resp.status,
        resp.body.len()
    );
    for (k, v) in &resp.headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str("\r\n");
    let _ = stream.write_all(out.as_bytes()).await;
    let _ = stream.write_all(&resp.body).await;
    let _ = stream.flush().await;
    let _ = stream.shutdown().await;
}

// ─────────────────────────────────────────────────────────────────────────────
// Fixtures
// ─────────────────────────────────────────────────────────────────────────────

const BOT: &str = "UBOT";
const ALLOWED: &str = "UALLOWED";

fn bot() -> BotIdentity {
    BotIdentity {
        user_id: BOT.to_string(),
    }
}

fn channel(base: &str, allowed: &[&str]) -> SlackChannel {
    SlackChannel::new(
        "xoxb-test".into(),
        "xapp-test".into(),
        allowed.iter().map(|s| s.to_string()).collect(),
    )
    .with_api_base(base)
}

fn envelope_frame(id: &str, kind: &str, payload: Value) -> String {
    json!({
        "envelope_id": id,
        "type": kind,
        "payload": payload,
        "accepts_response_payload": false,
        "retry_attempt": 0,
    })
    .to_string()
}

fn envelope(kind: &str, payload: Value) -> Envelope {
    Envelope {
        envelope_id: "env".into(),
        kind: kind.into(),
        payload,
    }
}

fn event_payload(event_id: &str, event: Value) -> Value {
    json!({ "type": "event_callback", "event_id": event_id, "event": event })
}

fn msg_event(user: &str, channel: &str, channel_type: &str, ts: &str, text: &str) -> Value {
    json!({
        "type": "message",
        "user": user,
        "channel": channel,
        "channel_type": channel_type,
        "ts": ts,
        "text": text,
    })
}

fn slash_payload(user: &str, channel: &str, text: &str, response_url: &str) -> Value {
    json!({
        "command": "/ccteam",
        "text": text,
        "user_id": user,
        "channel_id": channel,
        "response_url": response_url,
        "trigger_id": format!("trig-{user}-{text}"),
    })
}

fn click_payload(user: &str, channel: &str, message_ts: &str, thread_ts: Option<&str>) -> Value {
    let mut container =
        json!({ "type": "message", "message_ts": message_ts, "channel_id": channel });
    if let Some(t) = thread_ts {
        container["thread_ts"] = json!(t);
    }
    json!({
        "type": "block_actions",
        "user": { "id": user },
        "channel": { "id": channel },
        "container": container,
        "message": { "ts": message_ts },
        "trigger_id": format!("click-{message_ts}"),
        "actions": [{ "action_id": "ccteam_opt_1", "value": "tok:1", "type": "button" }],
    })
}

fn default_api(req: &Req) -> Resp {
    match req.method() {
        "auth.test" => {
            ok(json!({ "user_id": BOT, "bot_id": "BBOT", "team": "Acme", "user": "ccteam" }))
        }
        "chat.postMessage" => ok(json!({ "ts": "1700000999.000100" })),
        "chat.update" | "reactions.add" | "reactions.remove" => ok(json!({})),
        _ => api_err("unknown_method"),
    }
}

async fn recv(rx: &mut mpsc::Receiver<ChannelMessage>) -> ChannelMessage {
    tokio::time::timeout(WAIT, rx.recv())
        .await
        .expect("timed out waiting for an inbound ChannelMessage")
        .expect("listener closed the inbound stream")
}

async fn ws_server() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/link", listener.local_addr().unwrap());
    (listener, url)
}

async fn accept_ws(listener: &TcpListener) -> WebSocketStream<TcpStream> {
    let (stream, _) = tokio::time::timeout(WAIT, listener.accept())
        .await
        .expect("client never connected")
        .unwrap();
    tokio_tungstenite::accept_async(stream).await.unwrap()
}

async fn send_text(ws: &mut WebSocketStream<TcpStream>, text: String) {
    ws.send(WsMsg::Text(text)).await.unwrap();
}

fn ack_id(text: &str) -> Option<String> {
    serde_json::from_str::<Value>(text)
        .ok()?
        .get("envelope_id")?
        .as_str()
        .map(str::to_string)
}

// ─────────────────────────────────────────────────────────────────────────────
// Pure decode — the inbound thread invariant
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn top_level_message_threads_on_its_own_ts() {
    let event = msg_event(ALLOWED, "C1", "channel", "1700000001.000100", "hello");
    let msg = decode_message_event(&event, &bot())
        .unwrap()
        .into_channel_message("slack");
    assert_eq!(msg.thread_ts.as_deref(), Some("1700000001.000100"));
    assert_eq!(msg.id, "1700000001.000100", "id is the raw ts (👀 target)");
    assert_eq!(msg.reply_target, "C1");
    assert_eq!(msg.sender, ALLOWED);
    assert_eq!(msg.channel, "slack");
    assert_eq!(msg.timestamp, 1_700_000_001);
}

#[test]
fn thread_reply_and_broadcast_thread_on_the_parent_ts() {
    let mut reply = msg_event(ALLOWED, "C1", "channel", "1700000002.000200", "more");
    reply["thread_ts"] = json!("1700000001.000100");
    let decoded = decode_message_event(&reply, &bot()).unwrap();
    assert_eq!(decoded.thread_ts, "1700000001.000100");
    assert_eq!(
        decoded.into_channel_message("slack").id,
        "1700000002.000200"
    );

    let mut broadcast = reply.clone();
    broadcast["subtype"] = json!("thread_broadcast");
    assert_eq!(
        decode_message_event(&broadcast, &bot()).unwrap().thread_ts,
        "1700000001.000100"
    );
}

#[test]
fn own_and_other_bot_messages_and_foreign_subtypes_are_skipped() {
    let mine = msg_event(BOT, "C1", "channel", "1.1", "echo");
    assert!(decode_message_event(&mine, &bot()).is_none());

    let mut other_bot = msg_event("UOTHER", "C1", "channel", "1.2", "beep");
    other_bot["bot_id"] = json!("BOTHER");
    assert!(decode_message_event(&other_bot, &bot()).is_none());

    for subtype in [
        "message_changed",
        "message_deleted",
        "channel_join",
        "bot_message",
    ] {
        let mut e = msg_event(ALLOWED, "C1", "channel", "1.3", "x");
        e["subtype"] = json!(subtype);
        assert!(
            decode_message_event(&e, &bot()).is_none(),
            "{subtype} must be skipped"
        );
    }
    let mut share = msg_event(ALLOWED, "C1", "channel", "1.4", "see");
    share["subtype"] = json!("file_share");
    assert!(decode_message_event(&share, &bot()).is_some());

    let app_mention = json!({ "type": "app_mention", "user": ALLOWED, "channel": "C1", "ts": "1.5", "text": "<@UBOT> hi" });
    assert!(
        decode_message_event(&app_mention, &bot()).is_none(),
        "app_mention duplicates `message` and is never consumed"
    );
    let shared_channel = msg_event(ALLOWED, "C1", "app_home", "1.6", "x");
    assert!(decode_message_event(&shared_channel, &bot()).is_none());
}

#[test]
fn bot_mention_is_stripped_entities_unescaped_and_leading_space_trimmed() {
    let e = msg_event(
        ALLOWED,
        "C1",
        "channel",
        "1.1",
        "<@UBOT> ship it &amp; tell &lt;team&gt; <@UOTHER>",
    );
    let d = decode_message_event(&e, &bot()).unwrap();
    assert!(d.mentions_bot);
    assert_eq!(d.text, "ship it & tell <team> <@UOTHER>");

    // A leading space is how a user gets ` /status` past Slack's own slash
    // interception; the gateway must see the bare command.
    let e = msg_event(ALLOWED, "D1", "im", "1.2", " /status");
    let d = decode_message_event(&e, &bot()).unwrap();
    assert_eq!(d.text, "/status");
    assert!(!d.mentions_bot);

    let labelled = msg_event(ALLOWED, "C1", "channel", "1.3", "<@UBOT|ccteam> /help");
    assert_eq!(
        decode_message_event(&labelled, &bot()).unwrap().text,
        "/help"
    );

    // An escaped mention the user typed literally is text, not a mention.
    let literal = msg_event(ALLOWED, "C1", "channel", "1.4", "&lt;@UBOT&gt; hi");
    let d = decode_message_event(&literal, &bot()).unwrap();
    assert!(!d.mentions_bot);
    assert_eq!(d.text, "<@UBOT> hi");

    let only_mention = msg_event(ALLOWED, "C1", "channel", "1.5", "<@UBOT>");
    assert!(decode_message_event(&only_mention, &bot()).is_none());
}

#[test]
fn files_are_collected_and_unreachable_ones_skipped() {
    let mut e = msg_event(ALLOWED, "C1", "channel", "1.1", "");
    e["subtype"] = json!("file_share");
    e["files"] = json!([
        { "id": "F1", "name": "a.png", "mimetype": "image/png", "size": 3,
          "url_private_download": "http://x/F1/a.png" },
        { "id": "F2", "mode": "tombstone" },
    ]);
    let d = decode_message_event(&e, &bot()).unwrap();
    assert_eq!(d.text, "");
    assert_eq!(
        d.files,
        vec![PendingFile {
            id: "F1".into(),
            name: "a.png".into(),
            mimetype: Some("image/png".into()),
            size: Some(3),
            url: "http://x/F1/a.png".into(),
        }]
    );
}

#[test]
fn slash_text_maps_to_a_gateway_command_and_anchor_echoes_it() {
    assert_eq!(slash_content(""), "/help");
    assert_eq!(slash_content("   "), "/help");
    assert_eq!(slash_content("new codex"), "/new codex");
    assert_eq!(slash_content(" /status "), "/status");
    assert_eq!(slash_content("say a &amp; b"), "/say a & b");
    assert_eq!(
        anchor_text("/ccteam", "new codex", "U1"),
        "`/ccteam new codex` · <@U1>"
    );
    assert_eq!(anchor_text("/ccteam", "", "U1"), "`/ccteam` · <@U1>");
    assert_eq!(
        anchor_text("/ccteam", "a &lt;b&gt;", "U1"),
        "`/ccteam a &lt;b&gt;` · <@U1>",
        "echo stays Slack-escaped"
    );
}

#[test]
fn button_click_threads_on_the_clicked_messages_thread() {
    let in_thread =
        decode_block_actions(&click_payload(ALLOWED, "C1", "3.3", Some("1.1"))).unwrap();
    assert_eq!(in_thread.thread_ts, "1.1");
    assert_eq!(in_thread.message_ts, "3.3");
    assert_eq!(in_thread.value, "tok:1");

    let mut via_message = click_payload(ALLOWED, "C1", "3.3", None);
    via_message["message"]["thread_ts"] = json!("2.2");
    assert_eq!(decode_block_actions(&via_message).unwrap().thread_ts, "2.2");

    let top_level = decode_block_actions(&click_payload(ALLOWED, "C1", "3.3", None)).unwrap();
    assert_eq!(top_level.thread_ts, "3.3", "falls back to the message ts");

    let mut not_actions = click_payload(ALLOWED, "C1", "3.3", None);
    not_actions["type"] = json!("view_submission");
    assert!(decode_block_actions(&not_actions).is_none());
}

#[test]
fn a_few_options_render_as_one_row_of_buttons() {
    let options = options(3);
    let blocks = message_blocks("**bold** text", &options);
    assert_eq!(blocks.len(), 2, "markdown + one actions row");
    assert_eq!(
        blocks[0],
        json!({ "type": "markdown", "text": "**bold** text" })
    );
    let buttons = blocks[1]["elements"].as_array().unwrap();
    assert_eq!(buttons.len(), 3);
    assert_eq!(buttons[0]["type"], "button");
    assert_eq!(buttons[2]["value"], "tok:2");
    let ids: HashSet<&str> = buttons
        .iter()
        .map(|e| e["action_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.len(), 3, "action_ids are unique within the message");

    assert!(message_blocks("", &[]).is_empty());
}

/// A long list (a project picker, model × effort) is a dropdown, not a wall
/// of buttons; Slack caps one dropdown at 100 options.
#[test]
fn many_options_render_as_a_dropdown() {
    let options: Vec<MessageOption> = (0..130)
        .map(|i| MessageOption {
            weight: Default::default(),
            data: format!("nav:cd:p{i}"),
            label: if i == 0 {
                "x".repeat(200)
            } else {
                format!("  project {i}")
            },
            id: format!("p{i}"),
        })
        .collect();
    let blocks = message_blocks("📁 项目", &options);
    assert_eq!(blocks.len(), 3, "markdown + 100 + 30 in two dropdowns");
    let select = &blocks[1]["elements"][0];
    assert_eq!(select["type"], "static_select");
    let choices = select["options"].as_array().unwrap();
    assert_eq!(choices.len(), 100);
    assert_eq!(choices[0]["value"], "nav:cd:p0");
    assert_eq!(
        choices[0]["text"]["text"].as_str().unwrap().chars().count(),
        SLACK_BUTTON_LABEL_MAX_CHARS
    );
    assert_eq!(choices[1]["text"]["text"], "project 1", "padding trimmed");
    assert_ne!(
        blocks[1]["elements"][0]["action_id"],
        blocks[2]["elements"][0]["action_id"]
    );
}

#[test]
fn a_dropdown_pick_comes_back_like_a_button_click() {
    let mut payload = click_payload(ALLOWED, "C1", "5.5", Some("1.1"));
    payload["actions"] = json!([{
        "type": "static_select",
        "action_id": "ccteam_select_0",
        "selected_option": { "value": "nav:cd:beta", "text": { "type": "plain_text", "text": "beta" } },
    }]);
    let click = decode_block_actions(&payload).unwrap();
    assert_eq!(click.value, "nav:cd:beta");
    assert_eq!(click.thread_ts, "1.1");
}

#[test]
fn socket_frames_parse() {
    assert_eq!(
        parse_frame(r#"{"type":"hello","num_connections":1}"#),
        Frame::Hello
    );
    assert_eq!(
        parse_frame(r#"{"type":"disconnect","reason":"refresh_requested"}"#),
        Frame::Disconnect("refresh_requested".into())
    );
    match parse_frame(&envelope_frame("e1", "events_api", json!({"a": 1}))) {
        Frame::Envelope(env) => {
            assert_eq!(env.envelope_id, "e1");
            assert_eq!(env.kind, "events_api");
            assert_eq!(env.payload, json!({"a": 1}));
        }
        other => panic!("expected envelope, got {other:?}"),
    }
    assert_eq!(parse_frame("not json"), Frame::Other);
}

#[test]
fn seen_keys_dedupes_and_stays_bounded() {
    let mut seen = SeenKeys::default();
    assert!(seen.first_sighting(&["a".into(), "b".into()]));
    assert!(
        !seen.first_sighting(&["b".into()]),
        "any seen key = duplicate"
    );
    assert!(!seen.first_sighting(&["c".into(), "a".into()]));
    for i in 0..(SLACK_DEDUP_CAPACITY * 2) {
        seen.first_sighting(&[format!("k{i}")]);
    }
    assert!(seen.order.len() <= SLACK_DEDUP_CAPACITY);
    assert_eq!(seen.order.len(), seen.set.len());
    assert!(
        seen.first_sighting(&["a".into()]),
        "evicted keys are forgotten"
    );
}

#[test]
fn allowlist_is_fail_closed_with_wildcard() {
    let closed = channel("http://127.0.0.1:9", &[]);
    assert!(!closed.is_user_allowed(ALLOWED), "empty = deny all");
    let named = channel("http://127.0.0.1:9", &[ALLOWED]);
    assert!(named.is_user_allowed(ALLOWED));
    assert!(!named.is_user_allowed("USTRANGER"));
    let open = channel("http://127.0.0.1:9", &["*"]);
    assert!(open.is_user_allowed("USTRANGER"));
}

#[test]
fn channel_contract_threads_sessions_and_caps_length() {
    let ch = channel("http://127.0.0.1:9", &[]);
    assert!(
        ch.session_threads(),
        "Slack gives each session its own thread"
    );
    assert_eq!(ch.max_message_len(), Some(SLACK_MAX_MESSAGE_UTF16));
    assert_eq!(ch.name(), "slack");
    assert_eq!(
        ch.with_name("slack@u1".into()).name(),
        "slack@u1",
        "per-tenant key override"
    );
}

#[test]
fn retry_after_header_is_parsed_and_capped() {
    let mut headers = reqwest::header::HeaderMap::new();
    assert_eq!(retry_after(&headers), Duration::from_secs(1));
    headers.insert(reqwest::header::RETRY_AFTER, "3".parse().unwrap());
    assert_eq!(retry_after(&headers), Duration::from_secs(3));
    headers.insert(reqwest::header::RETRY_AFTER, "9999".parse().unwrap());
    assert_eq!(retry_after(&headers), RATE_LIMIT_MAX_WAIT);
}

// ─────────────────────────────────────────────────────────────────────────────
// Socket Mode end to end
// ─────────────────────────────────────────────────────────────────────────────

/// The load-bearing inbound contract over a real (local) socket: every
/// envelope is ACKed — the slash command's ACK provably lands BEFORE its
/// anchor post (the mock refuses the post until the ACK arrived) — a
/// re-delivery is deduped, Ping is answered, and every emitted message
/// carries `thread_ts`.
#[tokio::test]
async fn socket_mode_acks_first_dedupes_and_every_message_carries_thread_ts() {
    let (ws_listener, ws_url) = ws_server().await;
    let slash_acked = Arc::new(tokio::sync::Notify::new());
    let gate = slash_acked.clone();
    let url = ws_url.clone();
    let api = MockHttp::start(move |req| {
        let gate = gate.clone();
        let url = url.clone();
        Box::pin(async move {
            match req.method() {
                "apps.connections.open" => ok(json!({ "url": url })),
                "chat.postMessage" => {
                    if tokio::time::timeout(WAIT, gate.notified()).await.is_err() {
                        return api_err("anchor_posted_before_ack");
                    }
                    ok(json!({ "ts": "1700000100.000200" }))
                }
                "response" => ok(json!({})),
                _ => default_api(&req),
            }
        })
    })
    .await;

    let response_url = format!("{}/response", api.base);
    let frames = vec![
        envelope_frame(
            "e1",
            "events_api",
            event_payload(
                "Ev1",
                msg_event(
                    ALLOWED,
                    "C1",
                    "channel",
                    "1700000001.000100",
                    "<@UBOT> hello &amp; welcome",
                ),
            ),
        ),
        envelope_frame(
            "e2",
            "events_api",
            event_payload("Ev2", {
                let mut e = msg_event(ALLOWED, "C1", "channel", "1700000002.000200", "follow up");
                e["thread_ts"] = json!("1700000001.000100");
                e
            }),
        ),
        // Slack re-delivers Ev1 (new envelope, same event_id): ACKed, dropped.
        envelope_frame(
            "e3",
            "events_api",
            event_payload(
                "Ev1",
                msg_event(
                    ALLOWED,
                    "C1",
                    "channel",
                    "1700000001.000100",
                    "<@UBOT> hello &amp; welcome",
                ),
            ),
        ),
        envelope_frame(
            "e4",
            "slash_commands",
            slash_payload(ALLOWED, "C1", "new codex", &response_url),
        ),
        envelope_frame(
            "e5",
            "interactive",
            click_payload(
                ALLOWED,
                "C1",
                "1700000003.000300",
                Some("1700000001.000100"),
            ),
        ),
    ];
    let server = tokio::spawn(async move {
        let mut ws = accept_ws(&ws_listener).await;
        send_text(&mut ws, json!({ "type": "hello" }).to_string()).await;
        for frame in frames {
            send_text(&mut ws, frame).await;
        }
        ws.send(WsMsg::Ping(b"hb".to_vec())).await.unwrap();
        let mut acks = Vec::new();
        let mut pong = false;
        while acks.len() < 5 || !pong {
            match tokio::time::timeout(WAIT, ws.next()).await {
                Ok(Some(Ok(WsMsg::Text(text)))) => {
                    let id = ack_id(&text).expect("client frames are envelope ACKs");
                    if id == "e4" {
                        slash_acked.notify_one();
                    }
                    acks.push(id);
                }
                Ok(Some(Ok(WsMsg::Pong(data)))) => {
                    assert_eq!(data, b"hb".to_vec());
                    pong = true;
                }
                Ok(Some(Ok(_))) => {}
                other => panic!("socket ended before all ACKs: {other:?} acks={acks:?}"),
            }
        }
        (acks, ws)
    });

    let ch = Arc::new(channel(&api.base, &[ALLOWED]));
    let (tx, mut rx) = mpsc::channel(16);
    let listener = tokio::spawn({
        let ch = ch.clone();
        async move { ch.listen(tx).await }
    });

    let top = recv(&mut rx).await;
    let reply = recv(&mut rx).await;
    let slash = recv(&mut rx).await;
    let click = recv(&mut rx).await;
    let (acks, _ws) = server.await.unwrap();
    assert_eq!(acks, vec!["e1", "e2", "e3", "e4", "e5"]);

    for m in [&top, &reply, &slash, &click] {
        assert!(m.thread_ts.is_some(), "inbound invariant: {m:?}");
        assert_eq!(m.channel, "slack");
        assert_eq!(m.reply_target, "C1");
        assert_eq!(m.sender, ALLOWED);
    }
    assert_eq!(top.id, "1700000001.000100");
    assert_eq!(top.thread_ts.as_deref(), Some("1700000001.000100"));
    assert_eq!(top.content, "hello & welcome");
    assert_eq!(reply.id, "1700000002.000200");
    assert_eq!(reply.thread_ts.as_deref(), Some("1700000001.000100"));
    assert_eq!(slash.content, "/new codex");
    assert_eq!(slash.id, "1700000100.000200");
    assert_eq!(slash.thread_ts.as_deref(), Some("1700000100.000200"));
    assert_eq!(
        click.selection,
        Some(ChoiceReply {
            data: "tok:1".into()
        })
    );
    assert!(click.content.is_empty());
    assert_eq!(click.thread_ts.as_deref(), Some("1700000001.000100"));

    let anchor = api.calls("chat.postMessage");
    assert_eq!(anchor.len(), 1);
    let body = anchor[0].json();
    assert_eq!(body["channel"], "C1");
    assert_eq!(body["text"], "`/ccteam new codex` · <@UALLOWED>");
    assert!(body.get("thread_ts").is_none(), "the anchor is top-level");
    assert_eq!(
        api.calls("auth.test")[0].header("authorization"),
        Some("Bearer xoxb-test")
    );
    assert_eq!(
        api.calls("apps.connections.open")[0].header("authorization"),
        Some("Bearer xapp-test")
    );

    drop(rx);
    let ended = tokio::time::timeout(WAIT, listener)
        .await
        .expect("listen must return once the receiver is gone")
        .unwrap();
    assert!(ended.is_ok(), "{ended:?}");
}

#[tokio::test]
async fn refresh_requested_reopens_the_socket() {
    let (ws_listener, ws_url) = ws_server().await;
    let api = MockHttp::start_sync(move |req| match req.method() {
        "apps.connections.open" => ok(json!({ "url": ws_url })),
        _ => default_api(req),
    })
    .await;
    let server = tokio::spawn(async move {
        let mut first = accept_ws(&ws_listener).await;
        send_text(&mut first, json!({ "type": "hello" }).to_string()).await;
        send_text(
            &mut first,
            json!({ "type": "disconnect", "reason": "refresh_requested" }).to_string(),
        )
        .await;
        let mut second = accept_ws(&ws_listener).await;
        send_text(&mut second, json!({ "type": "hello" }).to_string()).await;
        send_text(
            &mut second,
            envelope_frame(
                "e1",
                "events_api",
                event_payload(
                    "Ev9",
                    msg_event(ALLOWED, "D1", "im", "5.5", "after refresh"),
                ),
            ),
        )
        .await;
        (first, second)
    });

    let ch = Arc::new(channel(&api.base, &[ALLOWED]));
    let (tx, mut rx) = mpsc::channel(4);
    let listener = tokio::spawn({
        let ch = ch.clone();
        async move { ch.listen(tx).await }
    });
    let msg = recv(&mut rx).await;
    assert_eq!(msg.content, "after refresh");
    assert_eq!(msg.thread_ts.as_deref(), Some("5.5"));
    assert_eq!(api.calls("apps.connections.open").len(), 2);
    assert_eq!(api.calls("auth.test").len(), 1, "identity is cached");
    let _sockets = server.await.unwrap();
    drop(rx);
    assert!(tokio::time::timeout(WAIT, listener)
        .await
        .unwrap()
        .unwrap()
        .is_ok());
}

#[tokio::test]
async fn link_disabled_stops_the_listener_with_an_error() {
    let (ws_listener, ws_url) = ws_server().await;
    let api = MockHttp::start_sync(move |req| match req.method() {
        "apps.connections.open" => ok(json!({ "url": ws_url })),
        _ => default_api(req),
    })
    .await;
    let server = tokio::spawn(async move {
        let mut ws = accept_ws(&ws_listener).await;
        send_text(&mut ws, json!({ "type": "hello" }).to_string()).await;
        send_text(
            &mut ws,
            json!({ "type": "disconnect", "reason": "link_disabled" }).to_string(),
        )
        .await;
        ws
    });
    let ch = channel(&api.base, &[ALLOWED]);
    let (tx, _rx) = mpsc::channel(4);
    let err = tokio::time::timeout(WAIT, ch.listen(tx))
        .await
        .expect("link_disabled must end the listener")
        .expect_err("link_disabled is fatal");
    assert!(err.to_string().contains("link_disabled"), "{err}");
    let _ws = server.await.unwrap();
}

#[tokio::test]
async fn invalid_app_token_stops_the_listener_without_retry_spin() {
    let api = MockHttp::start_sync(|req| match req.method() {
        "apps.connections.open" => api_err("invalid_auth"),
        _ => default_api(req),
    })
    .await;
    let ch = channel(&api.base, &[ALLOWED]);
    let (tx, _rx) = mpsc::channel(4);
    let err = tokio::time::timeout(WAIT, ch.listen(tx))
        .await
        .expect("a fatal auth error must end the listener")
        .expect_err("invalid_auth is fatal");
    assert!(err.to_string().contains("invalid_auth"), "{err}");
    assert_eq!(api.calls("apps.connections.open").len(), 1);
}

// ─────────────────────────────────────────────────────────────────────────────
// Allowlist + rejected senders (envelope handler, no socket)
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn rejected_senders_are_always_probed_but_only_notified_when_addressing_the_bot() {
    let api = MockHttp::start_sync(default_api).await;
    let tmp = tempfile::tempdir().unwrap();
    let probe_path = tmp.path().join("rejected.jsonl");
    let ch = channel(&api.base, &[ALLOWED]).with_probe_path(probe_path.clone());
    let response_url = format!("{}/response", api.base);

    let ambient = envelope(
        "events_api",
        event_payload("Ev1", msg_event("UX", "C1", "channel", "1.1", "chatting")),
    );
    assert!(ch.handle_envelope(&ambient).await.is_none());
    assert!(
        api.calls("chat.postMessage").is_empty(),
        "ambient channel chatter must never get a notice"
    );

    let mention = envelope(
        "events_api",
        event_payload(
            "Ev2",
            msg_event("UY", "C1", "channel", "1.2", "<@UBOT> let me in"),
        ),
    );
    assert!(ch.handle_envelope(&mention).await.is_none());

    let dm = envelope(
        "events_api",
        event_payload("Ev3", msg_event("UX", "D1", "im", "1.3", "hello?")),
    );
    assert!(ch.handle_envelope(&dm).await.is_none());
    let dm_again = envelope(
        "events_api",
        event_payload("Ev4", msg_event("UX", "D1", "im", "1.4", "anyone?")),
    );
    assert!(ch.handle_envelope(&dm_again).await.is_none());

    let slash = envelope(
        "slash_commands",
        slash_payload("UZ", "C1", "status", &response_url),
    );
    assert!(ch.handle_envelope(&slash).await.is_none());
    let click = envelope("interactive", click_payload("UW", "C1", "9.9", None));
    assert!(ch.handle_envelope(&click).await.is_none());

    let notices: Vec<Value> = api
        .calls("chat.postMessage")
        .iter()
        .map(Req::json)
        .collect();
    let targets: Vec<(&str, bool)> = notices
        .iter()
        .map(|n| {
            (
                n["channel"].as_str().unwrap(),
                n["text"].as_str().unwrap().contains("绑定 ID"),
            )
        })
        .collect();
    assert_eq!(
        targets,
        vec![("C1", true), ("D1", true), ("C1", true), ("C1", true)],
        "mention, first DM, slash and click notify once each; the repeat DM does not"
    );
    assert!(notices[1]["text"].as_str().unwrap().contains("UX"));

    let probes: Vec<RejectedSenderProbe> = std::fs::read_to_string(&probe_path)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let senders: Vec<&str> = probes.iter().map(|p| p.sender_id.as_str()).collect();
    assert_eq!(
        senders,
        vec!["UX", "UY", "UX", "UX", "UZ", "UW"],
        "every reject is probed"
    );
    assert!(probes.iter().all(|p| p.channel == "slack"));
    assert_eq!(probes[0].chat_id, "C1");
    assert_eq!(probes[0].message_id, "1.1");
}

#[tokio::test]
async fn empty_allowlist_admits_no_one() {
    let api = MockHttp::start_sync(default_api).await;
    let ch = channel(&api.base, &[]);
    let dm = envelope(
        "events_api",
        event_payload("Ev1", msg_event(ALLOWED, "D1", "im", "1.1", "hi")),
    );
    assert!(ch.handle_envelope(&dm).await.is_none());
    let click = envelope("interactive", click_payload(ALLOWED, "C1", "2.2", None));
    assert!(ch.handle_envelope(&click).await.is_none());
}

#[tokio::test]
async fn slash_command_without_a_postable_anchor_explains_ephemerally_and_drops() {
    let api = MockHttp::start_sync(|req| match req.method() {
        "chat.postMessage" => api_err("channel_not_found"),
        "response" => ok(json!({})),
        _ => default_api(req),
    })
    .await;
    let ch = channel(&api.base, &[ALLOWED]);
    let slash = envelope(
        "slash_commands",
        slash_payload(
            ALLOWED,
            "D0OTHER",
            "new codex",
            &format!("{}/response", api.base),
        ),
    );
    assert!(ch.handle_envelope(&slash).await.is_none());
    let replies = api.calls("response");
    assert_eq!(replies.len(), 1);
    let body = replies[0].json();
    assert_eq!(body["response_type"], "ephemeral");
    assert!(body["text"].as_str().unwrap().contains("channel_not_found"));
}

/// Each app declares its own command (`/cct2` for an app named cct2) and
/// Socket Mode only routes an app its own commands, so whatever command
/// arrives is this app's: it opens an anchor thread like `/ccteam` does.
#[tokio::test]
async fn an_apps_own_slash_command_works_whatever_it_is_named() {
    let api = MockHttp::start_sync(default_api).await;
    let ch = channel(&api.base, &[ALLOWED]);
    let mut payload = slash_payload(ALLOWED, "C1", "projects", "");
    payload["command"] = json!("/cct2");
    let message = ch
        .handle_envelope(&envelope("slash_commands", payload))
        .await
        .expect("the app's own command is handled");
    assert_eq!(message.content, "/projects");
    let posts = api.calls("chat.postMessage");
    assert_eq!(posts.len(), 1, "one anchor message");
    assert!(posts[0].json()["text"]
        .as_str()
        .unwrap()
        .contains("/cct2 projects"));
    assert_eq!(message.thread_ts.as_deref(), Some("1700000999.000100"));
}

// ─────────────────────────────────────────────────────────────────────────────
// Inbound files
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn inbound_files_download_with_the_bot_token_and_stage_by_kind() {
    let api = MockHttp::start_sync(|req| match req.path.as_str() {
        "/files/F1/shot.png" => Resp {
            status: 200,
            headers: vec![("Content-Type".into(), "image/png".into())],
            body: b"\x89PNG".to_vec(),
        },
        "/files/F2/notes.txt" => Resp {
            status: 200,
            headers: vec![("Content-Type".into(), "text/plain".into())],
            body: b"notes".to_vec(),
        },
        _ => default_api(req),
    })
    .await;
    let tmp = tempfile::tempdir().unwrap();
    let ch = channel(&api.base, &[ALLOWED]).with_staging_dir(tmp.path().to_path_buf());
    let mut event = msg_event(ALLOWED, "C1", "channel", "7.7", "look");
    event["subtype"] = json!("file_share");
    event["files"] = json!([
        { "id": "F1", "name": "shot.png", "mimetype": "image/png", "size": 4,
          "url_private_download": format!("{}/files/F1/shot.png", api.base) },
        { "id": "F2", "name": "../notes.txt", "mimetype": "text/plain",
          "url_private_download": format!("{}/files/F2/notes.txt", api.base) },
    ]);
    let msg = ch
        .handle_envelope(&envelope("events_api", event_payload("Ev1", event)))
        .await
        .expect("allowed message with files");
    assert_eq!(msg.content, "look");
    assert_eq!(msg.thread_ts.as_deref(), Some("7.7"));
    assert_eq!(msg.attachments.len(), 2);
    let image = &msg.attachments[0];
    assert_eq!(image.kind, AttachmentKind::Image);
    assert_eq!(image.file_name, "shot.png");
    assert_eq!(std::fs::read(&image.local_path).unwrap(), b"\x89PNG");
    assert!(image.local_path.starts_with(tmp.path().to_str().unwrap()));
    let file = &msg.attachments[1];
    assert_eq!(file.kind, AttachmentKind::File);
    assert_eq!(file.file_name, "notes.txt", "names are sanitized");
    assert_eq!(std::fs::read(&file.local_path).unwrap(), b"notes");
    for download in api
        .requests()
        .iter()
        .filter(|r| r.path.starts_with("/files/"))
    {
        assert_eq!(download.header("authorization"), Some("Bearer xoxb-test"));
    }
}

#[tokio::test]
async fn oversize_or_html_files_are_refused_with_a_thread_notice() {
    let api = MockHttp::start_sync(|req| match req.path.as_str() {
        "/files/F2/x.pdf" => Resp {
            status: 200,
            headers: vec![("Content-Type".into(), "text/html; charset=utf-8".into())],
            body: b"<html>sign in</html>".to_vec(),
        },
        _ => default_api(req),
    })
    .await;
    let tmp = tempfile::tempdir().unwrap();
    let ch = channel(&api.base, &[ALLOWED]).with_staging_dir(tmp.path().to_path_buf());
    let mut event = msg_event(ALLOWED, "C1", "channel", "8.8", "");
    event["thread_ts"] = json!("1.1");
    event["subtype"] = json!("file_share");
    event["files"] = json!([
        { "id": "F1", "name": "huge.bin", "size": SLACK_MAX_ATTACHMENT_BYTES + 1,
          "url_private_download": format!("{}/files/F1/huge.bin", api.base) },
        { "id": "F2", "name": "x.pdf", "mimetype": "application/pdf",
          "url_private_download": format!("{}/files/F2/x.pdf", api.base) },
    ]);
    assert!(
        ch.handle_envelope(&envelope("events_api", event_payload("Ev1", event)))
            .await
            .is_none(),
        "nothing usable left → no turn"
    );
    assert!(
        !api.requests()
            .iter()
            .any(|r| r.path == "/files/F1/huge.bin"),
        "an oversize file is never downloaded"
    );
    let notices: Vec<Value> = api
        .calls("chat.postMessage")
        .iter()
        .map(Req::json)
        .collect();
    assert_eq!(notices.len(), 2);
    assert!(notices[0]["text"].as_str().unwrap().contains("超过"));
    assert!(notices[1]["text"].as_str().unwrap().contains("下载失败"));
    assert!(notices.iter().all(|n| n["thread_ts"] == "1.1"));
    assert!(
        std::fs::read_dir(tmp.path())
            .map(|d| d.count())
            .unwrap_or(0)
            == 0
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Outbound
// ─────────────────────────────────────────────────────────────────────────────

fn options(n: usize) -> Vec<MessageOption> {
    (0..n)
        .map(|i| MessageOption {
            weight: Default::default(),
            data: format!("tok:{i}"),
            label: format!("Option {i}"),
            id: format!("o{i}"),
        })
        .collect()
}

#[tokio::test]
async fn send_posts_a_markdown_block_with_buttons_into_the_thread() {
    let api = MockHttp::start_sync(default_api).await;
    let ch = channel(&api.base, &[]);
    let ts = ch
        .send(
            &SendMessage::new("**done**\n1. a\n2. b", "C1")
                .in_thread(Some("1.1".into()))
                .with_options(options(2)),
        )
        .await
        .unwrap();
    assert_eq!(ts.as_deref(), Some("1700000999.000100"));
    let calls = api.calls("chat.postMessage");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].header("authorization"), Some("Bearer xoxb-test"));
    let body = calls[0].json();
    assert_eq!(body["channel"], "C1");
    assert_eq!(body["thread_ts"], "1.1");
    assert_eq!(body["unfurl_links"], false);
    assert_eq!(body["text"], "**done**\n1. a\n2. b");
    assert_eq!(
        body["blocks"][0],
        json!({ "type": "markdown", "text": "**done**\n1. a\n2. b" })
    );
    assert_eq!(body["blocks"][1]["type"], "actions");
    assert_eq!(body["blocks"][1]["elements"][1]["value"], "tok:1");
}

#[tokio::test]
async fn rejected_blocks_retry_once_as_plain_text() {
    let api = MockHttp::start_sync(|req| match req.method() {
        "chat.postMessage" if req.json().get("blocks").is_some() => api_err("invalid_blocks"),
        "chat.postMessage" => ok(json!({ "ts": "2.2" })),
        "chat.update" if !req.json()["blocks"].as_array().unwrap().is_empty() => {
            api_err("msg_blocks_too_long")
        }
        _ => default_api(req),
    })
    .await;
    let ch = channel(&api.base, &[]);
    let ts = ch
        .send(&SendMessage::new("hi", "C1").with_options(options(1)))
        .await
        .unwrap();
    assert_eq!(ts.as_deref(), Some("2.2"));
    let calls = api.calls("chat.postMessage");
    assert_eq!(calls.len(), 2);
    assert!(calls[1].json().get("blocks").is_none());
    assert_eq!(calls[1].json()["text"], "hi");

    let edited = ch.edit_message("C1", "2.2", "updated").await.unwrap();
    assert_eq!(edited.as_deref(), Some("2.2"));
    let updates = api.calls("chat.update");
    assert_eq!(updates.len(), 2);
    assert_eq!(
        updates[1].json()["blocks"],
        json!([]),
        "plain update clears blocks"
    );
}

#[tokio::test]
async fn other_api_errors_are_not_retried() {
    let api = MockHttp::start_sync(|req| match req.method() {
        "chat.postMessage" => api_err("channel_not_found"),
        _ => default_api(req),
    })
    .await;
    let ch = channel(&api.base, &[]);
    let err = ch.send(&SendMessage::new("hi", "C9")).await.unwrap_err();
    assert_eq!(api_error_code(&err), Some("channel_not_found"));
    assert_eq!(api.calls("chat.postMessage").len(), 1);
}

#[tokio::test]
async fn edit_message_updates_in_place_with_a_markdown_block() {
    let api = MockHttp::start_sync(default_api).await;
    let ch = channel(&api.base, &[]);
    let id = ch.edit_message("C1", "3.3", "progress 50%").await.unwrap();
    assert_eq!(id.as_deref(), Some("3.3"));
    let body = api.calls("chat.update")[0].json();
    assert_eq!(body["channel"], "C1");
    assert_eq!(body["ts"], "3.3");
    assert_eq!(body["text"], "progress 50%");
    assert_eq!(body["blocks"][0]["type"], "markdown");
}

#[tokio::test]
async fn eyes_reaction_tolerates_already_reacted_and_no_reaction() {
    let api = MockHttp::start_sync(|req| match (req.method(), req.json()["channel"].as_str()) {
        ("reactions.add", Some("C1")) => api_err("already_reacted"),
        ("reactions.remove", Some("C1")) => api_err("no_reaction"),
        ("reactions.add", _) => api_err("channel_not_found"),
        _ => default_api(req),
    })
    .await;
    let ch = channel(&api.base, &[]);
    assert_eq!(ch.add_reaction("C1", "4.4").await.unwrap(), None);
    ch.remove_reaction("C1", "4.4", None).await.unwrap();
    assert!(ch.add_reaction("C2", "4.4").await.is_err());
    let add = api.calls("reactions.add")[0].json();
    assert_eq!(
        add,
        json!({ "channel": "C1", "timestamp": "4.4", "name": "eyes" })
    );
    assert_eq!(api.calls("reactions.remove")[0].json()["name"], "eyes");
}

#[tokio::test]
async fn rate_limit_honours_retry_after_and_retries_once() {
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let api = MockHttp::start_sync(move |req| match req.method() {
        "chat.postMessage" => {
            if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                Resp {
                    status: 429,
                    headers: vec![
                        ("Retry-After".into(), "0".into()),
                        ("Content-Type".into(), "application/json".into()),
                    ],
                    body: br#"{"ok":false,"error":"ratelimited"}"#.to_vec(),
                }
            } else {
                ok(json!({ "ts": "6.6" }))
            }
        }
        "chat.update" => Resp {
            status: 429,
            headers: vec![("Retry-After".into(), "0".into())],
            body: br#"{"ok":false,"error":"ratelimited"}"#.to_vec(),
        },
        _ => default_api(req),
    })
    .await;
    let ch = channel(&api.base, &[]);
    let ts = ch.send(&SendMessage::new("hi", "C1")).await.unwrap();
    assert_eq!(ts.as_deref(), Some("6.6"));
    assert_eq!(hits.load(Ordering::SeqCst), 2);

    let err = ch.edit_message("C1", "6.6", "x").await.unwrap_err();
    assert_eq!(api_error_code(&err), Some("ratelimited"));
    assert_eq!(api.calls("chat.update").len(), 2, "exactly one retry");
}

#[tokio::test]
async fn attachments_upload_through_the_external_flow_after_the_text() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("report.txt");
    std::fs::write(&path, b"hello").unwrap();
    let base_cell: Arc<std::sync::OnceLock<String>> = Arc::default();
    let base_for_handler = base_cell.clone();
    let api = MockHttp::start_sync(move |req| match req.method() {
        "files.getUploadURLExternal" => ok(json!({
            "upload_url": format!("{}/upload/F1", base_for_handler.get().unwrap()),
            "file_id": "F1",
        })),
        "upload/F1" => Resp {
            status: 200,
            headers: vec![],
            body: b"OK - 5".to_vec(),
        },
        "files.completeUploadExternal" => ok(json!({ "files": [{ "id": "F1" }] })),
        _ => default_api(req),
    })
    .await;
    base_cell.set(api.base.clone()).unwrap();
    let ch = channel(&api.base, &[]);
    let message = SendMessage::new("see the report", "C1")
        .in_thread(Some("5.5".into()))
        .with_attachments(vec![OutboundFile {
            id: String::new(),
            size: 0,
            path: path.to_string_lossy().into_owned(),
            caption: Some("Q3 numbers".into()),
            kind: OutboundFileKind::Document,
        }]);
    let ts = ch.send(&message).await.unwrap();
    assert_eq!(ts.as_deref(), Some("1700000999.000100"));

    let order: Vec<String> = api
        .requests()
        .iter()
        .map(|r| r.method().to_string())
        .collect();
    assert_eq!(
        order,
        vec![
            "chat.postMessage",
            "files.getUploadURLExternal",
            "upload/F1",
            "files.completeUploadExternal"
        ]
    );
    let requests = api.requests();
    assert_eq!(requests[0].json()["thread_ts"], "5.5");
    let ticket = requests[1].form();
    assert_eq!(ticket["filename"], "report.txt");
    assert_eq!(ticket["length"], "5");
    assert_eq!(requests[2].body, b"hello");
    assert_eq!(
        requests[2].header("authorization"),
        None,
        "the pre-signed upload URL takes no token"
    );
    let complete = requests[3].form();
    assert_eq!(complete["channel_id"], "C1");
    assert_eq!(complete["thread_ts"], "5.5");
    assert_eq!(complete["initial_comment"], "Q3 numbers");
    let files: Value = serde_json::from_str(&complete["files"]).unwrap();
    assert_eq!(files, json!([{ "id": "F1", "title": "report.txt" }]));
}

#[tokio::test]
async fn health_check_follows_auth_test() {
    let good = MockHttp::start_sync(default_api).await;
    assert!(channel(&good.base, &[]).health_check().await);
    let bad = MockHttp::start_sync(|_| api_err("invalid_auth")).await;
    assert!(!channel(&bad.base, &[]).health_check().await);
}

// ─────────────────────────────────────────────────────────────────────────────
// `!` stands in for `/` (Slack swallows `/…`, and app slash commands cannot
// run in threads)
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a_bang_command_reaches_the_gateway_as_a_slash_command() {
    let as_text = |text: &str| {
        decode_message_event(&msg_event(ALLOWED, "C1", "channel", "5.5", text), &bot())
            .unwrap()
            .text
    };
    assert_eq!(as_text("!model"), "/model");
    assert_eq!(as_text("!status"), "/status");
    assert_eq!(as_text("!new codex"), "/new codex");
    assert_eq!(
        as_text(" /compact"),
        "/compact",
        "the leading-space form still works"
    );
    assert_eq!(as_text("!!! nice"), "!!! nice", "prose stays prose");
    assert_eq!(as_text("! ok"), "! ok");
    assert_eq!(
        as_text("hello !status"),
        "hello !status",
        "only at the start"
    );
}

#[test]
fn replies_name_ccteam_commands_with_the_bang_sigil() {
    let names: Vec<String> = ["status", "sessions", "use", "stop", "cd"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let rw = |t: &str| rewrite_command_sigils(t, &names);
    assert_eq!(
        rw("created s1\n↓ 查看状态 → /status"),
        "created s1\n↓ 查看状态 → !status"
    );
    assert_eq!(
        rw("无当前会话 —— /use <id> 选一个驱动(/sessions 看全部)"),
        "无当前会话 —— !use <id> 选一个驱动(!sessions 看全部)"
    );
    assert_eq!(rw("run `/stop s3` now"), "run `!stop s3` now");
    assert_eq!(
        rw("see /home/stop and /status.json"),
        "see /home/stop and /status.json"
    );
    assert_eq!(
        rw("/stopped /cd-x"),
        "/stopped /cd-x",
        "only whole command names"
    );
    assert_eq!(
        rw("/model and /compact"),
        "/model and /compact",
        "not ccteam commands"
    );
    assert_eq!(
        rw("```\n/status\n```\nthen /status."),
        "```\n/status\n```\nthen !status.",
        "fenced code is verbatim"
    );
}

#[tokio::test]
async fn sends_use_the_registered_command_names() {
    let api = MockHttp::start_sync(default_api).await;
    let ch = channel(&api.base, &[]);
    // Before the daemon registers the gateway's commands nothing is rewritten.
    ch.send(&SendMessage::new("→ /status", "C1")).await.unwrap();
    ch.register_commands(&[
        CommandSpec {
            name: "/status".into(),
            description: "fleet health".into(),
        },
        CommandSpec {
            name: "/sessions".into(),
            description: "list".into(),
        },
    ])
    .await
    .unwrap();
    ch.send(&SendMessage::new("→ /status · /sessions", "C1"))
        .await
        .unwrap();
    ch.send(&SendMessage::new("其他命令(如 /model /compact)", "C1"))
        .await
        .unwrap();
    ch.edit_message("C1", "9.9", "card → /status")
        .await
        .unwrap();
    let posts = api.calls("chat.postMessage");
    assert_eq!(posts[0].json()["text"], "→ /status");
    assert_eq!(posts[1].json()["text"], "→ !status · !sessions");
    assert_eq!(
        posts[2].json()["text"],
        "其他命令(如 !model !compact)",
        "the agent commands a reply names by example are typed with `!` too"
    );
    assert_eq!(api.calls("chat.update")[0].json()["text"], "card → !status");
}

/// Slack cannot size a button: main actions get the highlighted style, and a
/// minor one (interrupt) asks for confirmation before it acts.
#[test]
fn button_weight_maps_to_style_and_a_confirm_step() {
    let option = |data: &str, weight: OptionWeight| MessageOption {
        data: data.into(),
        label: format!("{data} label"),
        id: data.into(),
        weight,
    };
    let blocks = message_blocks(
        "",
        &[
            option("act:status", OptionWeight::Primary),
            option("act:sessions", OptionWeight::Normal),
            option("act:interrupt", OptionWeight::Minor),
        ],
    );
    let buttons = blocks[0]["elements"].as_array().unwrap();
    assert_eq!(buttons[0]["style"], "primary");
    assert!(buttons[0].get("confirm").is_none());
    assert!(buttons[1].get("style").is_none() && buttons[1].get("confirm").is_none());
    assert!(buttons[2].get("style").is_none());
    assert_eq!(buttons[2]["confirm"]["confirm"]["text"], "确定");
    assert_eq!(buttons[2]["confirm"]["deny"]["text"], "取消");
}
