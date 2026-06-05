//! V0.6.6 F172 V2 — tests for `ClaudeTuiAdapter::start_thread` spawn
//! argv carrying Anthropic `--name` / `--resume <name>` for lossless
//! context recovery via the official CLI surface.
//!
//! ## Coverage matrix
//!
//! 1. Fresh path (session absent): spawn argv must contain
//!    `--name ccteam-chat-<slug>-<role>`.
//! 2. Recreate path (dead pane): spawn argv must contain
//!    `--resume ccteam-chat-<slug>-<role>`.
//! 3. Recreate path with `--resume` failure: fallback to `--name` +
//!    emit `chat_session_reset` event carrying
//!    `reason="resume_failed_fallback_to_fresh"`.
//! 4. Cwd-collision: two bots in the same cwd get distinct
//!    `--name` values (per-role) so Anthropic's
//!    `<cwd>:<name>` lookup never crosses streams.
//! 5. F164 alive-reattach regression guard: when pane is alive we do
//!    NOT spawn (no argv constructed, pid unchanged). Reuses the
//!    F164 fake-claude pattern.
//! 6. F118 brand-new spawn path remains compatible (turns.jsonl dir
//!    is still created on first spawn — regression guard for
//!    session_recovery prerequisites).
//!
//! ## Test infrastructure
//!
//! We use a fake `claude` shell script that records its full argv to a
//! per-test logfile before sleeping. The `CCTEAM_CLAUDE_BIN` env var
//! redirects production code to this script — no real claude binary
//! invocation. Tests are `serial_test::serial` because they share the
//! tmux server + the `CCTEAM_CLAUDE_BIN` / `CCTEAM_HOME` env vars.
//!
//! Red line compliance: this file never invokes `tmux capture-pane`.
//! Pane liveness is probed via `list_pane_pids` + `ps -o comm=` only.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use ccteam_harness::execution::claude_tui::{
    chat_session_id_name, chat_session_name, ClaudeTuiAdapter,
};
use ccteam_harness::tmux_ops::TmuxSession;
use ccteam_harness::{AgentSpecBrief, HarnessAdapter, SpawnCtx, CLAUDE_BIN_ENV};
use serial_test::serial;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

struct TestCcteamPaths {
    root: PathBuf,
}

impl TestCcteamPaths {
    fn progress_jsonl(&self, slug: &str) -> PathBuf {
        self.root.join("progress").join(format!("{slug}.jsonl"))
    }
}

