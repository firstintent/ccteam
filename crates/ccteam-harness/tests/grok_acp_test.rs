//! Grok ACP adapter tests against the hermetic fake (`fixtures/grok_acp/fake_grok_acp.py`).
//!
//! Gate: no real grok / network. Set `CCTEAM_GROK_BIN` to the fake.

use std::path::PathBuf;
use std::time::Duration;

use ccteam_harness::execution::grok_acp::spawn_spec::{build_argv, GrokSpawnInput};
use ccteam_harness::{
    write_session_meta, AgentSpecBrief, AgentVendor, ExecutionMode, GrokAcpAdapter, HarnessAdapter,
    PermissionMode, SessionMeta, SessionOrigin, SessionProtocol, SpawnCtx, ThreadEvent,
    ThreadItemDetails, TurnDisposition, TurnInput, TurnRouting, GROK_BIN_ENV,
};
use futures::StreamExt;
use serial_test::serial;
use tempfile::TempDir;

fn fake_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/grok_acp/fake_grok_acp.py")
}

fn install_fake() {
    let bin = fake_bin();
    assert!(bin.is_file(), "missing fake at {}", bin.display());
    // Prefer python3 runner wrapper so the fake is executable without +x issues.
    let wrapper = std::env::temp_dir().join(format!(
        "ccteam-fake-grok-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let body = format!("#!/bin/sh\nexec python3 {} \"$@\"\n", bin.display());
    std::fs::write(&wrapper, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&wrapper).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&wrapper, perms).unwrap();
    }
    // SAFETY: tests are serial for GROK_BIN_ENV.
    unsafe {
        std::env::set_var(GROK_BIN_ENV, &wrapper);
    }
}

fn clear_fake() {
    unsafe {
        std::env::remove_var(GROK_BIN_ENV);
    }
}

fn spawn_ctx(tmp: &TempDir, sid: &str) -> SpawnCtx {
    SpawnCtx {
        slug: "demo".into(),
        sid: sid.into(),
        cwd: tmp.path().to_path_buf(),
        project_dir: tmp.path().to_path_buf(),
        extra_args: vec![],
        model_id: None,
        effort: None,
        permission_mode: PermissionMode::Skip,
        secret: String::new(),
        remote: None,
    }
}

#[test]
fn spawn_spec_always_approve_before_stdio() {
    let argv = build_argv(
        "grok",
        &GrokSpawnInput {
            permission_mode: PermissionMode::Skip,
            model_id: Some("grok-4.5"),
        },
    );
    assert_eq!(
        argv,
        vec![
            "grok",
            "agent",
            "--always-approve",
            "-m",
            "grok-4.5",
            "stdio"
        ]
    );
}

#[tokio::test]
#[serial]
async fn handshake_prompt_final_only_on_fake() {
    install_fake();
    let tmp = TempDir::new().unwrap();
    let adapter = GrokAcpAdapter::new();
    let handle = tokio::time::timeout(
        Duration::from_secs(10),
        adapter.start_thread(
            &AgentSpecBrief {
                role: String::new(),
            },
            &spawn_ctx(&tmp, "s1"),
        ),
    )
    .await
    .expect("start timeout")
    .expect("start ok");

    assert_eq!(handle.vendor, AgentVendor::Grok);
    assert_eq!(handle.mode, ExecutionMode::Chat);
    assert!(!handle.identity.is_empty());
    assert_eq!(
        handle
            .raw_extras
            .get("vendor_uuid")
            .and_then(|v| v.as_str()),
        Some(handle.identity.as_str())
    );
    assert_eq!(
        handle.raw_extras.get("protocol").and_then(|v| v.as_str()),
        Some("acp")
    );

    let mut stream = adapter.events(&handle);
    let collector = tokio::spawn(async move {
        let mut finals = Vec::new();
        let mut usage_in = None;
        let mut model = None;
        while let Some(ev) = stream.next().await {
            match ev {
                ThreadEvent::ItemCompleted { item } => {
                    if let ThreadItemDetails::AgentMessage(t) = item.details {
                        finals.push(t);
                    }
                }
                ThreadEvent::TurnCompleted {
                    usage, model: m, ..
                } => {
                    usage_in = Some(usage.input_tokens);
                    model = m;
                    break;
                }
                ThreadEvent::TurnFailed { err, .. } => {
                    panic!("turn failed: {err:?}");
                }
                _ => {}
            }
        }
        (finals, usage_in, model)
    });

    tokio::time::sleep(Duration::from_millis(50)).await;
    adapter
        .submit_turn(&handle, TurnInput::UserText("hello".into()))
        .await
        .expect("submit");

    let (finals, usage_in, model) = tokio::time::timeout(Duration::from_secs(10), collector)
        .await
        .expect("collector timeout")
        .expect("collector join");

    assert_eq!(finals.len(), 1, "exactly one final AgentMessage");
    assert_eq!(finals[0], "echo:hello");
    assert!(!finals[0].contains("thinking"), "thoughts not in final");
    assert!(!finals[0].contains("REPLAY"), "no replay text");
    assert_eq!(usage_in, Some(100));
    assert_eq!(model.as_deref(), Some("grok-4.5"));

    let status = adapter.thread_status(&handle).await.unwrap();
    assert_eq!(status.model.as_deref(), Some("grok-4.5"));

    adapter.close_thread(&handle).await.unwrap();
    clear_fake();
}

