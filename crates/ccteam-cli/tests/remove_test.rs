//! `ccteam project rm|stop` integration tests.
//!
//! Covers the reusable remove engine (`run_remove`) reached via the
//! v0.8.6 W3 `ccteam project` group (the flat `ccteam remove` alias was
//! deleted in v0.8.6 W4a — `project rm` is the only path now):
//!   t01–t02   dry-run + basic deregister (no fs change / config drop)
//!   t03       --purge deletes ccteam footprint ONLY (W2 layout): .ccteam/
//!             + ccteam hooks in settings.local.json; keeps all user roles
//!             (including cto.md), root workflow.yaml, CLAUDE.md, .env,
//!             settings.json, business code
//!   t03b/c    surgical settings.local.json hook strip / empty-file delete
//!   t04–t06   refusal gate (tmux / claude bg / open spawn) + --force
//!   t07–t08   daemon unroster trigger ack / timeout
//!   t09–t11   state/im/registry/<slug>/ purge semantics
//!   t12–t13   `ccteam project rm` group routing + dry-run
//!   t14–t15   `ccteam project stop` (no-session ok / kills matching
//!             `ccteam-chat-<slug>-*` panes, dash-aware slug match)
//!   t16–t17   `ccteam project rm` non-purge keeps project files /
//!             --purge deletes footprint + provably keeps user content
//!   t18       `ccteam project rm --dry-run` lists stop targets, acts
//!             on nothing
//!
//! All tests sandbox `HOME`, `CCTEAM_HOME`, `CCTEAM_PROJECTS_ROOT`, and
//! `CCTEAM_CLAUDE_JOBS_DIR` so they never touch the developer's real
//! filesystem.

use std::path::PathBuf;
use std::process::Command;

use ccteam_core::{config, CcteamPaths, ProjectEntry, ProjectState};
use chrono::Utc;
use serde_json::json;
use tempfile::TempDir;

fn cct_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ccteam")
}

struct Fixture {
    _tmp: TempDir,
    ccteam_home: PathBuf,
    projects_root: PathBuf,
    claude_jobs_dir: PathBuf,
    slug: String,
    project_dir: PathBuf,
}

impl Fixture {
    /// Build a sandboxed project layout:
    ///   <tmp>/ccteam-home/                    -- `CCTEAM_HOME`
    ///   <tmp>/projects/<slug>/.ccteam/        -- the project dir
    ///   <tmp>/jobs/                           -- `CCTEAM_CLAUDE_JOBS_DIR`
    /// Registers the slug in `~/.ccteam/config.yaml::projects[]`.
    fn new(slug: &str) -> Self {
        let tmp = TempDir::new().unwrap();
        let ccteam_home = tmp.path().join("ccteam-home");
        let projects_root = tmp.path().join("projects");
        let claude_jobs_dir = tmp.path().join("jobs");
        std::fs::create_dir_all(&ccteam_home).unwrap();
        std::fs::create_dir_all(&projects_root).unwrap();
        std::fs::create_dir_all(&claude_jobs_dir).unwrap();
        let project_dir = projects_root.join(slug);
        std::fs::create_dir_all(project_dir.join(".ccteam")).unwrap();
        std::fs::create_dir_all(project_dir.join(".claude").join("agents")).unwrap();
        // Drop a state.json so `project_dir` resolution works under
        // CcteamPaths::project_dir even before we register.
        let state = ProjectState::initial_for_team(slug.into(), "dev".into());
        state
            .save(&CcteamPaths::project_state_in(&project_dir))
            .unwrap();
        // Register in config.yaml so the runtime path matches what
        // `ccteam init` would have done.
        config::upsert_project(
            &ccteam_home,
            ProjectEntry {
                slug: slug.into(),
                path: project_dir.clone(),
                host: ccteam_core::LOCAL_HOST.to_string(),
                remote_slug: None,
                remote_path: None,
                team: "dev".into(),
                installed_at: Utc::now(),
            },
        )
        .unwrap();
        Self {
            _tmp: tmp,
            ccteam_home,
            projects_root,
            claude_jobs_dir,
            slug: slug.into(),
            project_dir,
        }
    }

    fn cmd(&self) -> Command {
        let mut c = Command::new(cct_bin());
        c.env("HOME", self._tmp.path())
            .env("CCTEAM_HOME", &self.ccteam_home)
            .env("CCTEAM_PROJECTS_ROOT", &self.projects_root)
            .env("CCTEAM_CLAUDE_JOBS_DIR", &self.claude_jobs_dir);
        c
    }

    fn paths(&self) -> CcteamPaths {
        CcteamPaths {
            root: self.ccteam_home.clone(),
            projects_root: self.projects_root.clone(),
        }
    }

    /// Drop a progress.jsonl with one closed agent_spawn pair so the
    /// liveness probe sees no open sessions (refusal gate must not fire).
    fn seed_closed_progress(&self) {
        let p = self.paths().progress_jsonl(&self.slug);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(
            &p,
            "{\"event\":\"workflow_start\",\"slug\":\"x\",\"ts\":\"2026-01-01T00:00:00Z\"}\n",
        )
        .unwrap();
    }

    /// Seed `<jobs>/<id>/state.json` describing a *live* claude --bg
    /// job whose `cwd` points at the project dir → triggers the
    /// claude bg refusal branch.
    fn seed_live_claude_bg(&self, job_id: &str) {
        let dir = self.claude_jobs_dir.join(job_id);
        std::fs::create_dir_all(&dir).unwrap();
        let body = json!({
            "state": "working",
            "cwd": self.project_dir.to_string_lossy(),
            "firstTerminalAt": null,
            "cost_usd": 0.05,
        });
        std::fs::write(dir.join("state.json"), body.to_string()).unwrap();
    }

