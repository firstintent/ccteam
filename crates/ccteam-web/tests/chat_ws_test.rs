//! Web chat WebSocket route tests.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use ccteam_core::CcteamPaths;
use ccteam_harness::execution::turns_mirror::{AttachmentRef, AttachmentRefKind};
use ccteam_web::chat_protocol::{ClientChatFrame, ServerChatFrame, WebSendMessage, SUBPROTOCOL};
use ccteam_web::{router_with_state, AppState, AuthState};
use futures_util::{SinkExt, StreamExt};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc, Mutex};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::client::Request as ClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderValue, StatusCode};
use tokio_tungstenite::tungstenite::Message;

const TOKEN_HEX: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

fn fake_paths(root: &Path) -> CcteamPaths {
    CcteamPaths {
        root: root.join(".ccteam"),
        projects_root: root.join("projects"),
    }
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
    req.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        HeaderValue::from_static(SUBPROTOCOL),
    );
    req
}

async fn recv_server_frame<S>(ws: &mut S) -> ServerChatFrame
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    let frame = tokio::time::timeout(std::time::Duration::from_secs(2), ws.next())
        .await
        .expect("timed out waiting for chat ws frame")
        .expect("socket closed before chat ws frame")
        .expect("chat ws frame error");
    let Message::Text(text) = frame else {
        panic!("expected text frame, got {frame:?}");
    };
    serde_json::from_str(&text).unwrap()
}

#[tokio::test]
async fn chat_ws_without_auth_rejects_with_401_when_auth_enabled() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let state = AppState::with_auth(paths, AuthState::enabled(TOKEN_HEX.into()));
    let addr = spawn(state).await;

    let req = ws_request_with_subprotocol(addr, "/ws/chat");
    let err = tokio_tungstenite::connect_async(req).await.unwrap_err();
    match err {
        tokio_tungstenite::tungstenite::Error::Http(resp) => {
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        }
        other => panic!("expected HTTP 401 error, got {other:?}"),
    }
}

#[tokio::test]
async fn chat_ws_echoes_subprotocol_and_sends_initial_sessions() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let state = AppState::with_auth(paths, AuthState::disabled());
    let addr = spawn(state).await;

    let req = ws_request_with_subprotocol(addr, "/ws/chat");
    let (mut ws, resp) = tokio_tungstenite::connect_async(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::SWITCHING_PROTOCOLS);
    let echoed = resp
        .headers()
        .get("sec-websocket-protocol")
        .and_then(|h| h.to_str().ok());
    assert_eq!(echoed, Some(SUBPROTOCOL));

    let frame = recv_server_frame(&mut ws).await;
    assert_eq!(frame, ServerChatFrame::Sessions { items: Vec::new() });
    let _ = ws.close(None).await;
}

#[tokio::test]
async fn chat_ws_forwards_text_to_inbound_and_renders_outbound_reply() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let (in_tx, mut in_rx) = mpsc::channel(8);
    let (out_tx, _) = broadcast::channel(8);
    let backlog = Arc::new(Mutex::new(Vec::new()));
    let state = AppState::with_auth(paths, AuthState::disabled()).with_chat_bridge(
        in_tx,
        out_tx.clone(),
        backlog.clone(),
        Arc::new(Mutex::new(HashMap::new())),
    );
    let addr = spawn(state).await;

    let req = ws_request_with_subprotocol(addr, "/ws/chat?chat_id=chat-1&user_id=alice");
    let (mut ws, _resp) = tokio_tungstenite::connect_async(req).await.unwrap();
    let _ = recv_server_frame(&mut ws).await;

    let frame = ClientChatFrame::Text {
        content: "hello".into(),
        id: Some("client-1".into()),
    };
    ws.send(Message::Text(serde_json::to_string(&frame).unwrap()))
        .await
        .unwrap();

    let inbound = tokio::time::timeout(std::time::Duration::from_secs(2), in_rx.recv())
        .await
        .expect("timed out waiting for inbound web message")
        .expect("inbound channel closed");
    assert_eq!(inbound.id, "client-1");
    assert_eq!(inbound.sender, "alice");
    assert_eq!(inbound.reply_target, "chat-1");
    assert_eq!(inbound.channel, "web");
    assert_eq!(inbound.content, "hello");

    let mut outbound = WebSendMessage::new("hi from gateway", "chat-1");
    outbound.attachments.push(AttachmentRef {
        id: "1780000000000-chart.png".into(),
        name: "chart.png".into(),
        kind: AttachmentRefKind::Image,
        size: 42,
    });
    backlog.lock().await.push(outbound.clone());
    out_tx.send(outbound).unwrap();
    let reply = recv_server_frame(&mut ws).await;
    assert_eq!(
        reply,
        ServerChatFrame::Reply {
            content: "hi from gateway".into(),
            attachments: vec![AttachmentRef {
                id: "1780000000000-chart.png".into(),
                name: "chart.png".into(),
                kind: AttachmentRefKind::Image,
                size: 42,
            }],
        }
    );
    let _ = ws.close(None).await;
}