/// Mid-turn steer: a second `submit_turn` fired before the first turn finalizes
/// must be injected into that active turn without cancelling it. Grok returns
/// the active turn id and produces one combined final answer.
#[tokio::test]
#[serial]
async fn mid_turn_submit_interjects_active_turn_by_default() {
    install_fake();
    let tmp = TempDir::new().unwrap();
    let adapter = GrokAcpAdapter::new();
    let handle = tokio::time::timeout(
        Duration::from_secs(10),
        adapter.start_thread(
            &AgentSpecBrief {
                role: String::new(),
            },
            &spawn_ctx(&tmp, "s1"),
        ),
    )
    .await
    .expect("start timeout")
    .expect("start ok");

    let mut stream = adapter.events(&handle);
    let collector = tokio::spawn(async move {
        let mut finals = Vec::new();
        while let Some(ev) = stream.next().await {
            match ev {
                ThreadEvent::ItemCompleted { item } => {
                    if let ThreadItemDetails::AgentMessage(t) = item.details {
                        finals.push(t);
                    }
                }
                ThreadEvent::TurnCompleted { .. } => {
                    break;
                }
                ThreadEvent::TurnFailed { err, .. } => panic!("turn failed: {err:?}"),
                _ => {}
            }
        }
        finals
    });

    // Fire both messages back-to-back. The second lands while the first is
    // active and must use Grok's no-cancel `_x.ai/interject` extension.
    let first = adapter
        .submit_turn_routed(
            &handle,
            TurnInput::UserText("one".into()),
            TurnRouting::Inject,
        )
        .await
        .expect("first submit ok");
    let second_submit = adapter.submit_turn_routed(
        &handle,
        TurnInput::UserText("two".into()),
        TurnRouting::Inject,
    );
    let third_submit = adapter.submit_turn_routed(
        &handle,
        TurnInput::UserText("three".into()),
        TurnRouting::Inject,
    );
    let (second, third) = tokio::join!(second_submit, third_submit);
    let mut second = second.expect("second submit must interject, not reject");
    let mut third = third.expect("third submit must preserve concurrent FIFO");
    assert_eq!(
        first.turn_id, second.turn_id,
        "interjection belongs to the active turn"
    );
    assert_eq!(
        first.turn_id, third.turn_id,
        "every interjection reuses the active turn"
    );
    assert_eq!(first.disposition, TurnDisposition::Started);
    assert_eq!(second.disposition, TurnDisposition::Injected);
    assert_eq!(third.disposition, TurnDisposition::Injected);
    assert_ne!(second.input_id, third.input_id);
    assert_ne!(first.input_id, second.input_id);
    second.release_completion();
    third.release_completion();

    let finals = tokio::time::timeout(Duration::from_secs(10), collector)
        .await
        .expect("collector timeout")
        .expect("collector join");

    assert_eq!(finals.len(), 1, "interjection keeps one turn boundary");
    assert_eq!(finals[0], "echo:one|interject:two|three");

    adapter.close_thread(&handle).await.unwrap();
    clear_fake();
}

