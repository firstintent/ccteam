# Flip-Default Migration Plan — making `CCTEAM_MUX_BACKEND` default to rmux

> Turns the "flip-default is blocked" gate item into an **executable, deterministic plan**. The blocker was never "impossible" — it's that flipping `default_backend()` to rmux naively breaks the test suite, and doing it on a 24/7 system without burn-in is bad engineering. This doc scopes the *safe* way to do it so a future session can execute step-by-step without destabilizing the green baseline.

## Why a naive flip breaks the suite (verified)

Production spawn paths call `ccteam_mux::default_backend()` (claude_tui.rs, codex_exec.rs, claude_bg.rs, orchestrator.rs, screenshot.rs, web pty.rs). `default_backend()` honors `CCTEAM_MUX_BACKEND` (commit `969b0e2`), defaulting to tmux. Many integration tests spawn through these paths with **tmux fixtures** gated on `tmux_available()` (e.g. `claude_tui_reattach_test`, `claude_tui_resume_test`, `claude_tui_env_test`, `tmux_test`, the `harness_trait_test` bg tests, orchestrator tests). If the default flips to rmux, those tests route to the rmux daemon instead of tmux and break (they assert tmux session names / pane behavior, and aren't set up to drive a live rmux daemon).

So the flip is a **test-migration**, not a one-liner.

## The migration (ordered, commit-per-step, baseline-safe)

### Step 1 — inventory the default-dependent tests
`grep -rln "tmux_available\|TmuxSession\|capture-pane\|ccteam-<slug>" crates/*/tests/` and find every test that (a) spawns through a production path AND (b) asserts tmux-specific behavior. Tag each: **tmux-specific** (must pin tmux) vs **backend-agnostic** (should pass under either).

### Step 2 — pin the tmux-specific tests
For each tmux-specific test, set `CCTEAM_MUX_BACKEND=tmux` explicitly (via the existing `EnvGuard` / `#[serial_test::serial]` pattern — see `harness_trait_test.rs`). This makes them test tmux *regardless* of the default. Commit: "pin tmux-specific integration tests to CCTEAM_MUX_BACKEND=tmux". Baseline stays green (default still tmux at this point).

### Step 3 — add rmux-default coverage for the agnostic paths
For backend-agnostic spawn tests, add an rmux-backend variant (gated on the daemon being launchable — ccteam self-hosts it, so `RMUX_SDK_DAEMON_BINARY=<ccteam bin>` + the test env). Reuse the `rmux_backend_session_roundtrip` harness pattern. These prove the production paths work under rmux. Commit separately.

### Step 4 — flip the default
Change `from_env()` / `default_backend()` in `crates/ccteam-mux/src/lib.rs`: env-unset → `RmuxBackend` (was `TmuxBackend`). Keep `CCTEAM_MUX_BACKEND=tmux` as the explicit opt-out. Update the `default_backend_defaults_to_tmux` test → `..._defaults_to_rmux`. Commit: "flip default backend tmux→rmux".

### Step 5 — full-suite green under rmux default
`cargo test --workspace --exclude ccteam-web` must be green with the flipped default (Step 2 pins protect tmux tests; Step 3 covers rmux paths). The 2 inotify flakes (`daemon_dm_*`, `daemon_wires_mock_*`) remain environmental, not regressions.

### Step 6 — CI green on all platforms
The `rmux-smoke` matrix (linux + macOS, commit `aaac3df`) must be green on a real macOS runner before this is production-trustworthy. Add Windows once the ConPTY port lands. **This step needs CI hardware, not local.**

## The burn-in caveat (why even after Steps 1-6, merge-to-main waits)

rmux v0.3.1 was published 2026-05-25 ("fresh public preview, bugs expected"). Even with the suite green under rmux-default on this **no-merge evaluation branch**, *merging to main / shipping* should wait for real-world burn-in (run a real squad under `CCTEAM_MUX_BACKEND=rmux` for days, watch for daemon hangs / PTY quirks / reconnect edge cases). The flip on THIS branch is a demonstration of "100% rmux as default"; the flip on main is a release decision gated on burn-in + macOS CI green.

## Effort estimate
Steps 1-5: ~1 focused session (1-2 subagents — one for the test inventory+pin, one for the rmux-path coverage). Step 6: CI-runner-dependent (not local). Burn-in: calendar time.

## Recommendation
This is **deliberate, deterministic work** — not a blind flip. A future session can execute Steps 1-5 safely on this branch (each step commit-isolated, baseline-protected) to reach "rmux is the default, full suite green." Steps 6 + burn-in are inherently environment/time-bound. Do NOT attempt Steps 1-5 with a near-full context window — the test-migration must not be left half-done.
