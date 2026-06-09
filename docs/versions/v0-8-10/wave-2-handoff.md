# v0.8.10 Wave 2 Handoff - D2/D3/D4

## Decided

- D3 reset events now keep `role` as a label and carry explicit `sid` for all
  `chat_session_reset*` builders. Hook session-end and Claude resume-fallback
  emit the sid-keyed shape; roleless reset is covered by tests.
- D3 transcript tail routing is sid-only for live handles. The old
  `sid.empty -> role` fallback was removed from `events()` and marker lookup;
  fanout coverage now uses same cwd + same role + three distinct sids.
- D2 production tmux shell-outs are confined to the backend whitelist
  (`tmux_ops.rs` and `tmux_backend/`). `init` health check now gets `tmux -V`
  through `tmux_version()` in the backend layer.
- D4 single stall truth uses existing `progress.jsonl`. The gateway turn
  watchdog appends `chat_turn_timeout { stuck:true }`; CLI and web read the
  shared `ccteam_core::stall::classify_progress_stall` classifier. Pure age can
  warn, but cannot say STUCK without the file-backed timeout event.

## Rejected

- Did not add any new RPC, config key, page, nav item, or command.
- Did not reintroduce role fallback for missing sid handles; tests were updated
  to the current sid-keyed session model instead.
- Did not make StatusView a new streaming panel. It only renders the existing
  `SessionView.status` field as a read-only activity label.

## Risks

- `SessionView.status` is still project-level progress-derived when returned
  from the web route; multiple sessions in one project share the same
  `progress.jsonl` glance. This matches the current file-backed SoT constraint
  and avoids a new status endpoint.
- `ccteam status` nested session rows also use the project-level progress
  glance. Per-session precision needs richer sid-bearing progress events and is
  not introduced in this phase.
- nas-box005 real short smoke remains pending by design; this phase only proves
  CI-fake gates.

## Files

- `crates/ccteam-harness/src/execution/progress_bridge.rs`
- `crates/ccteam-hooks/src/chat_progress.rs`
- `crates/ccteam-harness/src/execution/claude_tui.rs`
- `crates/ccteam-harness/tests/claude_tui_fanout_test.rs`
- `crates/ccteam-harness/tests/claude_tui_silence_warn_test.rs`
- `crates/ccteam-harness/tests/backend_literal_guard_test.rs`
- `crates/ccteam-core/src/stall.rs`
- `crates/ccteam-im/src/gateway.rs`
- `crates/ccteam-web/src/routes/sessions_api.rs`
- `crates/ccteam-web/web/src/pages/StatusView.tsx`

## Remaining

- P3/D5: boundary timeout/retry/idempotence matrix beyond the D1/D6 fake slices.
- P4/D8/D9: user-facing failure signal coverage, model support warn-once,
  onboarding copy, final UX polish.
- P5/D7: dead-code table execution, docs/version bump, OUT-gate set guards.

## Gate Results

- `cargo test --workspace --exclude ccteam-web`: 1917 passed, 19 ignored.
- `cargo test -p ccteam-web`: 275 passed.
- `cargo clippy --workspace --all-targets -- -D warnings`: 0 issues.
- `cargo fmt --all -- --check`: passed.
- `npm run lint` in `crates/ccteam-web/web`: passed, 0 warnings.
- `npm run test:unit` in `crates/ccteam-web/web`: 138 passed.
- `scripts/smoke-im.sh`: PASS.
