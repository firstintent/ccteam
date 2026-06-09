# v0.8.10 Wave 0 Handoff - Baseline and Anchors

## Decided

- Rebased on `origin/dev` and recorded the start point before implementation.
- Re-ran the baseline and used the observed green count as the phase floor.
- Re-grepped PRD anchors after dev branch movement; line numbers were treated
  as unstable and symbol names as authoritative.
- D7 backlog was limited to still-live core-loop residues; bug1/2/3/5/6 were
  treated as already-fixed classes to guard, not duplicate implementation scope.

## Rejected

- Did not re-open v0.8.9 feature work.
- Did not treat nas-box005 real smoke as satisfied by local CI-fake tests.
- Did not add new user-reachable commands, routes, pages, vendors, or channels.

## Risks

- Real rmux + real Claude host faults remain machine-dependent and must be run
  on nas-box005 using the checked-in checklist/script.
- Line numbers in the PRD remain historical; current verification should use
  symbols and tests.

## Files

- `docs/versions/v0-8-10/prd.md`
- `docs/versions/v0-8-10/dev-prompt.md`
- `docs/versions/v0-8-10/nas-box005-short-smoke-checklist.md`

## Remaining

- P1: CI-fake soak and notification reliability.
- P2: D2/D3/D4 structural guards.
- P3: D5 boundary reliability.
- P4: D8/D9 UX signal and clarity.
- P5: D7 dead-code, docs, version, OUT-gate.

## Gate Results

- Phase start `origin/dev`: recorded in the Phase 0 commit context.
- `cargo test --workspace --exclude ccteam-web`: baseline was green and used as
  the floor for later phases.
- `ccteam-web`, clippy, fmt, eslint, and vitest were re-run in later phase gates.
