//! VENDOR-INSTALL-1 — the admin one-click vendor install/update job surface:
//! `POST /api/v1/hosts/{host}/vendors/{vendor}/install` (202 + job, same-vendor
//! dedup) and `GET .../install/{job_id}` (poll).
//!
//! Proves:
//! 1. a non-admin tenant is 403 on both endpoints (the real gate; the SPA
//!    merely hides the button)
//! 2. unknown host / unknown vendor → 404; a recipe-less vendor (kimi) → 400
//! 3. happy path against a FAKE `npm` on PATH: the job runs the table-pinned
//!    argv to exit 0, captures an output tail, and a second POST for the same
//!    vendor while running returns the SAME job id (dedup, never duplicate)
//!
//! The fake-`npm` tests mutate the process PATH; they serialize on
//! `PATH_LOCK` so they cannot race each other. (This file is its own
//! integration-test process, so the mutation never reaches other suites.)

use std::net::SocketAddr;

use ccteam_core::tenants::TenantRegistry;
use ccteam_core::CcteamPaths;
use ccteam_web::{router_with_state, AppState, AuthState};
use tokio::net::TcpListener;

const ADMIN_HEX: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

/// Serializes the tests that rewrite the process PATH (a tokio Mutex: its
/// guard is meant to be held across `.await`, unlike a std MutexGuard).
static PATH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn fake_paths(root: &std::path::Path) -> CcteamPaths {
    CcteamPaths {
        root: root.join(".ccteam"),
        projects_root: root.join("projects"),
    }
}

async fn spawn(state: AppState) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router_with_state(state);
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    tokio::task::yield_now().await;
    addr
}

fn client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

fn admin_auth() -> String {
    format!("Bearer ccteam:{ADMIN_HEX}")
}

#[tokio::test]
async fn tenant_is_403_on_install_and_poll() {
    let tmp = tempfile::TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(&paths.root).unwrap();
    std::fs::create_dir_all(paths.users_dir()).unwrap();
    let mut reg = TenantRegistry::default();
    let tenant = reg.add("alice");
    reg.save(&paths.users_dir()).unwrap();
    let tauth = format!("Bearer ccteam:{}", tenant.web_token);

    let state = AppState::with_auth(paths, AuthState::enabled(ADMIN_HEX.into()));
    let addr = spawn(state).await;
    let c = client();

    let r = c
        .post(format!(
            "http://{addr}/api/v1/hosts/local/vendors/claude/install"
        ))
        .header("Authorization", &tauth)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403, "tenant must not start an install job");

    let r = c
        .get(format!(
            "http://{addr}/api/v1/hosts/local/vendors/claude/install/job-zzz"
        ))
        .header("Authorization", &tauth)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403, "tenant must not poll an install job");
}

#[tokio::test]
async fn unknown_host_vendor_and_recipe_less_vendor_are_rejected() {
    let tmp = tempfile::TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(&paths.root).unwrap();
    let state = AppState::with_auth(paths, AuthState::enabled(ADMIN_HEX.into()));
    let addr = spawn(state).await;
    let c = client();

    // A satellite host: installs are local-only.
    let r = c
        .post(format!(
            "http://{addr}/api/v1/hosts/sat-1/vendors/claude/install"
        ))
        .header("Authorization", admin_auth())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);

    // Unknown vendor token.
    let r = c
        .post(format!(
            "http://{addr}/api/v1/hosts/local/vendors/gemini/install"
        ))
        .header("Authorization", admin_auth())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);

    // kimi has no recipe: 400, and the error points at the manual docs link.
    let r = c
        .post(format!(
            "http://{addr}/api/v1/hosts/local/vendors/kimi/install"
        ))
        .header("Authorization", admin_auth())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    let body: serde_json::Value = r.json().await.unwrap();
    let error = body["error"].as_str().unwrap();
    assert!(error.contains("manually"), "{error}");
    assert!(error.contains("moonshotai.github.io/kimi-code"), "{error}");

    // Unknown job id on a valid vendor → 404.
    let r = c
        .get(format!(
            "http://{addr}/api/v1/hosts/local/vendors/claude/install/nope"
        ))
        .header("Authorization", admin_auth())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
}

