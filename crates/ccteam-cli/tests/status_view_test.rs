//! v0.8.8 F3 — `ccteam status` output-shape test.
//!
//! F3 rewrote `run_status` so the one-screen health view:
//!   - nests each project's tracked sessions (role · vendor · status · sid)
//!     under the project row, sourced from the daemon's persisted routing.json
//!     (live-set) ⋈ per-session `meta.json` (v0.8.21 Wave-2; the same
//!     out-of-process reader `session ls` uses —
//!     `ccteam_im::gateway::tracked_chat_sessions`);
//!   - DROPS the legacy "recent events (last N)" section (and the
//!     `--tail` arg);
//!   - prints the web token as BARE hex plus a separate `web url:` line
//!     embedding the token WITH the `ccteam:` prefix at port 7331.
//!
//! Driven through the real `ccteam status` binary with `CCTEAM_HOME`
//! pointing at an ephemeral tempdir (project registered via on-disk
//! `config.yaml` + `state.json`; sessions seeded via `state/gateway/routing.json`
//! (the live-set) + each session's `meta.json`; token seeded via `web-token`).
//! This pins the operator-facing text for host probe / CI greps.

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
    std::fs::create_dir_all(root.join("state").join("im")).unwrap();

    // config.yaml — register the project (full ProjectEntry shape) so BOTH
    // `collect_projects` AND the Wave-2 `tracked_chat_sessions` reader resolve
    // it. The reader enumerates project dirs from config.yaml to locate each
    // live sid's meta.json, so a partial entry (missing team/installed_at) would
    // fail to parse and hide every session — use the canonical writer.
    ccteam_core::config::append_project(
        &root,
        ccteam_core::config::ProjectEntry {
            slug: slug.to_string(),
            path: project_dir.clone(),
            host: ccteam_core::LOCAL_HOST.to_string(),
            remote_slug: None,
            remote_path: None,
            team: "dev".to_string(),
            installed_at: chrono::Utc::now(),
        },
    )
    .unwrap();

    // project/.ccteam/state.json via the API constructor (full struct shape).
    let state = ProjectState::initial(slug.into());
    std::fs::write(
        project_dir.join(".ccteam").join("state.json"),
        serde_json::to_string_pretty(&state).unwrap(),
    )
    .unwrap();

    // v0.8.21 Wave-2 — the session SoT is now per-session meta.json + a
    // routing.json carrying the live-set; `tracked_chat_sessions` reads
    // routing.live_sids ⋈ each session's meta.json (projects via config.yaml).
    // Seed one claude (reviewer) + one codex (builder) session for the project.
    let write_meta = |sid: &str,
                      role: &str,
                      vendor: ccteam_harness::AgentVendor,
                      mode: ccteam_harness::PermissionMode| {
        let now = "2026-01-01T00:00:00Z".to_string();
        let meta = ccteam_harness::SessionMeta {
            mode: None,
            tool_face: None,
            managed_by: Default::default(),
            stopped_at: None,
            sid: sid.to_string(),
            slug: slug.to_string(),
            vendor,
            protocol: ccteam_harness::SessionProtocol::StreamJson,
            role: role.to_string(),
            permission_mode: mode,
            owner: "telegram:c1".to_string(),
            vendor_uuid: String::new(),
            model: None,
            observed_model: None,
            effort: None,
            host: "local".to_string(),
            created_at: now.clone(),
            last_active: now,
            origin: ccteam_harness::SessionOrigin::Ccteam,
            title: None,
            title_source: None,
            turn_count: 0,
            cost_usd: None,
            tokens_total: None,
            role_sha: None,
            skills_sha: None,
            trigger: None,
            parent_sid: None,
            spawned_by_role: None,
            delegation_depth: 0,
        };
        ccteam_harness::write_session_meta(&project_dir, &meta).unwrap();
    };
    write_meta(
        "s1",
        "reviewer",
        ccteam_harness::AgentVendor::Claude,
        ccteam_harness::PermissionMode::Skip,
    );
    write_meta(
        "s2",
        "builder",
        ccteam_harness::AgentVendor::Codex,
        ccteam_harness::PermissionMode::Hitl,
    );
    // routing.json — the live-set the reader filters by.
    std::fs::create_dir_all(root.join("state").join("gateway")).unwrap();
    std::fs::write(
        root.join("state").join("gateway").join("routing.json"),
        serde_json::to_string_pretty(&json!({
            "default_project": slug,
            "current_project": [],
            "current_session": [],
            "live_sids": ["s1", "s2"],
        }))
        .unwrap(),
    )
    .unwrap();

    // web-token — bare hex, 0600 (load_existing tolerates the mode warning).
    let token_hex = "deadbeefcafe0123456789abcdef0123456789abcdef0123456789abcdef0123";
    let token_path = root.join("secrets").join("web-token");
    std::fs::create_dir_all(token_path.parent().unwrap()).unwrap();
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

