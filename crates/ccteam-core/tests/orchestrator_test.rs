//! Integration tests for `Orchestrator` — startup validation, the
//! shutdown contract, and that the loop ticks on a short interval.

use std::path::PathBuf;
use std::time::Duration;

use ccteam_core::tmux::{tmux_available, TmuxSession};
use ccteam_core::{
    bootstrap_project, CcteamPaths, Orchestrator, OrchestratorConfig, ProjectState,
};
use tempfile::TempDir;

fn fresh_paths(tmp: &TempDir) -> CcteamPaths {
    CcteamPaths {
        root: tmp.path().join("ccteam-home"),
        projects_root: tmp.path().join("projects"),
    }
}

fn write_template(dir: &PathBuf, file: &str, body: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(file), body).unwrap();
}

#[test]
fn orchestrator_constructs_when_phases_dir_is_absent() {
    let tmp = TempDir::new().unwrap();
    let paths = fresh_paths(&tmp);
    let orch = Orchestrator::new(paths, OrchestratorConfig::default()).unwrap();
    assert!(orch.templates().is_empty());
}

#[test]
fn orchestrator_loads_valid_solo_template() {
    let tmp = TempDir::new().unwrap();
    let paths = fresh_paths(&tmp);
    write_template(
        &paths.phases_dir(),
        "03-implement.md",
        concat!(
            "---\n",
            "name: implement\n",
            "parallelism: solo\n",
            "sub_skills: []\n",
            "---\n",
            "body\n",
        ),
    );

    let orch = Orchestrator::new(paths, OrchestratorConfig::default()).unwrap();
    assert_eq!(orch.templates().len(), 1);
    assert_eq!(orch.templates()[0].name, "implement");
}

#[test]
fn orchestrator_fails_fast_on_agent_team_template() {
    let tmp = TempDir::new().unwrap();
    let paths = fresh_paths(&tmp);
    write_template(
        &paths.phases_dir(),
        "03-implement.md",
        concat!(
            "---\n",
            "name: implement\n",
            "parallelism: agent_team\n",
            "---\n",
            "body\n",
        ),
    );

    let err = Orchestrator::new(paths, OrchestratorConfig::default()).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("solo"),
        "expected M0 validation failure, got: {msg}",
    );
}

#[test]
fn orchestrator_accepts_empty_sub_skills_no_op() {
    // M0 acceptance: empty sub_skills must NOT error (real scheduling
    // is M2). The phase parser already enforces this; the orchestrator
    // contract: load + validate without failing on empty lists.
    let tmp = TempDir::new().unwrap();
    let paths = fresh_paths(&tmp);
    write_template(
        &paths.phases_dir(),
        "03-implement.md",
        concat!(
            "---\n",
            "name: implement\n",
            "parallelism: solo\n",
            "sub_skills: []\n",
            "agent_team: []\n",
            "---\n",
        ),
    );
    Orchestrator::new(paths, OrchestratorConfig::default()).unwrap();
}

/// M0.5.3: phase that asks for an unreachable subagent must fail
/// orchestrator startup with a fix hint. We point CLAUDE_CONFIG_HOME
/// at an empty tempdir so only the five built-in subagents are
/// reachable; `code-reviewer` is then guaranteed missing.
#[test]
fn orchestrator_fails_fast_when_phase_requests_missing_subagent() {
    let tmp = TempDir::new().unwrap();
    let paths = fresh_paths(&tmp);
    write_template(
        &paths.phases_dir(),
        "03-implement.md",
        concat!(
            "---\n",
            "name: implement\n",
            "parallelism: solo\n",
            "tools_required:\n",
            "  subagents: [code-reviewer]\n",
            "---\n",
            "body\n",
        ),
    );

    // Isolated empty ~/.claude/ — only built-in subagents are reachable.
    let fake_claude = tmp.path().join("claude-empty");
    std::fs::create_dir_all(&fake_claude).unwrap();
    let _guard = ScopedEnv::set("CLAUDE_CONFIG_HOME", fake_claude.to_str().unwrap());

    let err = Orchestrator::new(paths, OrchestratorConfig::default()).unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("code-reviewer"), "got: {msg}");
    assert!(msg.contains("ccteam doctor"), "fix hint missing: {msg}");
}

#[test]
fn orchestrator_skip_tool_check_bypasses_validator() {
    let tmp = TempDir::new().unwrap();
    let paths = fresh_paths(&tmp);
    write_template(
        &paths.phases_dir(),
        "03-implement.md",
        concat!(
            "---\n",
            "name: implement\n",
            "parallelism: solo\n",
            "tools_required:\n",
            "  subagents: [definitely-not-installed]\n",
            "---\n",
            "body\n",
        ),
    );

    Orchestrator::new(
        paths,
        OrchestratorConfig {
            skip_tool_check: true,
            ..OrchestratorConfig::default()
        },
    )
    .unwrap();
}

/// Tiny RAII helper for mutating a process-global env var inside a
/// single test without leaking state to siblings. Tests that use this
/// must serialize via Rust's default test serial pool — fine for our
/// tiny number of CLAUDE_CONFIG_HOME consumers.
struct ScopedEnv {
    key: &'static str,
    prior: Option<String>,
}

