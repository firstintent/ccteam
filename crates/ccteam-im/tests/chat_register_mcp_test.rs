//! V0.6.5 F146 — integration tests for the `register_bot_in /
//! unregister_bot_in / list_bots_in` registry primitives the MCP
//! `chat_register_bot` / `chat_unregister_bot` / `chat_list_bots`
//! tools wrap.
//!
//! Lives in `ccteam-im/tests/` (not `ccteam-cli/tests/`) because
//! ccteam-cli imports ccteam-im, not the other way around. The MCP
//! dispatcher's own unit tests live alongside it in
//! `ccteam-cli/src/mcp_chat_tools.rs`.

use ccteam_core::harness::AgentVendor;
use ccteam_im::{
    bot_heartbeat_path_in, bot_running_status_in, last_turn_at, list_bots_in,
    register_bot_checked_in, register_bot_in, registration_path_in, touch_bot_heartbeat_in,
    unregister_bot_in, RegisterOutcome,
};
use tempfile::TempDir;

fn root(tmp: &TempDir) -> std::path::PathBuf {
    tmp.path().join("ccteam-root")
}

#[test]
fn register_bot_checked_writes_registration_at_documented_path() {
    let tmp = TempDir::new().unwrap();
    let r = root(&tmp);
    let outcome = register_bot_checked_in(
        &r,
        "demo",
        "helper",
        AgentVendor::Claude,
        "telegram",
        "42",
        None,
        None,
        None,
    )
    .unwrap();
    let path = match outcome {
        RegisterOutcome::Registered(p) => p,
        RegisterOutcome::AlreadyRegistered(_) => panic!("first register should not be a dup"),
    };
    // Layout: <root>/imd/registry/<slug>/<role>.json
    let expected = registration_path_in(&r, "demo", "helper");
    assert_eq!(path, expected);
    assert!(
        expected.ends_with("imd/registry/demo/helper.json"),
        "unexpected on-disk layout: {}",
        expected.display()
    );
    // Vendor on disk must be lowercase (Bug A防线).
    let on_disk: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&expected).unwrap()).unwrap();
    assert_eq!(on_disk["vendor"], "claude");
}

#[test]
fn register_bot_checked_does_not_clobber_existing() {
    let tmp = TempDir::new().unwrap();
    let r = root(&tmp);
    let _ = register_bot_checked_in(
        &r,
        "demo",
        "helper",
        AgentVendor::Claude,
        "telegram",
        "42",
        None,
        None,
        None,
    )
    .unwrap();
    let original_bytes = std::fs::read(registration_path_in(&r, "demo", "helper")).unwrap();

    // Second call with a different chat id — must NOT overwrite, must
    // return AlreadyRegistered.
    let outcome = register_bot_checked_in(
        &r,
        "demo",
        "helper",
        AgentVendor::Codex,
        "slack",
        "C999",
        None,
        None,
        None,
    )
    .unwrap();
    assert!(matches!(outcome, RegisterOutcome::AlreadyRegistered(_)));
    let after = std::fs::read(registration_path_in(&r, "demo", "helper")).unwrap();
    assert_eq!(
        original_bytes, after,
        "duplicate register_bot_checked must not clobber"
    );
}

#[test]
fn unregister_bot_in_returns_removed_true_then_false_idempotent() {
    let tmp = TempDir::new().unwrap();
    let r = root(&tmp);
    register_bot_in(&r, "demo", "helper", AgentVendor::Claude, "telegram", "42").unwrap();
    let (removed, _) = unregister_bot_in(&r, "demo", "helper").unwrap();
    assert!(removed);
    assert!(!registration_path_in(&r, "demo", "helper").exists());

    let (removed2, _) = unregister_bot_in(&r, "demo", "helper").unwrap();
    assert!(!removed2, "second unregister must be an idempotent miss");
}

#[test]
fn list_bots_in_filters_by_slug() {
    let tmp = TempDir::new().unwrap();
    let r = root(&tmp);
    register_bot_in(&r, "demo", "helper", AgentVendor::Claude, "telegram", "42").unwrap();
    register_bot_in(&r, "other", "lead", AgentVendor::Codex, "slack", "C123").unwrap();

    let all = list_bots_in(&r, None).unwrap();
    assert_eq!(all.len(), 2);
    let demo_only = list_bots_in(&r, Some("demo")).unwrap();
    assert_eq!(demo_only.len(), 1);
    assert_eq!(demo_only[0].workflow_slug, "demo");
    let none = list_bots_in(&r, Some("missing")).unwrap();
    assert!(none.is_empty());
}

#[test]
fn bot_running_status_reflects_heartbeat_freshness() {
    let tmp = TempDir::new().unwrap();
    let r = root(&tmp);
    register_bot_in(&r, "demo", "helper", AgentVendor::Claude, "telegram", "42").unwrap();
    // Heartbeat absent → false.
    assert!(!bot_running_status_in(&r, "demo", "helper"));
    touch_bot_heartbeat_in(&r, "demo", "helper").unwrap();
    assert!(bot_running_status_in(&r, "demo", "helper"));

    // Backdate the heartbeat 2 minutes — stale ⇒ false.
    let hb = bot_heartbeat_path_in(&r, "demo", "helper");
    let two_min_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(120);
    let f = std::fs::OpenOptions::new().write(true).open(&hb).unwrap();
    f.set_modified(two_min_ago).unwrap();
    drop(f);
    assert!(!bot_running_status_in(&r, "demo", "helper"));
}

