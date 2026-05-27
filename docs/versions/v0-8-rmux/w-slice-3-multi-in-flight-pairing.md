# Slice 3 — multi-in-flight typed-event pairing

> Robustness extension to the EnrichedEvent pipeline (Slice 2 base/enrich pairing).
> Scope: extend the daemon-side `TypedEventTap` and the orchestrator's
> `enrich_session_from_hook` to cover multi-in-flight chat-progress hooks
> (`tool-use`, `user-prompt`) without cascade mis-pair under enrichment loss.
> Flag-gating is unchanged (`CCTEAM_TYPED_EVENTS=1` + `CCTEAM_HOOK_VIA_DAEMON=1`).

## The problem (recap)

The merger pairs by `(session_id, kind, sequence_id)`. The tap mints
`sequence_id` in `typed_event_tap::SeqState`, today via a per-`EventKind`
**FIFO** of "pending bases waiting for an enrichment" and the symmetric
queue. For **single-in-flight** kinds (TurnDone — only one turn live per
session) this is safe. For **multi-in-flight** kinds it cascade-mispairs
under enrichment loss:

```
base[0]  → pending_base[kind] = [seq=0]
base[1]  → pending_base[kind] = [seq=0, seq=1]
(enrich[0] never arrives — hook subprocess crashed / dropped)
base[2]  → pending_base[kind] = [seq=0, seq=1, seq=2]
enrich[2]  → pops front → seq=0 → merger pairs the live (base[2]) hook with
             the long-parked (base[0]) pane match. WRONG PAIR.
```

The merger itself has a grace-window drop (`BaseLossy` after `grace`
elapses with no enrichment), but **the tap's `SeqState` has no such drop**
— stale entries stay in `pending_base[kind]` until something pops them,
and FIFO guarantees the wrong thing is popped under loss.

There is a secondary symptom on no-enrichment kinds (`RateLimitHit`,
`ContextOverflow`, `Idle`, `ProcessExited`): `mint_base` is called but
`mint_enrich` is never called → `pending_base[kind]` grows unbounded
over a long session (small leak, exposed by the same fix).

## The fix — time-windowed FIFO

Tag every pending slot with the wall-clock instant it was minted, and
drop stale slots **from the OPPOSITE queue** at the front of every
`mint_*` call. The kill window is the same `Duration` the merger uses
for its `BaseLossy` grace — once a slot is older than `grace`, the
merger has already aged out (or would have) the corresponding parked
base, so consuming that stale seq would always mis-pair.

```rust
// crates/ccteam-mux/src/typed_event_tap.rs
#[derive(Debug, Clone, Copy)]
struct PendingSlot {
    seq: u64,
    arrived_at: tokio::time::Instant,  // honours `tokio::time::pause()`
}

struct SeqState {
    next_seq: u64,
    grace: Duration,
    pending_base:   HashMap<EventKind, VecDeque<PendingSlot>>,
    pending_enrich: HashMap<EventKind, VecDeque<PendingSlot>>,
}

impl SeqState {
    fn new(grace: Duration) -> Self { /* ... */ }

    /// Pop every slot whose `arrived_at + grace < now` from the FRONT
    /// (FIFO → stale slots are always the oldest).
    fn drop_stale(q: &mut VecDeque<PendingSlot>, now: Instant, grace: Duration) {
        while let Some(front) = q.front() {
            if now.saturating_duration_since(front.arrived_at) > grace {
                q.pop_front();
            } else { break; }
        }
    }

    fn mint_base(&mut self, kind: EventKind) -> u64 {
        let now = Instant::now();
        if let Some(q) = self.pending_enrich.get_mut(&kind) {
            Self::drop_stale(q, now, self.grace);
            if let Some(slot) = q.pop_front() { return slot.seq; }
        }
        let s = self.next_seq; self.next_seq += 1;
        self.pending_base.entry(kind).or_default()
            .push_back(PendingSlot { seq: s, arrived_at: now });
        s
    }
    // mint_enrich symmetric.
}
```

### Why this is sufficient

| Failure mode | Recovery |
|---|---|
| Enrich lost, next pair **after** grace | `pending_base[kind]` front is stale → `drop_stale` pops it → next enrich pops a fresh in-window slot. No cascade. |
| Enrich lost, next pair **within** grace | Single-pair mis-pair within the grace window. FIFO re-aligns after grace. The merger emits `BaseLossy` for the orphaned stale base separately. |
| Base lost, next enrich **after** grace | Symmetric: `pending_enrich[kind]` front is stale → dropped → fresh seq → merger emits `EnrichmentOnly`. No cascade. |
| Base lost, next enrich **within** grace | **Silent-suppression race** — see §"Known within-grace races" below. |
| Two enriches lost in a row | After grace, `drop_stale` pops every stale entry in one pass; merger emits both as `BaseLossy`. No cascade. |
| No-enrichment kinds (Idle, RateLimitHit, ...) | `pending_base[kind]` slots age out via `drop_stale` on the next base mint of that kind. Bounded. |

