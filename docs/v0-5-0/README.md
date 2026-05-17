# V0.5.0 — Agent Teams mode + 真 cost 数据源

> **立项主线**:把 Claude Code 官方 Agent Teams(v2.1.32+,`CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1`)接成 ccteam 的**第二种编排 mode**,补齐 `orchestration-patterns.md §五` 的 Parallelization(vote)缺口 + 提供 ccteam 当前最弱的 composability 突破口。
>
> **用户原话**:"agent teams 的 agent 之间的通信模式很好用,我在现实中经常用这个模式,但是 agent teams 的创建工厂,还有可视化是痛点,可以在 ccteam 中提供"。

---

## 一、定位:工厂 + 长跑观测员,不抢 lead 控制权

V0.4.6 `workflow.yaml` 隐式一种 mode = **artifact-driven**(ArtifactWatcher inotify → spawn `claude --bg --agent <role>`)。V0.5.0 引入 **agent-team mode**:

| Mode | 控制平面 | 通信 | 适用 |
|---|---|---|---|
| `artifact-driven`(默认,V0.4.0+ 已有)| `progress.jsonl` 事件 + 文件 watch | agent 间不直接通信,artifact 文件接力 | 长跑、流水线、决定论 |
| `agent-team`(V0.5.0 新增)| `~/.claude/teams/<>/` + 官方 mailbox(LLM-to-LLM)| teammate 之间直接 SendMessage + 共享 task list claim | 突发、并行 review、debate / 假设竞争 |

**ccteam 在 agent-team mode 的角色**:**lead 的工厂 + 哺乳期 + 长期观测员**,**不进控制平面**。
- workflow.yaml 当 lead 的"团队蓝图"(替代每次手敲自然语言开 team)
- 起一个**专用 `__lead` role 的 claude bg session**,塞入 seed prompt + `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1`(确定 §三 抉择 A)
- 装官方 3 hook(`TeammateIdle` / `TaskCreated` / `TaskCompleted`)→ 镜像事件到 `progress.jsonl`(SoT 红线守住)
- web SPA 加 3 个面板:Team Topology / Task Board / Mailbox

---

## 二、Finding 列表

### MVP(本版本必交付)

| # | 标题 | 一句话 |
|---|---|---|
| **F92** | 真 cost 数据源(linkScanPath jsonl) | `~/.claude/jobs/<>/state.json::cost_usd_total` 实测为 0,真实 cost 在 transcript jsonl 的 `usage` 字段;ccteam `cost_summary` 切到这个源。**agent-team mode 的预置项**(否则 budget cap + Cost Sparkline 都假数据) |
| **F93** | workflow.yaml `mode: agent-team` schema + `__lead` role | 新顶层 `mode` 字段;`agent_team` 子表声明 `lead_seed` / `teammate_mode` / `default_model` / `require_plan_approval` / `cleanup_on_stop`;`__lead` role 默认模板 + `ccteam init --mode agent-team` 工厂 |
| **F94** | Agent Teams 3 hook 镜像 + 5 新 event 类型 | `templates/settings.json` 加 `TeammateIdle` / `TaskCreated` / `TaskCompleted` hook(沿用 `__CCTEAM_BIN__ internal hook progress-append <event>` pattern);`progress.jsonl` 加 `team_task_created` / `team_task_completed` / `team_teammate_idle` / `team_member_joined` / `team_message_sent` 五类 |
| **F95** | ArtifactWatcher 扩展到 `~/.claude/teams/` + `~/.claude/tasks/` | 监 `config.json` 变化 → `team_member_joined`;监 `tasks/<team>/*` → 镜像 task event。只读不写 — Anthropic 是源,ccteam 是镜 |
| **F96** | Web SPA 3 新面板 | **Team Topology**(节点图:lead + teammate,显示 model / 当前 activity / cost / 状态)+ **Task Board**(Kanban 3 列 + 依赖图)+ **Mailbox Stream**(消息时间线);复用 F90 SSE 推送 |

### V0.5.x 延期(MVP 后打磨)

| # | 标题 | 为什么延期 |
|---|---|---|
| **F97** | Lifecycle 完善 — `cleanup_on_stop` 3 策略 / `--restart-team` / hot-reload 约束 | MVP 用 `force-kill` 简化语义;`ask-lead` + `leave-running` 需要跟 lead LLM 协同测试 |
| **F98** | plan-approval ↔ outbox 联动(扩 F87 intercept-ask)| MVP 走 native plan-approval(lead 自决);要走 outbox 让用户 review 需要扩 hook |
| **F99** | Claude Code 版本 gating + `doctor --check-agent-teams` | MVP 假定 user 已升 ≥2.1.32;正式 gating 在 V0.5.1 |

---

## 三、与 V0.4.6 + 5 模式的关系

**承上**:V0.4.6 F82 hot-reload + F84 budget cap + F86 graceful shutdown 全为 agent-team mode 准备好基础设施。teammate 是 `claude --bg` job 走 `~/.claude/jobs/<>/state.json` → F84 budget cap 自然覆盖(F92 修真数据源之后)。

**对照 `orchestration-patterns.md §五` 5 模式缺口**:

| 模式缺口(V0.4.6) | V0.5.0 agent-team mode 解决? |
|---|---|
| Parallelization(vote)— 同 prompt fork N 次 | ✅ 直接 — lead seed 写 "spawn 3 teammate 同 prompt vote merge" native 支持 |
| Composability(workflow.yaml extends)| ⚠️ 部分 — 单 workflow 内 lead 可 native compose;跨 workflow 仍需 V0.5.x extends 语法 |
| Routing(动态选 agent)| ❌ 不解 — 留 V0.5.x `agent.router: <expr>` sugar |
| Evaluator-Optimizer 显式 sugar | ❌ 不解 — 留 V0.5.x `agent.evaluates: ... max_iterations: N` |

---

## 四、文档

| 文件 | 内容 |
|---|---|
| `prd.md` | F92-F96 5 个 finding 的产品需求 + 验收标准 + V0.5.x 延期 finding 草案 |
| `dev-plan.md` | 实现路径、文件改动、wave 划分(3 wave)、测试矩阵、风险盘点 |
| `user-manual.md`(ship 后写)| V0.5.0 用户使用手册 — workflow.yaml mode 选型 + agent-team 入门 |

---

## 五、参考

- 官方文档:
  - [Agent Teams](https://code.claude.com/docs/en/agent-teams) — lead/teammate/task list/mailbox 机制
  - [Subagents](https://code.claude.com/docs/en/sub-agents) — teammate 复用 subagent 定义
  - [Agent View](https://code.claude.com/docs/en/agent-view) — `claude agents` 跟 ccteam web 互补关系
- ccteam tier-1:
  - `orchestration-patterns.md §二.4 §五` — Orchestrator-Worker 两条路线 + 缺口表(本版本是缺口的部分落地)
  - `tech-design.md §2.1` — 3-layer 架构(L0/L1/L2/L3),V0.5.0 加 mode 后 L2 orchestrator 多一条分支
  - `interfaces.md §17`(workflow.yaml schema)+ §6.1(settings.json hook 模板)— F93 / F94 协议同步入口
