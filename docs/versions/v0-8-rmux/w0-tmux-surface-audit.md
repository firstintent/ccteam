# V0.8 W0 — tmux Surface Audit

Read-only inventory of every tmux CLI invocation in the ccteam workspace, mapped to a proposed `MuxBackend` trait so the V0.8 rmux migration covers every behavior without escape hatches.

**Scope** — workspace at branch `v0-8-rmux-integration` (HEAD `dc80687`). Numbers below are inclusive of tests; production-only counts are noted.

- 17 production `Command::new("tmux")` call sites + 16 test call sites
- 11 distinct tmux subcommands actually executed (`has-session`, `new-session`, `kill-session`, `display-message`, `send-keys`, `list-panes`, `capture-pane`, `resize-window`, `pipe-pane`, `attach`, `show-environment`, plus `-V` probe)
- 4 separate call-site clusters: `core/tmux.rs` wrapper (canonical), `core/execution/codex_exec.rs::send_codex_quit_keys` (direct), `web/src/pty.rs` (pipe-pane) + `web/src/routes/pty_ws.rs::send_keys+resize` (direct), `cli/src/commands.rs::run_attach+run_peek+run_session_attach` (direct interactive)

---

## §1 Operation inventory

Every tmux subcommand that ccteam actually invokes, with the wrapping Rust function and exact argv. Direct (non-wrapper) callers are flagged so §2 mapping doesn't collapse them.

| # | tmux subcommand | wrapper fn (file:line) | exact argv after `tmux` | inputs | outputs | error handling |
|---|---|---|---|---|---|---|
| 1 | `-V` probe | `tmux::tmux_available` (core/src/tmux.rs:489) | `["-V"]` | none | bool — installed? | swallowed; `false` on any non-zero / spawn err |
| 1b | `-V` probe (doctor) | `commands::run_doctor` (cli/src/commands.rs:205) | `["-V"]` | none | version string for doctor report | swallowed; "NOT FOUND" message branch |
| 2 | `has-session` | `TmuxSession::exists` (core/src/tmux.rs:72) | `["has-session", "-t", &name]` | session name | bool | swallowed; `false` on err |
| 3 | `new-session` | `TmuxSession::start` / `start_with_env` (core/src/tmux.rs:90,106) | `["new-session", "-d"]` + `["-e", "KEY=VAL"]*` + `["-s", &name, "-c", &wd, "-x", "200", "-y", "50"]` + `argv...` | wd, argv, env pairs | unit | `Result` w/ context; pre-checks `exists()` and bails; stderr quoted in error |
| 4 | `resize-window` (post-spawn workaround) | `TmuxSession::start_with_env` (core/src/tmux.rs:156) | `["resize-window", "-t", &name, "-x", "200", "-y", "50"]` | session name | unit (best-effort) | **result ignored** — see §4 |
| 4b | `resize-window` (ws client) | `pty_ws::resize_window` (web/src/routes/pty_ws.rs:241) | `["resize-window", "-t", &name, "-x", cols, "-y", rows]` | session, cols, rows | unit | `Result`; bail on non-zero |
| 5 | `list-panes` | `TmuxSession::list_pane_pids` (core/src/tmux.rs:172) | `["list-panes", "-t", &name, "-F", "#{pane_pid}"]` | session name | `Vec<u32>` of pane PIDs | swallowed; empty vec on any err |
| 6 | `kill-session` | `TmuxSession::kill` (core/src/tmux.rs:193) | `["kill-session", "-t", &name]` | session name | unit | `Result`; idempotent — pre-checks `exists()`, double-check post-failure tolerates kill-race |
| 7 | `display-message` (PID) | `TmuxSession::pane_pid` (core/src/tmux.rs:218) | `["display-message", "-p", "-t", &name, "-F", "#{pane_pid}"]` | session name | `Option<i32>` | `Result<Option<…>>`; non-zero exit → `None`, parse err → `Err` |
| 7b | `display-message` (dims) | `query_pane_dims_from_session` (core/src/tmux.rs:444) | `["display-message", "-p", "-t", &name, "-F", "#{pane_height} #{pane_width}"]` | session name | `Option<(u16,u16)>` (rows, cols) | `Result<Option<…>>`; non-zero → `None`, parse-fail → `Ok(None)` |
| 8 | `send-keys -l --` (literal) | `TmuxSession::send_keys_literal` (core/src/tmux.rs:301) | `["send-keys", "-t", &name, "-l", "--", text]` | session, text | unit | `Result`; pre-checks `exists()`, stderr quoted in error |
| 8b | `send-keys -l --` (codex quit) | `send_codex_quit_keys` (core/src/execution/codex_exec.rs:732) | `["send-keys", "-t", &name, "-l", "--", "q"]` | session | unit | direct `Command::new`; non-zero → `io::Error` |
| 8c | `send-keys -l --` (web ws) | `pty_ws::send_keys` (web/src/routes/pty_ws.rs:221) | `["send-keys", "-t", "<name>:0.0", "-l", "--", s]` | session, bytes | unit | direct `Command::new`; rejects non-UTF-8 input; **target is `:0.0` not bare name** |
| 9 | `send-keys Enter` | `TmuxSession::send_keys_enter` (core/src/tmux.rs:318) | `["send-keys", "-t", &name, "Enter"]` | session | unit | `Result`; pre-checks `exists()` |
| 9b | `send-keys Enter` (codex quit) | `send_codex_quit_keys` (core/src/execution/codex_exec.rs:741) | `["send-keys", "-t", &name, "Enter"]` | session | unit | direct `Command::new`; non-zero → `io::Error` |
| 10 | `capture-pane -p` (plain) | `capture_pane_tail_from_session` (core/src/tmux.rs:360) | `["capture-pane", "-p", "-t", &session, "-S", "-<N>"]` | session, lines | `Option<String>` (UTF-8 lossy) | swallowed; `None` on any failure |
| 10b | `capture-pane -e -p` (ANSI) | `capture_pane_with_ansi_from_session` (core/src/tmux.rs:412) | `["capture-pane", "-e", "-p", "-t", &session, "-S", "-<N>"]` | session, lines | `Option<Vec<u8>>` raw | `Result<Option<…>>`; non-zero → `Ok(None)` |
| 10c | `capture-pane -p` (peek cmd) | `commands::run_peek` (cli/src/commands.rs:1710) | `["capture-pane", "-p", "-t", &name]` | session | `String` | direct `Command::new`; bails w/ stderr |
| 11a | `pipe-pane` stop (cleanup) | `PtySession::bring_up` (web/src/pty.rs:153) | `["pipe-pane", "-t", "<session>:0.0"]` | session | unit (best-effort) | result ignored; defensive stop |
| 11b | `pipe-pane` start | `PtySession::bring_up` (web/src/pty.rs:163) | `["pipe-pane", "-t", "<session>:0.0", "cat >> '<fifo>'"]` | session, fifo path | unit | bail on non-zero, unlinks fifo |
| 11c | `pipe-pane` stop (teardown) | `PtySession::tear_down` (web/src/pty.rs:185) | `["pipe-pane", "-t", "<session>:0.0"]` | session | unit (best-effort) | result ignored |
| 12 | `attach` (interactive, project) | `commands::run_attach` (cli/src/commands.rs:1556) | `["attach", "-t", &name]` | session | exit status (terminal handover) | bail w/ exit status |
| 12b | `attach` (interactive, session) | `commands::run_session_attach` (cli/src/commands.rs:1949) | `["attach", "-t", &name]` | session | exit status | bail w/ exit status |
| 13 | `show-environment` | tests only — `claude_tui_env_test.rs:48` | `["show-environment", "-t", &session, key]` | session, key | env value (for assertions) | test-only |
| 14 | private-server probes | `tmux_test.rs::send_keys_works_with_base_index_one` | `["-L", &socket, "-f", &cfg, …]` family | sandbox socket | exit status | test-only base-index regression guard |

