//! Antigravity CLI ACP adapter tests against the hermetic fake
//! (`fixtures/agy_acp/fake_agy_acp.py`).
//!
//! Gate: no real agy / network. Set `CCTEAM_AGY_BIN` to the fake, whose wire
//! shapes were captured from a live agy 1.5.18 (`references/antigravity-cli`
//! is protocol reference only).

use std::path::{Path, PathBuf};
use std::time::Duration;

use ccteam_harness::execution::agy_acp::spawn_spec::{build_argv, AgySpawnInput};
use ccteam_harness::{
    write_session_meta, AgentSpecBrief, AgentVendor, AgyAcpAdapter, Directive, DirectiveOutcome,
    ExecutionMode, HarnessAdapter, PermissionMode, SessionMeta, SessionOrigin, SessionProtocol,
    SpawnCtx, ThreadEvent, ThreadHandle, ThreadItemDetails, TurnInput, UnifiedTokenUsage,
    AGY_BIN_ENV,
};
use futures::StreamExt;
use serial_test::serial;
use tempfile::TempDir;

/// The fake's fixed session id.
const FAKE_SESSION_ID: &str = "349c21ed-18c4-48da-aca8-4d5b54e09a9e";

const ARGV_DUMP_ENV: &str = "CCTEAM_AGY_ARGV_DUMP";
const PARAMS_DUMP_ENV: &str = "CCTEAM_AGY_PARAMS_DUMP";
const MCP_DUMP_ENV: &str = "CCTEAM_ACP_MCP_DUMP";

fn fake_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/agy_acp/fake_agy_acp.py")
}

/// Pin `HOME` + `CCTEAM_HOME` to one process-wide temp dir. Adapters record
/// vendor model catalogs under `ccteam_root_from_env()`; unpinned, these
/// fixtures would land in the developer's real `~/.ccteam/model-catalog.json`.
fn isolate_state_home() {
    static STATE: std::sync::OnceLock<TempDir> = std::sync::OnceLock::new();
    let dir = STATE.get_or_init(|| TempDir::new().unwrap());
    unsafe {
        std::env::set_var("HOME", dir.path());
        std::env::set_var("CCTEAM_HOME", dir.path().join(".ccteam"));
    }
}

/// One test's view of the fake: the binary override plus the dump files the
/// fake appends to. Dropping it clears every env var it set, so a failing
/// test cannot leak its dumps into the next (the suite is `#[serial]`).
struct Fake {
    tmp: TempDir,
}

impl Fake {
    fn install() -> Self {
        isolate_state_home();
        let bin = fake_bin();
        assert!(bin.is_file(), "missing fake at {}", bin.display());
        let tmp = TempDir::new().unwrap();
        let wrapper = tmp.path().join("agy");
        std::fs::write(
            &wrapper,
            format!("#!/bin/sh\nexec python3 {} \"$@\"\n", bin.display()),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&wrapper).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&wrapper, perms).unwrap();
        }
        let fake = Self { tmp };
        unsafe {
            std::env::set_var(AGY_BIN_ENV, &wrapper);
            std::env::set_var(ARGV_DUMP_ENV, fake.dump("argv"));
            std::env::set_var(PARAMS_DUMP_ENV, fake.dump("params"));
            std::env::set_var(MCP_DUMP_ENV, fake.dump("mcp"));
        }
        fake
    }

    fn dump(&self, name: &str) -> PathBuf {
        self.tmp.path().join(format!("{name}.dump"))
    }

    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.dump(name)).unwrap_or_default()
    }

    /// argv of every fake process started so far, one JSON array per spawn.
    fn argvs(&self) -> Vec<Vec<String>> {
        self.read("argv")
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    /// `session/*` method names in the order the fake received them.
    fn session_methods(&self) -> Vec<String> {
        self.read("mcp")
            .lines()
            .filter_map(|l| l.split('\t').next().map(str::to_string))
            .collect()
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        unsafe {
            for key in [AGY_BIN_ENV, ARGV_DUMP_ENV, PARAMS_DUMP_ENV, MCP_DUMP_ENV] {
                std::env::remove_var(key);
            }
        }
    }
}

