//! v0.8.24 gap-fill — compare history (`GET .../compare/history`) aggregated
//! from session meta `compare_group`, and the evolution 7-day trend stat
//! (`turn_records_7d`).

use std::net::SocketAddr;

use ccteam_core::CcteamPaths;
use ccteam_harness::execution::experience::{
    append_experience, ExperienceRecord, TurnExperience, TurnSignals,
};
use ccteam_harness::execution::turns_mirror::{append_turn, TurnRecord};
use ccteam_harness::{
    write_session_meta, AgentVendor, PermissionMode, SessionMeta, SessionOrigin, SessionProtocol,
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
    let mut st = ccteam_core::ProjectState::initial_for_team(slug.into(), "dev".into());
    st.owner = Some("user:web-api".into());
    st.save(&state_path).unwrap();
}

fn meta(
    sid: &str,
    slug: &str,
    vendor: AgentVendor,
    group: Option<&str>,
    cost: Option<f64>,
    created_at: &str,
) -> SessionMeta {
    SessionMeta {
        sid: sid.into(),
        slug: slug.into(),
        vendor,
        protocol: SessionProtocol::StreamJson,
        role: String::new(),
        permission_mode: PermissionMode::Skip,
        owner: "user:web-api".into(),
        vendor_uuid: String::new(),
        host: "local".into(),
        created_at: created_at.into(),
        last_active: created_at.into(),
        origin: SessionOrigin::Ccteam,
        title: None,
        title_source: None,
        turn_count: 1,
        cost_usd: cost,
        tokens_total: None,
        role_sha: None,
        skills_sha: None,
        trigger: Some("compare".into()),
        compare_group: group.map(str::to_string),
        parent_sid: None,
        spawned_by_role: None,
        delegation_depth: 0,
    }
}

#[tokio::test]
async fn compare_history_groups_members_prompt_and_subtotal() {
    let tmp = tempfile::TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(&paths.root).unwrap();
    seed_project(&paths, "alpha");
    let dir = paths.project_dir("alpha");

    // Two members of cmp-1 + one non-compare session (must be excluded).
    write_session_meta(
        &dir,
        &meta(
            "s2",
            "alpha",
            AgentVendor::Codex,
            Some("cmp-1"),
            None,
            "2026-07-10T10:00:01Z",
        ),
    )
    .unwrap();
    write_session_meta(
        &dir,
        &meta(
            "s1",
            "alpha",
            AgentVendor::Claude,
            Some("cmp-1"),
            Some(0.02),
            "2026-07-10T10:00:00Z",
        ),
    )
    .unwrap();
    write_session_meta(
        &dir,
        &meta(
            "s3",
            "alpha",
            AgentVendor::Grok,
            None,
            Some(1.0),
            "2026-07-11T00:00:00Z",
        ),
    )
    .unwrap();
    // First user turn of a member = the question summary.
    append_turn(
        &dir,
        "s1",
        &TurnRecord {
            turn_id: "t1".into(),
            ts: chrono::Utc::now(),
            vendor: "claude".into(),
            role: String::new(),
            user: "why is the test flaky?".into(),
            assistant: "because of a race".into(),
            usage: serde_json::Value::Null,
            tool_calls: vec![],
        },
    )
    .unwrap();

    let state = AppState::with_auth(paths, AuthState::enabled(ADMIN_HEX.into()));
    let addr = spawn(state).await;
    let body: serde_json::Value = client()
        .get(format!(
            "http://{addr}/api/v1/projects/alpha/compare/history"
        ))
        .header("Authorization", format!("Bearer ccteam:{ADMIN_HEX}"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();

    let groups = body["groups"].as_array().unwrap();
    assert_eq!(
        groups.len(),
        1,
        "non-compare session must not group: {body}"
    );
    let g = &groups[0];
    assert_eq!(g["group"], "cmp-1");
    assert_eq!(g["created_at"], "2026-07-10T10:00:00Z");
    assert_eq!(g["prompt"], "why is the test flaky?");
    let members = g["members"].as_array().unwrap();
    assert_eq!(members.len(), 2);
    assert_eq!(members[0]["sid"], "s1");
    assert_eq!(members[0]["vendor"], "claude");
    assert_eq!(members[1]["vendor"], "codex");
    assert!((g["cost_subtotal_usd"].as_f64().unwrap() - 0.02).abs() < 1e-9);
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
