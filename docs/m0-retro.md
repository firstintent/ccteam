# M0 Retro

**Status**: 15 tasks closed (M0.1 → M0.15), 99 workspace tests green, clippy clean. Single binary `ccteam` (~5.4 kLOC across `ccteam-core` / `ccteam-cli` / `ccteam-hooks`).

## Surprises

- **tmux's default pane size in headless mode is 1×1**, not 80×24, on machines without a controlling terminal (WSL2 in our case). Any send-keys silently truncates. Worked around with `-x 200 -y 50` on `new-session` plus a follow-up `resize-window`. Tests caught this because `dispatch_phase`'s capture-pane verification kept showing empty output.
- **Multi-byte YAML in test strings**: Rust string literal `\` line continuations strip leading whitespace, which silently flattens YAML indentation and reparents `trigger:` under the wrong list level. Switched to `concat!(...)` for inline test fixtures.
- **`let _ = scoped;` drops the value** — used for "discard a Result" but in our `Drop`-bearing `ScopedSession` it tore the tmux session down before `dispatch_phase` could send keys. Renaming to a real binding fixed it.

## What worked well

- **Pure decision functions + thin side-effect appliers** (`decide_tick`, `fix_loop::decide`, `cost::classify`, `stall::classify`, `progress::is_idle`) made every state-machine path testable without tmux/claude. M0.15's e2e walks the full DAG against tempdir-rooted state.json files in milliseconds.
- **`progress::append_event` shared between hooks and orchestrator** kept the JSONL format definition single-sourced. parse-phase-end and progress-append both write the same shape.
- **`OrchestratorConfig::claude_argv`** as a pluggable command let M0.10 reset-context tests substitute `sh -c 'touch ready; sleep 60'` for the real claude — exercises the kill+restart+ready-poll routine without an API key.

## Open for M1

- Real claude e2e: M0.15 verifies the orchestrator's decisions but doesn't drive a real claude. M1 should add a claude smoke test (or codex smoke) that runs one project end-to-end and asserts the `phase_history` matches the DAG.
- `running` field in `ccteam ls --format json` is currently `null`; M1's daemon-liveness check (PID file or socket) fills it in.
- Cost rates are hardcoded in `ccteam-hooks/src/cost.rs`; M1 should read `~/.ccteam/config.yml` `model_rates` per interfaces.md §6.3.
- Stall detection logs every tick once a threshold is crossed — fine for stderr but spammy when M1 hooks telegram. Add per-level dedup at telegram-push time.

## Numbers

- 15 commits on `Goths` branch, ~3 hours wall-clock (single fast-forward of `Goths` from `96a85cf` → `9dafd46` at start, no rebases since).
- 99 tests across 3 crates: 24 ccteam-core lib units + 5 phases/state/templates/dispatch/orchestrator integration + 13 hooks integration + 5 fix-loop integration + 11 state-machine integration + 9 tmux integration + 5 context-reset integration + 3 e2e + others.
- 0 panics or `unwrap`-in-prod-path; only test code uses `unwrap`.