    /// Seed an *open* agent_spawn row (no matching agent_done) backed
    /// by a job_id whose state.json reports state=working. F81 refusal
    /// gate fires on this — the orchestrator would clean it up on the
    /// next tick, but the user shouldn't blindly remove with active
    /// session activity in flight.
    fn seed_open_agent_spawn(&self, sid: &str, job_id: &str) {
        // Live state.json for the spawn's job_id.
        let dir = self.claude_jobs_dir.join(job_id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("state.json"),
            json!({
                "state": "working",
                "cwd": "/some/other/path",  // unrelated cwd: stays out of branch (2)
                "firstTerminalAt": null,
                "cost_usd": 0.01,
            })
            .to_string(),
        )
        .unwrap();
        let p = self.paths().progress_jsonl(&self.slug);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let line = json!({
            "event": "agent_spawn",
            "slug": self.slug,
            "role": "explorer",
            "session_id": sid,
            "job_id": job_id,
            "ts": "2026-01-01T00:00:00Z",
        });
        std::fs::write(&p, format!("{line}\n")).unwrap();
    }
}

#[test]
fn t01_remove_dry_run_prints_only() {
    let fx = Fixture::new("dex-ui");
    // Seed orchestration state to confirm dry-run reports them as
    // "would remove" but leaves them on disk.
    let progress = fx.paths().progress_jsonl(&fx.slug);
    std::fs::create_dir_all(progress.parent().unwrap()).unwrap();
    std::fs::write(&progress, "{}\n").unwrap();

    let out = fx
        .cmd()
        .args(["project", "rm", &fx.slug, "--dry-run"])
        .output()
        .expect("spawn ccteam remove --dry-run");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "remove --dry-run should succeed; stderr: {stderr}; stdout: {stdout}",
    );
    assert!(
        stdout.contains("[dry-run]"),
        "dry-run header missing; got: {stdout}",
    );
    assert!(
        stdout.contains("would drop config.yaml::projects entry"),
        "missing config-drop preview; got: {stdout}",
    );
    assert!(
        stdout.contains("would remove progress.jsonl"),
        "missing progress.jsonl preview; got: {stdout}",
    );
    // Filesystem must be untouched.
    assert!(
        progress.exists(),
        "progress.jsonl was deleted under --dry-run; bug",
    );
    let cfg = config::load(&fx.ccteam_home).unwrap();
    assert_eq!(
        cfg.projects.len(),
        1,
        "config.yaml::projects must keep the entry under --dry-run",
    );
}

#[test]
fn t02_remove_basic_drops_config_entry() {
    let fx = Fixture::new("dex-ui");
    fx.seed_closed_progress();
    let progress = fx.paths().progress_jsonl(&fx.slug);
    assert!(progress.exists(), "fixture: progress.jsonl seeded");

    let out = fx
        .cmd()
        .args(["project", "rm", &fx.slug])
        .output()
        .expect("spawn ccteam remove");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "remove should succeed when no active sessions; stderr: {stderr}; stdout: {stdout}",
    );
    // Config entry gone.
    let cfg = config::load(&fx.ccteam_home).unwrap();
    assert!(
        cfg.projects.iter().all(|p| p.slug != fx.slug),
        "config.yaml::projects still contains `{}`; cfg: {cfg:?}",
        fx.slug,
    );
    // Progress.jsonl gone.
    assert!(
        !progress.exists(),
        "progress.jsonl should be deleted; still at {}",
        progress.display(),
    );
    // Project dir + .ccteam/ untouched (no --purge).
    assert!(
        fx.project_dir.join(".ccteam").is_dir(),
        ".ccteam should survive without --purge",
    );
}

#[test]
fn t03_purge_clears_ccteam_footprint_only() {
    // `--purge` deletes exactly ccteam's footprint (.ccteam/ + ccteam hooks
    // in settings.local.json) and leaves every user role, root workflow.yaml,
    // CLAUDE.md, .env, settings.json, and business code in place.
    let fx = Fixture::new("dex-ui");
    fx.seed_closed_progress();
    // .ccteam/ already exists from the fixture; drop a state.json inside
    // so we can confirm the whole dir goes.
    std::fs::write(
        fx.project_dir.join(".ccteam").join("workflow.yaml"),
        "name: x\n",
    )
    .unwrap();
    // KEEP set:
    std::fs::write(fx.project_dir.join(".env"), "SECRET=hunter2\n").unwrap();
    std::fs::write(fx.project_dir.join("README.md"), "# real code\n").unwrap();
    std::fs::write(fx.project_dir.join("CLAUDE.md"), "# project memory\n").unwrap();
    // A root-level workflow.yaml is NOT ccteam's footprint post-W2 (it
    // lives under .ccteam/) — must survive.
    std::fs::write(fx.project_dir.join("workflow.yaml"), "user: stuff\n").unwrap();
    // User's committed settings.json — ccteam never touches it.
    std::fs::write(
        fx.project_dir.join(".claude").join("settings.json"),
        "{\"permissions\":{}}\n",
    )
    .unwrap();
    // KEEP set: v0.9.0 seeds no roles, so cto.md and reviewer.md are both
    // user-owned files.
    let agents = fx.project_dir.join(".claude").join("agents");
    std::fs::write(agents.join("cto.md"), "---\n---\nuser cto").unwrap();
    std::fs::write(agents.join("reviewer.md"), "---\n---\nuser role").unwrap();

    let out = fx
        .cmd()
        .args(["project", "rm", &fx.slug, "--purge"])
        .output()
        .expect("spawn ccteam remove --purge");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "remove --purge should succeed; stderr: {stderr}; stdout: {stdout}",
    );

    // .ccteam/ gone (covers .ccteam/workflow.yaml too).
    assert!(
        !fx.project_dir.join(".ccteam").exists(),
        ".ccteam/ should be purged",
    );
    // Every role file survives, including a user-authored cto.md.
    assert!(
        agents.join("cto.md").exists(),
        "user-owned .claude/agents/cto.md must survive --purge",
    );
    assert!(
        agents.join("reviewer.md").exists(),
        "user work-role .claude/agents/reviewer.md must survive --purge",
    );
    assert!(
        agents.is_dir(),
        ".claude/agents/ dir must survive (all role files are user-owned)",
    );
    // root workflow.yaml is NOT ccteam's footprint post-W2 — must survive.
    assert!(
        fx.project_dir.join("workflow.yaml").exists(),
        "root workflow.yaml must survive --purge (W2: ccteam's lives under .ccteam/)",
    );
    // .env preserved — CLAUDE.md §三 red line.
    assert!(
        fx.project_dir.join(".env").exists(),
        ".env must NEVER be deleted; still expected at {}",
        fx.project_dir.join(".env").display(),
    );
    // CLAUDE.md + business code preserved.
    assert!(
        fx.project_dir.join("CLAUDE.md").exists(),
        "project CLAUDE.md must survive --purge",
    );
    assert!(
        fx.project_dir.join("README.md").exists(),
        "business code must survive --purge",
    );
    // User's committed settings.json untouched.
    assert!(
        fx.project_dir
            .join(".claude")
            .join("settings.json")
            .exists(),
        "user settings.json must NEVER be touched by ccteam",
    );
}

