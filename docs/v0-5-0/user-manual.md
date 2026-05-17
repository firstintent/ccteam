# V0.5.0 — User Manual

> ccteam V0.5.0 加 **Anthropic Agent Teams 集成** + **真 cost 数据源**。两条路径并存:Primary skill 用于 95% 用户的 in-session 起 team;Advanced CLI 用于 automation / 长跑 unattended 场景。
>
> 适用 Claude Code ≥ 2.1.32 + experimental flag `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1`(advanced path 自动注入)。

---

## 一、Quick install

```
# 拉最新
cd /path/to/ccteam && git pull origin main

# build + install 到 ~/.cargo/bin/(或 host 路径)
cargo install --path crates/ccteam-cli --locked

# 装 V0.5.0 3 个 skill 到 ~/.claude/skills/
ccteam doctor --install-skill all
```

装完后 `ccteam --version` 应显示 `0.5.0`,`ls ~/.claude/skills/` 应见 `ccteam-control` / `ccteam-creator` / `ccteam-team`。

---

## 二、Primary path:`/ccteam:team` skill(95% 用户)

最贴近 Claude Code 原生习惯 — `cd project → claude → 输入 prompt`。team 在当前 session 里跑,关 session 即停(对齐 Anthropic + OMC 行为)。

### 起 team

```
$ cd ~/projects/blog
$ claude
> /ccteam:team "build a Next.js blog with researcher + frontend-dev + reviewer"
```

Claude 当前 turn 升级为 lead,输出 `TEAM PLAN ===` 框,**STOP 等 user 确认**:

```
TEAM PLAN
=========
Team name: blog-team
Proposed teammates:
  1. researcher (ad-hoc, sonnet, blue) — research Next.js + Velite + Shiki
  2. frontend-dev (ad-hoc, sonnet, green) — scaffold + content + pages
  3. reviewer (ad-hoc, sonnet, yellow) — code review

Reply 'go' / 'yes' / 'approve' to spawn, or free text to revise.
```

回 `go` → Claude 调 native `TeamCreate` + 3 × `Task(team_name, name, prompt)` 并行 spawn。后续 SendMessage 通信全在该 session。

### 入口语法

```
/ccteam:team <task>                 # 自动决定 N + 角色
/ccteam:team N "<task>"             # 指定 N 个 teammate
/ccteam:team N:role "<task>"        # 指定 N 个 + 主角色(如 3:debugger)
/ccteam:team auto "<task>"          # auto 别名(同第一个)
```

### web 可视化(同步发生)

```
$ ccteam start    # 起 daemon(无 slug 参数,只跑全局 watcher + web)
# 浏览器开 http://localhost:7331/teams → 看 host 所有 team 实时
```

ccteam daemon 跑着的情况下,**任何** `/ccteam:team` 起的 team 都会自动在 web `/teams` tab 出现 — 3 面板(Topology / TaskBoard / Mailbox)实时更新。**这是 ccteam 跟 OMC 等竞品的核心差异** — 跨设备 + 长跑可视化 + 多 team 总览 + 未读消息高亮。

---

## 三、Advanced path:`mode: agent-team` workflow.yaml(automation)

适用场景:**用户长时间不在,机器自跑** N 天 / cron 每日触发 / hot-reload 团队拓扑。普通用户用不到。

### 起 team

```
$ ccteam init --mode agent-team my-debate
  ✓ Wrote my-debate/.ccteam/workflow.yaml (mode=agent-team)
  ✓ Wrote my-debate/.claude/agents/__lead.md
  ✓ Registered to ~/.ccteam/config.yaml

$ vim my-debate/.ccteam/workflow.yaml
  # 编辑 agent_team.lead_seed / suggested_teammates / budget.max_cost_usd_per_24h

$ ccteam start my-debate
  ✓ Loaded .ccteam/workflow.yaml — mode=agent-team
  ✓ Loaded .claude/agents/__lead.md — model=sonnet, tools=[...]
  ✓ Suggested teammates: 3 definition + 2 ad-hoc

  About to spawn lead session:
    claude --bg --agent __lead --env CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1

  Proceed? [Y/n/attach]
```

3 选择:
- **`Y`(默认)**— spawn bg lead,daemon 接管。打印 attach 命令 + web URL。
- **`n`** — 取消,什么都不动。
- **`attach`** — spawn 后立刻 `exec claude attach <lead-session-id>`,用户进入 lead 交互 session。Detach 后(`Ctrl+B D` / 空 prompt `←`)daemon 继续 bg 监听。

