//! F185 + V0.6.8 F190 — `BotRegistration.project_dir` /
//! `~/.ccteam/config.yaml::projects[]` path-resolution coverage.
//!
//! Anchors the contract `supervisor::bot_dir` /
//! `inbound::DefaultMailboxResolver::inbox_dir` /
//! `outbound::{turns_jsonl_path, outbound_cursor_path}` honor on top of
//! the F185 optional field + F190 three-tier priority chain:
//!
//! 1. When the registration carries an explicit `project_dir`, every
//!    resolver lays out paths under that absolute path directly.
//! 2. F190 — when `reg.project_dir = None` AND the bot's slug is
//!    present in the `config.yaml::projects[]` map, resolvers use the
//!    config-recorded path. Lets legacy registrations (pre-F185) work
//!    on hosts whose project lives outside `~/projects/<slug>/`.
//! 3. When both `reg.project_dir = None` AND the slug is absent from
//!    config, every resolver falls back to the historical
//!    `<projects_root>/<workflow_slug>/.ccteam/chat/<role>/` layout.
//! 4. `BotRegistration` serde round-trips the field — JSON with
//!    `project_dir` populates it; JSON without it stays `None`
//!    (the `skip_serializing_if = "Option::is_none"` keeps the wire
//!    shape clean for legacy callers).

use std::collections::HashMap;
use std::path::PathBuf;

use ccteam_harness::AgentVendor;
use ccteam_im::inbound::{DefaultMailboxResolver, MailboxResolver};
use ccteam_im::supervisor::{bot_dir, bot_dir_with_config};
use ccteam_im::{
    chat_inbox_dir, chat_reset_signal_path, resolve_project_dir, turns_jsonl_path, BotRegistration,
};

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

// -------- V0.6.8 F190 — three-tier priority chain coverage ----------

#[test]
fn resolve_project_dir_prefers_explicit_over_config() {
    // Tier 1 wins even when tier 2 has a different path: explicit
    // `reg.project_dir` from F185 is the highest-priority source.
    let reg = mk_reg(
        "research-squad",
        "code-critic",
        Some(PathBuf::from("/from/explicit")),
    );
    let mut cfg: HashMap<String, PathBuf> = HashMap::new();
    cfg.insert(
        "research-squad".into(),
        PathBuf::from("/from/config/should-not-win"),
    );
    let got = resolve_project_dir(&reg, std::path::Path::new("/home/u/projects"), &cfg);
    assert_eq!(got, PathBuf::from("/from/explicit"));
}

#[test]
fn resolve_project_dir_uses_config_when_reg_none_and_slug_present() {
    // Tier 2 — legacy registration (project_dir = None) but slug is in
    // config.yaml::projects[]. The config path wins over the
    // projects_root fallback. This is the F190 NAS-share / out-of-tree
    // project unlock.
    let reg = mk_reg("nas-bot", "lead", None);
    let mut cfg: HashMap<String, PathBuf> = HashMap::new();
    cfg.insert("nas-bot".into(), PathBuf::from("/vol4/1000/ccteam"));
    let got = resolve_project_dir(&reg, std::path::Path::new("/home/u/projects"), &cfg);
    assert_eq!(got, PathBuf::from("/vol4/1000/ccteam"));
}

#[test]
fn resolve_project_dir_falls_through_to_projects_root_when_slug_absent() {
    // Tier 3 — neither reg.project_dir nor config map contains the
    // slug. Resolver falls through to the historical layout.
    let reg = mk_reg("orphan-slug", "lead", None);
    let cfg: HashMap<String, PathBuf> = HashMap::new(); // empty
    let got = resolve_project_dir(&reg, std::path::Path::new("/home/u/projects"), &cfg);
    assert_eq!(got, PathBuf::from("/home/u/projects/orphan-slug"));
}

#[test]
fn resolve_project_dir_falls_through_with_unrelated_config_entries() {
    // Tier 3 sanity — slug missing from config, but config isn't
    // empty (other projects present). Still falls through to the
    // projects_root tier; we don't grab the first entry or anything
    // silly like that.
    let reg = mk_reg("orphan", "lead", None);
    let mut cfg: HashMap<String, PathBuf> = HashMap::new();
    cfg.insert("other-project".into(), PathBuf::from("/srv/other"));
    let got = resolve_project_dir(&reg, std::path::Path::new("/home/u/projects"), &cfg);
    assert_eq!(got, PathBuf::from("/home/u/projects/orphan"));
}