fn spawn_ctx(project: &Path, sid: &str) -> SpawnCtx {
    SpawnCtx {
        claude_account: None,
        generation: 0,
        mode: None,
        slug: "demo".into(),
        sid: sid.into(),
        owner: "user:web-api".into(),
        cwd: project.to_path_buf(),
        project_dir: project.to_path_buf(),
        extra_args: vec![],
        model_id: None,
        effort: None,
        permission_mode: PermissionMode::Skip,
        secret: String::new(),
        remote: None,
    }
}

fn roleless() -> AgentSpecBrief {
    AgentSpecBrief {
        role: String::new(),
    }
}

async fn start(adapter: &AgyAcpAdapter, spec: &AgentSpecBrief, ctx: &SpawnCtx) -> ThreadHandle {
    tokio::time::timeout(Duration::from_secs(10), adapter.start_thread(spec, ctx))
        .await
        .expect("start timeout")
        .expect("start ok")
}

/// What `turns` consecutive turns produced: every final agent message, and
/// each turn's outcome (`Ok(usage)` or the failure kind).
struct Outcome {
    finals: Vec<String>,
    turns: Vec<Result<UnifiedTokenUsage, String>>,
}

/// Subscribe BEFORE submitting, then collect until `turns` turns have ended.
fn collect(
    adapter: &AgyAcpAdapter,
    handle: &ThreadHandle,
    turns: usize,
) -> tokio::task::JoinHandle<Outcome> {
    let mut stream = adapter.events(handle);
    tokio::spawn(async move {
        let mut out = Outcome {
            finals: Vec::new(),
            turns: Vec::new(),
        };
        while let Some(ev) = stream.next().await {
            match ev {
                ThreadEvent::ItemCompleted { item } => {
                    if let ThreadItemDetails::AgentMessage(t) = item.details {
                        out.finals.push(t);
                    }
                }
                ThreadEvent::TurnCompleted { usage, .. } => out.turns.push(Ok(usage)),
                ThreadEvent::TurnFailed { err, .. } => out.turns.push(Err(err.kind)),
                _ => {}
            }
            if out.turns.len() == turns {
                break;
            }
        }
        out
    })
}

async fn finish(collector: tokio::task::JoinHandle<Outcome>) -> Outcome {
    tokio::time::timeout(Duration::from_secs(10), collector)
        .await
        .expect("collector timeout")
        .expect("collector join")
}

async fn directive(adapter: &AgyAcpAdapter, handle: &ThreadHandle, args: &str) -> DirectiveOutcome {
    adapter
        .handle_directive(
            handle,
            Directive {
                name: "model".into(),
                args: args.into(),
                choice: None,
            },
        )
        .await
        .unwrap()
}

fn prior_session_meta(sid: &str, vendor_uuid: &str) -> SessionMeta {
    SessionMeta {
        claude_account: None,
        model_pinned_generation: None,
        mode: None,
        tool_face: None,
        managed_by: Default::default(),
        stopped_at: None,
        sid: sid.into(),
        slug: "demo".into(),
        vendor: AgentVendor::Agy,
        protocol: SessionProtocol::Acp,
        role: String::new(),
        permission_mode: PermissionMode::Skip,
        owner: "user:test".into(),
        vendor_uuid: vendor_uuid.into(),
        model: None,
        observed_model: None,
        effort: None,
        host: "local".into(),
        created_at: chrono::Utc::now().to_rfc3339(),
        last_active: chrono::Utc::now().to_rfc3339(),
        origin: SessionOrigin::Ccteam,
        title: None,
        title_source: None,
        turn_count: 1,
        cost_usd: None,
        tokens_total: None,
        role_sha: None,
        skills_sha: None,
        trigger: None,
        parent_sid: None,
        spawned_by_role: None,
        delegation_depth: 0,
    }
}