/// Real Grok admits an interjection even after its active turn completed, then
/// self-starts an answer without a matching `session/prompt` request. That
/// answer must surface as its own synthetic turn instead of being dropped or
/// glued onto the completed prompt's buffer.
#[tokio::test]
#[serial]
async fn completion_edge_interject_surfaces_vendor_self_started_turn() {
    install_fake();
    let tmp = TempDir::new().unwrap();
    let adapter = GrokAcpAdapter::new();
    let handle = adapter
        .start_thread(
            &AgentSpecBrief {
                role: String::new(),
            },
            &spawn_ctx(&tmp, "s-late"),
        )
        .await
        .expect("start ok");

    let mut stream = adapter.events(&handle);
    let collector = tokio::spawn(async move {
        let mut started = Vec::new();
        let mut finals = Vec::new();
        let mut completed = Vec::new();
        while let Some(event) = stream.next().await {
            match event {
                ThreadEvent::TurnStarted { turn_id } => started.push(turn_id),
                ThreadEvent::ItemCompleted { item } => {
                    if let ThreadItemDetails::AgentMessage(text) = item.details {
                        finals.push((item.id, text));
                    }
                }
                ThreadEvent::TurnCompleted { turn_id, .. } => {
                    completed.push(turn_id);
                    if completed.len() == 2 {
                        break;
                    }
                }
                ThreadEvent::TurnFailed { err, .. } => panic!("turn failed: {err:?}"),
                _ => {}
            }
        }
        (started, finals, completed)
    });

    let first = adapter
        .submit_turn_routed(
            &handle,
            TurnInput::UserText("__late_base__".into()),
            TurnRouting::Inject,
        )
        .await
        .unwrap();
    let mut late = adapter
        .submit_turn_routed(
            &handle,
            TurnInput::UserText("__finish_before_interject__".into()),
            TurnRouting::Inject,
        )
        .await
        .expect("late interject is admitted");
    assert_eq!(first.disposition, TurnDisposition::Started);
    assert_eq!(late.disposition, TurnDisposition::Injected);
    assert_eq!(first.turn_id, late.turn_id);
    late.release_completion();

    let (started, finals, completed) = tokio::time::timeout(Duration::from_secs(5), collector)
        .await
        .expect("vendor-self-started turn completion was not surfaced")
        .expect("collector join");
    assert_eq!(started.len(), 2, "normal + vendor-self-started turns");
    assert_eq!(completed.len(), 2, "both turns must complete canonically");
    assert_ne!(completed[0], completed[1]);
    assert!(finals.iter().any(|(_, text)| text == "echo:__late_base__"));
    assert!(finals
        .iter()
        .any(|(_, text)| text == "echo:__finish_before_interject__"));
    assert!(
        finals.iter().all(|(_, text)| {
            text == "echo:__late_base__" || text == "echo:__finish_before_interject__"
        }),
        "turn answers must not be torn together: {finals:?}"
    );

    adapter.close_thread(&handle).await.unwrap();
    clear_fake();
}

/// The lower layer retains a distinct FIFO route for a future composer toggle.
/// Unlike Inject, Queue returns a new id and produces a second turn boundary.
#[tokio::test]
#[serial]
async fn explicit_queue_retains_two_fifo_turns() {
    install_fake();
    let tmp = TempDir::new().unwrap();
    let adapter = GrokAcpAdapter::new();
    let handle = adapter
        .start_thread(
            &AgentSpecBrief {
                role: String::new(),
            },
            &spawn_ctx(&tmp, "s-queue"),
        )
        .await
        .expect("start ok");

    let mut stream = adapter.events(&handle);
    let collector = tokio::spawn(async move {
        let mut finals = Vec::new();
        let mut completed = 0;
        while let Some(event) = stream.next().await {
            match event {
                ThreadEvent::ItemCompleted { item } => {
                    if let ThreadItemDetails::AgentMessage(text) = item.details {
                        finals.push(text);
                    }
                }
                ThreadEvent::TurnCompleted { .. } => {
                    completed += 1;
                    if completed == 2 {
                        break;
                    }
                }
                ThreadEvent::TurnFailed { err, .. } => panic!("turn failed: {err:?}"),
                _ => {}
            }
        }
        finals
    });

    let first = adapter
        .submit_turn_routed(
            &handle,
            TurnInput::UserText("one".into()),
            TurnRouting::Queue,
        )
        .await
        .expect("first queue submit starts immediately");
    let second = adapter
        .submit_turn_routed(
            &handle,
            TurnInput::UserText("two".into()),
            TurnRouting::Queue,
        )
        .await
        .expect("second queue submit is retained");
    assert_ne!(
        first.turn_id.0, second.turn_id.0,
        "queued follow-up gets its own turn id"
    );
    assert_eq!(first.disposition, TurnDisposition::Started);
    assert_eq!(second.disposition, TurnDisposition::Queued);

    let finals = tokio::time::timeout(Duration::from_secs(10), collector)
        .await
        .expect("collector timeout")
        .expect("collector join");
    assert_eq!(finals, vec!["echo:one", "echo:two"]);

    adapter.close_thread(&handle).await.unwrap();
    clear_fake();
}

