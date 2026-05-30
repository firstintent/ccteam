# Slice 4 — pre-tool-use wiring, identity-based pairing, Codex vendor parity

> Closes the three deferred items from Slice 3:
> 1. `pre-tool-use` installer + `ToolCallStarted` mapping
> 2. Identity-based pairing for tool calls (eliminate the within-grace mis-pair)
> 3. Codex vendor reaching Slice 1+2+3 parity
>
> Same branch `v0-8-rmux-integration`. No release, no PR. Flag-gating
> unchanged (`CCTEAM_TYPED_EVENTS` + `CCTEAM_HOOK_VIA_DAEMON`).

## Item 3 — `pre-tool-use` + `ToolCallStarted`

The smallest change, but blocks identity-based pairing because we need
the PreToolUse hook to carry `tool_name` for the started side.

### Plumbing

1. `crates/ccteam-core/src/execution/claude_tui.rs:126-135` —
   chat-progress event table. Add `("PreToolUse", "pre-tool-use")`. The
   table is the single source of truth for which Claude hook events fire
   the daemon-bus reroute command.
2. `crates/ccteam-hooks/src/chat_progress.rs:54` — add a `"pre-tool-use"`
   match arm. Reuse the same `tool-use` handler body (it only reads
   `tool_name` + `tool_input`); write a **`chat_tool_call_started`**
   progress row (distinct `event` string — see "Mode-2 silence
   classifier note" below).
3. `crates/ccteam-core/src/execution/typed_events.rs::enrich_kind_for_chat_action`
   — add `("chat-progress", Some("pre-tool-use")) → EventKind::ToolCallStarted`.
4. `crates/ccteam-core/src/progress.rs` — add a `build_chat_tool_call_started`
   constructor that emits `{"event":"chat_tool_call_started", ...}`. **MUST
   NOT use `event:"PreToolUse"`** — that string is mode-2's progress-append
   event (`templates/settings.json:33-37`) and `silence_classifier.rs:188`
   counts it as a `closes_pending` Task-balance signal. Reusing the name
   would poison mode-2's silence detector with mode-3 rows.
5. Tests: extend `crates/ccteam-core/src/execution/typed_events.rs`'s
   inline mapping tests with `pre-tool-use → ToolCallStarted`; remove
   the existing `pre_tool_use_action_is_unmapped_until_installer_adds_it`
   guard (its job is done) and flip it to a positive mapping test.

### Mode-2 silence classifier note

Mode-2 (background `claude --bg`) already wires `PreToolUse` via the
`progress-append` dispatch (`templates/settings.json`) which writes
`{"event":"PreToolUse"}` rows. That path is **untouched** by this slice.
Item 3 strictly affects mode-3 chat — the `chat-progress` dispatch is a
separate command string in `.claude/settings.json`, with its own
`chat_*` event namespace.

### Migration note

Existing chat-mode projects must rerun `ccteam doctor --install-hooks`
to pick up the new PreToolUse entry in `.claude/settings.json`. Pre-v1.0
discipline (CLAUDE.md §五): no backwards-compat shim, no installer
schema migration. Just doc the rerun.

### Why first

Item (1) depends on PreToolUse fan-out: identity-based pairing for
`ToolCallStarted` needs the PreToolUse hook payload's `tool_name` to
match the pane's `^●\s+(\w+)\(` capture. Without (3), only
`ToolCallCompleted` is paired and identity is moot for the started side.

## Item 1 — Identity-based pairing for tool calls

### What we know (from explore phase)

- **`tool_name`** is reliably present in both PreToolUse and PostToolUse
  hook stdin (`crates/ccteam-hooks/src/chat_progress.rs:109`,
  `hooks_test.rs:135`).
- **`tool_use_id`** is NOT asserted by any test or consumer in this repo
  for the hook payload. Transcript JSONL carries `tool_use_id` (Anthropic
  upstream invariant), but whether it propagates to hook stdin is an
  untested assumption. **Treat as unknown until probed.**
- **Pane regexes**:
  - `tool_call_started`: `^●\s+(\w+)\(` — captures tool name ✓
  - `tool_call_completed`: `^\s*⎿` — captures NOTHING. Content after `⎿`
    is the tool's result summary (`Read 42 lines`, `Updated foo.rs`,
    `Done`); the tool name is sometimes the first word but **not
    guaranteed**.

### Strategy

Pair by `(EventKind, identity)` where identity is per-kind:

