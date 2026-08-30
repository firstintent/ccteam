//! Smoke tests for the `status` tool body.
//!
//! Previously driven through a `ccteam internal mcp-serve` child; that stdio
//! transport is deleted, so these call the protocol core directly. What they
//! guard is unchanged: `status` lists the projects it can see, and every
//! project entry carries the cost field the web pill and `/status` read.
//!
//! Operations covered (one test each):
//!  1. `status` lists bootstrapped projects
//!  2. `status` project entries carry `cost_24h_usd` (and not the retired alias)

use serde_json::{json, Value};
use tempfile::TempDir;

use ccteam_core::CcteamPaths;

const FIXTURE_WF: &str = r#"
name: test-workflow
agents:
  worker:
    executor: claude
    trigger: watch:.ccteam/inbox/
    input: .ccteam/inbox/
    output: .ccteam/output/
"#;

fn minimal_state_json(slug: &str) -> String {
    let now = chrono::Utc::now().to_rfc3339();
    format!(
        r#"{{
  "slug": "{slug}",
  "team": "dev",
  "created_at": "{now}",
  "tmux_session": "ccteam-{slug}",
  "soft_warn_threshold_usd": 20.0,
  "hard_kill_threshold_usd": 200.0,
  "context_tokens_used": 0,
  "context_reset_threshold_tokens": 600000,
  "context_reset_count": 0,
  "last_progress_event_at": null,
  "last_user_interaction_at": "{now}",
  "user_attached": false,
  "user_pause_pending": false
}}"#
    )
}

fn bootstrap(paths: &CcteamPaths, slug: &str) {
    std::fs::create_dir_all(&paths.root).unwrap();
    std::fs::create_dir_all(&paths.projects_root).unwrap();
    let ccteam_dir = paths.projects_root.join(slug).join(".ccteam");
    std::fs::create_dir_all(&ccteam_dir).unwrap();
    std::fs::write(ccteam_dir.join("workflow.yaml"), FIXTURE_WF).unwrap();
    std::fs::write(ccteam_dir.join("state.json"), minimal_state_json(slug)).unwrap();
}

fn tmp_paths() -> (TempDir, CcteamPaths) {
    ccteam_core::tool_surface::disable_tool_surface_bootstrap_for_tests();
    let tmp = TempDir::new().unwrap();
    let paths = CcteamPaths {
        root: tmp.path().join("home"),
        projects_root: tmp.path().join("projects"),
    };
    (tmp, paths)
}

/// Call a tool and assert `isError=false`; return the parsed body JSON.
async fn call_tool(paths: &CcteamPaths, name: &str, args: Value) -> Value {
    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": name, "arguments": args }
    });
    let resp = ccteam_im::mcp::handle_request(paths, &req, &ccteam_im::mcp::ToolFace::full())
        .await
        .expect("tools/call expects a response");
    assert_eq!(
        resp["result"]["isError"], false,
        "tools/call {name} returned isError=true: {resp:?}"
    );
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    serde_json::from_str(text).expect("parse tool body as JSON")
}

#[tokio::test]
async fn status_list_projects() {
    let (_tmp, paths) = tmp_paths();
    bootstrap(&paths, "dev-gamma");
    bootstrap(&paths, "dev-delta");

    let body = call_tool(&paths, "status", json!({})).await;
    let arr = body["projects"]
        .as_array()
        .expect("projects must be an array");
    assert!(
        arr.len() >= 2,
        "must list at least the 2 bootstrapped projects; got {}",
        arr.len()
    );
    let slugs: Vec<&str> = arr.iter().filter_map(|p| p["slug"].as_str()).collect();
    assert!(
        slugs.contains(&"dev-gamma"),
        "dev-gamma must appear in project list; got {slugs:?}"
    );
    assert!(
        slugs.contains(&"dev-delta"),
        "dev-delta must appear in project list; got {slugs:?}"
    );
}

/// `status` response must carry `cost_24h_usd` on every project entry — that
/// is the data backing the per-project cost surfaces (web cost pill,
/// `/status`).
#[tokio::test]
async fn status_cost_today() {
    let (_tmp, paths) = tmp_paths();
    bootstrap(&paths, "dev-epsilon");

    let body = call_tool(&paths, "status", json!({})).await;
    let arr = body["projects"]
        .as_array()
        .expect("projects must be an array");
    let proj = arr
        .iter()
        .find(|p| p["slug"] == "dev-epsilon")
        .expect("dev-epsilon must appear in listing");
    assert!(
        proj.get("cost_24h_usd").is_some(),
        "project entry must carry cost_24h_usd; got: {proj:?}"
    );
    // The retired `cost_used_usd` alias must stay gone: v0.9.10's STATUS-SLIM-1
    // cut the status wire to `{slug, cost_24h_usd}` because the dead load was
    // burning the caller's context on every call.
    assert!(
        proj.get("cost_used_usd").is_none(),
        "the retired cost_used_usd alias must not come back; got: {proj:?}"
    );
    let cost = proj["cost_24h_usd"].as_f64().unwrap_or(-1.0);
    assert!(
        cost >= 0.0,
        "cost_24h_usd must be non-negative for a fresh project; got {cost}"
    );
}