#[tokio::test]
#[serial]
async fn load_resume_filters_is_replay() {
    install_fake();
    let tmp = TempDir::new().unwrap();
    let project = tmp.path();
    let sid = "s-resume";
    // Seed meta as if a prior session existed with known ACP sessionId.
    let meta = SessionMeta {
        sid: sid.into(),
        slug: "demo".into(),
        vendor: AgentVendor::Grok,
        protocol: SessionProtocol::Acp,
        role: String::new(),
        permission_mode: PermissionMode::Skip,
        owner: "user:test".into(),
        vendor_uuid: "019f4547-0000-7000-8000-00000000cafe".into(),
        model: None,
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
    };
    write_session_meta(project, &meta).unwrap();

    let adapter = GrokAcpAdapter::new();
    let handle = adapter
        .start_thread(
            &AgentSpecBrief {
                role: String::new(),
            },
            &spawn_ctx(&tmp, sid),
        )
        .await
        .expect("resume start");
    assert_eq!(handle.identity, "019f4547-0000-7000-8000-00000000cafe");

    // Subscribe and send a turn — final must not include REPLAY_MUST_DROP.
    let mut stream = adapter.events(&handle);
    let collector = tokio::spawn(async move {
        let mut finals = Vec::new();
        while let Some(ev) = stream.next().await {
            match ev {
                ThreadEvent::ItemCompleted { item } => {
                    if let ThreadItemDetails::AgentMessage(t) = item.details {
                        finals.push(t);
                    }
                }
                ThreadEvent::TurnCompleted { .. } => break,
                _ => {}
            }
        }
        finals
    });
    tokio::time::sleep(Duration::from_millis(80)).await;
    adapter
        .submit_turn(&handle, TurnInput::UserText("after-load".into()))
        .await
        .unwrap();
    let finals = tokio::time::timeout(Duration::from_secs(10), collector)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(finals, vec!["echo:after-load".to_string()]);
    assert!(finals.iter().all(|t| !t.contains("REPLAY")));

    adapter.close_thread(&handle).await.unwrap();
    clear_fake();
}

#[tokio::test]
#[serial]
async fn unknown_directive_rejected() {
    install_fake();
    let tmp = TempDir::new().unwrap();
    let adapter = GrokAcpAdapter::new();
    let handle = adapter
        .start_thread(
            &AgentSpecBrief {
                role: String::new(),
            },
            &spawn_ctx(&tmp, "s-dir"),
        )
        .await
        .unwrap();
    let out = adapter
        .handle_directive(
            &handle,
            ccteam_harness::Directive {
                name: "bogus".into(),
                args: String::new(),
                choice: None,
            },
        )
        .await
        .unwrap();
    match out {
        ccteam_harness::DirectiveOutcome::Rejected { reason } => {
            assert!(reason.contains("does not support") || reason.contains("bogus"));
        }
        other => panic!("expected Rejected, got {other:?}"),
    }
    adapter.close_thread(&handle).await.unwrap();
    clear_fake();
}

