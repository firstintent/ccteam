//! Durable atomic file writes for the handful of small, critical
//! state-of-truth files whose rollback on power loss would break an
//! invariant (most notably `state/sessions/next-sid`: a rollback there
//! means sid reuse).
//!
//! Plain tmp+rename (as used elsewhere for e.g. `turns.jsonl` / hub cache)
//! survives a *process crash* mid-write (never a half-written file) but not
//! a *power loss*: the rename can itself be lost by the filesystem's
//! write-back cache, rolling the target back to its previous contents.
//! [`atomic_write_durable`] closes that gap for the few files that need it
//! by fsync-ing the tmp file before the rename and best-effort fsync-ing
//! the parent directory after (directory fsync makes the rename's directory
//! entry update durable too; some platforms/filesystems reject or no-op
//! this, so failures there are ignored).
//!
//! Deliberately NOT used for high-frequency appenders (`turns.jsonl`,
//! `progress.jsonl`, hub cache, transcript cursor): those stay best-effort
//! tmp+rename / `O_APPEND`, per the "no fsync on hot append paths" call in
//! the durability review.

use std::fs::File;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};

/// Write `bytes` to `path` durably: write to a sibling `.tmp` file, `fsync`
/// that tmp file, `rename` it over `path`, then best-effort `fsync` the
/// parent directory (ignoring errors — some platforms/filesystems reject a
/// bare directory fsync).
///
/// Caller is responsible for ensuring `path`'s parent directory exists.
pub fn atomic_write_durable(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = sibling_tmp_path(path);

    let mut file =
        File::create(&tmp).with_context(|| format!("create tmp file {}", tmp.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("write tmp file {}", tmp.display()))?;
    file.sync_all()
        .with_context(|| format!("fsync tmp file {}", tmp.display()))?;
    drop(file);

    std::fs::rename(&tmp, path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;

    // Best-effort: make the rename's directory-entry update durable too.
    // Ignored on failure — not all platforms/filesystems support fsync on a
    // directory (e.g. some older Windows/FAT setups), and this is
    // defense-in-depth on top of the fsync'd tmp file + rename above, not
    // the sole durability guarantee.
    if let Some(parent) = path.parent() {
        if let Ok(dir) = File::open(parent) {
            let _ = dir.sync_all();
        }
    }

    Ok(())
}

/// A `.tmp` sibling of `path` that doesn't collide with `with_extension`'s
/// "replace the last extension" behavior on extensionless files (e.g.
/// `state/sessions/next-sid`).
fn sibling_tmp_path(path: &Path) -> std::path::PathBuf {
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!("{file_name}.tmp"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_and_replaces_target_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("next-sid");

        atomic_write_durable(&path, b"1").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "1");

        atomic_write_durable(&path, b"2").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "2");

        // No leftover tmp file after a successful write.
        assert!(!dir.path().join("next-sid.tmp").exists());
    }

    #[test]
    fn works_on_extensionless_paths() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("next-sid");
        atomic_write_durable(&path, b"42").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "42");
    }

    #[test]
    fn works_on_dotted_paths() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("meta.json");
        atomic_write_durable(&path, b"{}").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{}");
    }
}
