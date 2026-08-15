use std::net::SocketAddr;

use ccteam_core::CcteamPaths;
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
