//! V0.3 M5.2 — file watcher → broadcast pump.
//!
//! Architecture (per `docs/v0-3/prd.md` §5.2.1-§5.2.2):
//!
//! ```text
//! ~/.ccteam/progress/<slug>.jsonl    (fs)
//!         │
//!         ▼
//! notify::RecommendedWatcher          (one instance, recursive)
//!         │  (Modify / Create events)
//!         ▼
//! per-file watermark map              (HashMap<PathBuf, u64> behind Mutex)
//!         │  (read appended bytes, parse one JSON object per line)
//!         ▼
//! tokio::sync::broadcast::Sender<ProgressUpdate>
//!         │  (capacity = 1024)
//!         ├──► /sse/all subscriber
//!         └──► /sse/project/<slug> subscriber
//! ```
//!
//! Architectural red lines (CLAUDE.md §三 + PRD §5.2):
//!
//! - `progress.jsonl` is the single source of truth — we **read** the
//!   file produced by `ccteam_core::progress::*`. We never tail tmux
//!   output or shell out to `tail -f`.
//! - The watcher is **single instance, single recursive subscription**
//!   on `~/.ccteam/progress/`. New `<slug>.jsonl` files surface as
//!   `Create` events under the same recursive watch — no per-file
//!   re-arm.
//! - Initial state on watcher start: existing files have their
//!   watermark seeded to **current size** so connecting clients see
//!   only events from connect-time forward (M5.4 retro replay is
//!   deferred).
//! - Channel capacity = `1024` literal (PRD §5.2.2 + dev-plan §8 grep).
//!   On overflow, lagging consumers receive `RecvError::Lagged` and
//!   the SSE handler emits a `reconnect_hint` synthetic event before
//!   closing the stream. The watcher never blocks on the broadcast.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde_json::Value;
use tokio::sync::broadcast;

/// Capacity of the broadcast channel that fans events out from the
/// single notify watcher to the subscribed SSE handlers. PRD §5.2.2 +
/// dev-plan §8 red-line grep both pin this literal at 1024.
pub const BROADCAST_CAPACITY: usize = 1024;

/// One progress.jsonl line, ready for SSE serialization.
///
/// `event_json` is the raw line as written by the orchestrator
/// (already a single-line JSON object — progress.jsonl is JSON-Lines
/// by spec — so SSE wire format is one `data:` line per `EventMsg`).
/// `slug` is parsed from the file name and injected for client-side
/// fan-out / dashboard correlation.
#[derive(Clone, Debug)]
pub struct ProgressUpdate {
    pub slug: String,
    /// Owned single-line JSON string. Already trimmed of trailing
    /// `\n`. Always parseable as a JSON object — invalid lines are
    /// dropped at the watcher with a `tracing::warn!` rather than
    /// propagated, so SSE consumers don't have to defensive-parse.
    pub event_json: String,
}

/// Handle to the broadcast Sender — held by [`AppState`] and
/// `subscribe()`d by SSE handlers. Drops the sender → background task
/// exits naturally on next iteration when no subscribers remain.
#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<ProgressUpdate>,
}

impl EventBus {
    pub fn subscribe(&self) -> broadcast::Receiver<ProgressUpdate> {
        self.tx.subscribe()
    }

    /// Construct an inert bus with no producer task. Used as a
    /// fallback when watcher startup fails so SSE handlers still
    /// return a well-formed (but empty) stream — clients see no
    /// events and the page still renders.
    pub fn inert() -> Self {
        let (tx, _rx) = broadcast::channel::<ProgressUpdate>(BROADCAST_CAPACITY);
        EventBus { tx }
    }

    /// Test helper for unit tests + integration tests. Not a stable
    /// API surface — `#[doc(hidden)]` so docs.rs hides it. Tests
    /// (in this crate's `tests/*.rs` and other internal callers)
    /// use it to push synthetic events without spinning a notify
    /// watcher (avoids inotify timing flake).
    #[doc(hidden)]
    pub fn publish_synthetic(&self, msg: ProgressUpdate) {
        let _ = self.tx.send(msg);
    }
}