/// Bare `/model` → NeedsChoice picker from the REAL `availableModels` capture
/// (never a hardcoded catalog). Choice re-entry + explicit arg both call
/// `session/set_model`.
#[tokio::test]
#[serial]
async fn model_directive_lists_and_sets() {
    install_fake();
    let tmp = TempDir::new().unwrap();
    let adapter = GrokAcpAdapter::new();
    let handle = adapter
        .start_thread(
            &AgentSpecBrief {
                role: String::new(),
            },
            &spawn_ctx(&tmp, "s-model"),
        )
        .await
        .unwrap();

    // Bare `/model` → picker with both fake catalog entries (effort-expanded).
    let bare = adapter
        .handle_directive(
            &handle,
            ccteam_harness::Directive {
                name: "model".into(),
                args: String::new(),
                choice: None,
            },
        )
        .await
        .unwrap();
    let prompt = match bare {
        ccteam_harness::DirectiveOutcome::NeedsChoice(p) => p,
        other => panic!("expected NeedsChoice, got {other:?}"),
    };
    assert!(!prompt.token.is_empty());
    assert!(!prompt.multi);
    let ids: Vec<_> = prompt.options.iter().map(|o| o.id.as_str()).collect();
    assert!(
        ids.iter().any(|id| id.starts_with("grok-4.5")),
        "picker must include grok-4.5, got {ids:?}"
    );
    assert!(
        ids.contains(&"grok-composer-2.5-fast"),
        "picker must include composer, got {ids:?}"
    );
    // Effort-axis models expand to model×effort options.
    assert!(
        ids.contains(&"grok-4.5 high")
            || ids.contains(&"grok-4.5 medium")
            || ids.contains(&"grok-4.5 low"),
        "grok-4.5 should expand with efforts, got {ids:?}"
    );

    // Explicit set with effort.
    let set = adapter
        .handle_directive(
            &handle,
            ccteam_harness::Directive {
                name: "model".into(),
                args: "grok-4.5 low".into(),
                choice: None,
            },
        )
        .await
        .unwrap();
    match set {
        ccteam_harness::DirectiveOutcome::Done { receipt } => {
            assert!(
                receipt.contains("grok-4.5") && receipt.contains("low"),
                "receipt={receipt}"
            );
        }
        other => panic!("expected Done, got {other:?}"),
    }
    let status = adapter.thread_status(&handle).await.unwrap();
    assert_eq!(status.model.as_deref(), Some("grok-4.5"));
    assert_eq!(status.effort.as_deref(), Some("low"));

    // Switch to a no-effort model via choice re-entry shape.
    let set2 = adapter
        .handle_directive(
            &handle,
            ccteam_harness::Directive {
                name: "model".into(),
                args: String::new(),
                choice: Some(ccteam_harness::ChoiceSelection {
                    token: prompt.token.clone(),
                    ids: vec!["grok-composer-2.5-fast".into()],
                    free_text: None,
                }),
            },
        )
        .await
        .unwrap();
    match set2 {
        ccteam_harness::DirectiveOutcome::Done { receipt } => {
            assert!(
                receipt.contains("grok-composer-2.5-fast"),
                "receipt={receipt}"
            );
        }
        other => panic!("expected Done, got {other:?}"),
    }
    let status2 = adapter.thread_status(&handle).await.unwrap();
    assert_eq!(status2.model.as_deref(), Some("grok-composer-2.5-fast"));

    // Unknown model → Rejected (vendor error).
    let bad = adapter
        .handle_directive(
            &handle,
            ccteam_harness::Directive {
                name: "model".into(),
                args: "not-a-real-model".into(),
                choice: None,
            },
        )
        .await
        .unwrap();
    match bad {
        ccteam_harness::DirectiveOutcome::Rejected { reason } => {
            assert!(
                reason.contains("切换失败") || reason.contains("unknown"),
                "reason={reason}"
            );
        }
        other => panic!("expected Rejected, got {other:?}"),
    }

    adapter.close_thread(&handle).await.unwrap();
    clear_fake();
}

/// v0.9.0 W1 (G2) — grok wires the ccteam `mcpServers` on BOTH session/new and
/// session/load (was hardcoded `[]`, dropping `ctx.secret`, so grok children
/// had no ccteam tool face). With a non-empty secret the adapter builds a
/// non-empty `mcpServers`; the fake records the count per session/* method →
/// assert both the fresh (session/new) and cold-resume (session/load) paths.
#[tokio::test]
#[serial]
async fn session_new_and_load_carry_mcp_servers() {
    install_fake();
    let tmp = TempDir::new().unwrap();
    let dump = tmp.path().join("acp_mcp_dump.tsv");
    let prior_dump = std::env::var_os("CCTEAM_ACP_MCP_DUMP");
    unsafe {
        std::env::set_var("CCTEAM_ACP_MCP_DUMP", &dump);
    }

    let ctx_with_secret = |sid: &str| SpawnCtx {
        slug: "demo".into(),
        sid: sid.into(),
        cwd: tmp.path().to_path_buf(),
        project_dir: tmp.path().to_path_buf(),
        extra_args: vec![],
        model_id: None,
        effort: None,
        permission_mode: PermissionMode::Skip,
        secret: "seKret1234".into(),
        remote: None,
    };

    // Phase A — fresh session/new (no prior meta).
    let adapter = GrokAcpAdapter::new();
    let fresh = adapter
        .start_thread(
            &AgentSpecBrief {
                role: String::new(),
            },
            &ctx_with_secret("s-new-mcp"),
        )
        .await
        .expect("fresh start");
    adapter.close_thread(&fresh).await.ok();

    // Phase B — cold resume via session/load (meta carries the vendor_uuid).
    let sid_load = "s-load-mcp";
    let meta = SessionMeta {
        sid: sid_load.into(),
        slug: "demo".into(),
        vendor: AgentVendor::Grok,
        protocol: SessionProtocol::Acp,
        role: String::new(),
        permission_mode: PermissionMode::Skip,
        owner: "user:test".into(),
        vendor_uuid: "019f4547-0000-7000-8000-00000000cafe".into(),
        model: None,
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
    };
    write_session_meta(tmp.path(), &meta).unwrap();
    let loaded = adapter
        .start_thread(
            &AgentSpecBrief {
                role: String::new(),
            },
            &ctx_with_secret(sid_load),
        )
        .await
        .expect("load start");
    adapter.close_thread(&loaded).await.ok();

    let recorded = std::fs::read_to_string(&dump).unwrap_or_default();
    let count_for = |method: &str| -> Option<usize> {
        recorded
            .lines()
            .find(|l| l.starts_with(&format!("{method}\t")))
            .and_then(|l| l.split('\t').nth(1))
            .and_then(|n| n.parse().ok())
    };
    assert!(
        count_for("session/new").is_some_and(|n| n >= 1),
        "session/new must carry a non-empty mcpServers, got: {recorded:?}"
    );
    assert!(
        count_for("session/load").is_some_and(|n| n >= 1),
        "session/load must carry a non-empty mcpServers (G2 fix), got: {recorded:?}"
    );

    match prior_dump {
        Some(v) => unsafe { std::env::set_var("CCTEAM_ACP_MCP_DUMP", v) },
        None => unsafe { std::env::remove_var("CCTEAM_ACP_MCP_DUMP") },
    }
    clear_fake();
}

