//! V0.3 M5.2 — file watcher → broadcast pump.
//!
//! Architecture (per `docs/versions/v0-3/prd.md` §5.2.1-§5.2.2):
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
use ccteam_core::HarnessSnapshot;
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
    /// V0.3.1 F50 — flex session id parsed from
    /// `~/.ccteam/progress/<slug>/<sid>.jsonl`. Legacy workflow
    /// `<slug>.jsonl` streams leave this as `None`.
    pub sid: Option<String>,
    /// Owned single-line JSON string. Already trimmed of trailing
    /// `\n`. Always parseable as a JSON object — invalid lines are
    /// dropped at the watcher with a `tracing::warn!` rather than
    /// propagated, so SSE consumers don't have to defensive-parse.
    pub event_json: String,
}

/// V0.3.1 F46 — one harness `<slug>-<sid>.json` snapshot, ready for
/// SSE serialization. Emitted by the sibling `~/.ccteam/harness/`
/// watcher (see [`spawn_harness_watcher_into`]) onto its own broadcast
/// channel inside [`EventBus`]. Wire format documented in
/// `routes::harness_sse`.
#[derive(Clone, Debug)]
pub struct HarnessSnapshotEvent {
    pub slug: String,
    pub sid: String,
    pub snapshot: HarnessSnapshot,
}

/// Handle to the broadcast Senders — held by [`AppState`] and
/// `subscribe()`d by SSE handlers. Drops the senders → background
/// tasks exit naturally on next iteration when no subscribers remain.
///
/// Two sibling channels (decision documented in PR body): one for
/// progress.jsonl events (M5.2), one for `~/.ccteam/harness/*.json`
/// snapshots (V0.3.1 F46). Sibling channels keep the existing
/// `subscribe()` / `publish_synthetic()` shape stable for the five
/// M5.2 tests + `routes::sse` consumer; switching to an enum would
/// have required signature churn for zero functional gain.
#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<ProgressUpdate>,
    /// V0.3.1 F46 — sibling channel for `harness_snapshot` events.
    harness_tx: broadcast::Sender<HarnessSnapshotEvent>,
}

impl EventBus {
    pub fn subscribe(&self) -> broadcast::Receiver<ProgressUpdate> {
        self.tx.subscribe()
    }

    /// V0.3.1 F46 — subscribe to the sibling harness snapshot
    /// channel. Mirrors [`subscribe`] but delivers
    /// [`HarnessSnapshotEvent`] from the `~/.ccteam/harness/`
    /// watcher.
    pub fn subscribe_harness(&self) -> broadcast::Receiver<HarnessSnapshotEvent> {
        self.harness_tx.subscribe()
    }

