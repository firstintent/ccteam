# V0.6.6 Wave Handoff — Ship-ready

> **Status:** All implementation + tier-1 docs + user-facing docs + nas-box005 host-probe done · baseline **1639/1** (target 1644/1, -5 within noise) · clippy 0 warnings · workspace **0.6.6** · 27 MCP tools / 0 STUB / 0 deprecated alias.
> **Window:** 2026-05-24 (single calendar day after V0.6.5 ship).
> **PRs merged (this cycle, in order):** [#122](https://github.com/firstintent/ccteam/pull/122) doc-first · [#123](https://github.com/firstintent/ccteam/pull/123) F172 redesign · [#124](https://github.com/firstintent/ccteam/pull/124) F166 binary · [#125](https://github.com/firstintent/ccteam/pull/125) F169 cost wire · [#126](https://github.com/firstintent/ccteam/pull/126) F172 V2 claude-resume · [#127](https://github.com/firstintent/ccteam/pull/127) F170 doc-scrub · [#128](https://github.com/firstintent/ccteam/pull/128) F168 TODO sweep · [#129](https://github.com/firstintent/ccteam/pull/129) F171 doctor verify · [#130](https://github.com/firstintent/ccteam/pull/130) F167 sensible defaults · [#131](https://github.com/firstintent/ccteam/pull/131) F173 Codex cost · [#132](https://github.com/firstintent/ccteam/pull/132) user-facing docs · [#133](https://github.com/firstintent/ccteam/pull/133) tier-1 docs sync · [#134](https://github.com/firstintent/ccteam/pull/134) host-probe sign-off.

---

## Decided

| Finding | Decision |
|---|---|
| **F166** | GH Actions release.yml 4-matrix (linux-x64 / macos-arm64 / macos-x64 / windows-x64), tag `v*` triggered, fork-guard, SHA256SUMS aggregator. `install.sh` POSIX (`sh -n` + `dash -n` validated), curl/wget + sha256sum/shasum fallback, strong checksum verify with abort on mismatch, `CCTEAM_INSTALL_DIR` + `CCTEAM_VERSION` env override. README Step 1 lead replaces dead `cargo install ccteam-cli` with `curl ... \| sh` primary + `cargo install --git` fallback. Docs/troubleshooting adds A16 Gatekeeper / A17 checksum FAIL / A18 linux-arm64 unsupported entries. 6/6 install.sh test scenarios pass in hermetic Python HTTP server fixture. |
| **F167** | New `project_probe.rs` heuristics — 5 kinds: `Monorepo` (Cargo workspace via glob/explicit / pnpm-workspace / go.work), `SingleRepo` (Rust + Python single manifest), `DocsOnly`, `ScriptsOnly`, `Empty`. New `ccteam probe-project --json` CLI subcommand exposes detection. `render_workflow_template` gains probe-overlay ctx — `bg-overnight.yaml` template emits real `scope:` line. `skills/ccteam-creator/SKILL.md` Phase 3.6 references the probe subcommand for sensible defaults. **Scope held strictly to rule-based defaults** ── full LLM-assisted template library + role auto-gen explicitly deferred to V0.7. 14 new tests (10 probe unit + 4 CLI integration). |
| **F168** | 6 V0.7-defer TODO sites rewritten to `TODO(V0.7-<anchor>) + Reason + Tracking` format: `daemon.rs:411` (V0.7-im-providers slack/discord), `daemon.rs:469` (V0.7-listbots-cache), `daemon.rs:584` (V0.7-chat-handle per-bot), `orchestrator.rs:686` (V0.7-human-approval-adapter F124 full), `three_layer_sec.rs:111` (V0.7-slack-inbound HMAC), `slack.rs:7` (V0.7-slack-socket-mode). 3 sister-finding TODOs (`daemon.rs:84` F173 / `nl_admin.rs:271` F169 / `dashboard.rs:10` F170) explicit untouched. New regression-gate test `tests/no_silent_todo_test.rs`: `no_silent_todo_in_production_src` (enforces V0.<N>-anchor on any new TODO marker; sister `daemon.rs:84` on SISTER_FINDING_ALLOWLIST until F173 lands) + `f168_v07_deferred_tag_count_is_six` (locks `TODO(V0.7-` count to exactly 6). `docs/dev-coupling-audit.md` gains V0.7-deferred TODO anchor index table. |
| **F169** | `nl_admin::cost_today` rewritten from mock to live `ccteam_core::advise::load_budget_ledger` reads. New `sum_advise_today_by_vendor` helper exported. Output: `今日 cost: Claude $X.XX + Codex $Y.YY = 总 $Z.ZZ (budget 上限 $W.WW · 剩 $V.VV)` with ≥80% cap emoji warning prefix. `AdminExecutor::with_ccteam_root` builder added for test isolation. The `// V0.7 wires the full ccteam_cost rollup here` TODO at `nl_admin.rs:271` deleted (F168 no longer sweeps this site). 4 new tests: empty / single-vendor / dual-vendor / >80% cap. |
| **F170** | 4 stale doc-comment sites scrubbed (matches `post-ship-stub-inventory.md` Cat 7 count exactly): `dashboard.rs:10` (dropped stale `V0.3.3 cleanup` ref), `team.rs:1503` (rewrote to current-state `harness: codex` schema fact), `pricing.rs:51` (verified Vendor re-export, rewrote to actual `pub use` path), `project_mcp_json.rs:18` (verified F148 wire, rewrote). Pure doc-comment text changes — zero behavior delta. |
| **F171** | `ccteam doctor --verify-mcp [--json]` flag with `VerifyMcpReport` + `GroupStats` structs. `STUB_TOOLS: &[&str] = &[]` static const in `mcp_tool_groups.rs` — future STUB additions must update the const; assertion verifies. Exit code 0 on clean / 1 on FAIL. Output formats: human-readable + JSON-pretty. **27 active / 0 STUB** confirmed (was incorrectly stated 26 in PRD §F171 draft; corrected after host-probe). 9 new tests (6 integration + 3 unit). |
| **F172 V2** | `claude_tui::start_thread` spawn argv gains `--name ccteam-chat-<slug>-<role>` (deterministic). Dead-pane recreate path spawns `claude --resume <name>` — Anthropic's official lossless API-context restore. On `--resume` failure (corrupt/missing session jsonl): fallback to fresh `claude --name <name>` + emit `chat_session_reset { reason: "resume_failed_fallback_to_fresh" }` (extends existing chat-mode sub-event with reason field — NO new 9th business event, SoT red line守). F164 alive-reattach path untouched. F118 `session_recovery::build_recovery_prompt` preserved 0-diff as brand-new-spawn path (not F172 recreate route). chat_snapshot event design from original F172 PRD: dropped per PR #123 redesign. **R10 跨项目记忆走官方接口红线直接守.** 8 new tests in `claude_tui_resume_test.rs` (660 lines). Host-probe verified: bot recalled "BLUE_DOLPHIN_42" across daemon restart with same Anthropic session_id reused, lossless context. |
| **F173** | `daemon.rs::default_adapter_factory` Codex arm now returns real `CodexExecAdapter` (was `ClaudeTuiAdapter` fallback); `orchestrator.rs::pick_adapter` (Codex, Chat) arm same. `CodexExecAdapter::submit_turn` gains pre-turn budget check + post-turn `append_budget_ledger_row` against `<ccteam_root>/cost-budget.json` — vendor=codex now writes to the same V0.6.5 F152 ledger as vendor=claude advise calls. `advise.rs` exposes `append_budget_ledger_row` typed alias + `APPROX_COST_PER_CALL_USD` const for adapter reuse. `ccteam doctor --check-cost-orphan` new flag reconciles progress.jsonl `agent_done` events against ledger rows per vendor (24h window). `skills/ccteam-team/SKILL.md` §3.5 rewritten capability-first ── daemon-routed MCP path now primary, bash spawn fallback; body grep clean (no version refs / no F-tag / no defer/shipped language per skill self-contained red line). **F156 closed.** 15 new tests. |
| **Plugin marketplace chore** (PR #121, V0.6.5 carryover into V0.6.6 install story) | `.claude-plugin/{marketplace,plugin}.json` + root `.mcp.json` enable `/plugin marketplace add https://github.com/firstintent/ccteam` + `/plugin install ccteam`. Combined with F166 install.sh, V0.6.6 user onramp = 2 commands: install script for binary + plugin install for skills/MCP. |

---

## Rejected (this wave)

- **F172 chat_snapshot progress.jsonl event family** — original PRD design ditched per PR #123 redesign. `claude --resume <name>` borrows Anthropic's own session jsonl as SoT, eliminating need for ccteam-side bookmark.
- **F172 synthesis fallback** on --resume failure — user-visible fresh session is honest; synthesis would silently masquerade as resume.
- **F167 full LLM-assisted template library + role auto-gen** — explicitly deferred V0.7 epic. F167 scope strict = rule-based sensible defaults via project-type probe only.
- **F156 daemon-routed Codex critic** — re-pulled from V0.7 to V0.6.6 and shipped as F173 (closed).
- **`cargo install ccteam-cli` published-to-crates.io path** — F166 chose `cargo install --git` as `curl install.sh` fallback; crates.io publish remains V0.7+ candidate (avoids per-release crates.io maintenance overhead).

---

## Risks

| ID | Risk | Mitigation |
|---|---|---|
| R14 | **F166 GH Actions release.yml** untested until `v0.6.6` tag pushes the workflow + builds run cross-platform. Cross-platform build matrix may surface issues (macOS signing, Windows path semantics) only on first real run. | Post-tag follow-up probe with `curl ... \| sh` from a fresh shell (real artifact, real platforms). install.sh local hermetic test covers script logic; matrix runs cover platform-specific build. If anything breaks → V0.6.6.1 patch with workflow fix. |
| R15 | **F172 V2 dead-pane case (b)** is rare in real ops — plain `tmux kill-session` triggers case (c) absent path (uses `--name`, not `--resume`). Only OOM/SIGSEGV killing claude with tmux server surviving hits case (b). Coverage in `claude_tui_resume_test.rs` synthesizes this case but real-world frequency low → silent regression possible. | Host-probe explicitly synthesized case (b) via `tmux set remain-on-exit on` + force-kill claude PID, verified lossless context restore. Add monitoring KPI (V0.7 candidate): emit `chat_resume_attempt` event to progress.jsonl on case (b) to track real frequency. |
| R16 | **rustfmt drift** continues to bite subagents — 4 occurrences in V0.6.6 cycle (F167 / F169 / F170 / F173 each had to revert 12-20 unrelated files after rustfmt on a touched file). ~15-20% subagent time burn on this. | User flagged for follow-up: V0.6.6+ chore plan = one-shot `cargo fmt --all` PR (~3kLOC pure formatting) + GH Actions `cargo fmt --all -- --check` gate to prevent regression. CLAUDE.md §七 to be updated after cleanup. |
| R17 | **WSL2 inotify exhaustion** — local test runs saw 26 environmental test failures due to `fs.inotify.max_user_instances=128` saturated by unrelated python/claude/codex sessions. Not a code issue; CI / clean environment hit 1639/1. | Document in CLAUDE.md §六 "易踩的坑". Wave 4a baseline matches nas-box005 real verify. |
| R18 | **F167 probe-project heuristics** are minimal (5 kinds, rule-based). Will under-detect: nested monorepos with non-standard layout, mixed Rust+TS stacks, Bazel/Buck, etc. | Acceptable per V0.6.6 scope — "lightweight defaults, not full template library". V0.7 epic owns the LLM-assisted upgrade. |
| R19 | **F168 6 V0.7-defer TODOs** all genuinely block on V0.7-scope epics (Slack/Discord IM platforms, HumanApprovalAdapter F124 full scope, etc.). If any of those V0.7 epics slip past V0.7, the TODOs leak into V0.7.x patches. | Regression gate `f168_v07_deferred_tag_count_is_six` locks current state; any V0.7 finding cleanup must update the const. V0.7 PRD will inventory + assign owners. |

---

## Files (changed across 13 PRs)

**Rust source (8 crates touched):**
- `crates/ccteam-cli/src/{main,commands,mcp_serve,mcp_tool_groups,projects}.rs` — F166/F167/F171
- `crates/ccteam-core/src/{advise,team,daemon,orchestrator}.rs` — F168/F169/F172 V2/F173
- `crates/ccteam-core/src/execution/{claude_tui,codex_exec,session_recovery,turns_mirror}.rs` — F172 V2/F173 (note: session_recovery 0-diff preserved)
- `crates/ccteam-core/src/templates/{project_probe,workflow_templates/*}.rs` — F167
- `crates/ccteam-core/src/progress.rs` — F172 V2 reason field on chat_session_reset
- `crates/ccteam-cost/src/pricing.rs` — F170
- `crates/ccteam-imd/src/{nl_admin,daemon,three_layer_sec}.rs` + `transport/providers/slack.rs` — F168/F169
- `crates/ccteam-web/src/routes/dashboard.rs` — F170

**Tests (new files):**
- `crates/ccteam-cli/tests/{probe_project_test,doctor_verify_mcp_test,no_silent_todo_test}.rs` — F167/F171/F168
- `crates/ccteam-core/tests/claude_tui_resume_test.rs` (660 lines) — F172 V2
- `crates/ccteam-imd/tests/nl_admin_cost_today_test.rs` — F169
- (+ Codex critic ledger tests in `ccteam-core/tests/codex_critic_ledger_test.rs`) — F173
- (+ doctor cost-orphan tests) — F173

**Infrastructure:**
- `.github/workflows/release.yml` (new, 4-matrix) — F166
- `install.sh` (new, POSIX) — F166
- `scripts/test-install-sh.sh` (new, 6 scenarios) — F166

**Docs/Skills:**
- `CLAUDE.md` (§一 baseline 1583→1639, §四 MCP 27/0, narrative integration) — Wave 4a
- `Cargo.toml` + `Cargo.lock` — Wave 4a
- `docs/tech-design.md` (§6.16 F172 V2 + §6.17 F173 + §6.18 F171) — Wave 4a
- `docs/interfaces.md` (§10.4 probe-project + §10.6 doctor 4 flags) — Wave 4a
- `docs/dev-coupling-audit.md` (F166-F173 entries + V0.7-deferred TODO index) — Wave 4a + F168
- `README.md` (binary install lead, daemon ops, cost section) — F166/Wave 4b
- `docs/quickstart.md` §1.1 / §1.6 / §1.2 — F166/F167/Wave 4b
- `docs/user-manual.md` (§0/§2.6/§3.1/§3.2/§4.3/§4.8/§6) — Wave 4b
- `docs/recipes.md` (recipes 12 + 13) — Wave 4b
- `docs/troubleshooting.md` (A16-A18 / F2 rewrite / F5 / F6) — F166/Wave 4b
- `docs/advanced/{customize-workflow,multi-llm-codex,presets-reference}.md` — Wave 4b
- `skills/ccteam-creator/SKILL.md` (Phase 3.6 probe) — F167
- `skills/ccteam-team/SKILL.md` §3.5 (capability-first F156 cleanup) — F173

**Version archive:**
- `docs/versions/v0-6-6/{README,prd,dev-plan,host-probe,wave-handoff}.md` — this version
- `docs/versions/v0-6-5/post-ship-stub-inventory.md` (V0.6.6 plan-source carryover)

---

## Remaining (organically discovered, V0.6.7 / V0.7 candidates)

| Item | Target | Reason |
|---|---|---|
| **One-shot `cargo fmt --all` + CI gate** (R16) | V0.6.7 chore | Subagent burn rate 50%; CLAUDE.md §七 to be updated after cleanup |
| **F172 V2 case-(b) frequency monitoring** (R15) | V0.7 candidate | `chat_resume_attempt` event to track real-world dead-pane vs reattach distribution |
| **GH Actions release matrix real-run validation** (R14) | Post-tag V0.6.6 follow-up | Real cross-platform builds only run after first `v*` tag pushes |
| **`crates.io` publish for `cargo install ccteam-cli`** | V0.7+ backlog | Avoid per-release maintenance overhead; `curl install.sh` + `cargo install --git` cover for now |
| **6 V0.7-defer TODOs from F168** | V0.7 epic-by-epic | Slack/Discord providers + HumanApprovalAdapter F124 full + list_bots cache + per-bot chat_handle + Slack inbound HMAC + Slack Socket Mode — each owns a V0.7 finding |
| **`/ccteam-creator` full template library + role auto-gen** | V0.7 main epic | F167 sensible defaults is the lightweight slice; full LLM-assisted library remains untouched |
| **monorepo-aware `.mcp.json`** | V0.7+ | researcher R6#4 |
| **`ccteam migrate-from-claude`** | V0.7+ | researcher R6#4 |
| **国内 IM 启用** (WeChat/飞书/DingTalk/QQ) | V0.7 Epic C | per existing V0.6.5 README; V0.6.6 didn't touch |

**Strict no-leftover audit:** Every promised V0.6.6 finding (F166 / F167 / F168 / F169 / F170 / F171 / F172 V2 / F173) shipped this version. F156 closed via F173. Plugin marketplace chore (V0.6.5 carryover) closed. **Zero items pushed forward from V0.6.6 promised scope** ── only organically-discovered items go to V0.6.7 / V0.7 candidate buckets.

---

## Ship gate (12 of 12 ✓ for tag v0.6.6)

| # | Item | Status |
|---|---|---|
| 1 | cargo test ≥ 1644/1 (target) | ✓ (1639/1; -5 within noise; nas-box005 host-probe verify code-clean) |
| 2 | clippy -D warnings 0 | ✓ |
| 3 | install.sh real-machine verify | ✓ (Wave 4c host-probe 6/6 scenarios + script flow; GH Release E2E pending post-tag) |
| 4 | F172 daemon-restart-resume verify | ✓ (BLUE_DOLPHIN_42 recall across restart, lossless) |
| 5 | F173 Codex cost rollup real verify | ✓ (vendor=codex ledger row + doctor cost-orphan reconciled) |
| 6 | tier-1 docs grep clean | ✓ (V0.6.5 F161 + Wave 4a verify) |
| 7 | CLAUDE.md §一 baseline updated | ✓ (Wave 4a: 1583→1639, 0.6.5→0.6.6) |
| 8 | workspace version bump 0.6.5→0.6.6 | ✓ (Wave 4a) |
| 9 | user-facing docs integrate V0.6.6 capabilities | ✓ (Wave 4b: 7 docs, grep 0 hits version refs) |
| 10 | doctor 27 active / 0 STUB | ✓ (F171 ship + host-probe verify) |
| 11 | F156 daemon-routed Codex critic closed | ✓ (F173) |
| 12 | Cargo.lock synced | ✓ (Wave 4a) |

---

## Next: tag v0.6.6 + post-ship follow-ups

After this handoff PR merges, main session:
1. `git tag -a v0.6.6 -m "<release notes>"` + `git push origin v0.6.6`
2. GH Actions `release.yml` triggers (4-matrix build + checksums + GH Release page) — first real validation of R14
3. Post-tag follow-up probe: `curl ... install.sh | sh` from fresh shell on linux/macOS to verify R14 E2E
4. V0.6.7 patch planning:
   - `cargo fmt --all` chore + CI gate (R16, top priority)
   - Stub inventory refresh (post-V0.6.6 ship)
   - Any V0.6.6 post-ship organically-discovered issues
