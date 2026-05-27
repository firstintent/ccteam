//! V0.8 rmux Slice 1 — typed-event pipeline integration coverage.
//!
//! Exercises [`ccteam_core::execution::typed_events::maybe_start_typed_event_tap`],
//! the flag-gated bridge that mirrors a live mux session's no-enrichment
//! pattern detections into a project's `progress.jsonl` as `typed_event`
//! rows.
//!
//! Three angles:
//!  1. `typed_event_row_written_for_rate_limit_pattern_when_flag_on`
//!     (`#[ignore]`, needs a real rmux daemon + PTY): with the flag ON,
//!     a pane line that trips the Claude `rate_limit` base pattern lands
//!     a `kind=="typed_event"` / `event_kind=="rate_limit"` row.
//!  2. `no_typed_event_row_when_flag_off`: with the flag UNSET the entry
//!     point is a cheap early return — nothing is written.
//!  3. `build_typed_event_event_has_expected_shape`: the row constructor
//!     produces the expected JSON shape.
//!
//! ## Why `rate_limit` is the chosen trigger
//! rmux `subscribe` emits `OutputChunk` + `PatternMatched` only — it does
//! NOT synthesize `ProcessExited` / `OutputIdle`. So the reliable
//! no-enrichment (BaseOnly) signal through a live daemon is a regex hit
//! on a registered base pattern. Claude's `rate_limit` pattern
//! (`(?i)rate limit|too many requests`) maps to `EventKind::RateLimitHit`
//! → `MergeOutcome::BaseOnly`, which is exactly what Slice 1 emits.
//!
//! ## Late-arrival caveat is irrelevant here
//! The `SeqState` pairing caveat (a late enrichment mispairing with the
//! next base) cannot bite this slice: Slice 1 feeds NO enrichment, so
//! only `None`-kind base patterns are exercised and every emitted merge
//! outcome is `BaseOnly`. There is no enrichment side to arrive late.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ccteam_core::execution::typed_events::{enrich_session, maybe_start_typed_event_tap};
use ccteam_core::progress;
use ccteam_mux::{
    EventKind, InProcBackend, MuxBackend, MuxSessionId, MuxSessionSpec, RmuxBackend, Vendor,
};

/// Drop guard that restores a `$ENV` var to its pre-test value. Mirrors
/// the guard used by the sibling rmux adapter test, plus a `remove`
/// ctor for the flag-OFF case.
struct EnvGuard {
    key: &'static str,
    prev: Option<std::ffi::OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let prev = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, prev }
    }

    fn remove(key: &'static str) -> Self {
        let prev = std::env::var_os(key);
        std::env::remove_var(key);
        Self { key, prev }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match self.prev.take() {
            Some(v) => std::env::set_var(self.key, v),
            None => std::env::remove_var(self.key),
        }
    }
}

/// Locate the ccteam binary in the workspace target dir; the rmux SDK
/// re-execs it with `--__internal-daemon <socket>` to host the daemon.
fn locate_ccteam_binary() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("CCTEAM_TEST_BIN") {
        return Some(PathBuf::from(path));
    }
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir.parent()?.parent()?;
    for rel in ["target/debug/ccteam", "target/release/ccteam"] {
        let candidate = workspace_root.join(rel);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

fn random_session_name(base: &str) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("ccteam-typed-ev-{base}-{nanos}")
}

