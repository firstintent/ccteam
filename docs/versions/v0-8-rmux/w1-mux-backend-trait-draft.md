# W1 Design Note — MuxBackend trait draft (source-grounded)

> Refined from `docs/research/embedded-mux-unified-architecture.md` §四 after reading rmux SDK's actual pane handle API (`references/rmux/crates/rmux-sdk/src/handles/pane.rs` line 188-528) and session handle API (`handles/session.rs`).

## Major refinements vs research doc §四

The research doc §四 sketched a 10-method trait. After reading the rmux SDK, **rmux's pane handle is itself a usable abstraction** — it already has `wait_for_text`, `line_stream`, `snapshot`, `info`, `send_text`, etc. at higher fidelity than tmux CLI subcommands.

So the trait should be **thin enough** to wrap both:
- `TmuxBackend` — translates trait calls to `tmux send-keys / capture-pane / has-session / kill-session / display-message`
- `RmuxBackend` — translates trait calls to `rmux_sdk::Pane::send_text / snapshot / wait_for_text / ...`

And **rich enough** that the daemon-side regex pattern registry (W2b) maps cleanly onto `line_stream` parsing.

## Proposed trait (W1 starting point)

```rust
// crates/ccteam-mux/src/lib.rs

use anyhow::Result;
use bytes::Bytes;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::Duration;
use tokio_stream::Stream;

/// Vendor-agnostic identity for a mux-backed session.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MuxSessionId(pub String);

/// Lifecycle category — determines daemon supervision policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MuxSessionKind {
    /// mode 2 bg — exit code is the natural termination signal.
    Ephemeral,
    /// mode 3 chat — long-lived; exit is anomaly; respawn on certain failures.
    LongLived,
    /// dev-server / daemon child — explicit kill only.
    Daemon,
}

#[derive(Debug, Clone)]
pub struct MuxSessionSpec {
    pub name: String,                       // display label
    pub argv: Vec<String>,                  // command line
    pub working_dir: PathBuf,
    pub env: Vec<(String, String)>,
    pub size: (u16, u16),                   // pty cols/rows; default (200, 50)
    pub kind: MuxSessionKind,
}

#[derive(Debug, Clone)]
pub struct MuxSnapshot {
    pub cols: u16,
    pub rows: u16,
    pub bytes: Vec<u8>,                     // raw bytes (with ANSI if requested)
    pub plain_text: String,                 // stripped, for non-rendering uses
}

/// Typed event the daemon emits per session.
///
/// `OutputChunk` is the raw bytes; orchestrator NEVER consumes this directly
/// per the "no business-side grep" red line. Higher layers (the
/// `PatternMatched` translator inside `RmuxBackend`) subscribe to chunks
/// internally and emit only the higher-level variants outward.
#[derive(Debug, Clone)]
pub enum MuxEvent {
    Started { pid: i32 },
    OutputIdle { duration: Duration },
    /// A registered pattern matched. `regex_id` is from the static registry
    /// (crates/ccteam-core/src/mux/patterns/{claude,codex}.rs).
    PatternMatched { regex_id: String, captured: String },
    ProcessExited { code: i32 },
    PaneResized { cols: u16, rows: u16 },
    /// Daemon-restart story: emitted when reconnecting to a daemon that has
    /// outlived the orchestrator process.
    DaemonReconnected,
}

pub type MuxEventStream = Pin<Box<dyn Stream<Item = MuxEvent> + Send>>;

/// The single abstraction over child-process supervision used by ccteam.
///
/// Implementations:
/// - `TmuxBackend` — wraps the existing `tmux` CLI shell-out path (V0.6.x compat)
/// - `RmuxBackend` — wraps `rmux-sdk::Rmux` (V0.8+ default after flip)
/// - `InProcBackend` — mode 1; zero-IPC fake-session for orchestrator-internal tasks
#[async_trait::async_trait]
pub trait MuxBackend: Send + Sync {
    /// Idempotent create-or-error spawn. Returns the session id.
    async fn spawn(&self, spec: MuxSessionSpec) -> Result<MuxSessionId>;

    /// True iff a session with this name exists right now.
    async fn exists(&self, id: &MuxSessionId) -> Result<bool>;

    /// True iff session exists AND its child PID is alive (defends against tmux
    /// stale-session-with-dead-pane state; for rmux this is daemon-tracked).
    async fn is_alive(&self, id: &MuxSessionId, expected_pid: Option<i32>) -> Result<bool>;

    /// Write raw text to the session's stdin/pty (no trailing Enter).
    async fn send_text(&self, id: &MuxSessionId, text: &str) -> Result<()>;

    /// Send a literal Enter keystroke.
    async fn send_enter(&self, id: &MuxSessionId) -> Result<()>;

    /// Convenience: send_text + send_enter atomically.
    async fn send_line(&self, id: &MuxSessionId, text: &str) -> Result<()> {
        self.send_text(id, text).await?;
        self.send_enter(id).await
    }

    /// Capture the last N lines of pane output.
    /// `with_ansi=true` preserves escape sequences (for vt100 rendering).
    /// `with_ansi=false` returns stripped plain text.
    async fn capture(&self, id: &MuxSessionId, lines: usize, with_ansi: bool) -> Result<MuxSnapshot>;

    /// Query pane dimensions.
    async fn pane_dims(&self, id: &MuxSessionId) -> Result<(u16, u16)>;

    /// Query the active pane's leader PID.
    async fn pane_pid(&self, id: &MuxSessionId) -> Result<Option<i32>>;

    /// Subscribe to the typed event stream. Stream ends when session ends.
    async fn subscribe(&self, id: &MuxSessionId) -> Result<MuxEventStream>;

    /// Register a regex pattern for daemon-side matching. Once matched, emits
    /// `MuxEvent::PatternMatched { regex_id }` on the session's subscribe stream.
    /// Idempotent (re-registering same regex_id replaces the pattern).
    async fn register_pattern(
        &self,
        id: &MuxSessionId,
        regex_id: String,
        regex: String,
    ) -> Result<()>;

    /// Idempotent cleanup — Ok(()) if session doesn't exist.
    async fn kill(&self, id: &MuxSessionId) -> Result<()>;

    /// List all live sessions managed by this backend.
    async fn list_sessions(&self) -> Result<Vec<MuxSessionId>>;
}
```

