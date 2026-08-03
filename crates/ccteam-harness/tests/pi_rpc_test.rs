use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ccteam_harness::{
    AgentSpecBrief, AgentVendor, ContextSource, Directive, DirectiveOutcome, HarnessAdapter,
    PermissionMode, PiRoleDocument, PiRpcAdapter, SpawnCtx, ThreadEvent, ThreadHandle,
    ThreadItemDetails, TurnDisposition, TurnInput, TurnRouting,
};
use futures::StreamExt;
use serial_test::serial;

fn role_reader() -> ccteam_harness::PiRoleReader {
    Arc::new(|project_dir: &Path, role: &str| {
        ccteam_core::read_role(project_dir, role)
            .map(|detail| {
                detail.map(|detail| PiRoleDocument {
                    frontmatter: detail.frontmatter,
                    body: detail.body,
                })
            })
            .map_err(|error| error.to_string())
    })
}

struct PiTestEnv {
    home: tempfile::TempDir,
    project: tempfile::TempDir,
    ccteam_home: PathBuf,
    log: PathBuf,
    sessions: PathBuf,
    previous: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl PiTestEnv {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let ccteam_home = home.path().join("ccteam-home");
        let log = home.path().join("fake-pi-log.jsonl");
        let sessions = home.path().join("fake-pi-sessions");
        std::fs::create_dir_all(&ccteam_home).unwrap();
        std::fs::create_dir_all(&sessions).unwrap();
        let fake = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/pi_rpc/fake_pi.py")
            .canonicalize()
            .unwrap();
        let values = [
            ("HOME", home.path().as_os_str()),
            ("CCTEAM_HOME", ccteam_home.as_os_str()),
            ("CCTEAM_PI_BIN", fake.as_os_str()),
            ("CCTEAM_PI_FAKE_LOG", log.as_os_str()),
            ("CCTEAM_PI_FAKE_SESSION_DIR", sessions.as_os_str()),
        ];
        let mut previous = Vec::new();
        for (key, value) in values {
            previous.push((key, std::env::var_os(key)));
            std::env::set_var(key, value);
        }
        Self {
            home,
            project,
            ccteam_home,
            log,
            sessions,
            previous,
        }
    }

    fn ctx(&self, sid: &str) -> SpawnCtx {
        SpawnCtx {
            slug: "pi-test".to_string(),
            sid: sid.to_string(),
            cwd: self.project.path().to_path_buf(),
            project_dir: self.project.path().to_path_buf(),
            extra_args: Vec::new(),
            model_id: None,
            effort: None,
            permission_mode: PermissionMode::Skip,
            secret: "test-secret".to_string(),
            remote: None,
        }
    }

    fn sidecar(&self, sid: &str) -> PathBuf {
        self.project
            .path()
            .join(".ccteam/chat")
            .join(sid)
            .join("pi-state.json")
    }

    fn log_rows(&self) -> Vec<serde_json::Value> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

impl Drop for PiTestEnv {
    fn drop(&mut self) {
        for (key, value) in self.previous.drain(..).rev() {
            if let Some(value) = value {
                std::env::set_var(key, value);
            } else {
                std::env::remove_var(key);
            }
        }
    }
}

async fn submit_and_terminal(
    adapter: &PiRpcAdapter,
    handle: &ThreadHandle,
    text: &str,
) -> Vec<ThreadEvent> {
    let mut events = adapter.events(handle);
    let mut submission = adapter
        .submit_turn_routed(
            handle,
            TurnInput::UserText(text.to_string()),
            TurnRouting::Inject,
        )
        .await
        .unwrap();
    submission.release_completion();
    let mut collected = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(3), events.next())
            .await
            .expect("Pi terminal timeout")
            .expect("Pi event stream closed");
        let terminal = matches!(
            event,
            ThreadEvent::TurnCompleted { .. } | ThreadEvent::TurnFailed { .. }
        );
        collected.push(event);
        if terminal {
            return collected;
        }
    }
}

fn terminal(events: &[ThreadEvent]) -> &ThreadEvent {
    events
        .iter()
        .find(|event| {
            matches!(
                event,
                ThreadEvent::TurnCompleted { .. } | ThreadEvent::TurnFailed { .. }
            )
        })
        .expect("terminal event")
}

