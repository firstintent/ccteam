//! v0.8.6 — REAL end-to-end smoke for the `session = role` keystone.
//!
//! The argv-level coverage (`claude_tui_resume_test`) proves the spawn
//! command *carries* `--agent <role>` / `--name` / `--resume`, but it
//! never runs a real `claude`, so it cannot prove the keystone actually
//! works: that `--agent <role>` loads the persona, that an interactive
//! turn round-trips through tmux send-keys, that the official hooks fire
//! on turn completion, and that `--resume` brings a dead session back.
//!
//! This file closes that gap with a real-binary smoke modeled on the
//! sibling real smokes (`tmux_backend_session_roundtrip`'s tmux gate and
//! `codex_app_server_test::f10_real_codex_stdio_new_smoke`'s `#[ignore]`
//! real-vendor pattern). It is `#[ignore]`d so the normal gate stays
//! hermetic — opt in with:
//!
//! ```text
//! cargo test -p ccteam-harness --test claude_agent_smoke_test \
//!     -- --ignored --nocapture
//! ```
//!
//! ## Guard conditions (both must hold or the test early-returns / skips)
//!
//! 1. `tmux` is on PATH (`ccteam_harness::tmux_ops::tmux_available()`).
//! 2. A real `claude` binary is resolvable — either on PATH or pinned via
//!    `CCTEAM_CLAUDE_BIN` (`CLAUDE_BIN_ENV`).
//!
//! When either guard is unmet the test prints why and returns Ok, so CI
//! machines without `claude` / `tmux` stay green.
//!
//! ## What it proves
//!
//! Phase 1 (start + persona + hook):
//!   - `ClaudeTuiAdapter::start_thread` spawns `claude --agent <role>`.
//!   - A submitted turn round-trips: the assistant transcript surfaces a
//!     recognizable token that ONLY the `.claude/agents/<role>.md` persona
//!     was told to emit ⇒ `--agent` loaded the role.
//!   - The official `Stop` hook fires on turn completion ⇒ the
//!     ccteam-managed hook log records the `stop` event.
//!
//! Phase 2 (resume):
//!   - We kill the pane process (keeping the tmux session via
//!     `remain-on-exit on`) so the next `start_thread` takes the dead-pane
//!     `--resume <name>` recreate branch.
//!   - `start_thread` brings the session back live and a follow-up turn
//!     produces another assistant message.
//!
//! ## Red-line compliance
//!
//! No `tmux capture-pane`: assistant output is read from the transcript
//! jsonl via the adapter's `events()` stream, and turn completion is
//! observed via the official `Stop` hook — never by scraping the pane.
//! Pane liveness is probed via `list_pane_pids` + `kill -0` only.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use futures::StreamExt;
use serial_test::serial;
use tempfile::TempDir;

