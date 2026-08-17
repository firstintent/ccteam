use std::io::Read;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message as AxMessage, WebSocketUpgrade};
use axum::http::{header, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use ccteam_core::CcteamPaths;
use ccteam_web::dsh_web::{DshWebRuntimeConfig, DshWebSupervisor};
use ccteam_web::{dsh_web, router_with_state, AppState, AuthState};
use flate2::read::GzDecoder;
use futures_util::{SinkExt, StreamExt};
use tempfile::TempDir;
use tokio::net::TcpListener;

const TOKEN_HEX: &str = "deadbeefcafef00ddeadbeefcafef00d";

fn fake_paths(root: &std::path::Path) -> CcteamPaths {
    CcteamPaths {
        root: root.join(".ccteam"),
        projects_root: root.join("projects"),
    }
}

async fn spawn_app(app: axum::Router) -> SocketAddr {
    std::env::set_var("NO_PROXY", "127.0.0.1,localhost,::1");
    std::env::set_var("no_proxy", "127.0.0.1,localhost,::1");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    tokio::task::yield_now().await;
    addr
}

#[tokio::test]
async fn companion_rejects_anonymous_and_query_token_requests() {
    let tmp = TempDir::new().unwrap();
    let state = AppState::with_auth(fake_paths(tmp.path()), AuthState::enabled(TOKEN_HEX.into()));
    let addr = spawn_app(dsh_web::companion_router().with_state(state)).await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    let resp = client.get(format!("http://{addr}/")).send().await.unwrap();
    assert_eq!(resp.status(), 401);

    let resp = client
        .get(format!("http://{addr}/?token=ccteam:{TOKEN_HEX}"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        401,
        "companion must not honor URL token shim"
    );
}

#[tokio::test]
async fn disabled_companion_reports_machine_readable_upstream_error() {
    let tmp = TempDir::new().unwrap();
    let state = AppState::with_auth(fake_paths(tmp.path()), AuthState::disabled());
    let addr = spawn_app(dsh_web::companion_router().with_state(state)).await;
    let response = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .get(format!("http://{addr}/"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body: serde_json::Value = response.json().await.unwrap();
    assert!(body["error"]
        .as_str()
        .is_some_and(|error| !error.is_empty()));
    assert_eq!(body["error_code"], "dsh_upstream_unready");
}

#[tokio::test]
async fn dsh_status_reports_disabled_shape_when_companion_is_off() {
    let tmp = TempDir::new().unwrap();
    let state = AppState::with_auth(fake_paths(tmp.path()), AuthState::disabled());
    let addr = spawn_app(router_with_state(state)).await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    let resp = client
        .get(format!("http://{addr}/api/v1/dsh/status"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let json: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(json["state"], "disabled");
    assert!(json.get("companion_port").is_none());
    assert_eq!(json["home_kind"], "own");
}

/// A fake "already-running dsh web" origin — just enough for
/// `probe_attached_dsh`'s heuristic (any 2xx body containing "dsh") to
/// attach without spawning a real process.
async fn spawn_fake_attached_dsh() -> SocketAddr {
    let app =
        axum::Router::new().route("/", get(|| async { "dsh web fake origin".into_response() }));
    spawn_app(app).await
}

async fn spawn_fake_dsh_origin() -> SocketAddr {
    let javascript = "window.__DSH_ASSET__ = true;".repeat(512);
    let app = axum::Router::new()
        .route(
            "/",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                    "<!doctype html><html><head><script src=\"/client.js\"></script></head><body>dsh web</body></html>",
                )
            }),
        )
        .route(
            "/client.js",
            get(move || {
                let javascript = javascript.clone();
                async move {
                    (
                        [(header::CONTENT_TYPE, "application/javascript")],
                        javascript,
                    )
                }
            }),
        )
        .route("/api/events.mux", get(echo_websocket))
        .route("/api/events.host", get(echo_websocket));
    spawn_app(app).await
}

async fn echo_websocket(ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(|mut socket| async move {
        while let Some(Ok(message)) = socket.next().await {
            let should_close = matches!(message, AxMessage::Close(_));
            if socket.send(message).await.is_err() || should_close {
                break;
            }
        }
    })
}

fn gunzip(bytes: &[u8]) -> String {
    let mut decoder = GzDecoder::new(bytes);
    let mut decoded = String::new();
    decoder.read_to_string(&mut decoded).unwrap();
    decoded
}

async fn spawn_companion_for_origin(origin: SocketAddr) -> (SocketAddr, TempDir) {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let supervisor = Arc::new(DshWebSupervisor::new(DshWebRuntimeConfig {
        enabled: true,
        daemon_url: "http://127.0.0.1:7331".to_string(),
        attach_url: Some(format!("http://{origin}")),
    }));
    let state = AppState::with_auth(paths, AuthState::disabled()).with_dsh_web(supervisor);
    (
        spawn_app(dsh_web::companion_router().with_state(state)).await,
        tmp,
    )
}

#[tokio::test]
async fn companion_compresses_assets_after_splicing_html() {
    let origin = spawn_fake_dsh_origin().await;
    let (companion, _tmp) = spawn_companion_for_origin(origin).await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    let asset = client
        .get(format!("http://{companion}/client.js"))
        .header(header::ACCEPT_ENCODING, "gzip")
        .send()
        .await
        .unwrap();
    assert_eq!(asset.status(), StatusCode::OK);
    assert_eq!(asset.headers()[header::CONTENT_ENCODING], "gzip");
    let encoded_asset = asset.bytes().await.unwrap();
    let decoded_asset = gunzip(&encoded_asset);
    assert!(decoded_asset.contains("window.__DSH_ASSET__"));
    assert!(encoded_asset.len() < decoded_asset.len());

    let html = client
        .get(format!("http://{companion}/"))
        .header(header::ACCEPT_ENCODING, "gzip")
        .send()
        .await
        .unwrap();
    assert_eq!(html.status(), StatusCode::OK);
    assert_eq!(html.headers()[header::CONTENT_ENCODING], "gzip");
    let decoded_html = gunzip(&html.bytes().await.unwrap());
    assert!(decoded_html.contains("randomUUID"));
    assert!(decoded_html.contains("<body>dsh web</body>"));
}

#[tokio::test]
async fn companion_websocket_downlinks_upgrade_and_carry_frames_with_compression_enabled() {
    let origin = spawn_fake_dsh_origin().await;
    let (companion, _tmp) = spawn_companion_for_origin(origin).await;

    for path in ["/api/events.mux", "/api/events.host"] {
        let request = Request::builder()
            .uri(format!("ws://{companion}{path}"))
            .header(header::ACCEPT_ENCODING, "gzip")
            .header(header::HOST, companion.to_string())
            .header(header::CONNECTION, "Upgrade")
            .header(header::UPGRADE, "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ==")
            .body(())
            .unwrap();
        let (mut socket, response) = tokio_tungstenite::connect_async(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
        assert!(response.headers().get(header::CONTENT_ENCODING).is_none());

        let payload = format!("frame-through-{path}");
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                payload.clone(),
            ))
            .await
            .unwrap();
        let echoed = tokio::time::timeout(Duration::from_secs(3), socket.next())
            .await
            .expect("proxied websocket frame timed out")
            .expect("proxied websocket closed")
            .expect("proxied websocket returned an error");
        assert_eq!(echoed.into_text().unwrap(), payload);
        socket.close(None).await.unwrap();
    }
}

#[tokio::test]
async fn main_spa_javascript_is_compressed_for_the_browser_hop() {
    let tmp = TempDir::new().unwrap();
    let state = AppState::with_auth(fake_paths(tmp.path()), AuthState::disabled());
    let addr = spawn_app(router_with_state(state)).await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    let index = client
        .get(format!("http://{addr}/app/"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let marker = "src=\"/app/";
    let start = index.find(marker).expect("SPA script src") + marker.len();
    let end = index[start..]
        .find('"')
        .expect("SPA script src closing quote")
        + start;
    let script_path = &index[start..end];

    let script = client
        .get(format!("http://{addr}/app/{script_path}"))
        .header(header::ACCEPT_ENCODING, "gzip")
        .send()
        .await
        .unwrap();
    assert_eq!(script.status(), StatusCode::OK);
    assert_eq!(script.headers()[header::CONTENT_ENCODING], "gzip");
    let compressed = script.bytes().await.unwrap();
    let decoded = gunzip(&compressed);
    assert!(
        decoded.len() > 100_000,
        "expected the real built SPA bundle"
    );
    assert!(compressed.len() < decoded.len());
}

/// Regression for a real production hang: `start_for`'s "already live" fast
/// path used to `status_for(identity).await` while STILL holding the
/// `instances` MutexGuard from the state check — `tokio::sync::Mutex` is not
/// reentrant, so that task deadlocked on a lock only it held. Because the
/// lock was never released, every LATER caller of `self.instances.lock()` —
/// every proxied request AND every `/status` poll — hung forever too (a
/// single stuck DSH-page click wedged the whole daemon). Two `start` calls
/// back to back (the second one must take the already-live fast path) plus a
/// `/status` poll afterward, all under a timeout, is the deterministic
/// reproduction: pre-fix this test hangs past the timeout instead of
/// asserting; post-fix every call returns in milliseconds.
#[tokio::test]
async fn repeated_start_on_an_already_attached_instance_never_deadlocks() {
    let attach_addr = spawn_fake_attached_dsh().await;
    let tmp = TempDir::new().unwrap();
    let supervisor = Arc::new(DshWebSupervisor::new(DshWebRuntimeConfig {
        enabled: true,
        daemon_url: "http://127.0.0.1:7331".to_string(),
        attach_url: Some(format!("http://{attach_addr}")),
    }));
    let state =
        AppState::with_auth(fake_paths(tmp.path()), AuthState::disabled()).with_dsh_web(supervisor);
    let addr = spawn_app(router_with_state(state)).await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let start_url = format!("http://{addr}/api/v1/dsh/start");
    let status_url = format!("http://{addr}/api/v1/dsh/status");
    let guard = Duration::from_secs(5);

    // First start attaches to the fake origin.
    let first = tokio::time::timeout(guard, client.post(&start_url).send())
        .await
        .expect("first start must not hang")
        .unwrap();
    assert_eq!(first.status(), 200);
    let json: serde_json::Value = first.json().await.unwrap();
    assert_eq!(json["state"], "attached");

    // Second start hits the "already live" fast path — this is exactly the
    // call that used to self-deadlock.
    let second = tokio::time::timeout(guard, client.post(&start_url).send())
        .await
        .expect(
            "second start on an already-attached instance must not hang (self-deadlock regression)",
        )
        .unwrap();
    assert_eq!(second.status(), 200);
    let json: serde_json::Value = second.json().await.unwrap();
    assert_eq!(json["state"], "attached");

    // The whole daemon must still be responsive — the defining symptom of
    // the original bug was that an unrelated /status poll hung too once the
    // mutex was stuck locked forever.
    let status = tokio::time::timeout(guard, client.get(&status_url).send())
        .await
        .expect("status must not hang after a repeated start (mutex must not be stuck locked)")
        .unwrap();
    assert_eq!(status.status(), 200);
}

/// A cancelled `/start` (browser abort, iframe timeout, companion-port
/// retry) used to drop the in-request spawn future. `kill_on_drop` then
/// killed the child, the map stayed at `Starting`, and every later start
/// took the already-live path — the page spun on "Starting the DSH web
/// instance…" forever. Start work must outlive the HTTP task.
#[tokio::test]
async fn cancelled_start_request_still_reaches_attached() {
    let attach_addr = spawn_slow_attached_dsh(Duration::from_millis(400)).await;
    let tmp = TempDir::new().unwrap();
    let supervisor = Arc::new(DshWebSupervisor::new(DshWebRuntimeConfig {
        enabled: true,
        daemon_url: "http://127.0.0.1:7331".to_string(),
        attach_url: Some(format!("http://{attach_addr}")),
    }));
    let state =
        AppState::with_auth(fake_paths(tmp.path()), AuthState::disabled()).with_dsh_web(supervisor);
    let addr = spawn_app(router_with_state(state)).await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let start_url = format!("http://{addr}/api/v1/dsh/start");
    let status_url = format!("http://{addr}/api/v1/dsh/status");

    let aborted =
        tokio::time::timeout(Duration::from_millis(80), client.post(&start_url).send()).await;
    assert!(
        aborted.is_err(),
        "the first start must still be inside the delayed attach probe"
    );

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut last = serde_json::json!(null);
    while tokio::time::Instant::now() < deadline {
        let resp = client.get(&status_url).send().await.unwrap();
        assert_eq!(resp.status(), 200);
        last = resp.json().await.unwrap();
        if last["state"] == "attached" {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("cancelled start left DSH web stuck at {last}");
}

async fn spawn_slow_attached_dsh(delay: Duration) -> SocketAddr {
    let app = axum::Router::new().route(
        "/",
        get(move || async move {
            tokio::time::sleep(delay).await;
            "dsh web fake origin".into_response()
        }),
    );
    spawn_app(app).await
}
