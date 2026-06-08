# v0.8.10 Wave 4 Handoff - D8/D9 UX Signals

## Decided

- Upgraded marker-silence from log-only WARN to one user-facing stream error
  while preserving the existing `MarkerSilenceWatch` detector.
- Added human-readable IM/web gateway error formatting for core-loop start,
  submit, and unknown-project failures.
- Added `is_claude_family(model)` in core and emit warn-once only for
  Claude-routed non-Claude role models.
- Added sid-aware `last_activity_seconds` on existing `/sessions` data and
  rendered it in the existing Status view.
- Updated `init`, `cto_role.md`, `README.md`, and `docs/usage.md` around the
  single six-step onboarding path and supported-model matrix.
- Added cost-pill loaded-state coverage for null cap and configured cap.

## Rejected

- Did not add any new command, route, web page, status RPC, model adapter, or
  streaming panel.
- Did not reuse pricing unknown-model warnings for model-family honesty.
- Did not make healthy quiet sessions fail; the marker-silence tests include
  an explicit false-positive guard.

## Risks

- Model warning delivery is best-effort through the gateway event stream; it is
  intentionally an honesty signal and does not block spawn.
- Status activity is read-only and derived from existing `progress.jsonl`; it
  does not claim a new live activity feed.

## Files

- `crates/ccteam-core/src/model_support.rs`
- `crates/ccteam-core/src/progress.rs`
- `crates/ccteam-harness/src/execution/claude_tui.rs`
- `crates/ccteam-harness/src/execution/progress_bridge.rs`
- `crates/ccteam-harness/tests/claude_tui_silence_warn_test.rs`
- `crates/ccteam-hooks/src/chat_progress.rs`
- `crates/ccteam-im/src/daemon.rs`
- `crates/ccteam-im/src/gateway.rs`
- `crates/ccteam-im/tests/inbound_wiring_test.rs`
- `crates/ccteam-web/src/routes/sessions_api.rs`
- `crates/ccteam-web/web/src/lib/sessionsApi.ts`
- `crates/ccteam-web/web/src/pages/StatusView.tsx`
- `crates/ccteam-web/web/src/components/CostPill.tsx`
- `crates/ccteam-cli/src/commands.rs`
- `crates/ccteam-core/src/templates/cto_role.md`
- `README.md`
- `docs/usage.md`

## Remaining

- P5: D7 dead-code table, version/docs finalization, final workspace gates,
  commit and push.

## Gate Results

- `cargo test -p ccteam-core model_support`: 2 passed.
- `cargo test -p ccteam-core last_event_for_sid`: 1 passed.
- `cargo test -p ccteam-harness --test claude_tui_silence_warn_test`: 2 passed.
- `cargo test -p ccteam-im` after fixes: 347 passed.
- `cargo test -p ccteam-web`: 276 passed.
- `npm run test:unit -- sessionsApi`: 19 passed.
- `npm run test:unit -- StatusView`: 8 passed.
- `npm run test:unit -- CostPill`: 3 passed.
