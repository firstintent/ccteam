//! V0.3 M5.3 — token file management integration tests.
//!
//! Mirrors `src/token.rs::tests` but at the integration level so any
//! regression that breaks the public surface (`generate_or_load_token`
//! / `load_existing` / `default_token_path`) shows up red as well as
//! the unit-level guards. Also exercises the `CcteamPaths` →
//! `default_token_path` integration.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use ccteam_core::CcteamPaths;
use ccteam_web::token::{default_token_path, generate_or_load_token, load_existing};
use tempfile::TempDir;

fn fake_paths(root: &std::path::Path) -> CcteamPaths {
    CcteamPaths {
        root: root.join(".ccteam"),
        projects_root: root.join("projects"),
    }
}

#[test]
fn first_call_generates_64_hex_chars_and_creates_file_with_mode_0600() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let token_path = default_token_path(&paths);
    assert!(!token_path.exists());

    let tok = generate_or_load_token(&token_path).unwrap();
    assert_eq!(tok.len(), 64, "32 bytes = 64 hex chars");
    assert!(tok.chars().all(|c| c.is_ascii_hexdigit()));
    assert!(token_path.exists());

    #[cfg(unix)]
    {
        let mode = std::fs::metadata(&token_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "fresh token file must be mode 600");
    }
}

#[test]
fn second_call_returns_same_token() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let token_path = default_token_path(&paths);

    let first = generate_or_load_token(&token_path).unwrap();
    let second = generate_or_load_token(&token_path).unwrap();
    assert_eq!(first, second, "subsequent calls must reuse the token");
}

#[test]
fn delete_then_regenerate_creates_fresh_token() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let token_path = default_token_path(&paths);

    let first = generate_or_load_token(&token_path).unwrap();
    std::fs::remove_file(&token_path).unwrap();
    assert!(!token_path.exists());
    let second = generate_or_load_token(&token_path).unwrap();
    assert_ne!(first, second, "regenerated token must differ");
}

#[test]
fn load_existing_trims_trailing_whitespace() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let token_path = default_token_path(&paths);
    std::fs::create_dir_all(token_path.parent().unwrap()).unwrap();
    std::fs::write(&token_path, "deadbeef\n").unwrap();
    let tok = load_existing(&token_path).unwrap();
    assert_eq!(tok, "deadbeef");
}

#[test]
fn default_token_path_resolves_under_root() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let token_path = default_token_path(&paths);
    assert_eq!(token_path, paths.root.join("web-token"));
}
