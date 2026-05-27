# V0.8 rmux Integration — As-Built Architecture

> **Audience**: whoever finishes / ships the `v0-8-rmux-integration` branch.
> **Scope**: what the ~55 commits across W0–W5 + audit + post-audit fixes
> actually *built*, how it fits together, and — crucially — the boundary
> between **wired / opt-in-gated / designed-only**. This is the consolidated
> as-built reference; the per-wave design docs (`w0`..`w6`,
> `w-production-readiness`) capture the rationale for each piece.
>
> **Method**: read the committed source under `crates/ccteam-mux/` +
> `crates/ccteam-core/src/execution/` + `crates/ccteam-web/`; cited inline as
> `file:line` / commit-hash. No code was edited to produce this doc.
>
> **Honesty contract**: every capability below is tagged one of —
> **WIRED** (production call-sites route through it),
> **OPT-IN** (behind an env flag, not the default path),
> **BUILT-NOT-WIRED** (library exists, no live consumer),
> **DESIGNED-ONLY** (a design doc exists, no code), or
> **IN-FLIGHT** (a parallel agent is landing it at this commit).

---

## 1. Layering

The migration introduces one new crate, **`ccteam-mux`**, that owns *all*
child-process / terminal-multiplexer supervision. The dependency direction is
strictly one-way: `ccteam-core → ccteam-mux` (and `ccteam-cli`,
`ccteam-web → ccteam-mux`). `ccteam-mux` depends on no other ccteam crate.

```
ccteam-cli  ─┐
ccteam-web  ─┼──▶  ccteam-mux  ─┬─▶ rmux-sdk / rmux-server  (RmuxBackend, daemon)
ccteam-core ─┘                  └─▶ tmux CLI                 (TmuxBackend, tmux_ops)
```

`crates/ccteam-mux/src/` module map (commit `98c0527` scaffold onward):

| Module | Role |
|---|---|
| `lib.rs` (392 ln) | `MuxBackend` trait + `MuxEvent` / `MuxSessionSpec` / `MuxSessionId` / `MuxSessionKind` / `BackendKind` types + `from_env` / `default_backend` / `backend_kind_from_env` / `interactive_attach_argv` free fns |
| `tmux_ops.rs` (537 ln) | The tmux CLI primitives, **moved here from `ccteam-core::tmux`** (W1, `98c0527`). `ccteam-core::tmux` is now a thin re-export shim (`tmux.rs:6,15` doc: "V0.9 retires this module"). |
| `tmux_backend/` (`mod.rs` + `subscribe.rs` + `fifo_relay.rs`) | `TmuxBackend` — async facade over `tmux_ops`. Owns the `pipe-pane` FIFO refcount relay (ported from `ccteam-web::pty`, W2b `4ff4a6e`). |
| `rmux_backend.rs` (695 ln) | `RmuxBackend` — impl over `rmux-sdk` 0.3 (W2a `9300e62`, W2b subscribe `ca1056b`). |
| `inproc_backend.rs` (217 ln) | `InProcBackend` — mode-1 stub; most ops return `Ok(())` / no-op, drives a `tokio::task`. Used by tests + the eventual mode-1 unification (W1 `23f0676`). |
| `daemon.rs` (122 ln) | `run_internal_daemon` — the single-binary re-exec daemon runtime (W2a `050edff`). |
| `patterns/` (`mod.rs` + `claude.rs` + `codex.rs`) | Layer-2 TUI-render regex registry: `PatternMatcher` engine + the static base-pattern tables (W2b `2158f18` Claude, W3b `aacc616` Codex). |
| `enriched_event.rs` (553 ln) | The `EventMerger` — priority-with-grace-window merge of P1 (typed) + P2 (regex) + P3 (process) signals (W3b `2a331d1`). **AHEAD-OF-CONSUMER** (built+tested, no production producer/consumer; deliberate, `TODO(V0.9-typed-event-consumer)` — see §5). |

`ccteam-core` consumes the trait via `ccteam_mux::default_backend()` from the
mode-2/3 adapters; `ccteam-cli` consumes `backend_kind_from_env()` for
interactive attach/peek; `ccteam-web` consumes `TmuxBackend::subscribe()`
(directly — see §5/§6 caveat) and `from_env()` (pane-snapshot route).

---

## 2. The `MuxBackend` trait

