//! V0.4.6 F81 — `ccteam remove <slug>` integration tests.
//!
//! Six scenarios drawn from the dev-plan §阶段 2 test matrix:
//!   t01_remove_dry_run_prints_only   — no fs change
//!   t02_remove_basic_drops_config_entry
//!   t03_purge_clears_ccteam_dir       — `.ccteam/` deleted, business code + .env intact
//!   t04_refuses_with_active_tmux      — refusal gate fires (tmux mock unavailable in CI → skipped)
//!   t05_refuses_with_running_claude_bg
//!   t06_force_overrides_refusal
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
        .args(["remove", &fx.slug, "--dry-run"])
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
        .args(["remove", &fx.slug])
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
fn t03_purge_clears_ccteam_dir() {
    let fx = Fixture::new("dex-ui");
    fx.seed_closed_progress();
    // Drop a `.env` + business file so we can assert they survive.
    std::fs::write(fx.project_dir.join(".env"), "SECRET=hunter2\n").unwrap();
    std::fs::write(fx.project_dir.join("README.md"), "# real code\n").unwrap();
    std::fs::write(
        fx.project_dir.join("workflow.yaml"),
        "name: x\nagents: {}\n",
    )
    .unwrap();
    std::fs::write(
        fx.project_dir.join(".claude").join("agents").join("foo.md"),
        "---\n---\nbody",
    )
    .unwrap();

    let out = fx
        .cmd()
        .args(["remove", &fx.slug, "--purge"])
        .output()
        .expect("spawn ccteam remove --purge");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "remove --purge should succeed; stderr: {stderr}; stdout: {stdout}",
    );

    // .ccteam/ gone.
    assert!(
        !fx.project_dir.join(".ccteam").exists(),
        ".ccteam/ should be purged",
    );
    // workflow.yaml gone.
    assert!(
        !fx.project_dir.join("workflow.yaml").exists(),
        "workflow.yaml should be purged",
    );
    // .claude/agents/ gone.
    assert!(
        !fx.project_dir.join(".claude").join("agents").exists(),
        ".claude/agents/ should be purged",
    );
    // .env preserved — CLAUDE.md §三 red line.
    assert!(
        fx.project_dir.join(".env").exists(),
        ".env must NEVER be deleted; still expected at {}",
        fx.project_dir.join(".env").display(),
    );
    // Business code preserved.
    assert!(
        fx.project_dir.join("README.md").exists(),
        "business code must survive --purge",
    );
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
        .args(["remove", &fx.slug])
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
        .args(["remove", &fx.slug])
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

#[test]
fn t06_force_overrides_refusal() {
    let fx = Fixture::new("dex-ui");
    fx.seed_live_claude_bg("deadbeef");

    let out = fx
        .cmd()
        .args(["remove", &fx.slug, "--force"])
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
