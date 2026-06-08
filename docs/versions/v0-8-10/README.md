# v0.8.10 Archive

v0.8.10 is the "core flow production-grade: STABILITY + high-quality UX"
release track.

Status: CI-fake gates complete; nas-box005 short smoke pending. Do not claim
tag-ready until `nas-box005-short-smoke-checklist.md` is completed on the
dedicated host.

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
- `nas-box005-short-smoke-checklist.md` — required real-machine short smoke
  record. It is intentionally still marked `SPECIAL MACHINE PENDING`.

## Commit Discipline

Local CI-fake evidence was collected after the v0.8.10 code changes had landed.
The nas-box005 script enforces a clean worktree and `HEAD == origin/dev` before
it records any real smoke result, so the dedicated-host record always names the
exact pushed commit under test. If any non-documentation code changes land after
the audit, rerun the full local gates before running the real smoke.
