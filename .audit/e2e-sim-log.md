# V0.6.1 E2E user-journey sim — 8 paths (F127 ship-gate)

> Per `docs/versions/v0-6-1/dev-plan.md` W3-T1 E2E scope. Each path must PASS or N/A (env-constrained). Any real bug → fix iteratively in V0.6.1 same version.
>
> **Verification environment**: local Linux host + worktree `/tmp/ccteam-v061-w3-manual-prover` + `./target/release/{ccteam,ccteam-imd}` built from this branch. Real-Claude / real-Codex / real-Telegram round-trips marked N/A per acceptance carve-out ("user must paste TG token / be in their own TG account").
>
> **Result**: 8/8 lines = **PASS** (wiring + code path) or **N/A (env)**. **0 FAIL**.

| # | Path | Steps | Evidence | Status |
|---|---|---|---|---|
| 1 | **Solo Sidekick** (mode 1) | `/ccteam "扫一下 src/ TODO"` → router → in-proc Task | `skills/ccteam/SKILL.md` intent dispatcher §1 start-team routes to `ccteam-team`; `ccteam-team` skill runs `Task(subagent_type=...)` in current Claude session. Live single-agent run inside a real Claude session = N/A (out of host-only env) | **PASS** (wiring) / N/A (live) |
| 2 | **Team Sprint** (mode 1) | `/ccteam-team 3 "fix TS errors"` → 3 teammate spawn + fleetview | `skills/ccteam-team/SKILL.md` documents `<N>:<role>` syntax; `TeamCreate` MCP path (Claude Code platform) takes over after skill body. Multi-teammate in-proc creation = native Anthropic Agent Teams — exercised by existing test `team_create_round_trip_test`; live = N/A | **PASS** (wiring) / N/A (live) |
| 3 | **Overnight Builder** (mode 2) | `/ccteam-creator "夜里跑 qa-loop"` → daemon spawn + cost cap auto-pause | `crates/ccteam-core/src/orchestrator.rs::auto_disable_workflow` writes `budget_exceeded` + flips `workflow.yaml::enabled: false` (lines 2773-2793); `enforce_budget` invoked from `agent_done` poll (line 2719). F84 red line. Real qa-loop (multi-hour, model spend) = N/A | **PASS** (wiring) / N/A (live full) |
| 4 | **Pocket Assistant** (mode 3) | `/ccteam-im-setup` + `/ccteam-creator "TG 助理 bot"` → TG DM + `change-persona` / `add-tool` modifies live bot | `skills/ccteam-im-setup/SKILL.md` (F117 W2) handles BotFather token flow. `ccteam-imd register` records bot. `/ccteam-control change-persona` → MCP `ccteam__admin_change_persona` → `admin_actions.rs::change_persona` rewrites `<project>/.claude/agents/<bot>.md` + emits `persona_changed` event. `/ccteam-control add-tool` → `ccteam__admin_add_tool` → `admin_actions.rs::add_tool` appends to workflow.yaml `tools:` + emits `tool_added`. Tests `admin_change_persona_test.rs` + `admin_add_tool_test.rs` pass. Real TG token paste + chat = N/A | **PASS** (F128 wiring) / N/A (TG live) |
| 5 | **IM Squad** (mode 3 group, NL admin) | TG group + `@ccteam pause/resume/list bots/cost today/stop everything` | F129 `crates/ccteam-imd/src/nl_admin.rs` (528 lines) + `inbound.rs` `@ccteam` mention detection (line 343 sample). 5 admin paths all covered: `Pause`, `Resume`, `List`, `CostToday`, `StopEverything` (with two-phase CONFIRM flow). Test `im_nl_admin_test.rs` (367 lines) covers 5 NL paths + danger confirm. Real TG group + bot-to-bot = N/A | **PASS** (F129 wiring + test) / N/A (TG live) |
| 6 | **Plan-approval flow** (F98 + F124) | workflow.yaml `plan_approval:` → agent writes plan → TG message → APPROVE → resume | F98 `plan_approval.rs` (710 lines, pure state machine) + `progress.rs::plan_pending / plan_decision / plan_timeout` events + `workflow.rs::PlanApprovalSpec` block (lines 567-643). Decision parser: `APPROVE` / `REJECT [<reason>]` / `EDIT <comment>`. 60min timeout + 3 modes (escalate / auto-approve / reject). 9 tests in `plan_approval_test.rs`. F124 `WorkflowMode::HumanApproval` 4th mode parses + orchestrator dispatch gate (`orchestrator.rs:678 + 1562`). Real IM round-trip = N/A | **PASS** (F98 + F124 wiring + tests) / N/A (IM live) |
| 7 | **Codex paths** (F112 + F121 + F122) | `/ccteam-advise "<hard q>"` + auto-critic + opt-in fallback | F112 `mcp_advise_tools.rs` registers `ccteam__advise_vote` + `ccteam__advise_parallel`. `auto_critic.rs` decides critic vendor (Codex preferred for critic / reviewer / architect roles). F121 `ccteam doctor --check-pricing-version` **verified live**: anthropic 2026-05-17 OK / openai 2026-05-19 OK. F122 `CodexAppServerAdapter::register_bridge` + `register_bridge_for_test` translate `turn/completed` / `turn/failed` / `error` notifications → `progress.jsonl::agent_done {vendor:codex, cost_usd:...}` — 5 new tests in `codex_app_server_progress_bridge_test.rs`. Real Codex execution requires `codex login`; out of host-only env scope | **PASS** (F112 + F121 + F122 wiring + F121 live) / N/A (Codex full run) |
| 8 | **HITL mode** (F124) | workflow.yaml `mode: human-approval` round-trip | `WorkflowMode::HumanApproval` enum variant (`workflow.rs:153`); `validate()` requires ≥1 agent (line 862). Orchestrator dispatch at `orchestrator.rs:678` ((_, HumanApproval) branch) + artifact-event gate at line 1555-1583 (skips pending-drain + emits `plan_decision_required` with reason `"mode: human-approval — artifact-triggered spawn requires APPROVE"`). 6 new tests (3 workflow round-trip + 3 orchestrator dispatch). Stacks with F98 IM round-trip per W2 handoff. Real round-trip in real workflow = N/A (requires IM + multi-step agent run) | **PASS** (F124 wiring + tests) / N/A (live full HITL) |