fn test_ccteam_paths_from_env() -> Option<TestCcteamPaths> {
    std::env::var_os("CCTEAM_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".ccteam")))
        .map(|root| TestCcteamPaths { root })
}

fn kill_session_quiet(name: &str) {
    let _ = std::process::Command::new("tmux")
        .args(["kill-session", "-t", name])
        .output();
}

/// A fake "claude" that records its argv to `<tmp>/claude-argv.log`
/// (one line per invocation, space-separated) then sleeps 999s so the
/// pane stays alive (process comm = "fake-claude" contains "claude" →
/// `is_pane_running_claude` returns true).
fn fake_claude_logging_script(tmp: &tempfile::TempDir) -> PathBuf {
    let log = tmp.path().join("claude-argv.log");
    let p = tmp.path().join("fake-claude");
    let body = format!(
        "#!/bin/sh\necho \"$@\" >> {}\nsleep 999\n",
        log.to_str().unwrap()
    );
    std::fs::write(&p, body).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

/// A fake "claude" that records argv then **exits with non-zero**
/// (simulating `--resume <name>` jsonl-not-found failure). Used to
/// exercise the F172 V2 fallback path.
fn fake_claude_failing_resume_script(tmp: &tempfile::TempDir) -> PathBuf {
    let log = tmp.path().join("claude-argv.log");
    let p = tmp.path().join("fake-claude-failing");
    // If argv contains `--resume`, exit fast (simulate failure).
    // Otherwise sleep (the fallback `--name` spawn must stay alive).
    let body = format!(
        r#"#!/bin/sh
echo "$@" >> {log}
for arg in "$@"; do
    if [ "$arg" = "--resume" ]; then
        # Simulate "name not found" — exit fast so tmux pane dies.
        exit 1
    fi
done
sleep 999
"#,
        log = log.to_str().unwrap()
    );
    std::fs::write(&p, body).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

fn read_argv_log(tmp: &Path) -> Vec<String> {
    let log = tmp.join("claude-argv.log");
    if !log.exists() {
        return vec![];
    }
    std::fs::read_to_string(&log)
        .unwrap_or_default()
        .lines()
        .map(|s| s.to_string())
        .collect()
}

fn make_ctx(slug: &str, tmp: &tempfile::TempDir) -> SpawnCtx {
    SpawnCtx {
        slug: slug.to_string(),
        sid: "chat-f172".into(),
        cwd: tmp.path().to_path_buf(),
        project_dir: tmp.path().to_path_buf(),
        extra_args: vec![],
        model_id: None,
    }
}

/// Create a tmux session whose pane has a **dead** process — used to
/// force the F164/F172 dead-pane recreate path. Uses
/// `remain-on-exit on` so tmux keeps the session around after the pane
/// process exits (default tmux behavior is to destroy the session, which
/// would land us in the "absent" path instead). The sleep process is
/// killed and we wait until `ps -p <pid>` returns nothing.
fn setup_dead_pane_session(session_name: &str, cwd: &Path) {
    let status = std::process::Command::new("tmux")
        .args([
            "new-session",
            "-d",
            "-s",
            session_name,
            "-c",
            cwd.to_str().unwrap(),
            "sleep",
            "100",
        ])
        .status()
        .expect("tmux new-session for dead-pane setup");
    assert!(status.success(), "pre-create live session failed");

    let _ = std::process::Command::new("tmux")
        .args(["set-option", "-t", session_name, "remain-on-exit", "on"])
        .status();

    // Grab the pane pid, kill it, wait for the process to actually die.
    let session = TmuxSession::from_name(session_name.to_string());
    let pids = session.list_pane_pids();
    assert!(!pids.is_empty(), "live session must have pid before kill");
    let pid = pids[0];
    let _ = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status();
    // Wait up to 3s for the process to vanish from ps.
    for _ in 0..30 {
        let alive = std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !alive {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    // Session must still exist (remain-on-exit kept it) so start_thread
    // hits the dead-pane recreate branch, not the absent branch.
    assert!(
        session.exists(),
        "session must still exist after pane death (dead-pane recreate path)"
    );
}

/// Wait up to `max_ms` for the argv log to contain at least `n` lines.
fn wait_for_argv_lines(tmp: &Path, n: usize, max_ms: u64) -> Vec<String> {
    let start = std::time::Instant::now();
    loop {
        let lines = read_argv_log(tmp);
        if lines.len() >= n {
            return lines;
        }
        if start.elapsed().as_millis() as u64 >= max_ms {
            return lines;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

// ---------------------------------------------------------------------------
// Test 1: chat_session_id_name helper matches expected format
// ---------------------------------------------------------------------------

#[test]
fn chat_session_id_name_uses_canonical_format() {
    assert_eq!(
        chat_session_id_name("dev-foo", "alice"),
        "ccteam-chat-dev-foo-alice"
    );
    // Must agree with tmux session name (same namespace today).
    assert_eq!(
        chat_session_id_name("dev-foo", "alice"),
        chat_session_name("dev-foo", "alice")
    );
}

// ---------------------------------------------------------------------------
// Test 2: Fresh path spawn argv contains `--name <name>`
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn fresh_spawn_argv_contains_name_flag() {
    std::env::set_var("CCTEAM_MUX_BACKEND", "tmux");
    if !ccteam_harness::tmux_ops::tmux_available() {
        eprintln!("skip: tmux not available");
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let bin = fake_claude_logging_script(&tmp);

    let slug = format!("f172-fresh-{}", std::process::id());
    let role = "alpha";
    let session_name = chat_session_name(&slug, role);
    let expected_id = chat_session_id_name(&slug, role);
    kill_session_quiet(&session_name);

    std::env::set_var(CLAUDE_BIN_ENV, bin.to_str().unwrap());
    let brief = AgentSpecBrief {
        role: role.to_string(),
    };
    let ctx = make_ctx(&slug, &tmp);

    let handle = ClaudeTuiAdapter::new()
        .start_thread(&brief, &ctx)
        .await
        .expect("start_thread fresh path must succeed");
    assert_eq!(handle.identity, session_name);

    let lines = wait_for_argv_lines(tmp.path(), 1, 2000);
    assert_eq!(
        lines.len(),
        1,
        "fresh path spawns exactly one claude invocation"
    );
    let argv = &lines[0];
    assert!(
        argv.contains("--name"),
        "fresh argv must contain --name flag; got: {argv}"
    );
    assert!(
        argv.contains(&format!("--agent {role}")),
        "fresh argv must carry --agent <role> (session-is-the-role keystone); got: {argv}"
    );
    assert!(
        argv.contains(&expected_id),
        "fresh argv must contain the deterministic session id `{expected_id}`; got: {argv}"
    );
    assert!(
        !argv.contains("--resume"),
        "fresh argv must NOT contain --resume; got: {argv}"
    );

    kill_session_quiet(&session_name);
    std::env::remove_var(CLAUDE_BIN_ENV);
}

// ---------------------------------------------------------------------------
// Test 3: Recreate path spawn argv contains `--resume <name>`
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn recreate_dead_pane_spawn_argv_contains_resume_flag() {
    std::env::set_var("CCTEAM_MUX_BACKEND", "tmux");
    if !ccteam_harness::tmux_ops::tmux_available() {
        eprintln!("skip: tmux not available");
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let bin = fake_claude_logging_script(&tmp);

    let slug = format!("f172-recreate-{}", std::process::id());
    let role = "beta";
    let session_name = chat_session_name(&slug, role);
    let expected_id = chat_session_id_name(&slug, role);
    kill_session_quiet(&session_name);

    // Pre-create a session with `remain-on-exit on` + a killed pane so
    // start_thread sees "session exists + pane dead" → recreate path.
    setup_dead_pane_session(&session_name, tmp.path());

    std::env::set_var(CLAUDE_BIN_ENV, bin.to_str().unwrap());
    let brief = AgentSpecBrief {
        role: role.to_string(),
    };
    let ctx = make_ctx(&slug, &tmp);

    let handle = ClaudeTuiAdapter::new()
        .start_thread(&brief, &ctx)
        .await
        .expect("start_thread recreate path must succeed");
    assert_eq!(handle.identity, session_name);

    let lines = wait_for_argv_lines(tmp.path(), 1, 2000);
    assert!(
        !lines.is_empty(),
        "recreate path must spawn at least one claude invocation"
    );
    let argv = &lines[0];
    assert!(
        argv.contains("--resume"),
        "recreate argv must contain --resume flag; got: {argv}"
    );
    assert!(
        argv.contains(&format!("--agent {role}")),
        "recreate argv must carry --agent <role> (resume must re-bind persona); got: {argv}"
    );
    assert!(
        argv.contains(&expected_id),
        "recreate argv must contain the deterministic session id `{expected_id}`; got: {argv}"
    );
    assert!(
        !argv.contains("--name"),
        "recreate argv (first attempt) must NOT contain --name (it's --resume); got: {argv}"
    );

    kill_session_quiet(&session_name);
    std::env::remove_var(CLAUDE_BIN_ENV);
}

// ---------------------------------------------------------------------------
// Test 4: --resume failure → fallback to --name + emit chat_session_reset
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn resume_failure_falls_back_to_fresh_name() {
    std::env::set_var("CCTEAM_MUX_BACKEND", "tmux");
    if !ccteam_harness::tmux_ops::tmux_available() {
        eprintln!("skip: tmux not available");
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let bin = fake_claude_failing_resume_script(&tmp);

    let slug = format!("f172-fallback-{}", std::process::id());
    let role = "gamma";
    let session_name = chat_session_name(&slug, role);
    let expected_id = chat_session_id_name(&slug, role);
    kill_session_quiet(&session_name);

    // Pre-create dead-pane session to force recreate path.
    setup_dead_pane_session(&session_name, tmp.path());

    // Redirect CCTEAM_HOME so progress.jsonl lands in tmp.
    let ccteam_home = tmp.path().join("ccteam-home");
    std::fs::create_dir_all(&ccteam_home).unwrap();
    std::env::set_var("CCTEAM_HOME", ccteam_home.to_str().unwrap());
    std::env::set_var(CLAUDE_BIN_ENV, bin.to_str().unwrap());

    let brief = AgentSpecBrief {
        role: role.to_string(),
    };
    let ctx = make_ctx(&slug, &tmp);

    let handle = ClaudeTuiAdapter::new()
        .start_thread(&brief, &ctx)
        .await
        .expect("start_thread fallback path must succeed");
    assert_eq!(handle.identity, session_name);

    // Wait for both invocations: the failing --resume, then the fresh --name.
    let lines = wait_for_argv_lines(tmp.path(), 2, 3000);
    assert!(
        lines.len() >= 2,
        "expected 2 invocations (--resume then --name); got: {lines:?}"
    );
    let first = &lines[0];
    let second = &lines[1];
    assert!(
        first.contains("--resume") && first.contains(&expected_id),
        "first invocation must be --resume <name>; got: {first}"
    );
    assert!(
        first.contains(&format!("--agent {role}")),
        "first invocation (--resume) must carry --agent <role>; got: {first}"
    );
    assert!(
        second.contains("--name") && second.contains(&expected_id),
        "second invocation must be --name <name> (fallback); got: {second}"
    );
    assert!(
        second.contains(&format!("--agent {role}")),
        "fallback (--name) must still carry --agent <role>; got: {second}"
    );
    assert!(
        !second.contains("--resume"),
        "fallback must NOT include --resume; got: {second}"
    );

    // Verify `chat_session_reset` event was appended with the right reason.
    let paths = test_ccteam_paths_from_env().expect("CcteamPaths::from_env");
    let progress_path = paths.progress_jsonl(&slug);
    assert!(
        progress_path.exists(),
        "progress.jsonl must be written; path: {}",
        progress_path.display()
    );
    let body = std::fs::read_to_string(&progress_path).unwrap();
    let reset_line = body
        .lines()
        .find(|line| line.contains("chat_session_reset"))
        .expect("expected chat_session_reset event line");
    let ev: serde_json::Value = serde_json::from_str(reset_line).expect("valid JSON");
    assert_eq!(ev["event"], "chat_session_reset");
    assert_eq!(ev["role"], role);
    assert_eq!(ev["reason"], "resume_failed_fallback_to_fresh");

    kill_session_quiet(&session_name);
    std::env::remove_var(CLAUDE_BIN_ENV);
    std::env::remove_var("CCTEAM_HOME");
}

// ---------------------------------------------------------------------------
// Test 5: F164 alive reattach regression guard — no spawn when pane alive
// ---------------------------------------------------------------------------

/// When a session exists AND its pane is running a claude-like process,
/// start_thread must reattach without spawning. This guards the F164
/// alive path against accidental F172 V2 argv churn.
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn alive_reattach_does_not_spawn_new_claude() {
    std::env::set_var("CCTEAM_MUX_BACKEND", "tmux");
    if !ccteam_harness::tmux_ops::tmux_available() {
        eprintln!("skip: tmux not available");
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    // Non-exec fake-claude (so the wrapper shell stays alive with comm
    // = "fake-claude" containing "claude").
    let p = tmp.path().join("fake-claude");
    std::fs::write(&p, "#!/bin/sh\nsleep 999\n").unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    let bin = p;

    let slug = format!("f172-alive-{}", std::process::id());
    let role = "delta";
    let session_name = chat_session_name(&slug, role);
    kill_session_quiet(&session_name);

    // Pre-create the session running fake-claude — pane will be alive.
    let status = std::process::Command::new("tmux")
        .args([
            "new-session",
            "-d",
            "-s",
            &session_name,
            "-c",
            tmp.path().to_str().unwrap(),
            bin.to_str().unwrap(),
        ])
        .status()
        .expect("tmux new-session");
    assert!(status.success());

    let session = TmuxSession::from_name(session_name.clone());
    let pre_pids = session.list_pane_pids();
    assert!(!pre_pids.is_empty(), "pre-created session must have pid");
    let pre_pid = pre_pids[0];

    std::env::set_var(CLAUDE_BIN_ENV, bin.to_str().unwrap());
    let brief = AgentSpecBrief {
        role: role.to_string(),
    };
    let ctx = make_ctx(&slug, &tmp);

    let _ = ClaudeTuiAdapter::new()
        .start_thread(&brief, &ctx)
        .await
        .expect("start_thread alive reattach must succeed");

    // Pane pid must be unchanged (no new spawn).
    let post_pids = session.list_pane_pids();
    assert_eq!(
        post_pids.first().copied(),
        Some(pre_pid),
        "alive reattach must not spawn a new claude process"
    );

    kill_session_quiet(&session_name);
    std::env::remove_var(CLAUDE_BIN_ENV);
}

// ---------------------------------------------------------------------------
// Test 6: cwd-collision — two roles in same cwd get distinct --name args
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn cwd_collision_two_roles_distinct_names() {
    std::env::set_var("CCTEAM_MUX_BACKEND", "tmux");
    if !ccteam_harness::tmux_ops::tmux_available() {
        eprintln!("skip: tmux not available");
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let bin = fake_claude_logging_script(&tmp);
    std::env::set_var(CLAUDE_BIN_ENV, bin.to_str().unwrap());

    let slug = format!("f172-coll-{}", std::process::id());
    let role_a = "roleA";
    let role_b = "roleB";
    let session_a = chat_session_name(&slug, role_a);
    let session_b = chat_session_name(&slug, role_b);
    let id_a = chat_session_id_name(&slug, role_a);
    let id_b = chat_session_id_name(&slug, role_b);
    kill_session_quiet(&session_a);
    kill_session_quiet(&session_b);

    let ctx = make_ctx(&slug, &tmp);

    // Same cwd, two roles.
    let h_a = ClaudeTuiAdapter::new()
        .start_thread(
            &AgentSpecBrief {
                role: role_a.to_string(),
            },
            &ctx,
        )
        .await
        .expect("role A spawn");
    let h_b = ClaudeTuiAdapter::new()
        .start_thread(
            &AgentSpecBrief {
                role: role_b.to_string(),
            },
            &ctx,
        )
        .await
        .expect("role B spawn");

    assert_ne!(h_a.identity, h_b.identity);

    let lines = wait_for_argv_lines(tmp.path(), 2, 2000);
    assert_eq!(lines.len(), 2, "two spawn calls expected; got: {lines:?}");
    // Each line carries its own --name <id> — independent namespaces.
    let combined = lines.join("\n");
    assert!(
        combined.contains(&id_a),
        "argv log must contain role-A id `{id_a}`; got:\n{combined}"
    );
    assert!(
        combined.contains(&id_b),
        "argv log must contain role-B id `{id_b}`; got:\n{combined}"
    );
    // Each spawn carries its own --agent <role> binding (no crossed personas).
    assert!(
        combined.contains(&format!("--agent {role_a}")),
        "argv log must carry --agent {role_a}; got:\n{combined}"
    );
    assert!(
        combined.contains(&format!("--agent {role_b}")),
        "argv log must carry --agent {role_b}; got:\n{combined}"
    );
    assert_ne!(id_a, id_b, "the two role ids must differ");

    kill_session_quiet(&session_a);
    kill_session_quiet(&session_b);
    std::env::remove_var(CLAUDE_BIN_ENV);
}

// ---------------------------------------------------------------------------
// Test 7: F118 brand-new spawn path regression — turns.jsonl dir created
// ---------------------------------------------------------------------------

/// F172 V2 must not break F118 prerequisites. On a brand-new spawn the
/// `<project>/.ccteam/chat/<role>/` directory must still be created so
/// turns_mirror has somewhere to write the per-bot turns.jsonl —
/// session_recovery::build_recovery_prompt reads from there.
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn fresh_spawn_creates_turns_mirror_dir_for_f118() {
    std::env::set_var("CCTEAM_MUX_BACKEND", "tmux");
    if !ccteam_harness::tmux_ops::tmux_available() {
        eprintln!("skip: tmux not available");
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let bin = fake_claude_logging_script(&tmp);

    let slug = format!("f172-f118-{}", std::process::id());
    let role = "epsilon";
    let session_name = chat_session_name(&slug, role);
    kill_session_quiet(&session_name);

    std::env::set_var(CLAUDE_BIN_ENV, bin.to_str().unwrap());
    let brief = AgentSpecBrief {
        role: role.to_string(),
    };
    let ctx = make_ctx(&slug, &tmp);

    ClaudeTuiAdapter::new()
        .start_thread(&brief, &ctx)
        .await
        .expect("brand-new spawn must succeed");

    // F118 dir contract.
    let chat_dir = tmp.path().join(".ccteam/chat").join(role);
    assert!(
        chat_dir.exists(),
        "<project>/.ccteam/chat/<role>/ must be created for F118 turns mirror"
    );
    // Heartbeat — sanity check the broader spawn path didn't regress.
    assert!(chat_dir.join("heartbeat").exists());

    kill_session_quiet(&session_name);
    std::env::remove_var(CLAUDE_BIN_ENV);
}

// ---------------------------------------------------------------------------
// Test 8: Daemon-restart cycle simulation — bot survives via --resume
// ---------------------------------------------------------------------------

/// Simulates daemon-restart recovery semantics: a session was previously
/// running, the pane died (daemon was killed → claude was orphaned →
/// eventually exited), and on next start_thread call we expect the
/// `--resume <name>` route to be taken. The fake-claude binary that
/// records argv lets us verify the argv carries `--resume`.
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn daemon_restart_uses_resume_route() {
    std::env::set_var("CCTEAM_MUX_BACKEND", "tmux");
    if !ccteam_harness::tmux_ops::tmux_available() {
        eprintln!("skip: tmux not available");
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let bin = fake_claude_logging_script(&tmp);

    let slug = format!("f172-restart-{}", std::process::id());
    let role = "zeta";
    let session_name = chat_session_name(&slug, role);
    let expected_id = chat_session_id_name(&slug, role);
    kill_session_quiet(&session_name);

    // Cycle 1: first spawn = fresh --name.
    std::env::set_var(CLAUDE_BIN_ENV, bin.to_str().unwrap());
    let brief = AgentSpecBrief {
        role: role.to_string(),
    };
    let ctx = make_ctx(&slug, &tmp);
    ClaudeTuiAdapter::new()
        .start_thread(&brief, &ctx)
        .await
        .expect("first start_thread (fresh)");

    let lines_after_first = wait_for_argv_lines(tmp.path(), 1, 2000);
    assert_eq!(lines_after_first.len(), 1);
    assert!(
        lines_after_first[0].contains("--name") && lines_after_first[0].contains(&expected_id),
        "cycle 1 spawn must use --name; got: {}",
        lines_after_first[0]
    );
    assert!(
        lines_after_first[0].contains(&format!("--agent {role}")),
        "cycle 1 spawn must carry --agent <role>; got: {}",
        lines_after_first[0]
    );

    // Simulate daemon kill: kill the tmux session entirely (orphan
    // cleanup), then re-setup a dead-pane session so start_thread sees
    // "session exists + pane dead" → recreate via --resume path.
    kill_session_quiet(&session_name);
    setup_dead_pane_session(&session_name, tmp.path());

    // Cycle 2: second spawn = --resume.
    ClaudeTuiAdapter::new()
        .start_thread(&brief, &ctx)
        .await
        .expect("second start_thread (recreate via --resume)");
    let lines_after_second = wait_for_argv_lines(tmp.path(), 2, 2000);
    assert!(
        lines_after_second.len() >= 2,
        "cycle 2 spawn must be recorded; got: {lines_after_second:?}"
    );
    let second_argv = &lines_after_second[1];
    assert!(
        second_argv.contains("--resume") && second_argv.contains(&expected_id),
        "cycle 2 spawn must use --resume <name>; got: {second_argv}"
    );
    assert!(
        second_argv.contains(&format!("--agent {role}")),
        "cycle 2 spawn (--resume) must carry --agent <role>; got: {second_argv}"
    );

    kill_session_quiet(&session_name);
    std::env::remove_var(CLAUDE_BIN_ENV);
}