**Production-only operations the trait must cover**: 1–12 (inclusive). 13 is test fixture introspection; 14 is a sandbox guard.

**Composite flows the caller orchestrates**:

| flow | site | steps |
|---|---|---|
| `TmuxSession::is_alive` | core/src/tmux.rs:251 | `exists` + (`pid_is_alive` via `kill -0`) + `pane_pid` cross-check |
| `TmuxSession::send_keys` | core/src/tmux.rs:283 | `send_keys_literal` → `send_keys_enter` |
| `ClaudeTuiAdapter::start_thread` (F164/F172 V2) | core/src/execution/claude_tui.rs:251–372 | `exists`? → `list_pane_pids` + `ps -p <pid> -o comm=` → branch (reattach / `kill` + `--resume` spawn / fresh `--name` spawn after 400ms liveness probe) |
| `CodexExecAdapter::close_thread` | core/src/execution/codex_exec.rs:496 | `send_codex_quit_keys` (literal `q`+Enter) → 500ms sleep → `exists`? → `kill` |
| `ClaudeTuiAdapter::close_thread` | core/src/execution/claude_tui.rs:545 | `send_keys_literal("/exit")` + `send_keys_enter` → 500ms sleep → `kill` |
| `PtySession::bring_up` | web/src/pty.rs:121 | mkfifo → spawn fifo-tail task → `pipe-pane` stop (cleanup) → `pipe-pane` start with `cat >> <fifo>` |

**Targets used**: three string formats appear (see §4):
- **session-name only** — every operation in `core/src/tmux.rs`, plus `commands.rs` attach/peek (chosen for `base-index` compat — see test `send_keys_works_with_base_index_one`).
- **`<session>:0.0`** — every operation in `web/src/pty.rs` and `web/src/routes/pty_ws.rs::send_keys` (assumes default `base-index 0` and first pane; CCTEAM-managed sessions are guaranteed one-window-one-pane).
- **`<session>:0`** — never used in production; explicitly tested as the **broken** form (`tmux_test.rs:300`).

---

## §2 Proposed `MuxBackend` trait method mapping

Refinement of `docs/research/embedded-mux-unified-architecture.md` §四. Per-operation mapping below; signature deltas vs the research draft are flagged with **Δ**.

| op # | proposed trait method (refined) | notes |
|---|---|---|
| 1 / 1b | `fn available() -> bool` (associated, not on trait) **or** the registry layer guards w/ `MuxBackend::probe() -> Result<()>` | research draft has nothing; needed for doctor + tests' skip gate |
| 2 | `async fn exists(&self, name: &str) -> Result<bool>` | matches research |
| 3 | `async fn spawn(&self, spec: MuxSessionSpec) -> Result<MuxSessionHandle>` | matches; `MuxSessionSpec` has `name/argv/working_dir/env/size/kind` — covers `start_with_env`. **Δ**: collapse `TmuxSession::start` + `start_with_env` to single `spawn` (no env pairs becomes `Vec::new()`). |
| 4 | (no trait method) — `spec.size` is honored by `spawn` impl itself | Tmux impl must replicate the **two-call** `new-session -x 200 -y 50` + post-spawn `resize-window -x 200 -y 50` workaround internally; the trait's `(u16,u16)` field is enough at the interface. See §4-A. |
| 4b | `async fn resize(&self, h: &MuxSessionHandle, cols: u16, rows: u16) -> Result<()>` | **Δ new**: research draft has no explicit resize method; needed by `pty_ws::resize_window` for browser xterm.js geometry. |
| 5 | `async fn list_pane_pids(&self, h: &MuxSessionHandle) -> Result<Vec<u32>>` | **Δ new**: not in research draft. F164 reattach + `claude_tui_resume_test.rs` consume this directly. Even if mux abstracts away "pane", "child process PIDs in this session" is a load-bearing signal — see §7-Q4. |
| 6 | `async fn kill(&self, h: &MuxSessionHandle) -> Result<()>` | matches; must preserve idempotence + post-failure race tolerance |
| 7 | `async fn pane_pid(&self, h: &MuxSessionHandle) -> Result<Option<i32>>` | **Δ refinement**: research draft proposes `MuxSessionHandle::pid` field at spawn; that's insufficient because `is_alive` cross-checks the **current** live PID, which may drift on backend-internal respawn. Keep an explicit query method. |
| 8 | `async fn send_keys(&self, h: &MuxSessionHandle, text: &str) -> Result<()>` | renamed from research's `send_keys` — semantics: **literal**, no trailing Enter. Matches `send_keys_literal`. |
| 9 | `async fn send_enter(&self, h: &MuxSessionHandle) -> Result<()>` | matches research draft (`send_enter`) |
| (composite) | helper `async fn submit_line(&self, h, text) = send_keys + send_enter` | NOT a trait method; provided as default `impl` block on the trait. Mirrors `TmuxSession::send_keys`. |
| 10 / 10b / 10c | `async fn capture(&self, h: &MuxSessionHandle, lines: usize, ansi: bool) -> Result<Vec<u8>>` | matches research; ANSI variant returns bytes (vt100 / screenshot path), plain string variant is a thin wrapper. **Δ**: ditch `capture_pane_tail`'s no-callers `Option<String>` form — only `capture_pane_with_ansi_from_session` has live consumers (screenshot + web `pane_snapshot`). |
| 7b | `async fn pane_dims(&self, h: &MuxSessionHandle) -> Result<Option<(u16, u16)>>` | matches research; **Δ**: `Option` (research had unwrapped `(u16,u16)`) — screenshot fallback to 80×24 needs the `None` branch. |
| 11a-c | `async fn subscribe(&self, h: &MuxSessionHandle) -> Result<MuxEventStream>` | matches research; **F56 refcount + FIFO bookkeeping moves inside the trait impl** — see §6 + §7-Q3. Drop = unsubscribe; teardown when refcount hits 0. |
| 12 / 12b | (does NOT map cleanly) — see §4-D | terminal handover to user. Either keep a separate non-trait `attach_interactive(name)` free function, or expose via a `MuxBackend::attach_command() -> Vec<String>` that returns argv the CLI can spawn. **Recommended**: free function `mux::interactive_attach_argv(backend, name) -> Vec<String>` since the actual `Command::status()` must run on the CLI's tty, not the mux backend's tokio task. |