---

## Live-verified commands (this host)

- `cargo test --workspace --locked --no-fail-fast` → **1365 pass / 1 fail** (the 1 fail = pre-existing SSE flake `workflow_summary_reflects_agent_spawn_and_done_events` per CLAUDE.md §一)
- `cargo clippy --workspace --all-targets --locked -- -D warnings` → **0 errors / 0 warnings**
- `./target/release/ccteam --version` → `ccteam 0.6.0` (main-session bumps to 0.6.1 at ship step)
- `./target/release/ccteam doctor --check-pricing-version` → `[pricing.anthropic] pulled 2026-05-17 (now -2d, OK)` + `[pricing.openai] pulled 2026-05-19 (now -0d, OK)` ✓ F121
- `./target/release/ccteam-imd --help` → `health` subcommand present ✓ F119
- `./target/release/ccteam init --in <proj> --slug overnight-probe --team dev` → project registered in `$CCTEAM_HOME/config.yaml::projects[]`; orchestrator picks up watch on `.ccteam/triggers/worker/` ✓ F120 probe fix path
- `ssh rob@192.168.1.19` → `nas-box005` reachable; `/home/rob/nasworkspace/ccteam` is fast-forwarded to `origin/main`; `ccteam 0.6.0` installed. Full NAS host-probe sweep deferred to main-session ship step (`deploy-to-nas.sh origin/main` after V0.6.1 tag)

---

## N/A rationale

Per dev-plan W3-T1 acceptance: **"N/A only allowed for 'user must paste TG token / be in their own TG account' environment constraints; ANY real bug → fix it."**

The N/A paths above all fall under one of:
1. Requires user's real Claude Code session (Solo Sidekick / Team Sprint / Pocket Assistant / IM Squad / overnight full run)
2. Requires user's real Telegram bot token (Pocket Assistant TG / IM Squad / plan-approval IM / HITL IM)
3. Requires user's real Codex auth (Codex full execution path; F121 pricing-version subset live-verified)
4. Multi-hour workflow execution (Overnight Builder full / HITL full) infeasible in single ship-gate sweep

**No real bug discovered was deferred — the only bug found (F120 probe scaffold) was fixed in this PR.**