    /// Construct an inert bus with no producer task. Used as a
    /// fallback when watcher startup fails so SSE handlers still
    /// return a well-formed (but empty) stream — clients see no
    /// events and the page still renders.
    pub fn inert() -> Self {
        let (tx, _rx) = broadcast::channel::<ProgressUpdate>(BROADCAST_CAPACITY);
        let (harness_tx, _hrx) = broadcast::channel::<HarnessSnapshotEvent>(BROADCAST_CAPACITY);
        EventBus { tx, harness_tx }
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

    /// V0.3.1 F46 — synthetic publish for the harness sibling
    /// channel. Same usage shape as [`publish_synthetic`].
    #[doc(hidden)]
    pub fn publish_harness_synthetic(&self, msg: HarnessSnapshotEvent) {
        let _ = self.harness_tx.send(msg);
    }
}

/// Spawn the watcher background task. Returns the bus handle — the
/// returned value MUST be retained by `AppState` for the lifetime of
/// the server (drop → broadcast Sender goes away → in-flight SSE
/// streams close naturally).
///
/// V0.3.1 F46 spawns a sibling thread for `~/.ccteam/harness/` so a
/// single `AppState::new` call wires up both pumps. Pass
/// `harness_dir = None` (or use [`spawn_progress_watcher_only`]) when
/// the caller only wants progress events (legacy callsite).
pub fn spawn_watcher(progress_dir: PathBuf, harness_dir: PathBuf) -> Result<EventBus> {
    // Ensure both dirs exist; notify::watch fails on a missing path.
    std::fs::create_dir_all(&progress_dir)
        .with_context(|| format!("create progress dir {}", progress_dir.display()))?;
    std::fs::create_dir_all(&harness_dir)
        .with_context(|| format!("create harness dir {}", harness_dir.display()))?;

    let (tx, _rx) = broadcast::channel::<ProgressUpdate>(BROADCAST_CAPACITY);
    let (harness_tx, _hrx) = broadcast::channel::<HarnessSnapshotEvent>(BROADCAST_CAPACITY);
    let bus = EventBus {
        tx: tx.clone(),
        harness_tx: harness_tx.clone(),
    };

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

    // V0.3.1 F46 — sibling thread tailing `~/.ccteam/harness/`.
    let harness_dir_for_thread = harness_dir.clone();
    std::thread::Builder::new()
        .name("ccteam-web-harness-watcher".into())
        .spawn(move || {
            run_harness_watcher_thread(harness_dir_for_thread, harness_tx);
        })
        .context("spawn ccteam-web harness watcher thread")?;

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
    let (slug, sid) = match progress_slug_sid(path) {
        Some(pair) => pair,
        None => {
            // Defensive — `is_progress_file` already filtered, but
            // a weird path shouldn't crash.
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
            sid: sid.clone(),
            event_json: line.to_string(),
        };
        // `send` returns Err if there are no subscribers, which is
        // fine — we just keep our watermark so the next subscriber
        // catches up from the latest bytes.
        let _ = tx.send(msg);
    }

    Ok(())
}

fn progress_slug_sid(path: &Path) -> Option<(String, Option<String>)> {
    let sid_or_slug = path.file_stem()?.to_str()?.to_string();
    let parent_name = path.parent()?.file_name()?.to_str()?;
    if parent_name != "progress" && is_session_sid(&sid_or_slug) {
        Some((parent_name.to_string(), Some(sid_or_slug)))
    } else {
        Some((sid_or_slug, None))
    }
}

fn is_session_sid(value: &str) -> bool {
    let Some((prefix, seq)) = value.rsplit_once('-') else {
        return false;
    };
    matches!(prefix, "claude" | "codex")
        && !seq.is_empty()
        && seq.bytes().all(|b| b.is_ascii_digit())
}

/// Seed watermarks at the current file size for every existing
/// `<slug>.jsonl` so connecting clients don't see history.
fn initial_watermarks(progress_dir: &Path) -> HashMap<PathBuf, u64> {
    let mut out = HashMap::new();
    let mut stack = vec![progress_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                stack.push(path);
            } else if is_progress_file(&path) {
                out.insert(path, meta.len());
            }
        }
    }
    out
}

// =====================================================================
// V0.3.1 F46 — harness snapshot watcher
// =====================================================================
//
// Sibling pump tailing `~/.ccteam/harness/<slug>-<sid>.json`. On each
// `Modify` / `Create` event we read the file once, parse as
// `HarnessSnapshot`, and broadcast a `HarnessSnapshotEvent` on the
// dedicated harness channel.
//
// Files come in two filename shapes (mirrors `derive_harness_path`):
//   - `<slug>-<sid>.json`   — sid = `claude-N` or `codex-N`
//   - `_meta-<handle>.json` — meta-agent project (slug = full stem,
//                              sid = "default")
//
// Splitting `dev-foo-claude-1.json` into ("dev-foo", "claude-1") needs
// a rule because both halves contain hyphens. We match the trailing
// `(claude|codex)-\d+` suffix; anything else (older static-fixtured
// filename, future harness type) is dropped with a `tracing::warn!`.