**Operations that do NOT map cleanly** to the research draft as-is:

- **A. `resize-window` post-creation workaround** (op 4): not a separate trait method; the `Tmux` impl runs it implicitly inside `spawn` to honor `MuxSessionSpec::size`. `Rmux` impl likely sets PTY size directly via `openpty(2)` and won't need the dance. Documented as a Tmux-only quirk inside the impl.
- **B. `is_alive` documented double-check** (composite op): trait has `exists` + `pane_pid` — caller composes. Provide a default `impl` block `async fn is_alive(&self, h, expected_pid) -> bool` to centralize. Note that `pid_is_alive` (`kill -0`) is OS-level, not backend-level; it stays a free fn in `mux::util` or moves to `MuxBackend::pid_is_alive_external`.
- **C. `is_pane_running_claude`** (`ClaudeTuiAdapter::is_pane_running_claude`, line 169): currently `list_pane_pids` + `ps -p <pid> -o comm=`. The `ps` half is OS-level, the `list_pane_pids` half is backend-level. **Two options** in §7-Q4:
  - (i) Expose `MuxBackend::pane_process_command(h) -> Result<Option<String>>` so impls can answer in one call (tmux runs `ps`, rmux can use stat /proc).
  - (ii) Keep caller composing `list_pane_pids` + external `ps`. **Recommended (ii)** — keeps trait narrow; the "is the wrapped vendor process alive" check is vendor-aware (look for "claude"), and that's adapter-level logic.