跳过 prompt 的 flag:`--no-confirm`/`-y`、`--attach`、`--dry-run`。

### 后续操作

```
ccteam attach my-debate                       # re-attach 已跑的 lead session
ccteam send my-debate "go"                    # 异步给 lead 写消息(不 attach)
ccteam web                                    # 开浏览器看 /teams/my-debate
ccteam stop my-debate                         # 依 workflow.yaml::cleanup_on_stop 处理
ccteam stop my-debate --stop-timeout 120      # F97 ask-lead 模式延长等待
ccteam start --restart-team my-debate         # F97 复活 detached / sleep-唤醒 后的 lead
```

### Plan-first Protocol(advanced path 默认开)

`auto_spawn_teammates: false`(workflow.yaml 默认)→ lead spawn 后第一条 message 必须是 `TEAM PLAN ===` 框 + STOP 等 user。User 回 `go` 才开始 spawn teammate。

熟用户 + 确定性任务:workflow.yaml 设 `auto_spawn_teammates: true` → lead 自决直接 spawn,但仍写 `.ccteam/outbox/team-bootstrap-<ts>.md` 留 audit log。

### V0.5.0 F97 — `cleanup_on_stop` 3 策略

`workflow.yaml::agent_team.cleanup_on_stop` 取三值之一,决定 `ccteam stop <slug>` 行为:

| 值 | 行为 | 用例 |
|---|---|---|
| `force-kill`(默认) | SIGKILL lead bg job + 清 snapshot;teammate 跟 lead 进程树死 | 一次性任务,跑完即销 |
| `ask-lead` | 写 user-turn 消息到 `.ccteam/inbox/<ts>-stop-request.md`;轮询 `progress.jsonl` 等 lead emit `workflow_done`,默认 60s timeout(`--stop-timeout <secs>` 覆盖);timeout 退化 force-kill + WARN | 想 graceful persist context(lead 自己整理 task / send 总结消息再退) |
| `leave-running` | 不 kill;清 ccteam 监听 + 设 `state.json::detached`;lead + teammate 继续跑 | 机器 sleep / 跨设备 / 跨 day token-maxxing |

`leave-running` + 后续 `ccteam start --restart-team <slug>` 是 V1.0.0 token-maxxing(用户长时间不在,机器自跑 N 天)的基础设施:

```
$ ccteam stop my-debate           # cleanup_on_stop: leave-running → 不 kill
  Lead session id: 8e4bab09-...
  Reconnect with `ccteam start --restart-team my-debate` or `claude attach 8e4bab09-...`

# 几小时 / 几天后机器唤醒:
$ ccteam start --restart-team my-debate
  ✓ team-snapshot.json found; lead_session_id=8e4bab09-...
  ✓ probe_job() == Running; lead bg job alive. Skipping spawn.
  → ccteam 重新接管监视(无新 spawn,无 context 浪费)
```

注:plain `ccteam start <slug>`(无 `--restart-team`)在 detached 项目上**拒绝**(避免起第二个 lead 跟原来的并存):

```
$ ccteam start my-debate
Error: project `my-debate` is in detached state (last `ccteam stop` used `cleanup_on_stop: leave-running`).
  To re-attach the existing lead: `ccteam start --restart-team my-debate`
  To force a fresh lead (and orphan the old one): edit state.json::detached → false, then re-run.
```

### V0.5.0 F97 — workflow.yaml hot-reload 约束

Agent-team mode workflow.yaml 改动 daemon 实时响应,但**只**对"hot"字段直接生效;改"cold"字段触发 `workflow_done reason="cold_reload_required"` event + 清 watcher,你必须显式 `ccteam start --restart-team` 才会起新 lead 匹配新拓扑。

| 字段 | HOT / COLD | 修改后行为 |
|---|---|---|
| `agent_team.lead_seed` | HOT | daemon 下个 tick 写 `.ccteam/inbox/<ts>-reload-update.md`,lead 下次拾取看到新方向 |
| `agent_team.teammate_mode` | HOT | env-only,记录但运行中不重启(下次 spawn 才生效)|
| `agent_team.cleanup_on_stop` | HOT | `ccteam stop` 时才读 |
| `agent_team.auto_spawn_teammates` | HOT | 下次 plan 时才读 |
| `suggested_teammates[].adhoc_color` / `.adhoc_tools` | HOT | cosmetic / web UI 元数据 |
| `agent_team.team_name` | **COLD** | 改 `~/.claude/teams/<>/` 目标 dir → 必须 fresh lead |
| `suggested_teammates[].role` / `.kind` / `.spawn_brief` | **COLD** | topology 变化 → 必须 fresh lead |
| `workflow.yaml::mode` | **COLD** | 工作模式切换 → 必须 fresh lead |