| kind | base side identity | enrich side identity | pairing |
|---|---|---|---|
| `ToolCallStarted` | `tool_name` (pane capture) | `tool_name` (PreToolUse payload) | exact identity match |
| `ToolCallCompleted` | unrecoverable | `tool_name` (PostToolUse payload) | per-`tool_name` FIFO cohort |
| `UserPromptSubmitted` | (prompt text — unstable for matching) | (full prompt) | FIFO only (single-in-flight in practice) |
| `TurnDone` / others | n/a | n/a | FIFO (Slice 3 drop-stale) |

**Cohort-FIFO for `ToolCallCompleted`**: instead of one queue per
`(session, kind)`, partition into one queue per `(session, kind,
tool_name)`. Two parallel tool calls of *different* tools never
cross-pair; a same-tool same-cohort cross-pair degenerates to FIFO (the
existing Slice 3 within-grace race, scoped to one tool name). This is
the honest improvement that the pane signal supports.

### Surfaces touched

1. `crates/ccteam-mux/src/typed_event_tap.rs` —
   - `RawEnrichment { kind, payload }` → `RawEnrichment { kind,
     identity: Option<String>, payload }`. Caller-side identity
     extraction (see surface 3); the mux crate never parses JSON.
   - `BaseEvent.payload.captured` carries the identity for
     `ToolCallStarted` (the pane regex `^●\s+(\w+)\(` already captures it).
   - `SeqState`'s queues key by **`(EventKind, Option<String>)`** —
     explicit composite key, not "EventKind, then maybe-identity". That
     is, `pending_base: HashMap<(EventKind, Option<String>),
     VecDeque<PendingSlot>>`. `None` identity is its own cohort with
     byte-identical Slice-3 algorithm; `Some("Edit")` and `Some("Read")`
     are distinct cohorts that never cross-mint a seq. The monotonic
     `next_seq` stays global per `SeqState` — globally-unique seqs make
     the merger's lookup trivial; cohort partitioning is purely on the
     SeqState bookkeeping side.
