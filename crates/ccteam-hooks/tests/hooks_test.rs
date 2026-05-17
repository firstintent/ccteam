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
use ccteam_hooks::{load_context, progress_append};

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
        // V0.4.6 F91 — `cost_used_usd` is deprecated; we still set it
        // in struct literals because the field is non-Option<…> for
        // serde compat. The single-attribute allow silences the warning
        // in fixtures (the field is never read post-F91).
        #[allow(deprecated)]
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
            detached: false,
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