#[tokio::test]
async fn chat_ws_switch_emits_cd_then_use_and_refreshes_sessions() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let (in_tx, mut in_rx) = mpsc::channel(8);
    let (out_tx, _) = broadcast::channel(8);
    let state = AppState::with_auth(paths, AuthState::disabled()).with_chat_bridge(
        in_tx,
        out_tx.clone(),
        Arc::new(Mutex::new(Vec::new())),
        Arc::new(Mutex::new(HashMap::new())),
    );
    let addr = spawn(state).await;

    let req = ws_request_with_subprotocol(addr, "/ws/chat?chat_id=chat-1&user_id=alice");
    let (mut ws, _resp) = tokio_tungstenite::connect_async(req).await.unwrap();
    let _ = recv_server_frame(&mut ws).await;

    let frame = ClientChatFrame::Switch {
        project: Some("alpha".into()),
        session: Some("s1".into()),
    };
    ws.send(Message::Text(serde_json::to_string(&frame).unwrap()))
        .await
        .unwrap();

    let first = tokio::time::timeout(std::time::Duration::from_secs(2), in_rx.recv())
        .await
        .expect("timed out waiting for /cd")
        .expect("inbound channel closed");
    let second = tokio::time::timeout(std::time::Duration::from_secs(2), in_rx.recv())
        .await
        .expect("timed out waiting for /use")
        .expect("inbound channel closed");
    assert_eq!(first.content, "/cd alpha");
    assert_eq!(second.content, "/use s1");
    assert_eq!(first.reply_target, "chat-1");
    assert_eq!(second.reply_target, "chat-1");

    let refresh = recv_server_frame(&mut ws).await;
    assert_eq!(refresh, ServerChatFrame::Sessions { items: Vec::new() });
    let _ = ws.close(None).await;
}

#[tokio::test]
async fn chat_ws_outbound_replies_are_scoped_by_chat_id() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let (in_tx, _in_rx) = mpsc::channel(8);
    let (out_tx, _) = broadcast::channel(8);
    let backlog = Arc::new(Mutex::new(Vec::new()));
    let state = AppState::with_auth(paths, AuthState::disabled()).with_chat_bridge(
        in_tx,
        out_tx.clone(),
        backlog.clone(),
        Arc::new(Mutex::new(HashMap::new())),
    );
    let addr = spawn(state).await;

    let req_one = ws_request_with_subprotocol(addr, "/ws/chat?chat_id=chat-1&user_id=alice");
    let req_two = ws_request_with_subprotocol(addr, "/ws/chat?chat_id=chat-2&user_id=bob");
    let (mut ws_one, _resp) = tokio_tungstenite::connect_async(req_one).await.unwrap();
    let (mut ws_two, _resp) = tokio_tungstenite::connect_async(req_two).await.unwrap();
    let _ = recv_server_frame(&mut ws_one).await;
    let _ = recv_server_frame(&mut ws_two).await;

    let outbound = WebSendMessage::new("only chat two", "chat-2");
    backlog.lock().await.push(outbound.clone());
    out_tx.send(outbound).unwrap();
    let reply = recv_server_frame(&mut ws_two).await;
    assert_eq!(
        reply,
        ServerChatFrame::Reply {
            content: "only chat two".into(),
            attachments: Vec::new(),
        }
    );

    let no_reply = tokio::time::timeout(std::time::Duration::from_millis(150), ws_one.next()).await;
    assert!(
        no_reply.is_err(),
        "chat-1 must not receive chat-2 outbound messages"
    );
    let _ = ws_one.close(None).await;
    let _ = ws_two.close(None).await;
}

