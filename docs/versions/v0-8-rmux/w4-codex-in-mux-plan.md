# W4 — Codex-in-mux migration plan (follow-up sketch)

> Status: design sketch for a **future wave**. W4 itself shipped only the 3
> app-server defect fixes (initialize handshake / `error` wire name /
> dead `item/updated` removal) + the blocking-server-request safe default.
> The full "Codex app-server runs inside a mux PTY" migration is large and
> was deliberately deferred to protect the defect fixes. This file records
> what that migration entails so a later wave can pick it up cleanly.

All paths absolute, rooted at the `v0-8-rmux-integration` worktree.

---

## What shipped in W4 (done)

1. **`initialize` handshake** (`codex_app_server.rs::handshake`) — dialed on
   connect inside `client()`, negotiates `experimentalApi: true`, sends the
   `initialized` notification, opts out of realtime/Windows/admin-UI noise.
   Unlocks `turn/plan/updated`, `thread/tokenUsage/updated`, `thread/goal/*`,
   `item/plan/delta` (previously server-filtered).
2. **`error` wire name** — replaced the dead `turn/failed` arm; honors
   `will_retry` (skip transient, surface terminal as `TurnFailed`).
3. **dead `item/updated` arm removed** — mode-3 has no such notification
   (mode-2-only wire shape).
4. **blocking server-initiated requests** — `codex_jsonrpc.rs::dispatch`
   now detects `{id, method, params}` server requests and replies with a
   JSON-RPC error (default-decline) so turns don't deadlock. Currently a
   SAFE DEFAULT; full HITL routing is the first follow-up below.

---

## Follow-up 1 — typed HITL routing for server-initiated requests

Today every server request (`item/commandExecution/requestApproval`,
`item/fileChange/requestApproval`, `item/permissions/requestApproval`,
`mcpServer/elicitation/request`, `item/tool/requestUserInput`, ...) gets a
default-decline JSON-RPC **error** reply. This unblocks the turn but never
gives the user a chance to approve.

Proper routing (mirrors V0.6.1 F98 `plan_decision` for Claude):

1. Add a server-request channel to `CodexJsonRpcClient` (alongside the
   notification broadcast) carrying `{id, method, params}`. The adapter
   subscribes and forwards approval-class requests to the orchestrator.
2. Orchestrator writes a `plan_pending`-style row to `progress.jsonl`,
   surfaces the prompt over IM (the existing F98 round-trip), waits for a
   `plan_decision` event.
3. Map the decision back to the typed response payload:
   - approve → `{decision: "accept"}` (command/file) / `{action: "accept"}`
     (elicitation), built per
     `references/codex/codex-rs/app-server-protocol/src/protocol/v2/item.rs`
     decision enums (camelCase: `accept`/`acceptForSession`/`decline`/`cancel`).
   - deny → `{decision: "decline"}` / `{action: "decline"}`.
   - timeout → `decline` (matches Codex's own `ReviewDecision::TimedOut → Decline`).
4. `item/permissions/requestApproval` needs a `GrantedPermissionProfile` on
   accept (`v2/permissions.rs:562`) — punt to decline-only until the
   permission-delta UX exists.

Keep the error-reply as the fallback for any request type with no HITL route.

## Follow-up 2 — Codex app-server inside the rmux PTY (mode 3b)

Per `w3b-codex-event-catalog.md` §4 + §6.4 and the embedded-mux research
doc §14:

- **PTY hosts the Codex TUI bytes** (mux owns L1: `process_exited`,
  `idle_30s` — the only safety net when the UDS goes dark; Codex won't
  notify its own death).
- **The daemon's `CodexUdsBridge` connects to the same app-server UDS** and
  is the truth for L3 (conversation) + L4 (semantic: plan tree, tool args,
  decisions, status flags). **Do not pattern-match Codex semantics off the
  TUI** (§6.3) — UDS is canonical.
- Migration touches `ccteam-mux` + daemon spawn protocol + `session_recovery`
  — all OWNED BY OTHER AGENTS this wave. A future wave must coordinate:
  - mux: spawn `codex app-server` (or `codex` TUI bound to a UDS) as a PTY
    process; wire `process_exited` → bridge teardown.
  - daemon: own one `CodexUdsBridge` per Codex bot, dial the negotiated
    handshake (reuse `CodexAppServerAdapter::handshake`), merge UDS
    notifications into the EnrichedEvent stream (see `w4-enriched-event-merger.md`).
  - `codex_app_server.rs`: add auto-reconnect on UDS `EOF` (catalog §7.3) —
    currently the adapter does NOT auto-reconnect; the orchestrator
    progress.jsonl poller is the fallback (V0.6.1 Wave-3 D9 retained risk).

## Follow-up 3 — newly-unlocked notifications to consume (post-handshake)

The handshake now delivers these; nothing consumes them yet
(`translate_notification` skips them via the forward-compat path):

| Wire method | Drives | Catalog ref |
|---|---|---|
| `turn/plan/updated` | `plan_pending` parity with Claude (F98) | §4.2 |
| `thread/tokenUsage/updated` | mid-turn budget tripwire (pre-turn-end) | §4.2, §8.6 |
| `thread/status/changed` | HITL detection (`WaitingOnApproval`) w/o polling | §4.2, §8.5 |
| `account/rateLimits/updated` | typed rate-limit → F84 budget-cap escalation | §4.2 |

These are additive `translate_notification` arms + new `progress.jsonl`
event constants — safe to land independently of the mux migration.
