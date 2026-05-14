//! Integration tests for the M0.3 hook handlers. Each test plumbs a
//! tempdir-rooted `CcteamPaths`, a fake project (state.json), and the
//! hook stdin payload that Claude Code would normally send, then
//! asserts the resulting filesystem side effect.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde_json::{json, Value};
use tempfile::TempDir;

use ccteam_core::{CcteamPaths, Parallelism, PhaseState, ProjectState, TeamKind};
use ccteam_hooks::{cost_accumulate, load_context, parse_phase_end, progress_append};

struct Fixture {
    _tmp: TempDir,
    paths: CcteamPaths,
    project_dir: PathBuf,
    transcript_path: PathBuf,
    slug: String,
}

impl Fixture {
    fn new(slug: &str) -> Self {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("ccteam-home");
        let projects_root = tmp.path().join("projects-home");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&projects_root).unwrap();

        let paths = CcteamPaths {
            root,
            projects_root,
        };
        let project_dir = paths.project_dir(slug);
        std::fs::create_dir_all(&project_dir).unwrap();
        let state_path = paths.project_state(slug);
        std::fs::create_dir_all(state_path.parent().unwrap()).unwrap();

        let now = Utc::now();
        let state = ProjectState {
            slug: slug.into(),
            team: "dev".into(),
            team_kind: TeamKind::Workflow,
            created_at: now,
            tmux_session: format!("ccteam-{slug}"),
            claude_session_id: None,
            claude_pid: None,
            phase_state: PhaseState::Idle,
            current_phase: "implement".into(),
            parallelism: Parallelism::Solo,
            phase_history: Vec::new(),
            auto_loop_cycle_count: 0,
            cost_used_usd: 0.0,
            soft_warn_threshold_usd: 20.0,
            hard_kill_threshold_usd: 200.0,
            context_tokens_used: 0,
            context_reset_threshold_tokens: 600_000,
            context_reset_count: 0,
            last_progress_event_at: None,
            last_event_type: None,
            last_user_interaction_at: now,
            user_attached: false,
            user_pause_pending: false,
            sessions: BTreeMap::new(),
            next_sid_seq: BTreeMap::new(),
        };
        state.save(&state_path).unwrap();

        let transcript_path = tmp.path().join("transcript.jsonl");

