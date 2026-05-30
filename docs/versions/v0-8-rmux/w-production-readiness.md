# V0.8 rmux — Production-Readiness Audit & Flip-Default Gate

> **Purpose**: clear-eyed assessment of how far `CCTEAM_MUX_BACKEND=rmux` is from
> being the production default (the **W7 flip-default gate**). The branch goal is
> "100% rmux integration to production grade"; this doc says how far we actually
> are and what is truly blocking vs nice-to-have.
>
> **Branch**: `v0-8-rmux-integration` @ HEAD `969b0e2` (42 commits ahead of main).
> **Method**: read all wave docs + `crates/ccteam-mux/` source + `git log main..HEAD`,
> grep production call-sites, run the `ccteam-mux` test suite, cross-check against
> the `references/rmux/` upstream source. Source-evidence is cited inline as
> `file:line` / commit-hash / test-name. Read-only on source; one new doc + tests run.
>
> **Bottom line up front**: rmux as a *backend* works for the spawn/send/capture/kill
> session-control surface and routes correctly through `default_backend()` under the
> env flag. But **three concrete defects block flip-default** (daemon `exit-empty`
> self-termination, the MCP `screenshot` tool hardwired to tmux, no CI real-binary
> validation), and the *architectural payoff* of the migration (W6 single-writer hook
> bus, mode-3b Codex-in-mux, mode-2 orchestrator wiring) is **designed but not
> delivered**. Flip-default is gated, not ready.

---

## §1 What works today under `CCTEAM_MUX_BACKEND=rmux`

Backend routing is real: `claude_tui.rs` and `codex_exec.rs` both call
`ccteam_mux::default_backend()` (claude_tui.rs:285,440,564,592; codex_exec.rs:222,508),
which since commit `969b0e2` honors `CCTEAM_MUX_BACKEND` (lib.rs:385-387 → `from_env`
lib.rs:360-369). Before `969b0e2` `default_backend()` hardcoded `TmuxBackend` so the
flag was a no-op on production spawns — that gap is closed.

