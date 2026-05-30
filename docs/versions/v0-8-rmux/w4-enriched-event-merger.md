# W4 Design Note — EnrichedEvent merger algorithm

> Sharpens the §15.5 EnrichedEvent design with a concrete algorithm. Implementation lands in `crates/ccteam-mux/src/enriched_event.rs` during W4.

## The semantic problem

A single user-visible event (e.g. "Claude called the Read tool with path /foo") can be detected from multiple sources:

| Source priority | Channel | Information quality |
|---|---|---|
| **P1 — highest** | Codex JSON-RPC event | Lossless (Layer 4 typed) |
| P1 — highest | Claude Code hook subprocess | Lossless (Layer 4 typed via Anthropic) |
| P2 | rmux `line_stream` + registered regex pattern | Lossy (Layer 2; TUI render) |
| P3 — fallback | rmux process events (`OutputIdle`, `ProcessExited`) | Process-level only |

The merger's job: **emit at most one `progress.jsonl` event per logical occurrence**, sourcing the richest available channel, while remaining responsive when only the lower-priority channel fires.

## Algorithm: priority-with-grace-window

```
on receive base_event (P2 PatternMatched or P3 process):
  if event_kind has known enrichment source (P1):
    wait up to GRACE_MS for matching enrichment
    if enrichment arrived:
      emit Enriched(base + enrichment.payload)
      consume the matched enrichment from queue
    else:
      emit Base(base, lossy=true)
  else:
    emit Base(base)

on receive enrichment_event (P1 hook / JSON-RPC):
  if matching base in recent_base_buffer (timestamp within GRACE_MS):
    pair them: emit Enriched(base + enrichment.payload)
    consume the matched base
  else:
    emit EnrichmentOnly(enrichment)
    (the base may still arrive later; in that case base self-suppresses
     if its sequence_id matches a recently-consumed enrichment)
```

Choice of `GRACE_MS`:
- Too low (e.g., 50ms): hook subprocess fork + UDS connect latency can exceed; we lose enrichments
- Too high (e.g., 2000ms): orchestrator stalls on real-time events
- **Recommendation: 500ms** — covers cold-start hook subprocess fork (~100-300ms observed on dev machines), still feels real-time to humans/IM bots downstream

## Pairing key

For event-kind matches, we use **(session_id, event_kind, sequence_id)**:

- `session_id` from MuxSessionId
- `event_kind` is the logical event class (`tool_call_started`, `permission_prompt_open`, `assistant_message_complete`, etc.)
- `sequence_id` is a monotonic counter per session per kind:
  - For P2 base events, daemon increments on each pattern match
  - For P1 enrichment events, the hook subprocess reads the kind-specific counter via a daemon RPC (`get_next_seq(session_id, kind)`) on event emission. Claude Code hook fires synchronously after the TUI render starts, so base usually arrives first, but order doesn't matter — sequence_id is the join key.