impl ScopedEnv {
    fn set(key: &'static str, value: &str) -> Self {
        let prior = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, prior }
    }
}

impl Drop for ScopedEnv {
    fn drop(&mut self) {
        match &self.prior {
            Some(v) => std::env::set_var(self.key, v),
            None => std::env::remove_var(self.key),
        }
    }
}

#[tokio::test]
async fn run_returns_when_shutdown_future_resolves() {
    let tmp = TempDir::new().unwrap();
    let paths = fresh_paths(&tmp);
    let orch = Orchestrator::new(
        paths,
        OrchestratorConfig {
            tick_interval: Duration::from_millis(50),
            ..OrchestratorConfig::default()
        },
    )
    .unwrap();

    let result = tokio::time::timeout(Duration::from_secs(2), async {
        orch.run(async {
            tokio::time::sleep(Duration::from_millis(150)).await;
        })
        .await
    })
    .await;

    let inner = result.expect("orchestrator did not honor the shutdown future");
    inner.expect("orchestrator returned an error during a clean shutdown");
}

/// `ensure_session` should bring up a tmux session for a fresh
/// project on demand. Uses a `sh` placeholder for `claude` so the
/// test doesn't depend on the real CLI; `load-context` writes the
/// `ready` marker SessionStart would normally produce.
#[test]
fn ensure_session_starts_a_missing_session() {
    if !tmux_available() {
        eprintln!("[skip] ensure_session_starts_a_missing_session: tmux not on PATH");
        return;
    }
    let tmp = TempDir::new().unwrap();
    let paths = fresh_paths(&tmp);
    let slug = format!("ensure-test-{}", std::process::id());
    bootstrap_project(&paths, &slug, "test request").unwrap();

    let project_dir = paths.project_dir(&slug);
    let ready = project_dir.join(".ccteam/ready");
    let argv = vec![
        "sh".into(),
        "-c".into(),
        format!(
            "touch {} && exec sh -i",
            ready.to_str().unwrap().replace(' ', r"\ "),
        ),
    ];

    let orch = Orchestrator::new(
        paths.clone(),
        OrchestratorConfig {
            claude_argv: argv,
            ready_timeout: Duration::from_secs(5),
            post_ready_warmup: Duration::from_millis(0),
            ..OrchestratorConfig::default()
        },
    )
    .unwrap();

    let mut state = ProjectState::load(&paths.project_state(&slug)).unwrap();
    let session = TmuxSession::for_slug(&slug);
    assert!(
        !session.exists(),
        "session must be absent before ensure_session",
    );

    orch.ensure_session(&slug, &mut state).unwrap();

    assert!(
        session.exists(),
        "ensure_session must spin up a fresh tmux session",
    );
    assert!(state.claude_pid.is_some(), "claude_pid must be recorded");
    let _ = session.kill();
}

#[test]
fn ensure_session_is_no_op_when_session_alive() {
    if !tmux_available() {
        eprintln!("[skip] ensure_session_is_no_op_when_session_alive: tmux not on PATH");
        return;
    }
    let tmp = TempDir::new().unwrap();
    let paths = fresh_paths(&tmp);
    let slug = format!("ensure-noop-{}", std::process::id());
    bootstrap_project(&paths, &slug, "test request").unwrap();

    let project_dir = paths.project_dir(&slug);
    let ready = project_dir.join(".ccteam/ready");
    std::fs::write(&ready, b"").unwrap();

    let session = TmuxSession::for_slug(&slug);
    session
        .start(&project_dir, &["sh", "-c", "sleep 60"])
        .unwrap();
    let pid_before = session.pane_pid().unwrap();

    let orch = Orchestrator::new(
        paths.clone(),
        OrchestratorConfig {
            ready_timeout: Duration::from_secs(5),
            post_ready_warmup: Duration::from_millis(0),
            ..OrchestratorConfig::default()
        },
    )
    .unwrap();
    let mut state = ProjectState::load(&paths.project_state(&slug)).unwrap();
    state.claude_pid = pid_before;
    state.save(&paths.project_state(&slug)).unwrap();

    orch.ensure_session(&slug, &mut state).unwrap();

    let pid_after = session.pane_pid().unwrap();
    assert_eq!(
        pid_before, pid_after,
        "ensure_session must not recycle a healthy session",
    );
    let _ = session.kill();
}

#[tokio::test]
async fn run_creates_progress_dir_when_absent() {
    let tmp = TempDir::new().unwrap();
    let paths = fresh_paths(&tmp);
    let progress = paths.root.join("progress");
    assert!(!progress.exists());

    let orch =
        Orchestrator::new(paths.clone(), OrchestratorConfig::default()).unwrap();

    tokio::time::timeout(Duration::from_secs(2), async {
        orch.run(async {
            // give the watcher setup a beat then shut down
            tokio::time::sleep(Duration::from_millis(80)).await;
        })
        .await
    })
    .await
    .unwrap()
    .unwrap();

    assert!(progress.is_dir(), "run() must create the progress dir");
}
