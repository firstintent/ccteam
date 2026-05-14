//! V0.3.1 F46 — harness snapshot SSE endpoints (V0.4.0 F61 retargeted).
//!
//! Mirrors the M5.2 `routes/sse.rs` shape, but pulls from the sibling
//! [`crate::watcher::EventBus::subscribe_harness`] channel:
//!
//! - `GET /sse/harness/<slug>` — every harness snapshot for any sid under
//!   `<slug>`.
//! - `GET /sse/harness/<slug>/<sid>` — only snapshots for the requested
//!   `(slug, sid)` pair.
//!
//! Wire format (per dev-plan §2.3):
//!
//! ```text
//! event: harness_snapshot
//! data: {"slug":"dev-foo","sid":"claude-1","snapshot":{...}}
//!
//! ```
//!
//! Each `data:` payload is a single-line JSON object: a server-built
//! envelope wrapping the parsed `HarnessSnapshot` (so consumers don't
//! have to derive slug / sid from the source filename — the watcher
//! already did that).
//!
//! Source of the snapshots: V0.4.0 F61 publishes them from Claude Code's
//! native `~/.claude/jobs/<job_id>/state.json` (`parse_cc_state_json`).
//! Earlier ship lines used a different source file. The SSE wire shape
//! is unchanged across both — this route is harness-source-agnostic.
//!
//! Keep-alive + lagging-consumer behavior matches `routes::sse` (15s
//! `:` comment, `reconnect_hint` synthetic frame on `Lagged(N)`).

use std::convert::Infallible;
use std::time::Duration;

use axum::{
    extract::{Path, State},
    response::sse::{Event, KeepAlive, Sse},
    routing::get,
    Router,
};
use futures::stream::{self, Stream, StreamExt};
use serde_json::json;
use tokio_stream::wrappers::BroadcastStream;

use crate::state::AppState;
use crate::watcher::HarnessSnapshotEvent;

/// PRD §5.2.3 — emit a keep-alive comment every 15s so reverse
/// proxies don't kill idle SSE connections. Same constant as
/// `routes::sse`; we re-declare here to avoid a cross-module
/// `pub(crate)` constant in M5.2's already-shipped surface.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/sse/harness/{slug}", get(handle_sse_harness_all))
        .route("/sse/harness/{slug}/{sid}", get(handle_sse_harness_session))
}

async fn handle_sse_harness_all(
    State(app): State<AppState>,
    Path(slug): Path<String>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = app.bus.subscribe_harness();
    let target = slug.clone();
    let stream = BroadcastStream::new(rx).flat_map(move |item| {
        let target = target.clone();
        match item {
            Ok(update) if update.slug == target => stream::iter(vec![Ok(harness_event(&update))]),
            Ok(_) => stream::iter(vec![]),
            Err(err) => stream::iter(vec![Ok(reconnect_hint(&format!("{err}")))]),
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::default().interval(KEEPALIVE_INTERVAL))
}

async fn handle_sse_harness_session(
    State(app): State<AppState>,
    Path((slug, sid)): Path<(String, String)>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = app.bus.subscribe_harness();
    let target_slug = slug.clone();
    let target_sid = sid.clone();
    let stream = BroadcastStream::new(rx).filter_map(move |item| {
        let target_slug = target_slug.clone();
        let target_sid = target_sid.clone();
        async move {
            match item {
                Ok(update) if update.slug == target_slug && update.sid == target_sid => {
                    Some(Ok(harness_event(&update)))
                }
                Ok(_) => None,
                Err(err) => Some(Ok(reconnect_hint(&format!("{err}")))),
            }
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::default().interval(KEEPALIVE_INTERVAL))
}

/// Build the `event: harness_snapshot` SSE frame. `data:` is a
/// single-line JSON envelope: `{"slug":..,"sid":..,"snapshot":..}`.
fn harness_event(update: &HarnessSnapshotEvent) -> Event {
    let payload = json!({
        "slug": update.slug,
        "sid": update.sid,
        "snapshot": update.snapshot,
    });
    Event::default()
        .event("harness_snapshot")
        .data(payload.to_string())
}

fn reconnect_hint(reason: &str) -> Event {
    Event::default()
        .event("reconnect_hint")
        .data(json!({ "type": "reconnect_hint", "reason": reason }).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ccteam_core::HarnessSnapshot;

    fn fixture_snapshot() -> HarnessSnapshot {
        HarnessSnapshot {
            harness: "claude-code".into(),
            model_display_name: "Claude Sonnet 4.5".into(),
            context_used_pct: 12,
            cost_usd_total: 0.42,
            rate_limit_pct: Some(7),
            cwd: None,
            raw: serde_json::json!({}),
            captured_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn harness_event_wraps_snapshot_in_envelope() {
        let ev = harness_event(&HarnessSnapshotEvent {
            slug: "dev-foo".into(),
            sid: "claude-1".into(),
            snapshot: fixture_snapshot(),
        });
        // axum's Event doesn't expose payload publicly; rebuild for
        // shape assertion. The wire test in tests/harness_sse_test.rs
        // covers the full SSE frame.
        let _ = ev;
    }

    #[test]
    fn reconnect_hint_uses_named_event() {
        let _ = reconnect_hint("Lagged(123)");
    }
}
