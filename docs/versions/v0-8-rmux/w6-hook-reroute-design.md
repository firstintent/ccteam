# W6 Design Note — Claude Code hook re-route to daemon UDS

> Source: research doc §13.4 (outbound path comparison) + §15.5 (EnrichedEvent merger). Concrete W6 implementation plan.

## Current state (V0.6.8)

ccteam installs Claude Code hooks per-project that fire on Claude lifecycle events. Hook command (from `crates/ccteam-core/src/execution/claude_tui.rs`):

```
ccteam internal hook progress-append [args...]
```

This subprocess:
1. Reads STDIN JSON (the Claude Code hook payload — `tool_name`, `tool_input`, `result`, etc.)
2. Opens the project's `progress.jsonl` (path from env)
3. Appends a translated `chat_*` or `tool_*` event line
4. Exits

**Two writers** end up touching `progress.jsonl`: the hook subprocess AND the orchestrator. Race-prone (V0.6.4 `OutboundCursor` race fix was symptomatic).

## V0.8 W6 target

Replace the hook command with:

```
ccteam mux hook-emit \
  --session-id <mux-session-uuid> \
  --kind <hook-kind> \
  --json '<claude hook payload verbatim>'
```

This subprocess:
1. Reads its argv + STDIN (Claude hook payload)
2. Connects to the local rmux daemon UDS (via `rmux_sdk::Rmux::builder().unix_socket(<ccteam mux sock>).connect()`)
3. Sends a custom `ccteam-hook-emit` RPC frame carrying the typed hook event
4. Daemon's `HookSidecar` receiver on that session publishes the typed event onto the session's event bus
5. ccteam orchestrator (subscribed to all sessions) receives the typed event
6. Orchestrator writes `progress.jsonl` (single writer — race window closed)
7. Hook subprocess exits

## Wire protocol

The rmux daemon's stock RPC vocabulary doesn't include "ccteam-hook-emit". Two options:

**Option A — extend rmux daemon via upstream PR**
- Add `RegisterHookSidecar` + `EmitHookEvent` RPCs to rmux-proto
- rmux daemon broadcasts on session subscribe stream
- Upstream contribution, requires rmux PR review

**Option B — use rmux daemon's built-in pane input fd**
- The hook subprocess writes its JSON payload to a per-session named-pipe / fd that the daemon already provides for pane input redirection
- daemon's CCTeam adapter layer reads from this fd and translates
- Lighter touch, can ship without upstream PR

**Option C — ccteam runs its own sidecar UDS alongside rmux's daemon UDS**
- `~/.ccteam/run/hook.sock` separate from `~/.ccteam/run/mux.sock`
- Hook subprocess writes to hook.sock; ccteam orchestrator reads
- Skips rmux daemon entirely for outbound; loses the "unified bus" property

**Recommendation: Option A** — the unified bus is core to the architecture. Upstream PR is small (one RPC pair) and the surface naturally extends rmux's design. Fallback to Option C only if upstream PR review drags past W6 timeline.

## W6 implementation steps

1. **Define hook RPC** in our wrapper layer (`crates/ccteam-mux/src/hook_sidecar.rs`):
   - `HookEmit { session_id, kind, payload_json }`
   - Daemon-side: per-session `tokio::sync::broadcast::Sender<HookEvent>`
   - Subscribe end: surfaces as `MuxEvent::HookEvent { ... }` variant (extend `MuxEvent` enum)

2. **Add `ccteam mux hook-emit` CLI** in `ccteam-cli/src/commands/mux.rs`:
   - Parses argv `--session-id` + `--kind` + `--json` (stdin if --json='-')
   - Calls `ccteam_mux::HookSidecarClient::emit(...)` async
   - Exits 0 on send accepted, non-zero on UDS connect failure (Claude Code may retry)

3. **Install path adjustment** in `claude_tui.rs::ensure_chat_hooks_installed`:
   - Generate `.claude/hooks/<kind>.sh` script with new argv
   - For each hook kind that ccteam wires (`pre_tool_use`, `post_tool_use`, `user_prompt_submit`, `stop`, `session_start`, etc.), use the matching `--kind` value

4. **Orchestrator subscribe** in ccteam orchestrator's `chat_event_loop`:
   - On `MuxEvent::HookEvent { kind, payload_json }`, deserialize the Claude payload
   - Translate to existing `progress::ChatEvent` variants
   - Append to `progress.jsonl` (single-writer path — orchestrator is the only writer)
   - F122-style bridge code in `codex_app_server.rs` becomes redundant — delete

5. **Backward compat / rollback**:
   - During V0.8 W6 dev: hook script accepts a `CCTEAM_HOOK_LEGACY=1` env to fall back to `progress-append` direct write (for emergency rollback)
   - In V0.8.x patch after burn-in: remove legacy path

## Schema for `MuxEvent::HookEvent`

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MuxEvent {
    // ... existing variants ...
    HookEvent {
        kind: String,         // claude code hook name: "pre_tool_use" etc.
        payload_json: String, // raw Claude hook payload, lossless passthrough
    },
}
```

The orchestrator-side translator (`progress::translate_hook_event`) deserializes the kind-specific payload into the existing typed `ChatEvent` variants — no lossy step.

## What this buys

| Property | Before (V0.6.8) | After (V0.8 W6) |
|---|---|---|
| `progress.jsonl` writers | 2 (orchestrator + hook subprocess) | 1 (orchestrator only) |
| Hook event latency to orchestrator | ~50ms (file tail polling) | <1ms (push via subscribe) |
| Schema validation | None (bad payload → silent file write → crash later) | Validated at daemon enqueue; bad payload returns error to hook subprocess immediately |
| Multi-subscriber (web UI live events) | Tails `progress.jsonl` file | Subscribes to daemon event stream |
| crash mid-write | Possible partial line | Atomic enqueue at daemon level |
| Codex parity | Codex F122 bridge writes file (different path) | Codex via daemon's `CodexUdsBridge` publishes same `MuxEvent::HookEvent` — same orchestrator code path |

## Red-line compliance

- Claude Code hook subprocess is Anthropic-official typed channel (per CLAUDE.md §三 R2 "走 vendor 的官方 typed event 通道")
- ccteam business code (orchestrator) NEVER greps pane bytes
- Daemon-side `HookSidecar` is a typed RPC receiver — no byte-stream parsing
- Hook payload is passed through verbatim as `payload_json: String` — orchestrator deserializes into typed Rust structs once (single point of schema dependency)
- Switch from "direct file write" to "daemon RPC" doesn't change information content, only routing

## W6 acceptance

- `crates/ccteam-mux/src/hook_sidecar.rs` lands with client + server actor
- `ccteam mux hook-emit` subcommand functional
- `claude_tui.rs::ensure_chat_hooks_installed` generates new hook scripts
- Orchestrator subscribes and writes `progress.jsonl` as single writer
- All V0.6.8 `chat_*` progress events still emitted (zero behavior change at observable layer)
- Test: 100 rapid hook events from a session — `progress.jsonl` has exactly 100 lines, no torn writes, in correct order
- ccteam-web SSE: 4 panels still update live (now via daemon event subscribe, not progress.jsonl tail)