/// The whole happy path on one session: handshake, the handle the gateway
/// persists, one turn, and what agy's own wire says about usage and context.
#[tokio::test]
#[serial]
async fn handshake_prompt_usage_and_context_on_fake() {
    let fake = Fake::install();
    let tmp = TempDir::new().unwrap();
    let adapter = AgyAcpAdapter::new();
    assert_eq!(adapter.name(), "agy-acp");
    assert_eq!(adapter.vendor(), AgentVendor::Agy);

    let handle = start(&adapter, &roleless(), &spawn_ctx(tmp.path(), "s1")).await;
    assert_eq!(handle.vendor, AgentVendor::Agy);
    assert_eq!(handle.mode, ExecutionMode::Chat);
    assert_eq!(handle.identity, FAKE_SESSION_ID);
    assert_eq!(handle.raw_extras["vendor_uuid"], FAKE_SESSION_ID);
    assert_eq!(handle.raw_extras["protocol"], "acp");
    assert_eq!(handle.raw_extras["adapter"], "agy-acp");
    // The handshake reports base model + thinking level as two axes.
    assert_eq!(handle.raw_extras["model"], "gemini-4-neon");
    let status = adapter.thread_status(&handle).await.unwrap();
    assert_eq!(status.model.as_deref(), Some("gemini-4-neon"));
    assert_eq!(status.effort.as_deref(), Some("high"));

    let collector = collect(&adapter, &handle, 1);
    adapter
        .submit_turn(&handle, TurnInput::UserText("hello".into()))
        .await
        .expect("submit");
    let out = finish(collector).await;

    assert_eq!(
        out.finals,
        vec!["echo:hello"],
        "one final, thoughts excluded"
    );
    let usage = out.turns[0].as_ref().expect("turn completed");
    // agy's own field names (`cachedReadTokens`, `thoughtTokens`) are read.
    assert_eq!(usage.input_tokens, 2298);
    assert_eq!(usage.output_tokens, 47);
    assert_eq!(usage.cached_input_tokens, 7394);
    assert_eq!(usage.reasoning_output_tokens, Some(45));
    assert!(usage.reported_cost_usd.is_none(), "agy reports no USD");

    // Context occupancy is pushed (`usage_update`), not probed.
    let ctx = adapter
        .thread_status(&handle)
        .await
        .unwrap()
        .context
        .expect("usage_update gives occupancy");
    assert_eq!(ctx.used_tokens, Some(9475));
    assert_eq!(ctx.window_tokens, 1_048_576);

    adapter.close_thread(&handle).await.unwrap();

    // A skip session's whole argv: the subcommand and agy's permission switch.
    assert_eq!(
        fake.argvs(),
        vec![vec!["acp", "--dangerously-skip-permissions"]]
    );
    // Red line: agy advertises a system-prompt extension; ccteam never sends it.
    let params = fake.read("params");
    assert!(params.contains("session/new"), "fake saw the handshake");
    assert!(
        !params.contains("systemPrompt") && !params.contains("_meta"),
        "no prompt-injection surface may reach agy: {params}"
    );
}

/// A role is agy's own `--agent`; the fake echoes which agent it runs as.
#[tokio::test]
#[serial]
async fn a_role_becomes_agys_native_agent_flag() {
    let fake = Fake::install();
    let tmp = TempDir::new().unwrap();
    let adapter = AgyAcpAdapter::new();
    let spec = AgentSpecBrief {
        role: "reviewer".into(),
    };
    let handle = start(&adapter, &spec, &spawn_ctx(tmp.path(), "s-role")).await;

    let collector = collect(&adapter, &handle, 1);
    adapter
        .submit_turn(&handle, TurnInput::UserText("hi".into()))
        .await
        .unwrap();
    assert_eq!(finish(collector).await.finals, vec!["[reviewer] echo:hi"]);
    adapter.close_thread(&handle).await.unwrap();

    assert_eq!(
        fake.argvs(),
        vec![vec![
            "acp",
            "--dangerously-skip-permissions",
            "--agent",
            "reviewer"
        ]]
    );
}

