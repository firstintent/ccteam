//! v0.8.8 F3 — `ccteam status` output-shape test.
//!
//! F3 rewrote `run_status` so the one-screen health view:
//!   - nests each project's tracked sessions (role · vendor · status · sid)
//!     under the project row, sourced from the daemon's persisted
//!     `gateway-state.json` (the same out-of-process reader `session ls`
//!     uses — `ccteam_im::gateway::tracked_chat_sessions`);
//!   - DROPS the legacy "recent events (last N)" section (and the
//!     `--tail` arg);
//!   - prints the web token as BARE hex plus a separate `web url:` line
//!     embedding the token WITH the `ccteam:` prefix at port 7331.
//!
//! Driven through the real `ccteam status` binary with `CCTEAM_HOME`
//! pointing at an ephemeral tempdir (project registered via on-disk
//! `config.yaml` + `state.json`; sessions seeded via a hand-written
//! `imd/gateway-state.json`; token seeded via `web-token`). This pins
//! the operator-facing text for host probe / CI greps.

use ccteam_core::state::ProjectState;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Command;

/// True iff `s` is exactly 64 lowercase hex digits.
fn is_hex64(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Materialise `<home>/.ccteam` with one registered project plus a
/// persisted gateway-state.json holding one claude + one codex session
/// in that project, and a web-token file. Returns
/// `(home_tempdir, ccteam_root, projects_root, slug)`.
fn ephemeral_home(slug: &str) -> (tempfile::TempDir, PathBuf, PathBuf, String) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join(".ccteam");
    let projects_root = tmp.path().join("projects");
    let project_dir = projects_root.join(slug);
    std::fs::create_dir_all(project_dir.join(".ccteam")).unwrap();
    std::fs::create_dir_all(root.join("imd")).unwrap();

    // config.yaml — register the project so `collect_projects` finds it.
    let cfg = serde_yaml::Value::Mapping({
        let mut m = serde_yaml::Mapping::new();
        m.insert(
            "projects".into(),
            serde_yaml::Value::Sequence(vec![{
                let mut p = serde_yaml::Mapping::new();
                p.insert("slug".into(), slug.into());
                p.insert(
                    "path".into(),
                    project_dir.to_string_lossy().to_string().into(),
                );
                serde_yaml::Value::Mapping(p)
            }]),
        );
        m
    });
    std::fs::write(
        root.join("config.yaml"),
        serde_yaml::to_string(&cfg).unwrap(),
    )
    .unwrap();

    // project/.ccteam/state.json via the API constructor (full struct shape).
    let state = ProjectState::initial(slug.into());
    std::fs::write(
        project_dir.join(".ccteam").join("state.json"),
        serde_json::to_string_pretty(&state).unwrap(),
    )
    .unwrap();

    // imd/gateway-state.json — one claude + one codex session for the project.
    // Mirrors the `SavedGatewayState` serde shape (AgentVendor / ExecutionMode
    // both `rename_all = "lowercase"`).
    let owner = json!({ "channel": "telegram", "chat_id": "c1", "user_id": "u1" });
    let thread = |vendor: &str| {
        json!({
            "vendor": vendor,
            "mode": "chat",
            "identity": format!("ccteam-chat-{slug}-sX"),
            "started_at": "2026-01-01T00:00:00Z",
            "raw_extras": {}
        })
    };
    let gw = json!({
        "default_project": slug,
        "current_project": [],
        "current_session": [],
        "next_session": 3,
        "sessions": [
            {
                "id": "s1",
                "owner": owner,
                "project": slug,
                "role": "reviewer",
                "vendor": "claude",
                "permission_mode": "skip",
                "secret": "",
                "handle": "reviewer",
                "thread": thread("claude")
            },
            {
                "id": "s2",
                "owner": owner,
                "project": slug,
                "role": "builder",
                "vendor": "codex",
                "permission_mode": "hitl",
                "secret": "",
                "handle": "builder",
                "thread": thread("codex")
            }
        ]
    });
    std::fs::write(
        root.join("imd").join("gateway-state.json"),
        serde_json::to_string_pretty(&gw).unwrap(),
    )
    .unwrap();

    // web-token — bare hex, 0600 (load_existing tolerates the mode warning).
    let token_hex = "deadbeefcafe0123456789abcdef0123456789abcdef0123456789abcdef0123";
    let token_path = root.join("web-token");
    std::fs::write(&token_path, token_hex).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&token_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    (tmp, root, projects_root, slug.to_string())
}

