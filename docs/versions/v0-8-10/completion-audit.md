# v0.8.10 Completion Audit

Audit date: 2026-06-09
Local host: `rob-ws`
Local gate evidence commit: `e38ff81425ffafc9b75f2df0820ba92eb481da18`
Remote: `origin/dev` matched that commit during this audit.

This file records what is proven by current repo state and command evidence,
and what remains unproven. It is not a replacement for the target-host
checklist.
If a later commit changes non-documentation code, rerun the full local gate
suite before using this audit as release evidence.

## Overall Status

Tag-ready for the user-directed local target-host gate.

The CI-fake, local deterministic gates, and local target-host short smoke are
complete at `e38ff81425ffafc9b75f2df0820ba92eb481da18`. Latest user direction
moved the checklist execution from `nas-box005` to this local workstation. The
checked-in script refuses to run as release evidence unless:

- hostname matches `CCTEAM_REAL_SMOKE_HOST` (default `nas-box005`; latest run
  used `rob-ws`);
- worktree is clean;
- `HEAD == origin/dev`;
- required tools are present;
- `target/debug/ccteam` exists for the real IM/web leg.

Host-fault caveat: the local target-host smoke proves a 600s `SIGSTOP`/`SIGCONT`
daemon freeze and WebSocket disconnect/reconnect backlog replay. It does not
claim full ACPI system suspend, RTC wake, or system-level outbound network
blocking.

## Stability Gates

| Gate | Status | Evidence |
|---|---|---|
| A1 golden-path soak | Passed for local target-host scope | CI-fake slice passed; local target-host short smoke passed at `e38ff81` (`scripts/smoke-v0-8-10-real-short.sh`: real rmux PASS + real IM WS restart + 600s SIGSTOP/SIGCONT + WS disconnect/reconnect + Claude pane-death fault PASS). Full ACPI suspend/system-level net block not claimed. |
| A2 failure-mode injection | Passed for local target-host scope | CI-fake restart/replay/fault slices passed; local smoke covered daemon restart, 600s SIGSTOP/SIGCONT recovery, WS reconnect exactly-once backlog replay, and Claude pane death user-visible failure. Codex app-server disconnect remains opt-in/best-effort. |
| A3 named guards for D2/D3/D4 | Passed for CI scope | `backend_literal_guard_test`, sid-bearing reset/progress tests, same-role/roleless routing tests, and file-backed stall classifier tests are included in the 1920-pass workspace gate. |
| A4 boundary timeout/retry/idempotence | Passed for CI scope | Gateway persistence, outbound replay/idempotence, WS replay, start/submit failure, and turn-timeout smoke slices passed in `scripts/smoke-im.sh`. |
| A5 baseline/gates | Passed locally | `cargo test --workspace --exclude ccteam-web`: 1920 passed, 19 ignored; `cargo test -p ccteam-web`: 276 passed; clippy/fmt/eslint/vitest/Playwright all passed. |

## UX Gates

| Gate | Status | Evidence |
|---|---|---|
| B1 zero silent failure signals | Passed for local target-host scope | IM/web deterministic failures and marker-missing signal tests pass; local short smoke recovered SIGSTOP/WS reconnect visibly exactly once and Claude pane death produced one user-visible `发送失败: tmux session missing:` message. |
| B2 dogfooding bug class regression tests | Passed for CI scope | Hook materialization, dynamic project registry, per-sid isolation, ANSI/capture cleanup, roleless tail/reply tests are in the local gates and handoffs. |
| B3 onboarding path | Passed for CI scope | `run_init_next_block_names_shortest_path_and_role_modes`, `cto_role_template_has_fresh_user_guidance`, `README.md`, and `docs/usage.md` carry the six-step sequence. Real phone-to-reply walk remains manual/best-effort evidence. |
| B4 model support honesty | Passed for CI scope | `is_claude_family` tests plus gateway warn-once positive/negative tests pass; README/usage supported-model matrix is present. |
| B5 observability | Passed for CI scope | `/sessions` sid-aware activity tests, StatusView tests, and CostPill loaded/null-cap tests pass. |
| B6 error copy lint | Passed for CI scope | Core IM/web error tests assert Chinese next-step messages and no `gateway error`; grep of tier-1 docs and code found no live user-facing `gateway error` string outside negative test assertions. |

## OUT-Gate

Current evidence:

- `AgentVendor` remains `Claude` + `Codex`.
- No new gateway command, CLI command, web route, page, IM channel, model
  adapter, or config key was added.
- The two intentional micro-exceptions are present and documented:
  model warn-once and read-only activity labels.
- D7 removals are complete in `crates/`: no remaining `marker_reporter`,
  `MarkerReporter`, `CHAT_BOT_MARKER_STUCK`,
  `render_project_settings_agent_team`,
  `write_project_settings_agent_team`, or
  `PROJECT_SETTINGS_AGENT_TEAM_JSON` references.

## Final Local Gate Results

- `cargo test --workspace --exclude ccteam-web`: 1920 passed, 19 ignored.
- `cargo test -p ccteam-web`: 276 passed.
- `cargo clippy --workspace --all-targets -- -D warnings`: 0 issues.
- `cargo fmt --all -- --check`: passed.
- `npm run lint` in `crates/ccteam-web/web`: passed, 0 warnings.
- `npm run test:unit` in `crates/ccteam-web/web`: 142 passed.
- `npm test` in `crates/ccteam-web/web`: 4 passed.
- `scripts/smoke-im.sh`: PASS.
- `bash -n scripts/smoke-v0-8-10-real-short.sh`: passed.
- `scripts/smoke-v0-8-10-real-short.sh --help`: passed.
- `scripts/smoke-v0-8-10-real-short.sh --skip-rmux --skip-im --preflight-only`
  on `rob-ws` without `CCTEAM_REAL_SMOKE_HOST=rob-ws`: correctly refused as
  target-host mismatch.
- `CCTEAM_REAL_SMOKE_HOST=rob-ws scripts/smoke-v0-8-10-real-short.sh`:
  PASS; log directory `/tmp/ccteam-v0-8-10-real-short`.

## Local Target-Host Smoke

Run on `rob-ws` at commit
`e38ff81425ffafc9b75f2df0820ba92eb481da18` with clean worktree and
`HEAD == origin/dev`:

- `cargo build --workspace`: passed.
- `scripts/smoke-v0-8-10-real-short.sh`: PASS.
- `real_rmux`: PASS, 7 passed.
- `real_im_ws_restart_faults`: PASS.
- `real_ws_dual_harness_smoke`: PASS, 1 passed in 616.66s.
- `smoke-im`: PASS.
- The script log is under `/tmp/ccteam-v0-8-10-real-short/` on `rob-ws`.

During this audit, two prior automated attempts exposed a real Claude TUI
submit race: `claude --agent reviewer --model sonnet ...` was launched
correctly, but immediate `send-keys Enter` could leave the prompt in the
composer. The final fix waits for the TUI input settle before Enter; the
automated smoke passed after that change.

## Remaining Best-Effort / Not Claimed

The local checklist is complete for the user-directed target-host scope. The
following are not claimed by this audit and remain best-effort or out of this
local run:

1. Full ACPI suspend / RTC wake.
2. System-level outbound network block.
3. Codex app-server disconnect real smoke.
4. Long-run M>=50 / 24h dogfood.
5. Marketplace install real-machine verification, blocked on hub public
   availability.
