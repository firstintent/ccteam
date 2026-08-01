//! Kimi Code ACP adapter tests against the hermetic fake
//! (`fixtures/kimi_acp/fake_kimi_acp.py`).
//!
//! Gate: no real kimi / network. Set `CCTEAM_KIMI_BIN` to the fake.
//! Wire pin: kimi 0.26.0 (`references/kimi-code` is protocol reference only).

use std::path::PathBuf;
use std::time::Duration;

use ccteam_harness::execution::kimi_acp::spawn_spec::{build_argv, KimiSpawnInput};
use ccteam_harness::{
    write_session_meta, AgentSpecBrief, AgentVendor, ExecutionMode, HarnessAdapter, KimiAcpAdapter,
    PermissionMode, SessionMeta, SessionOrigin, SessionProtocol, SpawnCtx, ThreadEvent,
    ThreadItemDetails, TurnInput, KIMI_BIN_ENV,
};
use futures::StreamExt;
use serial_test::serial;
use tempfile::TempDir;

fn fake_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/kimi_acp/fake_kimi_acp.py")
}

fn install_fake() {
    let bin = fake_bin();
    assert!(bin.is_file(), "missing fake at {}", bin.display());
    let wrapper = std::env::temp_dir().join(format!(
        "ccteam-fake-kimi-{}-{}",
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
    unsafe {
        std::env::set_var(KIMI_BIN_ENV, &wrapper);
    }
}

fn clear_fake() {
    unsafe {
        std::env::remove_var(KIMI_BIN_ENV);
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
fn spawn_spec_is_acp_only_no_pty_flags() {
    let argv = build_argv("kimi", &KimiSpawnInput::default());
    assert_eq!(argv, vec!["kimi", "acp"]);
    assert!(!argv
        .iter()
        .any(|a| a.contains("tmux") || a.contains("rmux")));
}

#[tokio::test]
#[serial]
async fn handshake_prompt_final_only_on_fake() {
    install_fake();
    let tmp = TempDir::new().unwrap();
    let adapter = KimiAcpAdapter::new();
    assert_eq!(adapter.name(), "kimi-acp");
    assert_eq!(adapter.vendor(), AgentVendor::Kimi);
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

    assert_eq!(handle.vendor, AgentVendor::Kimi);
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
    assert_eq!(
        handle.raw_extras.get("adapter").and_then(|v| v.as_str()),
        Some("kimi-acp")
    );

    let mut stream = adapter.events(&handle);
    let collector = tokio::spawn(async move {
        let mut finals = Vec::new();
        let mut usage_in = None;
        let mut reported = None;
        while let Some(ev) = stream.next().await {
            match ev {
                ThreadEvent::ItemCompleted { item } => {
                    if let ThreadItemDetails::AgentMessage(t) = item.details {
                        finals.push(t);
                    }
                }
                ThreadEvent::TurnCompleted { usage, .. } => {
                    usage_in = Some(usage.input_tokens);
                    reported = usage.reported_cost_usd;
                    break;
                }
                ThreadEvent::TurnFailed { err, .. } => {
                    panic!("turn failed: {err:?}");
                }
                _ => {}
            }
        }
        (finals, usage_in, reported)
    });

    tokio::time::sleep(Duration::from_millis(50)).await;
    adapter
        .submit_turn(&handle, TurnInput::UserText("hello".into()))
        .await
        .expect("submit");

    let (finals, usage_in, reported) = tokio::time::timeout(Duration::from_secs(10), collector)
        .await
        .expect("collector timeout")
        .expect("collector join");

    assert_eq!(finals.len(), 1, "exactly one final AgentMessage");
    assert_eq!(finals[0], "echo:hello");
    assert!(!finals[0].contains("thinking"), "thoughts not in final");
    assert!(!finals[0].contains("REPLAY"), "no replay text");
    // Kimi's ACP wire carries no usage → zeros, and no reported cost — the
    // pricing layer maps kimi to None ("—"), never another vendor's table.
    assert_eq!(usage_in, Some(0));
    assert!(reported.is_none(), "kimi must never report a cost");

    let status = adapter.thread_status(&handle).await.unwrap();
    assert_eq!(status.model.as_deref(), Some("kimi-k2-0905-preview"));

    adapter.close_thread(&handle).await.unwrap();
    clear_fake();
}

/// An ACP `session/prompt` result is a *successful* JSON-RPC response even when
/// the turn produced no answer, so ignoring `stopReason` reports every vendor
/// outcome as a clean reply — the partial text lands in `turns.jsonl` as the
/// final answer and a delegation parent is told the task completed. Any
/// non-clean reason must reach the event stream as `TurnFailed`, still carrying
/// whatever partial output the vendor produced.
#[tokio::test]
#[serial]
async fn abnormal_stop_reason_surfaces_as_turn_failed_with_partial_text() {
    install_fake();
    let tmp = TempDir::new().unwrap();
    let adapter = KimiAcpAdapter::new();
    let handle = tokio::time::timeout(
        Duration::from_secs(10),
        adapter.start_thread(
            &AgentSpecBrief {
                role: String::new(),
            },
            &spawn_ctx(&tmp, "s-stop"),
        ),
    )
    .await
    .expect("start timeout")
    .expect("start ok");

    let mut stream = adapter.events(&handle);
    let collector = tokio::spawn(async move {
        let mut partial = Vec::new();
        while let Some(ev) = stream.next().await {
            match ev {
                ThreadEvent::ItemCompleted { item } => {
                    if let ThreadItemDetails::AgentMessage(t) = item.details {
                        partial.push(t);
                    }
                }
                ThreadEvent::TurnFailed { err, .. } => return (partial, Some(err)),
                ThreadEvent::TurnCompleted { .. } => return (partial, None),
                _ => {}
            }
        }
        (partial, None)
    });

    tokio::time::sleep(Duration::from_millis(50)).await;
    adapter
        .submit_turn(&handle, TurnInput::UserText("STOP:refusal go".into()))
        .await
        .expect("submit");

    let (partial, failure) = tokio::time::timeout(Duration::from_secs(10), collector)
        .await
        .expect("collector timeout")
        .expect("collector join");

    let err = failure.expect("a refused turn must not report completion");
    assert_eq!(err.kind, "stop_reason:refusal");
    assert!(
        err.message.contains("refusal"),
        "the failure must name the wire reason: {}",
        err.message
    );
    assert_eq!(
        partial.len(),
        1,
        "output the vendor did produce is still delivered"
    );
    assert!(partial[0].contains("STOP:refusal"));

    adapter.close_thread(&handle).await.unwrap();
    clear_fake();
}

/// The application requests Inject by default, but Kimi's public ACP adapter
/// does not expose its internal `turn.steer`. The adapter must degrade without
/// loss to two FIFO turns rather than cancel or reject the second message.
#[tokio::test]
#[serial]
async fn mid_turn_inject_degrades_to_fifo_without_native_extension() {
    install_fake();
    let tmp = TempDir::new().unwrap();
    let adapter = KimiAcpAdapter::new();
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
        let mut completed = 0;
        while let Some(ev) = stream.next().await {
            match ev {
                ThreadEvent::ItemCompleted { item } => {
                    if let ThreadItemDetails::AgentMessage(t) = item.details {
                        finals.push(t);
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
        .submit_turn(&handle, TurnInput::UserText("one".into()))
        .await
        .expect("first submit ok");
    let second = adapter
        .submit_turn(&handle, TurnInput::UserText("two".into()))
        .await
        .expect("unsupported inject must degrade to queue, not reject");
    assert_ne!(first.0, second.0, "queued turn gets its own id");

    let finals = tokio::time::timeout(Duration::from_secs(10), collector)
        .await
        .expect("collector timeout")
        .expect("collector join");

    assert_eq!(finals.len(), 2, "both queued turns produce an answer");
    assert_eq!(finals[0], "echo:one", "first turn runs first (FIFO)");
    assert_eq!(finals[1], "echo:two", "queued turn drains after the first");

    adapter.close_thread(&handle).await.unwrap();
    clear_fake();
}

#[tokio::test]
#[serial]
async fn resume_prefers_session_resume_no_replay() {
    install_fake();
    let tmp = TempDir::new().unwrap();
    let project = tmp.path();
    let sid = "s-resume";
    let meta = SessionMeta {
        sid: sid.into(),
        slug: "demo".into(),
        vendor: AgentVendor::Kimi,
        protocol: SessionProtocol::Acp,
        role: String::new(),
        permission_mode: PermissionMode::Skip,
        owner: "user:test".into(),
        vendor_uuid: "01JYQX7A9D2E3F4G5H6J7K8M9N".into(),
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

    let adapter = KimiAcpAdapter::new();
    let handle = adapter
        .start_thread(
            &AgentSpecBrief {
                role: String::new(),
            },
            &spawn_ctx(&tmp, sid),
        )
        .await
        .expect("resume start");
    assert_eq!(handle.identity, "01JYQX7A9D2E3F4G5H6J7K8M9N");

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
        .submit_turn(&handle, TurnInput::UserText("after-resume".into()))
        .await
        .unwrap();
    let finals = tokio::time::timeout(Duration::from_secs(10), collector)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(finals, vec!["echo:after-resume".to_string()]);
    assert!(finals.iter().all(|t| !t.contains("REPLAY")));

    adapter.close_thread(&handle).await.unwrap();
    clear_fake();
}

/// The resume path MUST carry the ccteam `mcpServers` (a hardcoded `[]` on
/// resume would drop the in-agent tool face after any cold-resume / daemon
/// rebuild). With a non-empty secret the adapter builds a non-empty
/// `mcpServers`; the fake records the count per session/* method.
#[tokio::test]
#[serial]
async fn resume_carries_mcp_servers() {
    install_fake();
    let tmp = TempDir::new().unwrap();
    let project = tmp.path();
    let sid = "s-resume-mcp";
    let dump = tmp.path().join("acp_mcp_dump.tsv");
    let prior_dump = std::env::var_os("CCTEAM_ACP_MCP_DUMP");
    unsafe {
        std::env::set_var("CCTEAM_ACP_MCP_DUMP", &dump);
    }

    let meta = SessionMeta {
        sid: sid.into(),
        slug: "demo".into(),
        vendor: AgentVendor::Kimi,
        protocol: SessionProtocol::Acp,
        role: String::new(),
        permission_mode: PermissionMode::Skip,
        owner: "user:test".into(),
        vendor_uuid: "01JYQX7A9D2E3F4G5H6J7K8M9N".into(),
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

    // A SpawnCtx WITH a secret → `acp_mcp_servers_http` yields a non-empty array.
    let ctx = SpawnCtx {
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
    let adapter = KimiAcpAdapter::new();
    let handle = adapter
        .start_thread(
            &AgentSpecBrief {
                role: String::new(),
            },
            &ctx,
        )
        .await
        .expect("resume start");
    assert_eq!(handle.identity, "01JYQX7A9D2E3F4G5H6J7K8M9N");

    let recorded = std::fs::read_to_string(&dump).unwrap_or_default();
    let resume_line = recorded
        .lines()
        .find(|l| l.starts_with("session/resume\t"))
        .unwrap_or_else(|| panic!("fake must record a session/resume, got: {recorded:?}"));
    let count: usize = resume_line.split('\t').nth(1).unwrap().parse().unwrap();
    assert!(
        count >= 1,
        "session/resume must carry a non-empty mcpServers, got: {recorded:?}"
    );

    match prior_dump {
        Some(v) => unsafe { std::env::set_var("CCTEAM_ACP_MCP_DUMP", v) },
        None => unsafe { std::env::remove_var("CCTEAM_ACP_MCP_DUMP") },
    }
    clear_fake();
}

#[tokio::test]
#[serial]
async fn skip_auto_allows_permission_request() {
    // The fake emits session/request_permission on first prompt.
    // With InboundPolicy::AutoAllowPermission the transport must reply
    // allow — verified by turn completing (not stalling/rejecting).
    install_fake();
    let tmp = TempDir::new().unwrap();
    let adapter = KimiAcpAdapter::new();
    let handle = adapter
        .start_thread(
            &AgentSpecBrief {
                role: String::new(),
            },
            &spawn_ctx(&tmp, "s-perm"),
        )
        .await
        .unwrap();
    let mut stream = adapter.events(&handle);
    let collector = tokio::spawn(async move {
        while let Some(ev) = stream.next().await {
            if matches!(ev, ThreadEvent::TurnCompleted { .. }) {
                return true;
            }
            if matches!(ev, ThreadEvent::TurnFailed { .. }) {
                return false;
            }
        }
        false
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    adapter
        .submit_turn(&handle, TurnInput::UserText("need-tool".into()))
        .await
        .unwrap();
    let ok = tokio::time::timeout(Duration::from_secs(10), collector)
        .await
        .expect("timeout")
        .expect("join");
    assert!(
        ok,
        "skip session must complete turn (auto-allow permission)"
    );
    adapter.close_thread(&handle).await.unwrap();
    clear_fake();
}

/// A HITL kimi session must survive a `session/request_permission` round:
/// the transport FAIL-CLOSED declines it (never a silent auto-allow =
/// approval bypass), the rejected tool call does not kill the turn, and
/// nothing panics.
#[tokio::test]
#[serial]
async fn hitl_declines_permission_without_panic_or_kill() {
    install_fake();
    let tmp = TempDir::new().unwrap();
    let adapter = KimiAcpAdapter::new();
    let mut ctx = spawn_ctx(&tmp, "s-hitl");
    ctx.permission_mode = PermissionMode::Hitl;
    let handle = adapter
        .start_thread(
            &AgentSpecBrief {
                role: String::new(),
            },
            &ctx,
        )
        .await
        .expect("hitl start must not fail");
    let mut stream = adapter.events(&handle);
    let collector = tokio::spawn(async move {
        while let Some(ev) = stream.next().await {
            if matches!(ev, ThreadEvent::TurnCompleted { .. }) {
                return true;
            }
            if matches!(ev, ThreadEvent::TurnFailed { .. }) {
                return false;
            }
        }
        false
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    adapter
        .submit_turn(&handle, TurnInput::UserText("need-tool".into()))
        .await
        .expect("submit must not panic");
    let ok = tokio::time::timeout(Duration::from_secs(10), collector)
        .await
        .expect("hitl turn must not hang on a pending permission")
        .expect("join");
    assert!(ok, "declined tool must not kill the turn");
    adapter.close_thread(&handle).await.unwrap();
    clear_fake();
}

/// Bare `/model` → NeedsChoice from vendor `configOptions[id=model].options`
/// (never a ccteam-hardcoded catalog). Choice re-entry + explicit arg both
/// call `session/set_model` (`{sessionId, modelId}`).
#[tokio::test]
#[serial]
async fn model_directive_lists_and_sets() {
    install_fake();
    let tmp = TempDir::new().unwrap();
    let adapter = KimiAcpAdapter::new();
    let handle = adapter
        .start_thread(
            &AgentSpecBrief {
                role: String::new(),
            },
            &spawn_ctx(&tmp, "s-model"),
        )
        .await
        .unwrap();

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
    // Rows are model×effort while the vendor declares a thought_level axis
    // (kimi 0.31.1 does) — the id is exactly the `/model <id> [effort]` form.
    let ids: Vec<_> = prompt.options.iter().map(|o| o.id.as_str()).collect();
    assert!(
        ids.iter().any(|id| id.starts_with("kimi-k2-0905-preview")),
        "picker must include kimi-k2-0905-preview from vendor options, got {ids:?}"
    );
    assert!(
        ids.iter().any(|id| id.starts_with("kimi-k2-thinking")),
        "picker must include kimi-k2-thinking from vendor options, got {ids:?}"
    );

    let set = adapter
        .handle_directive(
            &handle,
            ccteam_harness::Directive {
                name: "model".into(),
                args: "kimi-k2-thinking".into(),
                choice: None,
            },
        )
        .await
        .unwrap();
    match set {
        ccteam_harness::DirectiveOutcome::Done { receipt } => {
            assert!(receipt.contains("kimi-k2-thinking"), "receipt={receipt}");
        }
        other => panic!("expected Done, got {other:?}"),
    }
    let status = adapter.thread_status(&handle).await.unwrap();
    assert_eq!(status.model.as_deref(), Some("kimi-k2-thinking"));

    // Choice re-entry.
    let set2 = adapter
        .handle_directive(
            &handle,
            ccteam_harness::Directive {
                name: "model".into(),
                args: String::new(),
                choice: Some(ccteam_harness::ChoiceSelection {
                    token: prompt.token.clone(),
                    ids: vec!["kimi-k2-0905-preview".into()],
                    free_text: None,
                }),
            },
        )
        .await
        .unwrap();
    match set2 {
        ccteam_harness::DirectiveOutcome::Done { receipt } => {
            assert!(
                receipt.contains("kimi-k2-0905-preview"),
                "receipt={receipt}"
            );
        }
        other => panic!("expected Done, got {other:?}"),
    }
    let status2 = adapter.thread_status(&handle).await.unwrap();
    assert_eq!(status2.model.as_deref(), Some("kimi-k2-0905-preview"));

    // An unknown model id is rejected by the vendor (honest error, no panic).
    let bad = adapter
        .handle_directive(
            &handle,
            ccteam_harness::Directive {
                name: "model".into(),
                args: "made-up-model".into(),
                choice: None,
            },
        )
        .await
        .unwrap();
    match bad {
        ccteam_harness::DirectiveOutcome::Rejected { reason } => {
            assert!(reason.contains("切换失败"), "reason={reason}");
        }
        other => panic!("expected Rejected for unknown model, got {other:?}"),
    }

    adapter.close_thread(&handle).await.unwrap();
    clear_fake();
}

/// Kimi's effort ladder is the ACP `thought_level` config option, id
/// `thinking` — NOT opencode's `effort` id. Reading only the latter is why a
/// kimi session that was plainly running at `high` reported no effort at all
/// (verified on kimi 0.31.1: `session/new` answers
/// `{"id":"thinking","category":"thought_level","currentValue":"high",
/// "options":[low,high,max]}`).
///
/// The whole round trip: the handshake value reaches the statusline, the
/// picker offers the vendor's own model×effort rows, `/model <id> <effort>`
/// sets it through `session/set_config_option`, and the vendor's
/// `config_option_update` — the frame ccteam used to drop — keeps the
/// statusline honest afterwards.
#[tokio::test]
#[serial]
async fn effort_reads_and_writes_kimis_thought_level_option() {
    install_fake();
    let tmp = TempDir::new().unwrap();
    let adapter = KimiAcpAdapter::new();
    let handle = adapter
        .start_thread(
            &AgentSpecBrief {
                role: String::new(),
            },
            &spawn_ctx(&tmp, "s-effort"),
        )
        .await
        .unwrap();

    // 1. Handshake: the current level rides the statusline.
    let status = adapter.thread_status(&handle).await.unwrap();
    assert_eq!(
        status.effort.as_deref(),
        Some("high"),
        "handshake thought_level must reach the statusline"
    );

    // 2. Picker: the vendor's own levels, never a ccteam-invented ladder.
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
    let ids: Vec<_> = prompt.options.iter().map(|o| o.id.as_str()).collect();
    assert!(
        ids.contains(&"kimi-k2-thinking max"),
        "picker must expand model×effort from the vendor levels, got {ids:?}"
    );

    // 3. Write: `/model <id> <effort>` lands the level and says so.
    let set = adapter
        .handle_directive(
            &handle,
            ccteam_harness::Directive {
                name: "model".into(),
                args: "kimi-k2-thinking max".into(),
                choice: None,
            },
        )
        .await
        .unwrap();
    match set {
        ccteam_harness::DirectiveOutcome::Done { receipt } => {
            assert!(receipt.contains("effort → max"), "receipt={receipt}");
        }
        other => panic!("expected Done, got {other:?}"),
    }
    let after = adapter.thread_status(&handle).await.unwrap();
    assert_eq!(after.model.as_deref(), Some("kimi-k2-thinking"));
    assert_eq!(after.effort.as_deref(), Some("max"));

    // 4. A level the vendor doesn't declare is rejected by the vendor, and the
    //    receipt says the model moved but the effort did not (never silent).
    let bad = adapter
        .handle_directive(
            &handle,
            ccteam_harness::Directive {
                name: "model".into(),
                args: "kimi-k2-0905-preview low".into(),
                choice: None,
            },
        )
        .await
        .unwrap();
    match bad {
        ccteam_harness::DirectiveOutcome::Done { receipt } => {
            assert!(receipt.contains("effort → low"), "receipt={receipt}");
        }
        other => panic!("expected Done, got {other:?}"),
    }
    assert_eq!(
        adapter
            .thread_status(&handle)
            .await
            .unwrap()
            .effort
            .as_deref(),
        Some("low")
    );

    adapter.close_thread(&handle).await.unwrap();
    clear_fake();
}

/// Kimi pushes no context usage at all — no `usage_update`, and a
/// `session/prompt` response that is literally `{"stopReason":"end_turn"}`.
/// What it DOES do is advertise `status` in `available_commands_update`, and
/// answer that command locally with a real occupancy report.
///
/// So the runner pulls after each turn: before any turn there is nothing to
/// report (and the statusline says so rather than showing a zero); after one,
/// `/status` carries a real percentage sourced from the vendor itself.
#[tokio::test]
#[serial]
async fn context_is_pulled_from_the_status_command_kimi_advertises() {
    install_fake();
    let tmp = TempDir::new().unwrap();
    let adapter = KimiAcpAdapter::new();
    let handle = adapter
        .start_thread(
            &AgentSpecBrief {
                role: String::new(),
            },
            &spawn_ctx(&tmp, "s-ctx"),
        )
        .await
        .expect("start ok");

    // Nothing has run: no occupancy to claim, and we do not invent one.
    let before = adapter.thread_status(&handle).await.unwrap();
    assert!(
        before.context.is_none_or(|c| c.used_tokens.is_none()),
        "occupancy before the first turn must be unknown, not zero: {before:?}"
    );

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
    tokio::time::sleep(Duration::from_millis(50)).await;
    adapter
        .submit_turn(&handle, TurnInput::UserText("hello".into()))
        .await
        .expect("submit");
    let finals = tokio::time::timeout(Duration::from_secs(10), collector)
        .await
        .expect("collector timeout")
        .expect("collector join");

    // The probe rides its own turn slot and publishes nothing: the user sees
    // exactly their own answer, and no `/status` text reaches the transcript.
    assert_eq!(finals, vec!["echo:hello".to_string()]);

    // The probe runs after the turn boundary; give the runner a moment.
    let mut ctx = None;
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let status = adapter.thread_status(&handle).await.unwrap();
        if let Some(c) = status.context.filter(|c| c.used_tokens.is_some()) {
            ctx = Some(c);
            break;
        }
    }
    let ctx = ctx.expect("occupancy pulled from the vendor's status command");
    assert_eq!(ctx.used_tokens, Some(12_345));
    assert_eq!(ctx.window_tokens, 1_048_576);
    assert_eq!(
        ctx.source,
        ccteam_harness::ContextSource::Probed,
        "a pulled number must not pass for a reported one"
    );

    // `/status` now renders the real reading — no vendor-specific apology.
    let outcome = adapter
        .handle_directive(
            &handle,
            ccteam_harness::Directive {
                name: "context".into(),
                args: String::new(),
                choice: None,
            },
        )
        .await
        .unwrap();
    match outcome {
        ccteam_harness::DirectiveOutcome::Done { receipt } => {
            assert!(
                receipt.contains("ctx 12.3k / 1.0M (1%)"),
                "receipt={receipt}"
            );
            assert!(!receipt.contains("不可用"), "receipt={receipt}");
        }
        other => panic!("expected Done, got {other:?}"),
    }

    adapter.close_thread(&handle).await.unwrap();
    clear_fake();
}

/// An explicit spawn-time model choice rides `session/set_model`
/// post-handshake (the fake acks it) and is reflected in `thread_status`. A
/// missing choice keeps kimi's own default (covered by
/// `handshake_prompt_final_only_on_fake`).
#[tokio::test]
#[serial]
async fn spawn_time_model_set_via_set_model() {
    install_fake();
    let tmp = TempDir::new().unwrap();
    let adapter = KimiAcpAdapter::new();
    let mut ctx = spawn_ctx(&tmp, "s-tune");
    ctx.model_id = Some("kimi-k2-thinking".into());
    let handle = adapter
        .start_thread(
            &AgentSpecBrief {
                role: String::new(),
            },
            &ctx,
        )
        .await
        .expect("start ok");
    let status = adapter.thread_status(&handle).await.unwrap();
    assert_eq!(status.model.as_deref(), Some("kimi-k2-thinking"));
    adapter.close_thread(&handle).await.unwrap();
    clear_fake();
}

#[test]
fn no_tmux_rmux_imports_in_kimi_module_sources() {
    // Structural red-line: kimi path must not import tmux/rmux crates/modules.
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/execution/kimi_acp");
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

/// Remote execution is claude-only (red line); kimi must fail clean +
/// readable rather than silently spawning locally under a remote host id.
/// No fake binary needed — the guard runs before spawn.
#[tokio::test(flavor = "current_thread")]
async fn start_thread_rejects_remote_ctx_readable() {
    let tmp = TempDir::new().unwrap();
    let mut ctx = spawn_ctx(&tmp, "s-remote");
    ctx.remote = Some(ccteam_harness::RemoteExecTarget {
        host_id: "sat".into(),
        wire_slug: "demo".into(),
        hub: std::sync::Arc::new(ccteam_harness::HostChannelHub::default()),
    });
    let adapter = KimiAcpAdapter::new();
    let err = adapter
        .start_thread(
            &AgentSpecBrief {
                role: String::new(),
            },
            &ctx,
        )
        .await
        .expect_err("remote kimi must be rejected");
    let msg = err.to_string();
    assert!(msg.contains("not yet supported for kimi"), "got: {msg}");
}
