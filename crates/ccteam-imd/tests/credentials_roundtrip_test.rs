//! V0.6.0 Wave 2 F117 — integration tests for credentials persistence.
//! Uses a tempdir to avoid touching the real `~/.ccteam/im/`.
//!
//! **MERGE INSTRUCTION** (imd's reply confirms): fold the
//! `write_creates_parent_dir` test into imd's
//! `crates/ccteam-imd/tests/credentials_test.rs` (it's the only
//! coverage gap — imd's `save()` calls `fs::create_dir_all(parent)`
//! but never asserts that branch). The other three tests
//! (round-trip / 0600 / missing→default) are already covered by
//! imd's file. Then `git rm` this file.

use ccteam_imd::credentials::{
    load_credentials_from, save, write_credentials_to, Credentials, TelegramCreds,
};

#[test]
fn round_trip_telegram_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("creds.json");

    let creds = Credentials {
        telegram: Some(TelegramCreds {
            bot_token: "12345:secret_token_value".into(),
            allowed_chat_ids: vec!["1234567890".into()],
        }),
    };

    save(&p, &creds).unwrap();
    let back = load_credentials_from(&p).unwrap();
    assert_eq!(creds, back);
}

#[test]
fn write_creates_parent_dir() {
    let dir = tempfile::tempdir().unwrap();
    let nested = dir.path().join("a/b/c/creds.json");
    let creds = Credentials::default();
    write_credentials_to(&nested, &creds).unwrap();
    assert!(nested.exists());
}

#[cfg(unix)]
#[test]
fn write_chmod_0600_enforced() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("creds.json");
    write_credentials_to(&p, &Credentials::default()).unwrap();
    let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn missing_file_loads_default() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("does_not_exist.json");
    let creds = load_credentials_from(&p).unwrap();
    assert_eq!(creds, Credentials::default());
}