#[cfg(unix)]
fn fake_healthy_daemon(ccteam_home: &Path) -> std::thread::JoinHandle<()> {
    use std::os::unix::net::UnixListener;

    let socket = ccteam_home.join("run").join("mcp.sock");
    std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket).unwrap();
    std::thread::spawn(move || {
        for _ in 0..8 {
            let _ = listener.accept();
        }
    })
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

#[cfg(unix)]
#[test]
fn status_classifies_nested_sessions_by_sid() {
    let (_tmp, root, projects_root, slug) = ephemeral_home("statusproj-sid");
    let _daemon = fake_healthy_daemon(&root);
    let paths = ccteam_core::CcteamPaths {
        root: root.clone(),
        projects_root: projects_root.clone(),
    };
    ccteam_core::progress::append_event(
        &paths.progress_jsonl(&slug),
        &json!({
            "event": ccteam_core::progress::CHAT_TURN_USER_PROMPT,
            "sid": "s1",
            "ts": chrono::Utc::now().to_rfc3339(),
        }),
    )
    .unwrap();
    ccteam_core::progress::append_event(
        &paths.progress_jsonl(&slug),
        &json!({
            "event": ccteam_core::progress::CHAT_TURN_TIMEOUT,
            "sid": "s2",
            "stuck": true,
            "ts": (chrono::Utc::now() - chrono::Duration::minutes(12)).to_rfc3339(),
        }),
    )
    .unwrap();

    let stdout = run_status(&root, &projects_root);
    let reviewer_line = stdout
        .lines()
        .find(|line| line.contains("reviewer") && line.contains("s1"))
        .expect("reviewer s1 line");
    let builder_line = stdout
        .lines()
        .find(|line| line.contains("builder") && line.contains("s2"))
        .expect("builder s2 line");
    let project_line = stdout
        .lines()
        .find(|line| line.contains(&slug) && !line.contains("session"))
        .expect("project line");
    assert!(
        reviewer_line.contains("working") && !reviewer_line.contains("stuck"),
        "healthy sibling must not inherit s2 timeout:\n{stdout}"
    );
    assert!(
        builder_line.contains("stuck"),
        "timed-out sid must be the stuck row:\n{stdout}"
    );
    assert!(
        project_line.contains("STUCK"),
        "project verdict must still escalate:\n{stdout}"
    );
    assert!(
        stdout.contains("attention:") && stdout.contains(&format!("{slug} s2 stuck")),
        "attention section must identify the stuck sid + activity:\n{stdout}"
    );
    assert!(
        !stdout.contains("/chat/s/s1"),
        "healthy session must not earn an attention line:\n{stdout}"
    );
}

/// Resume-by-sid architecture: an idle session silent for days is the NORMAL
/// resting state — the project stays OK and no check-in hint is printed.
#[test]
fn status_idle_silence_never_alarms() {
    let (_tmp, root, projects_root, slug) = ephemeral_home("statusproj-idle");
    let _daemon = fake_healthy_daemon(&root);
    let paths = ccteam_core::CcteamPaths {
        root: root.clone(),
        projects_root: projects_root.clone(),
    };
    // s1's last event is an idle boundary (turn completed) six days ago.
    ccteam_core::progress::append_event(
        &paths.progress_jsonl(&slug),
        &json!({
            "event": ccteam_core::progress::CHAT_TURN_COMPLETED,
            "sid": "s1",
            "ts": (chrono::Utc::now() - chrono::Duration::days(6)).to_rfc3339(),
        }),
    )
    .unwrap();

    let stdout = run_status(&root, &projects_root);
    let project_line = stdout
        .lines()
        .find(|line| line.contains(&slug) && !line.contains("session"))
        .expect("project line");
    assert!(
        project_line.contains("OK") && project_line.contains("last-event"),
        "idle-for-days project must stay OK:\n{stdout}"
    );
    assert!(
        !stdout.contains("attention:"),
        "idle sessions must not produce an attention section:\n{stdout}"
    );
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
