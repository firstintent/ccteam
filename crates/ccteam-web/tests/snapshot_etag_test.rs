use std::io::Write;
use std::net::SocketAddr;
use std::time::Duration;

use ccteam_core::{bootstrap_project, disable_tool_surface_bootstrap_for_tests, CcteamPaths};
use ccteam_web::{router_with_state, AppState};
use reqwest::header::{ETAG, IF_NONE_MATCH};
use serde_json::{json, Value};
use tokio::net::TcpListener;

async fn spawn(state: AppState) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router_with_state(state))
            .await
            .unwrap();
    });
    tokio::task::yield_now().await;
    addr
}

fn append_event(paths: &CcteamPaths, slug: &str, event: Value) {
    let path = paths.progress_jsonl(slug);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    writeln!(file, "{}", serde_json::to_string(&event).unwrap()).unwrap();
}

async fn stable_snapshot(
    client: &reqwest::Client,
    url: &str,
) -> (String, Value, reqwest::StatusCode) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let response = client.get(url).send().await.unwrap();
            let status = response.status();
            let etag = response
                .headers()
                .get(ETAG)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            let body = response.json::<Value>().await.unwrap();
            if let Some(etag) = etag {
                return (etag, body, status);
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("projection hydration reaches a stable snapshot")
}

#[tokio::test]
async fn status_and_projects_etags_track_projection_ingest() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let ccteam_home = tmp.path().join("ccteam-home");
    let projects_root = tmp.path().join("projects");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&ccteam_home).unwrap();
    std::fs::create_dir_all(&projects_root).unwrap();
    std::env::set_var("HOME", &home);
    std::env::set_var("CCTEAM_HOME", &ccteam_home);

    let paths = CcteamPaths {
        root: ccteam_home,
        projects_root,
    };
    disable_tool_surface_bootstrap_for_tests();
    bootstrap_project(&paths, "demo", "snapshot etag test", "dev").unwrap();

    let state = AppState::new(paths.clone());
    assert_eq!(
        state.progress_projection.snapshot_version(),
        None,
        "warming projection must not publish a stable version"
    );
    let addr = spawn(state).await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let status_url = format!("http://{addr}/api/v1/status");
    let projects_url = format!("http://{addr}/api/v1/projects");

    let (status_etag, status_body, status_code) = stable_snapshot(&client, &status_url).await;
    assert_eq!(status_code, 200);
    assert_eq!(status_body["warming_up"], false);
    let status_version = status_body["version"].as_u64().unwrap();

    let (projects_etag, projects_body, projects_code) =
        stable_snapshot(&client, &projects_url).await;
    assert_eq!(projects_code, 200);
    let projects_version = projects_body[0]["version"].as_u64().unwrap();
    assert_eq!(projects_version, status_version);

    let status_not_modified = client
        .get(&status_url)
        .header(IF_NONE_MATCH, &status_etag)
        .send()
        .await
        .unwrap();
    assert_eq!(status_not_modified.status(), 304);
    assert_eq!(status_not_modified.headers()[ETAG], status_etag);
    assert!(status_not_modified.bytes().await.unwrap().is_empty());

    let projects_not_modified = client
        .get(&projects_url)
        .header(IF_NONE_MATCH, &projects_etag)
        .send()
        .await
        .unwrap();
    assert_eq!(projects_not_modified.status(), 304);
    assert_eq!(projects_not_modified.headers()[ETAG], projects_etag);
    assert!(projects_not_modified.bytes().await.unwrap().is_empty());

    append_event(
        &paths,
        "demo",
        json!({"event": "agent_done", "cost_usd": 1.0, "vendor": "claude"}),
    );

    let changed_status = client
        .get(&status_url)
        .header(IF_NONE_MATCH, &status_etag)
        .send()
        .await
        .unwrap();
    assert_eq!(changed_status.status(), 200);
    let changed_status_etag = changed_status.headers()[ETAG].to_str().unwrap().to_string();
    assert_ne!(changed_status_etag, status_etag);
    let changed_status_body = changed_status.json::<Value>().await.unwrap();
    let changed_version = changed_status_body["version"].as_u64().unwrap();
    assert!(changed_version > status_version);

    let changed_projects = client
        .get(&projects_url)
        .header(IF_NONE_MATCH, &projects_etag)
        .send()
        .await
        .unwrap();
    assert_eq!(changed_projects.status(), 200);
    assert_ne!(changed_projects.headers()[ETAG], projects_etag);
    let changed_projects_body = changed_projects.json::<Value>().await.unwrap();
    assert_eq!(changed_projects_body[0]["version"], changed_version);
}
