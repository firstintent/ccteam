//! V0.4.6 F88 — copy a string to the OS clipboard via a fallback chain
//! of platform-specific helper binaries.
//!
//! Used by `ccteam start` to auto-copy the embedded web bearer token so
//! the operator can paste it straight into a browser without manual
//! mouse-select. The fallback chain is intentionally exec-based (no
//! `arboard` / `clipboard` crate) so:
//!
//! - WSL works (`clip.exe` lives in /mnt/c/Windows/System32; the
//!   common Rust clipboard crates assume native X / Wayland)
//! - headless servers (no X / Wayland, no Mac, no Windows) degrade
//!   silently — `copy_to_clipboard` returns `Ok(None)` so the caller
//!   prints the token plus a "copy manually" hint
//! - we don't add a heavy dependency for a 30-line job
//!
//! No tests live next to this module: the providers are platform
//! binaries, asserting on their behavior in CI is brittle. We rely on
//! manual smoke (Linux/WSL/macOS) at ship time.
//!
//! Spec: `docs/versions/v0-4-6/prd.md` F88, `docs/versions/v0-4-6/dev-plan.md` 阶段 9.

use std::io::Write;
use std::process::{Command, Stdio};

/// Try each known clipboard provider in order; the first one that
/// `spawn()`s successfully and exits 0 wins.
///
/// Returns:
/// - `Ok(Some(provider_name))` — clipboard write succeeded; the name
///   (e.g. `"xclip"`, `"clip.exe"`) is for the caller to surface in
///   user-facing output if it wants
/// - `Ok(None)` — no provider was reachable on this host (headless
///   Linux server, missing helpers); **not** an error — the caller is
///   expected to fall back to printing the value
///
/// Never returns `Err`: by design, clipboard failure is not a hard
/// error. (`std::process::Command::spawn` failure is mapped to "try
/// the next provider".)
pub fn copy_to_clipboard(s: &str) -> Option<&'static str> {
    // Order matters: native Linux helpers first, then WSL's
    // `clip.exe` so a Linux box with neither X nor Wayland still
    // tries the others before silently giving up.
    let candidates: &[(&str, &[&str])] = &[
        ("xclip", &["xclip", "-selection", "clipboard"]),
        ("xsel", &["xsel", "--clipboard", "--input"]),
        ("wl-copy", &["wl-copy"]),
        ("pbcopy", &["pbcopy"]),
        ("clip.exe", &["clip.exe"]),
    ];
    for (name, argv) in candidates {
        if try_provider(argv, s) {
            return Some(*name);
        }
    }
    None
}

fn try_provider(argv: &[&str], payload: &str) -> bool {
    let Ok(mut child) = Command::new(argv[0])
        .args(&argv[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    if let Some(mut stdin) = child.stdin.take() {
        // Write errors → child probably exited; we still wait() and
        // inspect status. clip.exe in WSL is happy with CRLF too.
        let _ = stdin.write_all(payload.as_bytes());
        drop(stdin);
    }
    match child.wait() {
        Ok(status) => status.success(),
        Err(_) => false,
    }
}