/// Run `ccteam status` against the supplied roots; return stdout.
fn run_status(ccteam_home: &Path, projects_root: &Path) -> String {
    let bin = env!("CARGO_BIN_EXE_ccteam");
    let out = Command::new(bin)
        .env("CCTEAM_HOME", ccteam_home)
        .env("CCTEAM_PROJECTS_ROOT", projects_root)
        .arg("status")
        .output()
        .expect("spawn ccteam status");
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// The status view nests both sessions (claude + codex) under the
/// project with their vendor + sid, and shows the web token (bare hex)
/// + web url (port 7331, `ccteam:`-prefixed token).
#[test]
fn status_nests_sessions_with_vendor_sid_and_web_lines() {
    let (_tmp, root, projects_root, slug) = ephemeral_home("statusproj");
    let stdout = run_status(&root, &projects_root);

    // Project row present.
    assert!(stdout.contains(&slug), "project row missing:\n{stdout}");

    // Both nested session rows carry their vendor + sid.
    assert!(stdout.contains("s1"), "claude sid missing:\n{stdout}");
    assert!(stdout.contains("s2"), "codex sid missing:\n{stdout}");
    assert!(
        stdout.contains("claude"),
        "claude vendor missing:\n{stdout}"
    );
    assert!(stdout.contains("codex"), "codex vendor missing:\n{stdout}");
    assert!(
        stdout.contains("reviewer"),
        "claude role missing:\n{stdout}"
    );
    assert!(stdout.contains("builder"), "codex role missing:\n{stdout}");
}

/// The legacy "recent events" section is gone.
#[test]
fn status_drops_recent_events_section() {
    let (_tmp, root, projects_root, _slug) = ephemeral_home("statusproj2");
    let stdout = run_status(&root, &projects_root);
    assert!(
        !stdout.contains("recent events"),
        "recent-events section must be removed:\n{stdout}"
    );
    assert!(
        !stdout.to_lowercase().contains("last 5"),
        "last-5 tail must be removed:\n{stdout}"
    );
}

/// `web token:` is BARE hex (no `ccteam:` prefix); `web url:` embeds the
/// token WITH the `ccteam:` prefix at port 7331.
#[test]
fn status_web_token_bare_and_url_prefixed_port_7331() {
    let (_tmp, root, projects_root, _slug) = ephemeral_home("statusproj3");
    let stdout = run_status(&root, &projects_root);

    // `web token:` line carries BARE 64-hex, NOT a `ccteam:` prefix.
    let token_line = stdout
        .lines()
        .find(|l| l.contains("web token:"))
        .expect("web token line");
    let token_val = token_line.split("web token:").nth(1).unwrap_or("").trim();
    assert!(
        is_hex64(token_val),
        "web token must be bare 64-hex, got {token_val:?}:\n{stdout}"
    );
    assert!(
        !token_line.contains("ccteam:"),
        "web token line must NOT carry the ccteam: prefix:\n{stdout}"
    );

    // The url line either reaches a LAN ip or degrades, but in both forms
    // it carries port 7331 + the `ccteam:`-prefixed token in the query.
    let url_line = stdout
        .lines()
        .find(|l| l.contains("web url:"))
        .expect("web url line");
    assert!(
        url_line.contains(":7331/?token=ccteam:") || url_line.contains("?token=ccteam:"),
        "web url must embed port 7331 + ccteam: token:\n{url_line}"
    );
    // The embedded token in the url is the ccteam:-prefixed bare hex.
    assert!(
        url_line.contains(&format!("ccteam:{token_val}")),
        "web url token must match the bare token with ccteam: prefix:\n{url_line}"
    );
}