/// Skip: agy never asks (its own flag), so a tool turn simply runs.
#[tokio::test]
#[serial]
async fn skip_session_runs_tools_without_a_permission_round_trip() {
    let _fake = Fake::install();
    let tmp = TempDir::new().unwrap();
    let adapter = AgyAcpAdapter::new();
    let handle = start(&adapter, &roleless(), &spawn_ctx(tmp.path(), "s-skip")).await;

    let collector = collect(&adapter, &handle, 1);
    adapter
        .submit_turn(&handle, TurnInput::UserText("USE_TOOL now".into()))
        .await
        .unwrap();
    let out = finish(collector).await;
    assert!(out.turns[0].is_ok());
    assert_eq!(out.finals, vec!["echo:USE_TOOL now"]);
    adapter.close_thread(&handle).await.unwrap();
}

/// Hitl: no permission switch on argv, and the ask is declined — which fails
/// that one tool call. The turn ends cleanly with no agent text (what live
/// agy does) and the session stays usable.
#[tokio::test]
#[serial]
async fn hitl_declines_the_tool_and_the_session_survives() {
    let fake = Fake::install();
    let tmp = TempDir::new().unwrap();
    let adapter = AgyAcpAdapter::new();
    let mut ctx = spawn_ctx(tmp.path(), "s-hitl");
    ctx.permission_mode = PermissionMode::Hitl;
    let handle = start(&adapter, &roleless(), &ctx).await;
    assert_eq!(handle.raw_extras["permission_mode"], "hitl");

    let collector = collect(&adapter, &handle, 1);
    adapter
        .submit_turn(&handle, TurnInput::UserText("USE_TOOL now".into()))
        .await
        .unwrap();
    let out = finish(collector).await;
    assert!(
        out.turns[0].is_ok(),
        "a declined tool must not fail the turn"
    );
    // The shared finalize reports a text-less turn as one empty final; what
    // matters is that no reply text exists — the fake only echoes when the
    // tool was allowed to run.
    assert!(
        out.finals.iter().all(|t| t.is_empty()),
        "a declined tool produced agent text, so it ran: {:?}",
        out.finals
    );

    assert!(adapter.thread_is_live(&handle));
    let collector = collect(&adapter, &handle, 1);
    adapter
        .submit_turn(&handle, TurnInput::UserText("still there?".into()))
        .await
        .unwrap();
    assert_eq!(finish(collector).await.finals, vec!["echo:still there?"]);
    adapter.close_thread(&handle).await.unwrap();

    assert_eq!(
        fake.argvs(),
        vec![vec!["acp"]],
        "hitl must never auto-allow"
    );
}

/// agy's steer answers the running prompt early and folds the reply into the
/// next one. ccteam must never trigger it: a mid-turn message is a second
/// FIFO turn with its own complete reply.
#[tokio::test]
#[serial]
async fn a_mid_turn_message_queues_instead_of_steering() {
    let _fake = Fake::install();
    let tmp = TempDir::new().unwrap();
    let adapter = AgyAcpAdapter::new();
    let handle = start(&adapter, &roleless(), &spawn_ctx(tmp.path(), "s-queue")).await;

    let collector = collect(&adapter, &handle, 2);
    let first = adapter
        .submit_turn(&handle, TurnInput::UserText("one".into()))
        .await
        .expect("first submit ok");
    let second = adapter
        .submit_turn(&handle, TurnInput::UserText("two".into()))
        .await
        .expect("a mid-turn message must queue, not be rejected");
    assert_ne!(first.0, second.0, "queued turn gets its own id");

    let out = finish(collector).await;
    assert_eq!(
        out.finals,
        vec!["echo:one", "echo:two"],
        "two complete replies in FIFO order; `steered:` would mean the \
         prompts overlapped on the wire"
    );
    adapter.close_thread(&handle).await.unwrap();
}

