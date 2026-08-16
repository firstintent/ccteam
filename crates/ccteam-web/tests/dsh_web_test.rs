use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::response::IntoResponse;
use axum::routing::get;
use ccteam_core::CcteamPaths;
use ccteam_web::dsh_web::{DshWebRuntimeConfig, DshWebSupervisor};
use ccteam_web::{dsh_web, router_with_state, AppState, AuthState};
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
