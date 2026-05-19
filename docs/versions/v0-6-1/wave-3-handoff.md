# V0.6.1 Wave 3 — Handoff

> **Status**: doc-syncer (W3-T3) PR open, awaiting merge. demo-recorder (W3-T2) shipped (TaskList #12 completed). manual-prover (W3-T1) running E2E sim ship-gate; tag v0.6.1 blocked until `[ship-gate-pass]` PR title arrives + W3 PRs merge.
> **Baseline (doc-syncer worktree)**: **1365 / 1**(本 wave doc-only — 与 Wave 2 整合 baseline 持平;1 fail = pre-existing SSE flake `workflow_summary_reflects_agent_spawn_and_done_events`)。
> **Clippy**: 0 errors,0 warnings(`-D warnings` clean)。
> **Wall time**: doc-syncer worktree ~45 min;主 session 整合(W3 PR review + version bump + nas-box005 probe + tag)预计 ~1h。

## Decided

### F125 finalize + tier-1 doc 同步(本 PR / doc-syncer)

- **CLAUDE.md §一 表格回填**:
  - `Workspace version` → **0.6.1**
  - `测试 baseline` → **1365 / 1**(Wave 2 整合后,Wave 3 doc-only 持平)
  - `代码规模` → ~73 kLOC Rust(workspace,V0.6.0 ~70 + V0.6.1 +3k:plan_approval.rs 710 + admin_actions.rs 396 + mcp_admin_tools.rs 210 + nl_admin.rs 528 + tests 等)
  - `当前最新版` → **V0.6.1**(Epic D cleanup + Epic E user-claim + Epic F plan-approval;F98 + F119 + F120 + F121 + F122 + F123 + F124 + F125 + F126 + F127 + F128 + F129)
  - `V0.6.x 延期候选` → **空**(本版闭所有 retained risk)
  - `V0.7 主线候选` → 新增独立行:Epic C 国内 IM(WeChat / 飞书 / DingTalk / QQ)+ chat memory 跨设备同步 + monorepo-aware `.mcp.json` + migrate-from-claude + 6 号编排模式深化(HumanApproval × bg/chat 矩阵)
  - 段落叙述更新:`workflow.yaml` 新增 `mode: human-approval`(F124) + `plan_approval:` block(F98) + `agents[*].tools:` field(F128 add-tool 写入);MCP 工具 **24 → 26**(+ admin_change_persona + admin_add_tool);新增 progress event:`plan_pending` / `plan_decision` / `plan_timeout`(F98)+ `persona_changed` / `tool_added`(F128)+ V0.6.1 F129 `@ccteam <NL>` IM mention 5 keyword admin
  - 必读文档表 row 9-10 由 V0.6.0 切到 V0.6.1,V0.6.0 退后到 row 11
  - §四 MCP 行:**24 → 26 工具**(V0.6 F111 24 + V0.6.1 F128 +2)

- **CLAUDE.md §三 红线表**:Wave 1 / Wave 2 已加 3 个新 row(F126 `root README.md MUST be English` + F126 `README.md 不含版本进展/状态信息` + F124 `HITL approval state SoT`)— Wave 3 verify 仍在,无重复合并。

- **`docs/tech-design.md`** sync(本 PR):
  - §3.3.1 schema 速览插入 V0.6.1 F124 + F98 HITL 扩展 yaml example(`mode: human-approval` + per-agent `plan_approval:` + `tools:` field)+ 解释段(`mode: human-approval` vs `plan_approval:` 独立可叠加;共享 `plan_pending` / `plan_decision` / `plan_timeout` event)
  - §6.5 MCP servers 标题 + 总数行:`24 工具` → `26 工具`(V0.6 F111 24 + V0.6.1 F128 +2 admin tool)
  - §2.1 架构 ascii 内 comment `24 → 26 mcp__ccteam__*` tools
  - **§6.13 Plan-approval ↔ IM outbox round-trip(V0.6.1 F98 + F124)** 新章节(~80 行):engine 解耦说明(pure state machine,plan_approval.rs 710 行 + orchestrator 解耦)+ ASCII sequence diagram(agent ↔ orchestrator ↔ ccteam-imd ↔ user IM round-trip)+ decision grammar(APPROVE / REJECT [<reason>] / EDIT <comment>)+ timeout 3 策略(escalate / auto-approve / reject)+ 4 红线 + tests reference
  - **§6.14 Admin actions(V0.6.1 F128)** 新章节(~30 行):daemon-side 文件 mutation only + skill-side NL → markdown merge 分工架构(R3 + R4 红线兜底)+ 2 MCP tool schema + 生效路径(下次 turn / F82 workflow.yaml 热加载)+ tests reference
  - **§6.15 IM NL admin via meta-agent(V0.6.1 F129)** 新章节(~30 行):`@ccteam` mention 检测路径(在 `@<bot>` route 之前)+ 5 keyword admin action table(pause / resume / list / cost / stop everything)+ 危险动作 2 步 confirm flow + hop_limit 不消耗 + tests reference

- **`docs/interfaces.md`** sync(本 PR):
  - §4.1 progress event 表 +6 行(`chat_session_reset` / `chat_session_reset_with_recovery` / `turn_done` / `plan_pending` / `plan_decision` / `plan_timeout` / `persona_changed` / `tool_added` — V0.6 F108/F118 之前漏写,V0.6.1 顺便补)
  - §12.2 工具表 header 24 → **26 工具**;`workflow_`(13 → 15)/ `chat_`(5)/ `advise_`(2)/ `admin_`(1 → 3)/ `screenshot`(1)— group 内数字精确
  - 新增子段 **"V0.6.1 F128 admin extension"**:2 工具 schema 表(`admin_change_persona` + `admin_add_tool`)+ 4 行红线说明(走 admin group / daemon 不调 LLM / 事件 SoT / `/ccteam-control` skill 入口)
  - §17.1.2 `WorkflowMode` 表 +2 row(`chat`(F108)+ `human-approval`(F124));段落补 "`mode: human-approval` 与 `agents[*].plan_approval:` 可独立使用 — workflow-level gate vs agent-level gate;两者共享 3 progress event"
  - §17.2 `AgentSpec` 表 +2 row(`plan_approval` Option<PlanApprovalSpec> + `tools` Vec<String>)
  - 新增 **§17.2.2 V0.6.1 F98 `PlanApprovalSpec` 字段**:完整 yaml example + 4 字段表 + 6-step flow + 红线(progress.jsonl SoT + 文件 inbox 路径 + no prompt injection + decision parser grammar)

- **`docs/dev-coupling-audit.md`** +12 row(F98 + F119-F129),格式与已 ship F-finding 一致(`F# | V0.6.1 | ✓ shipped wave N | <一行 scope> → docs/versions/v0-6-1/prd.md §F#`)。row 列在 V0.6 F106-F118 block 之后,V0.4.6 摘要更新之前。

- **`docs/claude-code-tool-surface.md`** sync:
  - L183 MCP 行 `17 个工具` → `26 个工具`(V0.6 F111 24 + V0.6.1 F128 +2)+ group 子前缀细化 + 高频示例 list 更新
  - L258 文末段 `17 个 mcp__ccteam__\* 工具` → `26 个`,补 V0.6.1 高频 `admin_change_persona` / `admin_add_tool` + 新 progress event 列表

- **`docs/orchestration-patterns.md`** + **`docs/ccteam-as-domain-agnostic-orchestrator.md`** — **不动**(F125 §2 drift grep 全 0,与 V0.6.1 finding 无交叉)

### F125 drift 验证(W3 finalize 后跑)

PRD §F125 §2 listed greps 在 tier-1 evergreen docs 全部 0 命中,除以下 **3 个 legit 命中**(legitimate serde-compat 字段文档,非 stale 引用):

- `docs/interfaces.md` L659 `"phase_state": "in_flight"` — `ccteam ls --format json` schema 描述,字段在 `crates/ccteam-core/src/state.rs::CcteamState` 仍 `#[serde(default)]` 兼容 V0.2/V0.3 era 老 state.json
- `docs/interfaces.md` L806 `current_phase` / `phase_state` — flex kind state 描述,同样是 serde-compat
- `docs/interfaces.md` L1256 `phase_state` 回 `Idle` — `/api/{slug}/resume` 行为描述,对应 `actions::resume` 实际逻辑

所有 3 处描述与代码一致,不是 stale。其他 drift grep(V0.5 旧 API `spawn_session` / V0.4 phase machinery `pag.rs` / V0.5 unprefixed MCP 名 / V0.5 cost path / V0.5 mode 3 `claude -p --resume` + `stream-json + stdin pipe`)全 0 命中。

## Rejected

- ~~把 phase_state / current_phase 描述从 interfaces.md 删~~ — 字段仍在 `CcteamState` serde-compat,删 doc 等于 doc-vs-code drift 反方向。
- ~~修 `orchestration-patterns.md` / `ccteam-as-domain-agnostic-orchestrator.md`~~ — V0.6.1 finding 不触这两 doc(orchestration 5 模式 / domain-agnostic 责任分界),F125 §2 drift grep 全 0;不动。
- ~~重写 `docs/interfaces.md §10.3 ls JSON schema` 加 V0.6.1 状态字段~~ — workflow_summary / ls schema 没新字段(F128 emit progress.jsonl event,不进 ls 顶层);schema 不变。
- ~~把 §6.13 / §6.14 / §6.15 合并成单一 "V0.6.1 features" 节~~ — 三 finding 各有独立架构故事(plan-approval engine 解耦 + admin daemon/skill 分工 + IM mention router),合并 = 信息损耗。

## Risks(待主 session 兜)

- **manual-prover Wave 3 E2E sim 仍在跑**(W3-T1)— sim 发现 bug 需在本版修(no V0.6.2 split)。doc-syncer 工作不包 user-facing docs(`docs/{quickstart,user-manual,recipes,troubleshooting}.md` + `docs/advanced/*`)— manual-prover own。如 manual-prover 在 user-facing doc 改 wording,本 PR doc-syncer 不冲突(不动文件)。
- **ship gate**:主 session 等 manual-prover PR title `[ship-gate-pass]` 才能 `git tag v0.6.1`;本 PR doc-syncer 仅作 final tier-1 sync,**不**直接 unlock ship。
- **nas-box005 host probe 未亲跑**:本 PR doc-only,无 host probe;主 session ship 阶段(`deploy-to-nas.sh + run-probes.sh`)真跑 8 scenarios。
- **demo GIF (F123 W3-T2)** 已 ship(TaskList #12 completed,但本 worktree 不含 demo 文件)— 主 session merge 时 verify `docs/versions/v0-6-0/demos/*.gif` 5 个文件存在 + ≤500KB + README + quickstart 引用 valid。

## Files

修改(7):
- `CLAUDE.md`(§一 表格 + 段落叙述 + §二 必读文档表 + §四 MCP 行)
- `docs/tech-design.md`(§3.3.1 yaml example + §6.5 MCP server 行 + §2.1 ascii comment + §6.13 + §6.14 + §6.15 三新章节 ~140 行)
- `docs/interfaces.md`(§4.1 progress event 表 +6 行 + §12.2 工具表 header + admin extension 子段 + §17.1.2 WorkflowMode 表 + §17.2 AgentSpec 表 +2 row + §17.2.2 PlanApprovalSpec 新子段)
- `docs/dev-coupling-audit.md`(+12 row F98 + F119-F129)
- `docs/claude-code-tool-surface.md`(L183 MCP 行 + L258 文末段)

新文件(1):
- `docs/versions/v0-6-1/wave-3-handoff.md`(本文件)

不动:
- `docs/orchestration-patterns.md` + `docs/ccteam-as-domain-agnostic-orchestrator.md`(F125 §2 drift grep 全 0,与 V0.6.1 finding 无交叉)
- `docs/{quickstart,user-manual,recipes,troubleshooting}.md` + `docs/advanced/*`(F127 manual-prover Wave 3 own;doc-syncer 不抢)
- `docs/versions/v0-X-Y/`(per CLAUDE.md §五 Pre-v1.0 不留技术债 + EOL 内容去版本 dir)— **除本 wave-3-handoff.md 写 v0-6-1/**
- `README.md`(root,F126 Wave 1 EN rewrite + 红线落,本 wave 无需再动)
- `.audit/`(无 W3 doc-syncer 产出)

## Remaining(主 session ship — W3-T4 等价 Task #14)

按 dev-plan.md Wave 3 集成 + ship 段:

1. **W3 PR review/merge**:
   - W3-T2 demo-recorder(F123)— TaskList #12 已 completed,verify PR open + merge
   - W3-T1 manual-prover(F127)— 等 PR title 含 `[ship-gate-pass]` 才 merge
   - W3-T3 doc-syncer(F125 finalize)— 本 PR,merge after CR

2. **整合 baseline + clippy**(merge 完跑):
   ```bash
   env -u HTTP_PROXY -u HTTPS_PROXY -u http_proxy -u https_proxy cargo test --workspace --locked --no-fail-fast
   env -u HTTP_PROXY -u HTTPS_PROXY -u http_proxy -u https_proxy cargo clippy --workspace --all-targets --locked -- -D warnings
   ```
   预期:**baseline ≥ 1365 / 1**(本 wave doc-only;manual-prover 若有 fix 可能增)。

3. **`workspace.package.version` bump 0.6.0 → 0.6.1**(`Cargo.toml`)+ `cargo build --workspace --release`(regen `Cargo.lock`)

4. **nas-box005 deploy + full host probe**:
   ```bash
   scripts/host-probe/deploy-to-nas.sh
   scripts/host-probe/run-probes.sh all
   ```
   预期:8 scenarios 全 rc=0 + cost.txt 非零(F119 + F120 已加 daemon-start + overnight-builder real probe,本 wave 该真跑通)。

5. **Tag + push**:
   ```bash
   git tag v0.6.1
   git push origin main v0.6.1
   ```

6. **TG ping**:`@web3op_bot` 推送 `[ccteam v0.6.1] shipped 🎉 — F98 + F119-F129 (12 findings) — Epic D cleanup + Epic E user-claim + Epic F plan-approval`

V0.7 主线候选(本版不在):Epic C 国内 IM 启用(WeChat / 飞书 / DingTalk / QQ)+ chat memory 跨设备同步 + monorepo-aware `.mcp.json` + migrate-from-claude + 6 号编排模式深化(HumanApproval × bg/chat 矩阵全开 + plan_approval scale-out 多 outbox channel)。