2. `crates/ccteam-mux/src/enriched_event.rs` —
   - The merger's pairing predicate extends from `kind == k &&
     sequence_id == s` to `kind == k && identity == id && sequence_id ==
     s` inside `process_base` (`:411-412`), `process_enrichment`
     (`:448-449`), and the deferred grace-expiry task.
   - `BaseEvent` / `EnrichmentEvent` / `ParkedBase` / `ParkedEnrichment`
     each gain an `identity: Option<String>`.
   - `EnrichmentEvent` parking for "late-base self-suppress" stays as-is
     for Claude. For Codex see surface 4.
   - `EventKind::TurnStarted` is **removed** entirely (variant +
     dispatch rows at `:149-150` + `event_kind_str` arm + the test-only
     enumeration at `:534`). Today nobody emits it, and keeping it
     invites a duplicate-row footgun when `turn/started` fans out to
     two distinct kinds.
3. `crates/ccteam-core/src/execution/typed_events.rs::enrich_session_from_hook`
   — add a `pub fn identity_for(kind: EventKind, payload_json: &str) ->
   Option<String>` helper that parses `{ "tool_name": String }` (with
   `serde_json::Value` for forgiveness) when `kind` is
   `ToolCall{Started,Completed}`. Returns `None` for every other kind
   AND for any payload that fails to deserialize / lacks the field.
   `enrich_session_from_hook` plumbs the result into `RawEnrichment`.
4. **Codex side** (surface 2 follow-on) — the merger is **bypassed**
   for Codex mode-3 (see item 2 §"Bypass merger, emit directly").
   `process_enrichment` is therefore not called for kinds where no base
   will ever arrive; no `pending_enrichment` accumulation. The merger
   keeps its current behaviour for Claude untouched.

### Acceptance criteria

- Two parallel tool calls of different tools (`Edit` + `Read`) under any
  pane/hook ordering pair correctly (no cross-tool mis-pair).
- Two parallel tool calls of the *same* tool degenerate to FIFO (same as
  Slice 3 for that case). Documented.
- All existing Slice 1/2/3 tests pass unchanged (identity-`None` codepath).
- New tests:
  - `cohort_fifo_partitions_by_tool_name` — Edit+Read interleaved,
    completion hooks arrive in REVERSE order → still paired correctly.
  - `identity_absent_falls_back_to_kind_fifo` — TurnDone / UserPromptSubmit
    keep the Slice 3 behaviour.
  - `cross_cohort_no_mint_collision` — `mint_base(ToolCallStarted, Some("Edit"))`
    + `mint_enrich(ToolCallStarted, Some("Read"))` mint distinct seqs and
    do not cross-pair (the §"Surfaces touched" surface-1 invariant).
  - `identity_none_from_malformed_payload_does_not_poison_some_bucket` —
    `payload_json = "{}"` produces identity `None`; a subsequent
    `Some("Edit")` lookup is unaffected. Defends against future hook
    payload shape drift.
  - `pre_only_post_dropped_both_age_out` — PreToolUse fires with
    identity `Some("Edit")`; PostToolUse hook crashes; pane `⎿` arrives
    with identity `None` (the regex doesn't capture). The two cohorts
    are disjoint → both park then age out as `BaseLossy`. Net: two
    `merger_lossy_partial` rows, no cross-pair contamination.

### Out of scope

- `tool_use_id` based pairing — needs an empirical probe of Claude Code
  hook stdin. Add a TODO; revisit when someone runs `claude` with a
  print-stdin hook on a real session and reports the JSON keys.
- Pane-side identity for `ToolCallCompleted` — `⎿` line content is too
  variable. Could try to capture `^\s*⎿\s*(\w+)\b` opportunistically but
  the falseh-negative rate makes it more confusing than helpful.

## Item 2 — Codex vendor parity (Slice 1 + 2 + 3)

### Constraint dictated by the architecture

`CodexAppServerAdapter` (mode-3 chat) is NOT mux-backed. `TypedEventTap`
requires `Arc<dyn MuxBackend>` (for `register_pattern` + `subscribe`).
Conclusion: we **cannot reuse `TypedEventTap` for Codex**. We build a
parallel producer that drives `EventMerger` directly from
`CodexJsonRpcClient::subscribe()`.

### Scope

- **Slice 1 (base path)**: skipped for Codex. There is no PTY pane to
  regex-match on in mode-3; the JSON-RPC channel IS the lossless source.
  The `patterns/codex.rs` entries (`rate_limit`, `thinking`,
  `turn_done`, `approval_prompt`) are L2 safety nets for a hypothetical
  pane-rendering Codex fallback; they remain in the registry but no
  producer wires them. (Mode-2 `codex exec --json` is deferred to a
  later slice — same JSON-RPC-style structured stream, no pane.)
- **Slice 2 (enrichment routing)**: the new producer translates
  `CodexJsonRpcClient::subscribe()` notifications → `EnrichmentEvent`s
  and feeds the merger. Each notification → at most one event kind (no
  fan-out). Since base never fires in mode-3, every emission is
  `MergeOutcome::EnrichmentOnly` — the merger's existing path for this
  outcome is exactly right.
- **Slice 3 (multi-in-flight)**: tool-call kinds in Codex are
  `item/started` / `item/completed` carrying `item_id` (Codex's stable
  invocation id, per `codex_jsonrpc.rs`). **`item_id` IS identity** —
  pair by `(EventKind, item_id)` cleanly. No FIFO fallback needed for
  Codex tool calls.

### Notification → EventKind mapping

| JSON-RPC method | EventKind | identity field |
|---|---|---|
| `item/started` | `ToolCallStarted` | `params.item_id` |
| `item/completed` | `ToolCallCompleted` | `params.item_id` |
| `turn/started` | `UserPromptSubmitted` | none (turn-id?) |
| `turn/completed` | `TurnDone` | none |
| `thread/started` | `SessionReset` | none |
| `thread/compacted` | `CompactDone` | none |
| `turn/plan/updated` | **skip for now** — Codex `update_plan` is the todo-tool, not plan-mode HITL. Don't conflate with Claude `PlanPending`. |
| `item/agentMessage/delta` | skip — too noisy for an event row; mode-2 already streams these via `codex exec --json`. |

`TurnStarted` is **not** wired (the dispatch table at
`enriched_event.rs:149-150` lists it, but no real consumer cares
differently from `UserPromptSubmitted` for our purposes; emitting both
from one notification is just noise).

### New module — bypass the merger

`crates/ccteam-core/src/execution/codex_typed_events.rs`:

```rust
/// Subscribe to a Codex thread's app-server JSON-RPC notifications and
/// translate them into `progress.jsonl` rows.
///
/// Unlike `TypedEventTap` (Claude path, mux-backed) we **do not** drive
/// `EventMerger`. The merger's reason for existing is to *pair* a lossy
/// pane base with a lossless P1 enrichment; Codex mode-3 has only the
/// P1 side (JSON-RPC), so every event is unambiguously
/// `MergeOutcome::EnrichmentOnly`. Going through the merger would
/// accumulate parked `pending_enrichment` slots that never resolve —
/// `BUFFER_CAPACITY=64` evicts FIFO-front silently
/// (`enriched_event.rs:481-487`), so a busy turn would silently drop
/// events. Direct path eliminates the leak.
///
/// We still reuse `EventKind` + `event_kind_str` + the row constructors
/// so a downstream tool can parse Claude and Codex `typed_event` rows
/// with the same struct.
pub fn maybe_start_codex_typed_event_tap(
    jsonrpc: Arc<CodexJsonRpcClient>,
    progress_path: PathBuf,
) -> tokio::task::JoinHandle<()>;
```

Called from `CodexAppServerAdapter::start_thread` just after the
notification bridge is set up. Gated on `CCTEAM_TYPED_EVENTS` (same flag
as Claude).

### Registry parity

Claude's registry (`typed_events::registry()`) is keyed by
`{slug}-{role}` from `HookEvent::session_id` because there's a
SECOND writer (the daemon hook subprocess) feeding enrichment back to
the tap. Codex has no second writer — JSON-RPC notifications arrive
directly on the producer's broadcast receiver. **No registry needed.**
Note this divergence in the module doc at `typed_events.rs:1-34` so
readers don't assume parity.

### Tests

- `codex_typed_event_emits_row_for_turn_completed` (new): use the
  in-process `tokio::io::duplex` peer pattern from `codex_jsonrpc.rs:362-413`
  to push a synthetic `Notification { method: "turn/completed", .. }`;
  assert a row with `kind=="typed_event"` / `event_kind=="turn_done"`
  lands in `progress.jsonl`. Use an explicit oneshot for peer shutdown
  (the existing 50ms sleep at `codex_jsonrpc.rs:399` is timing-fragile;
  don't propagate that into new tests).
- `codex_tool_call_started_completed_both_emit` (new): push `item/started`
  + `item/completed` carrying the same `item_id`; assert two rows
  (`tool_call_started` + `tool_call_completed`) — Codex side has no
  pairing semantics in the row layer (no merger), but we DO carry the
  `item_id` into the row's `captured` field so a downstream tool can
  correlate.
- `codex_no_leak_under_high_notification_volume` (new): push 100
  `turn/completed` notifications; assert producer never panics and
  doesn't hold unbounded state.
- `codex_lagged_broadcast_is_handled` (new): `broadcast::Receiver::recv`
  may yield `Lagged(n)` if the channel fills (`codex_jsonrpc.rs:109`).
  Force this and assert producer keeps consuming subsequent
  notifications (dropped notifications surface as missing rows; that's
  expected and documented).
- `codex_out_of_order_started_completed_does_not_pair` (new): completed
  arrives before started for the same `item_id`. Both produce rows
  independently. Documents the bypass-merger decision — no pairing is
  attempted on the Codex side.

### Out of scope

- Mode-2 (`codex exec --json` JSONL stdout) → typed events. Same producer
  pattern, different transport. Deferrable; doesn't ship in Slice 4.
- `turn/plan/updated` → `PlanPending`. Semantic mismatch with Claude;
  decide separately.
- Codex base patterns (`patterns/codex.rs`) producing actual events.
  Deferred until a mux-backed Codex transport exists ("Codex-in-mux
  mode 3b" per `w4-codex-in-mux-plan.md`).

## Combined acceptance

- Workspace test pass count ≥ pre-Slice-4 baseline (1694).
- `cargo clippy --workspace --all-targets --locked -- -D warnings` clean.
- `cargo fmt --all -- --check` clean.
- `scripts/rmux-smoke.sh` green; add a Codex notification smoke if a
  fake codex peer fits the smoke budget (else leave as a unit + integration
  test pair).
- Default path (both flags unset) byte-identical — even with item 3's
  installer table change, hooks only fire when the user-installed
  `settings.json` carries the new entry; existing projects unaffected
  until they rerun `ccteam doctor --install-hooks`.

## Risks

1. **`tool_use_id` probe** — fold it into a Slice-4.1 follow-up. Until
   then, identity is `tool_name`-only; same-tool parallel calls
   degenerate to FIFO. Documented.
2. **Mode-2 silence classifier collision** — RESOLVED by §"Mode-2 silence
   classifier note": new mode-3 row uses `event:"chat_tool_call_started"`,
   not `"PreToolUse"`.
3. **Codex notification accumulation** — RESOLVED by bypassing the
   merger entirely for mode-3. No `pending_enrichment` accumulation.
4. **Identity field plumb-through** — changing `RawEnrichment` / `BaseEvent`
   shapes touches `typed_event_tap.rs::SeqState` keying and every
   producer call site. Concentrated change but fairly mechanical.
5. **Test harness for Codex** — `tokio::io::duplex` + manual peer task
   per `codex_jsonrpc.rs:362-413`. Replace the existing 50ms sleep with
   a oneshot shutdown signal so tests don't carry timing fragility.
