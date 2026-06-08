//! v0.8.9 Phase 2 — marketplace REST route integration tests.
//!
//! Drives the FOUR `/api/v1` marketplace routes through the real
//! `stateful_router` against an in-process fake hub (a looping `TcpListener`
//! standing in for `raw.githubusercontent.com`). The `CCTEAM_HUB_BASE` env
//! override repoints `ccteam_im::hub::hub_base()` at the fake, and a
//! tempdir-backed `CcteamPaths` sandboxes the `~/.ccteam/hub-cache/` so the
//! catalog cache never escapes the test. (This file lives in `tests/` — a
//! separate process — because it mutates `CCTEAM_HUB_BASE`, per CLAUDE.md's
//! env-mutating-tests-go-in-integration rule.)
//!
//! The headline case `marketplace_round_trip_catalog_decorate_install` walks:
//!   GET /marketplace               → catalog carries the entry
//!   GET /projects/{slug}/marketplace → installed_status = not_installed
//!   POST .../marketplace/install   → 201, `.claude/agents/<id>.md` on disk
//!   GET /projects/{slug}/marketplace → installed_status = installed
//!
//! plus unknown-id (404) + unknown-project (404) + body-preview (200) edges.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};

use ccteam_core::{bootstrap_project, disable_tool_surface_bootstrap_for_tests, CcteamPaths};
use ccteam_web::{router_with_state, AppState};
use serial_test::serial;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const HUB_BASE_ENV: &str = "CCTEAM_HUB_BASE";

const AGENT_BODY: &str =
    "---\nname: helper\ndescription: A curated helper\n---\nYou are a helpful agent.\n";

fn sha256_hex(bytes: &[u8]) -> String {
    let d = Sha256::digest(bytes);
    let mut s = String::with_capacity(d.len() * 2);
    for b in d {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn fake_paths(root: &std::path::Path) -> CcteamPaths {
    CcteamPaths {
        root: root.join(".ccteam"),
        projects_root: root.join("projects"),
    }
}

fn fixture_project(paths: &CcteamPaths, slug: &str) {
    disable_tool_surface_bootstrap_for_tests();
    bootstrap_project(paths, slug, "marketplace test", "dev").unwrap();
}

/// Build an `index.json` with a single agent plugin whose `content_sha` is the
/// real sha256 of `AGENT_BODY` (so the integrity check passes on install).
fn good_index_json(id: &str) -> String {
    let sha = sha256_hex(AGENT_BODY.as_bytes());
    format!(
        r#"{{
          "version": 1,
          "name": "ccteam-hub",
          "description": "curated",
          "generated_at": "2026-01-01T00:00:00Z",
          "plugins": [
            {{
              "id": "{id}",
              "type": "agent",
              "name": "Helper",
              "description": "A curated helper",
              "path": "agents/{id}.md",
              "content_sha": "{sha}",
              "source": "agency-agents",
              "upstream": "https://github.com/example/x",
              "license": "MIT",
              "tags": ["util"]
            }}
          ]
        }}"#
    )
}

/// A looping multi-route fake hub: serves `/index.json` and a per-path body
/// map for EVERY connection until the listener is dropped at test end (the
/// real router makes a varying number of fetches across the round-trip — index
/// refresh + cached reads + a body fetch on install). Routes on the request
/// path; unknown paths get a 404. Returns the `base` URL.
fn spawn_looping_hub(index_json: String, bodies: Vec<(String, String)>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || loop {
        let Ok((mut stream, _)) = listener.accept() else {
            break;
        };
        let mut buf = [0u8; 8192];
        let n = stream.read(&mut buf).unwrap_or(0);
        let req = String::from_utf8_lossy(&buf[..n]);
        // First line: "GET /path HTTP/1.1"
        let path = req
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .unwrap_or("/")
            .to_string();
        let body: Option<&str> = if path == "/index.json" {
            Some(index_json.as_str())
        } else {
            bodies
                .iter()
                .find(|(p, _)| format!("/{p}") == path)
                .map(|(_, b)| b.as_str())
        };
        let resp = match body {
            Some(b) => format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                b.len(),
                b
            ),
            None => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .to_string(),
        };
        let _ = stream.write_all(resp.as_bytes());
        let _ = stream.flush();
    });
    format!("http://{addr}")
}

async fn spawn_router(state: AppState) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router_with_state(state);
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    tokio::task::yield_now().await;
    addr
}