fn run_harness_watcher_thread(harness_dir: PathBuf, tx: broadcast::Sender<HarnessSnapshotEvent>) {
    let (event_tx, event_rx) = std::sync::mpsc::channel::<notify::Result<notify::Event>>();
    let mut watcher: RecommendedWatcher = match notify::recommended_watcher(move |res| {
        let _ = event_tx.send(res);
    }) {
        Ok(w) => w,
        Err(err) => {
            tracing::error!(
                ?err,
                "ccteam-web: harness notify::recommended_watcher init failed"
            );
            return;
        }
    };
    if let Err(err) = watcher.watch(&harness_dir, RecursiveMode::NonRecursive) {
        tracing::error!(
            ?err,
            dir = %harness_dir.display(),
            "ccteam-web: harness notify::watch failed",
        );
        return;
    }

    tracing::info!(
        dir = %harness_dir.display(),
        "ccteam-web: harness watcher started",
    );

    while let Ok(res) = event_rx.recv() {
        match res {
            Ok(ev) => handle_harness_fs_event(ev, &tx),
            Err(err) => tracing::warn!(?err, "ccteam-web: harness watcher reported error"),
        }
    }
}

fn handle_harness_fs_event(ev: notify::Event, tx: &broadcast::Sender<HarnessSnapshotEvent>) {
    let interesting = matches!(ev.kind, EventKind::Modify(_) | EventKind::Create(_));
    if !interesting {
        return;
    }
    for path in ev.paths {
        if !is_harness_snapshot_file(&path) {
            continue;
        }
        if let Err(err) = publish_harness_snapshot(&path, tx) {
            tracing::warn!(
                file = %path.display(),
                error = %err,
                "publish harness snapshot failed",
            );
        }
    }
}

fn is_harness_snapshot_file(path: &Path) -> bool {
    if path.extension().and_then(|s| s.to_str()) != Some("json") {
        return false;
    }
    // Skip any `<name>.tmp` rename source produced by atomic file writers
    // (F68 / future harness observer paths still use the canonical tmp+rename
    // pattern even after F61 retired the V0.3.1 `write_harness_snapshot`).
    let name = match path.file_name().and_then(|s| s.to_str()) {
        Some(n) => n,
        None => return false,
    };
    if name.ends_with(".tmp") {
        return false;
    }
    true
}

fn publish_harness_snapshot(
    path: &Path,
    tx: &broadcast::Sender<HarnessSnapshotEvent>,
) -> Result<()> {
    let stem = match path.file_stem().and_then(|s| s.to_str()) {
        Some(s) => s.to_string(),
        None => return Ok(()),
    };
    let (slug, sid) = match split_harness_stem(&stem) {
        Some(pair) => pair,
        None => {
            tracing::warn!(
                file = %path.display(),
                stem = %stem,
                "harness file stem did not match `<slug>-<claude|codex>-N` or `_meta-<handle>` — skipping",
            );
            return Ok(());
        }
    };
    let body = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            // Race with rename or unlink — drop quietly.
            return Ok(());
        }
        Err(err) => return Err(err).context("read harness snapshot"),
    };
    let snapshot: HarnessSnapshot = match serde_json::from_str(&body) {
        Ok(s) => s,
        Err(err) => {
            tracing::warn!(
                file = %path.display(),
                error = %err,
                "harness snapshot did not parse as HarnessSnapshot — skipping",
            );
            return Ok(());
        }
    };
    let _ = tx.send(HarnessSnapshotEvent {
        slug,
        sid,
        snapshot,
    });
    Ok(())
}