| Capability | Status | Evidence |
|---|---|---|
| `spawn` | **DONE** | rmux_backend.rs:233-258 → `EnsureSession` w/ `CreateOnly` + env + size + wd |
| `exists` | **DONE** | rmux_backend.rs:260-266 → `has_session` |
| `kill` | **DONE** | rmux_backend.rs:461-486; idempotent via `has_session` pre-check |
| `send_text` | **DONE** | rmux_backend.rs:268-281 → `pane(0,0).send_text` |
| `send_enter` | **DONE** | rmux_backend.rs:283-296 → `send_key("Enter")` |
| `capture` (plain) | **DONE** | rmux_backend.rs:298-321 → `snapshot().visible_lines()` |
| `capture(with_ansi=true)` | **PARTIAL** | returns **plain text, not ANSI bytes** — `_with_ansi` ignored (rmux_backend.rs:298, doc 28-31). See §2. |
| `pane_dims` | **DONE** | rmux_backend.rs:323-330 → `PaneInfo.size` |
| `pane_pid` | **DONE** | rmux_backend.rs:332-340 → `PaneProcessState::Running{pid}` |
| `list_pane_pids` | **PARTIAL** | rmux_backend.rs:342-354 — returns only first pane's pid as 1-elem vec; ccteam sessions are single-pane so this is adequate today |
| `resize` | **DONE** | rmux_backend.rs:356-369 → `pane(0,0).resize` |
| `subscribe` | **DONE** | rmux_backend.rs:371-441 → `line_stream()` unfold → `OutputChunk`/`OutputDropped`/`PatternMatched`. **Caveat**: chunks are *rendered lines* + re-appended `\n`, NOT byte-faithful (rmux_backend.rs:410-420). |
| `register_pattern` | **DONE (but no consumer)** | rmux_backend.rs:443-459. `PatternMatched` events have **zero consumers** in ccteam-core/ccteam-web (only `OutputChunk`/`OutputDropped` are read, ccteam-web/src/pty.rs:83,99). Pattern plumbing is dead weight until a consumer lands. |
| daemon launch (re-exec) | **DONE** | daemon.rs:46-59 `run_internal_daemon`; CLI `main` intercepts `--__internal-daemon` argv before clap (commit `050edff`); `RMUX_SDK_DAEMON_BINARY=current_exe()` set at backend ctor (rmux_backend.rs:151-158) + main entry (commit `4249c0d`). No separate `rmux` artifact shipped. |
| backend routing honors env | **DONE** | commit `969b0e2`; `default_backend()` lib.rs:385, `from_env` lib.rs:360, `backend_kind_from_env` lib.rs:347. |
| mode-3a Claude chat | **DONE (routed)** | claude_tui.rs `start_thread`/`close_thread` use `default_backend()` for spawn/list_pane_pids/kill/send. Under rmux these hit the daemon. Validated only with FAKE binaries — see §3. |
| mode-2 bg (`claude --bg`) | **PARTIAL / opt-in & unusable in live run** | Default `--bg` path untouched (file-based F80 poller). Opt-in `CCTEAM_CLAUDE_BG_VIA_MUX=1` foreground-`-p`-in-mux path landed (commit `e4c0631`) but is **NOT end-to-end usable**: orchestrator F80 poller resolves liveness via `~/.claude/jobs/<id>/state.json` which never exists for a via_mux spawn → agent is prematurely retired on next tick (w3-mode2-bg-findings.md:164-192). |
| mode-3b Codex | **PARTIAL** | Codex *container/TUI* lifecycle (spawn/kill/quit-keys) routes through the trait (codex_exec.rs:222,508,751). But the **Codex app-server UDS bridge is still its own supervisor** — the "Codex app-server runs inside the mux PTY" migration (w4-codex-in-mux-plan.md follow-up 2) is **deferred to a future wave**. W4 shipped only the 3 app-server defect fixes + a default-decline for blocking server requests (commits `23e8e57`, `fb669b5`). |
| `ccteam attach` (W5) | **DONE (untested w/ real daemon)** | commands.rs branches on `backend_kind_from_env()` before the tmux exists-check; under rmux drives rmux-client `connect_or_absent`→`begin_attach`→`attach_terminal_with_initial_bytes` (commit `de6dad7`). Unix-only; non-unix arm bails. Never exercised against a live daemon. |
| `ccteam peek` (W5) | **DONE (untested w/ real daemon)** | `run_peek` routes through `backend_kind_from_env()` → `MuxBackend::capture` on a current-thread runtime (commit `bbfe076`). |

**Trait is the seam.** Only one production direct-tmux caller remains:
`commands.rs:211` `tmux -V` (doctor version probe — benign, not session control). All
session lifecycle ops go through `MuxBackend`. (W0 audit listed 4 direct-caller
clusters; W1/W2c migrated them.)

---

## §2 Known gaps / limitations

### G-A. `capture(with_ansi=true)` returns plain text, not ANSI bytes (rmux)
rmux's `PaneSnapshot` is a parsed cell grid — escape bytes are gone (rmux_backend.rs:28-31,298).
**Impact path 1 (web pane-snapshot)**: `crates/ccteam-web/src/routes/pane_snapshot.rs:55`
hardcodes `TmuxBackend::new()` (NOT `default_backend()`), so the web ANSI snapshot
**stays on tmux even under rmux** — the route would query a tmux session that doesn't
exist under rmux and 404. **Impact path 2 (MCP `screenshot` tool)**:
`crates/ccteam-core/src/screenshot.rs:112` calls `capture_pane_with_ansi_from_session`
**directly from `crate::tmux`** — it never touches the trait at all. Under rmux there
is no tmux session, so the screenshot MCP tool returns `None` / empty. **This is a
user-visible regression and a flip-default blocker** (G5).

