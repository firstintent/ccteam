//! V0.3.1 F47 — `ccteam session` CLI surface.
//!
//! F47 PR ships the parser shape only; the master state.json::sessions
//! runtime path is F49 (V0.3.1 PR #4). This integration test pins the
//! shape three different consumers depend on:
//!
//! 1. `ccteam session --help` lists every subcommand (`add` / `ls` /
//!    `attach` / `rm`) — a typo in the parser quietly hides one without
//!    this canary.
//! 2. `ccteam session add <slug> --harness=codex` returns exit 1 with
//!    a stderr message citing V0.3.2 + the codex-integration research
//!    doc. This is the F47 verification target — exercising the
//!    [`ccteam_core::CodexAdapter::spawn_session`] stub error path
//!    end-to-end.
//! 3. The F49 runtime path reads flex `state.json::sessions`, lists
//!    sessions, validates attach targets, and removes explicit sessions.
//!
//! Lives in `tests/` rather than the lib-internal mod so it spawns
//! the real `ccteam` binary via `env!("CARGO_BIN_EXE_ccteam")` — the
//! shape only matters end-to-end.

use std::process::Command;

use ccteam_core::{tmux_available, HarnessKind, ProjectState, SessionRecord, TeamKind};
use tempfile::TempDir;

fn cct_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ccteam")
}

struct Fixture {
    _tmp: TempDir,
    ccteam_home: std::path::PathBuf,
    projects_root: std::path::PathBuf,
    slug: String,
}

impl Fixture {
    fn new_flex() -> Self {
        Self::new_flex_with_session(true)
    }

    fn new_empty_flex() -> Self {
        Self::new_flex_with_session(false)
    }

    fn new_flex_with_session(include_session: bool) -> Self {
        let tmp = TempDir::new().unwrap();
        let ccteam_home = tmp.path().join("home");
        let projects_root = tmp.path().join("projects");
        let slug = "flex-demo".to_string();
        let paths = ccteam_core::CcteamPaths {
            root: ccteam_home.clone(),
            projects_root: projects_root.clone(),
        };
        std::fs::create_dir_all(paths.project_ccteam_dir(&slug)).unwrap();
        let mut state = ProjectState::initial_for_team(slug.clone(), "flex".into());
        state.team_kind = TeamKind::Flex;
        if include_session {
            state.sessions.insert(
                "claude-1".into(),
                SessionRecord {
                    harness: HarnessKind::Claude,
                    tmux_session: "ccteam-flex-demo-claude-1".into(),
                    started_at: chrono::Utc::now(),
                    pid: None,
                },
            );
            state.next_sid_seq.insert(HarnessKind::Claude, 2);
        }
        state.save(&paths.project_state(&slug)).unwrap();
        Self {
            _tmp: tmp,
            ccteam_home,
            projects_root,
            slug,
        }
    }

    fn new_workflow() -> Self {
        let fx = Self::new_flex();
        let paths = ccteam_core::CcteamPaths {
            root: fx.ccteam_home.clone(),
            projects_root: fx.projects_root.clone(),
        };
        let state = ProjectState::initial_for_team(fx.slug.clone(), "dev".into());
        state.save(&paths.project_state(&fx.slug)).unwrap();
        fx
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(cct_bin());
        cmd.env("CCTEAM_HOME", &self.ccteam_home)
            .env("CCTEAM_PROJECTS_ROOT", &self.projects_root);
        cmd
    }
}

#[test]
fn session_help_advertises_every_subcommand() {
    let out = Command::new(cct_bin())
        .args(["session", "--help"])
        .output()
        .expect("spawn ccteam session --help");
    assert!(
        out.status.success(),
        "ccteam session --help should exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    for sub in ["add", "ls", "attach", "rm"] {
        assert!(
            stdout.contains(sub),
            "ccteam session --help should advertise subcommand `{sub}`; got:\n{stdout}",
        );
    }
}

#[test]
fn session_add_help_advertises_harness_flag() {
    let out = Command::new(cct_bin())
        .args(["session", "add", "--help"])
        .output()
        .expect("spawn ccteam session add --help");
    assert!(
        out.status.success(),
        "ccteam session add --help should exit 0"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("--harness"));
    // ValueEnum derive surfaces the variant names in --help output.
    assert!(
        stdout.contains("claude") && stdout.contains("codex"),
        "--harness should list both `claude` and `codex` values; got:\n{stdout}",
    );
}

