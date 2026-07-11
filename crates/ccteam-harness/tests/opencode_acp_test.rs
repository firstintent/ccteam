//! OpenCode ACP adapter tests against the hermetic fake
//! (`fixtures/opencode_acp/fake_opencode_acp.py`).
//!
//! Gate: no real opencode / network. Set `CCTEAM_OPENCODE_BIN` to the fake.
//! Wire pin: OpenCode release 1.17.17 (W0).

use std::path::PathBuf;
use std::time::Duration;

use ccteam_harness::execution::opencode_acp::spawn_spec::{build_argv, OpencodeSpawnInput};
use ccteam_harness::{
    write_session_meta, AgentSpecBrief, AgentVendor, ExecutionMode, HarnessAdapter,
    OpencodeAcpAdapter, PermissionMode, SessionMeta, SessionOrigin, SessionProtocol, SpawnCtx,
    ThreadEvent, ThreadItemDetails, TurnInput, OPENCODE_BIN_ENV,
};
use futures::StreamExt;
use serial_test::serial;
use tempfile::TempDir;

fn fake_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/opencode_acp/fake_opencode_acp.py")
}

fn install_fake() {
    let bin = fake_bin();
    assert!(bin.is_file(), "missing fake at {}", bin.display());
    let wrapper = std::env::temp_dir().join(format!(
        "ccteam-fake-opencode-{}-{}",
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
        std::env::set_var(OPENCODE_BIN_ENV, &wrapper);
    }
}

fn clear_fake() {
    unsafe {
        std::env::remove_var(OPENCODE_BIN_ENV);
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
    }
}

#[test]
fn spawn_spec_is_acp_only_no_pty_flags() {
    let argv = build_argv("opencode", &OpencodeSpawnInput::default());
    assert_eq!(argv, vec!["opencode", "acp"]);
    assert!(!argv
        .iter()
        .any(|a| a.contains("tmux") || a.contains("rmux")));
}

#[tokio::test]
#[serial]
async fn handshake_prompt_final_only_on_fake() {
    install_fake();
    let tmp = TempDir::new().unwrap();
    let adapter = OpencodeAcpAdapter::new();
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

    assert_eq!(handle.vendor, AgentVendor::Opencode);
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
    assert_eq!(usage_in, Some(100));
    // cost amount 0 on pin → reported None ("—"), never Claude table.
    assert!(reported.is_none(), "zero cost must not report 0.0");

    let status = adapter.thread_status(&handle).await.unwrap();
    assert_eq!(status.model.as_deref(), Some("tokenopen/gpt-5.5"));
    // usage_update size → window
    if let Some(ctx) = status.context {
        assert_eq!(ctx.window_tokens, 128000);
    }

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
        vendor: AgentVendor::Opencode,
        protocol: SessionProtocol::Acp,
        role: String::new(),
        permission_mode: PermissionMode::Skip,
        owner: "user:test".into(),
        vendor_uuid: "ses_fake_opencode_0017cafe".into(),
        host: "local".into(),
        created_at: chrono::Utc::now().to_rfc3339(),
        last_active: chrono::Utc::now().to_rfc3339(),
        origin: SessionOrigin::Ccteam,
        title: None,
        title_source: None,
        turn_count: 1,
        cost_usd: None,
        role_sha: None,
        skills_sha: None,
        trigger: None,
        compare_group: None,
    };
    write_session_meta(project, &meta).unwrap();

    let adapter = OpencodeAcpAdapter::new();
    let handle = adapter
        .start_thread(
            &AgentSpecBrief {
                role: String::new(),
            },
            &spawn_ctx(&tmp, sid),
        )
        .await
        .expect("resume start");
    assert_eq!(handle.identity, "ses_fake_opencode_0017cafe");

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

#[tokio::test]
#[serial]
async fn skip_auto_allows_permission_request() {
    // The fake emits session/request_permission on first prompt.
    // With InboundPolicy::AutoAllowPermission the transport must reply
    // allow — verified by turn completing (not stalling/rejecting).
    install_fake();
    let tmp = TempDir::new().unwrap();
    let adapter = OpencodeAcpAdapter::new();
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

/// v0.8.24 gap-fix (PRD A7) — a HITL opencode session must survive a
/// `session/request_permission` round: the transport now FAIL-CLOSED
/// declines it (never the old silent auto-allow = approval bypass), the
/// rejected tool call does not kill the turn, and nothing panics.
#[tokio::test]
#[serial]
async fn hitl_declines_permission_without_panic_or_kill() {
    install_fake();
    let tmp = TempDir::new().unwrap();
    let adapter = OpencodeAcpAdapter::new();
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

/// v0.8.24 A-U3 — an explicit spawn-time model/effort choice rides
/// `session/set_config_option` (the fake acks it) and is reflected in
/// `thread_status`. A missing choice keeps opencode's self-selected default
/// (covered by `handshake_prompt_final_only_on_fake`).
#[tokio::test]
#[serial]
async fn spawn_time_model_effort_set_via_config_option() {
    install_fake();
    let tmp = TempDir::new().unwrap();
    let adapter = OpencodeAcpAdapter::new();
    let mut ctx = spawn_ctx(&tmp, "s-tune");
    ctx.model_id = Some("tokenopen/gpt-5.5-mini".into());
    ctx.effort = Some("high".into());
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
    assert_eq!(status.model.as_deref(), Some("tokenopen/gpt-5.5-mini"));
    assert_eq!(status.effort.as_deref(), Some("high"));
    adapter.close_thread(&handle).await.unwrap();
    clear_fake();
}

#[test]
fn no_tmux_rmux_imports_in_opencode_module_sources() {
    // Structural red-line: opencode path must not import tmux/rmux crates/modules.
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/execution/opencode_acp");
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
