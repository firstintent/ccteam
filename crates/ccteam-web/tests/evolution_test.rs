//! Integration coverage for the evolution panel's seven-day turn trend.

use std::net::SocketAddr;

use ccteam_core::CcteamPaths;
use ccteam_harness::execution::experience::{
    append_experience, ExperienceRecord, TurnExperience, TurnSignals,
};
use ccteam_web::{router_with_state, AppState, AuthState};
use tokio::net::TcpListener;

const ADMIN_HEX: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

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

fn seed_project(paths: &CcteamPaths, slug: &str) {
    let state_path = paths.project_state(slug);
    std::fs::create_dir_all(state_path.parent().unwrap()).unwrap();
    let mut state = ccteam_core::ProjectState::initial_for_team(slug.into(), "dev".into());
    state.owner = Some("user:web-api".into());
    state.save(&state_path).unwrap();
}

#[tokio::test]
async fn evolution_reports_7day_turn_trend() {
    let tmp = tempfile::TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(&paths.root).unwrap();
    seed_project(&paths, "alpha");
    let dir = paths.project_dir("alpha");

    let turn = |ts: chrono::DateTime<chrono::Utc>| {
        ExperienceRecord::Turn(TurnExperience {
            sid: "s1".into(),
            turn_id: format!("t-{}", ts.timestamp()),
            ts,
            vendor: "claude".into(),
            model: None,
            role: "cto".into(),
            usage: None,
            cost_usd: None,
            duration_ms: None,
            role_sha: Some("abc123abc123".into()),
            skills_sha: None,
            signals: TurnSignals {
                tool_calls: 0,
                steered: false,
                error_recovered: None,
            },
        })
    };
    append_experience(&dir, &turn(chrono::Utc::now())).unwrap();
    append_experience(&dir, &turn(chrono::Utc::now() - chrono::Duration::days(30))).unwrap();

    let state = AppState::with_auth(paths, AuthState::enabled(ADMIN_HEX.into()));
    let addr = spawn(state).await;
    let body: serde_json::Value = client()
        .get(format!("http://{addr}/api/v1/projects/alpha/evolution"))
        .header("Authorization", format!("Bearer ccteam:{ADMIN_HEX}"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["turn_records"], 2);
    assert_eq!(
        body["turn_records_7d"], 1,
        "only the recent turn counts: {body}"
    );
    assert_eq!(body["empty"], false);
}