#[test]
fn session_add_claude_spawns_tmux_session_and_records_state() {
    if !tmux_available() {
        eprintln!(
            "[skip] session_add_claude_spawns_tmux_session_and_records_state: tmux not on PATH"
        );
        return;
    }

    let fx = Fixture::new_empty_flex();
    let fake_bin = fx._tmp.path().join("bin");
    std::fs::create_dir_all(&fake_bin).unwrap();
    let fake_claude = fake_bin.join("claude");
    std::fs::write(
        &fake_claude,
        "#!/bin/sh\ntrap 'exit 0' TERM INT\nwhile true; do sleep 1; done\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&fake_claude).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&fake_claude, perms).unwrap();
    }
    let old_path = std::env::var("PATH").unwrap_or_default();
    let test_path = format!("{}:{old_path}", fake_bin.display());

    let out = fx
        .command()
        .env("PATH", test_path)
        .args(["session", "add", &fx.slug, "--harness=claude"])
        .output()
        .expect("spawn session add");
    assert!(
        out.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let paths = ccteam_core::CcteamPaths {
        root: fx.ccteam_home.clone(),
        projects_root: fx.projects_root.clone(),
    };
    let state = ProjectState::load(&paths.project_state(&fx.slug)).unwrap();
    let record = state.sessions.get("claude-1").expect("claude-1 record");
    assert_eq!(record.tmux_session, "ccteam-flex-demo-claude-1");
    assert_eq!(state.next_sid_seq.get(&HarnessKind::Claude), Some(&2));
    assert!(paths.project_session_dir(&fx.slug, "claude-1").exists());

    let _ = ccteam_core::TmuxSession::from_name(record.tmux_session.clone()).kill();
}

#[test]
fn session_add_codex_no_longer_returns_v0_3_1_stub_error() {
    // V0.4.0 F62 regression guard: CodexAdapter::spawn_session is no
    // longer the V0.3.1 NotImplemented stub. Invoking
    // `ccteam session add --harness=codex` against a non-existent
    // project must fail with the *project-not-found* path (or the
    // tmux/codex-CLI not-found path on hosts without those bins) —
    // never with the legacy "V0.3.2 deferral" stub message.
    let out = Command::new(cct_bin())
        .args(["session", "add", "some-slug", "--harness=codex"])
        .output()
        .expect("spawn ccteam session add --harness=codex");
    assert!(
        !out.status.success(),
        "ccteam session add --harness=codex against unknown project must exit non-zero; \
         stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    // Legacy V0.3.1 stub strings must not surface anymore.
    assert!(
        !stderr.contains("trait-stub in V0.3.1"),
        "V0.3.1 stub message leaked post-F62; got:\n{stderr}",
    );
    assert!(
        !stderr.contains("deferred to V0.3.2"),
        "V0.3.1 stub deferral message leaked post-F62; got:\n{stderr}",
    );
}

#[test]
fn session_add_claude_requires_existing_flex_project() {
    let out = Command::new(cct_bin())
        .args(["session", "add", "missing-slug", "--harness=claude"])
        .output()
        .expect("spawn ccteam session add --harness=claude");
    assert!(
        !out.status.success(),
        "missing project should fail before spawning tmux; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("project not found"),
        "stderr should cite missing project; got:\n{stderr}",
    );
}

#[test]
fn session_ls_prints_registered_sessions() {
    let fx = Fixture::new_flex();
    let out = fx
        .command()
        .args(["session", "ls", &fx.slug])
        .output()
        .expect("spawn session ls");
    assert!(
        out.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("claude-1"), "got:\n{stdout}");
    assert!(
        stdout.contains("ccteam-flex-demo-claude-1"),
        "got:\n{stdout}"
    );
}

#[test]
fn session_attach_unknown_sid_lists_available_sessions() {
    let fx = Fixture::new_flex();
    let out = fx
        .command()
        .args(["session", "attach", &fx.slug, "claude-9"])
        .output()
        .expect("spawn session attach");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unknown session"));
    assert!(stderr.contains("claude-1"));
}

#[test]
fn session_rm_removes_registered_session_even_if_tmux_is_already_gone() {
    let fx = Fixture::new_flex();
    let out = fx
        .command()
        .args(["session", "rm", &fx.slug, "claude-1"])
        .output()
        .expect("spawn session rm");
    assert!(
        out.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let paths = ccteam_core::CcteamPaths {
        root: fx.ccteam_home,
        projects_root: fx.projects_root,
    };
    let state = ProjectState::load(&paths.project_state(&fx.slug)).unwrap();
    assert!(state.sessions.is_empty());
    assert_eq!(state.next_sid_seq.get(&HarnessKind::Claude), Some(&2));
}

#[test]
fn session_subcommands_reject_workflow_projects() {
    let fx = Fixture::new_workflow();
    let out = fx
        .command()
        .args(["session", "ls", &fx.slug])
        .output()
        .expect("spawn session ls workflow");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("only work on flex teams"), "got:\n{stderr}");
}
