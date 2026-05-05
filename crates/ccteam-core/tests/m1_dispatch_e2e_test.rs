//! M1.3 acceptance test (end-to-end meta-agent dispatch).
//!
//! Production flow we're modeling:
//!   1. `ccteam doctor --install-meta-agent <user>` lays down the
//!      meta-agent project + role prompt + ccteam-control skill.
//!   2. orchestrator brings up `ccteam-meta-<user>` tmux session.
//!   3. user / channel adapter writes an inbox file with a project
//!      request.
//!   4. orchestrator's inbox watcher injects the body into the meta
//!      session via send-keys (idle path, since the session is fresh).
//!   5. inside the session, claude reads the body, decides to
//!      `ccteam new --team=dev` (we mock this — see below).
//!   6. meta writes an outbox `event_kind: reply` confirming the
//!      dispatch.
//!
//! Steps 5/6 require a real claude process; for an automated test we
//! substitute a shell stand-in that, on receiving the body, runs
//! `ccteam new` and writes the outbox file. The shape of the proof is
//! the same: progress.jsonl gains an `inbox_consumed` event, a project
//! was created via `ccteam new`, and an outbox reply lands.

use std::sync::OnceLock;
use std::time::Duration;

use ccteam_core::tmux::{tmux_available, TmuxSession};
use ccteam_core::{
    bootstrap_meta_project, current_ccteam_bin, disable_tool_surface_bootstrap_for_tests,
    inbox_filename, install_skill_into, write_global_phase_templates, CcteamPaths, InboxFrontMatter,
    InboxMessage, InstallSkillOptions, Orchestrator, OrchestratorConfig, OutboxEventKind,
    OutboxFrontMatter, OutboxMessage, OutboxPriority, ProjectState, SessionMailbox,
};
use chrono::Utc;
use tempfile::TempDir;

static DISABLE_TOOL_SURFACE: OnceLock<()> = OnceLock::new();
fn isolation() {
    DISABLE_TOOL_SURFACE.get_or_init(disable_tool_surface_bootstrap_for_tests);
}

fn fresh_paths(tmp: &TempDir) -> CcteamPaths {
    CcteamPaths {
        root: tmp.path().join("ccteam-home"),
        projects_root: tmp.path().join("projects"),
    }
}

