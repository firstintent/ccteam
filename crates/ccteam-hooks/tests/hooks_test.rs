//! Integration tests for the M0.3 hook handlers. Each test plumbs a
//! tempdir-rooted `CcteamPaths`, a fake project (state.json), and the
//! hook stdin payload that Claude Code would normally send, then
//! asserts the resulting filesystem side effect.

use std::path::{Path, PathBuf};

use chrono::Utc;
use serde_json::{json, Value};
use tempfile::TempDir;

use ccteam_core::{CcteamPaths, Parallelism, PhaseState, ProjectState};
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
            created_at: now,
            tmux_session: format!("ccteam-{slug}"),
            claude_session_id: None,
            claude_pid: None,
            phase_state: PhaseState::Idle,
            current_phase: "implement".into(),
            parallelism: Parallelism::Solo,
            phase_history: Vec::new(),
            fix_cycle_count: 0,
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
    assert!(e["ts"].as_str().unwrap().contains("T"), "ts must be ISO 8601");
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
}

#[test]
fn parse_phase_end_silent_when_no_sigil() {
    let fx = Fixture::new("bookmark-mgr-a3f9");
    fx.write_transcript(&[assistant_message(
        "Just a plain answer with no terminal sigil.",
        None,
    )]);
    let stdin = json!({
        "cwd": fx.project_dir,
        "transcript_path": fx.transcript_path,
    });

    parse_phase_end(&fx.paths, &stdin).unwrap();
    let p = fx.paths.progress_jsonl(&fx.slug);
    assert!(
        !p.exists(),
        "no progress.jsonl should be created when there is no terminal sigil",
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