/// Spawn the watcher background task. Returns the bus handle — the
/// returned value MUST be retained by `AppState` for the lifetime of
/// the server (drop → broadcast Sender goes away → in-flight SSE
/// streams close naturally).
///
/// The watcher itself runs in a dedicated `tokio::task::spawn_blocking`
/// thread because `notify::RecommendedWatcher` callbacks are invoked
/// from the notify thread and may block while reading the file — we
/// don't want that on a tokio worker.
pub fn spawn_watcher(progress_dir: PathBuf) -> Result<EventBus> {
    // Ensure the dir exists; notify::watch fails on a missing path.
    std::fs::create_dir_all(&progress_dir)
        .with_context(|| format!("create progress dir {}", progress_dir.display()))?;

    let (tx, _rx) = broadcast::channel::<ProgressUpdate>(BROADCAST_CAPACITY);
    let bus = EventBus { tx: tx.clone() };

    // Seed watermarks for files already present at startup so we do
    // NOT replay history (PRD §5.2.1 — connecting clients see only
    // events from connect-time forward).
    let watermarks = Arc::new(std::sync::Mutex::new(initial_watermarks(&progress_dir)));

    // Spawn a dedicated OS thread to host the notify watcher. The
    // watcher's callback runs synchronously and may do small file
    // reads; keeping it off the tokio runtime avoids accidental
    // blocking on async workers.
    let progress_dir_for_thread = progress_dir.clone();
    std::thread::Builder::new()
        .name("ccteam-web-progress-watcher".into())
        .spawn(move || {
            run_watcher_thread(progress_dir_for_thread, tx, watermarks);
        })
        .context("spawn ccteam-web watcher thread")?;

    Ok(bus)
}

/// Body of the dedicated watcher thread. Lives until the broadcast
/// Sender is dropped (i.e. server shutdown drops `AppState`).
fn run_watcher_thread(
    progress_dir: PathBuf,
    tx: broadcast::Sender<ProgressUpdate>,
    watermarks: Arc<std::sync::Mutex<HashMap<PathBuf, u64>>>,
) {
    // Use a std::sync::mpsc to ferry events from the notify thread
    // into our blocking loop. notify creates its own thread; we just
    // own the receiver here.
    let (event_tx, event_rx) = std::sync::mpsc::channel::<notify::Result<notify::Event>>();
    let mut watcher: RecommendedWatcher = match notify::recommended_watcher(move |res| {
        // Best-effort send; if our receiver has been dropped we're
        // shutting down anyway.
        let _ = event_tx.send(res);
    }) {
        Ok(w) => w,
        Err(err) => {
            tracing::error!(?err, "ccteam-web: notify::recommended_watcher init failed");
            return;
        }
    };
    if let Err(err) = watcher.watch(&progress_dir, RecursiveMode::Recursive) {
        tracing::error!(
            ?err,
            dir = %progress_dir.display(),
            "ccteam-web: notify::watch failed",
        );
        return;
    }

    tracing::info!(
        dir = %progress_dir.display(),
        "ccteam-web: progress watcher started",
    );

    while let Ok(res) = event_rx.recv() {
        match res {
            Ok(ev) => handle_fs_event(ev, &tx, &watermarks),
            Err(err) => tracing::warn!(?err, "ccteam-web: notify watcher reported error"),
        }
        // If there are no subscribers AND the broadcast Sender's
        // ref-count is 1 (just our copy), AppState has been dropped.
        // We'd race here in a real shutdown but the dedicated thread
        // tolerates a hung recv — process exit cleans it up.
    }
}

fn handle_fs_event(
    ev: notify::Event,
    tx: &broadcast::Sender<ProgressUpdate>,
    watermarks: &Arc<std::sync::Mutex<HashMap<PathBuf, u64>>>,
) {
    // Only Create / Modify on `.jsonl` files matter. Notify's recursive
    // watch fires both for the directory itself and for individual
    // file mutations; we filter on path suffix.
    let interesting = matches!(ev.kind, EventKind::Modify(_) | EventKind::Create(_));
    if !interesting {
        return;
    }
    for path in ev.paths {
        if !is_progress_file(&path) {
            continue;
        }
        if let Err(err) = drain_new_lines(&path, tx, watermarks) {
            tracing::warn!(file = %path.display(), error = %err, "drain progress lines failed");
        }
    }
}

