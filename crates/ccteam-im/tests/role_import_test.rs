//! v0.8.7 W3 (DC.2) — deterministic tests for `import_role_from_catalog`.
//!
//! The importer fetches a role `.md` from the upstream raw host and writes
//! it verbatim into `<project>/.claude/agents/<role>.md`. A one-shot std
//! `TcpListener` stands in for `raw.githubusercontent.com` so the round-trip
//! is real HTTP but never touches the network — no `wiremock`/axum dep, and
//! the base URL is parameterized exactly for this (mirrors
//! `lark_setup_with_base` in `lark_onboarding_test.rs`). The project dir is a
//! `TempDir`, so nothing writes to the real `~/.ccteam` / `~/.claude.json`.

use std::io::{Read, Write};
use std::net::TcpListener;

use ccteam_im::role_import::{import_role_from_catalog_with_base, ImportError};
use tempfile::TempDir;

/// Spawn a single-shot HTTP/1.1 responder on `127.0.0.1:0` that replies to
/// the first connection with `status_line` + `body` and exits. Returns
/// `http://127.0.0.1:<port>` — a raw-base override.
fn spawn_oneshot_http(status_line: &'static str, body: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let resp = format!(
                "{status_line}\r\nContent-Type: text/plain\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.flush();
        }
    });
    format!("http://{addr}")
}

/// Pick a real catalog id from the vendored manifest so the offline lookup
/// resolves (the test then overrides only the *network* base).
fn a_real_catalog_id() -> ccteam_core::CatalogEntry {
    ccteam_core::catalog_all()
        .expect("vendored catalog parses")
        .into_iter()
        .next()
        .expect("catalog is non-empty")
}

const FAKE_ROLE_MD: &str =
    "---\nname: imported\ndescription: A fetched role\n---\nYou are a helpful imported role.\n";

#[tokio::test]
async fn import_fetches_sanitizes_and_writes() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path();
    let entry = a_real_catalog_id();
    let base = spawn_oneshot_http("HTTP/1.1 200 OK", FAKE_ROLE_MD);

    // Override the role stem with one needing sanitizing ("My Agent").
    let result =
        import_role_from_catalog_with_base(project, &entry.id, Some("My Agent"), false, &base)
            .await
            .expect("import from a 200 mock must succeed");

    // Stem sanitized to [a-z0-9_-].
    assert_eq!(result.role, "my-agent");
    assert!(!result.overwrote);
    // The file landed under .claude/agents/ with the sanitized name ...
    let dest = project.join(".claude").join("agents").join("my-agent.md");
    assert!(dest.is_file(), "expected {} to exist", dest.display());
    assert_eq!(result.path, dest);
    // ... and was written VERBATIM (no frontmatter conversion).
    let written = std::fs::read_to_string(&dest).unwrap();
    assert_eq!(written, FAKE_ROLE_MD);
}

#[tokio::test]
async fn import_defaults_stem_to_catalog_display_name() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path();
    let entry = a_real_catalog_id();
    let base = spawn_oneshot_http("HTTP/1.1 200 OK", FAKE_ROLE_MD);

    let result = import_role_from_catalog_with_base(project, &entry.id, None, false, &base)
        .await
        .expect("import must succeed with default stem");

    // With no --as override, the stem is the (sanitized) catalog display_name.
    let expected = ccteam_core::sanitize_role_stem(&entry.display_name).unwrap();
    assert_eq!(result.role, expected);
}

#[tokio::test]
async fn import_unknown_id_is_offline_error() {
    let tmp = TempDir::new().unwrap();
    // Even if we hand it a (here unused) base, an unknown id must fail at the
    // offline lookup — no fetch attempted.
    let err = import_role_from_catalog_with_base(
        tmp.path(),
        "definitely-not-a-real-catalog-id-xyz",
        None,
        false,
        "http://127.0.0.1:1", // never contacted
    )
    .await
    .expect_err("unknown id must error");
    assert!(
        matches!(err, ImportError::UnknownId(_)),
        "expected UnknownId, got {err:?}"
    );
}

#[tokio::test]
async fn import_refuses_existing_without_force_then_overwrites_with_force() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path();
    let entry = a_real_catalog_id();

    // First import succeeds.
    let base1 = spawn_oneshot_http("HTTP/1.1 200 OK", FAKE_ROLE_MD);
    let first = import_role_from_catalog_with_base(project, &entry.id, Some("dup"), false, &base1)
        .await
        .expect("first import succeeds");
    assert_eq!(first.role, "dup");
    assert!(!first.overwrote);

    // Second import of the same stem WITHOUT force is refused (no fetch — the
    // exists check fires before the HTTP call, so we don't even need a mock).
    let err = import_role_from_catalog_with_base(
        project,
        &entry.id,
        Some("dup"),
        false,
        "http://127.0.0.1:1",
    )
    .await
    .expect_err("re-import without --force must be refused");
    assert!(
        matches!(err, ImportError::Exists(ref r) if r == "dup"),
        "expected Exists(\"dup\"), got {err:?}"
    );

    // With --force it overwrites.
    let base2 = spawn_oneshot_http("HTTP/1.1 200 OK", "---\nname: dup2\n---\noverwritten\n");
    let forced = import_role_from_catalog_with_base(project, &entry.id, Some("dup"), true, &base2)
        .await
        .expect("forced re-import succeeds");
    assert!(
        forced.overwrote,
        "force over an existing file sets overwrote"
    );
    let body = std::fs::read_to_string(project.join(".claude/agents/dup.md")).unwrap();
    assert!(body.contains("overwritten"), "file should be the new body");
}

#[tokio::test]
async fn import_honest_error_on_404() {
    let tmp = TempDir::new().unwrap();
    let entry = a_real_catalog_id();
    let base = spawn_oneshot_http("HTTP/1.1 404 Not Found", "not found");

    let err = import_role_from_catalog_with_base(tmp.path(), &entry.id, Some("x"), false, &base)
        .await
        .expect_err("a 404 from upstream must be an honest error");
    match err {
        ImportError::BadStatus { status, .. } => assert_eq!(status, 404),
        other => panic!("expected BadStatus 404, got {other:?}"),
    }
    // Nothing was written on a failed fetch.
    assert!(
        !tmp.path().join(".claude/agents/x.md").exists(),
        "no role file should be written on a 404"
    );
}

#[tokio::test]
async fn import_honest_error_on_network_failure() {
    let tmp = TempDir::new().unwrap();
    let entry = a_real_catalog_id();
    // Port 1 on loopback refuses immediately → a transport (Http) error.
    let err = import_role_from_catalog_with_base(
        tmp.path(),
        &entry.id,
        Some("x"),
        false,
        "http://127.0.0.1:1",
    )
    .await
    .expect_err("a connection refusal must surface as an error");
    assert!(
        matches!(err, ImportError::Http { .. }),
        "expected Http transport error, got {err:?}"
    );
}

#[tokio::test]
async fn import_rejects_empty_body() {
    let tmp = TempDir::new().unwrap();
    let entry = a_real_catalog_id();
    let base = spawn_oneshot_http("HTTP/1.1 200 OK", "   \n  ");

    let err = import_role_from_catalog_with_base(tmp.path(), &entry.id, Some("x"), false, &base)
        .await
        .expect_err("an empty fetched body must be refused");
    assert!(
        matches!(err, ImportError::EmptyBody(_)),
        "expected EmptyBody, got {err:?}"
    );
}