## Mapping to rmux SDK (RmuxBackend impl sketch)

| Trait method | rmux SDK call |
|---|---|
| `spawn(spec)` | `rmux.ensure_session(EnsureSession::named(name).policy(CreateOrError).detached(true).size(...).process(ProcessSpec::argv(spec.argv)))` |
| `exists(id)` | `rmux.session(name).exists()` |
| `is_alive(id, pid)` | `session.exists() && session.pane(0,0).info().pid == pid` |
| `send_text(id, text)` | `session.pane(0,0).send_text(text)` |
| `send_enter(id)` | `session.pane(0,0).send_key("Enter")` |
| `capture(id, N, ansi)` | `session.pane(0,0).snapshot()` → format per `ansi` flag |
| `pane_dims(id)` | `session.pane(0,0).info().size` |
| `pane_pid(id)` | `session.pane(0,0).info().pid` |
| `subscribe(id)` | adapter: spawn task that consumes `pane.output_stream()` + `pane.line_stream()` + per-pattern `wait_for_text` waiters; translates to `MuxEvent` stream |
| `register_pattern(id, regex_id, regex)` | adapter: register in our `Arc<Mutex<HashMap<...>>>` per-session pattern registry; subscribe task matches against `line_stream` |
| `kill(id)` | `session.kill().await` (returns `Ok(true)` if killed, `Ok(false)` if not present — both map to our `Ok(())`) |
| `list_sessions()` | `rmux.list_sessions()` |

## Mapping to tmux CLI (TmuxBackend impl — preserve V0.6.x behavior)

| Trait method | Current tmux.rs code |
|---|---|
| `spawn(spec)` | `TmuxSession::start_with_env(working_dir, argv, env)` |
| `exists(id)` | `TmuxSession::exists()` |
| `is_alive(id, pid)` | `TmuxSession::is_alive(pid)` (already implements the double-check) |
| `send_text(id, text)` | `TmuxSession::send_keys_literal(text)` |
| `send_enter(id)` | `TmuxSession::send_keys_enter()` |
| `capture(id, N, ansi)` | `capture_pane_tail_from_session(name, N, ansi)` + assemble `MuxSnapshot` |
| `pane_dims(id)` | `query_pane_dims_from_session(name)` |
| `pane_pid(id)` | `TmuxSession::pane_pid()` |
| `subscribe(id)` | NEW — implement via `tmux pipe-pane -o 'cat > <fifo>'` then tail fifo (this is the pattern used by ccteam-web `pty.rs`); for `PatternMatched` we'd need to add regex matching layer on top |
| `register_pattern(id, ...)` | NEW — pattern registry stored alongside backend; matched against pipe-pane tail |
| `kill(id)` | `TmuxSession::kill()` |
| `list_sessions()` | `tmux list-sessions -F '#{session_name}'` (new helper) |

## Audit-resolved questions(W0 subagent A delivered `w0-tmux-surface-audit.md` 460 lines)

