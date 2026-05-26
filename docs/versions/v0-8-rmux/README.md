# V0.8 rmux Integration — Branch-Local Progress Log

> **Branch**:`v0-8-rmux-integration`(off origin/main `446e33a`)
> **Goal**:100% rmux integration to production-grade; mode 1/2/3 child supervision unified under `MuxBackend` trait + embedded rmux daemon via `ccteam mux daemon` re-exec.
> **No PR / no release** — continuous development on this branch.
> **Design SoT**:`docs/research/embedded-mux-unified-architecture.md`(1571 lines, on main)
> **Worktree**:`/tmp/ccteam-rmux/`(references/ symlinked to main checkout's `references/`)

---

## Wave plan(from research §六 + §13.8 + §15.6)

| Wave | Subject | Status |
|---|---|---|
| **W0** | spike — Cargo dep + tmux surface audit + rmux SDK smoke | in progress |
| W1 | MuxBackend trait + TmuxBackend wrap(behavior-equivalent) | pending |
| W2 | RmuxBackend + `ccteam mux daemon` re-exec + Claude mode 3a + 10 base patterns | pending |
| W3 | mode 2 bg(Claude + Codex)into mux + Codex 10 patterns + EnrichedEvent merger | pending |
| W4 | typed events → progress.jsonl bridge + Codex app-server in mux(mode 3b)| pending |
| W5 | `ccteam attach` via rmux-client(cross-mode + Windows ConPTY) | pending |
| W6 | macOS + Windows CI matrix + Claude Code hook → daemon UDS reroute | pending |
| W7 | flip default to rmux + doc sync + production polish | pending |

## Acceptance gates per wave

- Each wave keeps cargo test `--workspace --exclude ccteam-web` baseline ≥ **1549/1**(CLAUDE.md §一)
- Clippy `--workspace --all-targets --locked -- -D warnings` 0 errors + 0 warnings
- `cargo fmt --all -- --check` clean
- New code includes integration tests behind `--feature rmux` flag where applicable

## Open log

### W0 — completed deliverables

- `w0-tmux-surface-audit.md` (460 lines, subagent A) — 11 tmux subcommands, 4 direct callers bypass `TmuxSession`, `CCTEAM_TMUX_BIN` does NOT exist (CLAUDE.md §六 outdated), 10 trait deltas vs research §四 draft
- `w3b-codex-event-catalog.md` (484 lines, subagent C) — 50 Codex notifications + 10 blocking HITL requests inventoried
- Design notes: `w1-mux-backend-trait-draft.md`, `w2-daemon-spawn-protocol.md`, `w4-enriched-event-merger.md`, `w6-hook-reroute-design.md`

### W4 must-fix defects (discovered by W0 audit)

Filed as part of V0.8 W4 Codex rework (not separate V0.6.x patches — W4 redoes the backend):

1. **Missing `initialize` handshake** — `crates/ccteam-core/src/execution/codex_app_server.rs` doc-comment claims handshake exists but the call is absent. `experimental_api` ends up `false`, silently filtering ~30% of Codex notifications including `turn/plan/updated` (the headline plan-tree event for HITL F124).

2. **Dead `"item/updated"` match arm** at `codex_app_server.rs:525-527` — mode-3 protocol has no such method; arm never fires.

3. **Dead `"turn/failed"` match arm** at `codex_app_server.rs:504` — wire name is actually `"error"`. ccteam may be silently dropping all Codex turn failures into the `warn_unknown_vendor_token` skip path.

### W3b unhandled critical path

ccteam currently handles ZERO of the 10 server→client requests Codex sends that BLOCK turn progress (sandbox-violation HITL, etc.). W3b lands the request handler dispatch + response routing.
