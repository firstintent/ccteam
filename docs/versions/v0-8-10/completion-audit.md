# v0.8.10 Completion Audit

Audit date: 2026-06-08
Local host: `rob-ws`
Local gate evidence commit: `b0e62df979ccf225bf5e4edc4260256e42bde194`
Remote: `origin/dev` matched that commit during this audit.

This file records what is proven by current repo state and command evidence,
and what remains unproven. It is not a replacement for the nas-box005 checklist.
If a later commit changes non-documentation code, rerun the full local gate
suite before using this audit as release evidence.

## Overall Status

Not tag-ready.

The CI-fake and local deterministic gates are complete. The required
nas-box005 short smoke is still pending and must be run on the dedicated host.
The checked-in script now refuses to run as release evidence unless:

- hostname is `nas-box005`;
- worktree is clean;
- `HEAD == origin/dev`;
- required tools are present;
- `target/debug/ccteam` exists for the real IM/web leg.

## Stability Gates

| Gate | Status | Evidence |
|---|---|---|
| A1 golden-path soak | Partial | CI-fake slice passed via `scripts/smoke-im.sh`; real nas-box005 short smoke remains pending in `nas-box005-short-smoke-checklist.md`. |
| A2 failure-mode injection | Partial | CI-fake restart/replay/fault slices passed; host suspend and real netdrop are still checklist-only pending items. |
| A3 named guards for D2/D3/D4 | Passed for CI scope | `backend_literal_guard_test`, sid-bearing reset/progress tests, same-role/roleless routing tests, and file-backed stall classifier tests are included in the 1918-pass workspace gate. |
| A4 boundary timeout/retry/idempotence | Passed for CI scope | Gateway persistence, outbound replay/idempotence, WS replay, start/submit failure, and turn-timeout smoke slices passed in `scripts/smoke-im.sh`. |
| A5 baseline/gates | Passed locally | `cargo test --workspace --exclude ccteam-web`: 1918 passed, 19 ignored; `cargo test -p ccteam-web`: 276 passed; clippy/fmt/eslint/vitest all passed. |

## UX Gates

| Gate | Status | Evidence |
|---|---|---|
| B1 zero silent failure signals | Partial | IM/web deterministic failures and marker-missing signal tests pass; nas-box005 no-silent-failure checklist is still pending. |
| B2 dogfooding bug class regression tests | Passed for CI scope | Hook materialization, dynamic project registry, per-sid isolation, ANSI/capture cleanup, roleless tail/reply tests are in the local gates and handoffs. |
| B3 onboarding path | Passed for CI scope | `run_init_next_block_names_shortest_path_and_role_modes`, `cto_role_template_has_fresh_user_guidance`, `README.md`, and `docs/usage.md` carry the six-step sequence. Real phone-to-reply walk remains part of nas-box005 smoke. |
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

- `cargo test --workspace --exclude ccteam-web`: 1918 passed, 19 ignored.
- `cargo test -p ccteam-web`: 276 passed.
- `cargo clippy --workspace --all-targets -- -D warnings`: 0 issues.
- `cargo fmt --all -- --check`: passed.
- `npm run lint` in `crates/ccteam-web/web`: passed, 0 warnings.
- `npm run test:unit` in `crates/ccteam-web/web`: 142 passed.
- `scripts/smoke-im.sh`: PASS.
- `bash -n scripts/smoke-v0-8-10-real-short.sh`: passed.
- `scripts/smoke-v0-8-10-real-short.sh --help`: passed.
- `scripts/smoke-v0-8-10-real-short.sh --skip-rmux --skip-im --preflight-only`
  on `rob-ws`: correctly refused as non-nas host.
- `CCTEAM_ALLOW_NON_NAS_SMOKE=1 scripts/smoke-v0-8-10-real-short.sh --skip-rmux --skip-im --preflight-only`
  on `rob-ws`: rehearsal preflight passed and printed matching `HEAD` /
  `origin/dev`.

## Remaining Required Evidence

Run on nas-box005:

1. Preflight commands in `nas-box005-short-smoke-checklist.md`.
2. `scripts/smoke-v0-8-10-real-short.sh`.
3. Manual host suspend/resume.
4. Manual network drop/restore.
5. Restart-after-fault and no-silent-failure checks.

Only after that checklist is completed with PASS should this release be treated
as tag-ready.