#[test]
fn t03b_purge_strips_chat_hooks_surgically_keeps_other_keys() {
    // settings.local.json holds ccteam chat hooks + an operator key.
    // --purge must strip only the ccteam hooks and keep the rest (file
    // survives because it still has a non-ccteam key).
    let fx = Fixture::new("dex-hooks");
    fx.seed_closed_progress();
    let settings_local = fx.project_dir.join(".claude").join("settings.local.json");
    std::fs::write(
        &settings_local,
        r#"{
  "permissions": {"allow": ["Bash"]},
  "hooks": {
    "SessionStart": [{"matcher": "*", "hooks": [{"type": "command", "command": "/h/hook.sh chat-progress session-start"}]}],
    "PreToolUse": [
      {"matcher": "*", "hooks": [{"type": "command", "command": "/h/hook.sh chat-progress pre-tool-use"}]},
      {"matcher": "AskUserQuestion", "hooks": [{"type": "command", "command": "/h/hook.sh intercept-ask", "timeout": 660}]},
      {"matcher": "Edit", "hooks": [{"type": "command", "command": "/h/my-own-linter.sh"}]}
    ]
  }
}
"#,
    )
    .unwrap();

    let out = fx
        .cmd()
        .args(["project", "rm", &fx.slug, "--purge"])
        .output()
        .expect("spawn ccteam remove --purge");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "remove --purge should succeed; stderr: {stderr}; stdout: {stdout}",
    );

    assert!(
        settings_local.exists(),
        "settings.local.json must survive (operator key + own hook remain)",
    );
    let body = std::fs::read_to_string(&settings_local).unwrap();
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    // Operator key preserved.
    assert!(
        v.get("permissions").is_some(),
        "operator `permissions` key must survive; got: {body}",
    );
    // ccteam chat hooks gone.
    assert!(
        !body.contains("chat-progress") && !body.contains("intercept-ask"),
        "ccteam chat hooks must be stripped; got: {body}",
    );
    // SessionStart event array (only had ccteam's hook) pruned entirely.
    assert!(
        v.get("hooks").and_then(|h| h.get("SessionStart")).is_none(),
        "emptied SessionStart event must be pruned; got: {body}",
    );
    // Operator's own PreToolUse Edit hook preserved.
    assert!(
        body.contains("my-own-linter.sh"),
        "operator's own PreToolUse hook must survive; got: {body}",
    );
}

#[test]
fn t03b2_purge_strips_hitl_permission_request_hook() {
    // v0.8.7 review-fix (R-M4) — a hitl-spawned session installs the
    // `PermissionRequest` hook (`{hook_sh} permission-request`). `project rm
    // --purge` is the `init` inverse and MUST clear it (red line), or the
    // deregistered project keeps a live HITL approval gate. End-to-end through
    // the real `ccteam project rm --purge` binary, not just the predicate.
    let fx = Fixture::new("dex-hitl");
    fx.seed_closed_progress();
    let settings_local = fx.project_dir.join(".claude").join("settings.local.json");
    std::fs::write(
        &settings_local,
        r#"{
  "permissions": {"allow": ["Bash"]},
  "hooks": {
    "SessionStart": [{"matcher": "*", "hooks": [{"type": "command", "command": "/h/hook.sh chat-progress session-start"}]}],
    "PermissionRequest": [{"hooks": [{"type": "command", "command": "/h/hook.sh permission-request"}]}]
  }
}
"#,
    )
    .unwrap();

    let out = fx
        .cmd()
        .args(["project", "rm", &fx.slug, "--purge"])
        .output()
        .expect("spawn ccteam remove --purge");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "remove --purge should succeed; stderr: {stderr}; stdout: {stdout}",
    );

    // The file survives (operator `permissions` key remains) but the HITL hook
    // and its now-empty PermissionRequest section are gone.
    assert!(
        settings_local.exists(),
        "settings.local.json survives (operator key remains)",
    );
    let body = std::fs::read_to_string(&settings_local).unwrap();
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(v.get("permissions").is_some(), "operator key kept: {body}");
    assert!(
        !body.contains("permission-request"),
        "HITL PermissionRequest hook must be purged (R-M4); got: {body}",
    );
    assert!(
        v.get("hooks")
            .and_then(|h| h.get("PermissionRequest"))
            .is_none(),
        "emptied PermissionRequest section must be pruned; got: {body}",
    );
}