### Known within-grace races

1. **Lost-enrich, next-base within grace** — the next enrich pops the lost slot's seq → merger pairs the live base's pane match with the live enrich's hook → one wrong `Paired`; the now-orphaned stale base ages out as `BaseLossy` after its own grace. **Net: one mis-attributed `Paired` + one extra `merger_lossy_partial` per dropped enrich, then FIFO re-aligns.**

2. **Lost-base, next-enrich within grace** — `mint_enrich` returns a fresh seq (no pending_base entry to pop) → merger emits `EnrichmentOnly` + parks in `pending_enrichment`. When the *next* real base arrives, `mint_base` pops the parked enrich's seq → merger sees the matching parked `EnrichmentOnly` slot and **self-suppresses the base** (`enriched_event.rs:360-373` "base self-suppresses if its sequence_id matches a recently-consumed enrichment"). Net: the next real base's pane payload is silently dropped instead of pairing. Recovery: after grace, the parked enrich is aged out by `drop_stale` on the next mint_enrich, and FIFO re-aligns. The cascade is bounded to one extra dropped occurrence per lost base.

3. **Parallel tool calls (kind = `ToolCallCompleted`)** — Claude can fire multiple tool calls in one turn (e.g. `Edit` + `Read` issued together). The Claude TUI pane completion glyph `⎿` is emitted in **completion order** (whichever tool finishes first); the `PostToolUse` chat-progress hook also fires in completion order. **Generally aligned.** Edge: if Claude renders an inline `⎿` line for a still-running tool (mid-stream progress) before its hook fires, pane order and callback order can diverge. The base pattern captures only the `⎿` glyph (no tool name, see `patterns/claude.rs:35-38`), so a wrong pair silently corrupts `chat_tool_use.tool` in any downstream that consumes the merged event's enrichment. **Identity-based pairing (tool_name in both sides) is the fix; out of scope here. Slice 3 ships best-effort FIFO and the §"Acceptance" suite covers the aligned-order common case.**

### Why not identity-based pairing?

A content-derived identity (tool_name + arg-hash; prompt-hash) would
eliminate the within-grace mis-pair window. **It is infeasible for
`ToolCallCompleted`** because the Claude TUI base pattern captures only
the `⎿` continuation glyph — no tool name. Carrying identity would
require either a richer pane regex (the Claude TUI render does not
always include the tool name on the result line) or threading identity
across vendor-specific channels. Out of scope for Slice 3; revisit if
the within-grace mis-pair rate is non-trivial in production.

## Consumer wiring (chat-progress mappings)

The orchestrator-side `enrich_kind_for_chat_action` (in
`ccteam-core/src/execution/typed_events.rs`) maps the chat-progress hook
`action` arg onto a merger `EventKind`. Hook action strings are
**kebab-case** (canonical table at
`crates/ccteam-core/src/execution/claude_tui.rs:126-135`):

| chat-progress action | EventKind today | EventKind after Slice 3 |
|---|---|---|
| `stop` | `TurnDone` | unchanged (Slice 2) |
| `user-prompt` | — | **`UserPromptSubmitted`** |
| `tool-use` (PostToolUse) | — | **`ToolCallCompleted`** |
| `subagent-stop` | — | — (no merger kind) |
| `session-start` | — | — (base `SessionReset` already covers via rmux pattern stream) |
| `session-end` / `pre-compact` / `post-compact` | — | — (out of scope) |

### `ToolCallStarted` is deferred

The chat-progress hook table at `claude_tui.rs:126-135` does **not**
install a `PreToolUse` entry today, so no `pre-tool-use` action is
emitted. Adding it would require a one-line table extension **plus** a
matching arm in `crates/ccteam-hooks/src/chat_progress.rs:54` and
re-running the installer against existing chat projects. Pure-additive
but out of Slice 3's blast radius. Defer.

## Out-of-scope (documented for next slice)

- **Identity-based pairing for tool calls.** See §"Why not identity-based pairing?".
- **`pre-tool-use` install + `ToolCallStarted` mapping.** Pure additive; new finding.
- **`compact-done` / `session-end` mappings.** Same pattern; no demand today.
- **Mode-2 bg (`progress-append`) integration.** Slice 1+2+3 all target the chat-TUI tap; mode-2's progress.jsonl writer already runs from the orchestrator and the existing fix-loop hooks; bringing the merger to mode-2 is a separate slice.

