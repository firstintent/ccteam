//! v0.8.6 Item 4 — `ccteam config` non-interactive (headless/CI) surface.
//!
//! `config` is the setup hub. Its interactive menu (register the MCP
//! server / set the IM token / show prefs) needs a TTY, but the
//! preference key/value forms are the headless path and wrap the same
//! `preferences.toml` store the retired `ccteam prefs` command used:
//!
//!   - `ccteam config <key> <value>`  set one preference
//!   - `ccteam config get <key>`      read one preference
//!   - `ccteam config show`           print the active preferences
//!
//! These tests drive the real binary with `CCTEAM_HOME` redirected into a
//! tempdir (env-mutating, hence a `tests/` integration file per
//! CLAUDE.md §六) so the round-trip touches a sandboxed
//! `preferences.toml`, never the developer's `~/.ccteam/`.

use std::process::{Command, Stdio};

use tempfile::TempDir;

/// Run the ccteam binary with `CCTEAM_HOME` + `HOME` redirected into a
/// tempdir. stdin is null so the non-interactive forms never block on a
/// prompt (and the interactive menu's TTY guard fires cleanly).
fn run_config(args: &[&str], home: &std::path::Path) -> std::process::Output {
    let bin = env!("CARGO_BIN_EXE_ccteam");
    Command::new(bin)
        .args(args)
        .env("CCTEAM_HOME", home.join("ccteam-home"))
        .env("HOME", home.join("fake-home"))
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|e| panic!("spawn ccteam {args:?}: {e}"))
}

#[test]
fn config_set_get_round_trips_a_preference() {
    // `config <key> <value>` (bare two-arg set) followed by
    // `config get <key>` must echo the value just written. Uses the
    // `fallback.on_claude_quota` knob whose accepted values are a closed
    // set (`off` | `codex`).
    let tmp = TempDir::new().unwrap();

    let set = run_config(&["config", "fallback.on_claude_quota", "codex"], tmp.path());
    let set_out = String::from_utf8_lossy(&set.stdout);
    let set_err = String::from_utf8_lossy(&set.stderr);
    assert!(
        set.status.success(),
        "config set should succeed; stdout={set_out}; stderr={set_err}",
    );
    assert!(
        set_out.contains("set fallback.on_claude_quota = codex"),
        "set must confirm the write; got: {set_out}",
    );

    let get = run_config(&["config", "get", "fallback.on_claude_quota"], tmp.path());
    let get_out = String::from_utf8_lossy(&get.stdout);
    assert!(get.status.success(), "config get should succeed: {get_out}");
    assert_eq!(
        get_out.trim(),
        "codex",
        "config get must echo the value written by config set",
    );
}

#[test]
fn config_single_word_is_treated_as_get() {
    // A single non-keyword word (`config <key>`) is shorthand for
    // `config get <key>`.
    let tmp = TempDir::new().unwrap();
    let _ = run_config(&["config", "fallback.on_claude_quota", "off"], tmp.path());
    let get = run_config(&["config", "fallback.on_claude_quota"], tmp.path());
    let out = String::from_utf8_lossy(&get.stdout);
    assert!(
        get.status.success(),
        "single-word config should succeed: {out}"
    );
    assert_eq!(out.trim(), "off", "single-word form must read the pref");
}

#[test]
fn config_show_prints_preferences_with_path() {
    // `config show` prints the active preferences plus the resolved
    // `preferences.toml` path (matches the former `ccteam prefs` /
    // `prefs show` backend it now wraps).
    let tmp = TempDir::new().unwrap();
    let show = run_config(&["config", "show"], tmp.path());
    let out = String::from_utf8_lossy(&show.stdout);
    let err = String::from_utf8_lossy(&show.stderr);
    assert!(
        show.status.success(),
        "config show should succeed; stdout={out}; stderr={err}",
    );
    assert!(
        out.contains("ccteam preferences") && out.contains("preferences.toml"),
        "config show must print the preferences header + store path; got: {out}",
    );
}

#[test]
fn config_get_unknown_key_errors() {
    // An unknown preference key must fail with a helpful message listing
    // the supported keys — not a silent empty read.
    let tmp = TempDir::new().unwrap();
    let get = run_config(&["config", "get", "fallback.bogus"], tmp.path());
    let out = String::from_utf8_lossy(&get.stdout);
    let err = String::from_utf8_lossy(&get.stderr);
    assert!(
        !get.status.success(),
        "unknown key should fail; stdout={out}; stderr={err}",
    );
    let combined = format!("{out}{err}");
    assert!(
        combined.contains("unknown preference key"),
        "error must name the unknown-key failure; got: {combined}",
    );
}

#[test]
fn config_bare_menu_refuses_without_a_tty() {
    // Bare `ccteam config` with a non-TTY stdin must refuse the
    // interactive menu (rather than hang) and point the operator at the
    // headless forms.
    let tmp = TempDir::new().unwrap();
    let bare = run_config(&["config"], tmp.path());
    let out = String::from_utf8_lossy(&bare.stdout);
    let err = String::from_utf8_lossy(&bare.stderr);
    assert!(
        !bare.status.success(),
        "bare config on non-tty should refuse; stdout={out}; stderr={err}",
    );
    let combined = format!("{out}{err}");
    assert!(
        combined.contains("needs a TTY") && combined.contains("ccteam config show"),
        "refusal must mention the TTY requirement + the headless forms; got: {combined}",
    );
}
