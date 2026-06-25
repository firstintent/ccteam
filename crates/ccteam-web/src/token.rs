//! V0.3 M5.3 — auth-token file management.
//!
//! Persists a 32-byte hex-encoded random token at
//! `~/.ccteam/web-token` (or a caller-supplied path) with mode 0600.
//! On startup the auth middleware loads the token from this file and
//! checks every request's `Authorization: Bearer ccteam:<token>` header
//! (or the matching `ccteam_token` cookie set via the URL shim).
//!
//! Threat-model anchor: PRD §9 — when `ccteam web` binds non-loopback
//! the orchestrator's `--dangerously-skip-permissions` claude session
//! is one POST away from arbitrary shell exec. The token gate keeps
//! that surface behind a 256-bit secret that only a user with shell
//! access to the host can read (mode 0600 enforced + warned).
//!
//! Architecture refs: `docs/versions/v0-3/prd.md` §6.2.4, §9.1.1.

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use anyhow::{anyhow, Context, Result};
use ccteam_core::CcteamPaths;
use rand::RngCore;

/// Length of the random secret (bytes). 32 bytes ⇒ 64 hex chars after
/// encoding — comfortably above brute-force feasibility for the
/// LAN-attacker threat model in PRD §9.1.
pub const TOKEN_BYTES: usize = 32;

/// Default token-file path (`~/.ccteam/secrets/web-token`, v0.8.20 layout).
pub fn default_token_path(paths: &CcteamPaths) -> PathBuf {
    paths.web_token_path()
}

/// Hex-encode a byte slice with lowercase digits. Inlined to avoid
/// pulling another crate just for this (the `subtle` dep gives us
/// constant-time compare; encoding doesn't need timing protection
/// because the input is already the freshly-generated random bytes).
fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Generate-or-load the auth token from `path`.
///
/// - **Generate**: if `path` does not exist, create it via
///   `OpenOptions::create_new` (refuses to overwrite a racing writer)
///   with mode 0600 on Unix. The freshly-generated 32-byte secret is
///   hex-encoded into the file and returned.
/// - **Load**: if `path` exists, read it, trim whitespace, and check
///   the file mode. If the mode is not exactly 0600 we log a stderr
///   warning (don't error) so the operator can fix it.
///
/// Returns the hex token string (no `ccteam:` prefix — callers that
/// want the wire-format prefix should construct `format!("ccteam:{}",
/// token)` themselves).
pub fn generate_or_load_token(path: &Path) -> Result<String> {
    if path.exists() {
        return load_existing(path);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create parent of {}", path.display()))?;
    }

    let mut buf = [0u8; TOKEN_BYTES];
    rand::thread_rng().fill_bytes(&mut buf);
    let hex = hex_encode(&buf);

    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        opts.mode(0o600);
    }
    let mut f = opts
        .open(path)
        .with_context(|| format!("create token file {}", path.display()))?;
    f.write_all(hex.as_bytes())
        .with_context(|| format!("write token file {}", path.display()))?;
    f.flush().ok();
    drop(f);
    Ok(hex)
}

/// Load an existing token file. Caller must have already verified
/// `path.exists()`.
pub fn load_existing(path: &Path) -> Result<String> {
    let mut f =
        std::fs::File::open(path).with_context(|| format!("open token file {}", path.display()))?;
    let mut s = String::new();
    f.read_to_string(&mut s)
        .with_context(|| format!("read token file {}", path.display()))?;
    let trimmed = s.trim().to_string();
    if trimmed.is_empty() {
        return Err(anyhow!(
            "token file {} is empty; remove it to regenerate",
            path.display(),
        ));
    }

    #[cfg(unix)]
    {
        match std::fs::metadata(path) {
            Ok(meta) => {
                let mode = meta.permissions().mode() & 0o777;
                if mode != 0o600 {
                    eprintln!(
                        "ccteam web: WARNING token file {} has mode {:o}, expected 600. Fix with: chmod 600 {}",
                        path.display(),
                        mode,
                        path.display(),
                    );
                }
            }
            Err(err) => {
                tracing::warn!(?err, "could not stat token file");
            }
        }
    }

    Ok(trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn generate_creates_file_with_64_hex_chars() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("web-token");
        let tok = generate_or_load_token(&path).unwrap();
        assert_eq!(tok.len(), 64, "32 bytes = 64 hex chars");
        assert!(tok.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn generated_file_is_mode_0600() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("web-token");
        generate_or_load_token(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "fresh token file must be mode 600");
    }

    #[test]
    fn second_call_loads_existing_token() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("web-token");
        let first = generate_or_load_token(&path).unwrap();
        let second = generate_or_load_token(&path).unwrap();
        assert_eq!(first, second, "load is idempotent for existing file");
    }

    #[test]
    fn load_existing_trims_whitespace() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("web-token");
        std::fs::write(&path, "abcd1234\n").unwrap();
        let tok = load_existing(&path).unwrap();
        assert_eq!(tok, "abcd1234");
    }

    #[test]
    fn load_existing_errors_on_empty_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("web-token");
        std::fs::write(&path, "   \n").unwrap();
        let err = load_existing(&path).unwrap_err();
        assert!(format!("{err:#}").contains("empty"));
    }

    #[test]
    fn delete_then_regenerate_changes_token() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("web-token");
        let first = generate_or_load_token(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        let second = generate_or_load_token(&path).unwrap();
        assert_ne!(first, second, "regenerated token must differ");
    }
}