        Self {
            _tmp: tmp,
            paths,
            project_dir,
            transcript_path,
            slug: slug.into(),
        }
    }

    fn write_transcript(&self, lines: &[Value]) {
        let body: String = lines
            .iter()
            .map(|v| serde_json::to_string(v).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&self.transcript_path, body + "\n").unwrap();
    }

    fn read_progress_lines(&self) -> Vec<Value> {
        let p = self.paths.progress_jsonl(&self.slug);
        if !p.exists() {
            return Vec::new();
        }
        std::fs::read_to_string(&p)
            .unwrap()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn read_state(&self) -> ProjectState {
        ProjectState::load(&self.paths.project_state(&self.slug)).unwrap()
    }

    fn read_session_progress_lines(&self, sid: &str) -> Vec<Value> {
        let p = self.paths.progress_jsonl_for_session(&self.slug, sid);
        if !p.exists() {
            return Vec::new();
        }
        std::fs::read_to_string(&p)
            .unwrap()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn mark_flex(&self) {
        let mut state = self.read_state();
        state.team = "flex".into();
        state.team_kind = TeamKind::Flex;
        state.save(&self.paths.project_state(&self.slug)).unwrap();
    }
}

fn assistant_message(text: &str, usage: Option<Value>) -> Value {
    let mut msg = json!({
        "role": "assistant",
        "content": [{"type": "text", "text": text}],
    });
    if let Some(u) = usage {
        msg["usage"] = u;
    }
    json!({"type": "message", "message": msg})
}

#[test]
fn progress_append_writes_pretooluse_with_path_and_tool() {
    let fx = Fixture::new("bookmark-mgr-a3f9");
    let stdin = json!({
        "session_id": "abc",
        "transcript_path": fx.transcript_path,
        "cwd": fx.project_dir,
        "hook_event_name": "PreToolUse",
        "tool_name": "Edit",
        "tool_input": {"file_path": "src/db.ts"}
    });

    progress_append(&fx.paths, "PreToolUse", &stdin).unwrap();

    let events = fx.read_progress_lines();
    assert_eq!(events.len(), 1);
    let e = &events[0];
    assert_eq!(e["event"], "PreToolUse");
    assert_eq!(e["tool"], "Edit");
    assert_eq!(e["path"], "src/db.ts");
    assert!(
        e["ts"].as_str().unwrap().contains("T"),
        "ts must be ISO 8601"
    );
}

#[test]
fn progress_append_captures_bash_command_and_exit_code() {
    let fx = Fixture::new("bookmark-mgr-a3f9");
    let stdin = json!({
        "cwd": fx.project_dir,
        "tool_name": "Bash",
        "tool_input": {"command": "pnpm test"},
        "tool_response": {"exit_code": 0}
    });

    progress_append(&fx.paths, "PostToolUse", &stdin).unwrap();
    let e = &fx.read_progress_lines()[0];
    assert_eq!(e["event"], "PostToolUse");
    assert_eq!(e["tool"], "Bash");
    assert_eq!(e["cmd"], "pnpm test");
    assert_eq!(e["exit_code"], 0);
}

#[test]
fn progress_append_appends_across_multiple_invocations() {
    let fx = Fixture::new("bookmark-mgr-a3f9");
    let stdin = json!({"cwd": fx.project_dir});
    progress_append(&fx.paths, "session_start", &stdin).unwrap();
    progress_append(&fx.paths, "Stop", &stdin).unwrap();
    progress_append(&fx.paths, "SessionEnd", &stdin).unwrap();

    let events = fx.read_progress_lines();
    assert_eq!(events.len(), 3);
    assert_eq!(events[0]["event"], "session_start");
    assert_eq!(events[1]["event"], "Stop");
    assert_eq!(events[2]["event"], "SessionEnd");
}

#[test]
fn progress_append_routes_flex_session_to_nested_jsonl() {
    let fx = Fixture::new("flex-demo");
    fx.mark_flex();
    let session_dir = fx.paths.project_session_dir(&fx.slug, "claude-1");
    std::fs::create_dir_all(&session_dir).unwrap();
    let stdin = json!({"cwd": session_dir});

    progress_append(&fx.paths, "Stop", &stdin).unwrap();

    assert!(fx.read_progress_lines().is_empty());
    let events = fx.read_session_progress_lines("claude-1");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["event"], "Stop");
    assert_eq!(events[0]["sid"], "claude-1");
}

#[test]
fn parse_phase_end_emits_phase_done_when_sigil_present() {
    let fx = Fixture::new("bookmark-mgr-a3f9");
    fx.write_transcript(&[assistant_message(
        "All implementation tasks done.\n\nPHASE_DONE: implement",
        None,
    )]);
    let stdin = json!({
        "cwd": fx.project_dir,
        "transcript_path": fx.transcript_path,
    });

    parse_phase_end(&fx.paths, &stdin).unwrap();
    let events = fx.read_progress_lines();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["event"], "phase_done");
    assert_eq!(events[0]["phase"], "implement");
}

#[test]
fn parse_phase_end_emits_escalate_when_sigil_present() {
    let fx = Fixture::new("bookmark-mgr-a3f9");
    fx.write_transcript(&[assistant_message(
        "Tried three rounds, still red.\nESCALATE: fix-cycle 已 3 轮未通过",
        None,
    )]);
    let stdin = json!({
        "cwd": fx.project_dir,
        "transcript_path": fx.transcript_path,
    });

    parse_phase_end(&fx.paths, &stdin).unwrap();
    let events = fx.read_progress_lines();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["event"], "escalate");
    assert_eq!(events[0]["reason"], "fix-cycle 已 3 轮未通过");
    // M0.5.4: bare text → kind="need_user_input", target_phase=null
    assert_eq!(events[0]["kind"], "need_user_input");
    assert!(events[0]["target_phase"].is_null());
}