### G-B. TmuxBackend vs RmuxBackend `subscribe` parity
TmuxBackend::subscribe (ported from ccteam-web pty, commits `4ff4a6e`/`65d3556`) emits
byte-faithful `OutputChunk` from the `pipe-pane` FIFO. RmuxBackend::subscribe emits
*rendered-line* chunks (lossy: strips `\r`, re-appends `\n`; rmux_backend.rs:410-420)
— adequate for SSE display + pattern matching but **NOT byte-exact replay**. Neither
backend emits `ProcessExited` from `subscribe` under tmux; only rmux can.

### G-C. mode-2-in-mux is unusable until orchestrator wiring lands
The `via_mux` `ThreadHandle` has `identity = "ccteam-bg-<slug>-<sid>"`, not a hex
job_id; orchestrator's F80 `probe_job` reads `state.json` by identity, finds nothing,
returns `Terminal{killed}` next tick → premature `agent_done`
(w3-mode2-bg-findings.md:170-184). The fix (teach orchestrator to honor
`raw_extras.via_mux == true` and route completion through the mux signal) is **out of
W3 scope, deferred**. Until then `CCTEAM_CLAUDE_BG_VIA_MUX` is for adapter-level /
RmuxBackend bring-up only. Default (`--bg`) path is unaffected.

### G-D. Daemon `exit-empty` self-termination — THE PRODUCTION-KILLER
`ccteam_mux::daemon::run_internal_daemon` builds `DaemonConfig::new(socket)`
(daemon.rs:48), which sets `config_load = ConfigLoadOptions::disabled()`
(references/rmux/.../daemon.rs:80-86) — so it **never overrides any server option**.
The rmux `exit-empty` server option defaults to **`"on"`**
(references/rmux/crates/rmux-core/src/options/table.rs:209 `DefaultValue::Scalar("on")`).
When the daemon's last session is killed it queues
`PendingShutdownReason::ExitEmpty` and **self-terminates**
(references/rmux/.../handler_session.rs:567-572). The roundtrip integration test
*already observes this* (rmux_backend_session_roundtrip.rs:139-160 treats both
`Ok(false)` and transport-closed-Err as proof of death).

**Why this strands users**: for 24/7 mode-3 chat with ≥1 session it's dormant. But:
(a) when an operator stops the last bot (`@ccteam stop everything`) the daemon dies;
(b) `RmuxBackend.rmux` is a `OnceCell<Rmux>` (rmux_backend.rs:113,166-186) that
**caches the connected handle for the process lifetime** — after the daemon dies the
cached handle is dead and the next op fails rather than transparently reconnecting
(the `connect_or_start` retry path is never re-entered for a cached `OnceCell`).
On the *next* bot start a *new* `RmuxBackend` instance would `connect_or_start` a
fresh daemon, but any long-lived orchestrator process holding the stale `OnceCell`
is wedged.

**Doc lied to the waves**: `w2-daemon-spawn-protocol.md:126` asserts "rmux daemon
doesn't self-terminate by default" — the rmux source disproves this. Subsequent waves
inherited a false premise.

**Fix (small, must land before flip)**: set `exit-empty=off` on the ccteam-hosted
daemon (via `DaemonConfig` extension / config-load override), AND make `RmuxBackend`
resilient to a dead cached handle (re-init the `OnceCell` on transport-closed errors,
or drop the `OnceCell` cache for liveness-critical ops). One-line for the option;
small for the reconnect.

### G-E. `ProcessExited` is rmux-only — but nothing depends on it (no divergence today)
`MuxEvent::ProcessExited` is documented "RmuxBackend uses this; TmuxBackend never
emits" (lib.rs:164-173). Grep confirms the **orchestrator does not subscribe to
`MuxEvent` at all** and never consumes `ProcessExited` — it uses file-based pollers
(F80 / progress.jsonl). So there is **no per-backend behavioral divergence** from this
asymmetry today. It only becomes load-bearing once mode-2 orchestrator wiring (G-C) or
W6 hook reroute starts consuming the typed stream — at which point tmux and rmux must
be reconciled (tmux's stream-end vs rmux's typed `ProcessExited{code}`).

