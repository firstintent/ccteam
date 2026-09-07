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
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};

/// Write `bytes` to `path` durably: write to a private sibling tmp file,
/// `fsync` that tmp file, `rename` it over `path`, then best-effort `fsync` the
/// parent directory (ignoring errors — some platforms/filesystems reject a
/// bare directory fsync).
///
/// **Two concurrent writers of the same target are safe.** The tmp name used to
/// be `<file>.tmp` for everybody, so a second writer truncated the first one's
/// tmp file and whichever renamed second failed with `ENOENT` — an error the
/// caller reported as a failed write of a file that was, in fact, fine. Every
/// call now mints its own tmp name (pid + a process-monotonic counter), so the
/// only thing two writers can race over is which one's `rename` lands last,
/// which is the semantics `rename` is chosen for. Serializing writers is still
/// the caller's job when the CONTENT is a read-modify-write; this helper only
/// guarantees no writer corrupts another's staging file.
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

/// Read an append-only JSONL log (`progress.jsonl` / `turns.jsonl` /
/// `experience.jsonl`) into `T`s. **Damage is per LINE, never per file**:
/// absent file ⇒ empty, and any line that does not deserialize — blank,
/// half-flushed, older-shape, or not even valid UTF-8 — is skipped while every
/// intact line survives.
///
/// This is the read-side counterpart of the append side documented above: those
/// logs are deliberately written with best-effort `O_APPEND` and no fsync, so an
/// interrupted write CAN leave a partial line, including one cut mid-multi-byte
/// character. Reading such a log via `read_to_string` makes one bad byte
/// anywhere fail the whole read, and callers of these logs degrade a read error
/// to "no records" — which is how a single torn byte in a 120 MB
/// `progress.jsonl` made every live session of a project report `idle` and its
/// cost roll-up report `$0` (seen in the wild 2026-08-08). One reader, one
/// tolerance rule, so no log reader has to remember this.
pub fn read_jsonl<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Vec<T>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let mut out = Vec::new();
    for line in bytes.split(|byte| *byte == b'\n') {
        // serde rejects invalid UTF-8 (torn append) exactly as it rejects
        // invalid JSON (half-flushed line) — and an empty line with "EOF while
        // parsing a value", so blank lines need no special case.
        if let Ok(record) = serde_json::from_slice::<T>(line) {
            out.push(record);
        }
    }
    Ok(out)
}

/// Per-process tmp-name counter. Paired with the pid it makes every staging
/// file unique across threads AND across processes sharing one directory.
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// A private tmp sibling of `path`, in the same directory (so the `rename` is
/// same-filesystem and therefore atomic). Suffixed rather than
/// `with_extension`-ed, which would eat the last extension of an
/// extensionless-looking name (e.g. `state/sessions/next-sid`).
fn sibling_tmp_path(path: &Path) -> std::path::PathBuf {
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    path.with_file_name(format!("{file_name}.{}.{seq:x}.tmp", std::process::id()))
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

        // No leftover staging file after a successful write.
        assert_eq!(tmp_files(dir.path()), Vec::<String>::new());
    }

    /// Two writers of ONE target never share a staging file. Before this,
    /// every call staged through `<file>.tmp`: the second writer truncated the
    /// first one's tmp and whichever renamed second failed
    /// `rename …tmp -> …: No such file or directory`, so a perfectly good
    /// write was reported as a failure (seen on `delegation.json` under two
    /// concurrent dispatches). Whichever content wins is the caller's problem
    /// to serialize; NOT losing the write to a shared temp name is this
    /// helper's.
    #[test]
    fn concurrent_writers_of_one_target_never_share_a_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("delegation.json");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let failures = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        std::thread::scope(|scope| {
            for writer in 0..8u32 {
                let path = path.clone();
                let barrier = std::sync::Arc::clone(&barrier);
                let failures = std::sync::Arc::clone(&failures);
                scope.spawn(move || {
                    let body = format!("{{\"writer\":{writer}}}");
                    barrier.wait();
                    for _ in 0..40 {
                        if let Err(error) = atomic_write_durable(&path, body.as_bytes()) {
                            failures.lock().unwrap().push(format!("{error:#}"));
                        }
                    }
                });
            }
        });
        assert_eq!(
            failures.lock().unwrap().as_slice(),
            Vec::<String>::new(),
            "no writer may fail because another was staging the same target"
        );
        // The target holds exactly one writer's body, whole — never a mix.
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(
            (0..8).any(|writer| body == format!("{{\"writer\":{writer}}}")),
            "torn content: {body}"
        );
        assert_eq!(
            tmp_files(dir.path()),
            Vec::<String>::new(),
            "every staging file is cleaned up by its own rename"
        );
    }

    /// Staging files left behind in `dir`, by name.
    fn tmp_files(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        names.sort();
        names
    }

    /// One torn append costs ONE line. The fixture is the real-world shape: a
    /// write cut mid-multi-byte-character with the next record appended right
    /// behind it, no newline between them.
    #[test]
    fn read_jsonl_drops_only_the_torn_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log.jsonl");
        let mut raw = Vec::new();
        raw.extend_from_slice(br#"{"n":1}"#);
        raw.push(b'\n');
        raw.extend_from_slice("{\"note\":\"配置\u{ff1a}".as_bytes());
        raw.truncate(raw.len() - 2); // cut mid-character
        raw.extend_from_slice(br#"{"n":2}"#);
        raw.push(b'\n');
        raw.extend_from_slice(br#"{"n":3}"#);
        raw.push(b'\n');
        std::fs::write(&path, &raw).unwrap();
        assert!(
            String::from_utf8(raw).is_err(),
            "fixture must actually be invalid UTF-8"
        );

        #[derive(serde::Deserialize)]
        struct Row {
            n: u32,
        }
        let rows: Vec<Row> = read_jsonl(&path).expect("a torn line is not a read failure");
        assert_eq!(rows.iter().map(|r| r.n).collect::<Vec<_>>(), vec![1, 3]);
    }

    #[test]
    fn read_jsonl_absent_file_is_empty_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let rows: Vec<serde_json::Value> = read_jsonl(&dir.path().join("nope.jsonl")).unwrap();
        assert!(rows.is_empty());
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
