# V0.6.5 Wave 3 — Handoff

> **Status:** Wave 3 ship-complete · 4 / 4 worktrees · 6 F-tags merged · baseline **1579 / 1** (target 1540 / 1, +39 over) · clippy 0 warnings.
> **Window:** 2026-05-24 (single calendar day, 4 parallel Opus worktrees).
> **PRs merged:** [#108](https://github.com/firstintent/ccteam/pull/108) F158 · [#109](https://github.com/firstintent/ccteam/pull/109) F162 · [#110](https://github.com/firstintent/ccteam/pull/110) F157 · [#111](https://github.com/firstintent/ccteam/pull/111) F159+F161.

---

## Decided

| Finding | Decision |
|---|---|
| **F157** | `ccteam-scan` skill gains `--quick` mode: 1 Sonnet (fallback Haiku) agent + 3 fixed questions (Q1 主语言/framework/entry / Q2 TODO-FIXME hotspots / Q3 CLAUDE.md/README/AGENTS.md state + 是否建议 `claude /init`). Output `<repo>/.ccteam/codebase-scan.md` with frontmatter `quick: true`. Briefing is **inline in SKILL.md** (~35 lines, copy-paste prompt for the user's Task tool call — no external fixture). Env check (skill body): non-git repo errors out; <24h old report shown directly + suggest `--force`. `/ccteam` SKILL.md now 8 intents (7 work + 1 fallback): start-team / create-workflow / configure-im / monitor / **code-scan** / advise / status-debug / other. Default route for `code-scan` → `--quick`; "large codebase / monorepo / scope / navigability / full audit" → audit mode. Disambiguation heuristic added (code-scan + start-team → verb read vs write; monitor + status-debug existing rule kept). Wall-clock ≤90 s host-probe deferred to Wave 4. |
| **F158** | New `docs/task-to-command.md` (Chinese, 162 lines): task-to-command decision tree, 9 rows covering 7 sub-skills. `README.md` rewritten English (87% diff) — decision-tree lead replaces the 5-preset table; 6 sections kept (intro / decision tree / get started / 3 talk modes / docs / license & ack). 63 lines (down from 81). `docs/quickstart.md` §1 new decision-tree lead, §2+ flagship walkthrough preserved. `docs/user-manual.md` new §0 quick-jump table. `docs/orchestration-patterns.md` gains `audience: contributors` frontmatter (this is a tech-design doc, not a user-facing decision tree). README is English (0 Chinese chars verified) and contains no version number / baseline / shipping date / "Status" section (CLAUDE.md §三 red line). |
| **F159** | `skills/ccteam/SKILL.md` gains explicit red-line section "未实现 intent 直接隐藏不渲染" with 3-condition ship gate (sub-skill body real + MCP dispatch real + host-probe verified). New regression-guard test file `crates/ccteam-core/tests/dispatcher_hide_unimpl_test.rs` (3 tests): verifies SKILL.md carries no stale Wave/STUB/NotImplemented phrasing, declares the red line, and does NOT route any V0.7+ deferred intent (`voice-input` / `image-input` / `multimodal`). |
| **F161** | Cross-docs grep sweep: 6 stale "Wave [123] / STUB / NotImplemented / 占位 / 准备中 / 待落地" hits cleared from `skills/ccteam-team/SKILL.md` (3), `skills/ccteam-creator/SKILL.md` (3), `skills/ccteam-im-setup/SKILL.md` (1), `docs/advanced/multi-llm-codex.md` (3). Final verification: `grep -rnE "Wave [123]\|STUB\|NotImplemented\|占位\|准备中\|待落地" docs/{quickstart,user-manual,recipes,troubleshooting}.md docs/advanced/*.md skills/*/SKILL.md README.md` exit=1 (0 hits). |
| **F162** | New `tests/intent-corpus.yaml` (50 entries: start-team 8 + 其他 6 intent 各 7; zh 30 / en 20). New `scripts/host-probe/intent-accuracy.sh` — default `mock` mode (static keyword scan, free + fast), `--real` flag for live `claude --print` Sonnet pass. First snapshot in `docs/versions/v0-6-5/intent-accuracy.md`: **accuracy 0.98 (49/50)** ── ship gate ≥0.90 passes by +8 points. Only miss: `"stop the overnight-builder workflow"` mock-routes to `create-workflow` (mock hits "overnight" before "stop" action verb) — real-LLM mode resolves via SKILL.md heuristic "具体 slug 提及 → status-debug". |

---

## Rejected (this wave)

- **`ccteam-scan --quick` writing under `<repo>/.ccteam-scan/`** — kept the existing `.ccteam/codebase-scan.md` location for ergonomics (one folder for all ccteam-owned project state).
- **F158 single-language docs** — kept bilingual (English README + Chinese docs/task-to-command.md) per CLAUDE.md README-English / docs-Chinese red line.
- **F158 README "Status" section restoration** — removed entirely per CLAUDE.md §三 "README.md 不含版本进展/状态信息" red line. Release notes / version progression live in `docs/versions/v0-X-Y/` only.
- **F159 grey-out unimpl intents instead of hiding** — explicit hide (no placeholder) is the red-line; grey-out would leak future-facing intents (voice-input etc.) into UX surface.
- **F162 real-LLM mode in the first ship snapshot** — kept mock mode for cost-free reproducibility; real mode reserved for Wave 4 host-probe (or any subsequent verification run).

---

## Risks

| ID | Risk | Mitigation |
|---|---|---|
| R10 | **F162 0.98 accuracy is mock-mode** (static keyword scan, not real classifier). Real-LLM (`--real` Sonnet) might give a different number — possibly lower if Sonnet over-thinks short queries, or higher if it disambiguates better than static rules. | Ship gate satisfied by mock number per PRD F162 spec. Wave 4 host-probe should run `--real` once and append result to `intent-accuracy.md`. If real-mode delta is >5 % either direction, flag follow-up finding. |
| R11 | **F157 wall-clock ≤90 s deferred to Wave 4** — not yet measured on actual `references/claude-code/` or similar real repo. Sonnet + 3 questions could exceed 90 s on big repos. | Wave 4 host-probe runs scan --quick on nas-box005 with a real repo and clocks wall time. If >90 s, F157 ships but flag risk in V0.7 ("scan --quick needs further tightening"). |
| R12 | **F158 README rewrite is 87 % diff** — significant surface area for taste / English wording disagreement. Future contributor reading old `git blame` may be confused by the discontinuity. | Decision-tree lead matches user-tested user-facing entry chain (`/ccteam` first line). Disagreements surface as small follow-up PRs against `README.md` directly. |
| R13 | **F159 hide-unimpl regression test** (`dispatcher_hide_unimpl_test.rs`) hard-codes V0.7+ deferred intent names. Adding a new V0.7 intent requires updating both the SKILL.md routing logic + this test. | Documented in test file header. Tradeoff: false negative (test passes even though SKILL.md leaked) costs more than the maintenance burden. |

---

## Files (changed across PRs #108–#111)

**Code:**
- `crates/ccteam-core/tests/dispatcher_hide_unimpl_test.rs` (new, F159, +3 tests)

**Skills / Docs:**
- `skills/ccteam-scan/SKILL.md` (F157, +123/-3)
- `skills/ccteam/SKILL.md` (F157 intent table + F159 red line)
- `skills/ccteam-team/SKILL.md` (F161, 3 hits cleared)
- `skills/ccteam-creator/SKILL.md` (F161, 3 hits cleared)
- `skills/ccteam-im-setup/SKILL.md` (F161, 1 hit cleared)
- `skills/ccteam-advise/SKILL.md` (not changed this wave — was F154 in Wave 2)
- `README.md` (F158, 87% rewrite, English decision tree lead)
- `docs/task-to-command.md` (F158, **new**, 162 lines, Chinese decision tree)
- `docs/quickstart.md` (F158, §1 rewrite, §2+ preserved)
- `docs/user-manual.md` (F158, §0 quick-jump table)
- `docs/orchestration-patterns.md` (F158, audience frontmatter)
- `docs/advanced/multi-llm-codex.md` (F161, 3 hits cleared)

**Test infrastructure:**
- `tests/intent-corpus.yaml` (F162, **new**, 50 queries)
- `scripts/host-probe/intent-accuracy.sh` (F162, **new**, executable, mock + --real modes)
- `docs/versions/v0-6-5/intent-accuracy.md` (F162, **new**, first 0.98 snapshot)

---

## Remaining (organically discovered)

- **F162 real-LLM run** ── add a `--real` line to `intent-accuracy.md` after Wave 4 host-probe; not a new finding, just a verification confirmation.
- **F157 wall-clock probe** ── Wave 4 host-probe responsibility (per dev-plan).

**Strict no-leftover audit:** every Wave 3 finding (F157 / F158 / F159 / F161 / F162) shipped in this wave. Zero items pushed to V0.6.6 / V0.7.

---

## Next: Wave 4

Wave 4 splits into 4a + 4b (per dev-kickoff §"真机 host-probe"):
- **Wave 4a — doc-syncer + version bump** (Opus subagent, local worktree, in flight): CLAUDE.md §一 baseline backfill (workspace `0.6.5`, baseline `1579/1`), tier-1 docs sync (tech-design / interfaces / dev-coupling-audit / claude-code-tool-surface), workspace version bump 0.6.4 → 0.6.5, ship PR with 11-item gate list (5 items left ▢ for 4b sign-off).
- **Wave 4b — nas-box005 host-probe** (Opus subagent, requires SSH access + destructive `~/.ccteam/ + project .ccteam/ + ~/.claude/projects/` wipe for F148 fresh probe): F148 `/ccteam-creator` → TG round-trip · F157 scan --quick ≤90 s · F162 intent-accuracy `--real` confirmation · F163 SIGTERM 5 s graceful · F164 tmux reattach across daemon restart. Sign-off in `docs/versions/v0-6-5/host-probe.md`.
- **Wave 4c (main session)**: final ship gate verification + tag v0.6.5 after 4a + 4b merge.

Main session **escalating Wave 4b scope to user before dispatch** (destructive wipe on user's production NAS box).
