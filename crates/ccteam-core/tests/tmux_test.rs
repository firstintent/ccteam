//! Integration tests for `ccteam_core::tmux`. Each test runs a real
//! tmux session under a unique name so parallel test threads can't
//! collide; cleanup is via a `Drop` guard. Tests skip gracefully if
//! `tmux` isn't on PATH.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use ccteam_core::tmux::{pid_is_alive, tmux_available, TmuxSession};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_session(test_name: &str) -> TmuxSession {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let slug = format!("test-{test_name}-{pid}-{n}");
    TmuxSession::for_slug(&slug)
}

/// Cleanup wrapper: kills the session on drop even if the test panics.
struct ScopedSession {
    inner: TmuxSession,
}

impl ScopedSession {
    fn new(inner: TmuxSession) -> Self {
        Self { inner }
    }

    fn session(&self) -> &TmuxSession {
        &self.inner
    }
}

impl Drop for ScopedSession {
    fn drop(&mut self) {
        let _ = self.inner.kill();
    }
}

fn skip_if_no_tmux(test_name: &str) -> bool {
    if !tmux_available() {
        eprintln!("[skip] {test_name}: tmux not on PATH");
        return true;
    }
    false
}

#[test]
fn start_creates_a_session_that_exists() {
    if skip_if_no_tmux("start_creates_a_session_that_exists") {
        return;
    }
    let session = ScopedSession::new(unique_session("start"));
    assert!(!session.session().exists());
    session
        .session()
        .start(Path::new("/tmp"), &["sh", "-c", "sleep 60"])
        .unwrap();
    assert!(session.session().exists());
}

#[test]
fn start_rejects_when_session_already_exists() {
    if skip_if_no_tmux("start_rejects_when_session_already_exists") {
        return;
    }
    let session = ScopedSession::new(unique_session("dup"));
    session
        .session()
        .start(Path::new("/tmp"), &["sh", "-c", "sleep 60"])
        .unwrap();
    let err = session
        .session()
        .start(Path::new("/tmp"), &["sh", "-c", "sleep 60"])
        .unwrap_err();
    assert!(format!("{err:#}").contains("already exists"));
}

#[test]
fn kill_removes_the_session() {
    if skip_if_no_tmux("kill_removes_the_session") {
        return;
    }
    let session = ScopedSession::new(unique_session("kill"));
    session
        .session()
        .start(Path::new("/tmp"), &["sh", "-c", "sleep 60"])
        .unwrap();
    session.session().kill().unwrap();
    assert!(!session.session().exists());
}

#[test]
fn kill_is_idempotent_on_missing_session() {
    if skip_if_no_tmux("kill_is_idempotent_on_missing_session") {
        return;
    }
    let session = unique_session("kill-missing");
    session.kill().unwrap(); // never started
    session.kill().unwrap(); // still gone
}

#[test]
fn pane_pid_returns_a_live_pid_after_start() {
    if skip_if_no_tmux("pane_pid_returns_a_live_pid_after_start") {
        return;
    }
    let session = ScopedSession::new(unique_session("pid"));
    session
        .session()
        .start(Path::new("/tmp"), &["sh", "-c", "sleep 60"])
        .unwrap();
    let pid = session
        .session()
        .pane_pid()
        .unwrap()
        .expect("session must have a pane PID");
    assert!(pid > 0);
    assert!(
        pid_is_alive(pid),
        "pane PID {pid} should be alive while session is up",
    );
}

#[test]
fn is_alive_double_checks_session_and_pid() {
    // The documented reattach contract (tech-design §6.1):
    // `tmux has-session` + `kill -0 pid`. is_alive must require BOTH.
    if skip_if_no_tmux("is_alive_double_checks_session_and_pid") {
        return;
    }
    let session = ScopedSession::new(unique_session("alive"));
    session
        .session()
        .start(Path::new("/tmp"), &["sh", "-c", "sleep 60"])
        .unwrap();
    let pid = session.session().pane_pid().unwrap().unwrap();

    assert!(session.session().is_alive(None));
    assert!(session.session().is_alive(Some(pid)));
    assert!(
        !session.session().is_alive(Some(1)),
        "wrong PID must invalidate reattach (PID 1 = init, mismatches the pane's pid)",
    );
}

#[test]
fn is_alive_after_kill_returns_false() {
    if skip_if_no_tmux("is_alive_after_kill_returns_false") {
        return;
    }
    let session = ScopedSession::new(unique_session("dead"));
    session
        .session()
        .start(Path::new("/tmp"), &["sh", "-c", "sleep 60"])
        .unwrap();
    let pid = session.session().pane_pid().unwrap().unwrap();
    session.session().kill().unwrap();
    assert!(!session.session().is_alive(None));
    assert!(!session.session().is_alive(Some(pid)));
}

#[test]
fn send_keys_delivers_to_the_session() {
    // We can't easily capture stdout from a sleeping shell, so we send
    // a command that creates a sentinel file under tempdir and poll
    // until it appears.
    if skip_if_no_tmux("send_keys_delivers_to_the_session") {
        return;
    }
    let dir = tempfile::TempDir::new().unwrap();
    let sentinel = dir.path().join("hello");
    let session = ScopedSession::new(unique_session("send"));
    // Run an interactive shell so it accepts keystrokes.
    session
        .session()
        .start(dir.path(), &["sh", "-i"])
        .unwrap();
    // Give the shell a moment to come up.
    std::thread::sleep(std::time::Duration::from_millis(200));

    session
        .session()
        .send_keys(&format!("touch {}", sentinel.display()))
        .unwrap();

    // Poll up to ~3s for the sentinel to appear.
    for _ in 0..30 {
        if sentinel.exists() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    panic!("sentinel {} never appeared — send_keys did not deliver", sentinel.display());
}

#[test]
fn pid_is_alive_rejects_non_positive() {
    assert!(!pid_is_alive(0));
    assert!(!pid_is_alive(-1));
}
