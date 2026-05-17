# V0.5.0 — Agent Teams skill + 长跑可视化 + 真 cost + skill/meta-agent 全面重塑

> **立项主线**:让用户在自己习惯的 `cd → claude → /<command>` 流里起 agent team,ccteam **不要拽用户出 session**;ccteam 的差异化价值是**长跑可视化** + **跨设备 / 跨重启监控**,不是抢 lead 控制权。
>
> **用户原话**(决定本版定位):
> - "agent teams 的 agent 之间的通信模式很好用,**创建工厂 + 可视化是痛点**"
> - "用户习惯:1、cd project 启动 claude;2、claude session 输入「创建一个团队…」;3、session 中观看团队成员通信细节"
> - "OMC 的模式是 `/oh-my-claudecode:team N:agent-type "task"`,ccteam 现在用户交互很复杂"

---

## 一、双路径定位

V0.5.0 提供**两条 user path**,分层清晰:

### 🟢 Primary path:**`/ccteam:team` skill in current Claude session**(95% 用户)

```
$ cd ~/projects/blog
$ claude
> /ccteam:team "build a Next.js blog: researcher, frontend-dev, reviewer"
Claude (becomes team lead via native TeamCreate):
  TEAM PLAN
  =========
  Team name: blog-team
  Proposed teammates:
    1. researcher (ad-hoc, sonnet, blue) — research Next.js + Velite + Shiki
    2. frontend-dev (ad-hoc, sonnet, green) — scaffold + content + pages
    3. reviewer (ad-hoc, sonnet, yellow) — code review
  Reply 'go' to spawn, or revise.
> go
Claude: [native TeamCreate + 3 × Task(team_name, name, prompt)]
> [用户在同一 session 里看 SendMessage 通信]
```

同时(daemon 已 `ccteam start` 跑着):
- ccteam daemon 全局 watch `~/.claude/teams/blog-team/`
- ccteam web `http://localhost:7331/teams/blog-team` 实时可视化(Topology + Task Board + Mailbox)
- 关 session 后 team 暂停;`claude attach <id>` 或 `/ccteam:team` 在新 session 里 resume

这条 path **完全对齐** OMC `/oh-my-claudecode:team` 用户体感 + **超过 OMC** 的长跑可视化能力。

### 🟡 Advanced path:**workflow.yaml `mode: agent-team` + bg `__lead`**(automation use case only)

```
$ ccteam init --mode agent-team my-debate
$ vim my-debate/.ccteam/workflow.yaml  # 声明 suggested_teammates / lead_seed / budget cap
$ ccteam start my-debate                # [Y/n/attach] confirm prompt
$ ccteam attach my-debate               # 或后续任意时刻 attach
```

适用场景:**用户长时间不在,机器自跑** N 天(对应 `requirements.md §14` V1.0.0 token-maxxing 终极目标的 building block)。普通用户用不到 — 用 primary path 即可。

---

## 二、Anthropic 是 SoT,ccteam 只读

实地探测 host `~/.claude/teams/roblog/` 发现 Anthropic 把完整 team 元数据落文件:
- `config.json` — members 拓扑(每 member 含 agentId/name/color/agentType/model/prompt/cwd/tmuxPaneId/subscriptions/backendType/planModeRequired)
- `inboxes/<teammate>.json` — **单文件 per 收件人,数组 of messages**(`from`/`text`/`timestamp`/`color`/`read`)
- `~/.claude/tasks/<team_name>/<id>.json` — 任务文件

→ ccteam **绝不复刻**这些结构;F95 watcher 读 + 镜像到 `progress.jsonl`,F96 web 可视化。两条 path 共享同一 F95/F96 观察层。

---

## 三、Finding 列表

### MVP(V0.5.0 完整交付,5 finding)

| # | 标题 | 所属 path | 一句话 |
|---|---|---|---|
| **F92** | 真 cost 数据源(linkScanPath jsonl)| 两条 path 共享 | `state.json::cost_usd_total` 实测为 0,真数据在 transcript jsonl `usage`;`cost_summary` 切源;F96 teammate cost / F84 budget cap 前置项 |
| **F93** | `/ccteam:team` skill 工厂(**primary path**) + `mode: agent-team` workflow.yaml + `__lead.md`(**advanced path**)| 两条 path 共建 | 共享 `agents/__lead.md` body(skill 加载 / advanced 当 system prompt);Worker Preamble + Plan-first Protocol + definition vs ad-hoc 两类 teammate 支持;skill 安装通过 `ccteam doctor --install-skill` |
| **F94** | Agent Teams 3 hook 镜像(精度提升)| Advanced path 装(skill 不动 settings.json) | 仅 ccteam-spawned `__lead` session 的 project settings.json 装 `TeammateIdle` / `TaskCreated` / `TaskCompleted`;Primary path 走 F95 watcher fallback;6 类 event 总数 |
| **F95** | 全局 watcher — 读 `~/.claude/teams/` SoT(**MVP 核心,两条 path 共享**)| 两条 path 共享 | daemon 全局扫 `~/.claude/teams/<>/` + `inboxes/<teammate>.json` + `~/.claude/tasks/<>/`;5 类 team event 镜像到 `progress.jsonl`。**完全独立于 ccteam workflow** — 无论 user 怎么起 team,都看得到 |
| **F96** | Web SPA Teams tab + 3 新面板(覆盖所有 host teams)| 两条 path 共享 | 新顶级 `/teams` tab;详情页 Team Topology + Task Board + Mailbox(`read: false` 高亮)+ 5 API + SSE。**核心差异化** vs OMC(无可视化)+ vs Claude Code native(关终端就丢)|
| **F100** | Skill surface refactor(5 → 3)| 全局 | 删 `ccteam-team-author` skill 整目录 + `ccteam team init/publish` CLI + `team_factory*.rs` + `teams/dev|research|research-academic/` V0.2 phase 残留;合并 `ccteam-project-creator` 到 `ccteam-creator`(4-phase dialogue 改名 step 1-4);重写 `ccteam-control` 清 phase;V0.5.0 ship 后只剩 `ccteam-control` / `ccteam-creator` / `ccteam-team` 3 skill |
| **F101** | Meta-agent 角色重塑 — 轻量 router + memory bridge + dashboard | 全局 | `meta_agent_role.md` 303 → ~150 行,删 26 处 phase 提及;删 `kickoff_reverse_interview.md` + `review_with_user_loop.md` V0.2 残留;meta-agent 不再 enforce phase / 不再当 agent team lead,delegate 到 `/ccteam:team` skill + `ccteam-creator` skill;保留 cross-project memory bridge + dashboard chat |

