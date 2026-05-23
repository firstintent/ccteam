# V0.6.5 Wave 2 — Handoff

> **Status:** Wave 2 ship-complete · 3 / 3 findings (5 F-tags) merged · baseline **1576 / 1** (target 1534 / 1, +42 over) · clippy 0 warnings.
> **Window:** 2026-05-24 (single calendar day, 3 parallel Opus worktrees).
> **PRs merged:** [#104](https://github.com/firstintent/ccteam/pull/104) F155+F156 · [#105](https://github.com/firstintent/ccteam/pull/105) F152 · [#106](https://github.com/firstintent/ccteam/pull/106) F153+F154.

---

## Decided

| Finding | Decision |
|---|---|
| **F152** | New module `crates/ccteam-core/src/advise.rs` (~1k LOC). `advise_vote(question, vendors) -> VoteVerdict` parallel-spawns one advisor per vendor (Claude via existing claude-tui adapter; Codex via `CodexExecAdapter` running `codex exec --json`). Synthesizer summarizes verdicts as majority/unanimous/split with per-vendor original answer + token cost. Budget enforcement: per-vendor `max_cost_usd_per_24h` checked from new `<ccteam_root>/cost-budget.json` ledger (atomic-rename writes + 48h GC); flat ~$0.005/call estimate for vendors without usage block. Codex-unavailable path explicit `codex_status:"unavailable"` (no panic) — verdict prose says "Codex unavailable: <reason>". `mcp__ccteam__advise_vote` MCP wired in `mcp_advise_tools.rs`. 15 unit tests in advise.rs + 5 hermetic e2e in `mcp_advise_vote_test.rs` (uses `CCTEAM_CLAUDE_BIN` / `CCTEAM_CODEX_BIN` fake script fixtures — hermetic, no real binaries needed). |
| **F153** | `advise_parallel` fn landed in F152 PR #105 alongside `advise_vote` (shared `run_claude_advisor` / `run_codex_advisor` / `append_budget_sample` helpers — zero dup spawn code). W2-T2 added the 5 missing hermetic e2e tests in `mcp_advise_parallel_test.rs` (N=4 round-robin / N=2 claude-only / codex-unavailable slot / budget exceeded / n-out-of-range invalid_input). `mcp__ccteam__advise_parallel` returns per-vendor raw outputs without synthesizer verdict (unlike `advise_vote`). |
| **F154** | `skills/ccteam-advise/SKILL.md` body rewritten — `grep -E "Wave [123]|STUB|NotImplemented|占位|准备中" skills/ccteam-advise/SKILL.md` = 0 hits. Intent path docs aligned (`vote` → `mcp__ccteam__advise_vote`; `parallel` → `mcp__ccteam__advise_parallel`). Example usage added (typical question + sample verdict output). |
| **F155** | `ccteam doctor --check-codex-auto-critic` flag added in `crates/ccteam-cli/src/{main.rs, commands.rs}`. `run_check_codex_auto_critic()` honors `$CCTEAM_CODEX_BIN`, runs `<bin> --version` + one-shot `<bin> exec --json --skip-git-repo-check` canary, emits single JSON line on stdout. Exit codes: **0** (available + well-formed `turn.completed` JSONL → skill injects `executor: codex`), **2** (binary missing / version probe failed), **3** (output malformed → silent fallback). 6 e2e tests in `doctor_codex_auto_critic_test.rs`. `ccteam-creator` SKILL.md Phase 3.5 updated to consult the doctor flag subprocess (replaces inline `codex --version && codex login status`). |
| **F156** | **Verified + partial defer.** Bash spawn path (skill body runs `codex exec --json` directly in parallel with Claude `Task` spawns) mechanically verified; guarded by 3 tests in `team_3reviewer_codex_critic_test.rs`. Daemon-routed variant (route critic through `CodexExecAdapter` for unified cost accounting) **explicitly deferred past V0.6.5** — rationale: even with `advise_*` MCP shipped this wave, cost rollup on top sits behind those landings and ergonomics needs separate UX iteration. `grep "in V0\.7" skills/ccteam-team/SKILL.md` = 0 hits (acceptance #1); deferral phrased "explicitly deferred past V0.6.5" + "Track in the V0.7 epic backlog". |

---

## Rejected (this wave)

- **`advise_vote` shrinking to single-vendor when budget exceeded one side** — kept proportional fallback (skip exceeded vendor, run others) for usefulness; explicit `vendors: ["claude"]` path is the supported single-vendor form.
- **`advise_parallel` synthesizer** — by design no synthesis (would duplicate `advise_vote`); raw per-vendor output is the value prop.
- **F155 yaml-render test for `executor: codex` injection** — deferred (no critic-flavored template preset exists in `workflow_templates/`; injection path already covered by `e2e_creator_full_path_test.rs`). Documented in PR #104 body. Future critic-template addition would also need this test.
- **F156 daemon-routed Codex critic with unified cost accounting** — V0.7+ (explicit defer per "verified-or-defer" PRD language).

---

## Risks

| ID | Risk | Mitigation |
|---|---|---|
| R6 | **F152 cost-budget.json schema is V0.6.5-introduced** — future cost-tracking unification (e.g. join claude `state.json` cost SoT, per V0.4.6 F91) needs migration. | Pre-v1.0 CLAUDE.md §五: no migration needed; data-clear approach. File path documented in `interfaces.md`. |
| R7 | **F152 flat ~$0.005/call estimate for vendors without usage block** is rough — real spending unknown until vendor returns usage in API. | Budget ledger sums conservatively; under-budget if usage block missing. Real number lands when vendors expose usage (Claude `-p` text mode currently doesn't). |
| R8 | **F156 partial defer (daemon-routed critic)** — Wave 4 ship gate doesn't include this; users wanting unified cost across critic + main spawn won't see it in V0.6.5. | Explicit in SKILL.md "deferred past V0.6.5"; tracked in V0.7 backlog. Bash spawn path **is** shipped — N≥3 critic injection works, just doesn't share cost rollup. |
| R9 | **F155 doctor flag exit codes (0/2/3)** — `ccteam-creator` Phase 3.5 must distinguish them; if skill reads non-zero as "absent" only, the malformed-output case silently shows wrong banner. | SKILL.md Phase 3.5 inspects exact exit code per spec. Smoke covered by 6 e2e tests including the malformed fixture path. |

---

## Files (changed across PRs #104–#106)

**Code:**
- `crates/ccteam-core/src/advise.rs` (new, ~1k LOC — F152, also exports F153 fn)
- `crates/ccteam-cli/src/mcp_advise_tools.rs` (new MCP dispatch — F152 + F153)
- `crates/ccteam-cli/src/mcp_serve.rs` (tool list + dispatch wiring — F152/F153)
- `crates/ccteam-cli/src/{main,commands}.rs` (F155 doctor flag)
- `crates/ccteam-cli/src/cmd_doctor.rs` (F155 `run_check_codex_auto_critic`)

**Tests (new):**
- `crates/ccteam-cli/tests/mcp_advise_vote_test.rs` (F152, +5 hermetic e2e)
- `crates/ccteam-cli/tests/mcp_advise_parallel_test.rs` (F153, +5 hermetic e2e)
- `crates/ccteam-cli/tests/doctor_codex_auto_critic_test.rs` (F155, +6 e2e)
- `crates/ccteam-cli/tests/team_3reviewer_codex_critic_test.rs` (F156, +3)
- (Plus advise.rs internal unit tests — +15)

**Docs/Skills:**
- `skills/ccteam-advise/SKILL.md` (F154 rewrite)
- `skills/ccteam-creator/SKILL.md` (F155 Phase 3.5 updated)
- `skills/ccteam-team/SKILL.md` (F156 verify+defer)
- `docs/interfaces.md` (F152/F153 advise_* MCP rows; F155 doctor flag)

---

## Remaining (organically discovered)

- **rustfmt sweep on drift files in F152 worktree** ── subagent ran a fmt that touched ~6 unrelated drift files; caught and reverted via `git checkout` before commit. **Indicates the rustfmt-direct workflow is still slipping when subagents pass globs**. Wave 3 briefings tightened: "手工传完整文件路径,禁通配符" — escalate W3 if it slips again.
- **F162 will pin intent classifier accuracy ≥ 0.90** — Wave 3 W3-T4 running. If accuracy < 0.90, surfaces a follow-up finding ("classifier needs more training data" or "intent set itself needs split") that requires escalation.

**Strict no-leftover audit:** every promised Wave 2 finding (F152 / F153 / F154 / F155 / F156) shipped in this wave. F156's daemon-routed Codex critic variant is **explicitly deferred** under the "verify-or-defer" PRD language (not a leftover — a documented PRD outcome). Zero items pushed to V0.6.6.

---

## Next: Wave 3 status

Wave 3 (Epic G — UX cohesion + F113 verification, 4 worktrees → target 1540/1, **already exceeded** at 1576/1) dispatched all-parallel:
- W3-T1 `scan-quick` (F157)
- W3-T2 `decision-tree` (F158)
- W3-T3 `dispatcher-hide` (F159 + F161)
- W3-T4 `intent-corpus` (F162 — accuracy ≥ 0.90 ship gate)

After Wave 3 ship → Wave 4 (doc-syncer + nas-box005 host-probe + tag v0.6.5).