1. Q: tmux callers outside `tmux.rs`/`pty.rs`/`commands.rs`/`codex_exec.rs`? — **A: also `screenshot.rs`, `web/src/routes/pty_ws.rs`, `web/src/routes/pane_snapshot.rs`, `projects.rs`. All inventoried in §3 of audit.**
2. Q: `claude_tui.rs` use TmuxSession directly? — **A: yes, 5 `from_name` sites + ~15 method calls; F164 reattach + F172 V2 `--resume` paths both depend on `list_pane_pids` for liveness check.**
3. Q: Tests bypass TmuxSession? — **A: yes, ~16 test `Command::new("tmux")` sites including a `base-index 1` regression guard that pins the bare-name target convention. Audit §5 categorizes mock-suitable vs real-tmux-required.**
4. Q: `CCTEAM_TMUX_BIN` env override exists? — **A: NO, it does not exist. CLAUDE.md §六 was incorrect. `tmux` is hardcoded in all 17 production sites. The V0.8 W1 migration to trait + `CCTEAM_MUX_BACKEND` will be the FIRST env-driven dispatch — there is no current behavior to preserve.**
5. Q: rmux ProcessSpec env? — **A: yes, `ProcessSpec.environment: Option<Vec<String>>` accepts `NAME=VAL` strings. Verified at `references/rmux/crates/rmux-sdk/src/spec.rs:61`.**

## Audit-driven trait deltas (vs the draft above)

Adopted into the trait (W1 implementation will use these):

1. **+ `resize(h, cols, rows)`** — production caller exists (`pty_ws::resize_window` for browser xterm.js geometry), research draft missed it
2. **+ `list_pane_pids(h) -> Vec<u32>`** — F164 reattach + `claude_tui_resume_test.rs` consume directly; even if rmux abstracts "pane", "child process PIDs in this session" is a load-bearing signal for vendor-aware liveness checks
3. **`pane_pid(h)` retained as distinct method** — research draft proposed handle.pid field at spawn; insufficient because `is_alive` re-queries the *current* live PID which may drift on internal respawn
4. **`pane_dims(h) -> Option<(u16,u16)>`** — `Option` (research had unwrapped) — screenshot fallback to 80×24 needs `None` branch
5. **Drop `capture_pane_tail` String form** — zero production callers; consolidate to `capture(h, lines, ansi=false) → Vec<u8>` only
6. **Interactive `attach` is NOT on the trait** — terminal handover doesn't fit async. Provide a free fn `mux::interactive_attach_argv(backend, name) -> Vec<String>` returning argv for the CLI to spawn via blocking `Command::status()` on its own tty
7. **`is_alive(h, expected_pid)` is a default-method composite** on the trait, calling `exists` + `pane_pid` + external `pid_is_alive`
8. **`pid_is_alive(pid)` (`kill -0`) stays outside the trait** — OS-level not mux-level; free fn in `mux::util`
9. **Three target-string formats** observed in workspace — bare name (canonical, base-index-safe), `<session>:0.0` (web pipe-pane + pty_ws), `<session>:0` (broken — test guarded against). RmuxBackend impl uses opaque SessionId so this asymmetry disappears
10. **F56 web SSE pipe-pane refcount + FIFO** behavior — `PtySession::bring_up` does mkfifo + cleanup-then-start `pipe-pane`. **Refcount + FIFO bookkeeping moves INSIDE the trait impl**'s `subscribe()` method; trait API exposes only `MuxEventStream`, drop = unsubscribe (decrement refcount), teardown when refcount hits 0

## Direct-caller migration list (audit §3.3 + §4)

These 4 sites bypass `TmuxSession` today and need their own refactor in W1:

| Site | file:line | tmux call | After W1 |
|---|---|---|---|
| `commands::run_attach` | cli/src/commands.rs:1556 | `tmux attach -t <name>` | call `mux::interactive_attach_argv(&backend, name)` → spawn |
| `commands::run_peek` | cli/src/commands.rs:1710 | `tmux capture-pane -p -t <name>` | `backend.capture(h, 1000, false).await` |
| `pty.rs::PtySession::{bring_up,tear_down}` | web/src/pty.rs:153,163,185 | `tmux pipe-pane …` 3 sites | `backend.subscribe(h).await` |
| `pty_ws::send_keys + resize_window` | web/src/routes/pty_ws.rs:221,241 | `tmux send-keys`, `tmux resize-window` | `backend.send_text(h, …)` + `backend.resize(h, cols, rows)` |
| `codex_exec::send_codex_quit_keys` | core/src/execution/codex_exec.rs:732,741 | 2 raw `tmux send-keys` | refactor to use `backend.send_text("q") + backend.send_enter()` |
| `commands::run_doctor` | cli/src/commands.rs:205 | inline `tmux -V` | use `backend.probe() -> Result<()>` (new associated/static fn) |

## Acceptance for W1

- All non-test callers of `TmuxSession::*` / `capture_pane_*` / `query_pane_dims*` / `pid_is_alive` / `tmux_available` migrate to taking `&dyn MuxBackend` (or `Arc<dyn MuxBackend>`)
- `CCTEAM_MUX_BACKEND=tmux` (default) preserves exact V0.6.8 behavior
- `cargo test --workspace --exclude ccteam-web` ≥ 1549 pass
- clippy 0 warning, fmt clean
- New `crates/ccteam-mux/` workspace member with trait + 3 impls (`TmuxBackend`, `InProcBackend` stub for mode 1, `RmuxBackend` skeleton — full RmuxBackend lands in W2)
