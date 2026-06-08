# v0.8.10 Archive

v0.8.10 is the "core flow production-grade: STABILITY + high-quality UX"
release track.

Status: CI-fake gates complete; local target-host short smoke passed on
`rob-ws` at `e38ff81425ffafc9b75f2df0820ba92eb481da18`. The host-fault record
uses `SIGSTOP`/`SIGCONT` daemon freeze plus WebSocket disconnect/reconnect; it
does not claim full ACPI suspend or system-level outbound network blocking.

## Source Documents

- `prd.md` — authoritative scope and acceptance rubric.
- `dev-prompt.md` — phase orchestration and honesty gates.
- `prd-review.html` — adversarial PRD review artifact.

## Implementation Handoffs

- `wave-0-handoff.md` — baseline, anchor refresh, initial backlog.
- `wave-1-handoff.md` — D1/D6 CI-fake soak and notification guards.
- `wave-2-handoff.md` — D2/D3/D4 backend, identity, and stall SoT guards.
- `wave-3-handoff.md` — D5 boundary reliability.
- `wave-4-handoff.md` — D8/D9 UX signals and model/onboarding/status polish.
- `wave-5-handoff.md` — D7 dead-code, version, docs, and OUT-gate.

## Release Evidence

- `completion-audit.md` — requirement-by-requirement A1-A5/B1-B6 status.
- `nas-box005-short-smoke-checklist.md` — target-host real-machine short smoke
  record. The latest run is local (`rob-ws`) per user direction and is marked
  PASS with the host-fault scope caveat above.

## Commit Discipline

Local CI-fake evidence was collected after the v0.8.10 code changes had landed.
The short-smoke script enforces a clean worktree and `HEAD == origin/dev`
before it records any real smoke result, so the target-host record names the
exact pushed commit under test. If any non-documentation code changes land
after the audit, rerun the full local gates before running the real smoke.