---

## 四、选型决策树

| 你的情况 | 用哪条 + 配置 |
|---|---|
| 在 project session 里临时想起几个 agent 协作 | Primary `/ccteam:team` |
| 已在 Claude session 里,不想切流程 | Primary |
| 要长时间不在,机器自跑数天 | Advanced + `cleanup_on_stop: leave-running` + 唤醒后 `ccteam start --restart-team` |
| 要 graceful persist context 再退 | Advanced + `cleanup_on_stop: ask-lead`(可选 `--stop-timeout`) |
| 要 hot-reload teammate **拓扑**(role/kind/spawn_brief 改) | Advanced + cold-reload 流程:改 workflow.yaml → 等 `workflow_done reason=cold_reload_required` → `ccteam start --restart-team` |
| 要 hot-reload **lead_seed**(任务方向变更) | Advanced — 直接编辑 workflow.yaml,daemon 下个 tick 自动写 lead inbox |
| 要 per-project budget cap 自动 disable | Advanced(F84 budget cap 仍是 ccteam-only 能力) |
| 要 cron / scheduled 触发 | Advanced |
| 关心 cost(实际跑了多少钱) | 任意路径,V0.5.0 F92 给真数据 |

---

## 五、Cost(F92 真数据源)

V0.5.0 切到 transcript jsonl 数据源(`linkScanPath`/`cwd+sessionId`),`state.json::cost_usd_total = 0` 不再坑人。

```
ccteam show my-project --cost     # 24h / 7d / lifetime + active session 累计
ccteam doctor --check-pricing-version   # 检查内嵌 pricing.json 是否过期
```

`pricing.json` 内嵌 binary(`include_str!`),覆盖 sonnet-4-6 / opus-4-7 / haiku-4-5 等。schema_version 字段;`doctor` 180 天 stale 触发 WARN(提示升级 ccteam binary)。

---

## 六、ccteam web `/teams` tab(F95 + F96)

V0.5.0 加新顶级 tab。任何 host `~/.claude/teams/<>/` 出现的 team(不限 ccteam-managed)自动可视化:

| 面板 | 数据源 | UI 特性 |
|---|---|---|
| **Topology** | `config.json::members[]` | 节点 = 每 teammate;头像色;model;agentType;**📝 ad-hoc** 徽章(`general-purpose`/`team-lead`)vs **↗ definition** 链接(其他 agentType,链 `.claude/agents/<role>.md`)|
| **TaskBoard** | `~/.claude/tasks/<team>/*.json` | Kanban 3 列(Pending / In Progress / Completed);拖动 owner / 依赖图标 |
| **MailboxStream** | `~/.claude/teams/<>/inboxes/*.json` | 时间线 from→to;**`read: false` 未读高亮**;`idle_notification` 分流到 Topology 状态徽章 |

**红线**:ccteam **只读** `~/.claude/teams/` + `~/.claude/tasks/` — Anthropic 是 SoT,绝不写。

---

## 七、Skill 速查(V0.5.0 3 个 skill)

| Skill | 用途 | 装到 |
|---|---|---|
| `ccteam-team` | `/ccteam:team` 在 session 里起 agent team(primary path) | `~/.claude/skills/ccteam-team/` |
| `ccteam-creator` | 新建 ccteam project(workflow.yaml + agents + skill scaffold)| `~/.claude/skills/ccteam-creator/` |
| `ccteam-control` | wrap ccteam CLI + 17 MCP 工具(`mcp__ccteam__*`),任意 session 可用 | `~/.claude/skills/ccteam-control/` |

V0.4.6 的 `ccteam-team-author` + `ccteam-project-creator` 已删(V0.5.0 F100):
- 起 team 改用 `/ccteam:team` skill
- 新 project 改用 `ccteam-creator` skill 或 `ccteam init --mode agent-team` CLI