#[test]
fn parse_phase_end_emits_revert_kind_with_target_phase() {
    let fx = Fixture::new("bookmark-mgr-a3f9");
    fx.write_transcript(&[assistant_message(
        "Plan-eng tech choice was wrong.\nESCALATE: REVERT_TO_PHASE plan-eng \u{2014} fix-loop 撞顶,根因在选型",
        None,
    )]);
    let stdin = json!({
        "cwd": fx.project_dir,
        "transcript_path": fx.transcript_path,
    });

    parse_phase_end(&fx.paths, &stdin).unwrap();
    let events = fx.read_progress_lines();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["event"], "escalate");
    assert_eq!(events[0]["kind"], "revert");
    assert_eq!(events[0]["target_phase"], "plan-eng");
    assert_eq!(events[0]["reason"], "fix-loop 撞顶,根因在选型");
}

#[test]
fn parse_phase_end_emits_abort_kind_for_abort_prefix() {
    let fx = Fixture::new("bookmark-mgr-a3f9");
    fx.write_transcript(&[assistant_message(
        "Out of options.\nESCALATE: ABORT \u{2014} ccteam capability exceeded",
        None,
    )]);
    let stdin = json!({
        "cwd": fx.project_dir,
        "transcript_path": fx.transcript_path,
    });

    parse_phase_end(&fx.paths, &stdin).unwrap();
    let events = fx.read_progress_lines();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["kind"], "abort");
    assert!(events[0]["target_phase"].is_null());
    assert_eq!(events[0]["reason"], "ccteam capability exceeded");
}

#[test]
fn parse_phase_end_emits_need_user_input_for_explicit_prefix() {
    let fx = Fixture::new("bookmark-mgr-a3f9");
    fx.write_transcript(&[assistant_message(
        "spec is too thin.\nESCALATE: NEED_USER_INPUT \u{2014} (1) platform? (2) audience?",
        None,
    )]);
    let stdin = json!({
        "cwd": fx.project_dir,
        "transcript_path": fx.transcript_path,
    });

    parse_phase_end(&fx.paths, &stdin).unwrap();
    let events = fx.read_progress_lines();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["kind"], "need_user_input");
    assert_eq!(events[0]["reason"], "(1) platform? (2) audience?");
}

#[test]
fn parse_phase_end_uses_stdin_last_assistant_message_when_present() {
    // Real Claude Code 2.x Stop hooks race against the transcript flush
    // — by the time we read transcript.jsonl, the final assistant turn
    // may not be there yet. But Claude Code passes the same text on
    // stdin as `last_assistant_message`. We must prefer stdin so the
    // hook never misses PHASE_DONE because of a flush race.
    let fx = Fixture::new("bookmark-mgr-a3f9");
    // Transcript has only the *previous* assistant turn (tool_use, no
    // PHASE_DONE) — simulating the not-yet-flushed final turn.
    fx.write_transcript(&[json!({
        "type": "assistant",
        "message": {
            "role": "assistant",
            "content": [{"type": "tool_use", "name": "Read", "input": {}}],
        }
    })]);
    let stdin = json!({
        "cwd": fx.project_dir,
        "transcript_path": fx.transcript_path,
        "last_assistant_message": "All set.\n\nPHASE_DONE: plan-eng",
    });

    parse_phase_end(&fx.paths, &stdin).unwrap();
    let events = fx.read_progress_lines();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["event"], "phase_done");
    assert_eq!(events[0]["phase"], "plan-eng");
}

