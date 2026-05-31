//! V0.8 rmux typed-event consumer — the first production consumer of the
//! [`ccteam_harness::EventMerger`].
//!
//! Bridges a live Claude chat-TUI session's [`ccteam_harness::TypedEventTap`]
//! into the project's `progress.jsonl`. Three slices:
//!
//! - **Slice 1 — `BaseOnly` pipeline** (flag: `CCTEAM_TYPED_EVENTS`): the
//!   no-enrichment kinds (rate-limit / context-overflow / idle /
//!   process-exit) are mirrored as `typed_event` observability rows.
//! - **Slice 2 — pairing + `BaseLossy` reliability fallback** (additionally
//!   requires `CCTEAM_HOOK_VIA_DAEMON`, so the orchestrator's hook sink can
//!   feed P1 enrichment): each tap's [`ccteam_harness::TapHandle`] is held in a
//!   session-keyed registry; the hook sink routes a Claude `Stop` hook to
//!   the matching tap as a `TurnDone` enrichment via
//!   [`enrich_session_from_hook`]. When a `turn_done` pane pattern fired but
//!   no `Stop` hook arrived within the merger's grace window, the merger
//!   emits `BaseLossy` and we write a `merger_lossy_partial` row — surfacing
//!   a turn whose lossless hook was lost (e.g. a crashed hook subprocess).
//! - **Slice 3 — multi-in-flight kinds.** Adds `user-prompt` →
//!   [`EventKind::UserPromptSubmitted`] and `tool-use` (Claude's
//!   `PostToolUse`) → [`EventKind::ToolCallCompleted`] to the
//!   chat-progress hook → enrichment routing in [`enrich_kind_for_chat_action`].
//!   Multi-in-flight pairing is robust against cascade-mispair via the
//!   `TypedEventTap::SeqState`'s time-windowed FIFO drop-stale-on-mint
//!   (see `docs/versions/v0-8-rmux/w-slice-3-multi-in-flight-pairing.md`).
//! - **Slice 4 — `ToolCallStarted` wiring.** Adds `pre-tool-use` (Claude's
//!   `PreToolUse`) → [`EventKind::ToolCallStarted`]. The chat-progress
//!   installer at `crates/ccteam-harness/src/execution/claude_tui.rs:126-135`
//!   now registers a `PreToolUse` entry; the hook writes a
//!   `chat_tool_call_started` row and routes a `ToolCallStarted`
//!   enrichment into the tap. Pre-rerun-of-`ccteam doctor --install-hooks`
//!   projects keep working with no `pre-tool-use` firing — the new
//!   mapping is unreachable until the user reinstalls hooks.
//!
//! Both progress rows (`typed_event`, `merger_lossy_partial`) share the same
//! JSON field layout (`vendor` / `event_kind` / `captured` / `session` /
//! `ts`) so a downstream tool can parse either with one struct, branching on
//! `kind`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use crate::execution::progress_bridge::{
    append_event, build_merger_lossy_partial_event, build_typed_event_event,
};
use crate::{
    EventKind, MergeOutcome, MuxSessionId, RawEnrichment, TapHandle, TerminalProcessBackend,
    TypedEventTap, Vendor, DEFAULT_GRACE,
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
/// it enriches.
///
/// The chat-progress installer at
/// `crates/ccteam-harness/src/execution/claude_tui.rs:126-135` uses kebab-case
/// action strings (`stop`, `user-prompt`, `tool-use`, `session-start`,
/// `subagent-stop`, `session-end`, `pre-compact`, `post-compact`). We map the
/// subset that has a merger [`EventKind`] today:
///
/// | action | EventKind | Slice |
/// |---|---|---|
/// | `stop` | [`EventKind::TurnDone`] | 2 (single-in-flight) |
/// | `user-prompt` | [`EventKind::UserPromptSubmitted`] | 3 (multi-in-flight; see [`ccteam_harness::TypedEventTap`] time-windowed FIFO) |
/// | `tool-use` | [`EventKind::ToolCallCompleted`] | 3 (multi-in-flight; pairs `PostToolUse` hook with the `^\s*⎿` pane glyph) |
/// | `pre-tool-use` | [`EventKind::ToolCallStarted`] | 4 (multi-in-flight; pairs `PreToolUse` hook with the `^●\s+(\w+)\(` pane regex) |
///
/// Returns `None` for unmapped actions (no merger kind, e.g. `subagent-stop`,
/// `session-end`, `pre-compact`, `post-compact`; or `session-start` whose
/// canonical signal is the pane `welcome to claude code` regex →
/// `SessionReset` already at the base level).
pub fn enrich_kind_for_chat_action(kind: &str, action: Option<&str>) -> Option<EventKind> {
    match (kind, action) {
        ("chat-progress", Some("stop")) => Some(EventKind::TurnDone),
        ("chat-progress", Some("user-prompt")) => Some(EventKind::UserPromptSubmitted),
        ("chat-progress", Some("tool-use")) => Some(EventKind::ToolCallCompleted),
        ("chat-progress", Some("pre-tool-use")) => Some(EventKind::ToolCallStarted),
        _ => None,
    }
}

/// Extract the **identity cohort** for a hook payload (Slice 4).
///
/// For `ToolCallStarted` / `ToolCallCompleted` the identity is the Claude
/// hook payload's `tool_name` field; the tap routes it into the merger's
/// pairing predicate so two parallel tool calls of different tools never
/// cross-pair (see `docs/versions/v0-8-rmux/w-slice-4-identity-and-codex.md`).
/// For every other kind (and any malformed / missing-field payload)
/// returns `None`, falling back to the pre-Slice-4 per-kind FIFO behaviour
/// exactly. Forgiving by design — a future hook payload shape drift
/// surfaces as `None`, not a panic or a poisoned cohort.
pub fn identity_for(kind: EventKind, payload_json: &str) -> Option<String> {
    match kind {
        EventKind::ToolCallStarted | EventKind::ToolCallCompleted => {
            let value: serde_json::Value = serde_json::from_str(payload_json).ok()?;
            value
                .get("tool_name")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
        }
        _ => None,
    }
}

/// Route a P1 enrichment to the tap registered for `session_key`. No-op if no
/// tap is registered (the session has no live tap, or typed events are off).
///
/// `identity` is the Slice-4 cohort key — call [`identity_for`] to derive
/// it from the hook payload when routing tool-call enrichments. Pass `None`
/// for kinds without identity (TurnDone / UserPromptSubmitted / etc).
pub fn enrich_session(
    session_key: &str,
    kind: EventKind,
    identity: Option<String>,
    payload: String,
) {
    let handle = {
        let reg = match registry().lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        reg.get(session_key).cloned()
    };
    if let Some(handle) = handle {
        handle.enrich(RawEnrichment {
            kind,
            identity,
            payload,
        });
    }
}

/// Entry point for the orchestrator's hook sink: translate a Claude
/// chat-progress [`crate::HookEvent`] into a merger enrichment and route
/// it to the session's tap. No-op unless typed events are enabled and the
/// action maps to a paired kind.
pub fn enrich_session_from_hook(event: &crate::HookEvent) {
    if !flag_enabled() {
        return;
    }
    if let Some(kind) = enrich_kind_for_chat_action(&event.kind, event.action.as_deref()) {
        let identity = identity_for(kind, &event.payload_json);
        enrich_session(
            &event.session_id,
            kind,
            identity,
            event.payload_json.clone(),
        );
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
    backend: Arc<dyn TerminalProcessBackend>,
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
        let lossy_meaningful = std::env::var("CCTEAM_HOOK_VIA_DAEMON")
            .map(|v| v == "1")
            .unwrap_or(false);

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
                "merger_lossy_partial" => build_merger_lossy_partial_event(
                    vendor_str(vendor),
                    event_kind_str(kind),
                    &captured,
                    &session,
                ),
                _ => build_typed_event_event(
                    vendor_str(vendor),
                    event_kind_str(kind),
                    &captured,
                    &session,
                ),
            };
            if let Err(err) = append_event(&progress_path, &row) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slice_2_stop_maps_to_turn_done() {
        assert_eq!(
            enrich_kind_for_chat_action("chat-progress", Some("stop")),
            Some(EventKind::TurnDone)
        );
    }

    #[test]
    fn slice_3_user_prompt_maps_to_user_prompt_submitted() {
        assert_eq!(
            enrich_kind_for_chat_action("chat-progress", Some("user-prompt")),
            Some(EventKind::UserPromptSubmitted)
        );
    }

    #[test]
    fn slice_3_tool_use_maps_to_tool_call_completed() {
        assert_eq!(
            enrich_kind_for_chat_action("chat-progress", Some("tool-use")),
            Some(EventKind::ToolCallCompleted)
        );
    }

    #[test]
    fn unmapped_chat_progress_actions_return_none() {
        // Sentinel: these are installed by the chat hook table but have no
        // merger kind today. Listing them explicitly so a future mapping
        // change is an opt-in edit, not a silent drop.
        for action in [
            "session-start",
            "subagent-stop",
            "session-end",
            "pre-compact",
            "post-compact",
        ] {
            assert_eq!(
                enrich_kind_for_chat_action("chat-progress", Some(action)),
                None,
                "{action} should not map to a merger EventKind today",
            );
        }
    }

    #[test]
    fn unknown_kind_returns_none() {
        assert_eq!(
            enrich_kind_for_chat_action("progress-append", Some("stop")),
            None,
            "progress-append is a different dispatch kind; must not collide",
        );
        assert_eq!(enrich_kind_for_chat_action("chat-progress", None), None);
        assert_eq!(enrich_kind_for_chat_action("", Some("stop")), None);
    }

    #[test]
    fn slice_4_pre_tool_use_maps_to_tool_call_started() {
        // V0.8 Slice 4: PreToolUse is now registered in the chat-progress
        // installer (claude_tui.rs:126-135) and the hook subprocess emits
        // a `chat_tool_call_started` row + routes a `ToolCallStarted`
        // enrichment through this mapper. Previous Slice-3 guard was
        // "unmapped until installer adds it" — installer added it, so
        // this assertion is now positive.
        assert_eq!(
            enrich_kind_for_chat_action("chat-progress", Some("pre-tool-use")),
            Some(EventKind::ToolCallStarted)
        );
    }

    /// Slice 4 — identity is extracted only for tool-call kinds; every
    /// other kind returns `None` regardless of payload contents.
    #[test]
    fn identity_for_tool_call_kinds_extracts_tool_name() {
        let payload = r#"{"tool_name":"Edit","tool_input":{"file_path":"/foo"}}"#;
        assert_eq!(
            identity_for(EventKind::ToolCallStarted, payload),
            Some("Edit".to_string())
        );
        assert_eq!(
            identity_for(EventKind::ToolCallCompleted, payload),
            Some("Edit".to_string())
        );
    }

    #[test]
    fn identity_for_non_tool_kinds_returns_none() {
        let payload = r#"{"tool_name":"Edit"}"#;
        // A payload that happens to carry tool_name does NOT poison
        // other kinds — identity is only meaningful for tool-call kinds.
        assert_eq!(identity_for(EventKind::TurnDone, payload), None);
        assert_eq!(identity_for(EventKind::UserPromptSubmitted, payload), None);
        assert_eq!(identity_for(EventKind::SessionReset, payload), None);
        assert_eq!(identity_for(EventKind::RateLimitHit, payload), None);
    }

    /// Slice 4 §"Risks" R5 — forgiving by design. A malformed payload, a
    /// missing field, an empty string, or a non-string tool_name all
    /// degrade to `None` so the cohort gracefully falls back to FIFO
    /// instead of a panic or a poisoned `Some("")` bucket.
    #[test]
    fn identity_for_malformed_payload_returns_none() {
        // Not valid JSON.
        assert_eq!(
            identity_for(EventKind::ToolCallStarted, "not json at all"),
            None
        );
        // Empty object — no tool_name field.
        assert_eq!(identity_for(EventKind::ToolCallStarted, "{}"), None);
        // tool_name is null.
        assert_eq!(
            identity_for(EventKind::ToolCallStarted, r#"{"tool_name":null}"#),
            None
        );
        // tool_name is a non-string.
        assert_eq!(
            identity_for(EventKind::ToolCallStarted, r#"{"tool_name":42}"#),
            None
        );
        // tool_name is an empty string — refuse to mint a `Some("")` cohort.
        assert_eq!(
            identity_for(EventKind::ToolCallStarted, r#"{"tool_name":""}"#),
            None
        );
    }
}