#[test]
fn t03c_purge_deletes_settings_local_when_it_collapses_to_empty() {
    // settings.local.json that holds ONLY ccteam chat hooks is the file
    // ccteam created — after stripping it collapses to {} and --purge
    // deletes the vestigial file.
    let fx = Fixture::new("dex-empty");
    fx.seed_closed_progress();
    let settings_local = fx.project_dir.join(".claude").join("settings.local.json");
    std::fs::write(
        &settings_local,
        r#"{
  "hooks": {
    "SessionStart": [{"matcher": "*", "hooks": [{"type": "command", "command": "/h/hook.sh chat-progress session-start"}]}],
    "Stop": [{"matcher": "*", "hooks": [{"type": "command", "command": "/h/hook.sh chat-progress stop"}]}]
  }
}
"#,
    )
    .unwrap();

    let out = fx
        .cmd()
        .args(["project", "rm", &fx.slug, "--purge"])
        .output()
        .expect("spawn ccteam remove --purge");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "remove --purge should succeed; stderr: {stderr}; stdout: {stdout}",
    );
    assert!(
        !settings_local.exists(),
        "settings.local.json that held only ccteam hooks should be deleted; stdout: {stdout}",
    );
    assert!(
        stdout.contains("removed now-empty"),
        "step list must report the now-empty file deletion; got: {stdout}",
    );
}

/// t12 — the grouped `ccteam project rm` is the same engine as the flat
/// `ccteam remove`: it drops the config entry and (with --purge) clears
/// the footprint.
#[test]
fn t12_project_rm_alias_drops_config_entry() {
    let fx = Fixture::new("dex-grp");
    fx.seed_closed_progress();
    let progress = fx.paths().progress_jsonl(&fx.slug);

    let out = fx
        .cmd()
        .args(["project", "rm", &fx.slug])
        .output()
        .expect("spawn ccteam project rm");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "project rm should succeed; stderr: {stderr}; stdout: {stdout}",
    );
    let cfg = config::load(&fx.ccteam_home).unwrap();
    assert!(
        cfg.projects.iter().all(|p| p.slug != fx.slug),
        "config.yaml::projects still contains `{}` after project rm",
        fx.slug,
    );
    assert!(
        !progress.exists(),
        "progress.jsonl should be deleted by project rm",
    );
}

/// t13 — `ccteam project rm --dry-run` acts on nothing.
#[test]
fn t13_project_rm_dry_run_acts_on_nothing() {
    let fx = Fixture::new("dex-grp-dry");
    let progress = fx.paths().progress_jsonl(&fx.slug);
    std::fs::create_dir_all(progress.parent().unwrap()).unwrap();
    std::fs::write(&progress, "{}\n").unwrap();

    let out = fx
        .cmd()
        .args(["project", "rm", &fx.slug, "--dry-run"])
        .output()
        .expect("spawn ccteam project rm --dry-run");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "project rm --dry-run should succeed");
    assert!(
        stdout.contains("[dry-run]"),
        "dry-run header missing; got: {stdout}"
    );
    assert!(progress.exists(), "dry-run must not delete progress.jsonl");
    let cfg = config::load(&fx.ccteam_home).unwrap();
    assert_eq!(cfg.projects.len(), 1, "dry-run must keep config entry");
}

/// t14 — `ccteam project stop <slug>` with no live sessions succeeds and
/// reports zero stopped (stop is not an error when nothing is running).
/// This is the tmux-free baseline; t15 proves the actual kill path.
#[test]
fn t14_project_stop_no_sessions_is_ok() {
    let fx = Fixture::new("dex-stop");
    let out = fx
        .cmd()
        .args(["project", "stop", &fx.slug])
        .output()
        .expect("spawn ccteam project stop");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "project stop must succeed even with no live sessions; stderr: {stderr}",
    );
    assert!(
        stdout.contains("stopped 0 chat sessions"),
        "stop must report zero sessions stopped; got: {stdout}",
    );
}