**没有 deprecation alias** — `ccteam team init` / `ccteam team publish` 子命令直接删,不留警告。

---

## 八、Meta-agent 重定位(F101)

V0.2 的 meta-agent 是 "全权调度 + phase 流水线 + 项目创建" singleton。V0.5.0 重塑为**轻量 router + memory bridge + dashboard**:

| V0.5.0 meta-agent 不再做 | 改成 |
|---|---|
| 自己起 team(phase pipeline)| delegate 到 `/ccteam:team` skill(让用户在 project session 里跑) |
| 自己起 project(4-phase dialogue)| delegate 到 `ccteam-creator` skill |
| 自己跑 ccteam 命令 | 优先 delegate 到 `ccteam-control` skill;长查询走 web UI |
| Phase 调度 / Seed Gate / kickoff reverse interview | **删了**(V0.4 已删 phase 模型;V0.5.0 删残留) |
| Cross-project memory bridge(`memory_bridge_*.md`)| **保留**(memory 跨项目仍有用)|
| Dashboard chat("ccteam 现在咋样")| **保留**,但建议 web UI 作主入口 |

---

## 九、故障排除

| 症状 | 排查 |
|---|---|
| `/ccteam:team` 不出 | `ls ~/.claude/skills/ccteam-team/` 不存在 → `ccteam doctor --install-skill all` |
| Web `/teams` tab 空 | ccteam daemon 没跑(`ccteam start` 启 daemon)或 `~/.claude/teams/` 真没东西 |
| Cost 显示 0 | `ccteam doctor --check-pricing-version` 看 pricing 是否过期;`grep linkScanPath ~/.claude/jobs/<id>/state.json` 看 transcript 路径是否有值 |
| `ccteam init --mode agent-team` 报错 | Claude Code 版本不够 (≥ 2.1.32 需要),或 `agents/__lead.md` 模板缺(`ccteam doctor --install-agents`) |
| Team mailbox 消息没显示 | `~/.claude/teams/<>/inboxes/<teammate>.json` 文件是否 schema 改了?ccteam degrade 到 mtime-only,daemon log 有 WARN |
| `cargo test` 跑出 ~60 reqwest 502 | host 设了 `HTTP_PROXY` 转 localhost — `unset HTTP_PROXY HTTPS_PROXY` 再跑 |

---

## 十、升级 from V0.4.6

V0.4.6 → V0.5.0 **breaking change**(pre-v1.0,不留 alias):
- `ccteam team init/publish/show` CLI 子命令删了 → 改用 `ccteam-creator` skill 或 `ccteam init --mode agent-team`
- `~/.claude/skills/ccteam-team-author/` + `ccteam-project-creator/` 不再装 → `ccteam doctor --install-skill all` 装 V0.5.0 新 3 skill(老的可手动 `rm -rf` 或等 `ccteam doctor` F44 reverse-migration 清理)
- workflow.yaml `mode` 字段可选(`#[serde(default)]` → `artifact-driven`)— **V0.4.6 工程的 workflow.yaml 不需要任何改动**,跑 V0.5.0 binary 无缝

新感觉到的能力:
- `/ccteam:team` in Claude session
- `~/.claude/teams/` 全可视化(无论谁起的 team)
- Real cost(V0.4.6 显示永远 0;V0.5.0 实数据)
- meta-agent 不再"独占"项目创建 — 让 skill 接手

---

## 十一、V0.5.x 候选(延期)

详 `prd.md §V0.5.x 延期 finding 草案` + `orchestration-patterns.md §五`:
- F98 plan-approval ↔ outbox 联动(扩 F87 `intercept-ask`)
- F99 Claude Code 版本 gating(`doctor --check-agent-teams`)
- Routing(动态)sugar:`agent.router: <expr>`
- Evaluator-Optimizer 显式 sugar

---

## 十二、参考

- 官方:[Agent Teams](https://code.claude.com/docs/en/agent-teams) / [Subagents](https://code.claude.com/docs/en/sub-agents) / [Agent View](https://code.claude.com/docs/en/agent-view)
- ccteam tier-1:[`requirements.md`](../requirements.md) / [`orchestration-patterns.md`](../orchestration-patterns.md) / [`tech-design.md`](../tech-design.md) / [`interfaces.md`](../interfaces.md)
- V0.5.0 详:[`README.md`](README.md) / [`prd.md`](prd.md) / [`dev-plan.md`](dev-plan.md)
