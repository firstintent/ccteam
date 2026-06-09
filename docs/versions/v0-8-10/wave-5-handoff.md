# v0.8.10 Wave 5 Handoff - D7/Version/OUT Gate

## Decided

- Deleted the unused `marker_reporter` registry and `MarkerReporter` trait while
  keeping `MarkerSilenceWatch` as the live marker-missing signal path.
- Deleted `CHAT_BOT_MARKER_STUCK` builder/const/taxonomy arm/test.
- Deleted the retired agent-team settings template and public helper exports,
  and removed the old flow tests that were the only callers.
- Left `--restart-team` text in deferred ccteam-flow only, per PRD §九; daemon
  and web do not run that orchestrator path.
- Bumped workspace version to `0.8.10` and updated CLAUDE/current docs.

## Rejected

- Did not delete deferred ccteam-flow `--restart-team` internals in this
  version; PRD marks them out of live daemon scope.
- Did not treat nas-box005 as the only possible execution target after the
  latest user direction; the target-host checklist was completed locally on
  `rob-ws`.
- Did not add new user-reachable surface while cleaning docs.

## Risks

- Full ACPI suspend / RTC wake and system-level outbound network blocking are
  not claimed by the local target-host smoke. The completed local host-fault
  scope is 600s daemon `SIGSTOP`/`SIGCONT` plus WebSocket reconnect.
- Market install real-machine verification remains best-effort and
  blocked-on ccteam-hub public availability.

## Files

- `Cargo.toml`
- `CLAUDE.md`
- `docs/tech-design.md`
- `docs/usage.md`
- `docs/versions/v0-8-10/wave-*.md`
- `crates/ccteam-harness/src/adapter.rs`
- `crates/ccteam-harness/src/execution/mod.rs`
- `crates/ccteam-harness/src/execution/claude_tui.rs`
- `crates/ccteam-harness/src/execution/progress_bridge.rs`
- `crates/ccteam-core/src/progress.rs`
- `crates/ccteam-core/src/templates/mod.rs`
- `crates/ccteam-core/src/lib.rs`
- `crates/ccteam-flow/tests/agent_team_spawn_test.rs`

## Remaining

- Phase 4/5 changes have been committed and pushed to `origin/dev`.
- Local target-host short smoke passed on `rob-ws` at
  `e38ff81425ffafc9b75f2df0820ba92eb481da18`.
- No required manual host-fault checklist items remain for the user-directed
  local target-host scope.

## Gate Results

- `cargo build -p ccteam-harness -p ccteam-core -p ccteam-flow`: passed.
- `cargo test -p ccteam-core -p ccteam-harness -p ccteam-flow`: 1233 passed,
  19 ignored.
- `cargo test --workspace --exclude ccteam-web`: 1920 passed, 19 ignored.
- `cargo test -p ccteam-web`: 276 passed.
- `cargo clippy --workspace --all-targets -- -D warnings`: 0 issues.
- `cargo fmt --all -- --check`: passed.
- `npm run lint` in `crates/ccteam-web/web`: passed, 0 warnings.
- `npm run test:unit` in `crates/ccteam-web/web`: 142 passed.
- `npm test` in `crates/ccteam-web/web`: 4 passed.
- `scripts/smoke-im.sh`: PASS.
- `scripts/smoke-v0-8-10-real-short.sh` on `rob-ws`: PASS.
- `real_ws_dual_harness_smoke`: 1 passed in 616.66s with restart + 600s
  `SIGSTOP`/`SIGCONT` + WS reconnect + pane-death fault.
