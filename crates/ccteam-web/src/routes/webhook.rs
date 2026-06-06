//! Per-project webhook secret helper.
//!
//! The HTTP `POST /webhook/{project}/{token}` ingress route has been
//! removed (the flow engine will re-introduce a webhook trigger later).
//! What remains is the per-project secret generator/loader, which the
//! CLI's `ccteam show` calls to surface the (stable) webhook URL for the
//! project.
//!
//! The secret is persisted at `<project>/.ccteam/webhook-token`
//! (64 hex chars, mode 0600 — same shape as `~/.ccteam/web-token`).

use std::path::Path;

use rand::RngCore;

/// Generate-or-load the per-project webhook secret. Mirrors
/// `token::generate_or_load_token` (the `~/.ccteam/web-token` flow):
/// 32 random bytes hex-encoded, file created mode 0600 on Unix via
/// `create_new` so a racing writer cannot be clobbered.
pub fn generate_or_load_secret(path: &Path) -> std::io::Result<String> {
    if path.exists() {
        let raw = std::fs::read_to_string(path)?;
        let trimmed = raw.trim().to_string();
        if !trimmed.is_empty() {
            return Ok(trimmed);
        }
        // Empty file (interrupted write) — fall through to regenerate.
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut buf = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut buf);
    let hex = hex_encode(&buf);

    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    match opts.open(path) {
        Ok(mut f) => {
            use std::io::Write;
            f.write_all(hex.as_bytes())?;
            f.flush().ok();
            Ok(hex)
        }
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            // A concurrent request won the create race — read theirs.
            Ok(std::fs::read_to_string(path)?.trim().to_string())
        }
        Err(err) => Err(err),
    }
}

/// Lowercase hex encoding (no extra crate).
fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn generate_secret_is_64_hex_chars_and_idempotent() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join(".ccteam").join("webhook-token");
        let first = generate_or_load_secret(&path).unwrap();
        assert_eq!(first.len(), 64);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
        let second = generate_or_load_secret(&path).unwrap();
        assert_eq!(first, second, "load is idempotent");
    }

    #[cfg(unix)]
    #[test]
    fn generated_secret_file_is_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("webhook-token");
        generate_or_load_secret(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