## Flag gating

Unchanged. The whole pipeline activates only when both flags are set:

- `CCTEAM_TYPED_EVENTS=1` (tap + consumer)
- `CCTEAM_HOOK_VIA_DAEMON=1` (hook subprocess routes via UDS instead of legacy `hook.sh` → `ccteam internal hook ...`)

Default path (both unset) is byte-identical to pre-Slice-3 — the
`SeqState` changes are pure refactor of an unused code path.

## Test plan

### Unit (`crates/ccteam-mux/src/typed_event_tap.rs`)

- `seq_stale_pending_base_dropped_before_pairing` (new): two bases of a
  multi-in-flight kind minted; tokio time advanced past `grace`; a new
  enrich mints → must produce a **fresh** seq (not pop the stale ones).
- `seq_stale_pending_enrich_dropped_before_pairing` (new): symmetric.
- `seq_no_enrichment_kind_pending_does_not_grow_unbounded` (new): N
  mints of Idle, advance time past grace, mint once more → previous
  pending dropped, queue length stays small.
- All existing `SeqState` tests (`seq_base_first_then_enrich_reuse_same_seq`,
  `seq_enrich_first_then_base_reuse_same_seq`,
  `seq_two_bases_before_any_enrich_get_distinct_seqs`,
  `seq_distinct_kinds_do_not_cross_pair`) must still pass.

### Integration (`crates/ccteam-core/tests/typed_event_pipeline_test.rs`)

- `tool_use_pair_with_two_in_flight` (new): two `tool_call_completed`
  pane matches arrive, two `tool-use` chat-progress hooks arrive →
  merger emits **two `Paired`** events. With both flags on; no
  `merger_lossy_partial` rows.
- `tool_use_one_hook_lost_does_not_cascade` (new): two pane matches,
  ONE hook → one `Paired`, one `BaseLossy` → one `merger_lossy_partial`
  row, and a third (in-window) pane match's pair is correct.
- `user_prompt_pair_with_two_in_flight` (new): symmetric for
  `user-prompt`.
- `flush_pending_drains_stale_queues_on_shutdown` (new): two minted
  bases never pair; advance time past grace; call merger `flush_pending`
  → exactly two `BaseLossy`. Ensures `flush_pending` and `drop_stale`
  do not double-count.
- `multiple_sessions_do_not_cross_pair` (new): two concurrent sessions
  each with their own tap and mint sequences; events on one must not
  pair with events on the other (`SeqState` is per-tap, but defence-in-
  depth via the `(session_id, kind, seq)` merger key).

### Mock-clock note for tests

`tokio::time::pause()` advances `tokio::time::Instant` but does **not**
advance `std::time::SystemTime`. The merger's grace expiry uses
`tokio::time::sleep` and so does `SeqState::drop_stale` (via
`tokio::time::Instant`); both honour `pause()`. But `BaseEvent.timestamp`
/ `EnrichedEvent.timestamp` are `SystemTime::now()` (`typed_event_tap.rs:243`),
so tests asserting on event timestamp ORDERING under `pause()` will see
identical instants. Tests should assert on `outcome` / `sequence_id` /
`captured`, not on `timestamp`.

### Acceptance

- `cargo test --workspace --locked --no-fail-fast --exclude ccteam-web`
  ≥ **1684/0** single-run on this host (matches handoff baseline).
- `cargo clippy --workspace --all-targets --locked -- -D warnings` → 0.
- `cargo fmt --all -- --check` → clean.
- Default path (both flags unset) writes **no new rows** to `progress.jsonl`.
- With both flags on, a paired tool-use turn writes **no**
  `merger_lossy_partial` row (paired path suppresses).
- With both flags on and a synthetically dropped hook, exactly **one**
  `merger_lossy_partial` row appears per dropped enrichment.

## Risks / known limitations

1. **Within-grace mis-pair** — a single lost enrichment whose next
   counterpart arrives **before** `grace` elapses will mis-pair (one
   wrong `Paired` event + one `BaseLossy`). Documented; identity-based
   pairing would eliminate but is out of scope. The merger's grace is
   500ms by default — small enough that this is a rare failure mode
   when hooks are healthy.
2. **Clock source** — `SeqState` uses `tokio::time::Instant` so unit
   tests can drive grace expiry via `tokio::time::pause()`. Production
   behaviour is identical to `std::time::Instant`.
3. **Memory bound** — `pending_*` queues grow at most O(events/grace);
   for sustained loss (e.g. hook subprocess dead) the merger's
   `BUFFER_CAPACITY=64` parked-base ring evicts under overload as
   `BufferOverflow`, which `typed_events.rs` consumer ignores by
   design (observability-only, not load-bearing).