`crates/ccteam-mux/src/lib.rs:206-310`. An `#[async_trait]` with **15
methods** (some defaulted). The whole point: a single seam so that every
session-control op can route to tmux *or* rmux *or* an in-proc task by config,
not by hardcoded call-site.

| Method | Contract (lib.rs) |
|---|---|
| `spawn(spec) -> MuxSessionId` | idempotent create-or-error |
| `exists(id) -> bool` | session present right now |
| `is_alive(id, expected_pid) -> bool` | **defaulted** — composes `exists` + `pane_pid` + OS-level `tmux_ops::pid_is_alive` (lib.rs:220-236); guards tmux stale-session-with-dead-pane |
| `send_text(id, text)` | write to pty, no Enter |
| `send_enter(id)` | literal Enter keystroke |
| `send_line(id, text)` | **defaulted** — `send_text` + `send_enter` |
| `capture(id, lines, with_ansi) -> Vec<u8>` | last N lines; **bytes** (string form dropped, audit delta 5) |
| `pane_dims(id) -> Option<(u16,u16)>` | `(rows, cols)`; `None` on missing/failed → screenshot 80×24 fallback (audit delta 4) |
| `pane_pid(id) -> Option<i32>` | active pane leader pid, distinct from spawn-time pid (audit delta 3) |
| `list_pane_pids(id) -> Vec<u32>` | every pane pid; F164 reattach consumes directly (audit delta 2) |
| `resize(id, cols, rows)` | pane geometry; xterm.js parity (audit delta 1) |
| `subscribe(id) -> MuxEventStream` | typed event stream; refcount/FIFO bookkeeping internalized (audit delta 10) |
| `register_pattern(id, regex_id, regex)` | register a Layer-2 regex → fires `PatternMatched` on the stream |
| `kill(id)` | idempotent cleanup |
| `list_sessions() -> Vec<MuxSessionId>` | live sessions for this backend |
| `backend_kind() -> BackendKind` | introspect the concrete impl behind the trait object |

**`MuxEvent`** (lib.rs:146-180): `Started{pid}`, `OutputChunk(Vec<u8>)`,
`OutputDropped{behind}`, `OutputIdle{duration}`, `PatternMatched{regex_id,
captured}`, `ProcessExited{code}`, `PaneResized{cols,rows}`,
`DaemonReconnected`. The red-line note in the source (lib.rs:138-145): the
orchestrator **NEVER** consumes `OutputChunk` directly — only the in-backend
pattern translator does, emitting higher-level variants outward.

**`BackendKind`** (lib.rs:186-191): `Tmux | Rmux | InProc`. `backend_kind()`
lets a caller recover the concrete impl after the trait object erases it.

**Routing** (the load-bearing functions):
- `from_env()` — fallible; reads `CCTEAM_MUX_BACKEND`. Explicit `tmux` →
  Tmux, `inproc-test` → InProc; `rmux` / unset / empty → Rmux; an
  unknown/typo'd value → hard error (fallible callers surface the mistake).
- `default_backend()` — infallible production selector; same routing as
  `from_env`, but a typo degrades to **Rmux** (the bundled always-available
  backend) rather than erroring. **W5 fix `969b0e2`** made it route via env
  (it used to hardcode tmux, so `CCTEAM_MUX_BACKEND=rmux` was a no-op on
  production spawns); **W7 `e9e2bdf`** flipped its env-unset default to rmux.
- `backend_kind_from_env()` — sync, side-effect-free; for CLI sites
  (`ccteam attach`/`peek`) that branch on backend without instantiating.
  Same routing: only explicit `tmux` opts out; everything else → Rmux.

**Default is `rmux`** (W7 `e9e2bdf` — the library is the single source of
truth). rmux is the bundled mux so ccteam works with no external tmux; an
operator opts out only with an explicit `CCTEAM_MUX_BACKEND=tmux`.

---

## 3. Single-binary daemon

ccteam does **not** ship a separate `rmux` artifact. It re-hosts the rmux
daemon inside its own binary:

1. `RmuxBackend::new()` sets `RMUX_SDK_DAEMON_BINARY = current_exe()`
   (`rmux_backend.rs:194-200`; also set at `main()` entry, commit `4249c0d`,
   so the env is inherited before any fork). `SDK_DAEMON_BINARY_ENV` is
   re-exported from the SDK (`daemon.rs:60`).