/// t15 — `ccteam project stop <slug>` kills the matching
/// `ccteam-chat-<slug>-*` tmux sessions and leaves a sibling project's
/// session (a slug that is a prefix of ours, to prove the dash-aware
/// parse) untouched. Guarded by tmux availability.
#[test]
fn t15_project_stop_kills_matching_chat_sessions() {
    use ccteam_harness::chat_session_name;
    use ccteam_harness::tmux_ops::{tmux_available, TmuxSession};

    if !tmux_available() {
        eprintln!("skipping t15: tmux not available");
        return;
    }

    // Pick a slug whose dash structure would alias a sibling under a
    // naive `starts_with`: `dex-stop` vs the sibling `dex`. The CLI must
    // stop only `dex-stop`'s sessions and leave `dex`'s alone.
    let suffix = std::process::id();
    let slug = format!("dexstop-{suffix}");
    let sibling = format!("dexstop-{suffix}-extra"); // longer slug, NOT ours
    let fx = Fixture::new(&slug);

    let ours_a = chat_session_name(&slug, "cto");
    let ours_b = chat_session_name(&slug, "reviewer");
    // sibling's role session — note its (slug, role) parses to
    // (`dexstop-<id>-extra`, `bob`), a DIFFERENT slug than ours.
    let sib = chat_session_name(&sibling, "bob");

    // Best-effort pre-clean in case a prior crashed run left them.
    for name in [&ours_a, &ours_b, &sib] {
        TmuxSession::from_name(name.clone()).kill().ok();
    }

    // Create three live detached sessions running a long-lived `sleep`.
    for name in [&ours_a, &ours_b, &sib] {
        let status = std::process::Command::new("tmux")
            .args(["new-session", "-d", "-s", name, "sleep", "300"])
            .status()
            .expect("tmux new-session");
        assert!(status.success(), "pre-create tmux session {name} failed");
    }
    assert!(TmuxSession::from_name(ours_a.clone()).exists());
    assert!(TmuxSession::from_name(ours_b.clone()).exists());
    assert!(TmuxSession::from_name(sib.clone()).exists());

    let out = fx
        .cmd()
        // This test seeds + asserts on real `tmux` sessions, so pin the CLI to
        // the tmux backend (the default is now `rmux`, which wouldn't see them).
        .env("CCTEAM_MUX_BACKEND", "tmux")
        .args(["project", "stop", &slug])
        .output()
        .expect("spawn ccteam project stop");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // Always clean the sibling so we don't leak it past the assertions.
    let sib_alive_after = TmuxSession::from_name(sib.clone()).exists();
    TmuxSession::from_name(sib.clone()).kill().ok();

    assert!(
        out.status.success(),
        "project stop must succeed; stderr: {stderr}; stdout: {stdout}",
    );
    assert!(
        !TmuxSession::from_name(ours_a.clone()).exists(),
        "our chat session {ours_a} must be killed; stdout: {stdout}",
    );
    assert!(
        !TmuxSession::from_name(ours_b.clone()).exists(),
        "our chat session {ours_b} must be killed; stdout: {stdout}",
    );
    assert!(
        sib_alive_after,
        "sibling slug's session {sib} must NOT be killed (dash-aware slug match)",
    );
    assert!(
        stdout.contains("stopped 2 chat sessions"),
        "stop must report two sessions stopped; got: {stdout}",
    );
}

/// t16 — `ccteam project rm` (non-purge) drops the registry + ~/.ccteam
/// per-slug state but PROVABLY keeps the project's on-disk files
/// (.ccteam/, seeded cto.md, user role, CLAUDE.md, .env, settings.json).
#[test]
fn t16_project_rm_nonpurge_keeps_project_files() {
    let fx = Fixture::new("dex-keep");
    fx.seed_closed_progress();
    let progress = fx.paths().progress_jsonl(&fx.slug);
    // Seed the ccteam footprint + user content that non-purge must keep.
    std::fs::write(
        fx.project_dir.join(".ccteam").join("workflow.yaml"),
        "name: x\n",
    )
    .unwrap();
    std::fs::write(fx.project_dir.join(".env"), "SECRET=hunter2\n").unwrap();
    std::fs::write(fx.project_dir.join("CLAUDE.md"), "# memory\n").unwrap();
    let agents = fx.project_dir.join(".claude").join("agents");
    std::fs::write(agents.join("cto.md"), "---\n---\nseeded cto").unwrap();
    std::fs::write(agents.join("reviewer.md"), "---\n---\nuser role").unwrap();
    std::fs::write(
        fx.project_dir.join(".claude").join("settings.json"),
        "{\"permissions\":{}}\n",
    )
    .unwrap();

    let out = fx
        .cmd()
        .args(["project", "rm", &fx.slug])
        .output()
        .expect("spawn ccteam project rm");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "project rm should succeed; stderr: {stderr}; stdout: {stdout}",
    );

    // Registry + ~/.ccteam state gone …
    let cfg = config::load(&fx.ccteam_home).unwrap();
    assert!(
        cfg.projects.iter().all(|p| p.slug != fx.slug),
        "config.yaml::projects must drop the slug under project rm",
    );
    assert!(
        !progress.exists(),
        "progress.jsonl should be deleted by project rm",
    );
    // … but EVERY project-dir file survives (non-purge keeps them all).
    assert!(
        fx.project_dir.join(".ccteam").is_dir(),
        ".ccteam/ must survive without --purge",
    );
    assert!(
        agents.join("cto.md").exists(),
        "seeded cto.md must survive without --purge",
    );
    assert!(
        agents.join("reviewer.md").exists(),
        "user role must survive without --purge",
    );
    assert!(
        fx.project_dir.join(".env").exists(),
        ".env must survive without --purge",
    );
    assert!(
        fx.project_dir.join("CLAUDE.md").exists(),
        "CLAUDE.md must survive without --purge",
    );
    assert!(
        fx.project_dir
            .join(".claude")
            .join("settings.json")
            .exists(),
        "user settings.json must survive without --purge",
    );
}

