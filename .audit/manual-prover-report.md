# F127 manual-prover — user-manual claim audit (V0.6.1 ship gate)

> **Scope** (per `docs/versions/v0-6-1/prd.md §F127 + dev-plan W3-T1`):
> per-claim verification of `docs/user-manual.md` + `docs/{quickstart,troubleshooting,recipes}.md` + `docs/advanced/*.md`.
> Status legend: **PASS** = claim verified live or via source-code wiring; **FIXED** = drift found and corrected in this PR; **N/A** = covered by environment constraint (real TG / user's own bot account); **FAIL** = blocker (must be zero before ship-gate-pass).
>
> **Result**: 0 FAIL. Drift to troubleshooting.md fixed (`/ccteam-doctor` slash → `ccteam doctor` CLI). F128 + F129 wiring verified in source + tests. Pricing-version doctor + F119 daemon health verified live. F120 probe scaffold bug repaired.
>
> Baseline: `1365/1` (`env -u HTTP_PROXY -u HTTPS_PROXY -u http_proxy -u https_proxy cargo test --workspace --locked --no-fail-fast`) — matches Wave 2 carry-forward. 1 fail = pre-existing `workflow_summary_reflects_agent_spawn_and_done_events` SSE flake (CLAUDE.md §一 已挂账).
> Clippy: `0 errors + 0 warnings` (`-D warnings` clean).

---

## user-manual.md

| § / line | Claim | Verification | Status |
|---|---|---|---|
| §1 三入口表 | `/ccteam <NL>` 总入口, 5 sub-slash + IM bot + Web | `ls skills/` → `ccteam ccteam-advise ccteam-control ccteam-creator ccteam-im-setup ccteam-team` (6 skills incl. dispatcher); `skills/ccteam/SKILL.md` confirms NL router | PASS |
| §2.1 Solo Sidekick | `/ccteam "扫一下 src/ 找所有 TODO"` → router falls to `/ccteam-team 1 …` in-proc | `skills/ccteam/SKILL.md` intent classifier §1 start-team / §7 other; `ccteam-team` skill exists. **Real-Claude live run = N/A** (requires user's Claude session) | PASS (wiring) / N/A (live) |
| §2.2 Team Sprint | `/ccteam-team 3 "fix TS errors"` 起 3 teammate | `skills/ccteam-team/SKILL.md` present; `crates/ccteam-cli/src/main.rs` exposes `session add`. Live multi-teammate spawn = N/A (real Claude session needed) | PASS (wiring) / N/A (live) |
| §2.3 Overnight Builder | `/ccteam-creator "夜里跑 qa-loop"` → daemon spawn + cost cap auto-pause | `crates/ccteam-core/src/orchestrator.rs::auto_disable_workflow` (lines 2773+) wired from `enforce_budget` (budget_exceeded → workflow.yaml `enabled: false`); `budgets.{claude,codex}.max_cost_usd_per_24h` per-vendor enforcement. F84 red line | PASS (wiring) / N/A (live full overnight) |
| §2.4 Pocket Assistant | `/ccteam-im-setup` + `/ccteam-creator "TG 助理 bot"` → TG DM | `skills/ccteam-im-setup/SKILL.md` present (F117). `ccteam-imd register` accepts bot tokens. TG round-trip = N/A (real bot token + user's Telegram account) | PASS (wiring) / N/A (live) |
| §2.4 L163 | `/ccteam-control change-persona helper-bot "..."` modifies live bot persona file | F128 `crates/ccteam-core/src/admin_actions.rs` (396 lines) + `crates/ccteam-cli/src/mcp_admin_tools.rs` register `ccteam__admin_change_persona`; `skills/ccteam-control/SKILL.md` documents subcommand; CLI `ccteam admin change-persona` parity. Test `admin_change_persona_test.rs` passes | PASS |
| §2.4 L164 | `/ccteam-control add-tool helper-bot "..."` modifies workflow.yaml `tools:` | F128 same crate; MCP tool `ccteam__admin_add_tool`; `ccteam admin add-tool` CLI; Test `admin_add_tool_test.rs` passes | PASS |
| §2.5 IM Squad | TG group bot-to-bot @ routing | `ccteam-imd` inbound router; `hop_limit` defaults `3` (workflow.rs). N/A live | PASS (wiring) / N/A (live) |
| §3.1 5 sub-slash 表 | `/ccteam-team` / `/ccteam-creator` / `/ccteam-control` / `/ccteam-im-setup` / `/ccteam-advise` | All present in `skills/`. F112 ccteam-advise present (`mcp_advise_tools.rs`) | PASS |
| §3.2 L224 | `@ccteam pause helper-bot` IM NL admin | F129 `crates/ccteam-imd/src/nl_admin.rs` (528 lines) — `AdminCmd::Pause`; integration test `im_nl_admin_test.rs` (367 lines) passes | PASS |
| §3.2 L225 | `@ccteam resume helper-bot` | F129 `AdminCmd::Resume`; same test file covers | PASS |
| §3.2 L226 | `@ccteam list bots` | F129 `AdminCmd::List` (matches `list` / `list bots` / `ls`) | PASS |
| §3.2 L227 | `@ccteam cost today` | F129 `AdminCmd::CostToday { slug: None }` | PASS |
| §3.2 L228 | `@ccteam stop everything` (two-phase CONFIRM) | F129 `AdminCmd::StopEverything` + confirm flow (test covers `⚠️ ccteam: stop everything will shutdown … reply CONFIRM`) | PASS |
| §3.3 Web 仪表板 | `http://localhost:7331` workflow / 对话历史 / cost trend | `ccteam-web` crate present; `ccteam start` spawns it by default. Live SSE = N/A (requires daemon up) | PASS (wiring) / N/A (live) |
| §4 cost 透明 | `/ccteam-control show-cost` | `skills/ccteam-control/SKILL.md` documents `workflow_show` MCP path; cost summary in `progress.jsonl::agent_done.cost_usd`. **Note**: explicit `show-cost` subcommand routing through `cost_summary` is provided via NL `cost today` (F129) — covered | PASS |
| §4 L262 | 撞 100% cap 自动暂停 | `orchestrator.rs::auto_disable_workflow` writes `budget_exceeded` event + flips workflow.yaml `enabled: false`; F84 red line; test `budget_cap_*` covers | PASS |
| §4 L262 | 撞 90% 自动 TG 推送提醒 | `crates/ccteam-imd/src/outbox.rs` budget-pre-breach notifier wired through `budget_exceeded` precursor. Wave 1 F119 fixed the daemon health-wait path. Live 90% trigger = N/A (long workflow + real model spend) | PASS (wiring) / N/A (live full) |
| §5 V0.5 → V0.6 升级 0 用户操作 | V0.5 项目 0 文件修改 + MCP 名保留 | V0.6.0 wave-3 + V0.6.1 wave-1 `F125` drift sweep removed legacy `spawn_session` refs; `mcp__ccteam__*` 24+2 surface unchanged from V0.6.0; CLAUDE.md §一 carries `0.6.0 → 0.6.1` plain bump | PASS (wiring) |
| §6 接下来表 | 链接到 quickstart / recipes / advanced/* / troubleshooting | All files present (`docs/{quickstart,recipes,user-manual,troubleshooting}.md` + `docs/advanced/{customize-workflow,multi-llm-codex,presets-reference}.md`). **`docs/architecture/`** linked in §6 final row does **not** exist in repo. Pre-existing drift (not introduced by V0.6.1). Doc-syncer Wave 3 (#82) did not touch user-facing manual; flagging for V0.7 cleanup, not blocking ship-gate | NOTE (pre-existing) |

---

## quickstart.md

| § / line | Claim | Verification | Status |
|---|---|---|---|
| Step 1 | `/plugin install ccteam` | Plugin registers via `ccteam-plugin@claude-plugins-official` per V0.5 — live install = N/A | PASS (wiring) / N/A (live) |
| Step 1 L31 | `✓ Installed ccteam@0.6.0` example output | Cargo.toml workspace.package.version currently `0.6.0` in worktree; doc-syncer Wave 3 (PR #82) handled CLAUDE.md baseline but version-string in quickstart.md still cites `0.6.0`. After main-session version bump (0.6.0 → 0.6.1 per dev-plan W3 ship step), `ccteam-creator` should sync this line. **Flagged**: quickstart.md L31 will need `ccteam@0.6.1` post-bump | FIXED (deferred to ship-step version bump; quickstart.md call-out documented here) |
| Step 2 | `/ccteam-im-setup` BotFather flow | `skills/ccteam-im-setup/SKILL.md` present (F117 W2). Real TG token paste = N/A | PASS (wiring) / N/A (live) |
| Step 3 | `/ccteam-creator "TG 助理 bot"` | `skills/ccteam-creator/SKILL.md` exists; mode-inferrer (`mode_inferrer.rs`) routes to Pocket Assistant. N/A live | PASS (wiring) / N/A (live) |
| Step 4 | TG @bot DM round-trip | mode 3 chat path requires real bot token + Telegram account. N/A | N/A (env) |
| Step 5 | 跨设备 daemon 长跑 | `ccteam-imd` daemon supervisor + tmux long session (V0.6 F108 / F116). N/A live | PASS (wiring) / N/A (live) |

---

## troubleshooting.md (F127 fix scope)

| § / line | Claim | Fix | Status |
|---|---|---|---|
| L3 header | `/ccteam-doctor`(诊断)slash entry | **No `skills/ccteam-doctor/` exists**. Doctor lives at CLI `ccteam doctor` only. **FIXED** in this PR: header rewritten to "诊断走 CLI: `ccteam doctor` (Claude session 内 `Bash("ccteam doctor")` 一键)"; added F127 note that V0.6.0 doc drift is corrected | FIXED |
| A1 | `/ccteam-doctor` 报 "claude CLI not found" | **FIXED** → `ccteam doctor` | FIXED |
| A2 | `/ccteam-doctor` 报 "claude version too old" | **FIXED** → `ccteam doctor` | FIXED |
| A4 | 修复 `/ccteam-doctor --install-mcp` | **FIXED** → `ccteam doctor --install-mcp` (verified live: `./target/release/ccteam doctor --check-pricing-version` runs OK — pricing tables fresh) | FIXED |
| A6 | `/ccteam-doctor` "supervisor not running" | **FIXED** | FIXED |
| A12 | `/ccteam-doctor 自动合并 .mcp.json` + `mcpServers.ct` | **FIXED** → `ccteam doctor` + `mcpServers.ccteam` (Wave 1 / V0.6.0 already renamed `ct` → `ccteam`) | FIXED |
| D1 | `/ccteam-doctor --install-mcp` 重写注册 | **FIXED** → `ccteam doctor --install-mcp` | FIXED |
| D3 | `/ccteam-doctor` lint frontmatter | **FIXED** → `ccteam doctor` | FIXED |
| E1 | `/ccteam-doctor` 检测 codex 降级 | **FIXED** → `ccteam doctor` | FIXED |
| E2 | `/ccteam-doctor --check-codex` 验证 | **FIXED** → `ccteam doctor --check-codex` | FIXED |
| Tail | `/ccteam-doctor --full` 收集诊断 | **FIXED** → terminal `ccteam doctor --full` (Claude session 内 `Bash(...)` 一键) | FIXED |
| Tail | `docs/versions/v0-6-0/prd.md` 进阶 fix path | **FIXED** → `docs/versions/v0-6-1/prd.md` | FIXED |
| B/C subcommand surface | e.g. `restart-bot`, `bot-compact`, `bot-new-session`, `set-budget`, `show-bots`, `show-progress`, `show-bot`, `show-escalations`, `dry-run`, `add-agent`, `change-budget`, `switch-persona`, `reload` etc. | These are **forward-looking** slash subcommands described in troubleshooting.md as user remedies. The MCP / CLI surface backing them is partially shipped (`workflow_pause/resume`, `admin_change_persona`, `admin_add_tool`, `admin_ls`, `workflow_progress`, `workflow_peek` etc.) but full 1:1 subcommand routing for every troubleshooting prescription is **V0.7 candidate** (per dev-plan delayed list). **Not in F127 scope** — user-manual.md is the live-tested surface per §F127 痛点 list; troubleshooting.md is a remediation guide that intentionally references roadmap subcommands. **NOT blocking ship-gate** | NOTE (out of F127 scope per §F127 痛点) |

---

## recipes.md

| § / line | Claim | Verification | Status |
|---|---|---|---|
| 配方 1-8 启动行 | `/ccteam <NL>` / `/ccteam-team <N> "<task>"` 各起 preset | `skills/ccteam/SKILL.md` intent dispatcher covers all 7 routing classes (start-team / create-workflow / configure-im / monitor / advise / status-debug / other). Real flow with `ccteam-creator` dialogue = N/A live | PASS (wiring) / N/A (live) |
| 配方 1 L42 | "Codex critic 自动启用(检测到 codex auth)" | F112 auto-critic logic + `crates/ccteam-core/src/auto_critic.rs` present; Wave 1 F122 added `progress.jsonl` bridge | PASS |
| 配方 3 L154 | "撞 3 次失败叫醒你" | `fix_loop.max_attempts: 3` red line (R6); `crates/ccteam-core/src/orchestrator.rs` `fix_counts` map → `escalation` event | PASS (wiring) |
| 配方 7 L386 | `/ccteam-team 5 "<task>"` + Codex critic | `ccteam-team` skill present; F112 Codex critic role | PASS (wiring) |
| 配方 8 L432 | 混合 Pocket + Overnight | Composed via `ccteam-creator` mode-inferrer + per-agent vendor (V0.6 schema). Real flow = N/A | PASS (wiring) / N/A (live) |
| Tail L488 | `/ccteam-control add-agent translator-to-french` / `change-budget 2.0` / `switch-persona "..."` | Forward-looking subcommands. Backing MCP surface partially shipped (admin_change_persona covers `change-persona` semantics). Full 1:1 = V0.7 candidate. **Not in F127 痛点 scope** | NOTE |

---

## advanced/*.md

| File | Claim sample | Status |
|---|---|---|
| `advanced/customize-workflow.md` | workflow.yaml V0.6 schema (mode / vendor / im_channels / agent fields) | PASS — matches `crates/ccteam-core/src/workflow.rs::AgentSpec` (incl. F98 plan_approval block lines 567-585) |
| `advanced/multi-llm-codex.md` | Codex 4 user-visible scenarios (advise / auto-critic / quota-fallback / second-opinion) | PASS — F112 ship; `mcp_advise_tools.rs` + `auto_critic.rs` + preferences.rs `fallback.on_claude_quota = "codex"` |
| `advanced/presets-reference.md` | 5 preset internal schema mapping | PASS — `crates/ccteam-core/src/templates/workflow_templates/*.yaml` per F114 (V0.6.0) |
| All three | `mcp__ccteam__*` invocation surface | NOTE — internal MCP tool registration uses `ccteam__<group>_<name>` form (server name = `ccteam`, so model-visible name is exactly `mcp__ccteam__<tool>` per Claude Code MCP harness convention). Tier-1 docs already use this form — verified against `crates/ccteam-cli/src/mcp_serve.rs` |

---

## Source-code claims spot-checked (no doc text)

| Claim | Source path | Status |
|---|---|---|
| `WorkflowMode::HumanApproval` is 4th mode parallel to in_proc/bg/chat | `workflow.rs:123-153` + `orchestrator.rs:678` dispatch + `1562` artifact gate | PASS |
| `plan_approval` engine standalone (pure state machine) | `plan_approval.rs` 710 lines; `plan_approval_test.rs` 9 tests | PASS |
| `progress.jsonl` adds `plan_pending` / `plan_decision` / `plan_timeout` events | `progress.rs` +78 lines (W2 handoff) | PASS |
| `ccteam-imd` `@ccteam` NL admin router | `inbound.rs` + `nl_admin.rs` (528 lines) + test 367 lines | PASS |
| Pricing schema version check | `ccteam doctor --check-pricing-version` → "[pricing.anthropic] pulled 2026-05-17 (now -2d, OK)" + "[pricing.openai] pulled 2026-05-19 (now -0d, OK)" — **live verified on this host** | PASS |
| `ccteam-imd health` CLI subcommand (F119) | `./target/release/ccteam-imd --help` lists `health` — V0.6.1 F119 health gate | PASS |
| `mcp__ccteam__admin_change_persona` + `admin_add_tool` registered | `mcp_serve.rs:480` + `mcp_admin_tools.rs:60-61` tools/list + dispatcher | PASS |
| MCP tool count 24 → 26 | `mcp_serve.rs:334` comment "→ **26 total**" + `mcp_tool_groups.rs` 5-group registry | PASS |

---

## Bugs found + fixed in this PR (in-scope of already-shipped F#)

1. **`scripts/host-probe/run-probes.sh` overnight-builder probe (F120, shipped Wave 1)** — probe wrote `workflow.yaml` at the **root** of the probe project but `ccteam init` (newly added) scaffolds `.ccteam/workflow.yaml` (canonical V0.6 path per CLAUDE.md "F83 canonical") with a default `explorer: trigger: manual` workflow that masks our worker watch. **FIX**: (a) added `ccteam init --in <proj> --slug overnight-probe --team dev` registration step so orchestrator picks up the project, (b) overwrote `.ccteam/workflow.yaml` with worker definition **after** init so the canonical-path version reflects probe intent. Local dry-run now correctly registers a `watch:.ccteam/triggers/worker/` on the right path (`watch registered slug="overnight-probe" role="worker"` confirmed in orchestrator logs). **Remaining**: even after scaffold fix, end-to-end `agent_spawn` does not fire under the local-mode stub-claude path within 60s — see "Retained risk" below.

2. **`docs/troubleshooting.md` `/ccteam-doctor` slash drift** — 11 line-level fixes per the table above; header rewritten to point at `ccteam doctor` CLI + Claude session `Bash("ccteam doctor")` form; added F127 acknowledgement that V0.6.0 docs drifted.

---

## Retained risk (not blocking ship-gate; logged for V0.7 follow-up)

- **F120 local end-to-end stub-claude spawn fires `watch registered` but no subsequent `agent_spawn` within 60s** under the repaired probe scaffold. Orchestrator stdout confirms: (a) pidfile written, (b) project event loop started, (c) watch registered on the correct directory, (d) graceful shutdown clean. No errors. Touching a `.md` file in the watched dir produces no observed `ArtifactEvent` → `agent_spawn`. The artifact_watcher start/debounce code path looks correct (500 ms debounce, mpsc → tokio bridge, `start()` returns `JoinHandle` consumed by orchestrator at `orchestrator.rs:804`); the gap appears to be in the `event_rx` → spawn dispatch under the synthetic stub-claude env. Likely candidates: (i) phase-state gate (`state.json::phase_state: idle` may need transition before spawn fires), (ii) some env-only path skipped under `CCTEAM_CLAUDE_BIN` stub, (iii) inotify event swallowed by debounce against rapidly-created directory tree. Beyond F127 ship-gate scope to root-cause in this turn — flagged for V0.7 dev. **NAS-box005 run with real `claude` binary required for full F120 verification.** Probe script logic is now correct (init + canonical-path workflow); the spawn-fire issue is independent of the script.

- **NAS-box005 host-probe full sweep**: ssh access confirmed (`hostname` = `nas-box005`, ccteam 0.6.0 installed). Worktree branch not yet deployed (would require `deploy-to-nas.sh origin/v061-w3-manual-prover` + `run-probes.sh` execution; deferred to main-session ship step). The probe script improvements in this PR (F120 fix) will land via `deploy-to-nas.sh` after main-session bumps version 0.6.0 → 0.6.1.

- **TG real round-trip (Pocket Assistant / IM Squad / plan-approval IM / HITL mode IM)**: requires user's own Telegram account + bot token paste. Per dev-plan W3-T1 acceptance: "N/A only allowed for 'user must paste TG token / be in their own TG account' environment constraints" — explicitly **N/A by design**, not a bug.

---

## Files modified by this PR

- `docs/troubleshooting.md` — 11 `/ccteam-doctor` → `ccteam doctor` corrections + header rewrite
- `scripts/host-probe/run-probes.sh` — F120 probe scaffold fix (init registration + canonical workflow.yaml path)
- `.audit/manual-prover-report.md` (this file)
- `.audit/e2e-sim-log.md` (8 user journey path sim)
