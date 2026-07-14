//! v0.9.0 reverse-connection — full satellite e2e over REAL loopback
//! sockets: daemon router (`/api/v1/hosts/channel` + `/hosts/exec/{nonce}`)
//! on one side, `run_satellite_client` on the other, a fake vendor
//! (`/bin/cat` via `CCTEAM_CLAUDE_BIN`) in between.
//!
//! Proves the whole inverted transport chain:
//! control channel dial + agent-token auth → hub registration → on-connect
//! report (projects land in the registry) → `exec_open` push →
//! `ccteam-exec.v1` dial-back → nonce claim + WS↔bridge pump →
//! `remote_exec::connect` byte round trip → clean EOF on child exit.
//!
//! Env note: sets `CCTEAM_CLAUDE_BIN` — allowed HERE because integration
//! tests run in their own process (CLAUDE.md §六: env-mutating tests never
//! go in lib `#[cfg(test)]`).

use std::time::Duration;

use ccteam_core::host_registry::{HostRecord, HostRegistry};
use ccteam_core::CcteamPaths;
use ccteam_harness::{ExecSpec, RemoteExecTarget};
use ccteam_web::{AppState, AuthState};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const ADMIN_HEX: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

async fn wait_until(deadline_secs: u64, what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(deadline_secs);
    while !cond() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for: {what}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn reverse_connection_exec_round_trips_bytes_end_to_end() {
    if !std::path::Path::new("/bin/cat").exists() {
        eprintln!("skipping: /bin/cat missing");
        return;
    }
    // The satellite resolves its own vendor binary — point `claude` at cat.
    std::env::set_var("CCTEAM_CLAUDE_BIN", "/bin/cat");
    ccteam_core::disable_tool_surface_bootstrap_for_tests();

    // ── daemon side ──────────────────────────────────────────────────────
    let tmp_daemon = tempfile::TempDir::new().unwrap();
    let paths_daemon = CcteamPaths {
        root: tmp_daemon.path().join(".ccteam"),
        projects_root: tmp_daemon.path().join("projects"),
    };
    std::fs::create_dir_all(&paths_daemon.root).unwrap();
    let agent_token = ccteam_core::session_secret::mint();
    {
        let mut reg = HostRegistry::default();
        reg.upsert(HostRecord {
            id: "sat-1".into(),
            hostname: "sat-1.local".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
            ccteam_version: "0.9.0".into(),
            agent_token: agent_token.clone(),
            last_heartbeat_unix: ccteam_core::host_registry::now_unix(),
            agents: vec![],
            projects: vec![],
            joined_at: chrono::Utc::now().to_rfc3339(),
        });
        reg.save(&paths_daemon.host_registry_path()).unwrap();
    }
    let state = AppState::with_auth(paths_daemon.clone(), AuthState::enabled(ADMIN_HEX.into()));
    let hub = state.host_hub.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = ccteam_web::router_with_state(state);
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // ── satellite side (its OWN home + project registry) ─────────────────
    let tmp_sat = tempfile::TempDir::new().unwrap();
    let paths_sat = CcteamPaths {
        root: tmp_sat.path().join(".ccteam"),
        projects_root: tmp_sat.path().join("projects"),
    };
    std::fs::create_dir_all(&paths_sat.root).unwrap();
    let project_dir = tmp_sat.path().join("projects/demo");
    std::fs::create_dir_all(&project_dir).unwrap();
    ccteam_core::config::upsert_project(
        &paths_sat.root,
        ccteam_core::config::ProjectEntry {
            slug: "demo".into(),
            path: project_dir.clone(),
            host: ccteam_core::LOCAL_HOST.to_string(),
            remote_slug: None,
            remote_path: None,
            team: "dev".into(),
            installed_at: chrono::Utc::now(),
        },
    )
    .unwrap();
    let me = ccteam_core::SatelliteSelf {
        daemon_url: format!("http://{addr}"),
        host: "sat-1".into(),
        agent_token: agent_token.clone(),
        heartbeat_ttl_secs: 90,
        joined_at: chrono::Utc::now().to_rfc3339(),
    };
    me.save(&ccteam_core::SatelliteSelf::path_in(&paths_sat.root))
        .unwrap();

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let client_paths = paths_sat.clone();
    let client = tokio::spawn(async move {
        ccteam_web::satellite::run_satellite_client(client_paths, shutdown_rx).await;
    });

    // Control channel comes up + the on-connect report lands `demo` in the
    // daemon's registry (the remote-spawn project gate reads this).
    {
        let hub = hub.clone();
        wait_until(10, "hub registration", move || hub.is_connected("sat-1")).await;
    }
    {
        let reg_path = paths_daemon.host_registry_path();
        wait_until(10, "on-connect report with projects", move || {
            HostRegistry::load(&reg_path)
                .ok()
                .and_then(|r| r.get("sat-1").map(|h| h.has_project("demo")))
                .unwrap_or(false)
        })
        .await;
    }

    // ── project_init: daemon catalog identity ↔ satellite wire identity ──
    let satellite_new_path = tmp_sat.path().join("work/remote-demo");
    let response = reqwest::Client::new()
        .post(format!("http://{addr}/api/v1/projects"))
        .bearer_auth(format!("ccteam:{ADMIN_HEX}"))
        .json(&serde_json::json!({
            "slug": "remote-demo",
            "path": satellite_new_path.display().to_string(),
            "host": "sat-1",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    let created: serde_json::Value = response.json().await.unwrap();
    assert_eq!(created["slug"], "remote-demo");
    assert_eq!(created["host"], "sat-1");
    let catalog = ccteam_core::lookup_project_in_config(&paths_daemon.root, "remote-demo")
        .unwrap()
        .unwrap();
    assert_eq!(catalog.host, "sat-1");
    assert_eq!(catalog.remote_slug.as_deref(), Some("remote-demo"));
    assert_eq!(
        catalog.remote_path.as_deref(),
        Some(satellite_new_path.as_path())
    );
    assert_eq!(catalog.path, paths_daemon.projects_root.join("remote-demo"));
    assert!(catalog.path.join(".ccteam/state.json").is_file());
    assert!(!catalog.path.join("AGENTS.md").exists());
    let satellite_entry = ccteam_core::lookup_project_in_config(&paths_sat.root, "remote-demo")
        .unwrap()
        .unwrap();
    assert_eq!(satellite_entry.path, satellite_new_path);
    assert!(satellite_entry.path.join(".ccteam/state.json").is_file());
    {
        let reg_path = paths_daemon.host_registry_path();
        wait_until(5, "immediate project report", move || {
            HostRegistry::load(&reg_path)
                .ok()
                .and_then(|r| r.get("sat-1").map(|h| h.has_project("remote-demo")))
                .unwrap_or(false)
        })
        .await;
    }

    // ── exec: open → dial-back → byte round trip over /bin/cat ──────────
    let target = RemoteExecTarget {
        host_id: "sat-1".into(),
        wire_slug: "demo".into(),
        hub: hub.clone(),
    };
    let spec = ExecSpec::new("claude", "demo", "s7", "stream-json");
    let (mut reader, mut writer) = ccteam_harness::remote_exec_connect(&target, spec)
        .await
        .expect("exec dial-back must pair and spawn");

    writer.write_all(b"hello over the inversion").await.unwrap();
    writer.flush().await.unwrap();
    let mut buf = [0u8; 64];
    let n = reader.read(&mut buf).await.unwrap();
    assert_eq!(&buf[..n], b"hello over the inversion", "cat echoes stdin");

    // Half-close stdin → cat exits → ExecExit tail reads as clean EOF.
    writer.shutdown().await.unwrap();
    let mut rest = Vec::new();
    reader.read_to_end(&mut rest).await.unwrap();
    assert!(rest.is_empty(), "no payload after exit tail, got {rest:?}");

    // A second exec on the SAME control channel works (nonce is per-exec).
    let spec2 = ExecSpec::new("claude", "demo", "s8", "stream-json");
    let (mut reader2, mut writer2) = ccteam_harness::remote_exec_connect(&target, spec2)
        .await
        .expect("second exec must pair");
    writer2.write_all(b"again").await.unwrap();
    writer2.flush().await.unwrap();
    let n2 = reader2.read(&mut buf).await.unwrap();
    assert_eq!(&buf[..n2], b"again");
    writer2.shutdown().await.unwrap();

    // Unknown slug is a readable rejection, not a hang.
    let bad = ExecSpec::new("claude", "nope", "s9", "stream-json");
    let err = ccteam_harness::remote_exec_connect(&target, bad)
        .await
        .err()
        .expect("unknown slug must be rejected")
        .to_string();
    assert!(err.contains("unknown-slug"), "got: {err}");

    let _ = shutdown_tx.send(true);
    let _ = tokio::time::timeout(Duration::from_secs(5), client).await;
    std::env::remove_var("CCTEAM_CLAUDE_BIN");
}
