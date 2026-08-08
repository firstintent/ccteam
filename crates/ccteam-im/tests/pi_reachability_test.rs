use std::ffi::OsString;
use std::path::{Path, PathBuf};

use ccteam_harness::{AgentVendor, PermissionMode, SessionProtocol};
use ccteam_im::daemon::default_adapter_factory;
use ccteam_im::gateway::{Gateway, SpawnTuning};
use ccteam_im::BotRegistration;

struct EnvGuard(Vec<(&'static str, Option<OsString>)>);

impl EnvGuard {
    fn set(values: &[(&'static str, &Path)]) -> Self {
        let previous = values
            .iter()
            .map(|(key, _)| (*key, std::env::var_os(key)))
            .collect();
        for (key, value) in values {
            std::env::set_var(key, value);
        }
        Self(previous)
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in self.0.drain(..).rev() {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

fn fake_pi() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../ccteam-harness/tests/fixtures/pi_rpc/fake_pi.py")
        .canonicalize()
        .unwrap()
}

fn log_rows(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// A managed Pi session's tool surface IS the ccteam bridge, so every spawn
/// here dials a real `POST /mcp`. Without a stub the endpoint resolves to the
/// default-bind fallback and the test dials the developer's own live daemon on
/// 7331 (401) — or gets ECONNREFUSED on CI. Kept dependency-free: three
/// JSON-RPC calls over `Connection: close`, which is all the fake bridge makes.
async fn start_stub_mcp() -> (tokio::task::JoinHandle<()>, String) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut raw = Vec::new();
                let mut buf = [0u8; 4096];
                let body = loop {
                    let Ok(read) = socket.read(&mut buf).await else {
                        return;
                    };
                    if read == 0 {
                        return;
                    }
                    raw.extend_from_slice(&buf[..read]);
                    if let Some(at) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&raw[..at]).to_lowercase();
                        let len: usize = head
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length:"))
                            .and_then(|v| v.trim().parse().ok())
                            .unwrap_or(0);
                        if raw.len() >= at + 4 + len {
                            break raw[at + 4..at + 4 + len].to_vec();
                        }
                    }
                };
                let request: serde_json::Value =
                    serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
                let result = match request["method"].as_str().unwrap_or_default() {
                    // The adapter refuses a partial tool set, so serve exactly
                    // the eight the bridge contract requires.
                    "tools/list" => serde_json::json!({
                        "tools": ccteam_harness::PI_REQUIRED_MCP_TOOL_NAMES
                            .iter()
                            .map(|name| serde_json::json!({
                                "name": name,
                                "description": name,
                                "inputSchema": {"type": "object"},
                            }))
                            .collect::<Vec<_>>()
                    }),
                    _ => serde_json::json!({}),
                };
                let payload = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": request.get("id").cloned().unwrap_or(serde_json::Value::Null),
                    "result": result,
                })
                .to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                    payload.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    (task, url)
}

#[tokio::test]
async fn all_four_entry_spines_share_pi_role_recipe_and_rejection_contract() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let ccteam_home = home.path().join("ccteam-home");
    let session_dir = home.path().join("pi-sessions");
    let log = home.path().join("pi-spawns.jsonl");
    std::fs::create_dir_all(&ccteam_home).unwrap();
    std::fs::create_dir_all(&session_dir).unwrap();
    let fake = fake_pi();
    let _env = EnvGuard::set(&[
        ("HOME", home.path()),
        ("CCTEAM_HOME", &ccteam_home),
        ("CCTEAM_PI_BIN", &fake),
        ("CCTEAM_PI_FAKE_LOG", &log),
        ("CCTEAM_PI_FAKE_SESSION_DIR", &session_dir),
    ]);
    // Publish the stub the way a real daemon publishes its bind, so the spawn
    // path resolves through `run/mcp-url` instead of the default fallback.
    let (_mcp, mcp_url) = start_stub_mcp().await;
    std::fs::create_dir_all(ccteam_home.join("run")).unwrap();
    std::fs::write(ccteam_home.join("run/mcp-url"), &mcp_url).unwrap();

    let agents = project.path().join(".claude/agents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(
        agents.join("reviewer.md"),
        "---\nmodel: generic/ignored\neffort: low\npi:\n  model: anthropic/pi-good\n  effort: high\n---\nPI ROLE BODY\n",
    )
    .unwrap();
    std::fs::write(
        agents.join("bad-model.md"),
        "---\npi:\n  model: anthropic/force-clamp\n  effort: high\n---\nBAD PI ROLE BODY\n",
    )
    .unwrap();
    std::fs::write(
        agents.join("bad-effort.md"),
        "---\npi:\n  model: anthropic/pi-good\n  effort: force-clamp\n---\nBAD PI EFFORT BODY\n",
    )
    .unwrap();

    let mut gateway = Gateway::new_with_factory(default_adapter_factory(), "demo", project.path());

    gateway
        .handle_text("mock", "im", "alice", "/new pi reviewer")
        .await
        .unwrap();
    let rest_sid = gateway
        .create_session_api_tuned(
            "demo".into(),
            "reviewer".into(),
            AgentVendor::Pi,
            PermissionMode::Skip,
            SessionProtocol::StreamJson,
            "web-api".into(),
            SpawnTuning::default(),
        )
        .await
        .unwrap()
        .sid;
    let mcp_sid = gateway
        .create_delegated_session(
            "demo".into(),
            "reviewer".into(),
            AgentVendor::Pi,
            PermissionMode::Skip,
            SessionProtocol::StreamJson,
            "web-api".into(),
            SpawnTuning::default(),
            None,
            None,
        )
        .await
        .unwrap()
        .sid;
    gateway.register_bot_template(
        &BotRegistration {
            workflow_slug: "demo".into(),
            role: "reviewer".into(),
            vendor: AgentVendor::Pi,
            persona_id: None,
            im_platform: "web".into(),
            im_chat_id: "lazy".into(),
            chat_handle: None,
            project_dir: Some(project.path().to_path_buf()),
            created_at: chrono::Utc::now(),
        },
        project.path(),
    );
    gateway
        .handle_text("web", "lazy", "web-user", "first message")
        .await
        .unwrap();

    let rows = log_rows(&log);
    assert_eq!(rows.len(), 4, "one Pi child per entry path: {rows:?}");
    for row in &rows {
        let args = row["args"].as_array().unwrap();
        let strings: Vec<&str> = args.iter().filter_map(|v| v.as_str()).collect();
        assert!(strings
            .windows(2)
            .any(|w| w == ["--model", "anthropic/pi-good"]));
        assert!(strings.windows(2).any(|w| w == ["--thinking", "high"]));
        assert!(strings.contains(&"--system-prompt"));
        assert_eq!(row["prompt_body"], "PI ROLE BODY\n");
        assert!(!strings.contains(&"--no-context-files"));
    }
    let catalog = ccteam_core::model_catalog::load_model_catalog_in(&ccteam_home);
    let pi_models = &catalog.0["pi"];
    assert_eq!(pi_models.source, "Pi RPC get_available_models");
    assert!(pi_models.models.iter().any(|model| {
        model.id == "anthropic/claude-sonnet-4-20250514"
            && model.efforts.iter().any(|effort| effort == "high")
    }));

    for (role, facet) in [("bad-model", "model"), ("bad-effort", "effort")] {
        for path in ["im", "rest", "mcp", "web-lazy"] {
            let result = match path {
                "im" => gateway
                    .handle_text(
                        "mock",
                        &format!("im-{facet}"),
                        "alice",
                        &format!("/new pi {role}"),
                    )
                    .await
                    .map(|_| ()),
                "rest" => gateway
                    .create_session_api_tuned(
                        "demo".into(),
                        role.into(),
                        AgentVendor::Pi,
                        PermissionMode::Skip,
                        SessionProtocol::StreamJson,
                        "web-api".into(),
                        SpawnTuning::default(),
                    )
                    .await
                    .map(|_| ()),
                "mcp" => gateway
                    .create_delegated_session(
                        "demo".into(),
                        role.into(),
                        AgentVendor::Pi,
                        PermissionMode::Skip,
                        SessionProtocol::StreamJson,
                        "web-api".into(),
                        SpawnTuning::default(),
                        None,
                        None,
                    )
                    .await
                    .map(|_| ()),
                "web-lazy" => {
                    gateway.register_bot_template(
                        &BotRegistration {
                            workflow_slug: "demo".into(),
                            role: role.into(),
                            vendor: AgentVendor::Pi,
                            persona_id: None,
                            im_platform: "web".into(),
                            im_chat_id: format!("lazy-{facet}"),
                            chat_handle: None,
                            project_dir: Some(project.path().to_path_buf()),
                            created_at: chrono::Utc::now(),
                        },
                        project.path(),
                    );
                    gateway
                        .handle_text("web", &format!("lazy-{facet}"), "web-user", "first message")
                        .await
                        .map(|_| ())
                }
                _ => unreachable!(),
            };
            let error = result.expect_err(path).to_string();
            assert!(
                error.contains("effective") && error.contains("force-clamp"),
                "{facet}/{path}: {error}"
            );
        }
    }

    for path in ["im", "rest", "mcp", "web-lazy"] {
        let result = match path {
            "im" => gateway
                .handle_text("mock", "im-missing", "alice", "/new pi missing")
                .await
                .map(|_| ()),
            "rest" => gateway
                .create_session_api_tuned(
                    "demo".into(),
                    "missing".into(),
                    AgentVendor::Pi,
                    PermissionMode::Skip,
                    SessionProtocol::StreamJson,
                    "web-api".into(),
                    SpawnTuning::default(),
                )
                .await
                .map(|_| ()),
            "mcp" => gateway
                .create_delegated_session(
                    "demo".into(),
                    "missing".into(),
                    AgentVendor::Pi,
                    PermissionMode::Skip,
                    SessionProtocol::StreamJson,
                    "web-api".into(),
                    SpawnTuning::default(),
                    None,
                    None,
                )
                .await
                .map(|_| ()),
            "web-lazy" => {
                gateway.register_bot_template(
                    &BotRegistration {
                        workflow_slug: "demo".into(),
                        role: "missing".into(),
                        vendor: AgentVendor::Pi,
                        persona_id: None,
                        im_platform: "web".into(),
                        im_chat_id: "lazy-missing".into(),
                        chat_handle: None,
                        project_dir: Some(project.path().to_path_buf()),
                        created_at: chrono::Utc::now(),
                    },
                    project.path(),
                );
                gateway
                    .handle_text("web", "lazy-missing", "web-user", "first message")
                    .await
                    .map(|_| ())
            }
            _ => unreachable!(),
        };
        let error = result.expect_err(path);
        assert!(
            error
                .downcast_ref::<ccteam_im::gateway::RoleNotFound>()
                .is_some(),
            "{path}: role failure must stay typed: {error:#}"
        );
    }

    for sid in ["s1", rest_sid.as_str(), mcp_sid.as_str(), "s4"] {
        gateway.stop_session(sid).await.unwrap();
    }
}