/// End-to-end: with the flag ON, a live rmux session whose pane prints a
/// rate-limit line produces a `typed_event` / `event_kind=="rate_limit"`
/// row in `progress.jsonl`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
#[ignore]
async fn typed_event_row_written_for_rate_limit_pattern_when_flag_on() {
    let Some(bin) = locate_ccteam_binary() else {
        eprintln!(
            "SKIP: ccteam binary not found in target/{{debug,release}}/ccteam; build with \
             `cargo build --bin ccteam` (or set CCTEAM_TEST_BIN=...) and rerun."
        );
        return;
    };
    eprintln!("ccteam binary: {}", bin.display());

    // Pin the rmux SDK's daemon re-exec target to the real ccteam binary
    // (the test binary has no --__internal-daemon handler).
    let _daemon_bin = EnvGuard::set("RMUX_SDK_DAEMON_BINARY", bin.to_str().unwrap());
    // Flag the consumer ON. `#[serial]` serializes the env mutation.
    let _flag = EnvGuard::set("CCTEAM_TYPED_EVENTS", "1");

    let tmpdir = tempfile::tempdir().expect("create tempdir");
    let socket_path = tmpdir.path().join("mux.sock");
    let backend: Arc<dyn MuxBackend> = Arc::new(RmuxBackend::with_socket_path(socket_path.clone()));
    eprintln!("socket: {}", socket_path.display());

    let progress_path = tmpdir.path().join("progress.jsonl");

    // A sleeper pane that, after a short lead, prints a rate-limit line
    // so the registered `rate_limit` base pattern trips. The 2s lead
    // gives the tap's spawned task time to register patterns + subscribe
    // before the line renders (register + subscribe both happen INSIDE
    // `maybe_start_typed_event_tap`'s detached task). The trailing sleep
    // keeps the pane alive for the poll window. This self-emitting form
    // does not depend on PTY echo of `send_text`.
    let session_name = random_session_name("ratelimit");
    let spec = MuxSessionSpec::new(
        &session_name,
        vec![
            "sh".into(),
            "-c".into(),
            "sleep 2; echo 'Error: rate limit reached'; sleep 30".into(),
        ],
        PathBuf::from("/tmp"),
    );

    let id = backend.spawn(spec).await.expect("spawn must succeed");

    // Let the daemon register the session before we attach the tap.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(backend.exists(&id).await.unwrap(), "session must exist");

    // Attach the tap. Its detached task registers the Claude base
    // patterns on `id`, subscribes, and starts streaming BaseOnly merges
    // into `progress_path`.
    maybe_start_typed_event_tap(
        backend.clone(),
        id.clone(),
        Vendor::Claude,
        format!("{session_name}-role"),
        progress_path.clone(),
    );

    // Give the tap time to register patterns + subscribe before the pane
    // line lands (the pane's own `sleep 2` is the primary safety margin).
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Belt-and-suspenders: also drive a matching line via the PTY in case
    // the pane's echo timing differs. Either trigger is sufficient; a
    // duplicate typed_event row is harmless for the assertion.
    let _ = backend.send_text(&id, "Error: too many requests").await;
    let _ = backend.send_enter(&id).await;

    // Poll the progress file for the typed_event row (up to ~5s).
    let mut found = false;
    let mut seen = Vec::new();
    for _ in 0..50 {
        seen = progress::read_all_events(&progress_path).unwrap_or_default();
        found = seen.iter().any(|e| {
            e.get("kind").and_then(|v| v.as_str()) == Some("typed_event")
                && e.get("event_kind").and_then(|v| v.as_str()) == Some("rate_limit")
        });
        if found {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let _ = backend.kill(&id).await;

    assert!(
        found,
        "expected a typed_event row with event_kind==\"rate_limit\" in {} within ~5s; \
         events seen: {:#?}",
        progress_path.display(),
        seen
    );
}

/// Flag-OFF path is behavior-neutral: with `CCTEAM_TYPED_EVENTS` unset,
/// `maybe_start_typed_event_tap` returns immediately and writes nothing.
/// Uses an `InProcBackend` (its `subscribe` is a no-op) so even if the
/// flag check regressed, no real work could produce a row — the
/// assertion isolates the flag gate.
#[tokio::test]
#[serial_test::serial]
async fn no_typed_event_row_when_flag_off() {
    let _flag = EnvGuard::remove("CCTEAM_TYPED_EVENTS");

    let tmpdir = tempfile::tempdir().expect("create tempdir");
    let progress_path = tmpdir.path().join("progress.jsonl");

    let backend: Arc<dyn MuxBackend> = Arc::new(InProcBackend::new());
    maybe_start_typed_event_tap(
        backend,
        MuxSessionId::new("x"),
        Vendor::Claude,
        "slug-role".to_string(),
        progress_path.clone(),
    );

    // If the flag gate were broken, a spawned task would need this long
    // to attempt any write; with the gate intact nothing is scheduled.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let events = progress::read_all_events(&progress_path).unwrap_or_default();
    let has_typed = events
        .iter()
        .any(|e| e.get("kind").and_then(|v| v.as_str()) == Some("typed_event"));
    assert!(
        !has_typed,
        "flag-OFF path must write no typed_event row; events: {events:#?}"
    );
}

/// The row constructor produces the agreed `typed_event` JSON shape.
#[test]
fn build_typed_event_event_has_expected_shape() {
    let row = progress::build_typed_event_event(
        "claude",
        "rate_limit",
        "Error: rate limit reached",
        "sess-1",
    );
    assert_eq!(
        row.get("kind").and_then(|v| v.as_str()),
        Some("typed_event")
    );
    assert_eq!(row.get("vendor").and_then(|v| v.as_str()), Some("claude"));
    assert_eq!(
        row.get("event_kind").and_then(|v| v.as_str()),
        Some("rate_limit")
    );
    assert!(
        row.get("ts").and_then(|v| v.as_str()).is_some(),
        "row must carry a `ts` timestamp field; got: {row:#?}"
    );
}

/// Spawn a self-emitting Claude session whose pane prints a `turn_done`
/// trigger line (`╰─`) after a 2s lead, returning the live backend, its
/// session id, and the temp dir holding the socket + progress file.
///
/// The 2s lead is the safety margin that lets `maybe_start_typed_event_tap`'s
/// detached task register patterns + subscribe before the line renders (the
/// Claude `turn_done` base pattern is `^╰─|^>\s*$`). The trailing `sleep 30`
/// keeps the pane alive across the poll/grace window.
async fn spawn_turn_done_session(
    socket_path: PathBuf,
    base: &str,
) -> (Arc<dyn MuxBackend>, MuxSessionId, String) {
    let backend: Arc<dyn MuxBackend> = Arc::new(RmuxBackend::with_socket_path(socket_path.clone()));
    eprintln!("socket: {}", socket_path.display());

    let session_name = random_session_name(base);
    let spec = MuxSessionSpec::new(
        &session_name,
        vec![
            "sh".into(),
            "-c".into(),
            "sleep 2; echo '╰─ turn done'; sleep 30".into(),
        ],
        PathBuf::from("/tmp"),
    );
    let id = backend.spawn(spec).await.expect("spawn must succeed");

    // Let the daemon register the session before the caller attaches the tap.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        backend.exists(&id).await.unwrap(),
        "session must exist after spawn"
    );

    (backend, id, session_name)
}

/// Slice 2 — reliability fallback. With BOTH `CCTEAM_TYPED_EVENTS` and
/// `CCTEAM_HOOK_VIA_DAEMON` ON, a `turn_done` pane pattern fires, parks for the
/// merger's grace window awaiting a `Stop`-hook enrichment, and — because NO
/// enrichment is routed to the tap — falls back to `MergeOutcome::BaseLossy`,
/// which the consumer mirrors as a `merger_lossy_partial` /
/// `event_kind=="turn_done"` row.
///
/// `CCTEAM_HOOK_VIA_DAEMON` is gating: the tap snapshots
/// `hook_via_daemon_enabled()` ONCE at start (typed_events.rs), so it must be
/// set BEFORE `maybe_start_typed_event_tap` is called — `#[serial]` + the
/// EnvGuard set order below guarantee that.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
#[ignore]
async fn merger_lossy_partial_written_for_unenriched_turn_done() {
    let Some(bin) = locate_ccteam_binary() else {
        eprintln!(
            "SKIP: ccteam binary not found in target/{{debug,release}}/ccteam; build with \
             `cargo build --bin ccteam` (or set CCTEAM_TEST_BIN=...) and rerun."
        );
        return;
    };
    eprintln!("ccteam binary: {}", bin.display());

    // Pin the rmux SDK's daemon re-exec target + flag the consumer ON. Both
    // env vars are set BEFORE the tap is started so the tap's one-shot
    // `lossy_meaningful` snapshot sees `CCTEAM_HOOK_VIA_DAEMON` ON.
    let _daemon_bin = EnvGuard::set("RMUX_SDK_DAEMON_BINARY", bin.to_str().unwrap());
    let _flag = EnvGuard::set("CCTEAM_TYPED_EVENTS", "1");
    let _hook_daemon = EnvGuard::set("CCTEAM_HOOK_VIA_DAEMON", "1");

    let tmpdir = tempfile::tempdir().expect("create tempdir");
    let socket_path = tmpdir.path().join("mux.sock");
    let progress_path = tmpdir.path().join("progress.jsonl");

    let (backend, id, session_name) = spawn_turn_done_session(socket_path, "lossy-turndone").await;
    let session_key = format!("{session_name}-role");

    // Attach the tap. No `enrich_session` call — the parked turn_done base
    // gets no partner, so after the grace window it resolves BaseLossy.
    maybe_start_typed_event_tap(
        backend.clone(),
        id.clone(),
        Vendor::Claude,
        session_key.clone(),
        progress_path.clone(),
    );

    // Poll up to ~6s (line at t≈2s + 500ms grace + emit + fs flush).
    let mut found = false;
    let mut seen = Vec::new();
    for _ in 0..60 {
        seen = progress::read_all_events(&progress_path).unwrap_or_default();
        found = seen.iter().any(|e| {
            e.get("kind").and_then(|v| v.as_str()) == Some("merger_lossy_partial")
                && e.get("event_kind").and_then(|v| v.as_str()) == Some("turn_done")
        });
        if found {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let _ = backend.kill(&id).await;

    assert!(
        found,
        "expected a merger_lossy_partial row with event_kind==\"turn_done\" in {} within ~6s \
         (no enrichment arrived → BaseLossy fallback); events seen: {:#?}",
        progress_path.display(),
        seen
    );
}

/// Slice 2 — pairing suppresses the lossy fallback. Same flags + self-emitting
/// session as the lossy test, but here a `TurnDone` enrichment is routed to the
/// tap so the parked `turn_done` base PAIRS (`MergeOutcome::Paired`) instead of
/// going lossy. A `Paired` outcome owns no fallback row, so NO
/// `merger_lossy_partial` row must appear.
///
/// ## Timing note (inherent)
/// The base fires when `╰─` renders at t≈2s and parks for the merger's 500ms
/// grace window; the enrichment must land inside `[t_base, t_base + 500ms]`.
/// We use an enrichment-first strategy: the tap attaches at t≈0, then we sleep
/// ~1.7s and call `enrich_session` — the enrichment parks first, the base
/// arrives ~300ms later, and they pair. To be robust against scheduling jitter
/// we spray a few more `enrich_session` calls spanning t≈1.7s–2.4s; surplus
/// enrichments leave only dangling PENDING-enrichments (which never emit a
/// `merger_lossy_partial` row — only unenriched BASES do), so spraying is safe.
/// We deliberately do NOT drive a second `╰─` line via `send_text`: a second
/// base would park unenriched and go BaseLossy, defeating the assertion.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
#[ignore]
async fn turn_done_paired_suppresses_lossy_partial() {
    let Some(bin) = locate_ccteam_binary() else {
        eprintln!(
            "SKIP: ccteam binary not found in target/{{debug,release}}/ccteam; build with \
             `cargo build --bin ccteam` (or set CCTEAM_TEST_BIN=...) and rerun."
        );
        return;
    };
    eprintln!("ccteam binary: {}", bin.display());

    let _daemon_bin = EnvGuard::set("RMUX_SDK_DAEMON_BINARY", bin.to_str().unwrap());
    let _flag = EnvGuard::set("CCTEAM_TYPED_EVENTS", "1");
    let _hook_daemon = EnvGuard::set("CCTEAM_HOOK_VIA_DAEMON", "1");

    let tmpdir = tempfile::tempdir().expect("create tempdir");
    let socket_path = tmpdir.path().join("mux.sock");
    let progress_path = tmpdir.path().join("progress.jsonl");

    let (backend, id, session_name) = spawn_turn_done_session(socket_path, "paired-turndone").await;
    let session_key = format!("{session_name}-role");

    maybe_start_typed_event_tap(
        backend.clone(),
        id.clone(),
        Vendor::Claude,
        session_key.clone(),
        progress_path.clone(),
    );

    // The registry slot for `session_key` is populated only AFTER
    // `TypedEventTap::spawn().await` returns inside the detached task. Wait for
    // that, then spray enrichments straddling the t≈2s base so at least one
    // lands within the 500ms grace. The first calls park ahead of the base
    // (enrichment-first pairing); the base then pairs with the earliest.
    tokio::time::sleep(Duration::from_millis(1700)).await;
    for _ in 0..6 {
        enrich_session(&session_key, EventKind::TurnDone, "{}".to_string());
        tokio::time::sleep(Duration::from_millis(120)).await;
    }

    // Wait comfortably past the grace window (line at t≈2s + 500ms grace, plus
    // margin) before the negative assertion.
    tokio::time::sleep(Duration::from_millis(2000)).await;

    let events = progress::read_all_events(&progress_path).unwrap_or_default();
    let has_lossy = events.iter().any(|e| {
        e.get("kind").and_then(|v| v.as_str()) == Some("merger_lossy_partial")
            && e.get("event_kind").and_then(|v| v.as_str()) == Some("turn_done")
    });

    let _ = backend.kill(&id).await;

    assert!(
        !has_lossy,
        "turn_done base paired with its enrichment → Paired → no merger_lossy_partial row \
         expected; events seen: {events:#?}"
    );
}