2. On first use, `RmuxBackend::connect_fresh()` calls the SDK's
   `Rmux::builder().connect_or_start()` (`rmux_backend.rs:214-229`). The SDK
   tries the existing socket first; only if none answers does it spawn the
   daemon binary as `<binary> --__internal-daemon <socket>`.
3. `ccteam-cli::main` intercepts that argv form **before clap parses** (commit
   `050edff`) and dispatches to `daemon::run_internal_daemon(socket)`
   (`daemon.rs:75-96`), which builds a tokio multi-thread runtime and blocks on
   `ServerDaemon::bind().wait()`.

Socket path: `$HOME/.ccteam/run/mux.sock` (`rmux_backend.rs:60-63`), parent dir
created mode-0700 on first use.

**exit-empty = off (audit G1, commit `d6bb4c1`).** rmux's `exit-empty` server
option defaults to `on`, so a stock daemon self-terminates the moment its last
session is killed — fatal for 24/7 mode-3 chat across "all bots stopped"
windows. `daemon.rs:31-34` writes a `ccteam-rmux.conf` next to the socket
containing `set -g exit-empty off` and passes it via
`DaemonConfig::with_config_files` (`daemon.rs:82-85`). Best-effort: a write
failure degrades to stock defaults rather than aborting startup
(`write_ccteam_rmux_conf` returns `None` → bare `DaemonConfig::new`).