fn is_progress_file(path: &Path) -> bool {
    path.extension().and_then(|s| s.to_str()) == Some("jsonl")
        && path.file_stem().and_then(|s| s.to_str()).is_some()
}

/// Read whatever has been appended since the last watermark. New
/// files (no watermark entry) start at offset 0 — they were created
/// AFTER the watcher started, so the user actively wants their
/// initial bytes (they aren't "history").
fn drain_new_lines(
    path: &Path,
    tx: &broadcast::Sender<ProgressUpdate>,
    watermarks: &Arc<std::sync::Mutex<HashMap<PathBuf, u64>>>,
) -> Result<()> {
    let slug = match path.file_stem().and_then(|s| s.to_str()) {
        Some(s) => s.to_string(),
        None => {
            // Defensive — `is_progress_file` already filtered, but a
            // weird path (`.jsonl` with no stem) shouldn't crash.
            return Ok(());
        }
    };

    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            // File was removed between fs event and open. Drop the
            // watermark so a future Create starts at 0.
            watermarks.lock().unwrap().remove(path);
            return Ok(());
        }
        Err(err) => return Err(err).context("open progress file"),
    };
    let metadata = file.metadata().context("stat progress file")?;
    let size = metadata.len();

    let mut prev = {
        let mut m = watermarks.lock().unwrap();
        // `entry(path).or_insert(0)` — first time we see this file
        // (Create event), we start at 0 so the appended bytes flush.
        *m.entry(path.to_path_buf()).or_insert(0)
    };

    if size < prev {
        // File rotated / truncated. Replay from start. Rare; log it.
        tracing::warn!(
            file = %path.display(),
            old_offset = prev,
            new_size = size,
            "progress file shrank — resetting watermark to 0",
        );
        prev = 0;
    }

    if size == prev {
        return Ok(());
    }

    // Seek to the previous offset and read the appended chunk into a
    // String. progress.jsonl is plain UTF-8 JSON-Lines; non-UTF8 means
    // the writer is misbehaving, which we surface as a warn + skip.
    file.seek(SeekFrom::Start(prev))
        .context("seek progress file to watermark")?;
    let mut buf = Vec::with_capacity((size - prev) as usize);
    file.read_to_end(&mut buf).context("read progress tail")?;

    // Update watermark eagerly; even if line parsing partially fails
    // we don't want to re-replay the same bytes on the next event.
    watermarks.lock().unwrap().insert(path.to_path_buf(), size);

    let chunk = match std::str::from_utf8(&buf) {
        Ok(s) => s,
        Err(err) => {
            tracing::warn!(
                file = %path.display(),
                error = %err,
                "non-UTF8 in progress tail; skipping chunk",
            );
            return Ok(());
        }
    };

    for line in chunk.split('\n') {
        let line = line.trim_end_matches('\r').trim();
        if line.is_empty() {
            continue;
        }
        // Validate it parses as a JSON object before broadcasting so
        // SSE clients always receive well-formed `data:` payloads.
        if serde_json::from_str::<Value>(line).is_err() {
            tracing::warn!(
                file = %path.display(),
                line_preview = &line[..line.len().min(120)],
                "skipping unparseable progress line",
            );
            continue;
        }
        let msg = ProgressUpdate {
            slug: slug.clone(),
            event_json: line.to_string(),
        };
        // `send` returns Err if there are no subscribers, which is
        // fine — we just keep our watermark so the next subscriber
        // catches up from the latest bytes.
        let _ = tx.send(msg);
    }

    Ok(())
}