#[tokio::test]
#[serial]
async fn pi_adapter_is_a_real_but_unreachable_vendor() {
    let env = PiTestEnv::new();
    assert!(env.home.path().exists());
    assert!(env.ccteam_home.exists());
    assert!(env.sessions.exists());
    let adapter = PiRpcAdapter::new(role_reader());
    assert_eq!(adapter.vendor(), AgentVendor::Pi);
    assert!(!AgentVendor::ALL.contains(&AgentVendor::Pi));
    assert!(serde_json::from_str::<AgentVendor>("\"pi\"").is_err());
    assert_eq!(
        serde_json::from_str::<AgentVendor>("\"claude\"").unwrap(),
        AgentVendor::Claude
    );
}

#[tokio::test]
#[serial]
async fn roleless_roleful_resume_and_transactional_role_restart() {
    let env = PiTestEnv::new();
    let adapter = PiRpcAdapter::new(role_reader());

    let roleless_spec = AgentSpecBrief {
        role: String::new(),
    };
    let roleless_ctx = env.ctx("s1");
    let roleless = adapter
        .start_thread(&roleless_spec, &roleless_ctx)
        .await
        .unwrap();
    assert_eq!(roleless.identity, "ccteam-s1");
    for message in ["one", "two"] {
        let events = submit_and_terminal(&adapter, &roleless, message).await;
        assert!(matches!(
            terminal(&events),
            ThreadEvent::TurnCompleted { .. }
        ));
    }
    adapter.close_thread(&roleless).await.unwrap();
    let resumed = adapter
        .start_thread(&roleless_spec, &roleless_ctx)
        .await
        .unwrap();
    assert_eq!(resumed.identity, roleless.identity);
    let third = submit_and_terminal(&adapter, &resumed, "three").await;
    assert!(matches!(
        terminal(&third),
        ThreadEvent::TurnCompleted { .. }
    ));

    let agents = env.project.path().join(".claude/agents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(
        agents.join("reviewer.md"),
        "---\nmodel: anthropic/claude-opus-4-6\neffort: high\npi:\n  model: anthropic/claude-opus-4-6\n  effort: high\n---\nROLE ONE BODY\n",
    )
    .unwrap();
    std::fs::write(
        agents.join("builder.md"),
        "---\npi:\n  model: anthropic/ignored-on-role-switch\n  effort: low\n---\nROLE TWO BODY\n",
    )
    .unwrap();
    std::fs::write(
        agents.join("broken.md"),
        "---\npi:\n  model: anthropic/ignored-on-role-switch\n---\nFAIL ROLE BODY\n",
    )
    .unwrap();
    let roleful_spec = AgentSpecBrief {
        role: "reviewer".to_string(),
    };
    let mut roleful_ctx = env.ctx("s2");
    roleful_ctx.model_id = Some("anthropic/claude-sonnet-4-20250514".to_string());
    roleful_ctx.effort = Some("medium".to_string());
    let roleful = adapter
        .start_thread(&roleful_spec, &roleful_ctx)
        .await
        .unwrap();
    for message in ["role-one", "role-two"] {
        let events = submit_and_terminal(&adapter, &roleful, message).await;
        assert!(matches!(
            terminal(&events),
            ThreadEvent::TurnCompleted { .. }
        ));
    }
    let before: serde_json::Value =
        serde_json::from_slice(&std::fs::read(env.sidecar("s2")).unwrap()).unwrap();
    let outcome = adapter
        .handle_directive(
            &roleful,
            Directive {
                name: "role".to_string(),
                args: "builder".to_string(),
                choice: None,
            },
        )
        .await
        .unwrap();
    assert!(matches!(outcome, DirectiveOutcome::Done { .. }));
    let after: serde_json::Value =
        serde_json::from_slice(&std::fs::read(env.sidecar("s2")).unwrap()).unwrap();
    assert_ne!(before["rolePromptSha"], after["rolePromptSha"]);
    assert_eq!(after["sessionId"], "ccteam-s2");
    assert_eq!(after["model"], "anthropic/claude-sonnet-4-20250514");
    assert_eq!(after["effort"], "medium");
    let role_three = submit_and_terminal(&adapter, &roleful, "role-three").await;
    assert!(matches!(
        terminal(&role_three),
        ThreadEvent::TurnCompleted { .. }
    ));
    let failed_switch = adapter
        .handle_directive(
            &roleful,
            Directive {
                name: "role".to_string(),
                args: "broken".to_string(),
                choice: None,
            },
        )
        .await
        .unwrap_err();
    assert!(failed_switch
        .to_string()
        .contains("previous role was restored"));
    let rolled_back: serde_json::Value =
        serde_json::from_slice(&std::fs::read(env.sidecar("s2")).unwrap()).unwrap();
    assert_eq!(rolled_back["rolePromptSha"], after["rolePromptSha"]);
    let role_four = submit_and_terminal(&adapter, &roleful, "role-four").await;
    assert!(matches!(
        terminal(&role_four),
        ThreadEvent::TurnCompleted { .. }
    ));

    let rows = env.log_rows();
    let roleless_rows: Vec<_> = rows
        .iter()
        .filter(|row| row["session_id"] == "ccteam-s1")
        .collect();
    assert_eq!(roleless_rows.len(), 2);
    assert_eq!(roleless_rows[0]["resumed"], false);
    assert_eq!(roleless_rows[1]["resumed"], true);
    assert!(roleless_rows[0]["prompt_path"].is_null());
    let resumed_args = roleless_rows[1]["args"].as_array().unwrap();
    assert!(resumed_args.iter().any(|arg| arg == "--session"));
    assert!(!resumed_args.iter().any(|arg| arg == "--session-id"));

    let role_rows: Vec<_> = rows
        .iter()
        .filter(|row| row["session_id"] == "ccteam-s2")
        .collect();
    assert_eq!(role_rows.len(), 4);
    assert_eq!(role_rows[0]["prompt_body"], "ROLE ONE BODY\n");
    assert_eq!(role_rows[1]["prompt_body"], "ROLE TWO BODY\n");
    assert_eq!(role_rows[2]["prompt_body"], "FAIL ROLE BODY\n");
    assert_eq!(role_rows[3]["prompt_body"], "ROLE TWO BODY\n");
    assert!(role_rows[1]["resumed"].as_bool().unwrap());
    for row in &rows {
        let args = row["args"].as_array().unwrap();
        assert!(!args.iter().any(|arg| arg == "--no-context-files"));
        assert!(!args.iter().any(|arg| arg == "test-secret"));
    }
    let prompt = PathBuf::from(role_rows[0]["prompt_path"].as_str().unwrap());
    assert!(prompt.starts_with(env.ccteam_home.join("runtime/pi/roles")));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(prompt).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[tokio::test]
#[serial]
async fn settled_terminal_routing_usage_context_and_directives() {
    let env = PiTestEnv::new();
    let adapter = PiRpcAdapter::new(role_reader());
    let handle = adapter
        .start_thread(
            &AgentSpecBrief {
                role: String::new(),
            },
            &env.ctx("s3"),
        )
        .await
        .unwrap();

    for (message, expected_kind) in [
        ("multi", None),
        ("retry", None),
        ("tool-preamble", None),
        ("length", Some("max_tokens")),
        ("error", Some("vendor_error")),
        ("aborted", Some("aborted")),
        ("unknown", Some("protocol")),
        ("no-terminal", Some("protocol")),
    ] {
        let events = submit_and_terminal(&adapter, &handle, message).await;
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    ThreadEvent::TurnCompleted { .. } | ThreadEvent::TurnFailed { .. }
                ))
                .count(),
            1,
            "{message}"
        );
        match (terminal(&events), expected_kind) {
            (ThreadEvent::TurnCompleted { .. }, None) => {}
            (ThreadEvent::TurnFailed { err, .. }, Some(kind)) => assert_eq!(err.kind, kind),
            (event, kind) => panic!("{message}: unexpected {event:?} / {kind:?}"),
        }
        if message == "tool-preamble" {
            assert!(!events.iter().any(|event| matches!(
                event,
                ThreadEvent::ItemCompleted { item }
                    if matches!(item.details, ThreadItemDetails::AgentMessage(_))
            )));
        }
    }

    let usage_events = submit_and_terminal(&adapter, &handle, "usage").await;
    let ThreadEvent::TurnCompleted { usage, model, .. } = terminal(&usage_events) else {
        panic!("usage scenario did not complete");
    };
    assert_eq!(usage.input_tokens, 32);
    assert_eq!(usage.output_tokens, 13);
    assert_eq!(usage.cached_input_tokens, 8);
    assert_eq!(usage.cache_creation_input_tokens, Some(4));
    assert_eq!(usage.reasoning_output_tokens, None);
    assert!((usage.reported_cost_usd.unwrap() - 1.0).abs() < 1e-9);
    assert_eq!(model.as_deref(), Some("anthropic/claude-sonnet-4-20250514"));

    let mut steer_events = adapter.events(&handle);
    let mut first = adapter
        .submit_turn_routed(
            &handle,
            TurnInput::UserText("wait-steer".into()),
            TurnRouting::Inject,
        )
        .await
        .unwrap();
    first.release_completion();
    let mut steered = adapter
        .submit_turn_routed(
            &handle,
            TurnInput::UserText("new direction".into()),
            TurnRouting::Inject,
        )
        .await
        .unwrap();
    assert_eq!(steered.disposition, TurnDisposition::Injected);
    assert_eq!(steered.turn_id, first.turn_id);
    steered.release_completion();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(3), steer_events.next())
            .await
            .unwrap()
            .unwrap();
        if matches!(event, ThreadEvent::TurnCompleted { .. }) {
            break;
        }
    }

    let mut abort_events = adapter.events(&handle);
    let mut aborting = adapter
        .submit_turn_routed(
            &handle,
            TurnInput::UserText("wait-abort".into()),
            TurnRouting::Inject,
        )
        .await
        .unwrap();
    aborting.release_completion();
    adapter.interrupt_turn(&handle).await.unwrap();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(3), abort_events.next())
            .await
            .unwrap()
            .unwrap();
        if let ThreadEvent::TurnFailed { err, .. } = event {
            assert_eq!(err.kind, "aborted");
            break;
        }
    }

    let _ = submit_and_terminal(&adapter, &handle, "context-null").await;
    let status = adapter.thread_status(&handle).await.unwrap();
    let context = status.context.unwrap();
    assert_eq!(context.used_tokens, None);
    assert_eq!(context.window_tokens, 200_000);
    assert_eq!(context.source, ContextSource::Probed);

    let model_choice = adapter
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
    assert!(matches!(model_choice, DirectiveOutcome::NeedsChoice(_)));
    let model_done = adapter
        .handle_directive(
            &handle,
            Directive {
                name: "model".into(),
                args: "anthropic/claude-opus-4-6".into(),
                choice: None,
            },
        )
        .await
        .unwrap();
    assert!(matches!(model_done, DirectiveOutcome::Done { .. }));
    let effort_done = adapter
        .handle_directive(
            &handle,
            Directive {
                name: "effort".into(),
                args: "high".into(),
                choice: None,
            },
        )
        .await
        .unwrap();
    assert!(matches!(effort_done, DirectiveOutcome::Done { .. }));
    assert!(adapter
        .handle_directive(
            &handle,
            Directive {
                name: "model".into(),
                args: "anthropic/force-clamp".into(),
                choice: None,
            },
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("changed model"));
}

