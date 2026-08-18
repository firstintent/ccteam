//! v0.9.0 W3 (F3) — end-to-end remote spawn over an in-process "fake
//! satellite" speaking the real `ccteam-exec.v1` frames, driving the SAME
//! fake-claude python fixture pattern `claude_stream_json_test.rs` uses.
//! Reverse-connection era: the fake satellite registers a control channel
//! in a real [`HostChannelHub`] and answers `exec_open` rendezvous with
//! paired [`ExecBridge`] halves — exactly the daemon-side path production
//! takes (the WS hop is proven separately in
//! `ccteam-web/tests/satellite_ws_test.rs`). This proves the whole remote
//! path: `ClaudeStreamJsonAdapter` (`ctx.remote` set) →
//! `remote_exec::connect` → hub rendezvous → fake satellite → real child
//! process → stdout bridged back → `initialize` handshake → turn → answer,
//! PLUS the exec invariants (vendor/args, `CCTEAM_*` env allowlist,
//! `mcp.json` shipped with `{{DAEMON_URL}}` substituted) and the
//! reconnect-uses-`--resume` contract (tech-design §4.4/§4.5).
//!
//! The fake satellite is a deliberately MINIMAL reimplementation of the
//! exec engine — just enough protocol to drive these assertions; the
//! engine's OWN safety invariants (vendor allowlist / slug registry / path
//! confinement) are tested in `ccteam-harness/src/execution/satellite_exec.rs`.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use ccteam_harness::execution::claude_stream_json::spawn_spec::deterministic_session_uuid;
use ccteam_harness::execution::claude_stream_json::ClaudeStreamJsonAdapter;
use ccteam_harness::execution::host_channel::ExecBridge;
use ccteam_harness::{
    AgentSpecBrief, ExecSpec, HarnessAdapter, HostChannelHub, HubCtrlMsg, PermissionMode,
    RemoteExecTarget, SpawnCtx, ThreadEvent, ThreadItemDetails,
};
use futures::StreamExt;
use serial_test::serial;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command as TokioCommand;
use tokio_tungstenite::tungstenite::Message as TMessage;

/// Same fake `claude` stream-json vendor `claude_stream_json_test.rs`
/// uses (kept in lockstep intentionally — a real second copy would drift).
const FAKE_PY: &str = r#"#!/usr/bin/env python3
import sys, os, json
argv = sys.argv[1:]
mode = "session-id"; sid = ""
i = 0
while i < len(argv):
    a = argv[i]
    if a == "--session-id" and i + 1 < len(argv):
        sid = argv[i+1]; mode = "session-id"; i += 2
    elif a == "--resume" and i + 1 < len(argv):
        sid = argv[i+1]; mode = "resume"; i += 2
    else:
        i += 1
log = os.environ.get("FAKE_SJ_ARGV_LOG")
if log:
    with open(log, "a") as f:
        f.write(mode + " " + sid + "\n")
# Stateful resume fault (mirrors real claude): --resume for a uuid this
# fake has never seen fails BEFORE init; --session-id always succeeds and
# leaves a marker so a LATER --resume for the same uuid succeeds. Models
# the satellite's own claude transcript store persisting across
# reconnects (tech-design SS4.4/SS4.5).
marker = ".fake-claude-seen-" + sid
if mode == "resume" and not os.path.exists(marker):
    sys.exit(1)
if mode == "session-id":
    open(marker, "w").close()
if "--no-chrome" not in argv:
    sys.stderr.write("fake-claude-sj: missing --no-chrome (no system:init)\n")
    sys.exit(3)
def emit(obj):
    sys.stdout.write(json.dumps(obj) + "\n"); sys.stdout.flush()
init_line = sys.stdin.readline()
init_rid = "init"
try:
    init_rid = json.loads(init_line).get("request_id", "init")
except Exception:
    pass
emit({"type":"control_response","response":{"subtype":"success","request_id":init_rid,
      "response":{"commands":[],"models":[{"model":"fake-model"}]}}})
reply = os.environ.get("FAKE_SJ_REPLY", "ok")
while True:
    line = sys.stdin.readline()
    if not line:
        break
    if not line.strip():
        continue
    try:
        ctl = json.loads(line)
    except Exception:
        ctl = None
    if isinstance(ctl, dict) and ctl.get("type") == "control_request":
        rid = ctl.get("request_id", "ctl")
        emit({"type":"control_response","response":{"subtype":"success","request_id":rid,"response":{}}})
        continue
    emit({"type":"assistant","session_id":sid,
          "message":{"role":"assistant","content":[{"type":"text","text":reply}]}})
    emit({"type":"result","subtype":"success","result":reply,"is_error":False,
          "total_cost_usd":0.001,"usage":{"input_tokens":7,"output_tokens":4},
          "session_id":sid})
