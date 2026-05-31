//! V0.8 rmux Slice 4 — Codex mode-3 typed-event producer.
//!
//! Parallels [`crate::execution::typed_events`] (Claude side) but with
//! one critical architectural divergence: **we do NOT drive
//! [`crate::EventMerger`]**.
//!
//! Why bypass the merger? The merger exists to *pair* a lossy P2 base
//! event (pane regex match) with a lossless P1 enrichment (Claude hook
//! payload). Codex mode-3 has only the lossless P1 side — `app-server`
//! JSON-RPC notifications. There is no ProcessBackend pane to regex on.
//! Going through the merger would push every notification into
//! `pending_enrichment` waiting for a base that will never arrive;
//! `BUFFER_CAPACITY=64` would silently FIFO-evict the front
//! ([`crate::EventMerger`] internals — see
//! `enriched_event.rs:481-487`), so a busy turn would drop events with
//! no visible signal. Direct writes from the JSON-RPC subscriber
//! eliminate the leak.
//!
//! Because Codex's `item/started` and `item/completed` notifications
//! carry `item_id` (a stable Codex invocation id), the row's `captured`
//! field carries that id — downstream tools can correlate the two rows
//! without our needing any pairing layer on this side. No registry,
//! no second writer to coordinate with (Claude's registry exists to
//! receive enrichment from the hook subprocess; Codex's notifications
//! arrive directly on our broadcast receiver).
//!
//! Both Claude (`typed_events`) and Codex (this module) emit
//! `typed_event` rows with the same JSON field layout (`vendor` /
//! `event_kind` / `captured` / `session` / `ts`) so a downstream tool
//! parses either with one struct, branching on `vendor`.

use std::path::PathBuf;
use std::sync::Arc;

use crate::EventKind;
use serde_json::Value;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use crate::execution::codex_jsonrpc::{CodexJsonRpcClient, Notification};
use crate::execution::progress_bridge::{append_event, build_typed_event_event};

/// True when `CCTEAM_TYPED_EVENTS` is set to a truthy value (`1` /
/// `true`). Mirrors [`crate::execution::typed_events::flag_enabled`] so
/// both Claude and Codex producers are gated on the same flag.
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

