//! REAL end-to-end smoke for the Antigravity CLI adapter.
//!
//! `agy_acp_test` runs against a fake whose wire shapes were captured from a
//! live binary — which proves the adapter against a recording, not against
//! whatever `agy` ships next. This file drives the same adapter against the
//! real `agy acp`. It is `#[ignore]`d so the normal gate stays hermetic; opt
//! in with:
//!
//! ```text
//! cargo test -p ccteam-harness --test agy_live_smoke_test -- --ignored --nocapture
//! ```
//!
//! Guards: a real `agy` must resolve on PATH (or via `CCTEAM_AGY_BIN`) and be
//! signed in; otherwise the test prints why and returns, so machines without
//! it stay green. It spends a few small model calls on the signed-in account.
//!
//! Isolation: `CCTEAM_HOME` is pinned to a temp dir (the adapter records the
//! model catalog there), while `HOME` is left alone — agy's own login lives
//! under it. This is its own test binary precisely because the hermetic suite
//! repoints `HOME` for the whole process.

use std::path::Path;
use std::time::Duration;

use ccteam_harness::{
    write_session_meta, AgentSpecBrief, AgentVendor, AgyAcpAdapter, Directive, DirectiveOutcome,
    HarnessAdapter, PermissionMode, SessionMeta, SessionOrigin, SessionProtocol, SpawnCtx,
    ThreadEvent, ThreadHandle, ThreadItemDetails, TurnInput, AGY_BIN_ENV,
};
use futures::StreamExt;
use tempfile::TempDir;

const TURN_TIMEOUT: Duration = Duration::from_secs(180);

fn agy_resolvable() -> bool {
    let bin = std::env::var(AGY_BIN_ENV).unwrap_or_else(|_| "agy".into());
    std::process::Command::new(bin)
        .arg("--version")
        .output()
        .is_ok_and(|out| out.status.success())
}

fn ctx(project: &Path, sid: &str, permission_mode: PermissionMode) -> SpawnCtx {
    SpawnCtx {
        claude_account: None,
        generation: 0,
        mode: None,
        slug: "agy-smoke".into(),
        sid: sid.into(),
        owner: "user:web-api".into(),
        cwd: project.to_path_buf(),
        project_dir: project.to_path_buf(),
        extra_args: vec![],
        model_id: None,
        effort: None,
        permission_mode,
        secret: String::new(),
        remote: None,
    }
}

fn roleless() -> AgentSpecBrief {
    AgentSpecBrief {
        role: String::new(),
    }
}

/// Run one turn to its end; returns the reply text and the input tokens.
async fn turn(adapter: &AgyAcpAdapter, handle: &ThreadHandle, text: &str) -> (String, u64) {
    let mut stream = adapter.events(handle);
    adapter
        .submit_turn(handle, TurnInput::UserText(text.into()))
        .await
        .expect("submit");
    tokio::time::timeout(TURN_TIMEOUT, async move {
        let mut reply = String::new();
        while let Some(ev) = stream.next().await {
            match ev {
                ThreadEvent::ItemCompleted { item } => {
                    if let ThreadItemDetails::AgentMessage(t) = item.details {
                        reply.push_str(&t);
                    }
                }
                ThreadEvent::TurnCompleted { usage, .. } => return (reply, usage.input_tokens),
                ThreadEvent::TurnFailed { err, .. } => panic!("live turn failed: {err:?}"),
                _ => {}
            }
        }
        panic!("event stream ended before the turn did");
    })
    .await
    .expect("live turn timed out")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "drives the real `agy` binary and spends model calls"]
