//! F185 — `BotRegistration.project_dir` path-resolution coverage.
//!
//! Anchors the contract `supervisor::bot_dir` /
//! `inbound::DefaultMailboxResolver::inbox_dir` /
//! `outbound::{turns_jsonl_path, outbound_cursor_path}` honor on top of
//! the new optional field:
//!
//! 1. When the registration carries an explicit `project_dir`, every
//!    resolver lays out paths under that absolute path directly.
//! 2. When `project_dir = None`, every resolver falls back to the
//!    historical `<projects_root>/<workflow_slug>/.ccteam/chat/<role>/`
//!    layout — pre-F185 registrations keep routing the same way.
//! 3. `BotRegistration` serde round-trips the field — JSON with
//!    `project_dir` populates it; JSON without it stays `None`
//!    (the `skip_serializing_if = "Option::is_none"` keeps the wire
//!    shape clean for legacy callers).

use std::path::PathBuf;

use ccteam_core::harness::AgentVendor;
use ccteam_imd::inbound::{DefaultMailboxResolver, MailboxResolver};
use ccteam_imd::supervisor::bot_dir;
use ccteam_imd::{chat_inbox_dir, chat_reset_signal_path, turns_jsonl_path, BotRegistration};

fn mk_reg(slug: &str, role: &str, project_dir: Option<PathBuf>) -> BotRegistration {
    BotRegistration {
        workflow_slug: slug.into(),
        role: role.into(),
        vendor: AgentVendor::Claude,
        persona_id: None,
        im_platform: "telegram".into(),
        im_chat_id: "1".into(),
        chat_handle: None,
        project_dir,
        created_at: chrono::Utc::now(),
    }
}

#[test]
fn bot_dir_honors_explicit_project_dir() {
    let reg = mk_reg(
        "research-squad",
        "tech-helper",
        Some(PathBuf::from("/vol4/1000/nasworkspace/ccteam")),
    );
    // projects_root is irrelevant when project_dir is set — assert that.
    let dir = bot_dir(std::path::Path::new("/home/ubuntu/projects"), &reg);
    assert_eq!(
        dir,
        PathBuf::from("/vol4/1000/nasworkspace/ccteam/.ccteam/chat/tech-helper")
    );
}

#[test]
fn bot_dir_falls_back_to_projects_root_when_project_dir_none() {
    let reg = mk_reg("dev-foo", "lead", None);
    let dir = bot_dir(std::path::Path::new("/home/user/projects"), &reg);
    assert_eq!(
        dir,
        PathBuf::from("/home/user/projects/dev-foo/.ccteam/chat/lead")
    );
}

#[test]
fn chat_inbox_dir_honors_explicit_project_dir() {
    let reg = mk_reg(
        "dir-basename-ne-slug",
        "ops",
        Some(PathBuf::from("/srv/ccteam")),
    );
    let inbox = chat_inbox_dir(std::path::Path::new("/home/user/projects"), &reg);
    assert_eq!(inbox, PathBuf::from("/srv/ccteam/.ccteam/chat/ops/inbox"));
}

#[test]
fn chat_inbox_dir_falls_back_when_project_dir_none() {
    let reg = mk_reg("legacy-bot", "lead", None);
    let inbox = chat_inbox_dir(std::path::Path::new("/home/user/projects"), &reg);
    assert_eq!(
        inbox,
        PathBuf::from("/home/user/projects/legacy-bot/.ccteam/chat/lead/inbox")
    );
}

#[test]
fn chat_reset_signal_path_honors_explicit_project_dir() {
    let reg = mk_reg(
        "any-slug",
        "helper",
        Some(PathBuf::from("/abs/path/to/project")),
    );
    let sig = chat_reset_signal_path(std::path::Path::new("/home/user/projects"), &reg);
    assert_eq!(
        sig,
        PathBuf::from("/abs/path/to/project/.ccteam/chat/helper/signals/reset.signal")
    );
}

#[test]
fn turns_jsonl_path_honors_explicit_project_dir() {
    let reg = mk_reg("squad", "support", Some(PathBuf::from("/vol4/ccteam")));
    let turns = turns_jsonl_path(std::path::Path::new("/home/user/projects"), &reg);
    assert_eq!(
        turns,
        PathBuf::from("/vol4/ccteam/.ccteam/chat/support/turns.jsonl")
    );
}

#[test]
fn default_mailbox_resolver_honors_explicit_project_dir() {
    let reg = mk_reg(
        "research-squad",
        "tech-helper",
        Some(PathBuf::from("/vol4/ccteam")),
    );
    let mailbox = DefaultMailboxResolver::with_projects_root("/home/user/projects");
    let dir = mailbox.inbox_dir(&reg).unwrap();
    assert_eq!(
        dir,
        PathBuf::from("/vol4/ccteam/.ccteam/chat/tech-helper/inbox")
    );
}

#[test]
fn default_mailbox_resolver_falls_back_when_project_dir_none() {
    let reg = mk_reg("dev-foo", "lead", None);
    let mailbox = DefaultMailboxResolver::with_projects_root("/home/user/projects");
    let dir = mailbox.inbox_dir(&reg).unwrap();
    assert_eq!(
        dir,
        PathBuf::from("/home/user/projects/dev-foo/.ccteam/chat/lead/inbox")
    );
}

#[test]
fn serde_round_trips_project_dir_when_present() {
    let reg = mk_reg(
        "demo",
        "helper",
        Some(PathBuf::from("/vol4/1000/nasworkspace/ccteam")),
    );
    let body = serde_json::to_string(&reg).unwrap();
    assert!(
        body.contains("\"project_dir\":\"/vol4/1000/nasworkspace/ccteam\""),
        "serialized JSON should include project_dir: {}",
        body
    );
    let parsed: BotRegistration = serde_json::from_str(&body).unwrap();
    assert_eq!(
        parsed.project_dir,
        Some(PathBuf::from("/vol4/1000/nasworkspace/ccteam"))
    );
}

#[test]
fn serde_omits_project_dir_when_none_and_parses_legacy_json() {
    // Legacy on-disk shape: BotRegistration JSON written before F185
    // had no `project_dir` key. Must still parse — serde `#[serde(default)]`
    // gives us the `None` fallback.
    let legacy_json = r#"{
      "workflow_slug": "dev-foo",
      "role": "lead",
      "vendor": "claude",
      "im_platform": "telegram",
      "im_chat_id": "42",
      "created_at": "2026-05-24T00:00:00Z"
    }"#;
    let parsed: BotRegistration = serde_json::from_str(legacy_json).unwrap();
    assert!(parsed.project_dir.is_none(), "legacy JSON parses to None");

    // Round-trip a None reg: `skip_serializing_if = "Option::is_none"`
    // keeps the field out of the serialized form — important for
    // pre-F185 callers who don't expect the extra key.
    let body = serde_json::to_string(&parsed).unwrap();
    assert!(
        !body.contains("project_dir"),
        "serialized JSON should omit project_dir when None: {}",
        body
    );
}

#[test]
fn project_root_helper_resolves_explicit_and_fallback() {
    let reg_explicit = mk_reg("any", "any", Some(PathBuf::from("/srv/myproj")));
    assert_eq!(
        reg_explicit.project_root(std::path::Path::new("/home/u/projects")),
        PathBuf::from("/srv/myproj")
    );

    let reg_fallback = mk_reg("legacy-slug", "any", None);
    assert_eq!(
        reg_fallback.project_root(std::path::Path::new("/home/u/projects")),
        PathBuf::from("/home/u/projects/legacy-slug")
    );
}