#[tokio::test]
#[serial]
async fn explicit_spawn_clamp_and_missing_native_session_fail_hard() {
    let env = PiTestEnv::new();
    let adapter = PiRpcAdapter::new(role_reader());
    let spec = AgentSpecBrief {
        role: String::new(),
    };
    let mut clamped = env.ctx("s4");
    clamped.model_id = Some("anthropic/force-clamp".to_string());
    assert!(adapter
        .start_thread(&spec, &clamped)
        .await
        .unwrap_err()
        .to_string()
        .contains("changed explicit model"));

    let mut clamped_effort = env.ctx("s6");
    clamped_effort.effort = Some("force-clamp".to_string());
    assert!(adapter
        .start_thread(&spec, &clamped_effort)
        .await
        .unwrap_err()
        .to_string()
        .contains("clamped explicit effort"));

    let ctx = env.ctx("s5");
    let handle = adapter.start_thread(&spec, &ctx).await.unwrap();
    let _ = submit_and_terminal(&adapter, &handle, "history").await;
    adapter.close_thread(&handle).await.unwrap();
    let sidecar: serde_json::Value =
        serde_json::from_slice(&std::fs::read(env.sidecar("s5")).unwrap()).unwrap();
    std::fs::remove_file(sidecar["sessionFile"].as_str().unwrap()).unwrap();
    let turns = env.project.path().join(".ccteam/chat/s5/turns.jsonl");
    std::fs::write(turns, "{\"role\":\"assistant\"}\n").unwrap();
    assert!(adapter
        .start_thread(&spec, &ctx)
        .await
        .unwrap_err()
        .to_string()
        .contains("Pi native session missing"));
}