#[tokio::test]
async fn happy_path_fake_npm_runs_to_ok_and_dedups_a_second_post() {
    let _guard = PATH_LOCK.lock().await;
    let tmp = tempfile::TempDir::new().unwrap();
    let bin_dir = tmp.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    // A fake npm that stays alive long enough for the dedup POST to land,
    // then exits 0 with a recognizable output line.
    let fake_npm = bin_dir.join("npm");
    std::fs::write(
        &fake_npm,
        "#!/bin/sh\necho \"fake-npm start $@\"\nsleep 0.5\necho \"added 1 package in 1s\"\nexit 0\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake_npm, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let old_path = std::env::var("PATH").unwrap_or_default();
    std::env::set_var("PATH", format!("{}:{old_path}", bin_dir.display()));

    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(&paths.root).unwrap();
    let state = AppState::with_auth(paths, AuthState::enabled(ADMIN_HEX.into()));
    let addr = spawn(state).await;
    let c = client();

    let r = c
        .post(format!(
            "http://{addr}/api/v1/hosts/local/vendors/codex/install"
        ))
        .header("Authorization", admin_auth())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 202);
    let first: serde_json::Value = r.json().await.unwrap();
    let job_id = first["job_id"].as_str().unwrap().to_string();
    assert_eq!(first["state"].as_str().unwrap(), "running");

    // Dedup: a second POST for the SAME vendor while running returns the
    // same job id instead of spawning a second npm.
    let r = c
        .post(format!(
            "http://{addr}/api/v1/hosts/local/vendors/codex/install"
        ))
        .header("Authorization", admin_auth())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 202);
    let second: serde_json::Value = r.json().await.unwrap();
    assert_eq!(second["job_id"].as_str().unwrap(), job_id);

    // Poll to completion (fake npm exits 0 after ~0.5s).
    let mut last: Option<serde_json::Value> = None;
    for _ in 0..100 {
        let r = c
            .get(format!(
                "http://{addr}/api/v1/hosts/local/vendors/codex/install/{job_id}"
            ))
            .header("Authorization", admin_auth())
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let view: serde_json::Value = r.json().await.unwrap();
        if view["state"].as_str().unwrap() != "running" {
            last = Some(view);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let done = last.expect("install job should finish within 10s");
    assert_eq!(done["state"].as_str().unwrap(), "ok");
    assert_eq!(done["exit_code"].as_i64().unwrap(), 0);
    let tail = done["output_tail"].as_str().unwrap();
    assert!(tail.contains("added 1 package in 1s"), "{tail}");
    // The echoed command line proves the table-pinned argv ran shell-free.
    assert!(tail.contains("install -g @openai/codex@latest"), "{tail}");

    std::env::set_var("PATH", old_path);
}

#[tokio::test]
async fn failing_installer_reports_exit_code_and_stderr_tail() {
    let _guard = PATH_LOCK.lock().await;
    let tmp = tempfile::TempDir::new().unwrap();
    let bin_dir = tmp.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    // A fake npm that fails the way a read-only global prefix does: a
    // diagnostic on stderr and a non-zero exit.
    let fake_npm = bin_dir.join("npm");
    std::fs::write(
        &fake_npm,
        "#!/bin/sh\necho 'npm ERR! EACCES: permission denied, mkdir /usr/lib/node_modules' >&2\nexit 1\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake_npm, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let old_path = std::env::var("PATH").unwrap_or_default();
    std::env::set_var("PATH", format!("{}:{old_path}", bin_dir.display()));

    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(&paths.root).unwrap();
    let state = AppState::with_auth(paths, AuthState::enabled(ADMIN_HEX.into()));
    let addr = spawn(state).await;
    let c = client();

    let r = c
        .post(format!(
            "http://{addr}/api/v1/hosts/local/vendors/grok/install"
        ))
        .header("Authorization", admin_auth())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 202);
    let job: serde_json::Value = r.json().await.unwrap();
    let job_id = job["job_id"].as_str().unwrap().to_string();

    let mut last: Option<serde_json::Value> = None;
    for _ in 0..100 {
        let r = c
            .get(format!(
                "http://{addr}/api/v1/hosts/local/vendors/grok/install/{job_id}"
            ))
            .header("Authorization", admin_auth())
            .send()
            .await
            .unwrap();
        let view: serde_json::Value = r.json().await.unwrap();
        if view["state"].as_str().unwrap() != "running" {
            last = Some(view);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let done = last.expect("install job should finish within 10s");
    assert_eq!(done["state"].as_str().unwrap(), "failed");
    assert_eq!(done["exit_code"].as_i64().unwrap(), 1);
    // The stderr tail reaches the admin verbatim — no sudo, no fallback.
    let tail = done["output_tail"].as_str().unwrap();
    assert!(tail.contains("EACCES"), "{tail}");

    std::env::set_var("PATH", old_path);
}