#[test]
fn bot_dir_with_config_uses_config_when_reg_none() {
    let reg = mk_reg("legacy-bot", "lead", None);
    let mut cfg: HashMap<String, PathBuf> = HashMap::new();
    cfg.insert("legacy-bot".into(), PathBuf::from("/srv/legacy"));
    let dir = bot_dir_with_config(std::path::Path::new("/home/u/projects"), &reg, &cfg);
    assert_eq!(dir, PathBuf::from("/srv/legacy/.ccteam/chat/lead"));
}

#[test]
fn bot_dir_with_config_explicit_wins_over_config() {
    let reg = mk_reg(
        "any-slug",
        "lead",
        Some(PathBuf::from("/from/explicit/project")),
    );
    let mut cfg: HashMap<String, PathBuf> = HashMap::new();
    cfg.insert("any-slug".into(), PathBuf::from("/from/config"));
    let dir = bot_dir_with_config(std::path::Path::new("/home/u/projects"), &reg, &cfg);
    assert_eq!(
        dir,
        PathBuf::from("/from/explicit/project/.ccteam/chat/lead")
    );
}

#[test]
fn default_mailbox_resolver_consults_config_when_reg_none() {
    // F190 — resolver constructed via `with_config_projects` honors
    // tier 2 of the priority chain when the registration lacks an
    // explicit `project_dir`.
    let reg = mk_reg("nas-bot", "lead", None);
    let mut cfg: HashMap<String, PathBuf> = HashMap::new();
    cfg.insert("nas-bot".into(), PathBuf::from("/vol4/1000/ccteam"));
    let mailbox = DefaultMailboxResolver::with_config_projects("/home/u/projects", cfg);
    let dir = mailbox.inbox_dir(&reg).unwrap();
    assert_eq!(
        dir,
        PathBuf::from("/vol4/1000/ccteam/.ccteam/chat/lead/inbox")
    );
}

#[test]
fn default_mailbox_resolver_falls_through_when_slug_absent_from_config() {
    // F190 — slug not in the config map. Resolver falls through to
    // projects_root layout (pre-F190 behavior preserved).
    let reg = mk_reg("orphan", "lead", None);
    let cfg: HashMap<String, PathBuf> = HashMap::new(); // empty
    let mailbox = DefaultMailboxResolver::with_config_projects("/home/u/projects", cfg);
    let dir = mailbox.inbox_dir(&reg).unwrap();
    assert_eq!(
        dir,
        PathBuf::from("/home/u/projects/orphan/.ccteam/chat/lead/inbox")
    );
}

#[test]
fn default_mailbox_resolver_explicit_project_dir_beats_config() {
    // F190 — when both reg.project_dir AND config map have entries
    // for the slug, reg.project_dir wins (F185 priority preserved).
    let reg = mk_reg("dual", "lead", Some(PathBuf::from("/from/explicit")));
    let mut cfg: HashMap<String, PathBuf> = HashMap::new();
    cfg.insert("dual".into(), PathBuf::from("/from/config"));
    let mailbox = DefaultMailboxResolver::with_config_projects("/home/u/projects", cfg);
    let dir = mailbox.inbox_dir(&reg).unwrap();
    assert_eq!(dir, PathBuf::from("/from/explicit/.ccteam/chat/lead/inbox"));
}

#[test]
fn load_config_projects_map_missing_file_returns_empty() {
    // Simulates a fresh install with no `~/.ccteam/config.yaml`. The
    // helper must yield an empty map (not an error) so resolvers fall
    // through cleanly to the projects_root tier.
    let tmp = tempfile::TempDir::new().unwrap();
    let got = ccteam_im::load_config_projects_map(tmp.path()).unwrap();
    assert!(got.is_empty(), "expected empty map, got {:?}", got);
}

#[test]
fn load_config_projects_map_populates_from_config_yaml() {
    use ccteam_core::config::{save, CcteamConfig, ProjectEntry};
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = CcteamConfig {
        projects: vec![
            ProjectEntry {
                slug: "alpha".into(),
                path: PathBuf::from("/vol4/alpha"),
                team: "dev".into(),
                installed_at: chrono::Utc::now(),
            },
            ProjectEntry {
                slug: "beta".into(),
                path: PathBuf::from("/srv/beta"),
                team: "research".into(),
                installed_at: chrono::Utc::now(),
            },
        ],
        ..Default::default()
    };
    save(tmp.path(), &cfg).unwrap();
    let got = ccteam_im::load_config_projects_map(tmp.path()).unwrap();
    assert_eq!(got.len(), 2);
    assert_eq!(got.get("alpha"), Some(&PathBuf::from("/vol4/alpha")));
    assert_eq!(got.get("beta"), Some(&PathBuf::from("/srv/beta")));
}
