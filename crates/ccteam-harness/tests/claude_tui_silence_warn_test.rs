//! F187 — verify chat-mode tail loops surface marker-missing silence
//! with one WARN per stuck-period instead of sleeping forever in
//! exponential backoff with zero log breadcrumb.
//!
//! The WARN-after threshold is overridable via the
//! `CCTEAM_TAIL_MARKER_WARN_MS` env var so the test doesn't wait the
//! full 60s production threshold.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ccteam_harness::execution::transcript_tail::{active_session_id_path, encode_project_cwd};
use serial_test::serial;
use tempfile::TempDir;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Metadata, Subscriber};

/// Minimal subscriber that records every WARN event's
/// `event` field (the `event = "tail_marker_missing"` tag we emit) so
/// the test can assert the WARN fired exactly the expected number of
/// times. Skips non-WARN levels to keep noise out.
#[derive(Default, Clone)]
struct WarnCapture {
    events: Arc<Mutex<Vec<String>>>,
}

impl WarnCapture {
    fn taken(&self) -> Vec<String> {
        self.events.lock().unwrap().clone()
    }
}

struct EventTagVisitor {
    out: Option<String>,
}

impl Visit for EventTagVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "event" {
            self.out = Some(format!("{value:?}").trim_matches('"').to_string());
        }
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "event" {
            self.out = Some(value.to_string());
        }
    }
}

impl Subscriber for WarnCapture {
    fn enabled(&self, meta: &Metadata<'_>) -> bool {
        *meta.level() == Level::WARN
    }
    fn new_span(&self, _attrs: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }
    fn record(&self, _span: &Id, _values: &Record<'_>) {}
    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}
    fn event(&self, event: &Event<'_>) {
        if *event.metadata().level() != Level::WARN {
            return;
        }
        let mut v = EventTagVisitor { out: None };
        event.record(&mut v);
        if let Some(tag) = v.out {
            self.events.lock().unwrap().push(tag);
        }
    }
    fn enter(&self, _span: &Id) {}
    fn exit(&self, _span: &Id) {}
}

/// Manufacture a `ThreadHandle` that points `events()` at a temp
/// project dir with no marker file. The tail loop should then sit in
/// the polling fallback (no Anthropic projects dir under HOME) waiting
/// for a marker that never lands → fire WARN after the env-tuned
/// threshold elapses.
#[tokio::test]
#[serial]
async fn tail_loop_warns_once_when_marker_missing_for_threshold() {
    use ccteam_harness::execution::claude_tui::ClaudeTuiAdapter;
    use ccteam_harness::{AgentVendor, ExecutionMode, HarnessAdapter, ThreadHandle};
    use chrono::Utc;
    use serde_json::json;

    // Pin a fake HOME so the Anthropic projects dir resolution either
    // creates a controlled empty dir OR returns None — either way the
    // tail loop won't find a marker and the silence WARN should fire.
    let tmp = TempDir::new().unwrap();
    let fake_home = tmp.path().join("home");
    let project_dir = tmp.path().join("project");
    let cwd = project_dir.clone();
    std::fs::create_dir_all(&project_dir).unwrap();
    // Pre-create the Anthropic projects dir so `parent_dir.exists()` is
    // true and we land in the watch-arming code path quickly.
    let encoded = encode_project_cwd(&cwd);
    let anthropic_dir = fake_home.join(".claude").join("projects").join(&encoded);
    std::fs::create_dir_all(&anthropic_dir).unwrap();

    let prev_home = std::env::var("HOME").ok();
    std::env::set_var("HOME", &fake_home);
    // Short WARN threshold so the test doesn't hold up CI for 60s.
    std::env::set_var("CCTEAM_TAIL_MARKER_WARN_MS", "100");

    // Confirm no marker is on disk — this is the "stuck" condition.
    let marker = active_session_id_path(&project_dir, "s1");
    assert!(!marker.exists());

    let capture = WarnCapture::default();
    let captured_for_assert = capture.clone();
    let dispatch = tracing::Dispatch::new(capture);

    let handle = ThreadHandle {
        vendor: AgentVendor::Claude,
        mode: ExecutionMode::Chat,
        identity: "ccteam-chat-test-alice".into(),
        started_at: Utc::now(),
        raw_extras: json!({
            "role": "alice",
            "sid": "s1",
            "project_dir": project_dir.display().to_string(),
            "cwd": cwd.display().to_string(),
        }),
    };

    let adapter = ClaudeTuiAdapter::new();
    let _stream = tracing::dispatcher::with_default(&dispatch, || adapter.events(&handle));

    // The 2s safety-net tick gates the WARN — sleep long enough for at
    // least one tick past the 100ms threshold to land. 3s is safe.
    tokio::time::sleep(Duration::from_millis(3_000)).await;

    // Cleanup env early so panic-on-assert doesn't leak state to
    // sibling tests via serial_test's strict ordering.
    if let Some(h) = prev_home {
        std::env::set_var("HOME", h);
    } else {
        std::env::remove_var("HOME");
    }
    std::env::remove_var("CCTEAM_TAIL_MARKER_WARN_MS");

    let warns = captured_for_assert.taken();
    let tail_warns: Vec<_> = warns
        .iter()
        .filter(|tag| tag.as_str() == "tail_marker_missing")
        .collect();
    assert!(
        !tail_warns.is_empty(),
        "expected at least one tail_marker_missing WARN, got events: {warns:?}",
    );
    // Suppress invariant — must NOT spam. The 2s tick means at most one
    // WARN before the test wakes (3s wall-clock). Strictly == 1.
    assert_eq!(
        tail_warns.len(),
        1,
        "tail_marker_missing WARN must fire exactly once per stuck period, got: {warns:?}",
    );
}