**Reconnect-capable cache (audit G1 part 2, commit `768a367`).** The cached SDK
handle is **not** an immutable `OnceCell` (the audit found that a `OnceCell`
wedges the orchestrator if the daemon dies anyway). It is a
`Mutex<Option<Arc<Rmux>>>` (`rmux_backend.rs:155-159`). Every op runs through
`Self::call()` (`rmux_backend.rs:272-288`): on a dead-transport error
(`is_dead_transport`, rmux_backend.rs:107-121 — `UnexpectedEof |
ConnectionReset | BrokenPipe | NotConnected | ConnectionRefused`) it drops the
stale handle, `reconnect()`s (`connect_or_start` re-spawns the daemon only if
no socket answers), and retries **once**. Concurrent reconnects converge via an
`Arc::ptr_eq` guard (`rmux_backend.rs:250-263`) so a daemon hiccup never causes
a reconnect storm. Narrow by design: protocol / per-session errors are *not*
treated as transport-dead (reconnecting wouldn't change their outcome).

---

## 4. Per-mode mapping

How each execution mode routes through (or alongside) the trait *today*:

| Mode | Adapter | Routing status |
|---|---|---|
| **3a — Claude chat (TUI)** | `claude_tui.rs` | **WIRED.** `start_thread` / `close_thread` / liveness call `default_backend()` (claude_tui.rs:285,440,564,592) for spawn / `list_pane_pids` / `kill` / `send`. Migrated to the trait W2c (`5433cb7`). Under `=rmux` these hit the daemon. Validated only with FAKE binaries (§7). |
| **2 — Claude bg (`claude --bg`)** | `claude_bg.rs` | **DEFAULT path UNCHANGED + OPT-IN mux path.** Default `--bg` is the file-based F80 poller (`~/.claude/jobs/<id>/state.json`), backend-independent, untouched. An opt-in foreground-`-p`-in-mux path is gated by `CCTEAM_CLAUDE_BG_VIA_MUX=1` (`claude_bg.rs:61-65,198-199`; commit `e4c0631`): `start_thread_via_mux` spawns via the trait and tags the handle `via_mux:true` (claude_bg.rs:161) so `close_thread` routes teardown through `MuxBackend::kill` (claude_bg.rs:294-299). **Not end-to-end usable**: the orchestrator's F80 poller resolves liveness by `state.json`, which never exists for a via_mux spawn → the agent is retired on the next tick. The orchestrator-side fix (honor `raw_extras.via_mux`, route completion through the mux signal) is **IN-FLIGHT** at this commit (parallel agent), not yet landed. |
| **3b — Codex chat (app-server)** | `codex_exec.rs` + `codex_app_server.rs` | **PARTIAL.** The Codex *container / TUI* lifecycle (spawn / kill / quit-keys) routes through the trait (`codex_exec.rs:222,508`; `send_codex_quit_keys(backend, id)` codex_exec.rs:751; migrated W2c `f16f8e7`). But the **Codex app-server UDS bridge is still its own supervisor** — the "app-server runs inside the mux PTY" migration (w4-codex-in-mux-plan follow-up 2) is **DESIGNED-ONLY / deferred**. W4 shipped: the `initialize` handshake fix negotiating `experimentalApi:true` (codex_app_server.rs:163-228, commit `23e8e57`) which had been silently filtering ~30% of notifications incl. `turn/plan/updated`; a default-decline for blocking server-initiated requests (`fb669b5`); and consumption of `turn/plan/updated` → `plan_pending` + `tokenUsage`/`status`/`rateLimits` notifications (W4-fu `27e49cd`/`6ba0f05`/`3988f48`). |
| **1 — in-proc** | `InProcBackend` | **STUB.** Trait surface present; most ops no-op / `NotApplicable`. Mode-1 unification is future work. |

`ccteam attach` / `ccteam peek` (W5, commits `de6dad7` / `bbfe076`): branch on
`backend_kind_from_env()`; under rmux drive the rmux-client attach
(`connect_or_absent`→`begin_attach`→`attach_terminal_with_initial_bytes`) /
`MuxBackend::capture`. **Unix-only** (non-unix arm bails). Never exercised
against a live daemon (§7).

---

## 5. subscribe + patterns + enriched_event

**`subscribe` — two backends, two transports, one event vocabulary.**

- **TmuxBackend** (`tmux_backend/subscribe.rs`, ported from `ccteam-web::pty`
  W2b `4ff4a6e`): a `pipe-pane` FIFO feeds a `broadcast::Receiver<Vec<u8>>`;
  each chunk becomes a **byte-faithful** `OutputChunk`, and bytes are buffered
  into `\n`-delimited lines, each run through the per-subscriber
  `PatternMatcher` → `PatternMatched`. A `broadcast Lagged(n)` →
  `OutputDropped{behind:n}` + partial-line-buffer clear. The FIFO refcount
  (`RelayGuard`) travels inside the unfold state so teardown is exactly on
  stream drop.

- **RmuxBackend** (`rmux_backend.rs:482-555`): drives the SDK
  `pane.line_stream()`. Each `PaneLineItem::Line` → `PatternMatched` per regex
  hit **then** an `OutputChunk` of the rendered line. **Caveat (audit G-B)**:
  this chunk is the *rendered line* with `\r` stripped and a `\n` re-appended
  (rmux_backend.rs:526-534) — **NOT byte-faithful** to the original pane bytes.
  Adequate for SSE display + pattern matching (which fires off the rendered
  line), **not** for byte-exact replay. `PaneLineItem::Lag` →
  `OutputDropped{behind}`. No FIFO machinery — the daemon owns the broadcast.

**Pattern registry** (`patterns/`): a vetted, static set of Layer-2 TUI-render
regexes. The "no business-side grep" red line holds — only the in-backend
translator runs them. **Claude: 10 patterns** (`patterns/claude.rs`,
`CLAUDE_BASE_PATTERNS.len()==10`): `tool_call_started`, `tool_call_completed`,
`permission_prompt`, `rate_limit`, `context_overflow`, `token_usage`,
`thinking`, `user_prompt_submit`, `session_reset`, `turn_done`. **Codex: 4
patterns** (`patterns/codex.rs`): `rate_limit`, `thinking`, `turn_done`,
`approval_prompt` — deliberately thin (Codex's semantic catalog is JSON-RPC,
not regex; these are lossy L2 fallbacks).

**`PatternMatched` has zero consumers today.** The registry + matching is fully
built and tested, but nothing in `ccteam-core` / `ccteam-web` reads
`MuxEvent::PatternMatched` — only `OutputChunk` / `OutputDropped` are consumed
(by the web SSE forwarder). It is **BUILT-NOT-WIRED** dead weight until a
consumer (the merger wiring, or W6) lands.

**`EnrichedEvent` merger** (`enriched_event.rs`): a pure-logic
priority-with-grace-window merger that emits at most one logical event per
occurrence, sourcing the richest of P1 (Claude hook / Codex JSON-RPC, lossless)
/ P2 (regex, lossy) / P3 (process). Pairs by `(session_id, kind,
sequence_id)`. **Explicitly marked AHEAD-OF-CONSUMER infrastructure** (a
deliberate decision, not an oversight) — the source module doc says so and
carries a searchable `TODO(V0.9-typed-event-consumer)` tag.

Status: built + acceptance-tested (W3b `f3bd694`), but with **neither a
production producer nor consumer**:
- *No producer* — base events would come from `MuxEvent::PatternMatched`,
  which needs `register_pattern` called in production; it has no production
  caller. (The raw `MuxBackend::subscribe` line stream IS consumed, by the
  web PTY SSE — but the typed pattern-match layer above it is unwired.)
- *No consumer* — nothing reads the merged stream.

The consuming feature is **V0.9 daemon-side typed-event-driven orchestration**
(rate-limit auto-resume, idle/turn-done detection from a merged typed stream
rather than `progress.jsonl` polling). A stub consumer is deliberately NOT
added — it would look load-bearing while doing nothing. This is the cleanest
designed/built boundary on the branch.

---

## 6. Outbound today

The "unified single-writer event bus" from the research docs is **partially
realized**. As built:

- **Claude outbound** is still **file-based and backend-independent**. The
  Claude Code hook subprocess writes `progress.jsonl` directly; the
  orchestrator tails the file. This path does not go through the daemon at all,
  so it behaves identically under tmux and rmux. Consequence: the two-writer
  *architecture* (hook subprocess + orchestrator) remains. **The acute
  `OutboundCursor` race was already fixed independently in V0.6.4
  (commit `504c208`)** — so this is a cleanliness/defense-in-depth concern,
  NOT an open correctness bug.

- **W6 hook-reroute** (daemon-bus single-writer: `ccteam mux hook-emit` →
  `MuxEvent::HookEvent` → daemon coalesces → single writer) is
  **DEFERRED — value reassessed downward post-V0.6.4** (`w6-hook-reroute-
  design.md`). It was framed in the research docs as the headline payoff of
  the unified-bus vision, but: (1) the race it would prevent is already
  patched (V0.6.4), so W6's remaining value is architectural elegance, not a
  fix; (2) the clean version needs an upstream rmux daemon RPC (we don't fork
  rmux), and the fallback (a ccteam-owned `hook.sock`) sidesteps the very
  daemon-bus it was meant to demonstrate; (3) it touches the hook mechanism
  ALL mode-3 Claude depends on — wrong risk/value for a no-merge branch.
  Whoever picks this up should re-justify it before building, not treat it as
  obligatory. No `hook_sidecar.rs`, no `hook-emit` subcommand, no `HookEvent`
  variant exist (audit G6 — accept-race carve-out is the recommended close).

- **Codex outbound** flows over the app-server **UDS JSON-RPC** bridge in
  `codex_app_server.rs`, which translates typed notifications into
  `progress.jsonl` rows. This is lossless (Layer-4 typed) but lives in its own
  supervisor, not the mux daemon bus (§4 mode-3b).

- **ccteam-web SSE** consumes `TmuxBackend::subscribe` **hardcoded**
  (`ccteam-web/src/pty.rs:31,45,69` constructs `Arc<TmuxBackend>` directly,
  not `from_env()` / `default_backend()`). So the live-pane SSE stream stays on
  tmux even under `CCTEAM_MUX_BACKEND=rmux`. The web *pane-snapshot* route
  (`pane_snapshot.rs:62`, commit `2a292f7`) and the MCP *screenshot* tool
  (`screenshot.rs:171`, commit `e49d787`) *do* use `from_env()` and so honor
  the backend — but the streaming PTY registry does not. Worth knowing before
  declaring web "rmux-clean".

Honest summary: rmux is a correct **backend** for session control + snapshot;
the **unified outbound bus** (the migration's headline) is **not** delivered.

---

## 7. Validation status — what's proven vs untested

**Proven (green tests this branch):**
- All non-ignored `ccteam-mux` unit tests: trait-object construction,
  `backend_kind_from_env`, `PatternMatcher` hits, InProc lifecycle, TmuxBackend
  roundtrip (real `tmux` if present), the `EnrichedEvent` merger acceptance
  suite (paired / base-only / enrichment-only / multi-session, W3b `f3bd694`),
  the daemon `conf_writer_disables_exit_empty` test, and the `RmuxBackend`
  reconnect/cache unit tests (`reconnect_reuses_cache_when_another_caller…`,
  `rmux_returns_cached_handle_on_second_call`, rmux_backend.rs:660-694).
- **Live end-to-end rmux roundtrip + reconnect (audit G2, commit `6d01a15`)**:
  `scripts/rmux-smoke.sh` builds `--bin ccteam` and runs the previously-ignored
  `rmux_backend_session_roundtrip` against a **real ccteam-hosted daemon** (no
  system `rmux` binary needed — ccteam re-execs itself). Wired into CI as the
  `rmux-smoke` job (`.github/workflows/check.yml:36`). This is the first
  automated exercise of the real daemon: spawn → exists → pane_pid → is_alive →
  list_pane_pids → pane_dims → send → capture → kill → gone, plus the
  reconnect-after-daemon-death path (`768a367`).
- `rmux_types_compile_link` semver-drift canary (not ignored) — fails the day
  the rmux API shape changes in `Cargo.lock`.

**Untested / unproven:**
- **macOS** — zero rmux validation. rmux upstream marks macOS `skipped`
  (`references/rmux/spec/feature-inventory-v1.yaml`). Daemon re-exec setsid /
  double-fork on Darwin, socket path under `$HOME/.ccteam/run/`, PTY size
  behavior, attach raw-mode TTY handoff: all unverified. (Audit G8 — a hard
  flip-default blocker; the research R3 gate says do not flip until upstream
  macOS is green.)
- **Real `claude` / `codex` under rmux** — mode-2/3 adapter tests use FAKE
  binaries (`CCTEAM_CLAUDE_BIN` / `CCTEAM_CODEX_BIN` print-and-exit scripts).
  No run has put a real agent TUI inside the rmux daemon.
- **Web SSE under rmux** — the PTY registry hardcodes `TmuxBackend` (§6), so
  the live-pane stream has never run on rmux at all.
- **Windows** — WSL2-only red-line retained; the W5 attach path bails on
  non-unix. Daemon re-exec on Windows is rmux-source-correct but unverified by
  ccteam.

> **Test baseline**: per the parent task's last full-workspace run, the branch
> is at **1655 pass / 0 fail** with clippy `-D warnings` clean. (Not
> re-verified in this doc — cargo was not run here. Confirm with
> `cargo test --workspace --locked --no-fail-fast` before any tag.)

---

## 8. Flip-default gate

> **EXECUTED on this branch (W7, commit `e9e2bdf`).** The library default
> is now rmux (`from_env` / `default_backend` / `backend_kind_from_env`);
> only an explicit `CCTEAM_MUX_BACKEND=tmux` opts out. The flip migration
> followed `w-flip-default-migration-plan.md` Steps 1-5 (pin 21 tmux-fixture
> adapter tests + the run_peek unit test to tmux → flip default → update
> default-assertion tests → full suite green) plus Step 3 positive
> adapter-layer rmux coverage. This is a **no-merge evaluation branch**:
> the flip demonstrates "rmux as the genuine default everywhere," while
> **merge-to-main stays gated** on the open hardware/decision items below
> (G8 macOS, G3/G4/G6 carve-outs) + real-claude burn-in. The gate table is
> the audit snapshot; "Remaining" now means remaining-before-MERGE, not
> remaining-before-flip.

The W7 flip was gated on the `w-production-readiness.md §6` checklist. That
audit was written at HEAD `969b0e2`; several gates have since been closed by
post-audit commits. Current state:

| Gate | Statement | Audit (`969b0e2`) | **Now** | Closed by |
|---|---|---|---|---|
| **G1** daemon lifecycle | `exit-empty=off` + reconnect-on-dead-handle | NOT MET | **MET** | `d6bb4c1` (exit-empty) + `768a367` (reconnect) |
| **G2** real-binary CI smoke | ignored real-daemon roundtrip runs green in CI (Linux) | NOT MET | **MET** | `6d01a15` (`rmux-smoke` job) |
| **G3** mode-2 usable OR opt-in-forever | orchestrator honors `via_mux` OR `--bg` documented tmux-file even under rmux | PARTIAL | **IN-FLIGHT** | orchestrator `via_mux` wiring underway (parallel agent); until it lands, the opt-in carve-out stands |
| **G4** mode-3b parity | Codex app-server-in-mux OR documented carve-out | PARTIAL | **PARTIAL (carve-out)** | container in trait; app-server UDS separate by design — needs the carve-out written |
| **G5** capture(ansi)/screenshot parity | screenshot + pane-snapshot work under rmux | NOT MET | **MET (degraded ANSI)** | `e49d787` + `2a292f7` (route via `from_env`) + `af3a5a3` (runtime fix); rmux returns plain-text not ANSI bytes — accepted as degraded-but-working, `TODO(V0.9-rmux-ansi-capture)` |
| **G6** hook reroute (W6) OR accept-race | single-writer bus OR documented two-writer + V0.6.4 race | NOT MET | **NOT MET (design-only)** | — W6 unbuilt; needs deliver-or-carve-out |
| **G7** zero direct tmux session callers | no `Command::new("tmux")` for session ops outside `tmux_ops`/`tmux_backend` | MET | **MET** | only `tmux -V` doctor probe remains |
| **G8** macOS real-binary smoke | G2 roundtrip green on real macOS hardware | NOT MET | **NOT MET** | — no macOS validation run; hard blocker |
| **G9** Windows scope | Windows OUT of flip scope; WSL2 red-line documented | doc-only | **doc-only** | carve-out |
| **G10** no semver drift | `rmux_types_compile_link` canary green | MET | **MET** | canary in suite |

**Remaining before MERGE-to-main (the flip itself is done on this branch):**
- **G8** (macOS real-binary smoke) — the one open *hard* blocker; needs real
  Darwin hardware. Honor the research R3 "do not flip *on main* until upstream
  macOS green" gate. The CI matrix is wired (linux+macOS) and runs on push.
- **G3** — land the orchestrator `via_mux` wiring (in-flight) *or* write the
  explicit "mode-2 `--bg` stays tmux-file-based under rmux" carve-out.
- **G4 / G6** — write the carve-out docs (Codex app-server stays its own
  supervisor; rmux ships with two `progress.jsonl` writers + retained V0.6.4
  race) *or* deliver W6.

**Default is `rmux` on this branch** (W7 `e9e2bdf`). Merging to main + a
version bump stay gated on G8 hard-closing, G3/G4/G6 being decided, and
real-claude mode-3 burn-in.

---

## Appendix — wave → commit index

| Wave | What landed | Key commits |
|---|---|---|
| W0 | rmux deps + crate scaffold + tmux surface audit + SDK smoke + semver canary | `9a2b5e4` `037e1e3` `0e78b0a` `0387f2b` |
| W1 | `MuxBackend` trait + types + `tmux_ops` move + `TmuxBackend` + `InProcBackend` + caller migration | `98c0527` `2f55c56` `23f0676` `128f093` `e683cd5` |
| W2a | `RmuxBackend` impl + daemon re-exec + `RMUX_SDK_DAEMON_BINARY` + roundtrip test | `050edff` `9300e62` `08fe897` `f131347` `4249c0d` |
| W2b | `subscribe` (FIFO port for tmux, line_stream for rmux) + pattern registry + web pty refactor | `4ff4a6e` `65d3556` `ca1056b` `2158f18` `f9efcb1` `1aa4cc7` |
| W2c | adapter lifecycle migration (claude_tui, codex_exec) + process_inspect | `5433cb7` `f16f8e7` `bee3f6b` |
| W3 | mode-2 bg-in-mux opt-in path + findings | `badcabf` `e4c0631` |
| W3b | Codex base patterns + `EnrichedEvent` merger + acceptance tests | `aacc616` `2a331d1` `f3bd694` |
| W4 | Codex app-server defect fixes (initialize handshake, error wire name) + blocking-request default-decline | `23e8e57` `fb669b5` |
| W4-fu | consume turn/plan/updated → plan_pending + tokenUsage/status/rateLimits + warn-silence | `27e49cd` `6ba0f05` `3988f48` |
| W5 | backend-aware `ccteam attach` + `peek` | `bbfe076` `de6dad7` |
| audit | production-readiness audit + flip-default gate | `ae168f3` |
| fixes | exit-empty off (G1) + reconnect (G1) + screenshot/snapshot via trait (G5) + G5 nested-runtime fix + CI real-daemon smoke (G2) + `default_backend` env routing | `d6bb4c1` `768a367` `e49d787` `2a292f7` `af3a5a3` `6d01a15` `969b0e2` |

**Still ahead:** W6 hook-reroute (daemon-bus single-writer, designed only),
W7 flip-default, macOS validation (G8), the in-flight Codex camelCase sweep +
mode-2 `via_mux` orchestrator wiring.
