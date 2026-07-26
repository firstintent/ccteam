//! V0.3 M5.4 — End-to-end happy-path canary.
//!
//! Each milestone (M5.1 dashboard / M5.2 pane snapshot / M5.3 write actions)
//! ships its own focused integration suite. This test exists to catch the
//! *cross-layer regression* the per-milestone tests miss: the same fixture
//! project must survive the F59 legacy redirects, the F52 JSON data API used
//! by the SPA, and a write-action POST in one sequenced run, with the disk
//! side-effect of the POST observable afterwards.
//!
//! The brief (`docs/versions/v0-3/dev-plan.md` §6 #5.1) explicitly orders the
//! sequence:
//!
//!   GET /                  → 301 /app/
//!   GET /api/v1/projects   → JSON dashboard data mentions slug
//!   GET /project/<slug>    → 301 /app/p/<slug>
//!   GET /api/v1/projects/<slug> → JSON detail data mentions phase
//!   POST /api/<slug>/btw   → 303, inbox file lands on disk
//!
//! v0.9.0 W4 — the original sequence's SSE step (`open /sse/project/<slug>,
//! append progress.jsonl line, observe SSE`) is dropped: `routes::sse` is
//! deleted (zero SPA consumers; see `crate::routes::agents` +
//! `routes::sessions_api` for its gateway-broadcast-backed successors, each
//! with their own focused integration coverage).
//!
//! Auth stays disabled (loopback default per PRD §6.2.4) so this test
//! exercises the routing + handler stack only; auth is covered in
//! `auth_test.rs`.

use std::fs;
use std::net::SocketAddr;
use std::time::Duration;

use ccteam_core::{
    bootstrap_project, disable_tool_surface_bootstrap_for_tests, CcteamPaths, ProjectState,
};
use ccteam_web::{router_with_state, AppState};
use reqwest::redirect::Policy;
use tempfile::TempDir;
use tokio::net::TcpListener;

fn fake_paths(root: &std::path::Path) -> CcteamPaths {
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

fn client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

#[tokio::test]
async fn v0_3_happy_path_dashboard_project_sse_and_btw() {
    // ── fixture ──────────────────────────────────────────────────
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let slug = "dev-e2e";

    // bootstrap_project gives us a real `<projects_root>/<slug>/`
    // tree (state.json + .ccteam/ skeleton incl. inbox dir) — same
    // fixture pattern actions_test.rs uses, so /api/<slug>/btw can
    // actually land on disk.
    disable_tool_surface_bootstrap_for_tests();
    bootstrap_project(&paths, slug, "v0.3 e2e canary", "dev").unwrap();

    // Stamp a recognisable phase so the project detail page has
    // something concrete to assert on. V0.4.6 F91 — `cost_used_usd`
    // is deprecated; we still set the field on the legacy serde path
    // so the JSON round-trip stays representative of files in the
    // wild, but the web cost label now comes from `cost_summary`
    // (synthetic `agent_done` events seeded below).
    let state_path = paths.project_state(slug);
    let mut state = ProjectState::load(&state_path).unwrap();
    state.current_phase = "implement".into();
    #[allow(deprecated)]
    {
        state.cost_used_usd = 0.42;
    }
    state.save(&state_path).unwrap();

    let app_state = AppState::new(paths.clone());
    let addr = spawn(app_state).await;
    let nofollow = reqwest::Client::builder()
        .no_proxy()
        .redirect(Policy::none())
        .build()
        .unwrap();

    // ── 1. Legacy root redirects; SPA dashboard data comes from JSON ─
    let resp = nofollow
        .get(format!("http://{addr}/"))
        .send()
        .await
        .expect("GET /");
    assert_eq!(resp.status(), 301, "legacy dashboard must redirect");
    assert_eq!(
        resp.headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .unwrap_or(""),
        "/app/",
    );

    let resp = client()
        .get(format!("http://{addr}/api/v1/projects"))
        .send()
        .await
        .expect("GET /api/v1/projects");
    assert_eq!(resp.status(), 200, "projects API must return 200");
    let rows: serde_json::Value = resp.json().await.unwrap();
    assert!(
        rows.as_array()
            .unwrap()
            .iter()
            .any(|row| row["slug"] == slug),
        "projects API must list slug {slug}; got {rows}",
    );

    // ── 2. Legacy project path redirects; SPA detail data comes from JSON ─
    let resp = nofollow
        .get(format!("http://{addr}/project/{slug}"))
        .send()
        .await
        .expect("GET /project/<slug>");
    assert_eq!(resp.status(), 301, "legacy project page must redirect");
    assert_eq!(
        resp.headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .unwrap_or(""),
        format!("/app/p/{slug}"),
    );

    let resp = client()
        .get(format!("http://{addr}/api/v1/projects/{slug}"))
        .send()
        .await
        .expect("GET /api/v1/projects/<slug>");
    assert_eq!(resp.status(), 200, "project JSON must return 200");
    let detail: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(detail["slug"], slug);
    assert_eq!(detail["state"]["current_phase"], "implement");

    // ── 3. POST /api/<slug>/btw — observe inbox side-effect ─────
    let inbox_dir = paths.project_ccteam_dir(slug).join("inbox");
    let before: usize = fs::read_dir(&inbox_dir).map(|it| it.count()).unwrap_or(0);

    let resp = nofollow
        .post(format!("http://{addr}/api/{slug}/btw"))
        .form(&[("text", "v0.3 e2e canary check")])
        .send()
        .await
        .expect("POST /api/<slug>/btw");
    assert_eq!(resp.status(), 303, "btw must 303 to project page");
    assert_eq!(
        resp.headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .unwrap_or(""),
        format!("/project/{slug}"),
    );

    // Inbox file should land synchronously (actions::send_to_session
    // writes before returning), but allow a tight grace window for
    // any scheduler jitter on slow CI.
    let mut after: usize = before;
    for _ in 0..10 {
        after = fs::read_dir(&inbox_dir).map(|it| it.count()).unwrap_or(0);
        if after > before {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        after,
        before + 1,
        "exactly one new inbox file must land in {} after POST /btw",
        inbox_dir.display(),
    );
    let entries: Vec<_> = fs::read_dir(&inbox_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    let body = fs::read_to_string(entries[0].path()).unwrap();
    assert!(
        body.contains("v0.3 e2e canary check"),
        "inbox body must echo the posted text; got:\n{body}",
    );
}