/// t17 — `ccteam project rm --purge` deletes ccteam's footprint via the
/// GROUP path (.ccteam/ + settings.local.json hook section +
/// config entry + ~/.ccteam/{progress,state/im/registry}/<slug>) and PROVABLY
/// keeps all user roles, CLAUDE.md, .env, and the user's settings.json.
#[test]
fn t17_project_rm_purge_via_group() {
    let fx = Fixture::new("dex-grp-purge");
    fx.seed_closed_progress();
    // state/im/registry/<slug>/ — must be purged.
    let (reg, hb) = seed_imd_registry(&fx, "helper");
    let slug_dir = ccteam_im::registry_root_in(&fx.ccteam_home).join(&fx.slug);
    // ccteam footprint.
    std::fs::write(
        fx.project_dir.join(".ccteam").join("workflow.yaml"),
        "name: x\n",
    )
    .unwrap();
    let agents = fx.project_dir.join(".claude").join("agents");
    // KEEP set: v0.9.0 seeds no cto role; both files are user-owned.
    std::fs::write(agents.join("cto.md"), "---\n---\nuser cto").unwrap();
    std::fs::write(agents.join("reviewer.md"), "---\n---\nuser role").unwrap();
    std::fs::write(fx.project_dir.join(".env"), "SECRET=hunter2\n").unwrap();
    std::fs::write(fx.project_dir.join("CLAUDE.md"), "# memory\n").unwrap();
    std::fs::write(
        fx.project_dir.join(".claude").join("settings.json"),
        "{\"permissions\":{}}\n",
    )
    .unwrap();
    // settings.local.json with only ccteam hooks → collapses + deleted.
    let settings_local = fx.project_dir.join(".claude").join("settings.local.json");
    std::fs::write(
        &settings_local,
        r#"{
  "hooks": {
    "SessionStart": [{"matcher": "*", "hooks": [{"type": "command", "command": "/h/hook.sh chat-progress session-start"}]}]
  }
}
"#,
    )
    .unwrap();

    let out = fx
        .cmd()
        .args(["project", "rm", &fx.slug, "--purge"])
        .output()
        .expect("spawn ccteam project rm --purge");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "project rm --purge should succeed; stderr: {stderr}; stdout: {stdout}",
    );

    // DELETE set.
    let cfg = config::load(&fx.ccteam_home).unwrap();
    assert!(
        cfg.projects.iter().all(|p| p.slug != fx.slug),
        "config entry must be dropped",
    );
    assert!(
        !fx.project_dir.join(".ccteam").exists(),
        ".ccteam/ must be purged",
    );
    assert!(
        !settings_local.exists(),
        "settings.local.json (only ccteam hooks) must be deleted",
    );
    assert!(
        !reg.exists(),
        "state/im/registry/<slug>/helper.json must be purged"
    );
    assert!(!hb.exists(), "state/im/registry heartbeat must be purged");
    assert!(
        !slug_dir.exists(),
        "state/im/registry/<slug>/ dir must be purged"
    );
    assert!(
        !fx.paths().progress_jsonl(&fx.slug).exists(),
        "progress.jsonl must be purged",
    );

    // KEEP set — provably untouched.
    assert!(
        agents.join("cto.md").exists(),
        "user-owned .claude/agents/cto.md must survive --purge",
    );
    assert!(
        agents.join("reviewer.md").exists(),
        "user role .claude/agents/reviewer.md must survive --purge",
    );
    assert!(agents.is_dir(), ".claude/agents/ dir must survive");
    assert!(
        fx.project_dir.join(".env").exists(),
        ".env must NEVER be deleted",
    );
    assert!(
        fx.project_dir.join("CLAUDE.md").exists(),
        "CLAUDE.md must survive --purge",
    );
    assert!(
        fx.project_dir
            .join(".claude")
            .join("settings.json")
            .exists(),
        "user settings.json must NEVER be touched",
    );
}

/// t18 — `ccteam project rm --dry-run` with a live chat session lists the
/// session it WOULD stop and the config/state it WOULD drop, and changes
/// nothing on disk or tmux. Guarded by tmux availability.
#[test]
fn t18_project_rm_dry_run_lists_stop_and_acts_on_nothing() {
    use ccteam_harness::chat_session_name;
    use ccteam_harness::tmux_ops::{tmux_available, TmuxSession};

    if !tmux_available() {
        eprintln!("skipping t18: tmux not available");
        return;
    }
    let slug = format!("dexdry-{}", std::process::id());
    let fx = Fixture::new(&slug);
    let progress = fx.paths().progress_jsonl(&slug);
    std::fs::create_dir_all(progress.parent().unwrap()).unwrap();
    std::fs::write(&progress, "{}\n").unwrap();

    let sess = chat_session_name(&slug, "cto");
    TmuxSession::from_name(sess.clone()).kill().ok();
    let status = std::process::Command::new("tmux")
        .args(["new-session", "-d", "-s", &sess, "sleep", "300"])
        .status()
        .expect("tmux new-session");
    assert!(status.success(), "pre-create tmux session failed");

    let out = fx
        .cmd()
        // Seeds + asserts on a real `tmux` session, so pin the CLI to the tmux
        // backend (the default is now `rmux`, which wouldn't enumerate it).
        .env("CCTEAM_MUX_BACKEND", "tmux")
        .args(["project", "rm", &slug, "--dry-run"])
        .output()
        .expect("spawn ccteam project rm --dry-run");
    let stdout = String::from_utf8_lossy(&out.stdout);

    let still_alive = TmuxSession::from_name(sess.clone()).exists();
    TmuxSession::from_name(sess.clone()).kill().ok(); // cleanup regardless

    assert!(out.status.success(), "dry-run rm should succeed");
    assert!(
        stdout.contains(&format!("would stop chat session `{sess}`")),
        "dry-run must list the chat session it would stop; got: {stdout}",
    );
    assert!(
        stdout.contains("[dry-run]"),
        "dry-run header missing; got: {stdout}",
    );
    // Nothing acted on.
    assert!(still_alive, "dry-run must NOT kill the chat session");
    assert!(progress.exists(), "dry-run must not delete progress.jsonl");
    let cfg = config::load(&fx.ccteam_home).unwrap();
    assert_eq!(cfg.projects.len(), 1, "dry-run must keep config entry");
}

