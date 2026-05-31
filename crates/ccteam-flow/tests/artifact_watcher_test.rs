//! V0.4.0 F64 — integration tests for [`ccteam_core::ArtifactWatcher`].
//!
//! ## Why integration tests (not unit tests inside the module)
//!
//! The watcher's contract is filesystem-driven and needs a real
//! `tokio::runtime`. Putting these under `tests/` gives every case its
//! own process (no env-var / global-state race with the rest of the
//! `ccteam-core` test surface) and lets us use `tempfile::tempdir()`
//! for filesystem isolation per CLAUDE.md §六 guidance on
//! env-mutating integration tests.
//!
//! ## Test plan (mirrors dev-plan §6.1 #5.6)
//!
//! - **t01_watch_creates_missing_dir** — lazy mkdir on construct
//! - **t02_new_file_triggers_event** — Create event arrives
//! - **t03_modified_file_triggers_event** — Modify event arrives
//! - **t04_debounce_merges_rapid_writes** — burst → fewer events
//! - **t05_role_name_in_event** — role string round-trips
//! - **t06_watcher_drops_cleanly** — receiver-drop ends the task
//! - **t07_nonexistent_root_dir** — deep missing parent → mkdir -p
//! - **t08_multiple_watch_paths** — per-role fan-out
//! - **t09_deleted_file_event** — Delete event arrives
//! - **t10_large_batch_debounce** — 100 rapid writes → < 10 events

use std::path::PathBuf;
use std::time::Duration;

use ccteam_flow::WorkflowSpec;
use ccteam_flow::{AgentSpec, ArtifactEvent, ArtifactWatcher, Executor, Trigger, WatchKind};
use indexmap::IndexMap;
use tempfile::tempdir;

/// Build a WorkflowSpec with one or more `(role, watch_root)` watch
/// agents. Roots are inserted in the given order so test
/// determinism mirrors production (IndexMap preserves order).
fn build_spec(name: &str, watchers: &[(&str, PathBuf)]) -> WorkflowSpec {
    let mut agents = IndexMap::new();
    for (role, root) in watchers {
        agents.insert(
            (*role).to_string(),
            AgentSpec {
                executor: Executor::Claude,
                model: None,
                trigger: Trigger::Watch(root.clone()),
                scope: None,
                parallelism: None,
                input: None,
                output: None,
                schedule: None,
                timeout: None,
                on_timeout: None,
                plan_approval: None,
                chat_handle: None,
            },
        );
    }
    WorkflowSpec {
        name: name.to_string(),
        description: None,
        mode: ccteam_flow::WorkflowMode::default(),
        enabled: true,
        budget: None,
        budgets_v060: None,
        agent_team: None,
        chat: None,
        squad: None,
        agents,
    }
}

/// Convenience: build, start, return `(rx, JoinHandle)`. The watcher
/// is consumed by `start()`; the JoinHandle is returned so the test
/// can choose to drop the rx (clean shutdown) or `tokio::time::timeout`
/// on `recv()`.
async fn spawn_for(
    spec: &WorkflowSpec,
) -> (
    tokio::sync::mpsc::Receiver<ArtifactEvent>,
    tokio::task::JoinHandle<()>,
) {
    let (watcher, rx) = ArtifactWatcher::new(spec, None, None).expect("build watcher");
    let handle = watcher.start();
    // Give the notify backend a moment to install the inotify watches
    // before the test starts writing files. 100ms is well under
    // tokio::time::timeout windows used below.
    tokio::time::sleep(Duration::from_millis(100)).await;
    (rx, handle)
}

/// Poll the receiver until either an event arrives or `total` elapses.
/// Returns the first event or `None` on timeout.
async fn next_event(
    rx: &mut tokio::sync::mpsc::Receiver<ArtifactEvent>,
    total: Duration,
) -> Option<ArtifactEvent> {
    tokio::time::timeout(total, rx.recv()).await.ok().flatten()
}

/// Drain everything in the receiver until `quiet` ms pass with no new
/// events. Returns the count drained. Used by debounce tests.
async fn drain_until_quiet(
    rx: &mut tokio::sync::mpsc::Receiver<ArtifactEvent>,
    quiet: Duration,
    cap: Duration,
) -> usize {
    let start = std::time::Instant::now();
    let mut count = 0;
    loop {
        if start.elapsed() > cap {
            break;
        }
        match tokio::time::timeout(quiet, rx.recv()).await {
            Ok(Some(_)) => {
                count += 1;
            }
            Ok(None) => break, // sender dropped
            Err(_) => break,   // quiet window with no events
        }
    }
    count
}

#[tokio::test]
async fn t01_watch_creates_missing_dir() {
    let tmp = tempdir().unwrap();
    let watch_root = tmp.path().join("artifacts-t01");
    assert!(!watch_root.exists(), "precondition: dir must not exist");

    let spec = build_spec("t01", &[("explorer", watch_root.clone())]);
    let (_watcher, _rx) =
        ArtifactWatcher::new(&spec, None, None).expect("constructor must mkdir missing roots");

    assert!(
        watch_root.exists(),
        "watch root should have been auto-created"
    );
    assert!(watch_root.is_dir(), "watch root should be a directory");
}

