# V0.8 W3 — mode-2 bg-in-mux: design finding

> **Owner**: W3 mode-2 implementation agent (parallel with codex / mux agents).
> **Scope**: `crates/ccteam-core/src/execution/claude_bg.rs` only.
> **Question this doc answers**: can `claude --bg` be supervised by a `MuxBackend`
> session so the daemon owns child lifecycle (gaining typed `ProcessExited`,
> crash-resilience, free peek/screenshot)?

---

## TL;DR — sub-question (a) determination

**`claude --bg` CANNOT be usefully supervised by running it inside a mux PTY.**

`claude --bg` is a *launcher*: it forks a detached background worker process,
prints the `backgrounded · <id>` marker to stdout, and the launcher invocation
**exits sub-second**. The actual agent work runs in the daemonized worker, which
is *not* a child of the `claude --bg` invocation.

Therefore, if we `backend.spawn(...)` a `claude --bg` argv inside a mux session,
the session's pane child is the *launcher*. Mux's `ProcessExited` (or, for
TmuxBackend, stream-`Closed`) fires the moment the launcher prints the marker and
dies — i.e. **near-instantly, telling us nothing about the agent**. The worker
keeps running, fully invisible to mux.

This is sub-question (a): **mux can NOT directly supervise the `--bg` worker.**

### Evidence (three independent confirmations)

1. **Doc-comment in `claude_bg.rs`** (the current direct-spawn impl):
   > "`claude --bg` blocks until the bg session detaches (sub-second typically)".
   The parent's only job is to print the marker and return.

2. **The existing test harness** (`tests/harness_trait_test.rs::
   claude_bg_start_thread_parses_backgrounded_marker`) satisfies a full
   `start_thread` with a fake `claude` that is literally:
   ```sh
   #!/bin/sh
   echo 'backgrounded · deadbeef'
   exit 0
   ```
   A print-and-exit script is a complete stand-in — proving the launcher's
   contract is *only* "emit marker, exit". There is no long-lived process for
   mux to attach to.

3. **`close_thread` kills the worker via `state.json::pid`, not the launcher
   pid.** `ClaudeBgAdapter::close_thread` reads
   `~/.claude/jobs/<job_id>/state.json`, extracts the worker-written `pid`, and
   SIGTERMs *that*. The launcher pid is long gone by then. This worker-pid
   asymmetry is the smoking gun: the thing we want to supervise (the worker) is
   reachable only through the file it writes — never through the launcher
   process mux would hold.

4. **Corroborating, from the Claude Code reference** (`references/claude-code/
   src/cli/bg.ts` + `main.tsx:1535`): the `--bg` session machinery writes
   per-session files under `$CLAUDE_CONFIG_HOME/sessions/<pid>.json`, and the
   `--bg`+`--agent` combination is gated behind a `feature('BG_SESSIONS')` flag.
   The bg path is a vendor-internal, feature-flagged surface — fragile to depend
   on for process supervision.

---

## Path analysis (sub-question (b))

The task laid out two non-(a) options. Both assessed:

### Path A — register the worker pid with the mux daemon (observability-only)

**BLOCKED.** The `MuxBackend` trait (`crates/ccteam-mux/src/lib.rs`) has no
`register_external_pid` / `track_pid` / `adopt` method. `spawn(MuxSessionSpec)`
is the only entry, and it launches an argv — it cannot adopt an
already-detached pid. Per W3 scope, this agent owns only `claude_bg.rs` and must
STOP rather than add a `MuxBackend` method. So Path A is not available without
cross-crate API the agent may not author. **Reported, not pursued.**

### Path B — switch bg-style single-turn work to a FOREGROUND invocation inside a mux PTY

