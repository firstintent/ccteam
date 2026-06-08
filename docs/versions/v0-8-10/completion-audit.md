# v0.8.10 Completion Audit

Audit date: 2026-06-09
Local host: `rob-ws`
Local gate evidence commit: `b7edeeeaf64d58ecba3f2f9fa014e3e09651b58d`
Remote: `origin/dev` matched that commit during this audit.

This file records what is proven by current repo state and command evidence,
and what remains unproven. It is not a replacement for the nas-box005 checklist.
If a later commit changes non-documentation code, rerun the full local gate
suite before using this audit as release evidence.

## Overall Status

Not tag-ready until the manual nas-box005 host-fault checklist is completed.

The CI-fake, local deterministic gates, and automated nas-box005 short smoke
are complete at `b7edeeeaf64d58ecba3f2f9fa014e3e09651b58d`. The remaining
release evidence is the manual host suspend/netdrop/no-silent-failure section
in `nas-box005-short-smoke-checklist.md`. The checked-in script refuses to run
as release evidence unless:

- hostname is `nas-box005`;
- worktree is clean;
- `HEAD == origin/dev`;
- required tools are present;
- `target/debug/ccteam` exists for the real IM/web leg.

## Stability Gates

| Gate | Status | Evidence |
|---|---|---|
| A1 golden-path soak | Partial | CI-fake slice passed; automated nas-box005 short smoke passed at `b7edeee` (`scripts/smoke-v0-8-10-real-short.sh`: real rmux PASS + real IM WS restart/faults PASS). Manual host suspend/netdrop remains pending. |
| A2 failure-mode injection | Partial | CI-fake restart/replay/fault slices passed; automated nas-box005 restart + Claude pane death fault passed; host suspend and real netdrop are still checklist-only pending items. |
| A3 named guards for D2/D3/D4 | Passed for CI scope | `backend_literal_guard_test`, sid-bearing reset/progress tests, same-role/roleless routing tests, and file-backed stall classifier tests are included in the 1920-pass workspace gate. |
| A4 boundary timeout/retry/idempotence | Passed for CI scope | Gateway persistence, outbound replay/idempotence, WS replay, start/submit failure, and turn-timeout smoke slices passed in `scripts/smoke-im.sh`. |
| A5 baseline/gates | Passed locally | `cargo test --workspace --exclude ccteam-web`: 1920 passed, 19 ignored; `cargo test -p ccteam-web`: 276 passed; clippy/fmt/eslint/vitest/Playwright all passed. |

## UX Gates

| Gate | Status | Evidence |
|---|---|---|
| B1 zero silent failure signals | Partial | IM/web deterministic failures and marker-missing signal tests pass; automated nas-box005 Claude pane death produced one user-visible `发送失败: tmux session missing:` message; manual no-silent-failure checklist remains pending. |
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
  on `rob-ws`: correctly refused as non-nas host.
- `CCTEAM_ALLOW_NON_NAS_SMOKE=1 scripts/smoke-v0-8-10-real-short.sh --skip-rmux --skip-im --preflight-only`
  on `rob-ws`: rehearsal preflight passed and printed matching `HEAD` /
  `origin/dev`.

## nas-box005 Automated Smoke

Run on `nas-box005` at commit
`b7edeeeaf64d58ecba3f2f9fa014e3e09651b58d` with clean worktree and
`HEAD == origin/dev`:

- `cargo build --workspace`: passed.
- `scripts/smoke-v0-8-10-real-short.sh`: PASS.
- `real_rmux`: PASS, 7 passed.
- `real_im_ws_restart_faults`: PASS.
- The script log is under `/tmp/ccteam-v0-8-10-real-short/` on nas-box005.

During this audit, two prior automated attempts exposed a real Claude TUI
submit race: `claude --agent reviewer --model sonnet ...` was launched
correctly, but immediate `send-keys Enter` could leave the prompt in the
composer. The final fix waits for the TUI input settle before Enter; the
automated nas-box005 smoke passed after that change.

## Remaining Required Evidence

Run and record in `nas-box005-short-smoke-checklist.md`:

1. Manual host suspend/resume.
2. Manual network drop/restore.
3. Restart-after-fault after those manual checks.
4. Manual no-silent-failure check.
5. Optional/best-effort Codex app-server disconnect, long-run, and marketplace
   install checks.

Only after that checklist is completed with PASS should this release be treated
as tag-ready.