/// Split a harness file stem into `(slug, sid)`.
///
/// Match rules (mirrors the writer in `ccteam_core::harness`):
/// - `_meta-<handle>` → `("_meta-<handle>", "default")` (meta-agent project,
///   single session, no real sid)
/// - `<slug>-<claude|codex>-N` → `(<slug>, <claude|codex>-N)`
/// - anything else → `None` (drop event)
pub(crate) fn split_harness_stem(stem: &str) -> Option<(String, String)> {
    if stem.starts_with("_meta-") {
        return Some((stem.to_string(), "default".to_string()));
    }
    // Search right-to-left for the first hyphen that separates the
    // last `claude|codex-N` group from the slug.
    let bytes = stem.as_bytes();
    let mut tail_start = bytes.len();
    // Walk back through digits...
    while tail_start > 0 && bytes[tail_start - 1].is_ascii_digit() {
        tail_start -= 1;
    }
    if tail_start == bytes.len() || tail_start == 0 || bytes[tail_start - 1] != b'-' {
        return None;
    }
    // ...consume the `-` separating digits from the harness keyword
    let kw_end = tail_start - 1;
    // The keyword must be `claude` or `codex`.
    for kw in ["claude", "codex"] {
        let kw_start = kw_end.checked_sub(kw.len())?;
        if &stem[kw_start..kw_end] == kw && (kw_start == 0 || bytes[kw_start - 1] == b'-') {
            if kw_start == 0 {
                // No slug prefix — invalid (we always expect `<slug>-claude-N`).
                return None;
            }
            let slug = &stem[..kw_start - 1];
            let sid = &stem[kw_start..];
            if slug.is_empty() {
                return None;
            }
            return Some((slug.to_string(), sid.to_string()));
        }
    }
    None
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
    fn initial_watermarks_seeds_nested_session_files() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("dev-flex").join("claude-1.jsonl");
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(&f, b"{}\n").unwrap();
        let wm = initial_watermarks(tmp.path());
        assert_eq!(wm.get(&f).copied(), Some(3));
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
        assert_eq!(m1.sid, None);
        assert!(m1.event_json.contains("phase_inject"));
        assert_eq!(m2.slug, "dev-foo");
        assert_eq!(m2.sid, None);
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
        assert_eq!(m.sid, None);
        assert!(m.event_json.contains("good"));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn drain_new_lines_tags_nested_session_sid() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("dev-flex").join("claude-1.jsonl");
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(&f, b"{\"event\":\"PostToolUse\",\"tool\":\"Read\"}\n").unwrap();

        let (tx, mut rx) = broadcast::channel::<ProgressUpdate>(BROADCAST_CAPACITY);
        let wm = Arc::new(std::sync::Mutex::new(HashMap::new()));
        drain_new_lines(&f, &tx, &wm).unwrap();
        let m = rx.try_recv().unwrap();
        assert_eq!(m.slug, "dev-flex");
        assert_eq!(m.sid.as_deref(), Some("claude-1"));
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

    // V0.3.1 F46 — `split_harness_stem` is the only non-trivial
    // routing logic in the harness watcher path; cover the main
    // shapes here so the integration test can stay focused on the
    // wire format.

    #[test]
    fn split_harness_stem_handles_single_team_segment_slug() {
        assert_eq!(
            split_harness_stem("dev-foo-claude-1"),
            Some(("dev-foo".into(), "claude-1".into()))
        );
    }

    #[test]
    fn split_harness_stem_handles_multi_segment_slug() {
        assert_eq!(
            split_harness_stem("research-bigquery-tax-claude-2"),
            Some(("research-bigquery-tax".into(), "claude-2".into()))
        );
    }

    #[test]
    fn split_harness_stem_handles_codex_sid() {
        assert_eq!(
            split_harness_stem("dev-foo-codex-7"),
            Some(("dev-foo".into(), "codex-7".into()))
        );
    }

    #[test]
    fn split_harness_stem_handles_meta_handle() {
        assert_eq!(
            split_harness_stem("_meta-rob"),
            Some(("_meta-rob".into(), "default".into()))
        );
    }

    #[test]
    fn split_harness_stem_rejects_garbage() {
        assert!(split_harness_stem("just-a-name").is_none());
        assert!(split_harness_stem("dev-foo").is_none());
        assert!(split_harness_stem("claude-1").is_none()); // missing slug
        assert!(split_harness_stem("").is_none());
    }

    #[test]
    fn is_harness_snapshot_file_filters_correctly() {
        assert!(is_harness_snapshot_file(Path::new(
            "/x/dev-foo-claude-1.json"
        )));
        assert!(!is_harness_snapshot_file(Path::new(
            "/x/dev-foo-claude-1.tmp"
        )));
        assert!(!is_harness_snapshot_file(Path::new(
            "/x/dev-foo-claude-1.json.tmp"
        )));
        assert!(!is_harness_snapshot_file(Path::new("/x/notes.md")));
    }
}