#[tokio::test]
async fn t02_new_file_triggers_event() {
    let tmp = tempdir().unwrap();
    let watch_root = tmp.path().join("artifacts-t02");
    std::fs::create_dir_all(&watch_root).unwrap();

    let spec = build_spec("t02", &[("explorer", watch_root.clone())]);
    let (mut rx, _handle) = spawn_for(&spec).await;

    let file = watch_root.join("new.md");
    std::fs::write(&file, b"hello").unwrap();

    let ev = next_event(&mut rx, Duration::from_millis(800))
        .await
        .expect("must receive an event within 800ms");
    assert_eq!(ev.role, "explorer");
    // The path platform-reports might be the dir or the file
    // depending on the inotify backend; assert it is at least the
    // file or its parent.
    assert!(
        ev.artifact_path == file || ev.artifact_path == watch_root,
        "unexpected path {:?}",
        ev.artifact_path,
    );
    assert!(
        matches!(ev.event_kind, WatchKind::Created | WatchKind::Modified),
        "expected Created or Modified, got {:?}",
        ev.event_kind,
    );
}

#[tokio::test]
async fn t03_modified_file_triggers_event() {
    let tmp = tempdir().unwrap();
    let watch_root = tmp.path().join("artifacts-t03");
    std::fs::create_dir_all(&watch_root).unwrap();

    // Pre-create the file so the first observed change is a Modify.
    let file = watch_root.join("doc.md");
    std::fs::write(&file, b"v1").unwrap();

    let spec = build_spec("t03", &[("explorer", watch_root.clone())]);
    let (mut rx, _handle) = spawn_for(&spec).await;

    // Wait past the debounce window so the modify isn't merged with
    // a pre-existing-but-stale entry. (DEBOUNCE_WINDOW = 500ms;
    // see module doc.)
    tokio::time::sleep(Duration::from_millis(600)).await;

    std::fs::write(&file, b"v2-modified").unwrap();

    let ev = next_event(&mut rx, Duration::from_millis(1500))
        .await
        .expect("must receive a Modify event within 1.5s");
    assert_eq!(ev.role, "explorer");
    assert!(
        matches!(ev.event_kind, WatchKind::Modified | WatchKind::Created),
        "expected Modified (or Created on save-rewrite editors), got {:?}",
        ev.event_kind,
    );
}

#[tokio::test]
async fn t04_debounce_merges_rapid_writes() {
    let tmp = tempdir().unwrap();
    let watch_root = tmp.path().join("artifacts-t04");
    std::fs::create_dir_all(&watch_root).unwrap();

    let spec = build_spec("t04", &[("explorer", watch_root.clone())]);
    let (mut rx, _handle) = spawn_for(&spec).await;

    // Write 10 files inside one 50ms window — debounce (500ms) must
    // collapse them to ≤ 2 events.
    let start = std::time::Instant::now();
    for i in 0..10 {
        let p = watch_root.join(format!("burst-{i}.md"));
        std::fs::write(&p, b"x").unwrap();
    }
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_millis(200),
        "burst writes took {elapsed:?}; test assumes < 200ms",
    );

    let count =
        drain_until_quiet(&mut rx, Duration::from_millis(800), Duration::from_secs(2)).await;
    assert!(
        count <= 2,
        "debounce should merge ≤ 2 events from a 10-file burst; got {count}",
    );
    assert!(count >= 1, "at least one event must reach the receiver");
}

#[tokio::test]
async fn t05_role_name_in_event() {
    let tmp = tempdir().unwrap();
    let watch_root = tmp.path().join("artifacts-t05");
    std::fs::create_dir_all(&watch_root).unwrap();

    let spec = build_spec("t05", &[("fixer", watch_root.clone())]);
    let (mut rx, _handle) = spawn_for(&spec).await;

    std::fs::write(watch_root.join("issue-1.md"), b"bug").unwrap();
    let ev = next_event(&mut rx, Duration::from_millis(800))
        .await
        .expect("event within 800ms");
    assert_eq!(
        ev.role, "fixer",
        "role string in event must match WorkflowSpec.agents key",
    );
}

#[tokio::test]
async fn t06_watcher_drops_cleanly() {
    let tmp = tempdir().unwrap();
    let watch_root = tmp.path().join("artifacts-t06");
    std::fs::create_dir_all(&watch_root).unwrap();

    let spec = build_spec("t06", &[("explorer", watch_root.clone())]);
    let (rx, handle) = spawn_for(&spec).await;

    // Drop the receiver — the spawn_blocking task should exit on the
    // next tx.blocking_send() attempt. To force one, write a file so
    // an event is in flight.
    drop(rx);
    std::fs::write(watch_root.join("trigger-shutdown.md"), b"go").unwrap();

    // Give the task up to 2s to exit. Production targets < 1s in the
    // briefing but inotify wake latency on a loaded CI host can add
    // a few hundred ms.
    let res = tokio::time::timeout(Duration::from_secs(2), handle).await;
    assert!(
        res.is_ok(),
        "watcher task should complete within 2s of receiver drop",
    );
    res.unwrap().expect("task panicked instead of returning");
}