### G-F. W6 hook-reroute is DESIGNED, not implemented — the migration's main payoff is unrealized
The architectural reason for the daemon (single-writer `progress.jsonl` via a unified
event bus; close the V0.6.4 `OutboundCursor` race) is captured in
`w6-hook-reroute-design.md` but **no code lands it**: no `hook_sidecar.rs`, no
`ccteam mux hook-emit`, no `MuxEvent::HookEvent`, orchestrator still tails files. So
even under rmux today there remain **two writers to `progress.jsonl`** (hook subprocess
+ orchestrator) and the V0.6.4 race persists. "rmux backend works" ≠ "the unified-bus
architecture the migration was for is delivered."

---

## §3 Test coverage gaps

### Tested with FAKE binaries (no real claude/codex/rmux)
- All non-ignored `ccteam-mux` tests pass: `cargo test -p ccteam-mux` → trait_object_construction (6), backend_kind_from_env, pattern_matcher_hits, inproc lifecycle, tmux roundtrip (4 pass / 1 ignored). **Result captured this session: all green.**
- `rmux_types_compile_link` (smoke_rmux_sdk.rs:36, **NOT ignored**) is a pure compile-link semver-drift canary — fails the day rmux API shape changes in `Cargo.lock`. Runs every `cargo test`. Does **not** touch the daemon.
- TmuxBackend roundtrip uses real `tmux` if present; mode-2/3 adapter tests use `CCTEAM_CLAUDE_BIN`/`CCTEAM_CODEX_BIN` fake scripts (per CLAUDE.md §六). A print-and-exit shell script is a complete `claude --bg` stand-in (w3-mode2-bg-findings.md:34-44).

### `#[ignore]`'d — need a real daemon / binary
| Test | File | Needs |
|---|---|---|
| `spawn_send_capture_kill_through_trait` | rmux_backend_session_roundtrip.rs:55 | ccteam binary (re-exec daemon) — **NOT** a system `rmux` |
| `kill_is_idempotent_on_missing_session` | rmux_backend_session_roundtrip.rs:165 | same |
| `register_pattern_w2a_stub_is_ok` | rmux_backend_session_roundtrip.rs:181 | same |
| `smoke_rmux_sdk_echo` | smoke_rmux_sdk.rs:79 | a **system `rmux` binary on PATH** (W0 spike; superseded by the re-exec path) |
| `subscribe_streams_output_and_fires_pattern` | tmux_backend_session_roundtrip.rs | real `tmux` + writable FIFO dir |

### The critical gap: **no CI exercises a real rmux daemon**
The only true end-to-end rmux validation is the `#[ignore]`'d
`rmux_backend_session_roundtrip` trio. **Nothing runs them in CI** — the rmux
real-daemon path has never been exercised automatically. For a 24/7 long-session
system this is a flip-default blocker on its own (G2). Note the *positive*: the
re-exec test needs only the **ccteam binary** built (no system `rmux`), so the bar to
wire it into CI is low — `cargo build --bin ccteam` then run the ignored test.

---

## §4 Cross-platform status

- **Linux**: primary dev target; trait + tmux backend exercised. rmux real-daemon path
  unvalidated in CI (§3).
- **macOS (tier-1 for ccteam)**: rmux upstream marks macOS **`skipped`** in
  `references/rmux/spec/feature-inventory-v1.yaml` (research doc §R3 line 463). ccteam
  has done **zero** macOS rmux validation. Untested: daemon re-exec setsid/double-fork
  on Darwin, `connect_or_start` socket path under `$HOME/.ccteam/run/`, PTY size
  behavior, attach raw-mode TTY handoff. The research doc's own gate: "在上游补齐前
  不 flip default" (do not flip default until upstream macOS is green) — **honor this**.