- **D. Interactive `tmux attach -t`** (op 12 / 12b): hands terminal to the user, not a programmatic call. Doesn't fit `async fn -> Result<>`. Options:
  - (i) `fn interactive_attach_argv(backend, name) -> Vec<String>` returning argv that the CLI invokes via blocking `Command::status()` (the CLI's controlling tty inherits cleanly).
  - (ii) Make `MuxBackend::attach_blocking(&self, name) -> Result<ExitStatus>` synchronous on the trait, but this couples the trait to blocking IO. **Recommended (i)**.
- **E. `send_codex_quit_keys` direct caller** (codex_exec.rs:732): two raw `tmux send-keys` calls bypassing `TmuxSession`. After migration this should route through `MuxBackend::send_keys` + `send_enter` like the Claude adapter's `close_thread` does. **No new trait method**, just a refactor.
- **F. `pty_ws::send_keys` raw caller** (pty_ws.rs:221) and **`pty_ws::resize_window`** (line 241): also bypass `TmuxSession`. Migrate to trait calls after the `pty.rs::Subscription` exposes the handle. The `<session>:0.0` target string convention disappears with rmux (no `<window>.<pane>` namespace).

### Trait signature deltas from research §四 — summary

1. **+ `resize(h, cols, rows)`** — production caller exists (pty_ws), research draft missing
2. **+ `list_pane_pids(h)`** — F164 reattach + tests use it; research draft only has `MuxSessionHandle::pid`
3. **+ `pane_pid(h)`** — distinct from `handle.pid` for liveness re-query
4. **`pane_dims` returns `Option`** — screenshot fallback path requires it
5. **Drop `capture_pane_tail` string form** — zero production callers; consolidate to `capture(h, lines, ansi=false) → Vec<u8>`
6. **Interactive attach is NOT on the trait** — free fn returning argv
7. **`is_alive` is a default-method composite**, not a primitive
8. **`pid_is_alive` (`kill -0`) stays outside the trait** — OS-level, not mux-level

---

## §3 Caller map

Every call site of `TmuxSession::*`, free functions in `core::tmux`, and direct `Command::new("tmux")` in production + tests. Tests are tagged `[test]`.

### 3.1 `TmuxSession` constructors and methods

| caller | file:line | call | context (1-line) |
|---|---|---|---|
| `commands::run_attach` | cli/src/commands.rs:1553 | `TmuxSession::from_name(session_name_for_project(paths, slug))` | `ccteam attach` workflow project entry |
| `commands::run_attach` | cli/src/commands.rs:1554 | `.exists()` | guard before `tmux attach` shell-out |
| `commands::run_peek` | cli/src/commands.rs:1706 | `TmuxSession::from_name(session_name_for_project(paths, slug))` | `ccteam peek` |
| `commands::run_peek` | cli/src/commands.rs:1707 | `.exists()` | guard before raw `capture-pane` |
| `commands::run_session_attach` | cli/src/commands.rs:1945 | `TmuxSession::from_name(record.tmux_session.clone())` | flex session attach |
| `commands::run_session_attach` | cli/src/commands.rs:1946 | `.exists()` | guard |
| `projects::refuse_active_session` | core/src/projects.rs:595 | `TmuxSession::from_name(tmux_name.clone())` | `ccteam remove --force` gate |
| `projects::refuse_active_session` | core/src/projects.rs:596 | `.exists()` | refuse if alive |
| `claude_tui::is_pane_running_claude` | core/src/execution/claude_tui.rs:170 | `session.list_pane_pids()` | F164 reattach pane-liveness check |
| `ClaudeTuiAdapter::start_thread` | core/src/execution/claude_tui.rs:252 | `TmuxSession::from_name(session_name.clone())` | bot session handle |
| `…` | claude_tui.rs:259 | `.exists()` | branch a/b/c selector |
| `…` | claude_tui.rs:263, 281 | `.list_pane_pids()` | logging the old/new pane pid |
| `…` | claude_tui.rs:292, 324 | `.kill()` | dead-pane recreate path |
| `…` | claude_tui.rs:303, 333, 370 | `.start_with_env(cwd, argv, env)` | three branches: `--resume`, `--name fresh fallback`, brand-new |
| `ClaudeTuiAdapter::submit_turn` | core/src/execution/claude_tui.rs:406 | `TmuxSession::from_name(h.identity.clone())` | per-turn handle rebuild |
| `…` | claude_tui.rs:407, 437, 441 | `.exists()`, `.send_keys_literal(&text)`, `.send_keys_enter()` | turn submit (2-step) |
| `ClaudeTuiAdapter::resume_thread` | core/src/execution/claude_tui.rs:523 | `TmuxSession::from_name(persistent_id.into())` | resume → return handle |
| `…` | claude_tui.rs:524 | `.exists()` | else `NotImplemented` |
| `ClaudeTuiAdapter::close_thread` | core/src/execution/claude_tui.rs:546 | `TmuxSession::from_name(h.identity.clone())` | shutdown handle |
| `…` | claude_tui.rs:547, 552, 553, 555 | `.exists()`, `.send_keys_literal("/exit")`, `.send_keys_enter()`, `.kill()` | `/exit` + grace + SIGTERM |
| `CodexExecAdapter::start_thread` | core/src/execution/codex_exec.rs:219 | `TmuxSession::from_name(session_name.clone())` | codex bg session container |
| `…` | codex_exec.rs:220, 229, 233 | `.exists()`, `.start(&cwd, &argv_refs)`, `.pane_pid()` | container spawn + PID record |
| `CodexExecAdapter::close_thread` | core/src/execution/codex_exec.rs:499 | `TmuxSession::from_name(session_name.clone())` | shutdown handle |
| `…` | codex_exec.rs:500, 512, 513 | `.exists()` x2, `.kill()` | `q`+Enter (direct) → 500ms → kill |

### 3.2 Free functions in `core::tmux` (production callers only)

| free fn | caller | file:line | context |
|---|---|---|---|
| `session_name_for_slug` | `ClaudeBgAdapter::start_thread` | core/src/execution/claude_bg.rs:114 | builds `ccteam-<slug>-<sid>` name |
| `session_name_for_slug` | `CodexExecAdapter::start_thread` | core/src/execution/codex_exec.rs:208 | builds `ccteam-<slug>-<sid>` |
| `session_name_for_project` | `commands::run_attach` | cli/src/commands.rs:1553 | resolve project slug → session name |
| `session_name_for_project` | `commands::run_peek` | cli/src/commands.rs:1706 | ditto |
| `session_name_for_project` | `screenshot::render_screenshot` | core/src/screenshot.rs:108 | screenshot resolves name |
| `session_name_for_project` | `projects::refuse_active_session` | core/src/projects.rs:594 | remove gate |
| `session_name_for_project` | `pane_snapshot` handler | web/src/routes/pane_snapshot.rs:34 | web pane PNG snapshot |
| `capture_pane_with_ansi_from_session` | `screenshot::render_screenshot` | core/src/screenshot.rs:112 | ANSI-preserving capture for vt100 |
| `capture_pane_with_ansi_from_session` | `pane_snapshot::render` | web/src/routes/pane_snapshot.rs:137 | web snapshot |
| `query_pane_dims_from_session` | `screenshot::render_screenshot` | core/src/screenshot.rs:131 | rows/cols for vt100 grid |
| `query_pane_dims_from_session` | `pane_snapshot::render` | web/src/routes/pane_snapshot.rs:141 | web snapshot |
| `pid_is_alive` | `TmuxSession::is_alive` | core/src/tmux.rs:258 | the `kill -0` half of double-check |
| `tmux_available` | `commands::run_doctor` (indirect via raw `tmux -V`) | cli/src/commands.rs:205 | doctor inline; `tmux_available` itself is **only used by tests** in production code |

### 3.3 Direct `Command::new("tmux")` outside the wrapper (production)

| caller | file:line | subcommand | reason |
|---|---|---|---|
| `commands::run_doctor` | cli/src/commands.rs:205 | `-V` | doctor inline (predates `tmux_available`) |
| `commands::run_attach` | cli/src/commands.rs:1556 | `attach -t` | interactive terminal handover |
| `commands::run_peek` | cli/src/commands.rs:1710 | `capture-pane -p -t` | one-shot peek |
| `commands::run_session_attach` | cli/src/commands.rs:1949 | `attach -t` | interactive (flex session) |
| `pty.rs::PtySession::bring_up` | web/src/pty.rs:153, 163 | `pipe-pane` stop + start | F56 byte relay setup |
| `pty.rs::PtySession::tear_down` | web/src/pty.rs:185 | `pipe-pane` (no cmd = stop) | refcount-zero teardown |
| `pty_ws::send_keys` | web/src/routes/pty_ws.rs:231 | `send-keys -t <s>:0.0 -l --` | WS binary→keystroke relay |
| `pty_ws::resize_window` | web/src/routes/pty_ws.rs:242 | `resize-window -t -x -y` | WS resize control frame |
| `codex_exec::send_codex_quit_keys` | core/src/execution/codex_exec.rs:732, 741 | `send-keys -l -- q` + `Enter` | codex quit keybinding (bypasses `TmuxSession` for historical reasons; trivially refactor to `TmuxSession::send_keys_literal/enter` post-migration) |

### 3.4 Test-only callers

| file | role |
|---|---|
| `core/tests/tmux_test.rs` | unit tests for `tmux.rs` wrapper itself (8 `skip_if_no_tmux` tests + 2 unconditional) |
| `core/tests/claude_tui_test.rs` | `ClaudeTuiAdapter` start/submit/close with fake-claude + real tmux (5 async tests gated on tmux) |
| `core/tests/claude_tui_reattach_test.rs` | F164 reattach paths (5 tests, all tmux-gated; 1 unconditional `list_pane_pids_on_absent_session_is_empty`) |
| `core/tests/claude_tui_resume_test.rs` | F172 V2 `--resume` paths (8 async tests + 1 unit, mostly tmux-gated) |
| `core/tests/claude_tui_env_test.rs` | env injection via `start_with_env` (1 async test using `tmux show-environment`) |
| `core/tests/harness_trait_test.rs` | `codex_exec_start_thread_creates_tmux_session` (1 test, tmux-gated) |
| `web/tests/pty_ws_test.rs` | F56 end-to-end (8 tmux-gated tests + 5 non-tmux protocol/auth tests) |

See §5 for the per-test enumeration.

---

## §4 Behavior subtleties to preserve

These are non-obvious quirks that the trait + Rmux impl must replicate **exactly** to avoid regression. Failure to honor any of these has historically caused bugs (F164, F172, the base-index trap).

### §4-A. Two-call pane-size workaround for daemon-spawned sessions

`TmuxSession::start_with_env` issues `new-session ... -x 200 -y 50` **then** a follow-up `resize-window -t <name> -x 200 -y 50` (`tmux.rs:156`). Reason documented inline: tmux otherwise inherits a server-default that can collapse to 1×1 with no controlling client (the `-d` flag), silently truncating every `send-keys` write. The `resize-window` is a no-op once a real client attaches. Result is **deliberately ignored** (best-effort).

Trait honors this: `MuxSessionSpec::size = (200, 50)` default — the `TmuxBackend::spawn` impl runs the two-call dance internally; `RmuxBackend::spawn` sets PTY size via `openpty` at spawn (single call).

### §4-B. Session-name-only target (never `:0` suffix)

Every operation in `core/src/tmux.rs` targets `-t <name>` with **no `:N` window suffix**. Reason documented at `tmux.rs:154–155` and verified by the dedicated regression test `tmux_test.rs::send_keys_works_with_base_index_one`: users with `set -g base-index 1` in their `~/.tmux.conf` have no window 0; a hard-coded `:0` errors with `can't find window: 0`.

**But**: `web/src/pty.rs` and `pty_ws::send_keys` use `<session>:0.0` (first window, first pane). This is safe because CCTEAM-managed tmux sessions are guaranteed one-window-one-pane (no user-side base-index involvement — the session is spawned by ccteam, defaulting to `base-index 0`). However, this convention vanishes with rmux (no `<window>.<pane>` namespace).

### §4-C. PID + `has-session` double-check

`is_alive(expected_pid)` (`tmux.rs:251`) does:
1. `has-session -t <name>` (cheap)
2. `kill -0 <pid>` (cheap, fails if process gone OR caller can't signal)
3. `display-message ... #{pane_pid}` cross-check (defensive against PID recycling — fresh process inherits old slot)

Step 3 is the load-bearing one: without it a recycled PID looks alive but the session would be a stale tmux container.

### §4-D. `kill-session` race tolerance

`kill()` (`tmux.rs:193`):
1. Pre-check `exists()` — return `Ok(())` if absent
2. Run `kill-session`
3. If non-zero, re-check `exists()` — return `Ok(())` if absent now (someone else killed it between checks); otherwise bail

Idempotence + no-throw on benign race is contracted behavior.

### §4-E. `start` is non-clobbering

`start_with_env` pre-checks `exists()` and bails with `"tmux session already exists: <name>"`. Callers (F164) are expected to either reattach or `kill()` first. **Never** auto-overwrites.

### §4-F. F164 reattach branching (`ClaudeTuiAdapter::start_thread`)

When a chat-bot tmux session already exists, the adapter does NOT trust `exists() == true`. Instead it:
1. `list_pane_pids()` → for each PID, `ps -p <pid> -o comm=` → if any contains "claude", **reattach** (no spawn)
2. If no PID's `comm` contains "claude", `.kill()` the stale container and **recreate** via `claude --resume <name>` (F172 V2 lossless context restore)
3. Wait 400ms, re-run `is_pane_running_claude` → if dead, the `--resume` failed; `kill()` again and spawn fresh `--name`, emitting `chat_session_reset` with reason `resume_failed_fallback_to_fresh`

This is the single most complex composite in the workspace. The trait must let callers express it: `exists` + `list_pane_pids` + (external `ps`) + `kill` + `spawn` + tunable sleep + re-poll.

### §4-G. F56 single-relay-many-subscribers via refcount

`web/src/pty.rs::PtyRegistry`:
- One `pipe-pane` per tmux session, regardless of WS connections
- `subscribe()` increments refcount under registry mutex (race-safe with concurrent first-subscribers)
- `Drop` decrements; on zero, runs `pipe-pane` stop + unlinks FIFO
- Spawn FIFO read-end tail task **before** `pipe-pane` starts the write end (POSIX FIFO open ordering — read blocks until first writer)
- Defensive `pipe-pane` stop before start (clean up stale pipe from prior crash)
- `broadcast::channel(256)`; lag surfaces as `{"type":"lag","behind":N}` text frame, NOT a socket close

Trait `subscribe()` must internally implement all of the above; the trait surface is just the stream returned.

### §4-H. `pipe-pane` semantics: target `<session>:0.0`, no `-o`

`web/src/pty.rs:152` targets `<session>:0.0` (note: **NOT** session-name only — this is the only production path that uses the `:0.0` form). The ccteam-managed convention of "one window, one pane" makes this safe. `-o` (only-if-no-pipe) is intentionally **not** used; the registry's refcount + defensive stop replaces it.

### §4-I. CCTEAM-OWNED, never user's tmux

CLAUDE.md §三 red line: "永不主动 kill 长 session". This is upheld by:
- `tests/graceful_shutdown_test.rs:316`: asserts daemon shutdown logs do **not** contain "tmux kill-session"
- `core/src/tmux.rs:193`: only the chat / codex adapters' `close_thread` calls `kill()`; the daemon never does
- `web/src/pty.rs:42`: explicitly documents F56 never invokes `kill-session`

Trait migration must preserve: backend impls expose `kill`, but **only adapter close paths** call it; orchestrator/daemon paths must not.

### §4-J. `send-keys -l --` literal-mode separator

`send_keys_literal` uses `-l --` (literal flag + end-of-options sentinel). Without `--`, a payload starting with `-` would be parsed as a flag (text like `-tfoo` would be eaten as `-t foo`). Trait impl must inject `--` for both tmux and rmux backends.

### §4-K. `capture_pane_with_ansi` swallows non-zero exit

`tmux.rs:412` returns `Ok(None)` on tmux exit failure (not `Err`) — screenshot caller wants graceful degrade ("no PNG produced" reason in the response), not an aborted operation. Same applies to `query_pane_dims_from_session` — fallback to 80×24.

### §4-L. `resize-window` post-creation result is ignored

`tmux.rs:156` deliberately uses `let _ = …` — failure is non-fatal because newer tmux versions inherit a sane size when the session is later attached. The trait should similarly tolerate failure here (e.g. `tracing::debug!` instead of `Err`).

---

## §5 Tests touching tmux

`tmux_available()` guards every test that needs a real tmux server. Migration must add an equivalent `mux_available()` guard per backend, OR replace these tests with a `MockBackend` injection so CI runs them without binary deps.

| file | test fn | needs real tmux? | mock candidate? |
|---|---|---|---|
| `core/tests/tmux_test.rs` | `session_name_for_project_falls_back_to_slug_when_state_missing` | no (pure path resolution) | n/a |
| `core/tests/tmux_test.rs` | `session_name_for_project_uses_state_tmux_session` | no | n/a |
| `core/tests/tmux_test.rs` | `start_creates_a_session_that_exists` | yes | **yes** — exercises `spawn` + `exists` |
| `core/tests/tmux_test.rs` | `start_rejects_when_session_already_exists` | yes | **yes** |
| `core/tests/tmux_test.rs` | `kill_removes_the_session` | yes | **yes** |
| `core/tests/tmux_test.rs` | `kill_is_idempotent_on_missing_session` | yes | **yes** |
| `core/tests/tmux_test.rs` | `pane_pid_returns_a_live_pid_after_start` | yes | partial — `pane_pid` returns OS PID, MockBackend can synthesize |
| `core/tests/tmux_test.rs` | `is_alive_double_checks_session_and_pid` | yes | hard — composes external `kill -0`, prefer real backend smoke |
| `core/tests/tmux_test.rs` | `is_alive_after_kill_returns_false` | yes | **yes** |
| `core/tests/tmux_test.rs` | `send_keys_delivers_to_the_session` | yes | partial — real tmux validates the keystrokes land in shell |
| `core/tests/tmux_test.rs` | `pid_is_alive_rejects_non_positive` | no | n/a |
| `core/tests/tmux_test.rs` | `send_keys_works_with_base_index_one` | yes | **no** — tmux-specific regression guard, retire when tmux backend retires |
| `core/tests/claude_tui_test.rs` | `start_thread_spawns_tmux_and_returns_handle` | yes | **yes** |
| `core/tests/claude_tui_test.rs` | `submit_turn_sends_literal_text_to_tmux_pane` | yes | **yes** |
| `core/tests/claude_tui_test.rs` | `submit_turn_artifact_uses_read_protocol` | yes | **yes** |
| `core/tests/claude_tui_test.rs` | `close_thread_is_idempotent_on_missing_session` | yes (but exits early) | **yes** |
| `core/tests/claude_tui_test.rs` | `resume_thread_on_live_session_returns_handle` | yes | **yes** |
| `core/tests/claude_tui_test.rs` | `chat_session_name_format_is_stable` | no | n/a |
| `core/tests/claude_tui_test.rs` | `ensure_chat_hooks_creates_settings_with_all_events` | no | n/a |
| `core/tests/claude_tui_test.rs` | `ensure_chat_hooks_preserves_unrelated_top_level_keys` | no | n/a |
| `core/tests/claude_tui_test.rs` | `adapter_metadata_advertises_claude_tui` | no | n/a |
| `core/tests/claude_tui_reattach_test.rs` | `start_thread_reattaches_alive_session` | yes | **yes** — F164 |
| `core/tests/claude_tui_reattach_test.rs` | `start_thread_recreates_dead_session` | yes | **yes** — F164 |
| `core/tests/claude_tui_reattach_test.rs` | `start_thread_creates_new_session_when_absent` | yes | **yes** |
| `core/tests/claude_tui_reattach_test.rs` | `list_pane_pids_on_absent_session_is_empty` | no (probes missing session) | n/a |
| `core/tests/claude_tui_reattach_test.rs` | `list_pane_pids_on_live_session_returns_pid` | yes | **yes** |
| `core/tests/claude_tui_reattach_test.rs` | `start_thread_is_idempotent_on_alive_session` | yes | **yes** |
| `core/tests/claude_tui_resume_test.rs` | `chat_session_id_name_uses_canonical_format` | no | n/a |
| `core/tests/claude_tui_resume_test.rs` | `fresh_spawn_argv_contains_name_flag` | yes | **yes** — verifies argv has `--name <id>` |
| `core/tests/claude_tui_resume_test.rs` | `recreate_dead_pane_spawn_argv_contains_resume_flag` | yes | **yes** — F172 V2 |
| `core/tests/claude_tui_resume_test.rs` | `resume_failure_falls_back_to_fresh_name` | yes | **yes** — F172 V2 |
| `core/tests/claude_tui_resume_test.rs` | `alive_reattach_does_not_spawn_new_claude` | yes | **yes** |
| `core/tests/claude_tui_resume_test.rs` | `cwd_collision_two_roles_distinct_names` | yes | **yes** |
| `core/tests/claude_tui_resume_test.rs` | `fresh_spawn_creates_turns_mirror_dir_for_f118` | yes | **yes** |
| `core/tests/claude_tui_resume_test.rs` | `daemon_restart_uses_resume_route` | yes | **yes** |
| `core/tests/claude_tui_env_test.rs` | `fresh_spawn_injects_chat_role_and_slug_env` | yes — uses `tmux show-environment` | **yes** — `MockBackend` must record `MuxSessionSpec.env` for inspection |
| `core/tests/harness_trait_test.rs` | `codex_exec_start_thread_creates_tmux_session` | yes (gated `tmux_on_path()`) | **yes** |
| `web/tests/pty_ws_test.rs` | `ws_unknown_slug_returns_404_before_upgrade` | no | n/a |
| `web/tests/pty_ws_test.rs` | `ws_without_auth_rejects_with_401_when_auth_enabled` | no | n/a |
| `web/tests/pty_ws_test.rs` | `ws_with_bearer_header_accepts_upgrade` | yes | **yes** — `subscribe` mock |
| `web/tests/pty_ws_test.rs` | `ws_no_auth_loopback_accepts_upgrade` | yes | **yes** |
| `web/tests/pty_ws_test.rs` | `ws_receives_pane_bytes_from_pipe_pane` | yes | **yes** — `subscribe` stream mock |
| `web/tests/pty_ws_test.rs` | `ws_two_clients_share_one_pipe_pane` | yes | **yes** — refcount mock invariant |
| `web/tests/pty_ws_test.rs` | `ws_resize_control_frame_invokes_tmux_resize` | yes | **yes** — `resize` mock |
| `web/tests/pty_ws_test.rs` | `ws_last_client_disconnect_stops_pipe_pane` | yes — checks `#{pane_pipe}` via `list-panes` | hard (probes server state); keep as backend smoke |
| `web/tests/pty_ws_test.rs` | `ws_flex_sid_scoped_route_relays_bytes` | yes | **yes** |

**Mock backend requirements** (derived): `MockBackend` must support `spawn` (recording `MuxSessionSpec`), `exists`, `kill`, `list_pane_pids` (injectable), `pane_pid`, `send_keys`/`send_enter` (recording), `capture`, `pane_dims`, `subscribe` (programmable byte stream), `resize` (recording).

**Tmux-specific tests to retire** with the tmux backend: `send_keys_works_with_base_index_one`, `ws_last_client_disconnect_stops_pipe_pane` (the `#{pane_pipe}` probe is tmux-format-specific).

**Test helpers that use raw `Command::new("tmux")`** (need backend abstraction or backend-conditional compile):
- `web/tests/pty_ws_test.rs::pane_pipe_status` (line 159) — `list-panes -F #{pane_pipe}`
- `web/tests/pty_ws_test.rs::window_dims` (line 175) — `display-message -p '#{window_width}x#{window_height}'`
- `core/tests/claude_tui_env_test.rs::show_env` (line 47) — `show-environment -t <s> <KEY>`
- `core/tests/tmux_test.rs::send_keys_works_with_base_index_one` — private tmux server `-L <socket>` + `-f <cfg>`
- Various `kill_session_quiet` helpers across `claude_tui_*_test.rs` — `tmux kill-session -t <name>`

---

## §6 Web SSE pipe-pane

`ccteam-web/src/pty.rs` + `ccteam-web/src/routes/pty_ws.rs` implement F56 — a refcounted `tmux pipe-pane` registry that fans bytes out to multiple browser WebSocket subscribers. Exact mechanics:

### 6.1 FIFO lifecycle (per `key`)

`key = "<slug>"` for workflow projects, `key = "<slug>/<sid>"` for flex sessions. The FIFO path is `<paths.root>/pty/<key.replace('/', '-')>.fifo` with mode `0600` (`nix::sys::stat::Mode::S_IRUSR | S_IWUSR`).

1. **First subscriber** (`PtyRegistry::subscribe` under registry-wide tokio mutex):
   - `tokio::fs::remove_file(fifo)` — best-effort cleanup of stale FIFO from a crashed prior run (otherwise `mkfifo` would `EEXIST`)
   - `mkfifo(fifo, 0600)` via `nix::unistd::mkfifo`
   - Create `broadcast::channel::<Vec<u8>>(256)` (`BROADCAST_CAPACITY`)
   - **Spawn the FIFO read-end tail task BEFORE running `pipe-pane`** (POSIX FIFO open semantics: read-only `open(2)` blocks until first writer opens write end; the two opens must unblock together)
   - Run `tmux pipe-pane -t <session>:0.0` (no command = stop) defensively — clean state regardless of whether a prior pipe was attached
   - Run `tmux pipe-pane -t <session>:0.0 'cat >> <quoted-fifo>'` — the `cat` is the write end; tmux dups pane stdout into `cat`'s stdin
   - Increment refcount; return `Subscription` w/ a `broadcast::Receiver`
2. **Subsequent subscribers**: hit the existing entry in the registry's `HashMap`, increment refcount, subscribe to the same `broadcast::Sender`.

### 6.2 The tail task (`spawn_fifo_tail`)

`tokio::spawn` reads the FIFO read-end with an 8192-byte buffer. Each `read` chunk → `tx.send(buf[..n].to_vec())` to fan out to all `broadcast::Receiver`s. Loop exits on `Ok(0)` (EOF: all writers gone, i.e. `cat` died because `pipe-pane` stopped) or error.

### 6.3 Multi-subscriber handling

- `broadcast::channel(256)` capacity → slow subscribers see `RecvError::Lagged(n)`
- The WS relay (`pty_ws.rs::relay`) catches `Lagged` and emits a **single** text frame `{"type":"lag","behind":n}`, then **continues** reading. The socket is NOT closed. The browser xterm.js layer is expected to render this as a "lost N bytes" hint or ignore it.
- `RecvError::Closed` (sender dropped during teardown) → sends WS `Message::Close(None)` and exits cleanly.

### 6.4 Teardown (`Subscription::drop`)

`Drop` impl:
1. Guard with `tokio::runtime::Handle::try_current()` — tests using `#[tokio::test]` may shut down the runtime before all `Drop`s run; without a live handle we accept a single in-flight teardown leak rather than panic.
2. `handle.spawn(async move { … })` — drop work is async (mutex + tmux invocation)
3. Inside: registry mutex → session refcount mutex → decrement → if zero: remove from registry map, drop refcount guard, call `PtySession::tear_down`
4. `tear_down` runs `tmux pipe-pane -t <session>:0.0` (no command = stop) + `tokio::fs::remove_file(fifo)`. **No `kill-session`** (CLAUDE.md §三 red line; explicit comment at pty.rs:13).

### 6.5 Client→server WS reception

- Binary frame → `pty_ws::send_keys(&session, &data)` → `tmux send-keys -t <session>:0.0 -l -- <utf8-string>` (non-UTF-8 rejected explicitly; xterm.js sends UTF-8 in practice)
- Text frame `{"type":"resize","cols":C,"rows":R}` → `tmux resize-window -t <session> -x C -y R` (note: bare session-name target, NOT `:0.0` — different from send-keys)

### 6.6 Trait `subscribe()` requirements

The trait method `async fn subscribe(&self, h: &MuxSessionHandle) -> Result<MuxEventStream>` must internalize **all** of the above:
- Refcount-share a single backend-side relay
- Replace the FIFO + `cat >> <fifo>` mechanism with backend-native streaming (rmux likely exposes a stdout broadcast natively over its UDS; no FIFO needed)
- Preserve "many subscribers, single backend channel, drop = decrement, last-drop = teardown" semantics
- Preserve lag-isn't-fatal: events delivered with `OutputChunk { bytes }` + occasional `OutputDropped { behind }` (or equivalent — `MuxEvent::OutputIdle` from the research draft does NOT cover lag; **suggest adding `MuxEvent::OutputDropped { behind: u64 }` to the research draft**)

### 6.7 Tradeoffs flagged for migration

- **FIFO is filesystem-visible** — the `<paths.root>/pty/<key>.fifo` path is documented (e.g. `paths::pty_dir()`); some debug tooling may have grown to inspect it. After migration the FIFO disappears; check for downstream consumers (none found in current grep, but worth a code-search pre-W3).
- **Race ordering** ("read-end open BEFORE pipe-pane writes") is POSIX-specific. Rmux's native stream subscribe has no such ordering concern, simplifying the impl.
- **CCTEAM-owned, not user's tmux** — the pipe is on a CCTEAM-managed session; we never attach pipes to arbitrary user tmux sessions. Same invariant holds with rmux.

---

## §7 Open questions

**Q1. `CCTEAM_TMUX_BIN` does not exist.** CLAUDE.md §六 mentions `CCTEAM_{CLAUDE,CODEX}_BIN` env overrides; there is **no** `CCTEAM_TMUX_BIN` — the literal `"tmux"` is hardcoded in 17+ production call sites. The research draft proposes `CCTEAM_MUX_BACKEND={tmux,rmux,inproc-test}` to swap backends; the binary path itself per-backend is a separate question. Should `TmuxBackend` accept a path arg in its constructor for test override (e.g. `TmuxBackend::with_bin("/usr/local/bin/tmux")`)? Currently the test suite assumes `tmux` is on PATH — `tmux_available()` is the only escape.

**Q2. `start_with_env` → `MuxSessionSpec::env` introspection.** `claude_tui_env_test.rs` uses `tmux show-environment -t <s> <KEY>` to verify env injection from `start_with_env`. The trait `MuxBackend` has no `show-environment` equivalent; the test would need to either (a) record `MuxSessionSpec.env` via `MockBackend` (loses the real-backend round-trip), or (b) the trait gains `pane_env(h, key) -> Result<Option<String>>`. **Recommend (a)**, with one real-backend smoke retained for tmux-only regression.

**Q3. `subscribe()` and the `pipe-pane` refcount lift.** The research draft says `async fn subscribe(h) -> EventStream`, leaving refcount semantics under the impl. But the `pty.rs::PtyRegistry` is a non-trivial 320 LOC. Where does the refcount live?
  - Option (a): Each backend impl maintains its own internal `Arc<Mutex<HashMap<…>>>` registry; subscribers get cheap clones.
  - Option (b): The trait stays "one stream per call, no sharing"; the `ccteam-web::pty` registry survives as a layer above the trait.
  - **Recommend (a)** — folding the registry into the backend lets `RmuxBackend` use rmux-sdk's native multi-subscriber primitive (if it has one) without an extra abstraction layer. But this requires the trait contract to **promise** sharing-by-key, which is a non-obvious obligation.

**Q4. `is_pane_running_claude`: backend or adapter responsibility?** The function combines `list_pane_pids` (backend) + `ps -p <pid> -o comm=` (OS). After migration to `MuxBackend`:
  - Option (i) Add `MuxBackend::pane_process_command(h) -> Result<Option<String>>` — clean adapter layer, tmux impl runs `ps`, rmux impl reads `/proc/<pid>/comm`.
  - Option (ii) Adapter composes `list_pane_pids` + external `ps` itself.
  - **Recommend (ii)** — keeps the trait narrow; F164's "is vendor process X" check is adapter-specific (claude looks for "claude"; codex looks for "codex"), so it's natural at adapter level.

**Q5. Interactive `tmux attach -t` shape.** Three options (§2-D); the recommended `interactive_attach_argv()` free function returns argv that the CLI invokes synchronously. But what's the rmux equivalent? Does rmux expose a CLI front-end suitable for `Stdio::inherit()` terminal handover? If not, ccteam needs a built-in TUI client (much bigger scope). **Verify in W0 spike S2/S3.**

**Q6. Codex `q`+Enter close path is currently bypassing `TmuxSession`.** `send_codex_quit_keys` does two raw `Command::new("tmux") send-keys` calls. Migration target: route through `MuxBackend::send_keys` + `send_enter`. **No new trait method needed**, just a refactor — but the W2 implementation should pick this up to avoid splitting the migration.

**Q7. The post-spawn `resize-window` workaround is silently best-effort.** `tmux.rs:156` uses `let _ = …`. If `RmuxBackend::spawn` correctly sets PTY size in one shot, this whole branch disappears. If it doesn't (unlikely), the trait needs to expose either a synchronous post-spawn resize OR a "is the spawn properly sized?" probe. **Verify in W0 spike S1.**

**Q8. `capture_pane_tail` (string form) has zero production callers.** The function is in `pub use` but unused in production code. Migration could drop it entirely. Worth a separate grep across `references/` / `skills/` / `docs/` before deletion to ensure no downstream consumer.

**Q9. `tmux_available()` is itself unused in production** (only tests reference it). After migration, the equivalent backend-availability probe lives in `MuxBackend::probe()` or `Doctor::check_mux_backend()`. The current `commands::run_doctor` runs `tmux -V` inline; the migration should route this through the trait.

**Q10. `pid_is_alive` (`kill -0`) stays as a free function outside the trait** — OS-level signal probe, not mux-related. But where? `core::tmux::pid_is_alive` no longer makes sense; suggest `core::process_util::pid_is_alive` or similar. **Pure refactor, low risk.**

**Q11. Test parallelism.** All `serial_test::serial`-tagged tmux tests assume a single shared tmux server. After migration:
  - `TmuxBackend` tests retain `#[serial]`
  - `MockBackend` tests can run in parallel (no shared state)
  - `RmuxBackend` tests need their own socket per-test (rmux supports `--socket` flag) — verify in W3 the path pattern.

**Q12. Web `pane_snapshot` route depends on `capture_pane_with_ansi_from_session`.** A one-shot ANSI capture for PNG rendering. The trait's `capture(h, lines, ansi=true)` covers it; no new method needed. Confirm `MuxEvent::OutputChunk` byte format matches what vt100::Parser expects (ANSI escape sequences preserved, not stripped). **Verify in W0 spike S2.**

---

## Appendix A — file-by-file footprint

| file | LOC tmux-related | trait migration size estimate |
|---|---|---|
| `core/src/tmux.rs` | 495 (entire file) | replaced by `core/src/mux/{mod,inproc,tmux_backend}.rs` |
| `core/src/execution/claude_tui.rs` | ~70 (lines 169–193, 252–372, 406–442, 523–559) | adapter rewrite to trait calls, ~30 LOC reduction expected (no more `TmuxSession::from_name` wrapping) |
| `core/src/execution/codex_exec.rs` | ~60 (lines 207–256, 496–521, 729–751) | adapter rewrite + drop `send_codex_quit_keys` direct-call helper |
| `core/src/execution/claude_bg.rs` | 2 (line 34 import + 114 use) | trivial — only uses `session_name_for_slug` |
| `core/src/screenshot.rs` | 5 (imports + 2 call sites) | swap to `MuxBackend::capture` + `pane_dims` |
| `core/src/projects.rs` | 8 (refusal gate) | swap to `MuxBackend::exists` |
| `cli/src/commands.rs` | ~40 (doctor + attach + peek + session_attach) | swap to trait + retain raw `Command` only for interactive `attach` (per §2-D) |
| `web/src/pty.rs` | 322 (entire file) | rewrite as `MuxBackend::subscribe` consumer; refcount/registry moves into backend impl per §7-Q3 |
| `web/src/routes/pty_ws.rs` | ~50 (send_keys + resize_window + handler glue) | swap to trait calls |
| `web/src/routes/pane_snapshot.rs` | 4 (2 call sites) | swap to trait |
| tests (all) | ~3,500 across 8 files | mock injection for ~30 cases; ~10 retained as real-tmux smoke until backend retired |

**Total production tmux-related Rust**: ~1,000 LOC across 9 source files; tests another ~3,500 LOC.

---

*Audit complete; no source files modified. References: `crates/ccteam-core/src/tmux.rs` (canonical wrapper), `crates/ccteam-web/src/pty.rs` (F56 SSE registry), `docs/research/embedded-mux-unified-architecture.md` §四 (trait draft basis).*