#[test]
fn parse_phase_end_handles_claude_code_2x_schema() {
    // Real Claude Code 2.1.x transcripts wrap each turn under
    // `type: "assistant"` (not the older `type: "message"` shape our
    // earlier test fixture used). Regressing on this would mean every
    // production session exits a phase without the orchestrator ever
    // seeing PHASE_DONE — exactly the bug uncovered during M0 e2e.
    let fx = Fixture::new("bookmark-mgr-a3f9");
    let live_shape = json!({
        "type": "assistant",
        "message": {
            "type": "message",
            "role": "assistant",
            "content": [{
                "type": "text",
                "text": "All set.\n\nPHASE_DONE: ship"
            }],
            "usage": {"input_tokens": 10, "output_tokens": 5}
        }
    });
    fx.write_transcript(&[live_shape]);
    let stdin = json!({
        "cwd": fx.project_dir,
        "transcript_path": fx.transcript_path,
    });

    parse_phase_end(&fx.paths, &stdin).unwrap();
    let events = fx.read_progress_lines();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["event"], "phase_done");
    assert_eq!(events[0]["phase"], "ship");
}

#[test]
fn cost_accumulate_handles_claude_code_2x_schema() {
    // Same regression target on the cost path — pre-fix, scan_transcript
    // returned (0.0, 0) for every real session because no top-level
    // `type` matched `"message"`.
    let fx = Fixture::new("bookmark-mgr-a3f9");
    let live_shape = json!({
        "type": "assistant",
        "message": {
            "type": "message",
            "role": "assistant",
            "model": "claude-opus-4-7",
            "content": [{"type": "text", "text": "ok"}],
            "usage": {
                "input_tokens": 100,
                "output_tokens": 50,
                "cache_read_input_tokens": 1000,
                "cache_creation_input_tokens": 200
            }
        }
    });
    fx.write_transcript(&[live_shape]);

    let stdin = json!({
        "cwd": fx.project_dir,
        "transcript_path": fx.transcript_path,
    });
    cost_accumulate(&fx.paths, &stdin).unwrap();
    let state = fx.read_state();
    assert!(
        state.cost_used_usd > 0.0,
        "cost must be non-zero after one assistant turn, got: ${}",
        state.cost_used_usd
    );
    assert!(state.context_tokens_used > 0);
}

#[test]
fn parse_phase_end_returns_block_missing_output_when_no_sigil_no_outbox() {
    // V0.2 M0.19: Stop hook fallback. The phase wrote nothing legal —
    // no PHASE_DONE / ESCALATE in the assistant text, no outbox file,
    // and no `stop_hook_active` recursion guard. The hook returns
    // BlockMissingOutput so the dispatcher exits 2 and stderr is
    // re-injected by Claude Code.
    let fx = Fixture::new("bookmark-mgr-a3f9");
    fx.write_transcript(&[assistant_message(
        "Just a plain answer with no terminal sigil.",
        None,
    )]);
    let stdin = json!({
        "cwd": fx.project_dir,
        "transcript_path": fx.transcript_path,
    });

    let decision = parse_phase_end(&fx.paths, &stdin).unwrap();
    match decision {
        ccteam_hooks::ParseDecision::BlockMissingOutput { stderr } => {
            assert!(stderr.contains("PHASE_DONE"), "stderr: {stderr}");
            assert!(stderr.contains("outbox"), "stderr: {stderr}");
        }
        other => panic!("expected BlockMissingOutput, got {other:?}"),
    }
    let p = fx.paths.progress_jsonl(&fx.slug);
    assert!(
        !p.exists(),
        "no progress event should be appended when phase produced nothing legal",
    );
}

#[test]
fn parse_phase_end_uses_only_the_most_recent_assistant_message() {
    let fx = Fixture::new("bookmark-mgr-a3f9");
    fx.write_transcript(&[
        assistant_message("PHASE_DONE: plan-eng", None),
        json!({"type": "message", "message": {"role": "user", "content": "next"}}),
        assistant_message("PHASE_DONE: implement", None),
    ]);
    let stdin = json!({
        "cwd": fx.project_dir,
        "transcript_path": fx.transcript_path,
    });

    parse_phase_end(&fx.paths, &stdin).unwrap();
    let events = fx.read_progress_lines();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["phase"], "implement");
}

