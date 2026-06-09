# v0.8.10 Wave 3 Handoff - D5 Boundary Reliability

## Decided

- Hardened gateway persistence around multiple sessions with distinct secrets.
- Expanded restore coverage so `/sessions`, `/use`, and routing remain correct
  after gateway state reload.
- Fixed a cost-summary test race by serializing tests that mutate the global
  transcript cache.

## Rejected

- Did not introduce a new persistence store or new recovery endpoint.
- Did not widen boundary reliability into ccteam-flow orchestration.
- Did not hide flaky tests; the cache race was made deterministic.

## Risks

- Real app-server socket and host-level netdrop remain covered by the
  nas-box005 smoke script/checklist, not by local sandbox execution.

## Files

- `crates/ccteam-im/src/gateway.rs`
- `crates/ccteam-core/tests/cost_summary_test.rs`

## Remaining

- P4: user-facing failure signals, model support honesty, onboarding, status
  activity labels, cost pill edge states.
- P5: D7 dead-code, version/docs, final OUT-gate evidence.

## Gate Results

- `cargo test --workspace --exclude ccteam-web`: 1917 passed, 19 ignored.
- `cargo test -p ccteam-web`: 275 passed.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo fmt --all -- --check`: clean.
- `npm run lint`: clean.
- `npm run test:unit`: 138 passed.
- `scripts/smoke-im.sh`: PASS.