**VIABLE.** Claude Code's stable non-interactive surface is `-p` / `--print`
(`main.tsx:1166`, documented as "starts an interactive session by default, use
`-p/--print` for non-interactive output"). Key properties verified in the
reference:

- `--agent <agent>` is a **top-level option** (`main.tsx:1377`), orthogonal to
  `-p` vs `--bg`. So `claude -p <prompt> --agent <role>` is well-formed.
- `-p` runs the agentic loop **in the foreground to completion**, streaming to
  stdout, then exits with a process exit code. `--max-turns`, `--max-spend`
  etc. are all "(only works with --print)" — print mode is the supported
  batch/headless path.
- Because the `-p` process stays alive for the whole turn and exits when done,
  a mux session whose pane child is `claude -p ...` gives mux a **real** exit
  signal that corresponds to agent completion — exactly the W3 goal.

So Path B trades the `--bg` self-detach for a foreground `-p` run *inside* the
mux PTY. Mux owns the child lifecycle; the agent's stdout is captured in the
pane (peek/screenshot for free); on exit, mux reports termination.

---

## TmuxBackend caveat (default backend does NOT emit typed ProcessExited)

The W3 acceptance gates run against the **default** backend (`tmux`, via
`CCTEAM_MUX_BACKEND` unset). Reading `tmux_backend/subscribe.rs`:
`TmuxBackend::subscribe` emits `OutputChunk` / `PatternMatched` /
`OutputDropped`, and **ends the stream on broadcast `Closed`**. It never emits
`MuxEvent::ProcessExited` — the trait doc on that variant says explicitly
*"RmuxBackend (W2) uses this; TmuxBackend never emits."*

Consequence for Path B under TmuxBackend: agent completion surfaces as
**stream-end (the subscribe stream returns `None`)**, not as a typed
`ProcessExited { code }`. The exit *code* is only available from the daemon-true
`RmuxBackend` (W2) or from a follow-up `backend.exists()` / `is_alive()` poll.
This is fine for the "is it done?" signal the orchestrator needs, but the
typed-`ProcessExited`-with-code gain the W3 goal advertises is **only realized
under RmuxBackend**, not the V0.8-default tmux backend.

---

## Decision for this wave

1. **Do not migrate the default `claude --bg` path.** It self-detaches; mux
   adds nothing and would break the marker/job_id/`raw_extras` contract that
   F80 + the orchestrator poller depend on. The existing direct-spawn +
   file-based F80 poller stays the default, untouched.

2. **Add an opt-in foreground-in-mux path (Path B), default OFF**, behind an env
   flag (`CCTEAM_CLAUDE_BG_VIA_MUX=1`). When enabled, `start_thread`:
   - builds `claude -p <prompt-from-extra_args> --agent <role>
     --dangerously-skip-permissions` (foreground, NOT `--bg`),
   - `backend.spawn(MuxSessionSpec { kind: Ephemeral, name: "ccteam-bg-<sid>",
     ... })` so the daemon owns the child,
   - returns a `ThreadHandle` whose `raw_extras` records `{"mux_session":
     "<name>", "via_mux": true}` while **preserving** the `tmux_session` field
     and `identity` shape so downstream code is unperturbed,
   - completion is observed by the mux session ending (stream-`Closed` under
     tmux; typed `ProcessExited` under rmux W2).

   This lands *something* that moves mode-2 toward mux observability without
   touching the default flow — exactly the task's "land SOMETHING" instruction,
   while honoring "behavior preservation is paramount" for the default.

3. **`close_thread`** under the mux path kills via `backend.kill(session_id)`;
   the legacy file-based SIGTERM stays for the default path.

### Why opt-in, not default-flip

- The `-p` foreground run has a **different cost/lifecycle profile** than
  `--bg` (it holds an executor/pane for the whole turn instead of fire-and-
  forget). Flipping the default would change concurrency + budget semantics for
  every existing mode-2 workflow — out of W3 scope.
- The typed-`ProcessExited` payoff is gated on RmuxBackend (W2), which is not
  the V0.8 default yet (W7 flips it). Until then the gain over the F80 poller is
  modest under tmux.
- Keeping the marker/F80 path as the untouched default preserves the entire
  existing test + orchestrator contract.

A future wave (W4/W7) can promote the mux path to default once RmuxBackend is
the default backend and the cost/lifecycle delta is measured.