/// A non-clean stop reason is a failed turn, still carrying the partial text.
#[tokio::test]
#[serial]
async fn abnormal_stop_reason_surfaces_as_turn_failed() {
    let _fake = Fake::install();
    let tmp = TempDir::new().unwrap();
    let adapter = AgyAcpAdapter::new();
    let handle = start(&adapter, &roleless(), &spawn_ctx(tmp.path(), "s-stop")).await;

    let collector = collect(&adapter, &handle, 1);
    adapter
        .submit_turn(&handle, TurnInput::UserText("STOP:refusal go".into()))
        .await
        .unwrap();
    let out = finish(collector).await;
    assert_eq!(
        out.turns[0].as_ref().unwrap_err(),
        "stop_reason:refusal",
        "a refused turn must not report completion"
    );
    assert_eq!(out.finals.len(), 1, "partial output is still delivered");
    adapter.close_thread(&handle).await.unwrap();
}

/// Cold resume: `session/resume` (no replay) with the SAME `mcpServers` a
/// fresh session gets — an empty array there would drop the ccteam tool face
/// after any idle release.
#[tokio::test]
#[serial]
async fn resume_keeps_the_vendor_session_and_the_tool_face() {
    let fake = Fake::install();
    let tmp = TempDir::new().unwrap();
    let sid = "s-resume";
    write_session_meta(tmp.path(), &prior_session_meta(sid, "prior-session-id")).unwrap();

    let mut ctx = spawn_ctx(tmp.path(), sid);
    ctx.secret = "seKret1234".into();
    let adapter = AgyAcpAdapter::new();
    let handle = start(&adapter, &roleless(), &ctx).await;
    assert_eq!(handle.identity, "prior-session-id");

    let collector = collect(&adapter, &handle, 1);
    adapter
        .submit_turn(&handle, TurnInput::UserText("again".into()))
        .await
        .unwrap();
    assert_eq!(finish(collector).await.finals, vec!["echo:again"]);
    adapter.close_thread(&handle).await.unwrap();

    assert_eq!(
        fake.read("mcp")
            .lines()
            .next()
            .expect("fake saw a session call"),
        "session/resume\t1",
        "resume first, carrying the ccteam MCP server"
    );
    let methods = fake.session_methods();
    assert!(
        !methods.iter().any(|m| m == "session/new"),
        "a resumable session must not be replaced by a new one"
    );
    // Release goes through agy's own teardown (stops its subagents and
    // background tasks) before the process is let go.
    assert_eq!(
        methods.last().map(String::as_str),
        Some("session/close"),
        "close_thread must end on session/close: {methods:?}"
    );
}

/// A vendor session agy no longer has: resume fails, load fails, and the
/// ladder ends on a fresh session rather than a dead spawn.
#[tokio::test]
#[serial]
async fn a_lost_vendor_session_falls_back_to_a_fresh_one() {
    let fake = Fake::install();
    let tmp = TempDir::new().unwrap();
    let sid = "s-gone";
    write_session_meta(tmp.path(), &prior_session_meta(sid, "gone-1234")).unwrap();

    let adapter = AgyAcpAdapter::new();
    let handle = start(&adapter, &roleless(), &spawn_ctx(tmp.path(), sid)).await;
    assert_eq!(handle.identity, FAKE_SESSION_ID);
    adapter.close_thread(&handle).await.unwrap();

    assert_eq!(
        fake.session_methods()[..3],
        ["session/resume", "session/load", "session/new"]
    );
}

/// `session/load` replays history before answering. None of it may surface
/// as output of the turn that follows.
#[tokio::test]
#[serial]
async fn replayed_history_never_reaches_the_next_turn() {
    let fake = Fake::install();
    let tmp = TempDir::new().unwrap();
    let adapter = AgyAcpAdapter::new();
    // `loadonly-*`: the fake refuses `session/resume`, so the ladder lands on
    // `session/load`, which replays a user + an agent chunk first.
    let sid = "s-load";
    write_session_meta(tmp.path(), &prior_session_meta(sid, "loadonly-77")).unwrap();
    let handle = start(&adapter, &roleless(), &spawn_ctx(tmp.path(), sid)).await;
    assert_eq!(handle.identity, "loadonly-77");
    assert_eq!(
        fake.session_methods(),
        ["session/resume", "session/load"],
        "load restores the session; no fresh one is minted"
    );

    let collector = collect(&adapter, &handle, 1);
    adapter
        .submit_turn(&handle, TurnInput::UserText("fresh".into()))
        .await
        .unwrap();
    let out = finish(collector).await;
    assert_eq!(
        out.finals,
        vec!["echo:fresh"],
        "replayed history leaked into the live turn"
    );
    adapter.close_thread(&handle).await.unwrap();
}