#[test]
fn t04_refuses_with_active_tmux() {
    // tmux liveness on CI may or may not be mockable. We force the
    // tmux path by *not* having tmux at all (`tmux has-session` exits
    // non-zero → exists() == false) AND seed an *open* agent_spawn
    // so the third refusal arm fires regardless of tmux availability.
    // This exercises the refusal-message rendering + bail exit shape
    // even when the host has no live tmux server.
    //
    // (A pure tmux test would require a real tmux daemon; we trade
    // coverage of arm 1 for portability.)
    let fx = Fixture::new("dex-ui");
    fx.seed_open_agent_spawn("sid-1", "abc12345");

    let out = fx
        .cmd()
        .args(["project", "rm", &fx.slug])
        .output()
        .expect("spawn ccteam remove");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "remove must refuse with running agent_spawn; stderr: {stderr}",
    );
    assert!(
        stderr.contains("refusing"),
        "stderr must explain refusal; got: {stderr}",
    );
    // Refusal cites either the agent-spawn arm or tmux arm; both
    // mention `--force` as the override knob.
    assert!(
        stderr.contains("--force"),
        "refusal must hint at --force override; got: {stderr}",
    );
    // Config entry must still be present (no mutation on refusal).
    let cfg = config::load(&fx.ccteam_home).unwrap();
    assert!(
        cfg.projects.iter().any(|p| p.slug == fx.slug),
        "config.yaml::projects must keep the slug when refused",
    );
}

#[test]
fn t05_refuses_with_running_claude_bg() {
    let fx = Fixture::new("dex-ui");
    fx.seed_live_claude_bg("deadbeef");

    let out = fx
        .cmd()
        .args(["project", "rm", &fx.slug])
        .output()
        .expect("spawn ccteam remove");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "remove must refuse when a claude --bg job points at the project; stderr: {stderr}",
    );
    assert!(
        stderr.contains("claude --bg job") || stderr.contains("refusing"),
        "refusal message must cite the claude bg branch; got: {stderr}",
    );
    // Config entry intact.
    let cfg = config::load(&fx.ccteam_home).unwrap();
    assert!(
        cfg.projects.iter().any(|p| p.slug == fx.slug),
        "config.yaml::projects must keep the slug when refused",
    );
}

/// Bind a fake MCP socket listener so daemon reachability is true for
/// the sandbox. The returned listener must stay alive while the spawned
/// CLI process runs.
fn seed_daemon(fx: &Fixture) -> std::os::unix::net::UnixListener {
    let socket = ccteam_core::daemon::daemon_socket_path(&fx.paths());
    std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
    std::os::unix::net::UnixListener::bind(socket).unwrap()
}

/// Compute the unroster trigger path the CLI writes (mirrors
/// `commands::unroster_trigger_path`). Shares the USER env so the test
/// process and the spawned ccteam subprocess agree on the path.
fn unroster_trigger(slug: &str) -> std::path::PathBuf {
    let user = std::env::var("USER").unwrap_or_else(|_| "ccteam".into());
    std::path::PathBuf::from("/tmp").join(format!("ccteam-{user}.unroster.{slug}"))
}

/// t07: when the daemon socket is reachable, `ccteam remove` writes the
/// per-slug unroster trigger file and polls for it to disappear. A
/// background "acker" thread simulates the daemon consuming the trigger.
#[test]
fn t07_remove_writes_unroster_trigger_when_daemon_alive() {
    let fx = Fixture::new("dex-ui-t07");
    fx.seed_closed_progress();
    let _daemon = seed_daemon(&fx);

    let trigger = unroster_trigger(&fx.slug);
    // Clean up any leftover from a prior run.
    let _ = std::fs::remove_file(&trigger);

    // Background thread simulates the daemon's poll_unroster_triggers task:
    // it watches for the trigger file and deletes it within 20ms of creation.
    let trigger_for_thread = trigger.clone();
    let acker = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if trigger_for_thread.exists() {
                let _ = std::fs::remove_file(&trigger_for_thread);
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        false
    });

    let out = fx
        .cmd()
        .args(["project", "rm", &fx.slug])
        .output()
        .expect("spawn ccteam remove");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "remove should succeed with daemon alive; stderr: {stderr}; stdout: {stdout}",
    );
    let acked = acker.join().unwrap();
    assert!(acked, "acker must have seen and deleted the trigger file");
    assert!(
        stdout.contains("acknowledged by daemon"),
        "step 4 should report daemon acknowledgment; got: {stdout}",
    );
    assert!(!trigger.exists(), "trigger file must be gone after remove");
}

/// t08: when the daemon socket is reachable but nothing acks the trigger
/// (simulates a slow/stalled daemon), `ccteam remove` times out after 5s,
/// self-cleans the trigger file, and reports the timeout.
///
#[test]
fn t08_remove_timeout_when_daemon_unresponsive() {
    let fx = Fixture::new("dex-ui-t08");
    fx.seed_closed_progress();
    let _daemon = seed_daemon(&fx);

    let trigger = unroster_trigger(&fx.slug);
    let _ = std::fs::remove_file(&trigger);

    let out = fx
        .cmd()
        .args(["project", "rm", &fx.slug])
        .output()
        .expect("spawn ccteam remove");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "timeout must not fail the remove; stderr: {stderr}; stdout: {stdout}",
    );
    assert!(
        stdout.contains("did not acknowledge"),
        "timeout path must be reported; got: {stdout}",
    );
    assert!(
        !trigger.exists(),
        "trigger must be self-cleaned after timeout",
    );
}

#[test]
fn t06_force_overrides_refusal() {
    let fx = Fixture::new("dex-ui");
    fx.seed_live_claude_bg("deadbeef");

    let out = fx
        .cmd()
        .args(["project", "rm", &fx.slug, "--force"])
        .output()
        .expect("spawn ccteam remove --force");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "--force must bypass refusal; stderr: {stderr}; stdout: {stdout}",
    );
    // Should print the "forced through guard" notice.
    assert!(
        stdout.contains("forced through guard"),
        "force should still report the guard it bypassed; got: {stdout}",
    );
    // Config entry gone.
    let cfg = config::load(&fx.ccteam_home).unwrap();
    assert!(
        cfg.projects.iter().all(|p| p.slug != fx.slug),
        "config.yaml::projects must drop the slug under --force",
    );
}

