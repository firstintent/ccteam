//! v0.8.6 — generic pull-based hot-reload wrapper for on-disk config.
//!
//! ccteam's architecture is **"no file-watch"** (the orchestrator tick is
//! retired; the filesystem is a state plane, not a push-based control plane —
//! CLAUDE.md §三). So config hot-reload is *pull-based*: every [`HotConfig::get`]
//! stats the watched file and re-parses only when its mtime changed since the
//! last read. That's cheap (one `stat` per access), watcher-free (no inotify
//! pressure on WSL / NAS hosts), and never stale. Wrap any config file in one
//! and every reader picks up edits without a daemon restart — and the same
//! primitive serves future config (budgets, preferences, workflow.yaml, …),
//! not just the projects registry.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use anyhow::Result;

/// A config value backed by a file, re-parsed lazily on mtime change.
///
/// Construct with [`HotConfig::new`] (a watched file + a loader closure that
/// captures whatever the parse needs, e.g. the ccteam root). Read with
/// [`HotConfig::get`], which returns an `Arc<T>` shared across readers.
pub struct HotConfig<T> {
    /// File whose mtime gates a re-parse (e.g. `~/.ccteam/config.yaml`).
    watch_path: PathBuf,
    /// Parse the current value. Captures what the parse needs (e.g. the root),
    /// so callers aren't tied to a fixed loader signature.
    load: Box<dyn Fn() -> Result<T> + Send + Sync>,
    /// The last parse and what it was keyed on; `None` until the first
    /// successful read.
    cached: Mutex<Option<CacheEntry<T>>>,
}

/// One cached parse. `mtime`+`len` identify the file contents; `read_at` is
/// when we parsed, and it is what makes a same-tick rewrite visible.
struct CacheEntry<T> {
    mtime: SystemTime,
    len: u64,
    read_at: SystemTime,
    value: Arc<T>,
}

impl<T> HotConfig<T> {
    /// Watch `watch_path`; parse via `load`. `load` typically reads through a
    /// higher-level loader (capturing the root) while `watch_path` is the
    /// concrete file whose mtime drives invalidation.
    pub fn new(
        watch_path: impl Into<PathBuf>,
        load: impl Fn() -> Result<T> + Send + Sync + 'static,
    ) -> Self {
        Self {
            watch_path: watch_path.into(),
            load: Box::new(load),
            cached: Mutex::new(None),
        }
    }

    /// The current value, re-parsed only if the file changed since last read.
    ///
    /// When the file is missing its mtime is unavailable, so `load` runs every
    /// call (it should return a sensible default); once the file exists, reads
    /// are served from cache until its mtime advances.
    ///
    /// A cache keyed on mtime ALONE cannot see a rewrite that lands inside the
    /// filesystem's timestamp granularity: the second write keeps the mtime the
    /// first one had, and the stale parse is served until some later edit moves
    /// the clock — an operator's config change silently ignored. Two guards
    /// close that: the file's length is part of the key, and a parse whose read
    /// was not strictly newer than the mtime it saw is never reused (the same
    /// "racily clean" rule git applies to its index). The cost is one extra
    /// parse for a file touched in the same tick as the read.
    pub fn get(&self) -> Result<Arc<T>> {
        let stat = std::fs::metadata(&self.watch_path)
            .and_then(|meta| meta.modified().map(|mtime| (mtime, meta.len())))
            .ok();
        let mut guard = self.cached.lock().unwrap_or_else(|e| e.into_inner());
        if let (Some((mtime, len)), Some(entry)) = (stat, guard.as_ref()) {
            if entry.mtime == mtime && entry.len == len && entry.mtime < entry.read_at {
                return Ok(Arc::clone(&entry.value));
            }
        }
        let read_at = SystemTime::now();
        let value = Arc::new((self.load)()?);
        if let Some((mtime, len)) = stat {
            *guard = Some(CacheEntry {
                mtime,
                len,
                read_at,
                value: Arc::clone(&value),
            });
        }
        Ok(value)
    }

    /// The watched file path.
    pub fn watch_path(&self) -> &Path {
        &self.watch_path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tempfile::TempDir;

    /// Re-parse happens only when the file's mtime advances; an unchanged file
    /// is served from cache (the loader runs once).
    #[test]
    fn reparses_only_on_mtime_change() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("c.txt");
        let calls = Arc::new(AtomicUsize::new(0));
        let hot = {
            let p = path.clone();
            let calls = Arc::clone(&calls);
            HotConfig::new(path.clone(), move || {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok::<_, anyhow::Error>(std::fs::read_to_string(&p).unwrap_or_default())
            })
        };

        std::fs::write(&path, "v1").unwrap();
        set_mtime(&path, 1_000);
        assert_eq!(hot.get().unwrap().as_str(), "v1");
        assert_eq!(hot.get().unwrap().as_str(), "v1");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "unchanged mtime → parsed once"
        );

        // Edit + advance mtime explicitly (avoids coarse-clock flakiness).
        std::fs::write(&path, "v2").unwrap();
        set_mtime(&path, 2_000);
        assert_eq!(
            hot.get().unwrap().as_str(),
            "v2",
            "mtime advanced → re-parsed"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    /// A missing file falls back to the loader (which returns a default).
    #[test]
    fn missing_file_uses_loader() {
        let tmp = TempDir::new().unwrap();
        let hot = HotConfig::new(tmp.path().join("nope.txt"), || {
            Ok::<_, anyhow::Error>("default".to_string())
        });
        assert_eq!(hot.get().unwrap().as_str(), "default");
    }

    fn set_mtime(path: &Path, epoch_secs: u64) {
        OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(epoch_secs))
            .unwrap();
    }
}