use ccteam_harness::execution::claude_tui::ClaudeTuiAdapter;
use ccteam_harness::tmux_ops::{tmux_available, TmuxSession};
use ccteam_harness::{
    chat_session_name, AgentSpecBrief, HarnessAdapter, SpawnCtx, ThreadEvent, ThreadItemDetails,
    TurnInput, CLAUDE_BIN_ENV,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const ROLE: &str = "smoke-persona";
const SLUG: &str = "agent-smoke";

/// A token the persona is instructed to emit verbatim. Made unusual so a
/// substring match in the assistant transcript is unambiguous proof the
/// `--agent <role>` persona (and not some default behavior) shaped the
/// reply.
const PERSONA_TOKEN: &str = "CCTEAM-PERSONA-OK-7F3A";

/// Real-model latency budget for a single turn (cold model + first
/// transcript flush). Generous on purpose — the test is opt-in.
const TURN_TIMEOUT: Duration = Duration::from_secs(180);

// ---------------------------------------------------------------------------
// Guards
// ---------------------------------------------------------------------------

/// Whether the `claude` binary the adapter will spawn is resolvable:
/// `CCTEAM_CLAUDE_BIN` if set (the production override), else `claude` on
/// PATH. `false` when neither is usable, so the test can skip on hermetic
/// CI.
fn claude_available() -> bool {
    if let Ok(bin) = std::env::var(CLAUDE_BIN_ENV) {
        let p = Path::new(&bin);
        if p.is_file() {
            return true;
        }
    }
    // `claude --version` is the cheapest "is it on PATH and runnable"
    // probe that does not start a session.
    std::process::Command::new("claude")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Combined guard. Returns true (→ caller should early-return) when the
/// real-binary preconditions are not met, printing why.
fn skip_if_unavailable() -> bool {
    if !tmux_available() {
        eprintln!("claude_agent_smoke: skipping — tmux not on PATH");
        return true;
    }
    if !claude_available() {
        eprintln!(
            "claude_agent_smoke: skipping — no real `claude` (set CCTEAM_CLAUDE_BIN or put \
             claude on PATH)"
        );
        return true;
    }
    false
}

// ---------------------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------------------

/// Seed `<project>/.claude/agents/<role>.md` with a tiny persona that,
/// crucially, is told to emit [`PERSONA_TOKEN`] on every reply. If the
/// token shows up in the assistant transcript, `--agent <role>` loaded
/// this file.
fn seed_persona(project_dir: &Path) {
    let agents_dir = project_dir.join(".claude").join("agents");
    std::fs::create_dir_all(&agents_dir).expect("create .claude/agents");
    let body = format!(
        "---\nname: {ROLE}\ndescription: ccteam real-binary smoke persona.\n---\n\n\
         You are a terse smoke-test persona. On EVERY reply, no matter what \
         the user asks, you MUST include the exact token `{PERSONA_TOKEN}` \
         somewhere in your answer. Keep replies to a single short sentence.\n"
    );
    std::fs::write(agents_dir.join(format!("{ROLE}.md")), body).expect("write persona .md");
}

/// Write a hook wrapper script that appends each invocation's argv to a
/// log and exits 0. `ClaudeTuiAdapter::start_thread` reads `CCTEAM_HOOK_SH`
/// and installs the official Claude hooks (incl. `Stop`) as
/// `<hook_sh> chat-progress <action>`; on turn completion the `Stop` hook
/// fires `<hook_sh> chat-progress stop`, so the log records `stop`.
///
/// `Stop` is a fire-and-forget hook (its stdout is ignored), so a no-op
/// append + exit-0 wrapper faithfully exercises the firing path without a
/// running daemon.
fn write_hook_logger(tmp: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let log = tmp.join("hook.log");
    let script = tmp.join("hook.sh");
    let body = format!(
        "#!/bin/sh\necho \"$@\" >> {}\nexit 0\n",
        log.to_str().unwrap()
    );
    std::fs::write(&script, body).expect("write hook.sh");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
        .expect("chmod hook.sh");
    (script, log)
}

fn make_ctx(cwd: &Path) -> SpawnCtx {
    SpawnCtx {
        slug: SLUG.to_string(),
        sid: "s-agent-smoke".to_string(),
        cwd: cwd.to_path_buf(),
        project_dir: cwd.to_path_buf(),
        extra_args: vec![],
        model_id: None,
        effort: None,
        permission_mode: ccteam_harness::PermissionMode::Skip,
        secret: String::new(),
    }
}

/// v0.8.7 review-fix (verification gap) — write a `PermissionRequest` hook
/// wrapper that returns a FIXED decision (`allow` or `deny`) to stdout, the
/// exact `hookSpecificOutput` shape `ccteam_hooks::permission_request::decision`
/// emits and the real claude binary consumes. Unlike `write_hook_logger`
/// (empty stdout — only proves firing), this lets the smoke assert the
/// CONTRACT: deny ⇒ the tool is blocked (victim survives), allow ⇒ it runs
/// (victim deleted). Also appends `permission-request` to a log so the test
/// can confirm the hook actually fired before judging the file state.
fn write_decision_hook(tmp: &Path, allow: bool) -> (std::path::PathBuf, std::path::PathBuf) {
    let log = tmp.join("decision-hook.log");
    let script = tmp.join("decision-hook.sh");
    // The decision JSON the real claude PermissionRequest hook reads. `allow`
    // lets the tool run; `deny` blocks just this one tool call.
    let decision = if allow {
        r#"{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"}}}"#
    } else {
        r#"{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"deny","message":"denied by smoke"}}}"#
    };
    // Log the firing (with argv so we see `permission-request`), then print the
    // decision to stdout and exit 0.
    let body = format!(
        "#!/bin/sh\necho \"$@\" >> {log}\ncat <<'CCTEAM_EOF'\n{decision}\nCCTEAM_EOF\nexit 0\n",
        log = log.to_str().unwrap(),
    );
    std::fs::write(&script, body).expect("write decision-hook.sh");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
        .expect("chmod decision-hook.sh");
    (script, log)
}

/// Poll until `path` no longer exists, or `max` elapses. Returns true if the
/// file is gone (i.e. the tool ran).
fn wait_for_file_gone(path: &Path, max: Duration) -> bool {
    let start = std::time::Instant::now();
    loop {
        if !path.exists() {
            return true;
        }
        if start.elapsed() >= max {
            return false;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn kill_session_quiet(name: &str) {
    let _ = std::process::Command::new("tmux")
        .args(["kill-session", "-t", name])
        .output();
}

/// Drive `events()` until an assistant message containing `needle`
/// arrives, or `TURN_TIMEOUT` elapses. Returns true on match.
async fn await_assistant_token(
    adapter: &ClaudeTuiAdapter,
    handle: &ccteam_harness::ThreadHandle,
    needle: &str,
) -> bool {
    let mut stream = adapter.events(handle);
    let deadline = tokio::time::Instant::now() + TURN_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(2), stream.next()).await {
            Ok(Some(ev)) => {
                if let Some(text) = assistant_text(&ev) {
                    if text.contains(needle) {
                        return true;
                    }
                }
            }
            Ok(None) => break, // stream ended
            Err(_) => {}       // 2s tick — keep polling until deadline
        }
    }
    false
}

/// Extract assistant message text from item-carrying events.
fn assistant_text(ev: &ThreadEvent) -> Option<&str> {
    let item = match ev {
        ThreadEvent::ItemCompleted { item }
        | ThreadEvent::ItemUpdated { item }
        | ThreadEvent::ItemStarted { item } => item,
        _ => return None,
    };
    match &item.details {
        ThreadItemDetails::AgentMessage(s) => Some(s.as_str()),
        _ => None,
    }
}

/// Poll a file until it contains `needle` or `max` elapses.
fn wait_for_file_contains(path: &Path, needle: &str, max: Duration) -> bool {
    let start = std::time::Instant::now();
    loop {
        if let Ok(body) = std::fs::read_to_string(path) {
            if body.contains(needle) {
                return true;
            }
        }
        if start.elapsed() >= max {
            return false;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Kill the live pane process of `session` but KEEP the tmux session
/// (via `remain-on-exit on`), so the next `start_thread` hits the
/// dead-pane `--resume` recreate branch rather than the absent (fresh)
/// branch. Mirrors `claude_tui_resume_test::setup_dead_pane_session`.
fn kill_pane_keep_session(session_name: &str) {
    let _ = std::process::Command::new("tmux")
        .args(["set-option", "-t", session_name, "remain-on-exit", "on"])
        .status();
    let session = TmuxSession::from_name(session_name.to_string());
    let pids = session.list_pane_pids();
    let Some(pid) = pids.first().copied() else {
        return;
    };
    let _ = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status();
    // Wait up to 5s for the process to actually vanish.
    for _ in 0..50 {
        let alive = std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !alive {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

// ---------------------------------------------------------------------------
// The smoke
// ---------------------------------------------------------------------------

/// Real end-to-end keystone smoke. `#[ignore]`d — opt in via
/// `cargo test -p ccteam-harness --test claude_agent_smoke_test -- --ignored`.
///
/// `serial` because it shares the tmux server + process-global env
/// (`CCTEAM_HOME`, `CCTEAM_HOOK_SH`) with the other harness smokes.
#[tokio::test(flavor = "multi_thread")]
#[serial]
#[ignore = "real claude binary + login; session=role keystone, run with --ignored"]
async fn claude_agent_session_role_keystone_smoke() {
    if skip_if_unavailable() {
        return;
    }

    let project = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    seed_persona(project.path());
    let (hook_sh, hook_log) = write_hook_logger(home.path());

    // Route ccteam-owned paths into the tempdir layout; pin the hook
    // wrapper at our logger. (CCTEAM_HOME keeps progress.jsonl etc out of
    // the real ~/.ccteam; CCTEAM_HOOK_SH makes start_thread install hooks
    // that call our logger.) The transcript itself is read from the real
    // ~/.claude/projects/<encoded-cwd>/ where the real claude writes it —
    // the temp cwd encodes to a unique dir, so no cross-talk.
    let prior_home = std::env::var_os("CCTEAM_HOME");
    let prior_hook = std::env::var_os("CCTEAM_HOOK_SH");
    let prior_via = std::env::var_os("CCTEAM_HOOK_VIA_DAEMON");
    std::env::set_var("CCTEAM_HOME", home.path());
    std::env::set_var("CCTEAM_HOOK_SH", &hook_sh);
    std::env::remove_var("CCTEAM_HOOK_VIA_DAEMON"); // force the {hook_sh} form

    let session_name = chat_session_name(SLUG, ROLE);
    // Defensive: ensure no stale session from a prior aborted run.
    kill_session_quiet(&session_name);

    let adapter = ClaudeTuiAdapter::new();

    // ---- Phase 1: start + persona-loaded turn + Stop hook fired --------
    let handle = adapter
        .start_thread(
            &AgentSpecBrief {
                role: ROLE.to_string(),
            },
            &make_ctx(project.path()),
        )
        .await
        .expect("start_thread (claude --agent) must succeed");

    // Give the TUI a moment to finish booting before send-keys.
    tokio::time::sleep(Duration::from_secs(3)).await;

    adapter
        .submit_turn(
            &handle,
            TurnInput::UserText("Reply in one short sentence: what is 2+2?".to_string()),
        )
        .await
        .expect("submit_turn must succeed");

    let saw_persona = await_assistant_token(&adapter, &handle, PERSONA_TOKEN).await;

    // The official Stop hook should have fired on turn completion → our
    // logger recorded `chat-progress stop`. Allow a short extra window
    // since the hook fires right at turn end.
    let saw_stop_hook = wait_for_file_contains(&hook_log, "stop", Duration::from_secs(20));

    // ---- Phase 2: resume (dead-pane --resume path) + follow-up turn ----
    kill_pane_keep_session(&session_name);

    let resumed = adapter
        .start_thread(
            &AgentSpecBrief {
                role: ROLE.to_string(),
            },
            &make_ctx(project.path()),
        )
        .await
        .expect("start_thread resume (--resume) must bring the session back");

    let session = TmuxSession::from_name(session_name.clone());
    let came_back = session.exists();

    tokio::time::sleep(Duration::from_secs(3)).await;
    adapter
        .submit_turn(
            &resumed,
            TurnInput::UserText("Reply in one short sentence: name a color.".to_string()),
        )
        .await
        .expect("follow-up submit_turn after resume must succeed");
    let saw_followup = await_assistant_token(&adapter, &resumed, PERSONA_TOKEN).await;

    // ---- Cleanup (self-cleaning) ---------------------------------------
    kill_session_quiet(&session_name);
    if let Some(v) = prior_home {
        std::env::set_var("CCTEAM_HOME", v);
    } else {
        std::env::remove_var("CCTEAM_HOME");
    }
    if let Some(v) = prior_hook {
        std::env::set_var("CCTEAM_HOOK_SH", v);
    } else {
        std::env::remove_var("CCTEAM_HOOK_SH");
    }
    if let Some(v) = prior_via {
        std::env::set_var("CCTEAM_HOOK_VIA_DAEMON", v);
    }

    // ---- Assertions ----------------------------------------------------
    assert!(
        saw_persona,
        "assistant transcript must contain the persona token `{PERSONA_TOKEN}` \
         (proves `claude --agent {ROLE}` loaded .claude/agents/{ROLE}.md)"
    );
    assert!(
        saw_stop_hook,
        "the official Stop hook must fire on turn completion (hook log `{}` should \
         contain `stop`)",
        hook_log.display()
    );
    assert!(
        came_back,
        "after `--resume`, the tmux session `{session_name}` must be live again"
    );
    assert!(
        saw_followup,
        "a follow-up turn after `--resume` must produce another assistant message \
         carrying the persona token"
    );
}

// ===========================================================================
// v0.8.7 W2 — HITL `PermissionRequest` smoke (real binary, #[ignore]d).
//
// Proves the smoke-gate GROUND TRUTH against a live `claude`: a HITL chat
// session (spawned with `--permission-mode default`, NO skip flag, with the
// `PermissionRequest` hook installed) FIRES the `PermissionRequest` hook when
// the assistant attempts a NON-allowlist tool. We log the hook firing with a
// shell wrapper and assert it recorded `permission-request`.
//
// The hook wrapper here exits 0 with EMPTY stdout (it only needs to prove the
// firing, not drive the IM round-trip — the daemon-side approve/deny path is
// covered by the deterministic unit tests). We never assert the tool ran; the
// point is the hook reached our handler, which only happens on the ask-path
// that `--permission-mode default` (vs `--dangerously-skip-permissions`)
// keeps alive.
// ===========================================================================

/// A persona that, when asked, attempts a NON-allowlist tool (a `Bash rm` on
/// a path under the system temp dir). The exact token is unimportant — we
/// only care that the model issues a tool call that Claude classifies as
/// "ask" (non-allowlist), which is what fires the PermissionRequest hook.
const HITL_ROLE: &str = "smoke-hitl";
const HITL_SLUG: &str = "agent-hitl";

fn seed_hitl_persona(project_dir: &Path, victim: &Path) {
    let agents_dir = project_dir.join(".claude").join("agents");
    std::fs::create_dir_all(&agents_dir).expect("create .claude/agents");
    let body = format!(
        "---\nname: {HITL_ROLE}\ndescription: ccteam HITL permission-request smoke persona.\n---\n\n\
         When the user asks you to delete the file, immediately run a single Bash \
         tool call: `rm {}`. Do not ask for confirmation in text; just issue the \
         Bash tool call.\n",
        victim.display()
    );
    std::fs::write(agents_dir.join(format!("{HITL_ROLE}.md")), body).expect("write persona .md");
}

fn make_ctx_hitl(cwd: &Path) -> SpawnCtx {
    SpawnCtx {
        slug: HITL_SLUG.to_string(),
        sid: "s-agent-hitl".to_string(),
        cwd: cwd.to_path_buf(),
        project_dir: cwd.to_path_buf(),
        extra_args: vec![],
        model_id: None,
        effort: None,
        permission_mode: ccteam_harness::PermissionMode::Hitl,
        secret: String::new(),
    }
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
#[ignore = "real claude binary + login; HITL PermissionRequest fires on non-allowlist tool, run with --ignored"]
async fn claude_agent_hitl_permission_request_fires_smoke() {
    if skip_if_unavailable() {
        return;
    }

    let project = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    // The non-allowlist tool target: a real file the model is told to `rm`.
    let victim = project.path().join("delete-me.txt");
    std::fs::write(&victim, b"smoke").unwrap();
    seed_hitl_persona(project.path(), &victim);
    let (hook_sh, hook_log) = write_hook_logger(home.path());

    let prior_home = std::env::var_os("CCTEAM_HOME");
    let prior_hook = std::env::var_os("CCTEAM_HOOK_SH");
    let prior_via = std::env::var_os("CCTEAM_HOOK_VIA_DAEMON");
    std::env::set_var("CCTEAM_HOME", home.path());
    std::env::set_var("CCTEAM_HOOK_SH", &hook_sh);
    std::env::remove_var("CCTEAM_HOOK_VIA_DAEMON");

    let session_name = chat_session_name(HITL_SLUG, HITL_ROLE);
    kill_session_quiet(&session_name);

    let adapter = ClaudeTuiAdapter::new();
    let handle = adapter
        .start_thread(
            &AgentSpecBrief {
                role: HITL_ROLE.to_string(),
            },
            &make_ctx_hitl(project.path()),
        )
        .await
        .expect("start_thread (claude --permission-mode default) must succeed");

    tokio::time::sleep(Duration::from_secs(3)).await;

    adapter
        .submit_turn(
            &handle,
            TurnInput::UserText("Delete the file now.".to_string()),
        )
        .await
        .expect("submit_turn must succeed");

    // The PermissionRequest hook should fire on the (non-allowlist) Bash rm →
    // our logger records `permission-request`. Generous window: the model has
    // to plan + emit the tool call, then Claude evaluates the allowlist.
    let saw_permission_hook =
        wait_for_file_contains(&hook_log, "permission-request", Duration::from_secs(120));

    // Cleanup (self-cleaning).
    kill_session_quiet(&session_name);
    if let Some(v) = prior_home {
        std::env::set_var("CCTEAM_HOME", v);
    } else {
        std::env::remove_var("CCTEAM_HOME");
    }
    if let Some(v) = prior_hook {
        std::env::set_var("CCTEAM_HOOK_SH", v);
    } else {
        std::env::remove_var("CCTEAM_HOOK_SH");
    }
    if let Some(v) = prior_via {
        std::env::set_var("CCTEAM_HOOK_VIA_DAEMON", v);
    }

    assert!(
        saw_permission_hook,
        "the PermissionRequest hook must fire for a non-allowlist tool under \
         `--permission-mode default` (hook log `{}` should contain \
         `permission-request`). If this fails, the HITL ask-path is not alive — \
         verify the spawn dropped --dangerously-skip-permissions and the \
         settings.local.json carries the PermissionRequest hook.",
        hook_log.display()
    );
}

// ===========================================================================
// v0.8.7 review-fix (verification gap) — HITL allow/deny CONTRACT, real binary.
//
// The whole HITL safety story rests on ONE assumption: that the real `claude`
// binary HONORS the `PermissionRequest` decision JSON ccteam returns —
// `{behavior:"deny"}` actually blocks the tool, `{behavior:"allow"}` actually
// lets it run. The `..._fires_smoke` test above only proves the hook FIRES; it
// returns empty stdout and never checks the tool's effect. This test closes
// that gap by returning a fixed decision from the hook and asserting the
// victim file's fate: DENY ⇒ file survives, ALLOW ⇒ file deleted.
//
// `#[ignore]` + `#[serial]` (real claude + tmux + process-global env), like the
// other real smokes. Run with:
//   cargo test -p ccteam-harness --test claude_agent_smoke_test -- --ignored
// ===========================================================================

/// Run ONE HITL turn that asks the persona to `rm <victim>`, with the
/// `PermissionRequest` hook wired to return `allow`/`deny`. Returns
/// `(hook_fired, file_deleted)`. Self-cleaning (kills the tmux session,
/// restores env). Each call uses a distinct slug/victim so allow + deny don't
/// collide on the shared tmux server.
async fn run_hitl_decision_turn(allow: bool, tag: &str) -> (bool, bool) {
    let project = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let victim = project.path().join(format!("delete-me-{tag}.txt"));
    std::fs::write(&victim, b"smoke").unwrap();
    seed_hitl_persona(project.path(), &victim);
    let (hook_sh, hook_log) = write_decision_hook(home.path(), allow);

    let prior_home = std::env::var_os("CCTEAM_HOME");
    let prior_hook = std::env::var_os("CCTEAM_HOOK_SH");
    let prior_via = std::env::var_os("CCTEAM_HOOK_VIA_DAEMON");
    std::env::set_var("CCTEAM_HOME", home.path());
    std::env::set_var("CCTEAM_HOOK_SH", &hook_sh);
    std::env::remove_var("CCTEAM_HOOK_VIA_DAEMON");

    // Distinct slug per phase so the tmux pane name doesn't clash.
    let slug = format!("agent-hitl-{tag}");
    let session_name = chat_session_name(&slug, HITL_ROLE);
    kill_session_quiet(&session_name);

    let adapter = ClaudeTuiAdapter::new();
    let ctx = SpawnCtx {
        slug: slug.clone(),
        sid: format!("s-agent-hitl-{tag}"),
        cwd: project.path().to_path_buf(),
        project_dir: project.path().to_path_buf(),
        extra_args: vec![],
        model_id: None,
        effort: None,
        permission_mode: ccteam_harness::PermissionMode::Hitl,
        secret: String::new(),
    };
    let handle = adapter
        .start_thread(
            &AgentSpecBrief {
                role: HITL_ROLE.to_string(),
            },
            &ctx,
        )
        .await
        .expect("start_thread (hitl) must succeed");

    tokio::time::sleep(Duration::from_secs(3)).await;
    adapter
        .submit_turn(
            &handle,
            TurnInput::UserText("Delete the file now.".to_string()),
        )
        .await
        .expect("submit_turn must succeed");

    // Wait for the hook to fire (the model has to plan + emit the tool call).
    let hook_fired =
        wait_for_file_contains(&hook_log, "permission-request", Duration::from_secs(120));
    // Decide the file's fate. On allow we expect it gone; on deny it must
    // survive. Give the tool a moment to execute after the decision.
    let file_deleted = if allow {
        wait_for_file_gone(&victim, Duration::from_secs(30))
    } else {
        // Deny: poll a fixed window; the file must STILL exist at the end.
        std::thread::sleep(Duration::from_secs(8));
        !victim.exists()
    };

    kill_session_quiet(&session_name);
    if let Some(v) = prior_home {
        std::env::set_var("CCTEAM_HOME", v);
    } else {
        std::env::remove_var("CCTEAM_HOME");
    }
    if let Some(v) = prior_hook {
        std::env::set_var("CCTEAM_HOOK_SH", v);
    } else {
        std::env::remove_var("CCTEAM_HOOK_SH");
    }
    if let Some(v) = prior_via {
        std::env::set_var("CCTEAM_HOOK_VIA_DAEMON", v);
    }
    (hook_fired, file_deleted)
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
#[ignore = "real claude binary + login; HITL allow/deny contract, run with --ignored"]
async fn claude_agent_hitl_permission_decision_contract_smoke() {
    if skip_if_unavailable() {
        return;
    }

    // ---- DENY: the hook returns {behavior:"deny"} → the rm must NOT run ----
    let (deny_fired, deny_deleted) = run_hitl_decision_turn(false, "deny").await;
    assert!(
        deny_fired,
        "DENY phase: the PermissionRequest hook must fire (else the contract is untested)"
    );
    assert!(
        !deny_deleted,
        "DENY contract VIOLATED: claude deleted the victim file despite a \
         `{{behavior:\"deny\"}}` decision. HITL would NOT actually protect the user."
    );

    // ---- ALLOW: the hook returns {behavior:"allow"} → the rm MUST run ----
    let (allow_fired, allow_deleted) = run_hitl_decision_turn(true, "allow").await;
    assert!(
        allow_fired,
        "ALLOW phase: the PermissionRequest hook must fire (else the contract is untested)"
    );
    assert!(
        allow_deleted,
        "ALLOW contract VIOLATED: claude did NOT run the approved tool despite a \
         `{{behavior:\"allow\"}}` decision (victim file still present). An approve \
         click would not actually let the tool run."
    );
}