/// Translate a Codex JSON-RPC notification into a `(kind, captured)`
/// pair suitable for a `typed_event` row. Returns `None` for any method
/// we don't want to emit a row for (noisy stream deltas, plan updates
/// whose semantics differ from Claude `PlanPending`, unknown methods).
///
/// Mapping per design `docs/versions/v0-8-rmux/w-slice-4-identity-and-codex.md`
/// §"Notification → EventKind mapping":
///
/// | method | EventKind | captured |
/// |---|---|---|
/// | `item/started` | `ToolCallStarted` | `params.item_id` (empty if absent) |
/// | `item/completed` | `ToolCallCompleted` | `params.item_id` |
/// | `turn/started` | `UserPromptSubmitted` | `""` |
/// | `turn/completed` | `TurnDone` | `""` |
/// | `thread/started` | `SessionReset` | `""` |
/// | `thread/compacted` | `CompactDone` | `""` |
/// | `turn/plan/updated` | **skip** (semantics differ from Claude `PlanPending`) |
/// | `item/agentMessage/delta` | **skip** (too noisy) |
/// | anything else | **skip** |
fn notif_to_kind_and_captured(notif: &Notification) -> Option<(EventKind, String)> {
    let item_id = || {
        notif
            .params
            .get("item_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    match notif.method.as_str() {
        "item/started" => Some((EventKind::ToolCallStarted, item_id())),
        "item/completed" => Some((EventKind::ToolCallCompleted, item_id())),
        "turn/started" => Some((EventKind::UserPromptSubmitted, String::new())),
        "turn/completed" => Some((EventKind::TurnDone, String::new())),
        "thread/started" => Some((EventKind::SessionReset, String::new())),
        "thread/compacted" => Some((EventKind::CompactDone, String::new())),
        _ => None,
    }
}

/// Stable snake_case string for the row's `event_kind` field. Mirrors
/// the Claude side's mapping so both vendors share a downstream parser.
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

/// Inner consumer loop. Pulled out of the spawn so tests can drive it
/// with a hand-rolled broadcast channel without needing a real
/// [`CodexJsonRpcClient`]. Exits when the broadcast sender drops
/// (`RecvError::Closed`); logs `Lagged(n)` at `warn` and keeps going so
/// a brief receiver back-pressure event doesn't terminate the producer.
///
/// `#[doc(hidden)]` because integration tests need this entrypoint;
/// production code should always go through
/// [`maybe_start_codex_typed_event_tap`].
#[doc(hidden)]
pub async fn run_loop(mut rx: broadcast::Receiver<Notification>, progress_path: PathBuf) {
    loop {
        match rx.recv().await {
            Ok(notif) => {
                let Some((kind, captured)) = notif_to_kind_and_captured(&notif) else {
                    continue;
                };
                // No `session` value to thread through — the Codex
                // adapter doesn't carry the Claude-style
                // `"{slug}-{role}"` session_key here. Downstream tools
                // pair rows by `progress.jsonl` file path (one file per
                // slug) + `vendor`.
                let row = build_typed_event_event("codex", event_kind_str(kind), &captured, "");
                if let Err(err) = append_event(&progress_path, &row) {
                    tracing::debug!(
                        error = %err,
                        method = %notif.method,
                        "codex typed-event tap: failed to append typed_event row"
                    );
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!(
                    lagged = n,
                    "codex typed-event tap: broadcast receiver lagged; dropped {n} notifications"
                );
                continue;
            }
            Err(broadcast::error::RecvError::Closed) => {
                tracing::debug!("codex typed-event tap: broadcast closed; producer exiting");
                break;
            }
        }
    }
}

/// Spawn the Codex mode-3 typed-event producer for a freshly-started
/// thread. No-op (returns `None`) when [`flag_enabled`] is false.
///
/// Called from `CodexAppServerAdapter::start_thread` just after the
/// progress bridge is registered. The producer task ends when the
/// underlying JSON-RPC notification broadcast closes (i.e. when the
/// thread / connection ends).
pub fn maybe_start_codex_typed_event_tap(
    jsonrpc: Arc<CodexJsonRpcClient>,
    progress_path: PathBuf,
) -> Option<JoinHandle<()>> {
    if !flag_enabled() {
        return None;
    }
    let rx = jsonrpc.subscribe();
    Some(tokio::spawn(run_loop(rx, progress_path)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn flag_disabled_when_env_unset() {
        // Don't mutate the env in unit tests; just verify the mapper.
        // The env-mutation behaviour is integration-tested where
        // process isolation makes it safe.
        let _ = flag_enabled();
    }

    #[test]
    fn unmapped_methods_return_none() {
        for method in [
            "turn/plan/updated",
            "item/agentMessage/delta",
            "thread/tokenUsage/updated",
            "totally/unknown",
            "",
        ] {
            assert_eq!(
                notif_to_kind_and_captured(&Notification {
                    method: method.to_string(),
                    params: json!({}),
                }),
                None,
                "{method} should be unmapped",
            );
        }
    }

    #[test]
    fn item_started_extracts_item_id() {
        let n = Notification {
            method: "item/started".into(),
            params: json!({ "item_id": "i-42" }),
        };
        let (kind, captured) = notif_to_kind_and_captured(&n).unwrap();
        assert_eq!(kind, EventKind::ToolCallStarted);
        assert_eq!(captured, "i-42");
    }

    #[test]
    fn item_completed_extracts_item_id() {
        let n = Notification {
            method: "item/completed".into(),
            params: json!({ "item_id": "i-99", "result": "ok" }),
        };
        let (kind, captured) = notif_to_kind_and_captured(&n).unwrap();
        assert_eq!(kind, EventKind::ToolCallCompleted);
        assert_eq!(captured, "i-99");
    }

    #[test]
    fn item_started_without_item_id_emits_empty_captured() {
        let n = Notification {
            method: "item/started".into(),
            params: json!({}),
        };
        let (kind, captured) = notif_to_kind_and_captured(&n).unwrap();
        assert_eq!(kind, EventKind::ToolCallStarted);
        assert_eq!(captured, "");
    }

    #[test]
    fn turn_completed_maps_to_turn_done() {
        let n = Notification {
            method: "turn/completed".into(),
            params: json!({}),
        };
        let (kind, captured) = notif_to_kind_and_captured(&n).unwrap();
        assert_eq!(kind, EventKind::TurnDone);
        assert_eq!(captured, "");
    }

    #[test]
    fn turn_started_maps_to_user_prompt_submitted() {
        let n = Notification {
            method: "turn/started".into(),
            params: json!({}),
        };
        let (kind, _) = notif_to_kind_and_captured(&n).unwrap();
        assert_eq!(kind, EventKind::UserPromptSubmitted);
    }

    #[test]
    fn thread_started_maps_to_session_reset() {
        let n = Notification {
            method: "thread/started".into(),
            params: json!({ "thread_id": "t-1" }),
        };
        let (kind, _) = notif_to_kind_and_captured(&n).unwrap();
        assert_eq!(kind, EventKind::SessionReset);
    }

    #[test]
    fn thread_compacted_maps_to_compact_done() {
        let n = Notification {
            method: "thread/compacted".into(),
            params: json!({}),
        };
        let (kind, _) = notif_to_kind_and_captured(&n).unwrap();
        assert_eq!(kind, EventKind::CompactDone);
    }
}
