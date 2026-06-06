//! V0.6.8 F202 — `ccteam session register` / `ccteam session
//! unregister` CLI smoke tests (v0.8.6 W4a folded the former
//! `ccteam admin register-bot` / `unregister-bot` into the `session`
//! group; the handlers are unchanged).
//!
//! Mirrors the MCP `chat_register_bot` / `chat_unregister_bot` path —
//! same on-disk JSON shape under
//! `<CCTEAM_HOME>/imd/registry/<slug>/<role>.json`. The CLI is the
//! scripted / no-daemon fallback for environments where MCP isn't
//! registered yet.

use std::process::Command;

use serde_json::Value;
use tempfile::TempDir;

fn ccteam_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ccteam")
}

/// Run `ccteam session <subcommand> <args...>` against a tempdir
/// `CCTEAM_HOME`. Returns parsed stdout JSON. Asserts exit success.
fn run_session(home: &std::path::Path, args: &[&str]) -> Value {
    let mut cmd = Command::new(ccteam_bin());
    cmd.arg("session")
        .args(args)
        .env("CCTEAM_HOME", home)
        .env("CCTEAM_DISABLE_TOOL_SURFACE_BOOTSTRAP", "1");
    let out = cmd.output().expect("spawn ccteam session");
    assert!(
        out.status.success(),
        "`ccteam session {}` exited non-zero ({}): stderr=`{}` stdout=`{}`",
        args.join(" "),
        out.status,
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout),
    );
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("parse session stdout JSON ({e}): `{stdout}`"))
}

/// V0.6.8 F202 — round-trip: register a fresh bot, assert the registry
/// JSON file exists with the expected shape, then unregister and
/// confirm the file is gone.
#[test]
fn admin_register_bot_writes_registry_json_then_unregister_removes_it() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("ccteam-home");
    std::fs::create_dir_all(&home).unwrap();

    // Provide a real project_dir on disk so canonicalize() resolves.
    let project_dir = tmp.path().join("my-project");
    std::fs::create_dir_all(&project_dir).unwrap();

    let body = run_session(
        &home,
        &[
            "register",
            "--slug",
            "demo",
            "--role",
            "helper",
            "--platform",
            "mock",
            "--chat-id",
            "12345",
            "--project-dir",
            project_dir.to_str().unwrap(),
        ],
    );

    assert_eq!(
        body["ok"], true,
        "session register must return ok=true; got {body}"
    );
    assert_eq!(body["workflow_slug"], "demo");
    assert_eq!(body["role"], "helper");

    let registry_file = home.join("imd/registry/demo/helper.json");
    assert!(
        registry_file.exists(),
        "registry JSON must exist at {}",
        registry_file.display()
    );

    let on_disk: Value =
        serde_json::from_str(&std::fs::read_to_string(&registry_file).unwrap()).unwrap();
    assert_eq!(on_disk["workflow_slug"], "demo");
    assert_eq!(on_disk["role"], "helper");
    assert_eq!(on_disk["vendor"], "claude", "default vendor must be claude");
    assert_eq!(on_disk["im_platform"], "mock");
    assert_eq!(on_disk["im_chat_id"], "12345");
    // Auto-minted chat_handle (caller did not pass --chat-handle).
    assert!(
        on_disk["chat_handle"].is_string(),
        "chat_handle must be auto-minted to a string; got {on_disk}",
    );
    let handle = on_disk["chat_handle"].as_str().unwrap();
    assert!(
        !handle.is_empty(),
        "auto-minted chat_handle must be non-empty"
    );

    // Unregister the bot — round-trip the registry back to clean.
    let body = run_session(&home, &["unregister", "--slug", "demo", "--role", "helper"]);
    assert_eq!(body["ok"], true, "session unregister must return ok=true");
    assert_eq!(
        body["removed"], true,
        "first unregister must report removed=true"
    );
    assert!(
        !registry_file.exists(),
        "registry JSON must be gone after unregister; still at {}",
        registry_file.display()
    );

    // Idempotent miss — second unregister returns ok=true, removed=false.
    let body = run_session(&home, &["unregister", "--slug", "demo", "--role", "helper"]);
    assert_eq!(
        body["ok"], true,
        "idempotent unregister must return ok=true"
    );
    assert_eq!(
        body["removed"], false,
        "second unregister must report removed=false (idempotent miss)"
    );
}

/// V0.6.8 F202 — explicit `--chat-handle` wins over auto-mint.
#[test]
fn admin_register_bot_respects_explicit_chat_handle() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("ccteam-home");
    std::fs::create_dir_all(&home).unwrap();
    let project_dir = tmp.path().join("p");
    std::fs::create_dir_all(&project_dir).unwrap();

    let body = run_session(
        &home,
        &[
            "register",
            "--slug",
            "demo2",
            "--role",
            "lead",
            "--platform",
            "mock",
            "--chat-id",
            "999",
            "--chat-handle",
            "captain",
            "--project-dir",
            project_dir.to_str().unwrap(),
        ],
    );
    assert_eq!(body["ok"], true);
    assert_eq!(body["chat_handle"], "captain");

    let registry_file = home.join("imd/registry/demo2/lead.json");
    let on_disk: Value =
        serde_json::from_str(&std::fs::read_to_string(&registry_file).unwrap()).unwrap();
    assert_eq!(on_disk["chat_handle"], "captain");
}

/// V0.6.8 F202 — re-registering the same `(slug, role)` is non-clobber
/// (matches the MCP `chat_register_bot` semantics).
#[test]
fn admin_register_bot_refuses_duplicate_without_unregister() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("ccteam-home");
    std::fs::create_dir_all(&home).unwrap();
    let project_dir = tmp.path().join("p");
    std::fs::create_dir_all(&project_dir).unwrap();

    let first = run_session(
        &home,
        &[
            "register",
            "--slug",
            "demo3",
            "--role",
            "lead",
            "--platform",
            "mock",
            "--chat-id",
            "1",
            "--project-dir",
            project_dir.to_str().unwrap(),
        ],
    );
    assert_eq!(first["ok"], true);

    let dup = run_session(
        &home,
        &[
            "register",
            "--slug",
            "demo3",
            "--role",
            "lead",
            "--platform",
            "mock",
            "--chat-id",
            "1",
            "--project-dir",
            project_dir.to_str().unwrap(),
        ],
    );
    assert_eq!(
        dup["ok"], false,
        "duplicate registration must report ok=false"
    );
    assert_eq!(
        dup["error"], "already_registered",
        "duplicate must surface the `already_registered` sentinel"
    );
}