- **Windows**: ccteam treats Windows as WSL2-only today; rmux has first-class ConPTY
  (research §51). The W5 attach path is explicitly **Unix-only** (non-unix arm bails,
  commit `de6dad7`). Daemon re-exec on Windows (`CREATE_NO_WINDOW | DETACHED_PROCESS`)
  is per-rmux-source-correct but **unverified by ccteam** (w2 open item 3). W6 CI would
  need a Windows runner building `ccteam.exe` + running the re-exec roundtrip + ConPTY
  attach. **Out of scope for flip-default** (G9) — keep WSL2 red-line until a Windows
  wave lands.

---

## §5 Real end-to-end run — could NOT execute here

`which rmux claude codex` on this host: only **`claude`** is present (`/home/ubuntu/.local/bin/claude`);
**`rmux` and `codex` are absent**. The re-exec roundtrip test needs the **built ccteam
binary** (not a system `rmux`), but a parallel agent may be mid-edit so I did not build
the full `ccteam` bin here. Real-binary validation is therefore an **open gap**.

**Manual procedure a human should run (no `rmux` binary required):**
```sh
cd /tmp/ccteam-rmux            # or the v0-8-rmux worktree
cargo build --bin ccteam      # the daemon is re-exec'd from THIS binary
cargo test -p ccteam-mux --test rmux_backend_session_roundtrip -- --ignored --nocapture
# Expect: spawn → exists → pane_pid Some → is_alive → list_pane_pids non-empty →
# pane_dims Some → send "echo world" → capture contains "hello" → kill → gone.
# NOTE: after kill, the daemon self-terminates (exit-empty=on, G-D) — the test
# tolerates transport-closed as "gone". A passing run still leaves G-D unfixed.
```
This run is **prerequisite to flip-default** and should be wired into CI (G2).

---

## §6 The flip-default checklist (THE deliverable)

ALL must be true before W7 flips `default_backend()` / `from_env()` default from `tmux`
to `rmux`. Each is a binary gate a human can run/verify. Conservative by design — a
daemon bug strands user agents in a 24/7 system.

| Gate | Statement | Status | Blocking? |
|---|---|---|---|
| **G1 daemon lifecycle** | `exit-empty=off` set on the ccteam-hosted daemon (`DaemonConfig`) so killing the last session does NOT terminate the daemon; AND `RmuxBackend` recovers if the daemon dies anyway (re-init the `OnceCell` / reconnect on transport-closed). Test: spawn→kill-last-session→daemon still `list_sessions`-able; kill daemon externally→next op reconnects. | **NOT MET** (G-D) | **YES** |
| **G2 real-binary smoke in CI** | `cargo build --bin ccteam && cargo test -p ccteam-mux --test rmux_backend_session_roundtrip -- --ignored` runs green in CI on Linux. | **NOT MET** (§3) | **YES** |
| **G3 mode-2 usable OR opt-in-forever** | Either orchestrator honors `raw_extras.via_mux` (G-C closed) OR `CCTEAM_CLAUDE_BG_VIA_MUX` stays opt-in and the default mode-2 `--bg` path is documented as tmux-file-based even under rmux. | **PARTIAL** — opt-in path exists but unusable in live run; needs explicit "stays opt-in" carve-out OR the fix. | **YES (carve-out or fix)** |
| **G4 mode-3b parity** | Either Codex app-server-in-mux lands (w4 follow-up 2) OR it is documented that under rmux mode-3b still uses its own UDS supervisor with the TUI pane in mux, and this introduces no observability regression vs tmux. | **PARTIAL** — container in trait, app-server UDS separate. Needs explicit documented carve-out. | **YES (carve-out)** |
| **G5 capture(ansi)/screenshot parity** | MCP `screenshot` tool + web `pane-snapshot` work under rmux. Today both are hardwired to tmux (`screenshot.rs:112` direct `crate::tmux`; `pane_snapshot.rs:55` `TmuxBackend::new()`) → would silently return empty under rmux. Fix: route through `default_backend()` AND make rmux `capture(ansi)` return usable bytes (or render server-side). | **NOT MET** (G-A) | **YES** |
| **G6 hook reroute (W6) OR accept-race** | Either W6 single-writer hook bus lands OR the audit explicitly ships rmux with two `progress.jsonl` writers + the retained V0.6.4 `OutboundCursor` race, documented. | **NOT MET** (G-F, design-only) | **YES (deliver or carve-out)** |
| **G7 zero direct tmux session callers** | No production `Command::new("tmux")` for session ops outside `tmux_ops`/`tmux_backend`. | **MET** — only `tmux -V` doctor probe remains (commands.rs:211, benign). | no |
| **G8 macOS real-binary smoke** | The G2 roundtrip passes on real macOS hardware (rmux upstream `skipped` → ccteam must self-verify per research R3). | **NOT MET** (§4) | **YES** |
| **G9 Windows scope** | Windows explicitly OUT of flip-default scope; WSL2 red-line retained; documented. | document-only | no (carve-out) |
| **G10 no semver drift** | `rmux_types_compile_link` canary green on the pinned `Cargo.lock`. | **MET** (this session) | no |