For Codex (no hook subprocess; daemon's CodexUdsBridge ingests events directly), the bridge assigns sequence_ids on event arrival.

## Backpressure / overflow

- Per-session `recent_base_buffer`: ring buffer, capacity 64 events, age-out at 2 × GRACE_MS
- If buffer overflows (sustained rate > 128 events/sec, unlikely): drop oldest base; emit `BufferOverflow` typed event for observability
- Per-session `pending_enrichment_buffer`: same shape, capacity 64, age-out at 2 × GRACE_MS

## Edge cases

| Case | Behavior |
|---|---|
| Base fires, enrichment never arrives (hook subprocess crashed) | After GRACE_MS, emit `Base(lossy=true)`; orchestrator translates to `*_partial` progress.jsonl variant |
| Enrichment fires, base never arrives (pattern regex regression) | Emit `EnrichmentOnly`; orchestrator treats as fully lossless (P1 was always authoritative) |
| Multiple base events of same kind in rapid succession (e.g., 3 tool calls in 100ms) | Each gets its own sequence_id; merger pairs by exact sequence |
| Daemon restart mid-pairing | All buffers reset; both sides re-fire from fresh start; brief duplication possible — orchestrator dedups by `(session_id, kind, sequence_id, ts)` tuple if needed |
| Enrichment arrives well past GRACE_MS due to network hiccup | `EnrichmentOnly` emitted; orchestrator may receive an unmatched enrichment; treats as supplementary metadata, not a duplicate event |

## Per-event-kind dispatch table

In `crates/ccteam-mux/src/patterns/dispatch.rs`:

```rust
pub fn enrichment_source(kind: &EventKind, vendor: Vendor) -> EnrichmentSource {
    use EventKind::*;
    match (kind, vendor) {
        (ToolCallStarted, Vendor::Claude) => EnrichmentSource::ClaudeHook("pre_tool_use"),
        (ToolCallStarted, Vendor::Codex)  => EnrichmentSource::CodexJsonRpc("notification/tool_call_begin"),
        (ToolCallCompleted, Vendor::Claude) => EnrichmentSource::ClaudeHook("post_tool_use"),
        (ToolCallCompleted, Vendor::Codex)  => EnrichmentSource::CodexJsonRpc("notification/tool_call_end"),
        (UserPromptSubmitted, Vendor::Claude) => EnrichmentSource::ClaudeHook("user_prompt_submit"),
        (UserPromptSubmitted, Vendor::Codex)  => EnrichmentSource::CodexJsonRpc("notification/user_prompt"),
        // ... etc for all 10 base patterns
        (RateLimitHit, _) => EnrichmentSource::None, // P2 base is sufficient
        (ContextOverflow, _) => EnrichmentSource::None,
        (Idle, _) => EnrichmentSource::None,
        (ProcessExited, _) => EnrichmentSource::None,
    }
}
```

Events with `EnrichmentSource::None` skip the grace window entirely — emit immediately from base.

## API surface

```rust
// crates/ccteam-mux/src/enriched_event.rs

pub struct EnrichedEvent {
    pub session_id: MuxSessionId,
    pub kind: EventKind,
    pub timestamp: SystemTime,
    pub sequence_id: u64,
    pub base: BasePayload,         // always present
    pub enrichment: Option<EnrichmentPayload>, // None = lossy fallback or no enrichment source
}

pub struct EventMerger {
    // private state: dual buffers + GRACE_MS timer
}

impl EventMerger {
    pub fn new(grace: Duration) -> Self { ... }
    pub async fn process_base(&self, base: BaseEvent) -> Option<EnrichedEvent>;
    pub async fn process_enrichment(&self, enrich: EnrichmentEvent) -> Option<EnrichedEvent>;
    pub async fn flush_pending(&self) -> Vec<EnrichedEvent>; // call on shutdown
}
```

## Why this is simpler than initially feared

Reading research doc §15.5 again, the merger sounds heavy ("by timestamp + sequence ID, ±2s window"). The actual algorithm is **a priority queue with a grace window** — 100-150 LOC of Rust including tests. Most of the complexity lives in the dispatch table (one entry per known event-kind × vendor pair).

## W4 acceptance

- `crates/ccteam-mux/src/enriched_event.rs` implements `EventMerger`
- 10 base pattern entries × 2 vendors = 20 dispatch table entries
- Integration test: drive 100 paired (base + enrichment) through merger in 1s, verify all 100 paired correctly
- Edge test: 100 base-only (enrichment never arrives) verified — all 100 emit as `Base(lossy=true)` after GRACE_MS
- Edge test: 100 enrichment-only verified — all 100 emit as `EnrichmentOnly` immediately
- Race test: rapid-fire 1000 (50ms apart) pairs across 10 sessions, verify zero crosstalk between sessions

## V0.6.8 progress.jsonl event types this merger feeds

Per inventory of `crates/ccteam-core/src/progress.rs` 15 event constants:

| progress.jsonl event | Source layer | Merger handles? |
|---|---|---|
| `chat_session_started` | Claude hook (session_start) | Yes — enrichment-only path |
| `chat_turn_user_prompt` | Claude hook (user_prompt_submit) | Yes — paired with P2 `>` prompt regex |
| `chat_turn_completed` | Claude hook (stop) | Yes — paired with P2 idle-after-spinner regex |
| `chat_session_reset` | Claude hook + `/new /compact` pattern | Yes |
| `chat_session_reset_with_recovery` | turns.jsonl tail | NOT via merger — orchestrator-internal |
| `chat_compact_done` | Claude hook | Yes |
| `chat_hop_escalate` | orchestrator internal (fix_counts) | NOT via merger |
| `chat_bot_permanent_failure` | rmux ProcessExited (P3) | Yes — base-only |
| `chat_marker_self_heal_attempt` (F196) | orchestrator internal | NOT via merger |
| `chat_bot_marker_stuck` (F195) | orchestrator internal timer | NOT via merger |
| `chat_turn_running_long` | orchestrator internal timer | NOT via merger |
| `chat_turn_timeout` | orchestrator internal timer | NOT via merger |
| `plan_pending` (F124) | Claude hook | Yes |
| `plan_decision` | IM round-trip | NOT via merger |
| `plan_timeout` | orchestrator internal timer | NOT via merger |

**8 of 15 events flow through the merger; 7 are orchestrator-internal and stay as-is**. The merger is significantly narrower than initially scoped.
