//! V0.8 rmux typed-event consumer — the first production consumer of the
//! [`ccteam_mux::EventMerger`].
//!
//! Bridges a live Claude chat-TUI session's [`ccteam_mux::TypedEventTap`]
//! into the project's `progress.jsonl`. Two slices:
//!
//! - **Slice 1 — `BaseOnly` pipeline** (flag: `CCTEAM_TYPED_EVENTS`): the
//!   no-enrichment kinds (rate-limit / context-overflow / idle /
//!   process-exit) are mirrored as `typed_event` observability rows.
//! - **Slice 2 — pairing + `BaseLossy` reliability fallback** (additionally
//!   requires `CCTEAM_HOOK_VIA_DAEMON`, so the orchestrator's hook sink can
//!   feed P1 enrichment): each tap's [`ccteam_mux::TapHandle`] is held in a
//!   session-keyed registry; the hook sink routes a Claude `Stop` hook to
//!   the matching tap as a `TurnDone` enrichment via
//!   [`enrich_session_from_hook`]. When a `turn_done` pane pattern fired but
//!   no `Stop` hook arrived within the merger's grace window, the merger
//!   emits `BaseLossy` and we write a `merger_lossy_partial` row — surfacing
//!   a turn whose lossless hook was lost (e.g. a crashed hook subprocess).
//!
//! Scope of Slice 2 is deliberately limited to **`TurnDone` only**: a turn
//! is single-in-flight per session, so the tap's pending-partner sequence
//! pairing cannot mis-pair. Multi-in-flight kinds (tool calls, prompts) need
//! the per-kind-counter redesign and are left for a later slice. Nothing
//! acts on the emitted rows yet — they are for visibility. The `Stop` hook's
//! PRESENCE (not its payload content) is what pairs; the payload is carried
//! opaquely.
//!
//! Both progress rows (`typed_event`, `merger_lossy_partial`) share the same
//! JSON field layout (`vendor` / `event_kind` / `captured` / `session` /
//! `ts`) so a downstream tool can parse either with one struct, branching on
//! `kind`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use ccteam_mux::{
    EventKind, MergeOutcome, MuxBackend, MuxSessionId, RawEnrichment, TapHandle, TypedEventTap,
    Vendor, DEFAULT_GRACE,
};

/// True when `CCTEAM_TYPED_EVENTS` is set to a truthy value (`1` / `true`).
pub fn flag_enabled() -> bool {
    match std::env::var_os("CCTEAM_TYPED_EVENTS") {
        Some(v) => {
            let v = v.to_string_lossy();
            let v = v.trim();
            v == "1" || v.eq_ignore_ascii_case("true")
        }
        None => false,
    }
}

/// Session-keyed registry of live taps' [`TapHandle`]s, so the global hook
/// sink (which sees `HookEvent { session_id, .. }`) can route P1 enrichment
/// to the correct per-session tap. Keyed by `"{slug}-{role}"` — the exact
/// `HookEvent::session_id` the chat hook emits.
///
/// Identity contract: a value is removed by its OWNING tap task only, and
/// only if the slot still holds *that* tap's `Arc<TapHandle>` (compared by
/// [`Arc::ptr_eq`]). This prevents a session reset — which ends one tap and
/// starts another for the same key — from having the OLD tap's teardown nuke
/// the NEW tap's freshly-inserted handle.
fn registry() -> &'static Mutex<HashMap<String, Arc<TapHandle>>> {
    static REG: OnceLock<Mutex<HashMap<String, Arc<TapHandle>>>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Map a Claude chat-progress hook `(kind, action)` to the merger event kind
/// it enriches. Slice 2: only `Stop` → `TurnDone` (single-in-flight, safe to
/// pair). All other actions return `None` (deferred — multi-in-flight pairing
/// needs the per-kind-counter redesign).
pub fn enrich_kind_for_chat_action(kind: &str, action: Option<&str>) -> Option<EventKind> {
    match (kind, action) {
        ("chat-progress", Some("stop")) => Some(EventKind::TurnDone),
        _ => None,
    }
}

/// Route a P1 enrichment to the tap registered for `session_key`. No-op if no
/// tap is registered (the session has no live tap, or typed events are off).
pub fn enrich_session(session_key: &str, kind: EventKind, payload: String) {
    let handle = {
        let reg = match registry().lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        reg.get(session_key).cloned()
    };
    if let Some(handle) = handle {
        handle.enrich(RawEnrichment { kind, payload });
    }
}