/// The full round-trip through the real router: global catalog → decorated
/// (not_installed) → install (201) → decorated (installed) + file on disk.
#[tokio::test]
#[serial]
async fn marketplace_round_trip_catalog_decorate_install() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    fixture_project(&paths, "demo");
    let project_dir = paths.project_dir("demo");

    let id = "helper";
    let hub_base = spawn_looping_hub(
        good_index_json(id),
        vec![(format!("agents/{id}.md"), AGENT_BODY.to_string())],
    );
    std::env::set_var(HUB_BASE_ENV, &hub_base);

    let addr = spawn_router(AppState::new(paths)).await;
    let client = reqwest::Client::new();

    // 1. Global catalog — force a refresh so the fake hub populates the
    //    tempdir-backed cache. The entry must be present.
    let resp = client
        .get(format!("http://{addr}/api/v1/marketplace?refresh=true"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "global catalog");
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["name"], "ccteam-hub");
    let plugins = v["plugins"].as_array().unwrap();
    assert_eq!(plugins.len(), 1);
    assert_eq!(plugins[0]["id"], id);
    assert_eq!(plugins[0]["type"], "agent");

    // 2. Per-project decorated catalog — before install, status = not_installed.
    let resp = client
        .get(format!("http://{addr}/api/v1/projects/demo/marketplace"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "decorated catalog (pre-install)");
    let v: serde_json::Value = resp.json().await.unwrap();
    let plugins = v["plugins"].as_array().unwrap();
    assert_eq!(plugins.len(), 1);
    assert_eq!(
        plugins[0]["installed_status"], "not_installed",
        "fresh project must show not_installed; got {plugins:#?}"
    );
    // The decorated entry still carries the plugin fields.
    assert_eq!(plugins[0]["id"], id);
    assert_eq!(plugins[0]["license"], "MIT");

    // 3. Install into the project → 201 + the agent .md on disk.
    let resp = client
        .post(format!(
            "http://{addr}/api/v1/projects/demo/marketplace/install"
        ))
        .json(&serde_json::json!({ "id": id }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "install");
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["id"], id);
    assert_eq!(v["type"], "agent");
    assert_eq!(v["overwrote"], false);
    let dest = project_dir.join(".claude/agents/helper.md");
    assert!(dest.exists(), "install must write the agent .md");
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), AGENT_BODY);
    assert_eq!(
        v["path"].as_str().unwrap(),
        dest.display().to_string(),
        "install result path"
    );

    // 4. Decorated catalog again — now status = installed.
    let resp = client
        .get(format!("http://{addr}/api/v1/projects/demo/marketplace"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "decorated catalog (post-install)");
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        v["plugins"][0]["installed_status"], "installed",
        "after install the entry must show installed; got {:#?}",
        v["plugins"]
    );

    std::env::remove_var(HUB_BASE_ENV);
}

/// Body preview for review: GET /marketplace/{id}/body returns the verified
/// markdown; an unknown id is a 404.
#[tokio::test]
#[serial]
async fn marketplace_body_preview_and_unknown_id() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());

    let id = "helper";
    let hub_base = spawn_looping_hub(
        good_index_json(id),
        vec![(format!("agents/{id}.md"), AGENT_BODY.to_string())],
    );
    std::env::set_var(HUB_BASE_ENV, &hub_base);

    let addr = spawn_router(AppState::new(paths)).await;
    let client = reqwest::Client::new();

    // Prime the cache so the body handler can resolve the id (it reads the
    // cached index, no forced refresh).
    let _ = client
        .get(format!("http://{addr}/api/v1/marketplace?refresh=true"))
        .send()
        .await
        .unwrap();

    // Known id → 200 with the verified body.
    let resp = client
        .get(format!("http://{addr}/api/v1/marketplace/{id}/body"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "body preview");
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["id"], id);
    assert_eq!(v["body"], AGENT_BODY);

    // Unknown id → 404.
    let resp = client
        .get(format!(
            "http://{addr}/api/v1/marketplace/no-such-plugin/body"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404, "unknown plugin id");

    std::env::remove_var(HUB_BASE_ENV);
}

/// Unknown project → 404 on both the decorated GET and the install POST
/// (the `reject_unknown_project` short-circuit runs before any hub I/O).
#[tokio::test]
#[serial]
async fn marketplace_unknown_project_404() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());

    // A fake hub exists but should never be consulted for an unknown project.
    let hub_base = spawn_looping_hub(good_index_json("helper"), vec![]);
    std::env::set_var(HUB_BASE_ENV, &hub_base);

    let addr = spawn_router(AppState::new(paths)).await;
    let client = reqwest::Client::new();

    let get = client
        .get(format!("http://{addr}/api/v1/projects/ghost/marketplace"))
        .send()
        .await
        .unwrap();
    assert_eq!(get.status(), 404, "decorated catalog on unknown project");

    let post = client
        .post(format!(
            "http://{addr}/api/v1/projects/ghost/marketplace/install"
        ))
        .json(&serde_json::json!({ "id": "helper" }))
        .send()
        .await
        .unwrap();
    assert_eq!(post.status(), 404, "install on unknown project");

    std::env::remove_var(HUB_BASE_ENV);
}

/// Install with an id that isn't in the catalog → 404 (resolved before any
/// install I/O), and no file is written.
#[tokio::test]
#[serial]
async fn marketplace_install_unknown_plugin_404() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    fixture_project(&paths, "demo");
    let project_dir = paths.project_dir("demo");

    let hub_base = spawn_looping_hub(good_index_json("helper"), vec![]);
    std::env::set_var(HUB_BASE_ENV, &hub_base);

    let addr = spawn_router(AppState::new(paths)).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!(
            "http://{addr}/api/v1/projects/demo/marketplace/install"
        ))
        .json(&serde_json::json!({ "id": "nope" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404, "unknown plugin id on install");
    assert!(
        !project_dir.join(".claude/agents/nope.md").exists(),
        "nothing should be written for an unknown plugin"
    );

    std::env::remove_var(HUB_BASE_ENV);
}
