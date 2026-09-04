//! The flow-run envelope, end to end through one daemon.
//!
//! This is the whole bridge in one round-trip: the rows a `ccteam flow run`
//! submits (`POST /internal/hook/flow-run/{action}`) land on the project's
//! `progress.jsonl`, and `GET /api/v1/projects/{slug}/flow-runs` folds them back
//! into runs. Both ends are exercised against a real `router_with_state` on a
//! real socket, because the two halves are only useful if they agree — a schema
//! change on either side has to fail here.
//!
//! Isolation: `CcteamPaths` is built by hand over a `TempDir` and injected into
//! `AppState`, so nothing reads or writes the real `~/.ccteam` (the same
//! discipline as `evolution_test.rs` / `internal_hook_test.rs`).

use std::net::SocketAddr;

use ccteam_core::CcteamPaths;
use ccteam_web::{router_with_state, AppState, AuthState};
use serde_json::{json, Value};
use tempfile::TempDir;
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

/// Submit one envelope row exactly the way the flow CLI does.
async fn submit(addr: SocketAddr, action: &str, body: Value) {
    let response = client()
        .post(format!("http://{addr}/internal/hook/flow-run/{action}"))
        .header("Authorization", format!("Bearer ccteam:{ADMIN_HEX}"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "flow-run/{action} rejected: {} {}",
        response.status(),
        response.text().await.unwrap_or_default()
    );
}

async fn flow_runs(addr: SocketAddr, slug: &str) -> Value {
    client()
        .get(format!("http://{addr}/api/v1/projects/{slug}/flow-runs"))
        .header("Authorization", format!("Bearer ccteam:{ADMIN_HEX}"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap()
}

async fn serving(tmp: &TempDir) -> (CcteamPaths, SocketAddr) {
    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(&paths.root).unwrap();
    seed_project(&paths, "alpha");
    let state = AppState::with_auth(paths.clone(), AuthState::enabled(ADMIN_HEX.into()));
    let addr = spawn(state).await;
    (paths, addr)
}

#[tokio::test]
async fn a_submitted_run_envelope_comes_back_as_one_finished_run() {
    let tmp = TempDir::new().unwrap();
    let (paths, addr) = serving(&tmp).await;

    submit(
        addr,
        "started",
        json!({
            "project": "alpha",
            "run_id": "audit-20260901-101500",
            "parent_sid": "s42",
            "name": "audit-routes",
            "description": "Audit route handlers for missing auth",
            "script_path": "/w/.agents/flows/audit.flow.js",
            "started_at": "2026-09-01T10:15:00Z",
        }),
    )
    .await;
    submit(
        addr,
        "finished",
        json!({
            "project": "alpha",
            "run_id": "audit-20260901-101500",
            "agents": 5,
            "cost_usd": 1.25,
            "ok": true,
            "finished_at": "2026-09-01T10:20:00Z",
        }),
    )
    .await;

    // The rows really are on the state source of truth, not in daemon memory.
    let journal = std::fs::read_to_string(paths.progress_jsonl("alpha")).unwrap();
    assert!(journal.contains("\"flow_run_started\""), "{journal}");
    assert!(journal.contains("\"flow_run_finished\""), "{journal}");

    let body = flow_runs(addr, "alpha").await;
    let runs = body["runs"].as_array().expect("runs array");
    assert_eq!(runs.len(), 1, "two rows are one run: {body}");
    let run = &runs[0];
    assert_eq!(run["run_id"], "audit-20260901-101500");
    assert_eq!(run["status"], "ok");
    assert_eq!(run["name"], "audit-routes");
    assert_eq!(run["description"], "Audit route handlers for missing auth");
    assert_eq!(run["parent_sid"], "s42");
    assert_eq!(run["agents"], 5);
    assert_eq!(run["cost_usd"], 1.25);
    assert_eq!(run["started_at"], "2026-09-01T10:15:00Z");
    assert_eq!(run["finished_at"], "2026-09-01T10:20:00Z");
    assert_eq!(body["truncated"], false, "{body}");
}

/// The URL-shaped ACL choke point (`auth::project_acl_layer`) cannot see a
/// slug that travels in a POST body, so `flow-run` carries its own ownership
/// gate (checker s523 R1): a tenant must not append envelope rows to a
/// project it cannot see — and must not learn whether that project exists.
#[tokio::test]
async fn a_tenant_cannot_write_another_owners_journal() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(&paths.root).unwrap();
    std::fs::create_dir_all(paths.users_dir()).unwrap();
    // Owned by `user:web-api` — NOT the tenant minted below.
    seed_project(&paths, "alpha");

    let mut reg = ccteam_core::tenants::TenantRegistry::default();
    let tenant = reg.add("mallory");
    reg.save(&paths.users_dir()).unwrap();

    let state = AppState::with_auth(paths.clone(), AuthState::enabled(ADMIN_HEX.into()));
    let addr = spawn(state).await;

    let response = client()
        .post(format!("http://{addr}/internal/hook/flow-run/started"))
        .header(
            "Authorization",
            format!("Bearer ccteam:{}", tenant.web_token),
        )
        .json(&json!({
            "project": "alpha",
            "run_id": "intruder-1",
            "started_at": "2026-09-01T10:15:00Z",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status().as_u16(),
        404,
        "a foreign project must read as not-found, disclosing nothing"
    );
    assert!(
        !paths.progress_jsonl("alpha").exists(),
        "the refused row must not have touched the journal"
    );
}

/// Append `n` non-envelope rows to a project's journal, the way a day of chat
/// and tool calls does — the noise every flow envelope is buried under.
fn bury_under_chat(paths: &CcteamPaths, slug: &str, n: usize) {
    use std::io::Write as _;
    let filler: String = (0..n)
        .map(|i| {
            format!(
                "{}\n",
                json!({"ts": "2026-09-01T11:00:00Z", "event": "chat_tool_call_started", "n": i})
            )
        })
        .collect();
    let mut journal = std::fs::OpenOptions::new()
        .append(true)
        .open(paths.progress_jsonl(slug))
        .unwrap();
    journal.write_all(filler.as_bytes()).unwrap();
}

/// The bug behind docs-local/issues/#16: the list is bounded in RUNS, not in
/// journal rows. A run buried under any amount of later chat is still listed,
/// and the list is reported complete — the first cut's 5000-row window lost
/// the run AND raised its "older runs scrolled out" flag on every busy
/// project, including ones that had never run a flow.
#[tokio::test]
async fn a_run_buried_under_a_busy_journal_is_still_listed_and_not_disclaimed() {
    let tmp = TempDir::new().unwrap();
    let (paths, addr) = serving(&tmp).await;

    submit(
        addr,
        "started",
        json!({"project": "alpha", "run_id": "buried-1", "started_at": "2026-09-01T10:15:00Z"}),
    )
    .await;
    submit(
        addr,
        "finished",
        json!({"project": "alpha", "run_id": "buried-1", "agents": 2, "cost_usd": 0.5, "ok": true,
               "finished_at": "2026-09-01T10:20:00Z"}),
    )
    .await;
    // Well past what any row-counted window would cover.
    bury_under_chat(&paths, "alpha", 12_000);

    let body = flow_runs(addr, "alpha").await;
    assert_eq!(body["truncated"], false, "{body}");
    let runs = body["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 1, "{body}");
    assert_eq!(runs[0]["run_id"], "buried-1");
    assert_eq!(runs[0]["status"], "ok");
}

/// `truncated` means exactly "a run exists that is not listed": more runs than
/// the window keeps. The newest survive, the flag is up, and one run under the
/// bound reads complete — the flag is never inferred from a full page.
#[tokio::test]
async fn more_runs_than_the_window_keeps_lists_the_newest_and_says_so() {
    let tmp = TempDir::new().unwrap();
    let (_paths, addr) = serving(&tmp).await;

    // The window is 150 envelope rows; two rows per run here, so 75 runs fill
    // it exactly and the 76th (oldest, submitted first) falls off.
    for i in 0..76 {
        let run_id = format!("run-{i:03}");
        let minute = i % 60;
        let hour = 10 + i / 60;
        submit(
            addr,
            "started",
            json!({"project": "alpha", "run_id": run_id,
                   "started_at": format!("2026-09-01T{hour:02}:{minute:02}:00Z")}),
        )
        .await;
        submit(
            addr,
            "finished",
            json!({"project": "alpha", "run_id": run_id, "agents": 1, "cost_usd": 0.1, "ok": true,
                   "finished_at": format!("2026-09-01T{hour:02}:{minute:02}:30Z")}),
        )
        .await;
    }

    let body = flow_runs(addr, "alpha").await;
    assert_eq!(body["truncated"], true, "{body}");
    let ids: Vec<&str> = body["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|run| run["run_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.len(), 75, "{body}");
    assert_eq!(ids[0], "run-075", "newest first");
    assert_eq!(ids[74], "run-001", "the oldest listed is the second run");
    assert!(
        !ids.contains(&"run-000"),
        "the run past the window is not listed"
    );
}

/// A traversal-shaped "slug" is refused on SHAPE, before `can_see_project`
/// can resolve it into a filesystem path (s523 R2): the ordering is the fix;
/// this test locks the refusal itself (non-disclosing 404, journal-free).
#[tokio::test]
async fn a_traversal_shaped_slug_is_refused_before_path_resolution() {
    let tmp = TempDir::new().unwrap();
    let (paths, addr) = serving(&tmp).await;

    for bad in ["../alpha", "a/b", "a\\b", ".."] {
        let response = client()
            .post(format!("http://{addr}/internal/hook/flow-run/started"))
            .header("Authorization", format!("Bearer ccteam:{ADMIN_HEX}"))
            .json(&json!({
                "project": bad,
                "run_id": "probe-1",
                "started_at": "2026-09-01T10:15:00Z",
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 404, "slug {bad:?} must 404");
    }
    // Nothing was filed anywhere — the seeded project's journal included.
    assert!(!paths.progress_jsonl("alpha").exists());
}

#[tokio::test]
async fn an_unfinished_run_reads_as_running_until_its_terminal_row_lands() {
    let tmp = TempDir::new().unwrap();
    let (_paths, addr) = serving(&tmp).await;

    submit(
        addr,
        "started",
        json!({
            "project": "alpha",
            "run_id": "live-1",
            "name": "long-audit",
            "started_at": "2026-09-01T10:15:00Z",
        }),
    )
    .await;

    let body = flow_runs(addr, "alpha").await;
    assert_eq!(body["runs"][0]["status"], "running");
    assert!(body["runs"][0]["finished_at"].is_null());
    // A flow started from a plain shell belongs to no session, and says so
    // rather than inventing an attribution.
    assert!(body["runs"][0]["parent_sid"].is_null());

    submit(
        addr,
        "finished",
        json!({
            "project": "alpha",
            "run_id": "live-1",
            "agents": 1,
            "cost_usd": 0.1,
            "ok": false,
            "finished_at": "2026-09-01T10:30:00Z",
        }),
    )
    .await;

    let body = flow_runs(addr, "alpha").await;
    assert_eq!(body["runs"].as_array().unwrap().len(), 1);
    assert_eq!(body["runs"][0]["status"], "error");
}

#[tokio::test]
async fn a_braked_run_is_not_reported_as_a_broken_one() {
    let tmp = TempDir::new().unwrap();
    let (_paths, addr) = serving(&tmp).await;

    submit(
        addr,
        "started",
        json!({"project": "alpha", "run_id": "braked-1", "started_at": "2026-09-01T10:15:00Z"}),
    )
    .await;
    submit(
        addr,
        "brake",
        json!({"project": "alpha", "run_id": "braked-1", "reason": "max_agents"}),
    )
    .await;
    // The runner reports a braked run with the same `ok:false` a thrown script
    // gets; the brake is what tells the two apart.
    submit(
        addr,
        "finished",
        json!({
            "project": "alpha",
            "run_id": "braked-1",
            "agents": 8,
            "cost_usd": 2.0,
            "ok": false,
            "brake": "max_agents",
            "finished_at": "2026-09-01T10:40:00Z",
        }),
    )
    .await;

    let body = flow_runs(addr, "alpha").await;
    assert_eq!(body["runs"][0]["status"], "brake");
    assert_eq!(body["runs"][0]["agents"], 8);
}

#[tokio::test]
async fn several_runs_come_back_newest_first() {
    let tmp = TempDir::new().unwrap();
    let (_paths, addr) = serving(&tmp).await;

    for (run_id, started_at) in [
        ("older", "2026-09-01T09:00:00Z"),
        ("newer", "2026-09-01T11:00:00Z"),
        ("middle", "2026-09-01T10:00:00Z"),
    ] {
        submit(
            addr,
            "started",
            json!({"project": "alpha", "run_id": run_id, "started_at": started_at}),
        )
        .await;
    }

    let body = flow_runs(addr, "alpha").await;
    let order: Vec<&str> = body["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|run| run["run_id"].as_str().unwrap())
        .collect();
    assert_eq!(order, ["newer", "middle", "older"]);
}

/// "No runs" and "complete" together, whether the project is fresh or has
/// chatted for weeks — a busy journal is not evidence of runs.
#[tokio::test]
async fn a_project_that_never_ran_a_flow_answers_honestly_empty() {
    let tmp = TempDir::new().unwrap();
    let (paths, addr) = serving(&tmp).await;

    let body = flow_runs(addr, "alpha").await;
    assert_eq!(body["runs"].as_array().unwrap().len(), 0);
    assert_eq!(body["truncated"], false, "{body}");

    // The journal exists now, and is long; still no runs, still complete.
    std::fs::create_dir_all(paths.progress_jsonl("alpha").parent().unwrap()).unwrap();
    std::fs::write(paths.progress_jsonl("alpha"), "").unwrap();
    bury_under_chat(&paths, "alpha", 12_000);
    let body = flow_runs(addr, "alpha").await;
    assert_eq!(body["runs"].as_array().unwrap().len(), 0);
    assert_eq!(body["truncated"], false, "{body}");
}

#[tokio::test]
async fn a_project_the_caller_cannot_see_is_not_answered() {
    let tmp = TempDir::new().unwrap();
    let (_paths, addr) = serving(&tmp).await;

    // No credential at all: the project ACL gate must refuse before the
    // handler ever reads a journal.
    let response = client()
        .get(format!("http://{addr}/api/v1/projects/alpha/flow-runs"))
        .send()
        .await
        .unwrap();
    assert!(
        !response.status().is_success(),
        "an unauthenticated caller must not read another project's runs (got {})",
        response.status()
    );
}