/// v0.9.0 W3 (F3) — remote execution is claude-only this version; grok must
/// fail clean + readable rather than silently spawning locally under a
/// remote host id. No fake binary needed — the guard runs before spawn.
#[tokio::test(flavor = "current_thread")]
async fn start_thread_rejects_remote_ctx_readable() {
    let tmp = TempDir::new().unwrap();
    let mut ctx = spawn_ctx(&tmp, "s-remote");
    ctx.remote = Some(ccteam_harness::RemoteExecTarget {
        host_id: "sat".into(),
        wire_slug: "demo".into(),
        hub: std::sync::Arc::new(ccteam_harness::HostChannelHub::default()),
    });
    let adapter = GrokAcpAdapter::new();
    let err = adapter
        .start_thread(
            &AgentSpecBrief {
                role: String::new(),
            },
            &ctx,
        )
        .await
        .expect_err("remote grok must be rejected");
    let msg = err.to_string();
    assert!(msg.contains("not yet supported for grok"), "got: {msg}");
}

/// stdio-leak fix — managed grok spawns must disable Grok's Claude MCP compat
/// scan (`GROK_CLAUDE_MCPS_ENABLED=false` on the child env, and only there):
/// with the scan on (vendor default), grok imports ccteam's global stdio
/// registration from `~/.claude.json` on top of the ACP-injected HTTP server —
/// one orphan `ccteam internal mcp-serve` child per session plus a same-name
/// double registration whose winner (admin stdio vs session-principal HTTP)
/// depends on the grok version. The fake dumps the toggle it inherited to
/// $CCTEAM_ACP_ENV_DUMP; asserting on it proves the env reaches the child.
#[tokio::test]
#[serial]
async fn spawn_disables_claude_mcp_compat_scan() {
    install_fake();
    let tmp = TempDir::new().unwrap();
    let dump = tmp.path().join("env-dump.txt");
    let prev = std::env::var("CCTEAM_ACP_ENV_DUMP").ok();
    unsafe {
        std::env::set_var("CCTEAM_ACP_ENV_DUMP", &dump);
    }

    let adapter = GrokAcpAdapter::new();
    let handle = tokio::time::timeout(
        Duration::from_secs(10),
        adapter.start_thread(
            &AgentSpecBrief {
                role: String::new(),
            },
            &spawn_ctx(&tmp, "s1"),
        ),
    )
    .await
    .expect("start timeout")
    .expect("start ok");

    let recorded = std::fs::read_to_string(&dump).expect("fake wrote env dump");
    assert_eq!(
        recorded.trim(),
        "GROK_CLAUDE_MCPS_ENABLED=false",
        "managed grok child must see the Claude MCP compat scan disabled"
    );

    adapter.close_thread(&handle).await.unwrap();
    match prev {
        Some(v) => unsafe { std::env::set_var("CCTEAM_ACP_ENV_DUMP", v) },
        None => unsafe { std::env::remove_var("CCTEAM_ACP_ENV_DUMP") },
    }
    clear_fake();
}