#[test]
fn cost_accumulate_updates_context_tokens_from_latest_usage() {
    let fx = Fixture::new("bookmark-mgr-a3f9");
    fx.write_transcript(&[assistant_message(
        "ack",
        Some(json!({
            "input_tokens": 1000,
            "output_tokens": 200,
            "cache_read_input_tokens": 80_000,
            "cache_creation_input_tokens": 5_000
        })),
    )]);
    let stdin = json!({
        "cwd": fx.project_dir,
        "transcript_path": fx.transcript_path,
    });

    cost_accumulate(&fx.paths, &stdin).unwrap();
    let s = fx.read_state();
    assert_eq!(s.context_tokens_used, 1000 + 80_000 + 5_000);
    assert_eq!(s.last_event_type.as_deref(), Some("PostToolUse"));
    assert!(s.last_progress_event_at.is_some());
}

#[test]
fn cost_accumulate_sums_dollars_across_all_assistant_turns() {
    use ccteam_hooks::cost::{message_cost_usd, scan_transcript};
    let fx = Fixture::new("cost-sum");
    fx.write_transcript(&[
        json!({
            "type": "message",
            "message": {
                "role": "assistant",
                "model": "claude-sonnet-4-6",
                "usage": {
                    "input_tokens": 1_000,
                    "output_tokens": 200,
                    "cache_read_input_tokens": 0,
                    "cache_creation_input_tokens": 0,
                }
            }
        }),
        json!({
            "type": "message",
            "message": {
                "role": "assistant",
                "model": "claude-opus-4-7",
                "usage": {
                    "input_tokens": 500,
                    "output_tokens": 100,
                    "cache_read_input_tokens": 80_000,
                    "cache_creation_input_tokens": 0,
                }
            }
        }),
    ]);

    let (total, last_tokens) = scan_transcript(&fx.transcript_path).unwrap();
    // sonnet turn: 1000 * 3 / 1e6  +  200 * 15 / 1e6  = 0.003 + 0.003 = 0.006
    // opus turn:   500 * 15 / 1e6  +  100 * 75 / 1e6  +  80000 * 1.5 / 1e6
    //            = 0.0075 + 0.0075 + 0.12 = 0.135
    let expected = 0.006 + 0.135;
    assert!(
        (total - expected).abs() < 1e-6,
        "total {total} ≉ expected {expected}",
    );
    assert_eq!(last_tokens, 500 + 80_000);

    // Smoke: message_cost_usd on a single message also works
    let opus_msg = json!({
        "model": "claude-opus-4-7",
        "usage": {"input_tokens": 1_000_000, "output_tokens": 0,
                  "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0}
    });
    assert!((message_cost_usd(&opus_msg) - 15.0).abs() < 1e-6);

    let stdin = json!({
        "cwd": fx.project_dir,
        "transcript_path": fx.transcript_path,
    });
    cost_accumulate(&fx.paths, &stdin).unwrap();
    let s = fx.read_state();
    assert!((s.cost_used_usd - expected).abs() < 1e-6);
    assert_eq!(s.context_tokens_used, last_tokens);
}

#[test]
fn cost_accumulate_no_op_when_no_assistant_message_yet() {
    let fx = Fixture::new("bookmark-mgr-a3f9");
    std::fs::write(&fx.transcript_path, "").unwrap();
    let stdin = json!({
        "cwd": fx.project_dir,
        "transcript_path": fx.transcript_path,
    });

    cost_accumulate(&fx.paths, &stdin).unwrap();
    let s = fx.read_state();
    assert_eq!(s.context_tokens_used, 0);
}

#[test]
fn load_context_writes_ready_marker_under_dot_ccteam() {
    let fx = Fixture::new("bookmark-mgr-a3f9");
    let stdin = json!({"cwd": fx.project_dir});

    load_context(&fx.paths, &stdin).unwrap();
    let ready = fx.project_dir.join(".ccteam").join("ready");
    assert!(
        ready.exists(),
        "load-context must create {} so orchestrator can detect the launched session",
        ready.display(),
    );
}

#[test]
fn load_context_creates_dot_ccteam_dir_when_missing() {
    let tmp = TempDir::new().unwrap();
    let paths = CcteamPaths {
        root: tmp.path().join("home"),
        projects_root: tmp.path().join("projects"),
    };
    let project = paths.project_dir("brand-new");
    std::fs::create_dir_all(&project).unwrap();
    // Note: no `.ccteam/` subdir yet — load-context must create it.
    let stdin = json!({"cwd": project});

    load_context(&paths, &stdin).unwrap();
    assert!(project.join(".ccteam/ready").exists());
}

#[test]
fn progress_append_errors_when_state_json_absent() {
    // Project dir exists but no state.json — slug discovery must fail loudly
    // rather than silently dropping events.
    let tmp = TempDir::new().unwrap();
    let paths = CcteamPaths {
        root: tmp.path().join("ccteam-home"),
        projects_root: tmp.path().join("projects-home"),
    };
    let project_dir = paths.project_dir("missing");
    std::fs::create_dir_all(&project_dir).unwrap();
    let stdin = json!({"cwd": Path::to_str(&project_dir).unwrap()});

    let err = progress_append(&paths, "Stop", &stdin).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("state.json"),
        "expected state.json read failure, got: {msg}",
    );
}