/// Entry point for the orchestrator's hook sink: translate a Claude
/// chat-progress [`ccteam_mux::HookEvent`] into a merger enrichment and route
/// it to the session's tap. No-op unless typed events are enabled and the
/// action maps to a paired kind.
pub fn enrich_session_from_hook(event: &ccteam_mux::HookEvent) {
    if !flag_enabled() {
        return;
    }
    if let Some(kind) = enrich_kind_for_chat_action(&event.kind, event.action.as_deref()) {
        enrich_session(&event.session_id, kind, event.payload_json.clone());
    }
}

/// Map a merger [`EventKind`] to a stable snake_case string for the row.
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

/// Stable vendor string for the row.
fn vendor_str(vendor: Vendor) -> &'static str {
    match vendor {
        Vendor::Claude => "claude",
        Vendor::Codex => "codex",
    }
}

/// Start a [`TypedEventTap`] for a freshly-live session and stream its merged
/// events into `progress_path`. `session_key` is `"{slug}-{role}"` (the hook
/// sink's `HookEvent::session_id`), used to register this tap's handle so the
/// hook sink can route enrichment to it.
///
/// No-op (returns immediately) unless [`flag_enabled`]. When enabled, spawns
/// a detached background task that ends when the session's stream closes (the
/// tap lingers one grace window first to drain pending fallbacks). Append
/// errors are logged at debug and skipped — never panics.
pub fn maybe_start_typed_event_tap(
    backend: Arc<dyn MuxBackend>,
    id: MuxSessionId,
    vendor: Vendor,
    session_key: String,
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
        // Hold the handle in the session registry so the orchestrator's hook
        // sink can route P1 enrichment (Slice 2). We keep our own `Arc` for
        // the identity-checked removal below.
        let handle = Arc::new(handle);
        {
            let mut reg = match registry().lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            reg.insert(session_key.clone(), handle.clone());
        }

        // Whether the merger's BaseLossy reliability fallback is meaningful:
        // only when hook enrichment is actually being routed to taps (else a
        // turn_done pattern with no enrichment ALWAYS goes BaseLossy, which
        // would spam spurious partials). Snapshot once at start.
        let lossy_meaningful = crate::hooks_dispatcher::hook_via_daemon_enabled();

        while let Some(ev) = rx.recv().await {
            let kind_for_row = match ev.outcome {
                // No-enrichment kinds (rate-limit / context-overflow / idle /
                // process-exit) — Slice 1 observability rows.
                MergeOutcome::BaseOnly => Some(("typed_event", ev.kind)),
                // A lossy pattern fired but the lossless hook never arrived
                // within the grace window — Slice 2 reliability fallback.
                // Only emit when enrichment was actually expected.
                MergeOutcome::BaseLossy if lossy_meaningful => {
                    Some(("merger_lossy_partial", ev.kind))
                }
                // Paired / EnrichmentOnly already have a lossless path that
                // owns the canonical progress row; BufferOverflow / unmet
                // BaseLossy are ignored.
                _ => None,
            };
            let Some((row_kind, kind)) = kind_for_row else {
                continue;
            };
            let captured = ev
                .base
                .as_ref()
                .map(|b| b.captured.clone())
                .unwrap_or_default();
            let row = match row_kind {
                "merger_lossy_partial" => crate::progress::build_merger_lossy_partial_event(
                    vendor_str(vendor),
                    event_kind_str(kind),
                    &captured,
                    &session,
                ),
                _ => crate::progress::build_typed_event_event(
                    vendor_str(vendor),
                    event_kind_str(kind),
                    &captured,
                    &session,
                ),
            };
            if let Err(err) = crate::progress::append_event(&progress_path, &row) {
                tracing::debug!(
                    error = %err,
                    session = %session,
                    "typed-event tap: failed to append {row_kind} row"
                );
            }
        }

        // Tap torn down (session gone). Remove our handle from the registry,
        // but only if the slot still holds OURS — a session reset may have
        // already replaced it with a newer tap's handle.
        let mut reg = match registry().lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        if reg
            .get(&session_key)
            .map(|h| Arc::ptr_eq(h, &handle))
            .unwrap_or(false)
        {
            reg.remove(&session_key);
        }
    });
}