"#;

fn write_fake(tmp: &Path) -> PathBuf {
    let p = tmp.join("fake-claude-sj-remote.py");
    std::fs::write(&p, FAKE_PY).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

fn ctx_remote(
    tmp: &Path,
    slug: &str,
    sid: &str,
    secret: &str,
    target: RemoteExecTarget,
) -> SpawnCtx {
    SpawnCtx {
        mode: None,
        slug: slug.to_string(),
        sid: sid.to_string(),
        owner: "user:web-api".into(),
        cwd: tmp.to_path_buf(),
        project_dir: tmp.to_path_buf(),
        extra_args: vec![],
        model_id: None,
        effort: None,
        permission_mode: PermissionMode::Skip,
        secret: secret.to_string(),
        remote: Some(target),
    }
}

/// State shared with the fake satellite: every `ExecSpec` it ever
/// received (in order), for the test to assert against.
#[derive(Clone, Default)]
struct FakeSatelliteState {
    received: Arc<StdMutex<Vec<ExecSpec>>>,
    project_root: Arc<StdMutex<Option<PathBuf>>>,
}

/// Register a fake satellite on a fresh hub: a control-channel task that
/// answers every `exec_open` rendezvous by pairing an [`ExecBridge`] and
/// running one fake exec session over it (each in its own task, so a
/// retry can open a second exec while the first drains).
async fn spawn_fake_satellite(project_root: PathBuf) -> (Arc<HostChannelHub>, FakeSatelliteState) {
    let state = FakeSatelliteState {
        received: Arc::new(StdMutex::new(Vec::new())),
        project_root: Arc::new(StdMutex::new(Some(project_root))),
    };
    let hub = Arc::new(HostChannelHub::default());
    let mut reg = hub.register("sat");
    let hub_for_task = hub.clone();
    let state_for_task = state.clone();
    tokio::spawn(async move {
        while let Some(HubCtrlMsg::ExecOpen { nonce, .. }) = reg.ctrl_rx.recv().await {
            let slot = hub_for_task.claim_exec(&nonce, "sat").unwrap();
            let (mine, theirs) = ExecBridge::pair();
            if slot.send(theirs).is_err() {
                continue;
            }
            tokio::spawn(run_fake_exec_session(mine, state_for_task.clone()));
        }
    });
    (hub, state)
}

/// Minimal `ccteam-exec.v1` satellite: read `ExecSpec` → record it →
/// materialize `files` (substituting `{{DAEMON_URL}}`, no confinement
/// check — that invariant is proven separately in
/// `execution/satellite_exec.rs`) → spawn `spec.vendor`'s
/// `CCTEAM_<VENDOR>_BIN` env override → bridge stdio.
async fn run_fake_exec_session(bridge: ExecBridge, state: FakeSatelliteState) {
    let ExecBridge { tx, mut rx } = bridge;
    let Some(TMessage::Text(t)) = rx.recv().await else {
        return;
    };
    let spec: ExecSpec = serde_json::from_str(&t).expect("valid ExecSpec");
    state.received.lock().unwrap().push(spec.clone());

    let cwd = state.project_root.lock().unwrap().clone().unwrap();
    for f in &spec.files {
        let dest = cwd.join(&f.relpath);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        let content = f.content.replace(
            ccteam_harness::ExecSpec::DAEMON_URL_TOKEN,
            "http://127.0.0.1:7331",
        );
        std::fs::write(&dest, content).unwrap();
    }

    let bin_env = format!("CCTEAM_{}_BIN", spec.vendor.to_uppercase());
    let bin = std::env::var(&bin_env).unwrap_or_else(|_| spec.vendor.clone());
    let mut cmd = TokioCommand::new(bin);
    cmd.args(&spec.args)
        .current_dir(&cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    for (k, v) in &spec.env {
        cmd.env(k, v);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let _ = tx
                .send(TMessage::Text(format!(
                    r#"{{"ok":false,"code":"spawn-failed","message":"{e}"}}"#
                )))
                .await;
            return;
        }
    };
    let _ = tx.send(TMessage::Text(r#"{"ok":true}"#.into())).await;

    let mut stdout = child.stdout.take().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    if let Some(mut stderr) = child.stderr.take() {
        tokio::spawn(async move {
            let mut buf = Vec::new();
            let _ = stderr.read_to_end(&mut buf).await;
        });
    }
    let mut buf = [0u8; 8192];
    loop {
        tokio::select! {
            n = stdout.read(&mut buf) => {
                match n {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx.send(TMessage::Binary(buf[..n].to_vec())).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            msg = rx.recv() => {
                match msg {
                    Some(TMessage::Binary(b)) => {
                        if stdin.write_all(&b).await.is_err() { break; }
                    }
                    Some(TMessage::Text(t)) if t.contains("stdin_close") => {
                        let _ = stdin.shutdown().await;
                    }
                    Some(TMessage::Close(_)) | None => break,
                    Some(_) => {}
                }
            }
        }
    }
    let _ = child.start_kill();
    let status = child.wait().await.ok();
    let ev = format!(
        r#"{{"exit":{}}}"#,
        status.and_then(|s| s.code()).unwrap_or(-1)
    );
    let _ = tx.send(TMessage::Text(ev)).await;
}

/// v0.9.0 W3 (F3) — remote spawn e2e: `ExecSpec` carries claude/args/an
/// env allowlisted to `CCTEAM_*`/a `mcp.json` with `{{DAEMON_URL}}`
/// substituted, and the fake vendor's answer comes back through the SAME
/// `ThreadEvent` stream a local spawn would use (adapter protocol logic
/// is unaware of the transport — tech-design §0.4).
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn remote_spawn_e2e_answer_flows_and_exec_spec_is_sane() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fake_bin = write_fake(tmp.path());
    std::env::set_var("CCTEAM_CLAUDE_BIN", &fake_bin);
    std::env::set_var("FAKE_SJ_REPLY", "hello-from-satellite");
    std::env::set_var("CCTEAM_STREAM_JSON_INIT_TIMEOUT_MS", "2000");
    std::env::remove_var("FAKE_SJ_ARGV_LOG");

    let (hub, sat_state) = spawn_fake_satellite(tmp.path().to_path_buf()).await;
    let target = RemoteExecTarget {
        host_id: "sat".into(),
        wire_slug: "sat-demo".into(),
        hub,
    };

    let adapter = ClaudeStreamJsonAdapter::new();
    let ctx = ctx_remote(tmp.path(), "demo", "s7", "sekret", target.clone());
    let handle = adapter
        .start_thread(
            &AgentSpecBrief {
                role: String::new(),
            },
            &ctx,
        )
        .await
        .expect("remote start_thread");
    assert!(
        !tmp.path().join(".claude").exists(),
        "remote daemon-side data home must not get vendor settings"
    );

    // Submit a turn and collect the answer over the events stream.
    adapter
        .submit_turn(&handle, ccteam_harness::TurnInput::UserText("hi".into()))
        .await
        .expect("submit turn");
    let mut stream = adapter.events(&handle);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut answer = None;
    while let Ok(Some(ev)) = tokio::time::timeout_at(deadline, stream.next()).await {
        if let ThreadEvent::ItemCompleted { item } = ev {
            if let ThreadItemDetails::AgentMessage(t) = item.details {
                answer = Some(t);
                break;
            }
        }
    }
    assert_eq!(answer.as_deref(), Some("hello-from-satellite"));

    // ExecSpec invariants. v0.9.0 W3 — a remote spawn ALWAYS attempts
    // `--resume` first (no local fs to check a prior transcript against);
    // the fake vendor mirrors real claude's "unknown uuid" resume failure,
    // so the FIRST-EVER spawn is two exec-bridge connections: a failed
    // `--resume` then a successful fallback `--session-id` (same contract
    // as the local adapter's existing resume-then-fallback path).
    let specs = sat_state.received.lock().unwrap().clone();
    assert_eq!(
        specs.len(),
        2,
        "first-ever remote spawn: failed --resume + fallback --session-id"
    );
    assert!(
        specs[0].args.iter().any(|a| a == "--resume"),
        "first attempt optimistically tries --resume: {:?}",
        specs[0].args
    );
    let spec = &specs[1];
    assert_eq!(spec.vendor, "claude");
    assert_eq!(spec.slug, "sat-demo");
    assert_eq!(spec.sid, "s7");
    assert!(
        spec.args.iter().any(|a| a == "--session-id"),
        "fallback must use --session-id: {:?}",
        spec.args
    );
    // env allowlist: every key is CCTEAM_*.
    assert!(!spec.env.is_empty());
    assert!(spec.env.keys().all(|k| k.starts_with("CCTEAM_")));
    assert_eq!(
        spec.env.get("CCTEAM_CHAT_SID").map(String::as_str),
        Some("s7")
    );
    // mcp.json shipped with the DAEMON_URL token already substituted by
    // the (fake) satellite.
    assert_eq!(spec.files.len(), 1);
    assert!(spec.files[0].relpath.ends_with(".ccteam/chat/s7/mcp.json"));
    let written = std::fs::read_to_string(tmp.path().join(&spec.files[0].relpath)).unwrap();
    assert!(
        written.contains("http://127.0.0.1:7331/mcp"),
        "got: {written}"
    );
    assert!(
        !written.contains("{{DAEMON_URL}}"),
        "token must be substituted, got: {written}"
    );

    adapter.close_thread(&handle).await.ok();
    std::env::remove_var("CCTEAM_CLAUDE_BIN");
    std::env::remove_var("FAKE_SJ_REPLY");
    std::env::remove_var("CCTEAM_STREAM_JSON_INIT_TIMEOUT_MS");
}

/// v0.9.0 W3 (F3/F7) — reconnect contract: a SECOND `start_thread` for the
/// SAME (slug, sid) — modeling a rebuild after the satellite connection
/// died — dials the satellite again and uses `--resume <same deterministic
/// uuid>` (never a fresh `--session-id`, which would lose context).
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn remote_reconnect_uses_resume_with_the_same_deterministic_uuid() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fake_bin = write_fake(tmp.path());
    std::env::set_var("CCTEAM_CLAUDE_BIN", &fake_bin);
    std::env::set_var("FAKE_SJ_REPLY", "ok");
    std::env::set_var("CCTEAM_STREAM_JSON_INIT_TIMEOUT_MS", "2000");
    std::env::remove_var("FAKE_SJ_ARGV_LOG");

    let (hub, sat_state) = spawn_fake_satellite(tmp.path().to_path_buf()).await;
    let target = RemoteExecTarget {
        host_id: "sat".into(),
        wire_slug: "demo".into(),
        hub,
    };
    let uuid = deterministic_session_uuid("demo", "s9");

    let adapter = ClaudeStreamJsonAdapter::new();
    let ctx = ctx_remote(tmp.path(), "demo", "s9", "sekret", target.clone());
    // First spawn ever for this sid: the fake vendor's stateful resume
    // fault fails an optimistic --resume, then falls back to
    // --session-id (two exec-bridge connections; same contract the local
    // adapter already exercises for a missing local transcript).
    let handle1 = adapter
        .start_thread(
            &AgentSpecBrief {
                role: String::new(),
            },
            &ctx,
        )
        .await
        .expect("first remote start_thread");
    adapter.close_thread(&handle1).await.ok();

    // Reconnect — same sid, brand new WS connection (models a rebuild
    // after the satellite connection was lost). The satellite's fake
    // claude has now "seen" this uuid (from the --session-id spawn
    // above), so --resume succeeds on the FIRST attempt this time.
    let handle2 = adapter
        .start_thread(
            &AgentSpecBrief {
                role: String::new(),
            },
            &ctx,
        )
        .await
        .expect("reconnect remote start_thread");
    adapter.close_thread(&handle2).await.ok();

    let specs = sat_state.received.lock().unwrap().clone();
    assert_eq!(
        specs.len(),
        3,
        "1st spawn = resume-fail + session-id-fallback; reconnect = resume-success"
    );
    assert!(specs[0].args.iter().any(|a| a == "--resume"));
    assert!(specs[1].args.iter().any(|a| a == "--session-id"));
    let resume_idx = specs[2]
        .args
        .iter()
        .position(|a| a == "--resume")
        .expect("reconnect must pass --resume");
    assert_eq!(
        specs[2].args[resume_idx + 1],
        uuid,
        "reconnect must resume the SAME deterministic uuid"
    );

    std::env::remove_var("CCTEAM_CLAUDE_BIN");
    std::env::remove_var("CCTEAM_STREAM_JSON_INIT_TIMEOUT_MS");
    std::env::remove_var("FAKE_SJ_REPLY");
}