/// CROSS-USER (2026-07-28 owner report) — with auth ON the socket binds to the AUTHENTICATED
/// identity; `?chat_id=` is a client-supplied label and must not choose it.
/// The regression: a tenant could connect as `chat_id=web-api` and receive
/// (and drive) the admin console's chat, because `chat_id` keyed both the
/// gateway `ChatKey` and the outbound delivery filter. The project picker is
/// filtered by the same ownership policy as `GET /api/v1/projects`.
#[tokio::test]
async fn chat_ws_binds_to_the_authenticated_identity_not_the_query() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(&paths.root).unwrap();

    // A registered tenant, plus one admin-owned and one tenant-owned project.
    let mut reg = ccteam_core::tenants::TenantRegistry::default();
    let tenant = reg.add("alice");
    reg.save(&paths.users_dir()).unwrap();
    let tenant_tok = tenant.web_token.clone();
    let tenant_id = tenant.id.clone();
    for (slug, owner) in [
        ("adminproj", "user:web-api".to_string()),
        ("aliceproj", format!("user:{tenant_id}")),
    ] {
        let path = paths.project_state(slug);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut st = ccteam_core::ProjectState::initial_for_team(slug.into(), "dev".into());
        st.owner = Some(owner);
        st.save(&path).unwrap();
    }

    let (in_tx, _in_rx) = mpsc::channel(8);
    let (out_tx, _) = broadcast::channel(8);
    let backlog = Arc::new(Mutex::new(Vec::new()));
    let state = AppState::with_auth(paths, AuthState::enabled(TOKEN_HEX.into())).with_chat_bridge(
        in_tx,
        out_tx.clone(),
        backlog.clone(),
        Arc::new(Mutex::new(HashMap::new())),
    );
    let addr = spawn(state).await;

    // The tenant ASKS for the admin console's chat id …
    let mut req = ws_request_with_subprotocol(addr, "/ws/chat?chat_id=web-api&user_id=web-api");
    req.headers_mut().insert(
        "Authorization",
        HeaderValue::from_str(&format!("Bearer ccteam:{tenant_tok}")).unwrap(),
    );
    let (mut ws, _resp) = tokio_tungstenite::connect_async(req).await.unwrap();

    // … and gets its OWN view: only the project it owns.
    let frame = recv_server_frame(&mut ws).await;
    let ServerChatFrame::Sessions { items } = frame else {
        panic!("expected the initial Sessions frame");
    };
    let projects: Vec<&str> = items.iter().map(|i| i.project.as_str()).collect();
    assert_eq!(
        projects,
        vec!["aliceproj"],
        "the picker must not leak another user's projects"
    );

    // … and is NOT wired to the admin console's delivery target.
    let outbound = WebSendMessage::new("admin console only", "web-api");
    backlog.lock().await.push(outbound.clone());
    out_tx.send(outbound).unwrap();
    let leaked = tokio::time::timeout(std::time::Duration::from_millis(150), ws.next()).await;
    assert!(
        leaked.is_err(),
        "a tenant socket must not receive the admin console's messages"
    );
    let _ = ws.close(None).await;
}