**Verdict: NOT READY.** Hard blockers: **G1, G2, G5, G8**. Soft blockers needing a
fix-or-carve-out decision: **G3, G4, G6**. Clean: G7, G9, G10.

---

## §7 Recommended remaining wave order

Strictly: **production grade = G1+G2+G5+G8 hard-closed, G3/G4/G6 decided.** Order:

1. **ExitEmpty + reconnect fix (G1)** — ship first; it is the single largest
   strand-the-user risk and the fix is small (`exit-empty=off` on `DaemonConfig` +
   `OnceCell` reconnect-on-transport-closed). Also correct `w2-daemon-spawn-protocol.md:126`.
2. **CI hook for the ignored real-daemon smoke (G2)** — only requires building the
   ccteam bin; the lowest-effort, highest-value validation. Without it nothing
   confirms the real daemon works.
3. **Screenshot/snapshot under rmux (G5)** — route `screenshot.rs` + `pane_snapshot.rs`
   through `default_backend()` and decide ANSI strategy (server-side render, or accept
   plain-text screenshots under rmux). User-visible regression otherwise.
4. **macOS real-binary smoke on real hw (G8)** — honor the research R3 gate; file
   upstream rmux issues if it fails.
5. **W6 hook reroute (G6)** OR an explicit "two-writer + accept-V0.6.4-race" carve-out.
   This is the architectural payoff; if time-boxed, ship the carve-out and schedule W6
   post-flip — but be honest that the migration's headline benefit is then deferred.
6. **W7 flip-default** — only after 1-5. Flip `from_env`/`default_backend` default to
   `rmux`, sync tier-1 + user docs, version bump.

**Post-flip (NOT blocking production grade):**
- mode-2 orchestrator `via_mux` wiring (G-C / w3 deferred) — only matters when someone
  actually wants mode-2 in mux; default `--bg` path is fine on tmux-file semantics.
- W4 follow-ups: Codex app-server-in-mux (mode-3b), typed HITL routing for Codex
  server-requests, the 4 newly-unlocked notifications. mode-3a + container-lifecycle
  routing are enough to flip if G4 ships as a documented carve-out.
- `PatternMatched` consumer (the registry is dead weight until something reads it).
- Windows ConPTY wave (G9).

**Truly blocking "production grade"**: G1, G2, G5, G8 + a decision on G3/G4/G6.
**Nice-to-have / post-flip**: everything in the post-flip list. The branch has built a
correct *backend*; what remains is **lifecycle hardening + real-binary validation +
the unified-bus payoff**, not more backend surface.