// ---------------- V0.2 M0.19 self-loop fallback ----------------

#[test]
fn parse_phase_end_continues_when_phase_wrote_outbox_clarify() {
    // V0.2 M0.19: a phase that ended in a user-decision pause writes
    // .ccteam/outbox/clarify-<ts>.md as one of the three legal outputs.
    // The Stop hook must NOT block — the orchestrator's existing
    // decisions queue path takes over.
    let fx = Fixture::new("bookmark-mgr-a3f9");
    fx.write_transcript(&[assistant_message(
        "Need a sec — written .ccteam/outbox/clarify-2026-05-08-001.md.",
        None,
    )]);
    let outbox = fx.project_dir.join(".ccteam").join("outbox");
    std::fs::create_dir_all(&outbox).unwrap();
    std::fs::write(
        outbox.join("clarify-2026-05-08-001.md"),
        "---\nschema_version: 1\nevent_kind: clarify\n---\n\nq?",
    )
    .unwrap();
    let stdin = json!({
        "cwd": fx.project_dir,
        "transcript_path": fx.transcript_path,
    });

    let decision = parse_phase_end(&fx.paths, &stdin).unwrap();
    assert_eq!(decision, ccteam_hooks::ParseDecision::Continue);
}

#[test]
fn parse_phase_end_appends_needs_attention_when_stop_hook_active_recurses() {
    // V0.2 M0.19 L3: second Stop entry carries `stop_hook_active: true`.
    // The hook must not block again (recursion guard) and instead drop
    // a `needs_attention.outbox.json` so the watchdog (M0.21) can
    // surface the stall to the user.
    let fx = Fixture::new("bookmark-mgr-a3f9");
    fx.write_transcript(&[assistant_message("I'll keep waiting for clarity.", None)]);
    let stdin = json!({
        "cwd": fx.project_dir,
        "transcript_path": fx.transcript_path,
        "stop_hook_active": true,
    });

    let decision = parse_phase_end(&fx.paths, &stdin).unwrap();
    assert_eq!(decision, ccteam_hooks::ParseDecision::Continue);

    let outbox = ccteam_hooks::needs_attention_outbox_path(&fx.project_dir);
    assert!(
        outbox.exists(),
        "needs_attention.outbox.json must be written"
    );
    let body: Value = serde_json::from_slice(&std::fs::read(&outbox).unwrap()).unwrap();
    assert_eq!(body["schema_version"], 1);
    assert_eq!(body["slug"], "bookmark-mgr-a3f9");
    assert!(body["last_assistant_message"]
        .as_str()
        .unwrap()
        .contains("waiting"));
}

