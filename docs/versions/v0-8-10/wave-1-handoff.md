# v0.8.10 Wave 1 Handoff - D1/D6 Fake Soak

## Decided

- Extended existing fake harnesses instead of creating a new e2e framework.
- Added CI-fake guards for restart/replay and notification routing paths.
- Kept the real-machine short smoke honest by adding a one-command script and
  checklist, marked nas-box005 pending.
- Strengthened `scripts/smoke-im.sh` around the IM/web fake slice.

## Rejected

- Did not claim host suspend, real rmux, or real Claude smoke as locally green.
- Did not add a new IM channel or route.
- Did not make Codex real-machine long-run a blocker.

## Risks

- nas-box005 short smoke is still pending by design.
- CI-fake covers deterministic failure modes; it does not prove terminal byte
  rendering or host suspend behavior on the target box.

## Files

- `crates/ccteam-im/src/gateway.rs`
- `crates/ccteam-im/tests/inbound_wiring_test.rs`
- `scripts/smoke-im.sh`
- `scripts/smoke-v0-8-10-real-short.sh`
- `docs/versions/v0-8-10/nas-box005-short-smoke-checklist.md`

## Remaining

- P2: backend literal, sid identity, stall SoT.
- P3: boundary reliability matrix.
- P4/P5: UX signal, model honesty, dead-code/docs/version.

## Gate Results

- Phase committed as `a4160de test(v0.8.10): add phase 1 fake soak guards`.
- Full phase gate was run before advancing; later phases re-ran the full
  workspace/web/clippy/fmt/eslint/vitest gates.