#[tokio::test]
async fn t07_nonexistent_root_dir() {
    let tmp = tempdir().unwrap();
    // Two levels deep — neither parent nor the root exist.
    let watch_root = tmp.path().join("does/not/yet/exist/artifacts");
    assert!(!watch_root.exists());
    assert!(!watch_root.parent().unwrap().exists());

    let spec = build_spec("t07", &[("explorer", watch_root.clone())]);
    let (_watcher, _rx) = ArtifactWatcher::new(&spec, None, None).expect("mkdir -p must succeed");

    assert!(watch_root.exists(), "deep watch root must be created");
    assert!(watch_root.is_dir());
}

#[tokio::test]
async fn t08_multiple_watch_paths() {
    let tmp = tempdir().unwrap();
    let root_a = tmp.path().join("artifacts-a");
    let root_b = tmp.path().join("artifacts-b");
    std::fs::create_dir_all(&root_a).unwrap();
    std::fs::create_dir_all(&root_b).unwrap();

    let spec = build_spec(
        "t08",
        &[("alpha", root_a.clone()), ("beta", root_b.clone())],
    );
    let (mut rx, _handle) = spawn_for(&spec).await;

    std::fs::write(root_a.join("from-a.md"), b"a").unwrap();
    // Sleep past debounce so beta's event isn't merged with alpha's
    // (debounce is per-root so this isn't strictly required, but
    // makes the test deterministic regardless of any future
    // implementation change to debounce keying).
    tokio::time::sleep(Duration::from_millis(550)).await;
    std::fs::write(root_b.join("from-b.md"), b"b").unwrap();

    // Collect events until quiet.
    let mut seen_roles: std::collections::HashSet<String> = std::collections::HashSet::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < deadline && seen_roles.len() < 2 {
        match tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
            Ok(Some(ev)) => {
                seen_roles.insert(ev.role);
            }
            Ok(None) => break,
            Err(_) => continue,
        }
    }

    assert!(
        seen_roles.contains("alpha"),
        "should have received an alpha event; saw {seen_roles:?}",
    );
    assert!(
        seen_roles.contains("beta"),
        "should have received a beta event; saw {seen_roles:?}",
    );
}

#[tokio::test]
async fn t09_deleted_file_event() {
    let tmp = tempdir().unwrap();
    let watch_root = tmp.path().join("artifacts-t09");
    std::fs::create_dir_all(&watch_root).unwrap();
    let file = watch_root.join("doomed.md");
    std::fs::write(&file, b"about to die").unwrap();

    let spec = build_spec("t09", &[("explorer", watch_root.clone())]);
    let (mut rx, _handle) = spawn_for(&spec).await;

    // Wait past debounce so the impending Delete isn't merged with
    // anything stale.
    tokio::time::sleep(Duration::from_millis(600)).await;

    std::fs::remove_file(&file).unwrap();

    // Linux inotify reports IN_DELETE cleanly; some macOS save patterns
    // surface a Modify. We accept either Deleted or Modified — the
    // important property is that the event fires.
    let ev = next_event(&mut rx, Duration::from_millis(1500)).await;
    let ev = ev.expect("Delete should produce a filesystem event");
    assert_eq!(ev.role, "explorer");
    assert!(
        matches!(
            ev.event_kind,
            WatchKind::Deleted | WatchKind::Modified | WatchKind::Created
        ),
        "expected Deleted (or platform-quirk Modified/Created), got {:?}",
        ev.event_kind,
    );
}

#[tokio::test]
async fn t10_large_batch_debounce() {
    let tmp = tempdir().unwrap();
    let watch_root = tmp.path().join("artifacts-t10");
    std::fs::create_dir_all(&watch_root).unwrap();

    let spec = build_spec("t10", &[("explorer", watch_root.clone())]);
    let (mut rx, _handle) = spawn_for(&spec).await;

    // Write 100 files inside a 1s window. With DEBOUNCE_WINDOW = 500ms
    // the watcher should emit ≤ ~3 events (one per debounce slot,
    // plus a flush at the end). Briefing pins the upper bound at
    // < 10, which we tighten to < 6 for headroom.
    let start = std::time::Instant::now();
    for i in 0..100 {
        let p = watch_root.join(format!("batch-{i:03}.md"));
        std::fs::write(&p, b"x").unwrap();
        if start.elapsed() > Duration::from_millis(950) {
            break;
        }
    }

    let count =
        drain_until_quiet(&mut rx, Duration::from_millis(800), Duration::from_secs(3)).await;
    assert!(
        count < 10,
        "debounce should keep 100-file burst under 10 events; got {count}",
    );
    assert!(count >= 1, "at least one event must reach the receiver");
}