async fn real_agy_round_trip_resume_model_switch_and_hitl() {
    if !agy_resolvable() {
        eprintln!("skip: no runnable `agy` (PATH or {AGY_BIN_ENV})");
        return;
    }
    let state = TempDir::new().unwrap();
    unsafe {
        std::env::set_var("CCTEAM_HOME", state.path().join(".ccteam"));
    }
    let project = TempDir::new().unwrap();
    let adapter = AgyAcpAdapter::new();

    // ── Phase 1: handshake + one turn + what agy says about itself ────────
    let sid = "s-live";
    let handle = adapter
        .start_thread(&roleless(), &ctx(project.path(), sid, PermissionMode::Skip))
        .await
        .expect("real agy must start (signed in?)");
    let status = adapter.thread_status(&handle).await.unwrap();
    let model = status.model.clone().expect("handshake names a model");
    eprintln!("live: model={model} effort={:?}", status.effort);

    let (reply, input_tokens) = turn(&adapter, &handle, "Reply with exactly one word: pong").await;
    assert!(reply.to_lowercase().contains("pong"), "reply={reply:?}");
    assert!(input_tokens > 0, "agy's own usage fields must be read");
    let context = adapter
        .thread_status(&handle)
        .await
        .unwrap()
        .context
        .expect("usage_update gives occupancy");
    assert!(context.used_tokens.unwrap_or(0) > 0 && context.window_tokens > 0);
    eprintln!(
        "live: context {:?}/{} ({:?})",
        context.used_tokens, context.window_tokens, context.source
    );

    // ── Phase 2: `/model` from agy's own catalog ─────────────────────────
    let picker = adapter
        .handle_directive(
            &handle,
            Directive {
                name: "model".into(),
                args: String::new(),
                choice: None,
            },
        )
        .await
        .unwrap();
    let DirectiveOutcome::NeedsChoice(prompt) = picker else {
        panic!("bare /model must offer agy's catalog, got {picker:?}");
    };
    let other = prompt
        .options
        .iter()
        .map(|o| o.id.clone())
        .find(|id| id.split_whitespace().next() != Some(model.as_str()))
        .expect("the account offers more than one model");
    let outcome = adapter
        .handle_directive(
            &handle,
            Directive {
                name: "model".into(),
                args: other.clone(),
                choice: None,
            },
        )
        .await
        .unwrap();
    assert!(
        matches!(outcome, DirectiveOutcome::Done { .. }),
        "switch to `{other}` refused: {outcome:?}"
    );
    let switched = adapter.thread_status(&handle).await.unwrap();
    assert_eq!(
        switched.model.as_deref(),
        other.split_whitespace().next(),
        "status must report what agy answered"
    );
    eprintln!(
        "live: /model {other} → {:?} · {:?}",
        switched.model, switched.effort
    );

    // ── Phase 3: cold resume keeps the conversation ──────────────────────
    let vendor_uuid = handle.identity.clone();
    adapter.close_thread(&handle).await.unwrap();
    let now = chrono::Utc::now().to_rfc3339();
    write_session_meta(
        project.path(),
        &SessionMeta {
            claude_account: None,
            model_pinned_generation: None,
            mode: None,
            tool_face: None,
            managed_by: Default::default(),
            stopped_at: None,
            sid: sid.into(),
            slug: "agy-smoke".into(),
            vendor: AgentVendor::Agy,
            protocol: SessionProtocol::Acp,
            role: String::new(),
            permission_mode: PermissionMode::Skip,
            owner: "user:web-api".into(),
            vendor_uuid: vendor_uuid.clone(),
            model: None,
            observed_model: None,
            effort: None,
            host: "local".into(),
            created_at: now.clone(),
            last_active: now,
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
        },
    )
    .unwrap();
    let resumed = adapter
        .start_thread(&roleless(), &ctx(project.path(), sid, PermissionMode::Skip))
        .await
        .expect("resume");
    assert_eq!(resumed.identity, vendor_uuid, "same vendor session");
    let (reply, _) = turn(
        &adapter,
        &resumed,
        "What single word did I ask you to reply with earlier? Answer with that word only.",
    )
    .await;
    assert!(
        reply.to_lowercase().contains("pong"),
        "context lost: {reply:?}"
    );
    adapter.close_thread(&resumed).await.unwrap();

    // ── Phase 4: hitl declines the tool; nothing runs, the turn ends ─────
    let hitl = adapter
        .start_thread(
            &roleless(),
            &ctx(project.path(), "s-live-hitl", PermissionMode::Hitl),
        )
        .await
        .expect("hitl start");
    let marker = project.path().join("hitl-must-not-exist.txt");
    let (reply, _) = turn(
        &adapter,
        &hitl,
        "Run the shell command `echo denied > hitl-must-not-exist.txt` and nothing else.",
    )
    .await;
    assert!(
        !marker.exists(),
        "a hitl session ran a command nobody approved (reply={reply:?})"
    );
    adapter.close_thread(&hitl).await.unwrap();
}
