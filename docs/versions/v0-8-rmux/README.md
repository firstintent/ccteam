# V0.8 rmux Integration — Branch-Local Progress Log

> **Branch**:`v0-8-rmux-integration`(off origin/main `446e33a`)
> **Goal**:100% rmux integration to production-grade; mode 1/2/3 child supervision unified under `MuxBackend` trait + embedded rmux daemon via `ccteam mux daemon` re-exec.
> **No PR / no release** — continuous development on this branch.
> **Design SoT**:`docs/research/embedded-mux-unified-architecture.md`(1571 lines, on main)
> **Worktree**:`/tmp/ccteam-rmux/`(references/ symlinked to main checkout's `references/`)

---

> **As-built reference**: `as-built-architecture.md` (this dir) is the
> consolidated "what got built and how it fits" doc — read it first if you're
> picking up the branch. The per-wave docs below remain the design rationale.

## Wave plan(from research §六 + §13.8 + §15.6)

| Wave | Subject | Status |
|---|---|---|
| **W0** | spike — Cargo dep + tmux surface audit + rmux SDK smoke | **DONE** `9a2b5e4` `037e1e3` `0e78b0a` `0387f2b` |
| W1 | MuxBackend trait + TmuxBackend wrap(behavior-equivalent) | **DONE** `98c0527` `2f55c56` `23f0676` `128f093` `e683cd5` |
| W2a | RmuxBackend + `ccteam mux daemon` re-exec + RMUX_SDK_DAEMON_BINARY | **DONE** `050edff` `9300e62` `08fe897` `f131347` `4249c0d` |
| W2b | subscribe (FIFO tmux / line_stream rmux) + pattern registry + web pty refactor | **DONE** `4ff4a6e` `65d3556` `ca1056b` `2158f18` `f9efcb1` `1aa4cc7` |
| W2c | adapter lifecycle migration (claude_tui + codex_exec) + process_inspect | **DONE** `5433cb7` `f16f8e7` `bee3f6b` |
| W3 | mode-2 bg-in-mux **opt-in** path (`CCTEAM_CLAUDE_BG_VIA_MUX`) + findings | **DONE (opt-in)** `badcabf` `e4c0631` |
| W3b | Codex base patterns (4) + EnrichedEvent merger + acceptance tests | **DONE (merger built-not-wired)** `aacc616` `2a331d1` `f3bd694` |
| W4 | Codex app-server defect fixes + blocking-request default-decline | **DONE** `23e8e57` `fb669b5` |
| W4-fu | consume turn/plan/updated → plan_pending + tokenUsage/status/rateLimits | **DONE** `27e49cd` `6ba0f05` `3988f48` |
| W5 | backend-aware `ccteam attach` + `peek` (rmux-client, Unix-only) | **DONE (untested w/ live daemon)** `bbfe076` `de6dad7` |
| audit | production-readiness audit + flip-default gate | **DONE** `ae168f3` |
| fixes | G1 exit-empty + G1 reconnect + G5 screenshot/snapshot via trait + G2 CI real-daemon smoke + default_backend env routing | **DONE** `d6bb4c1` `768a367` `e49d787` `2a292f7` `af3a5a3` `6d01a15` `969b0e2` |
| W6 | Claude Code hook → daemon UDS reroute (single-writer bus) | **DESIGNED-ONLY** (`w6-hook-reroute-design.md`, no code) |
| W7 | flip default to rmux + doc sync + production polish | **PENDING** (gated — see §flip-default) |

## Acceptance gates per wave

- Each wave keeps cargo test `--workspace --exclude ccteam-web` baseline ≥ **1549/1**(CLAUDE.md §一)
- Clippy `--workspace --all-targets --locked -- -D warnings` 0 errors + 0 warnings
- `cargo fmt --all -- --check` clean
- New code includes integration tests behind `--feature rmux` flag where applicable

**Current baseline**: **1655 pass / 0 fail** (per the last full-workspace run;
clippy `-D warnings` clean). Re-confirm with `cargo test --workspace --locked
--no-fail-fast` before any tag.

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

---

## Status as of HEAD (post-audit + fixes)

W0–W5 + audit + the G1/G2/G5 fixes are all landed (see wave table). The
production-readiness audit (`w-production-readiness.md`, written at `969b0e2`)
verdicted G1/G2/G5 as NOT MET; **all three are now MET** via post-audit commits
(`d6bb4c1` exit-empty, `768a367` reconnect, `e49d787`/`2a292f7`/`af3a5a3`
screenshot+snapshot via trait, `6d01a15` CI real-daemon smoke). Current
flip-default gate state (full table in `as-built-architecture.md §8`):

- **MET**: G1 (daemon lifecycle), G2 (CI smoke), G5 (screenshot — degraded ANSI
  accepted), G7 (zero direct tmux session callers), G10 (semver canary).
- **NOT MET / hard blocker**: **G8** (macOS real-binary smoke — zero macOS
  validation; needs Darwin hardware).
- **carve-out-or-deliver**: G3 (mode-2 `via_mux` — orchestrator wiring
  in-flight), G4 (Codex app-server-in-mux — designed only), G6 (W6 hook bus —
  designed only).

### What REMAINS

- **W6 hook-reroute** — daemon-bus single-writer `progress.jsonl`
  (`w6-hook-reroute-design.md`). **DEFERRED, value reassessed downward**: the
  `OutboundCursor` race it targets was already fixed in V0.6.4 (`504c208`), so
  W6's remaining value is architectural cleanup, not a fix; the clean version
  needs an upstream rmux RPC and it touches the hook path all mode-3 depends
  on. Recommended close = accept-race carve-out, not build. See
  `as-built-architecture.md §6/§8`.
- **W7 flip-default** — flip env-unset default `tmux → rmux`. Gated on G8 +
  G3/G4/G6 decisions.
- **macOS validation (G8)** — the one open hard flip blocker.
- **In-flight (parallel agents at this commit)**: Codex app-server camelCase
  wire-name sweep; mode-2 `via_mux` orchestrator wiring (teach the F80 poller to
  honor `raw_extras.via_mux` instead of `state.json`, closing G3/G-C).
- **Post-flip / non-blocking**: `PatternMatched` consumer (registry is
  built-not-wired dead weight), `EnrichedEvent` merger wiring into `subscribe`
  (merger built-not-wired), rmux ANSI capture (`TODO(V0.9-rmux-ansi-capture)`),
  ccteam-web SSE PTY registry routing through `from_env` (still hardcodes
  `TmuxBackend`), Windows ConPTY wave (G9).