#[test]
fn unregister_removes_heartbeat_sidecar() {
    // Important: stale heartbeat surviving unregister/re-register would
    // report `running:true` for a freshly registered bot whose daemon
    // task hasn't actually spawned yet. F146 risk row #2.
    let tmp = TempDir::new().unwrap();
    let r = root(&tmp);
    register_bot_in(&r, "demo", "helper", AgentVendor::Claude, "telegram", "42").unwrap();
    touch_bot_heartbeat_in(&r, "demo", "helper").unwrap();
    assert!(bot_heartbeat_path_in(&r, "demo", "helper").exists());

    let (_removed, _) = unregister_bot_in(&r, "demo", "helper").unwrap();
    assert!(
        !bot_heartbeat_path_in(&r, "demo", "helper").exists(),
        "unregister must clean up the sidecar heartbeat"
    );
}

#[test]
fn register_bot_persists_caller_supplied_chat_handle() {
    let tmp = TempDir::new().unwrap();
    let r = root(&tmp);
    let outcome = register_bot_checked_in(
        &r,
        "demo",
        "helper",
        AgentVendor::Claude,
        "telegram",
        "42",
        None,
        Some("curie"),
        None,
    )
    .unwrap();
    assert!(matches!(outcome, RegisterOutcome::Registered(_)));
    let path = registration_path_in(&r, "demo", "helper");
    let on_disk: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(on_disk["chat_handle"], "curie");
    // Re-listing the registry round-trips chat_handle through serde.
    let bots = list_bots_in(&r, None).unwrap();
    assert_eq!(bots.len(), 1);
    assert_eq!(bots[0].chat_handle.as_deref(), Some("curie"));
    assert_eq!(bots[0].effective_handle(), "curie");
}

#[test]
fn register_bot_chat_handle_none_persists_as_omitted_then_falls_back_to_role() {
    let tmp = TempDir::new().unwrap();
    let r = root(&tmp);
    register_bot_checked_in(
        &r,
        "demo",
        "helper",
        AgentVendor::Claude,
        "telegram",
        "42",
        None,
        None,
        None,
    )
    .unwrap();
    let bots = list_bots_in(&r, None).unwrap();
    assert_eq!(bots.len(), 1);
    assert!(bots[0].chat_handle.is_none());
    // Effective handle falls back to role.
    assert_eq!(bots[0].effective_handle(), "helper");
}

#[test]
fn register_bot_persists_caller_supplied_project_dir() {
    // F185 — when MCP `chat_register_bot` passes a `project_dir`, the
    // registry JSON on disk must round-trip the absolute path through
    // serde so the daemon's path resolvers can honor it.
    let tmp = TempDir::new().unwrap();
    let r = root(&tmp);
    let proj = tmp.path().join("vol4/1000/nasworkspace/ccteam");
    std::fs::create_dir_all(&proj).unwrap();
    let outcome = register_bot_checked_in(
        &r,
        "research-squad",
        "helper",
        AgentVendor::Claude,
        "telegram",
        "42",
        None,
        None,
        Some(&proj),
    )
    .unwrap();
    assert!(matches!(outcome, RegisterOutcome::Registered(_)));
    let path = registration_path_in(&r, "research-squad", "helper");
    let on_disk: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(
        on_disk["project_dir"].as_str().unwrap(),
        proj.to_string_lossy()
    );

    // Re-listing the registry round-trips project_dir through serde.
    let bots = list_bots_in(&r, None).unwrap();
    assert_eq!(bots.len(), 1);
    assert_eq!(bots[0].project_dir.as_deref(), Some(proj.as_path()));
}

#[test]
fn register_bot_project_dir_none_persists_as_omitted_field() {
    // Legacy / non-F185 callers don't supply `project_dir`. The serde
    // `skip_serializing_if = "Option::is_none"` must keep the field
    // out of the on-disk JSON entirely so older readers / round-trip
    // assertions don't trip on a `null` they didn't expect.
    let tmp = TempDir::new().unwrap();
    let r = root(&tmp);
    register_bot_checked_in(
        &r,
        "demo",
        "helper",
        AgentVendor::Claude,
        "telegram",
        "42",
        None,
        None,
        None,
    )
    .unwrap();
    let path = registration_path_in(&r, "demo", "helper");
    let body = std::fs::read_to_string(path).unwrap();
    assert!(
        !body.contains("project_dir"),
        "JSON must omit project_dir when None: {}",
        body
    );
    let bots = list_bots_in(&r, None).unwrap();
    assert!(bots[0].project_dir.is_none());
}

#[test]
fn last_turn_at_returns_none_when_file_missing_and_mtime_when_present() {
    let tmp = TempDir::new().unwrap();
    let projects = tmp.path().join("projects");
    std::fs::create_dir_all(&projects).unwrap();
    let reg = ccteam_im::BotRegistration {
        workflow_slug: "demo".into(),
        role: "helper".into(),
        vendor: AgentVendor::Claude,
        persona_id: None,
        im_platform: "telegram".into(),
        im_chat_id: "42".into(),
        chat_handle: None,
        project_dir: None,
        created_at: chrono::Utc::now(),
    };
    assert!(last_turn_at(&projects, &reg).is_none());
    let turns_dir = projects.join("demo/.ccteam/chat/helper");
    std::fs::create_dir_all(&turns_dir).unwrap();
    std::fs::write(turns_dir.join("turns.jsonl"), b"{}\n").unwrap();
    assert!(last_turn_at(&projects, &reg).is_some());
}