/// Seed watermarks at the current file size for every existing
/// `<slug>.jsonl` so connecting clients don't see history.
fn initial_watermarks(progress_dir: &Path) -> HashMap<PathBuf, u64> {
    let mut out = HashMap::new();
    let entries = match std::fs::read_dir(progress_dir) {
        Ok(e) => e,
        Err(_) => return out,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !is_progress_file(&path) {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            out.insert(path, meta.len());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn is_progress_file_accepts_jsonl() {
        assert!(is_progress_file(Path::new("/x/dev-foo.jsonl")));
        assert!(!is_progress_file(Path::new("/x/dev-foo.json")));
        assert!(!is_progress_file(Path::new("/x/.jsonl"))); // no stem
        assert!(!is_progress_file(Path::new("/x/dev-foo.md")));
    }

    #[test]
    fn initial_watermarks_seeds_existing_files() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("dev-foo.jsonl");
        std::fs::write(&f, b"{}\n{}\n").unwrap();
        let wm = initial_watermarks(tmp.path());
        assert_eq!(wm.get(&f).copied(), Some(6));
    }

    #[test]
    fn drain_new_lines_emits_appended_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("dev-foo.jsonl");
        std::fs::write(&f, b"").unwrap();

        let (tx, mut rx) = broadcast::channel::<ProgressUpdate>(BROADCAST_CAPACITY);
        let wm = Arc::new(std::sync::Mutex::new(HashMap::new()));

        // Append two events.
        let mut handle = std::fs::OpenOptions::new().append(true).open(&f).unwrap();
        handle
            .write_all(b"{\"event\":\"phase_inject\",\"phase\":\"plan-eng\"}\n")
            .unwrap();
        handle
            .write_all(b"{\"event\":\"PostToolUse\",\"tool\":\"Read\"}\n")
            .unwrap();
        drop(handle);

        drain_new_lines(&f, &tx, &wm).unwrap();
        let m1 = rx.try_recv().unwrap();
        let m2 = rx.try_recv().unwrap();
        assert_eq!(m1.slug, "dev-foo");
        assert!(m1.event_json.contains("phase_inject"));
        assert_eq!(m2.slug, "dev-foo");
        assert!(m2.event_json.contains("PostToolUse"));
        assert!(rx.try_recv().is_err());

        // Watermark advanced past file end.
        let wm_after = *wm.lock().unwrap().get(&f).unwrap();
        assert_eq!(wm_after, std::fs::metadata(&f).unwrap().len());
    }

    #[test]
    fn drain_new_lines_skips_unparseable_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("dev-bar.jsonl");
        std::fs::write(&f, b"not json\n{\"event\":\"good\"}\n").unwrap();

        let (tx, mut rx) = broadcast::channel::<ProgressUpdate>(BROADCAST_CAPACITY);
        let wm = Arc::new(std::sync::Mutex::new(HashMap::new()));
        drain_new_lines(&f, &tx, &wm).unwrap();
        let m = rx.try_recv().unwrap();
        assert_eq!(m.slug, "dev-bar");
        assert!(m.event_json.contains("good"));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn drain_new_lines_resets_on_truncation() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("dev-rotate.jsonl");
        std::fs::write(&f, b"{\"event\":\"a\"}\n{\"event\":\"b\"}\n").unwrap();

        let (tx, mut rx) = broadcast::channel::<ProgressUpdate>(BROADCAST_CAPACITY);
        let wm = Arc::new(std::sync::Mutex::new(HashMap::new()));
        // Pre-seed watermark to file end (mirrors "started after
        // existing data" path).
        let len = std::fs::metadata(&f).unwrap().len();
        wm.lock().unwrap().insert(f.clone(), len);

        // Truncate + write a smaller payload.
        std::fs::write(&f, b"{\"event\":\"c\"}\n").unwrap();
        drain_new_lines(&f, &tx, &wm).unwrap();
        let m = rx.try_recv().unwrap();
        assert!(m.event_json.contains("\"c\""));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn broadcast_capacity_pinned_at_1024() {
        // PRD §5.2.2 + dev-plan §8 grep red line — channel must be
        // bounded at exactly 1024. If a future PR widens this without
        // updating the spec, the grep matrix flag will surface it,
        // but the unit test makes the contract explicit too.
        assert_eq!(BROADCAST_CAPACITY, 1024);
    }
}