#[test]
fn meta_dispatch_e2e_inbox_to_outbox() {
    if !tmux_available() {
        eprintln!("[skip] tmux not on PATH");
        return;
    }
    // Note: in the test binary `current_ccteam_bin()` returns the test
    // binary itself (which doesn't speak `ccteam new`). Cross-crate
    // CARGO_BIN_EXE_ccteam isn't exported either, so we model the
    // dispatch by laying down a project directory + state.json
    // directly inside the shell stand-in. The real meta-agent binds
    // its `Bash("ccteam new ...")` call in production; that subshell
    // execution path is covered by the dedicated `commands::run_new`
    // unit tests.
    let _ = current_ccteam_bin().ok();
    isolation();

    let tmp = TempDir::new().unwrap();
    let paths = fresh_paths(&tmp);
    write_global_phase_templates(&paths.root, true).unwrap();

    // Step 1: install meta-agent + ccteam-control skill (M1.0 + M1.8).
    let user = format!("e2e-{}", std::process::id());
    let report = bootstrap_meta_project(&paths, &user).unwrap();
    let claude_root = tmp.path().join("claude-home");
    install_skill_into(&claude_root, InstallSkillOptions::default()).unwrap();
    assert!(claude_root.join("skills/ccteam-control/SKILL.md").is_file());

    // Step 2/3: prepare a stand-in for claude that, on first run,
    // (a) touches the ready marker so ensure_session unblocks,
    // (b) waits for the inbox-injected message body to arrive on
    //     stdin (we approximate this by polling for the orchestrator's
    //     `inbox_consumed` progress event),
    // (c) executes `ccteam new --team=dev "<brief>"` to dispatch,
    // (d) writes the meta outbox reply file.
    //
    // The orchestrator's send-keys will land "做一个 todo cli\n" in
    // the pane; our stand-in blocks until it reads that line, then
    // performs the dispatch. We use `read line` on stdin since
    // tmux send-keys delivers to the pane's stdin.
    let ready = report.project_dir.join(".ccteam/ready");
    let progress_path = paths.progress_jsonl(&report.slug);
    let outbox_dir = paths.project_ccteam_dir(&report.slug).join("outbox");
    std::fs::create_dir_all(&outbox_dir).unwrap();
    let projects_root_str = paths.projects_root.to_string_lossy().to_string();
    let outbox_str = outbox_dir.to_string_lossy().to_string();
    let dispatched_slug_dir = paths.projects_root.join("todo-cli");
    let dispatched_str = dispatched_slug_dir.to_string_lossy().to_string();

    // Shell stand-in for `claude` running ccteam-control via Bash:
    //   1. touch ready marker
    //   2. read first line from the pane (orchestrator's send-keys
    //      delivers the inbox body)
    //   3. simulate `ccteam new --team=dev "<brief>"` by laying down
    //      `<projects>/todo-cli/.ccteam/state.json`
    //   4. write meta outbox `event_kind: reply` confirming dispatch
    //   5. stay alive so the test can attach.
    let script = format!(
        r#"set -e
        touch '{ready}'
        read -r LINE || LINE='timeout'
        mkdir -p '{dispatched}/.ccteam'
        cat > '{dispatched}/.ccteam/state.json' <<JSON
{{
  "slug": "todo-cli",
  "team": "dev",
  "created_at": "2026-05-06T10:30:00Z",
  "tmux_session": "ccteam-todo-cli",
  "claude_session_id": null,
  "claude_pid": null,
  "phase_state": "idle",
  "current_phase": "",
  "parallelism": "solo",
  "phase_history": [],
  "fix_cycle_count": 0,
  "cost_used_usd": 0.0,
  "soft_warn_threshold_usd": 20.0,
  "hard_kill_threshold_usd": 200.0,
  "context_tokens_used": 0,
  "context_reset_threshold_tokens": 600000,
  "context_reset_count": 0,
  "last_progress_event_at": null,
  "last_event_type": null,
  "last_user_interaction_at": "2026-05-06T10:30:00Z",
  "user_attached": false,
  "user_pause_pending": false
}}
JSON
        cat > '{outbox}/reply-001.md' <<'OUTBOX'
---
schema_version: 1
created_at: 2026-05-06T10:30:45Z
priority: normal
event_kind: reply
---

dispatched: todo-cli
OUTBOX
        exec sh -c 'sleep 60'
        "#,
        ready = ready.to_string_lossy(),
        dispatched = dispatched_str,
        outbox = outbox_str,
    );
    let _ = projects_root_str;

    let argv = vec!["sh".into(), "-c".into(), script];

    let orch = Orchestrator::new(
        paths.clone(),
        OrchestratorConfig {
            claude_argv: argv,
            ready_timeout: Duration::from_secs(8),
            post_ready_warmup: Duration::from_millis(50),
            skip_tool_check: true,
            tick_interval: Duration::from_millis(100),
        },
    )
    .unwrap();

    // Step 4: drop an inbox file containing the project request.
    let mailbox = SessionMailbox::for_ccteam_dir(
        &paths.project_ccteam_dir(&report.slug),
    );
    mailbox.ensure_dirs().unwrap();
    let now = Utc::now();
    let inbox_path = mailbox.inbox.join(inbox_filename(now, 1));
    let msg = InboxMessage {
        front: InboxFrontMatter {
            schema_version: 1,
            source: "terminal".into(),
            source_chat_id: None,
            source_msg_id: None,
            source_user: user.clone(),
            created_at: now,
            ingested_at: now,
            content_type: "text".into(),
            attachments: Vec::new(),
        },
        body: "做一个 todo cli".into(),
    };
    msg.save(&inbox_path).unwrap();

    // Drive the orchestrator manually rather than via run() — we want
    // synchronous control to assert state.  ensure_session +
    // process_session_inbox is exactly the meta-agent dispatch path.
    let mut state = ProjectState::load(&paths.project_state(&report.slug)).unwrap();
    let session = TmuxSession::from_name(state.tmux_session.clone());

    orch.ensure_session(&report.slug, &mut state).unwrap();
    orch.process_session_inbox(&report.slug, &state).unwrap();

    // Inbox should be ack'd (deleted).
    assert!(!inbox_path.exists(), "inbox file should be consumed");

    // progress.jsonl should record `inbox_consumed`.
    let body = std::fs::read_to_string(&progress_path).unwrap();
    assert!(
        body.contains("\"event\":\"inbox_consumed\""),
        "missing inbox_consumed event; got:\n{body}",
    );

    // Wait up to 5s for the shell stand-in to dispatch + write outbox.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut outbox_files = Vec::new();
    while std::time::Instant::now() < deadline {
        outbox_files = mailbox.list_outbox().unwrap_or_default();
        if !outbox_files.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        !outbox_files.is_empty(),
        "meta-agent stand-in should have written an outbox reply",
    );
    let parsed = OutboxMessage::load(&outbox_files[0]).unwrap();
    assert_eq!(parsed.front.event_kind, OutboxEventKind::Reply);
    assert_eq!(parsed.front.priority, OutboxPriority::Normal);

    // Project dispatch: a `~/projects/做一个-todo-cli/` (or slugified
    // equivalent) should now exist beyond the meta-agent's own.
    let mut dispatched_slug = None;
    if paths.projects_root.exists() {
        for entry in std::fs::read_dir(&paths.projects_root).unwrap() {
            let e = entry.unwrap();
            let name = e.file_name().to_string_lossy().to_string();
            if name == report.slug {
                continue; // meta itself
            }
            // bootstrap_project writes state.json for any new project.
            if e.path().join(".ccteam/state.json").exists() {
                dispatched_slug = Some(name);
                break;
            }
        }
    }
    assert!(
        dispatched_slug.is_some(),
        "meta-agent stand-in should have dispatched a new project via `ccteam new`",
    );

    // Confirmed front matter contract.
    drop(parsed);
    let _ = OutboxFrontMatter {
        schema_version: 1,
        in_reply_to: None,
        in_reply_to_source_msg_id: None,
        target_channels: Vec::new(),
        created_at: Utc::now(),
        priority: OutboxPriority::Normal,
        event_kind: OutboxEventKind::Reply,
    };

    let _ = session.kill();
}