#[test]
fn parse_phase_end_ignores_stale_outbox_from_previous_phase() {
    // Simulate: previous phase wrote clarify-A.md, then orchestrator
    // dispatched the next phase (phase_inject ts in progress.jsonl);
    // current phase ended with no new output. The Stop hook must NOT
    // treat the stale file as a fresh output — it should fall to
    // BlockMissingOutput so the assistant gets re-prompted.
    let fx = Fixture::new("bookmark-mgr-a3f9");
    let outbox = fx.project_dir.join(".ccteam").join("outbox");
    std::fs::create_dir_all(&outbox).unwrap();
    let stale = outbox.join("clarify-old-phase.md");
    std::fs::write(&stale, "old").unwrap();

    // Backdate the stale file so phase_inject's ts is "later".
    let early = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    let f = std::fs::File::options().write(true).open(&stale).unwrap();
    f.set_times(std::fs::FileTimes::new().set_modified(early))
        .unwrap();

    let progress_path = fx.paths.progress_jsonl(&fx.slug);
    std::fs::create_dir_all(progress_path.parent().unwrap()).unwrap();
    // phase_inject at "now" — well after the backdated outbox file.
    let later = chrono::Utc::now().to_rfc3339();
    std::fs::write(
        &progress_path,
        format!(
            "{}\n",
            json!({"event": "phase_inject", "phase": "implement", "ts": later})
        ),
    )
    .unwrap();

    fx.write_transcript(&[assistant_message("done thinking", None)]);
    let stdin = json!({
        "cwd": fx.project_dir,
        "transcript_path": fx.transcript_path,
    });
    let decision = parse_phase_end(&fx.paths, &stdin).unwrap();
    assert!(matches!(
        decision,
        ccteam_hooks::ParseDecision::BlockMissingOutput { .. }
    ));
}

#[test]
fn parse_phase_end_treats_legacy_outbox_files_as_fresh_when_no_phase_inject() {
    // No phase_inject event in progress.jsonl yet (e.g. project just
    // bootstrapped) → the hook can't compute a "since" cutoff. It
    // must still treat any pre-existing outbox file as legitimate and
    // keep returning Continue.
    let fx = Fixture::new("bookmark-mgr-a3f9");
    fx.write_transcript(&[assistant_message("see clarify file", None)]);
    let outbox = fx.project_dir.join(".ccteam").join("outbox");
    std::fs::create_dir_all(&outbox).unwrap();
    std::fs::write(outbox.join("clarify-pre-existing.md"), "body").unwrap();
    let stdin = json!({
        "cwd": fx.project_dir,
        "transcript_path": fx.transcript_path,
    });
    let decision = parse_phase_end(&fx.paths, &stdin).unwrap();
    assert_eq!(decision, ccteam_hooks::ParseDecision::Continue);
}

#[test]
fn parse_phase_end_does_not_recurse_after_first_block_missing_output() {
    // Second-entry contract: stop_hook_active=true short-circuits to
    // append + Continue, never to BlockMissingOutput. Otherwise the
    // assistant could oscillate between Stop entries forever.
    let fx = Fixture::new("bookmark-mgr-a3f9");
    fx.write_transcript(&[assistant_message("still nothing", None)]);
    let stdin = json!({
        "cwd": fx.project_dir,
        "transcript_path": fx.transcript_path,
        "stop_hook_active": true,
    });
    let decision = parse_phase_end(&fx.paths, &stdin).unwrap();
    assert!(matches!(decision, ccteam_hooks::ParseDecision::Continue));
}

#[test]
fn intercept_ask_decision_returns_deny_with_outbox_reason() {
    // V0.2 M0.19.3: pure unit shape — the decision JSON must carry a
    // `permissionDecision: deny` and the reason must steer the
    // assistant toward the outbox protocol.
    let v = ccteam_hooks::intercept_ask_decision();
    assert_eq!(
        v["hookSpecificOutput"]["hookEventName"].as_str(),
        Some("PreToolUse"),
    );
    assert_eq!(
        v["hookSpecificOutput"]["permissionDecision"].as_str(),
        Some("deny"),
    );
    let reason = v["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .unwrap();
    assert!(reason.contains("outbox"));
    assert!(reason.contains("AskUserQuestion"));
}
