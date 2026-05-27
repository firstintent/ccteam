//! V0.8 rmux typed-event consumer — Slice 1 (None-kind pipeline).
//!
//! Bridges a live Claude chat-TUI session's [`ccteam_mux::TypedEventTap`]
//! into the project's `progress.jsonl` as `typed_event` observability
//! rows. This is the **first production consumer** of the
//! ahead-of-consumer EnrichedEvent merger (see
//! `ccteam-mux::enriched_event` module doc / `TODO(V0.9-typed-event-consumer)`).
//!
//! Scope of this slice is deliberately narrow:
//! - Gated entirely behind the `CCTEAM_TYPED_EVENTS` env flag. With the
//!   flag unset the entry point is a cheap env check + early return, so
//!   the flag-OFF path is behavior-neutral.
//! - We feed **no** enrichment (no [`ccteam_mux::TapHandle::enrich`]
//!   calls), so the only outcome that carries real signal is
//!   [`MergeOutcome::BaseOnly`] — the no-enrichment kinds (rate-limit /
//!   context-overflow / idle / process-exit). Every other outcome
//!   (`Paired` / `BaseLossy` / `EnrichmentOnly` / `BufferOverflow`) is
//!   ignored, because without enrichment fed the enrichment-kind
//!   patterns would otherwise surface spurious `BaseLossy` rows.
//!
//! Nothing currently acts on the emitted `typed_event` rows — they are
//! for visibility only. Enrichment wiring + pairing is a later slice.

use std::path::PathBuf;
use std::sync::Arc;

use ccteam_mux::{
    EventKind, MergeOutcome, MuxBackend, MuxSessionId, TypedEventTap, Vendor, DEFAULT_GRACE,
};

/// True when `CCTEAM_TYPED_EVENTS` is set to a truthy value (`1` / `true`).
fn flag_enabled() -> bool {
    match std::env::var_os("CCTEAM_TYPED_EVENTS") {
        Some(v) => {
            let v = v.to_string_lossy();
            let v = v.trim();
            v == "1" || v.eq_ignore_ascii_case("true")
        }
        None => false,
    }
}

/// Map a merger [`EventKind`] to a stable snake_case string for the
/// `typed_event` row. Only the no-enrichment kinds reach the consumer in
/// this slice; the enrichment kinds are mapped for completeness.
fn event_kind_str(kind: EventKind) -> &'static str {
    match kind {
        EventKind::RateLimitHit => "rate_limit",
        EventKind::ContextOverflow => "context_overflow",
        EventKind::Idle => "idle",
        EventKind::ProcessExited => "process_exited",
        EventKind::ToolCallStarted => "tool_call_started",
        EventKind::ToolCallCompleted => "tool_call_completed",
        EventKind::UserPromptSubmitted => "user_prompt_submitted",
        EventKind::AssistantMessageComplete => "assistant_message_complete",
        EventKind::TurnStarted => "turn_started",
        EventKind::TurnDone => "turn_done",
        EventKind::PlanPending => "plan_pending",
        EventKind::SessionReset => "session_reset",
        EventKind::CompactDone => "compact_done",
    }
}

/// Stable vendor string for the `typed_event` row.
fn vendor_str(vendor: Vendor) -> &'static str {
    match vendor {
        Vendor::Claude => "claude",
        Vendor::Codex => "codex",
    }
}

/// Start a [`TypedEventTap`] for a freshly-live session and stream its
/// `BaseOnly` merged events into `progress_path` as `typed_event` rows.
///
/// No-op (returns immediately) unless [`flag_enabled`]. When enabled,
/// spawns a detached background task; the task ends naturally when the
/// tap's receiver closes (session / backend dropped). Append errors are
/// logged at debug and skipped — never panics.
pub fn maybe_start_typed_event_tap(
    backend: Arc<dyn MuxBackend>,
    id: MuxSessionId,
    vendor: Vendor,
    progress_path: PathBuf,
) {
    if !flag_enabled() {
        return;
    }

    let session = id.as_str().to_string();
    tokio::spawn(async move {
        let (handle, mut rx) = match TypedEventTap::spawn(id, vendor, backend, DEFAULT_GRACE).await
        {
            Ok(pair) => pair,
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    session = %session,
                    "typed-event tap: failed to spawn TypedEventTap"
                );
                return;
            }
        };
        // Slice 1 feeds NO enrichment, so drop the handle now: this closes
        // the tap's enrichment channel, which (with the stream) lets the
        // tap tear down cleanly when the session ends instead of leaking an
        // idle task. The merger still services the `None`-kind base
        // patterns and emits them as `MergeOutcome::BaseOnly`.
        drop(handle);
        while let Some(ev) = rx.recv().await {
            if ev.outcome != MergeOutcome::BaseOnly {
                continue;
            }
            let captured = ev
                .base
                .as_ref()
                .map(|b| b.captured.clone())
                .unwrap_or_default();
            let row = crate::progress::build_typed_event_event(
                vendor_str(vendor),
                event_kind_str(ev.kind),
                &captured,
                &session,
            );
            if let Err(err) = crate::progress::append_event(&progress_path, &row) {
                tracing::debug!(
                    error = %err,
                    session = %session,
                    "typed-event tap: failed to append typed_event row"
                );
            }
        }
    });
}