// ─────────────────────────── V0.6.5 F151 ────────────────────────────
//
// `ccteam remove <slug> --purge` must also clean
// `~/.ccteam/state/im/registry/<slug>/` (registration JSON + heartbeat
// sidecars). Without `--purge` the registry stays put so a re-init
// of the same slug can resume the existing chat bots.

/// Seed an state/im/registry/<slug>/<role>.json + matching heartbeat
/// sidecar — mirrors the on-disk shape F146's `register_bot_checked_in`
/// produces.
fn seed_imd_registry(fx: &Fixture, role: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    use ccteam_harness::AgentVendor;
    let outcome = ccteam_im::register_bot_checked_in(
        &fx.ccteam_home,
        &fx.slug,
        role,
        AgentVendor::Claude,
        "telegram",
        "42",
        Some(role),
        None,
        None,
    )
    .expect("seed registry");
    let reg_path = match outcome {
        ccteam_im::RegisterOutcome::Registered(p) => p,
        ccteam_im::RegisterOutcome::AlreadyRegistered(p) => p,
    };
    // Drop a heartbeat sidecar so we can assert it gets cleaned too.
    let hb = ccteam_im::bot_heartbeat_path_in(&fx.ccteam_home, &fx.slug, role);
    std::fs::write(&hb, chrono::Utc::now().to_rfc3339()).unwrap();
    assert!(reg_path.exists(), "fixture: registration JSON seeded");
    assert!(hb.exists(), "fixture: heartbeat seeded");
    (reg_path, hb)
}

#[test]
fn t09_purge_cleans_imd_registry_dir() {
    let fx = Fixture::new("dex-bot");
    fx.seed_closed_progress();
    // Seed two roles under the slug — proves the per-role unregister
    // loop + final rm -rf both work.
    let (reg_a, hb_a) = seed_imd_registry(&fx, "helper");
    let (reg_b, hb_b) = seed_imd_registry(&fx, "critic");
    let slug_dir = ccteam_im::registry_root_in(&fx.ccteam_home).join(&fx.slug);
    assert!(
        slug_dir.is_dir(),
        "fixture: state/im/registry/<slug>/ seeded"
    );

    let out = fx
        .cmd()
        .args(["project", "rm", &fx.slug, "--purge"])
        .output()
        .expect("spawn ccteam remove --purge");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "remove --purge should succeed; stderr: {stderr}; stdout: {stdout}",
    );

    // Every role file + heartbeat gone.
    assert!(
        !reg_a.exists(),
        "state/im/registry/<slug>/helper.json should be purged"
    );
    assert!(
        !reg_b.exists(),
        "state/im/registry/<slug>/critic.json should be purged"
    );
    assert!(
        !hb_a.exists(),
        "state/im/registry/<slug>/helper.heartbeat should be purged"
    );
    assert!(
        !hb_b.exists(),
        "state/im/registry/<slug>/critic.heartbeat should be purged"
    );
    // The slug dir itself gone.
    assert!(
        !slug_dir.exists(),
        "state/im/registry/<slug>/ dir should be purged; still at {}",
        slug_dir.display()
    );
    // Progress log mentions the purge so the user can see what happened.
    assert!(
        stdout.contains("state/im/registry/dex-bot/"),
        "purge step must be reported; got: {stdout}",
    );
}

#[test]
fn t10_remove_without_purge_keeps_imd_registry() {
    let fx = Fixture::new("dex-bot");
    fx.seed_closed_progress();
    let (reg_path, hb_path) = seed_imd_registry(&fx, "helper");
    let slug_dir = ccteam_im::registry_root_in(&fx.ccteam_home).join(&fx.slug);

    let out = fx
        .cmd()
        .args(["project", "rm", &fx.slug])
        .output()
        .expect("spawn ccteam remove");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "remove (no --purge) should succeed; stderr: {stderr}; stdout: {stdout}",
    );

    // Without --purge the state/im/registry/<slug>/ tree must survive so a
    // re-`ccteam init` of the same slug picks up where it left off.
    assert!(
        reg_path.exists(),
        "state/im/registry/<slug>/helper.json must survive without --purge",
    );
    assert!(
        hb_path.exists(),
        "state/im/registry/<slug>/helper.heartbeat must survive without --purge",
    );
    assert!(
        slug_dir.is_dir(),
        "state/im/registry/<slug>/ must survive without --purge",
    );
    // Step list must NOT mention the state/im/registry purge step.
    assert!(
        !stdout.contains("state/im/registry/"),
        "non-purge run must not touch state/im/registry/; got: {stdout}",
    );
}

#[test]
fn t11_purge_dry_run_reports_imd_registry_count() {
    let fx = Fixture::new("dex-bot");
    fx.seed_closed_progress();
    let (reg_path, hb_path) = seed_imd_registry(&fx, "helper");

    let out = fx
        .cmd()
        .args(["project", "rm", &fx.slug, "--purge", "--dry-run"])
        .output()
        .expect("spawn ccteam remove --purge --dry-run");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "dry-run --purge should succeed; stderr: {stderr}; stdout: {stdout}",
    );

    // Filesystem untouched under --dry-run.
    assert!(
        reg_path.exists(),
        "dry-run must not delete registration JSON"
    );
    assert!(
        hb_path.exists(),
        "dry-run must not delete heartbeat sidecar"
    );
    // PRD §F151 acceptance #1 — output names the dir + JSON count.
    assert!(
        stdout.contains("would purge state/im/registry/dex-bot/") && stdout.contains("1 JSON file"),
        "dry-run must preview state/im/registry/<slug>/ with count; got: {stdout}",
    );
}
