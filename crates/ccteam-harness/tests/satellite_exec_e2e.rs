//! v0.9.0 reverse-connection — satellite exec ENGINE round trip over an
//! in-process [`ExecBridge`] (no WS, no daemon): `ExecSpec` → spawn a fake
//! vendor (`/bin/cat` via `CCTEAM_CLAUDE_BIN`) → byte pump both ways →
//! `stdin_close` half-close → `ExecExit` tail.
//!
//! Lives in `tests/` (own process) because it mutates `CCTEAM_CLAUDE_BIN`
//! — lib `#[cfg(test)]` must never mutate env (CLAUDE.md §六).

use ccteam_harness::execution::host_channel::ExecBridge;
use ccteam_harness::{ExecExit, ExecSpec, ExecStarted, SatelliteExecCtx};
use tokio_tungstenite::tungstenite::Message;

#[tokio::test]
async fn engine_bridges_bytes_and_exits_clean() {
    if !std::path::Path::new("/bin/cat").exists() {
        eprintln!("skipping: /bin/cat missing");
        return;
    }
    std::env::set_var("CCTEAM_CLAUDE_BIN", "/bin/cat");

    let tmp = tempfile::TempDir::new().unwrap();
    let cwd = tmp.path().join("demo");
    std::fs::create_dir_all(&cwd).unwrap();
    let cwd_for_resolver = cwd.clone();
    let resolver = move |slug: &str| -> Option<std::path::PathBuf> {
        (slug == "demo").then(|| cwd_for_resolver.clone())
    };

    let (daemon_half, sat_half) = ExecBridge::pair();
    let engine = tokio::spawn(async move {
        let (stream, sink) = sat_half.into_io();
        let ctx = SatelliteExecCtx {
            daemon_url: "http://127.0.0.1:7331",
            resolve_project_dir: &resolver,
        };
        ccteam_harness::run_exec_session(stream, sink, &ctx).await;
    });

    // Ship one confined file to prove the {{DAEMON_URL}} substitution on
    // the way (write happens before spawn).
    let mut spec = ExecSpec::new("claude", "demo", "s7", "stream-json");
    spec.files.push(ccteam_harness::ExecFile {
        relpath: ".ccteam/chat/s7/mcp.json".into(),
        content: format!(r#"{{"url":"{}/mcp"}}"#, ExecSpec::DAEMON_URL_TOKEN),
    });
    daemon_half
        .tx
        .send(Message::Text(serde_json::to_string(&spec).unwrap()))
        .await
        .unwrap();

    let mut rx = daemon_half.rx;
    let started: ExecStarted = match rx.recv().await {
        Some(Message::Text(t)) => serde_json::from_str(&t).unwrap(),
        other => panic!("expected ExecStarted, got {other:?}"),
    };
    assert!(started.ok, "spawn failed: {:?}", started.message);
    assert!(started.pid.is_some());
    assert_eq!(
        std::fs::read_to_string(cwd.join(".ccteam/chat/s7/mcp.json")).unwrap(),
        r#"{"url":"http://127.0.0.1:7331/mcp"}"#,
        "confined file materialized with daemon-url substitution"
    );

    daemon_half
        .tx
        .send(Message::Binary(b"hello".to_vec()))
        .await
        .unwrap();
    assert_eq!(rx.recv().await, Some(Message::Binary(b"hello".to_vec())));

    daemon_half
        .tx
        .send(Message::Text(r#"{"op":"stdin_close"}"#.into()))
        .await
        .unwrap();
    // cat exits on stdin EOF → ExecExit tail.
    let exit: ExecExit = match rx.recv().await {
        Some(Message::Text(t)) => serde_json::from_str(&t).unwrap(),
        other => panic!("expected ExecExit, got {other:?}"),
    };
    assert_eq!(exit.exit, Some(0));
    engine.await.unwrap();
    std::env::remove_var("CCTEAM_CLAUDE_BIN");
}