/// Spawn-time picks ride agy's own setters, and the state is whatever agy
/// answered — not an echo of the request.
#[tokio::test]
#[serial]
async fn spawn_picks_land_and_report_what_agy_answered() {
    let fake = Fake::install();
    let tmp = TempDir::new().unwrap();
    let adapter = AgyAcpAdapter::new();

    // A composite id on the model axis comes back as base model + level.
    let mut ctx = spawn_ctx(tmp.path(), "s-composite");
    ctx.model_id = Some("gemini-3.8-flash-low".into());
    let handle = start(&adapter, &roleless(), &ctx).await;
    let status = adapter.thread_status(&handle).await.unwrap();
    assert_eq!(status.model.as_deref(), Some("gemini-3.8-flash"));
    assert_eq!(status.effort.as_deref(), Some("low"));
    assert_eq!(handle.raw_extras["model"], "gemini-3.8-flash");
    adapter.close_thread(&handle).await.unwrap();

    // Model, then effort (the levels belong to the model just selected), then
    // mode through `session/set_mode`.
    let mut ctx = spawn_ctx(tmp.path(), "s-all");
    ctx.model_id = Some("gemini-3.8-flash".into());
    ctx.effort = Some("medium".into());
    ctx.mode = Some("plan".into());
    let handle = start(&adapter, &roleless(), &ctx).await;
    let status = adapter.thread_status(&handle).await.unwrap();
    assert_eq!(status.model.as_deref(), Some("gemini-3.8-flash"));
    assert_eq!(status.effort.as_deref(), Some("medium"));
    adapter.close_thread(&handle).await.unwrap();
    let params = fake.read("params");
    assert!(
        params.contains(r#""method": "session/set_mode""#)
            && params.contains(r#""modeId": "plan""#),
        "mode must reach agy: {params}"
    );

    // A model with no thinking axis reports no effort, not a stale level.
    let mut ctx = spawn_ctx(tmp.path(), "s-no-axis");
    ctx.model_id = Some("gemini-pro-orchestrator".into());
    let handle = start(&adapter, &roleless(), &ctx).await;
    let status = adapter.thread_status(&handle).await.unwrap();
    assert_eq!(status.model.as_deref(), Some("gemini-pro-orchestrator"));
    assert_eq!(status.effort, None);
    adapter.close_thread(&handle).await.unwrap();
}

/// A caller who NAMED a model, effort or mode gets it or an error carrying
/// agy's own reason — never a session quietly running on something else.
#[tokio::test]
#[serial]
async fn a_refused_spawn_pick_fails_the_spawn_with_agys_reason() {
    let fake = Fake::install();
    let tmp = TempDir::new().unwrap();
    let adapter = AgyAcpAdapter::new();

    type Pick = fn(&mut SpawnCtx);
    let cases: [(&str, Pick, &[&str]); 3] = [
        (
            "s-bad-model",
            |c| c.model_id = Some("made-up-model".into()),
            &["model", "Unknown model ID: made-up-model"],
        ),
        (
            "s-bad-effort",
            |c| {
                c.model_id = Some("gemini-3.8-flash".into());
                c.effort = Some("xhigh".into());
            },
            &["effort", "is not offered for model \"Gemini 3.8 Flash\""],
        ),
        (
            "s-bad-mode",
            |c| c.mode = Some("yolo".into()),
            &["mode", "Unknown mode: yolo"],
        ),
    ];
    for (sid, pick, wants) in cases {
        let mut ctx = spawn_ctx(tmp.path(), sid);
        pick(&mut ctx);
        let err = adapter
            .start_thread(&roleless(), &ctx)
            .await
            .expect_err("agy refuses the pick")
            .to_string();
        for want in wants {
            assert!(err.contains(want), "{sid}: `{want}` missing from: {err}");
        }
    }
    // A refused spawn leaves no session behind: each of the three handshaken
    // sessions was closed again, not orphaned with nobody holding its handle.
    let closes = fake
        .session_methods()
        .iter()
        .filter(|m| *m == "session/close")
        .count();
    assert_eq!(closes, 3, "every refused spawn must close what it opened");
}

/// `/model`: the picker is agy's own catalog, an explicit `<id> [effort]`
/// switches live, and the effort is validated by agy for the model being
/// switched TO — not against the levels the session happened to start with.
#[tokio::test]
#[serial]
async fn model_directive_lists_sets_and_follows_the_current_models_levels() {
    let _fake = Fake::install();
    let tmp = TempDir::new().unwrap();
    let adapter = AgyAcpAdapter::new();
    let handle = start(&adapter, &roleless(), &spawn_ctx(tmp.path(), "s-model")).await;

    let picker_ids = |outcome: DirectiveOutcome| -> Vec<String> {
        match outcome {
            DirectiveOutcome::NeedsChoice(p) => p.options.into_iter().map(|o| o.id).collect(),
            other => panic!("expected NeedsChoice, got {other:?}"),
        }
    };
    let ids = picker_ids(directive(&adapter, &handle, "").await);
    assert!(ids.contains(&"gemini-4-neon xhigh".to_string()), "{ids:?}");
    assert!(ids.contains(&"gemini-3.8-flash low".to_string()), "{ids:?}");

    match directive(&adapter, &handle, "gemini-3.8-flash low").await {
        DirectiveOutcome::Done { receipt } => {
            assert!(receipt.contains("gemini-3.8-flash") && receipt.contains("low"));
        }
        other => panic!("expected Done, got {other:?}"),
    }
    let status = adapter.thread_status(&handle).await.unwrap();
    assert_eq!(status.model.as_deref(), Some("gemini-3.8-flash"));
    assert_eq!(status.effort.as_deref(), Some("low"));

    // The picker now offers the levels of the model that is actually running.
    let ids = picker_ids(directive(&adapter, &handle, "").await);
    assert!(
        !ids.iter().any(|id| id.ends_with(" xhigh")),
        "flash has no xhigh; a picker still offering it is the handshake's stale copy: {ids:?}"
    );

    // …yet `xhigh` is still reachable, because it exists on the TARGET model.
    assert!(matches!(
        directive(&adapter, &handle, "gemini-4-neon xhigh").await,
        DirectiveOutcome::Done { .. }
    ));
    let status = adapter.thread_status(&handle).await.unwrap();
    assert_eq!(status.model.as_deref(), Some("gemini-4-neon"));
    assert_eq!(status.effort.as_deref(), Some("xhigh"));

    match directive(&adapter, &handle, "no-such-model").await {
        DirectiveOutcome::Rejected { reason } => {
            assert!(
                reason.contains("Unknown model ID: no-such-model"),
                "{reason}"
            );
        }
        other => panic!("expected Rejected, got {other:?}"),
    }
    assert_eq!(
        adapter
            .thread_status(&handle)
            .await
            .unwrap()
            .model
            .as_deref(),
        Some("gemini-4-neon"),
        "a refused switch changes nothing"
    );
    adapter.close_thread(&handle).await.unwrap();
}

/// `/compact` is passed through as agy's own slash command; the occupancy
/// agy then reports replaces the old one.
#[tokio::test]
#[serial]
async fn compact_is_passed_through_as_agys_slash_command() {
    let fake = Fake::install();
    let tmp = TempDir::new().unwrap();
    let adapter = AgyAcpAdapter::new();
    let handle = start(&adapter, &roleless(), &spawn_ctx(tmp.path(), "s-compact")).await;

    let collector = collect(&adapter, &handle, 1);
    adapter
        .submit_turn(&handle, TurnInput::UserText("fill".into()))
        .await
        .unwrap();
    finish(collector).await;

    let collector = collect(&adapter, &handle, 1);
    let outcome = adapter
        .handle_directive(
            &handle,
            Directive {
                name: "/compact".into(),
                args: String::new(),
                choice: None,
            },
        )
        .await
        .unwrap();
    assert!(
        matches!(outcome, DirectiveOutcome::Done { .. }),
        "{outcome:?}"
    );
    assert!(finish(collector).await.turns[0].is_ok());

    let ctx = adapter
        .thread_status(&handle)
        .await
        .unwrap()
        .context
        .expect("occupancy after compact");
    assert_eq!(ctx.used_tokens, Some(1200));
    assert!(
        fake.read("params").contains(r#""text": "/compact""#),
        "the slash command must reach agy verbatim"
    );
    adapter.close_thread(&handle).await.unwrap();
}

/// The fake rejects unknown flags the way the real binary does (usage + exit
/// before `initialize`). This pins that the guard has teeth: a spawn whose
/// argv agy does not know dies loudly instead of hanging.
#[tokio::test]
#[serial]
async fn an_unknown_argv_flag_kills_the_spawn() {
    let _fake = Fake::install();
    let tmp = TempDir::new().unwrap();
    // Wrap the wrapper: same fake, plus a flag `agy acp` does not define.
    let inner = std::env::var(AGY_BIN_ENV).unwrap();
    let outer = tmp.path().join("agy-bad-flag");
    std::fs::write(
        &outer,
        format!("#!/bin/sh\nsub=\"$1\"; shift\nexec {inner} \"$sub\" --auto \"$@\"\n"),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&outer, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    unsafe {
        std::env::set_var(AGY_BIN_ENV, &outer);
    }

    let adapter = AgyAcpAdapter::new();
    let err = tokio::time::timeout(
        Duration::from_secs(10),
        adapter.start_thread(&roleless(), &spawn_ctx(tmp.path(), "s-bad-flag")),
    )
    .await
    .expect("a dead child must fail the spawn, not hang it")
    .expect_err("agy exits on an unknown flag");
    assert!(err.to_string().contains("agy"), "err={err}");
}

#[test]
fn spawn_spec_has_no_pty_flags() {
    for mode in [PermissionMode::Skip, PermissionMode::Hitl] {
        let argv = build_argv(
            "agy",
            &AgySpawnInput {
                permission_mode: mode,
                role: "",
            },
        );
        assert_eq!(argv[..2], ["agy", "acp"]);
        assert!(!argv
            .iter()
            .any(|a| a.contains("tmux") || a.contains("rmux")));
    }
}

#[test]
fn no_tmux_rmux_imports_in_agy_module_sources() {
    // Structural red line: a new harness must not depend on tmux/rmux.
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/execution/agy_acp");
    for entry in std::fs::read_dir(&root).unwrap() {
        let entry = entry.unwrap();
        if entry.path().extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let body = std::fs::read_to_string(entry.path()).unwrap();
        for line in body.lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            assert!(
                !trimmed.contains("use crate::tmux")
                    && !trimmed.contains("use crate::rmux")
                    && !trimmed.contains("tmux_backend")
                    && !trimmed.contains("rmux_backend")
                    && !trimmed.contains("tmux_ops"),
                "{} must not import tmux/rmux: {trimmed}",
                entry.path().display()
            );
        }
    }
}

/// Remote execution is claude-only (red line); agy must fail clean + readable
/// rather than silently spawning locally under a remote host id. No fake
/// binary needed — the guard runs before spawn.
#[tokio::test(flavor = "current_thread")]
async fn start_thread_rejects_remote_ctx_readable() {
    let tmp = TempDir::new().unwrap();
    let mut ctx = spawn_ctx(tmp.path(), "s-remote");
    ctx.remote = Some(ccteam_harness::RemoteExecTarget {
        host_id: "sat".into(),
        wire_slug: "demo".into(),
        hub: std::sync::Arc::new(ccteam_harness::HostChannelHub::default()),
    });
    let err = AgyAcpAdapter::new()
        .start_thread(&roleless(), &ctx)
        .await
        .expect_err("remote agy must be rejected");
    assert!(
        err.to_string().contains("not yet supported for agy"),
        "got: {err}"
    );
}