### V0.5.x 延期

| # | 标题 | 为什么延期 |
|---|---|---|
| **F97** | Advanced path lifecycle 完善 — `cleanup_on_stop` 3 策略 / `--restart-team` / orphan scan | MVP `force-kill` + ccteam doctor `--gc-orphan-teammates` 够用 |
| **F98** | plan-approval ↔ outbox 联动(扩 F87 intercept-ask)| MVP 走 native plan-approval(lead 自决)|
| **F99** | Claude Code 版本 gating + `doctor --check-agent-teams` | MVP 假定 user 已升 ≥2.1.32 |

---

## 四、跟 OMC + Anthropic 的差异化对比

| 维度 | OMC `/team` | Anthropic native | ccteam V0.5.0 |
|---|---|---|---|
| 在 session 起 team(habit-aligned)| ✅ | ✅ | ✅ Primary path `/ccteam:team` |
| Worker Preamble + Plan-first | ✅ | ❌ | ✅(borrow OMC)|
| Long-running unattended | ❌ user session 一关就停 | ❌ same | ✅ Advanced path:`ccteam start` bg `__lead` + `claude attach` |
| **跨设备可视化** | ❌ pure CLI | ❌ in-process/tmux 限本机终端 | ✅ web SPA on `http://<host>:7331/teams/` |
| **多 team 总览** | ❌ one team at a time | ❌ same | ✅ `/teams` tab 列全部 host teams |
| **Mailbox 未读高亮** | ❌ | ❌ in-process 自动标已读丢信息 | ✅ Anthropic 的 `read: bool` 直接用 |
| Stage routing 固定 5-stage | ✅ enforced | — | ❌ shape-agnostic(差异化保留)|

---

## 五、跟 V0.4.6 + 5 模式的关系

**承上**:V0.4.6 F82 hot-reload + F84 budget cap + F86 graceful shutdown 给 advanced path 准备好基础设施。Primary path 不用 ccteam workflow 机制,纯靠 F95 全局 watcher。

**对照 `orchestration-patterns.md §五` 5 模式缺口**:

| 缺口(V0.4.6) | V0.5.0 解决? |
|---|---|
| Parallelization(vote)| ✅ Primary `/ccteam:team` skill 自然支持(lead 自决)|
| Composability | ⚠️ 部分(skill 内 lead 可 native compose;跨 workflow 仍 V0.5.x)|
| Routing(动态)| ❌ V0.5.x `agent.router: <expr>` sugar |
| Evaluator-Optimizer 显式 sugar | ❌ V0.5.x |

---

## 六、文档

| 文件 | 内容 |
|---|---|
| `prd.md` | F92-F96 5 个 finding 产品需求 + 验收 + V0.5.x 延期 finding 草案。**§ F93a 是 skill primary path,§ F93b 是 advanced CLI factory** |
| `dev-plan.md` | 实现路径 + wave 划分 + 测试矩阵。**Wave 1 = skill + 全局 watcher + web(覆盖 95% 用户),Wave 2 = advanced CLI** |
| `user-manual.md` | Primary path 入门 + advanced path 选型决策树 + 故障排除 + V0.4.6→V0.5.0 升级注 — V0.5.0 ship 后写完 |

---

## 七、参考

- 官方文档:
  - [Agent Teams](https://code.claude.com/docs/en/agent-teams) — lead/teammate/task list/mailbox 机制
  - [Subagents](https://code.claude.com/docs/en/sub-agents) — teammate 复用 subagent 定义(definition vs ad-hoc)
  - [Agent View](https://code.claude.com/docs/en/agent-view) — `claude agents` 跟 ccteam web 互补关系
- ccteam tier-1:
  - `orchestration-patterns.md §二.4 §五` — Orchestrator-Worker 两条路线 + 缺口表
  - `tech-design.md §2.1` — 3-layer 架构,V0.5.0 在 L1 加 skill,L2 daemon 加全局 team watcher,L3 不变
  - `interfaces.md §17`(workflow.yaml schema)+ §6.1(settings.json hook 模板)
- 研究笔记:
  - `research/omc-team-comparison.md` — OMC 1040 行 SKILL.md 借鉴 + Anthropic 实测 schema(本版立项依据)
