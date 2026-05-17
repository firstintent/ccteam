# V0.5.0 — PRD

5 个 finding。F92 是 prerequisite,F93/F94/F95 是 agent-team mode 核心,F96 是用户感知层。

---

## F92 — 真 cost 数据源:linkScanPath jsonl `usage` 字段

### 痛点
V0.4.6 host E2E 实测发现 `~/.claude/jobs/<id>/state.json::cost_usd_total` 字段**始终为 0**(2026-05-16 dex-ui session 4h 跑 $1.10,state.json 仍 0)。真实 cost 数据在 Claude Code 的 `linkScanPath` 指向的 transcript jsonl 文件,每个 assistant turn 写 `usage` 字段(input_tokens / output_tokens / cache_creation_input_tokens / cache_read_input_tokens)。

ccteam `cost_summary`(`crates/ccteam-core/src/cost_summary.rs`)目前读 state.json 字段,结果是:
- F84 budget cap 永远不触发(cost 看着永远 0)
- F90 Cost Sparkline 永远空线
- workflow_done event 的 `cost_usd_accumulated` 假
- V0.5.0 agent-team mode 的 budget 边界对 teammate 完全失效

### 需求
`ccteam_core::cost_summary` 重构数据源:

1. **数据源**:`~/.claude/jobs/<id>/state.json::linkScanPath` 指向的 transcript jsonl 文件(每个 session 一个)
2. **解析**:遍历 assistant 消息的 `message.usage`,按 Anthropic price table(`pricing.json` 内嵌)算 dollar
3. **price table 来源**:仿 Anthropic 公开 pricing 页;内嵌 ccteam binary(`include_str!`),版本字段 → `ccteam doctor --check-pricing-version` 提示用户升级 ccteam(不在线拉)
4. **回退**:linkScanPath 缺失或 transcript jsonl 不存在 → 退回读 state.json `cost_usd_total`(老路径),写日志 WARN 但**不 fail**
5. **缓存**:同一 session id 的 jsonl 文件按 mtime + 长度 memoize,避免每秒 `cost_summary` 调用都重扫整个 jsonl

### 验收
1. host probe:dex-ui 4h $1.10 session,`ccteam show dex-ui --cost` 显示真实 $1.10 ± 5%
2. F84 budget cap:`max_cost_usd_per_24h: 0.10` 设极小阈值跑测试 → 触发 auto-disable workflow + workflow_done reason="budget_exceeded"
3. Cost Sparkline:24h / 7d 曲线非空,有真实分桶
4. `state.json::cost_usd_total` 缺失但 linkScanPath 存在 → cost 正常算出,日志无 ERROR
5. linkScanPath 缺失 → 退回 state.json,日志 WARN 一次,后续 cost 是 0(不假装)
6. pricing table 版本:测试覆盖 sonnet-4 / opus-4.6 / opus-4.7 / haiku-4.5,数字跟 Anthropic 公开 pricing 差 ≤1 cent

---

## F93 — workflow.yaml `mode: agent-team` schema + `__lead` role 工厂

### 痛点
官方 Agent Teams 起团队靠用户每次手敲自然语言:
```
"I'm designing a CLI tool... Create an agent team to explore this from
different angles: one teammate on UX, one on technical architecture,
one playing devil's advocate."
```

每次重起会话都要重打;无版本控制;teammate 拓扑漂移;无法在多项目间复用模板。

### 需求
`workflow.yaml` 加 `mode` 顶层字段(`#[serde(default = "default_mode_artifact_driven")]`),取值 `artifact-driven` / `agent-team`。

#### Schema(完整示例)

```yaml
name: flaky-test-debate
mode: agent-team
budget:
  max_cost_usd_per_24h: 10.00

agent_team:
  lead_seed: |
    Investigate why integration tests in src/auth/ flake intermittently.
    Spawn teammates per workflow.yaml agents:; have them debate
    competing hypotheses; converge on root cause; require plan approval.
  teammate_mode: tmux                # in-process | tmux | auto
  default_model: sonnet              # 选填,缺省 inherit lead 的 /model
  require_plan_approval: true        # lead 要审 teammate plan
  cleanup_on_stop: force-kill        # MVP 只支持 force-kill

agents:
  auth-expert:
    teammate_name: auth-expert       # 选填,默认 = role name
  network-expert: {}
  db-expert: {}
  test-runner: {}
  devil-advocate: {}
```

#### `__lead` role 模板

V0.5.0 加 `agents/__lead.md`(repo 根,沿用 F89 explorer.md scaffold pattern):

```markdown
---
name: __lead
description: |
  ccteam-managed lead session for agent-team mode workflows. On spawn,
  reads .ccteam/workflow.yaml::agent_team.lead_seed and creates a team
  per workflow.yaml::agents map.
tools: Read, Bash, SendMessage, TaskCreate, TaskList, TaskUpdate, Spawn
model: claude-sonnet-4-6[1m]
color: orange
---

You are the team lead for a ccteam-managed agent-team workflow. ...
```

`ccteam init --mode agent-team <slug>` 工厂:
1. 写 `<project>/.ccteam/workflow.yaml`(从 `crates/ccteam-core/src/templates/workflow.agent-team.yaml` 模板)
2. 写 `<project>/.claude/agents/__lead.md`(从 `agents/__lead.md` `include_str!`)
3. **不写**业务 teammate `.md` — 由用户在 dialogue 中由 `ccteam-creator` skill 加(沿用 F89 模式)

#### orchestrator 行为
- 解析 `mode: agent-team` → 跳过 ArtifactWatcher 安装,改装"lead 单 session 看护"
- spawn lead:`claude --bg --agent __lead --env CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1 --env CLAUDE_CODE_TEAMMATE_MODE=<mode>` + 把 `lead_seed` 作为初始 user-turn message 写入 lead 的 `~/.claude/jobs/<id>/inputs/`(或 stdin pipe)
- lead 自己 spawn teammate;ccteam 不干预 teammate 拓扑
- lead 退出(状态 done / failed)→ workflow_done event reason="lead_exited"

### 验收
1. `ccteam init --mode agent-team my-debate` 生成 4 个文件:`.ccteam/workflow.yaml`(mode=agent-team)+ `.claude/agents/__lead.md` + `.ccteam/inbox/`(空目录,沿用)+ 注册到 `~/.ccteam/config.yaml`
2. `ccteam start` 后 `ccteam show my-debate` 显示 lead session id + state=working
3. lead session 接收到 `lead_seed` 作为 user-turn(verify via `claude attach <lead-id>` 看 transcript)
4. lead spawn teammate 后 `~/.claude/teams/my-debate/config.json` 出现,members 数 == workflow.yaml agents 数 + 1(lead)
5. teammate 跑 `claude --bg` 在自己 `~/.claude/jobs/<id>/` 起 process(F92 cost 跟踪到位)
6. workflow.yaml 改 `enabled: false`(F82 hot-reload)→ lead 收 cancel token graceful exit + workflow_done reason="disabled"
7. `mode` 字段缺失 → 解析为 `artifact-driven`,完全向后兼容 V0.4.6

### 红线
- `__lead` 是 ccteam-managed role;**用户不应该写自己的 `__lead.md`**;`ccteam doctor --validate-team` 警告若发现用户改了 `__lead.md` body
- `lead_seed` 是 user-turn message,**不是 system prompt** — 守 CLAUDE.md §三 "永不向 session 注入 system prompt"

---

## F94 — Agent Teams 3 hook 镜像 + 5 新 event 类型

### 痛点
官方 Agent Teams 的 3 hook(`TeammateIdle` / `TaskCreated` / `TaskCompleted`)是 ccteam 唯一能可靠观察 team 内部事件的接口。不装这 3 hook,ccteam 只能扫 `~/.claude/teams/<>/config.json` mtime,丢细粒度。

### 需求
`crates/ccteam-core/src/templates/settings.json` 加 3 hook(仅 agent-team mode 项目装,artifact-driven 项目不装):

```json
"TeammateIdle": [
  {"hooks": [{"type": "command", "command": "__CCTEAM_BIN__ internal hook progress-append team_teammate_idle", "async": true}]}
],
"TaskCreated": [
  {"hooks": [{"type": "command", "command": "__CCTEAM_BIN__ internal hook progress-append team_task_created", "async": true}]}
],
"TaskCompleted": [
  {"hooks": [{"type": "command", "command": "__CCTEAM_BIN__ internal hook progress-append team_task_completed", "async": true}]}
]
```

`progress.jsonl` SoT 加 5 类新 event:

| event | 来源 | payload |
|---|---|---|
| `team_member_joined` | F95 watcher 监 `~/.claude/teams/<>/config.json` mtime → diff members | `{ teammate_name, agent_id, agent_type, started_at }` |
| `team_message_sent` | F95 watcher 扫 teammate transcript jsonl 找 SendMessage tool_use | `{ from, to, summary }`(摘要由 Haiku 一句话,避免泄露全文) |
| `team_task_created` | Hook | `{ task_id, title, assignee?, dependencies[] }` |
| `team_task_completed` | Hook | `{ task_id, result_summary }` |
| `team_teammate_idle` | Hook | `{ teammate_name, reason }` |

`ccteam-core/src/orchestrator.rs::Event` enum 加 5 变体(`#[serde(rename = "team_*")]`)。

### 验收
1. agent-team mode 项目 `.claude/settings.json` 含 3 个新 hook;artifact-driven 项目不含
2. lead 创建 1 task → `progress.jsonl` 出现 `{"event": "team_task_created", ...}` 一行
3. teammate 完成 task → `team_task_completed` 一行,`task_id` 同 created 的 id 一致
4. teammate idle → `team_teammate_idle` 一行,`reason` 字段非空
5. 5 个 event 全在 `interfaces.md §6.4`(progress.jsonl event 表)更新
6. 老的 7 event(F60+)不破:agent-team 项目跑 artifact 模式 hook 失败 不影响 artifact-driven 项目

---

## F95 — ArtifactWatcher 扩展到 `~/.claude/teams/` + `~/.claude/tasks/`

### 痛点
F94 三 hook 只覆盖 `Teammate*` / `Task*` 事件;**teammate 之间 SendMessage 没有官方 hook**(只在 lead 的 transcript jsonl 出现)。ccteam 需要扫 transcript jsonl 才能镜像 mailbox。

且 `~/.claude/teams/<>/config.json` 是团队拓扑的 SoT(members 列表),mtime 变化要被 ccteam 立即知道。

### 需求
扩展 `crates/ccteam-core/src/artifact_watcher.rs`:

1. **新 watch target**(`mode: agent-team` 项目才装):
   - `~/.claude/teams/<workflow-name>/config.json`(file watch,mtime+content hash)
   - `~/.claude/tasks/<workflow-name>/*.json`(directory watch,新文件 + modify)
   - 每个 teammate 的 transcript jsonl(动态加 watch,teammate spawn 后由 hook 通知 ccteam 加 watch;teammate 退出后 remove watch)

2. **handler**:
   - `config.json` 变 → diff members 数组 → 发 `team_member_joined` 或 `team_member_left` event
   - `tasks/*.json` 新增 → 解析后写 `team_task_created`(冗余 hook,作为 safety net;hook 失败仍可恢复)
   - transcript jsonl tail → 找 `tool_use: SendMessage` block → 提取 `to` / `body`,起一个 Haiku one-shot 总结 body → 写 `team_message_sent` event

3. **只读**:**绝不写**任何 Anthropic-owned 文件(`~/.claude/teams/` + `~/.claude/tasks/`);官方明确警告"don't edit by hand"

### 验收
1. agent-team mode workflow 起后,inotify 表里出现 3 类新 watch
2. lead spawn 1 teammate → `team_member_joined` event 5s 内落 `progress.jsonl`
3. teammate A `SendMessage` to teammate B → `team_message_sent` event 落,from/to/summary 字段非空
4. hook 失败(测试时 deliberately 杀 hook process)→ `tasks/*.json` 仍能被 watcher fallback 捕获
5. `~/.claude/teams/<>/config.json` schema 改(模拟 Anthropic 升级)→ ccteam 解析失败时**WARN 而非 panic**,镜像 degrade 到 mtime-only

---

## F96 — Web SPA 3 新面板

### 痛点
Agent Teams 自带 UI(in-process Shift+Down 切换 / tmux 分屏)在长跑场景下不好用:
- 关掉终端就看不到团队状态
- 历史 task 完成后从 list 滚走
- 多 team 并跑没有总览
- mailbox 消息没法回看

### 需求
ccteam web SPA 在 `mode: agent-team` workflow 详情页加 3 面板(artifact-driven 页保持现状):

#### 1. Team Topology 面板
节点图:lead 居中,N teammate 围绕。每节点显示:
- `name`(workflow.yaml `agents.<role>.teammate_name`)
- 头像色(`color` frontmatter,沿用 V0.4.0 模式)
- model(从 `~/.claude/jobs/<id>/state.json` 读)
- 当前 activity(Haiku 摘要,跟 agent view 同源)
- cost(F92 数据源)
- 状态色:working / waiting-input / idle / completed / failed

边线条颜色按消息流频次,鼠标 hover 显示最近 SendMessage 摘要。

#### 2. Task Board 面板
Kanban 3 列:Pending / In Progress / Completed。每个 task 卡显示:
- task title
- assignee teammate(头像)
- 依赖图标(若 `dependencies[]` 非空)
- 创建时间 + 完成时间(若已完成)

点击 task 卡 → 展开 task body + 历史相关 message。

#### 3. Mailbox Stream 面板
时间线,from → to 消息流。支持:
- 按 teammate 对过滤
- 按时间倒序 / 正序
- 搜索消息摘要
- 折叠老消息(>1h)

#### SSE wiring
复用 F90 SSE 推送基础设施(`/api/v1/projects/<slug>/events`),不开新通道。3 面板订阅同一 SSE stream,前端按 event type 路由更新。

### 验收
1. agent-team workflow 起后 web SPA 详情页显示 3 个新面板;artifact-driven 工作流页面不变(SPA 检测 `mode` 字段路由)
2. Team Topology:lead spawn 5 teammate → 节点图 5s 内显示 6 个节点 + 边线连接
3. Task Board:lead create 3 task → Pending 列出现 3 卡;teammate 完成 1 个 → 该卡移到 Completed 列
4. Mailbox:teammate A → B 发消息 → Stream 出现一行,可点击展开
5. SSE 断线 → 面板显示"reconnecting...",5s 内自动重连,无数据丢失(后端从 last_event_id resume)
6. 跨浏览器:Chrome / Firefox / Safari 最新版 + iOS Safari(响应式)

---

## V0.5.x 延期 finding 草案

### F97 — Lifecycle 完善
- `cleanup_on_stop: ask-lead` — `ccteam stop` 给 lead 发 user-turn "Clean up the team",lead 走 native cleanup 流程
- `cleanup_on_stop: leave-running` — `ccteam stop` 只断 ccteam 监听,lead + teammate 继续跑(用户可后续 `claude attach` 或 `ccteam start --reconnect`)
- `--restart-team` — 机器 sleep 后 `claude respawn --all` 不够(Agent Teams 限制),ccteam 重起 lead + 用 last team config 重建 teammate
- hot-reload 约束:`agent_team.teammate_mode` / `default_model` / `lead_seed` 可热改;`agents` 拓扑改强制 force-kill + 重起 team

### F98 — plan-approval ↔ outbox 联动
扩 F87 `intercept-ask` hook 覆盖 lead 的 plan-approval 决策:
- 默认 lead autonomously decides(MVP 行为)
- 设 `agent_team.plan_review_mode: user-via-outbox` → lead 收到 teammate plan 时,**不自决**,改写 `.ccteam/outbox/plan-approval-<teammate>-<ts>.md` → 用户在 web SPA / `ccteam send <slug> approve|reject:<reason>` 决策

### F99 — Claude Code 版本 gating
- `ccteam doctor --check-agent-teams` 检测 `claude --version` ≥ 2.1.32(`CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS` 引入版本)
- `ccteam init --mode agent-team` 若版本不够 → refuse + 提示用户升级 Claude Code
- pricing version check(F92 内嵌 pricing.json 版本)半年 stale 触发 WARN

---

## V0.5.0 整体红线

1. **agent-team mode 不进 ccteam-core 控制平面** — Rust 进程只 spawn lead + 装 hook + 镜像事件;**不**模拟 lead 行为,**不**直接写 `~/.claude/teams/` 或 `~/.claude/tasks/`
2. **`progress.jsonl` 仍是 ccteam SoT** — Anthropic 文件是 ccteam 镜像源,不是 SoT;5 个新 event 类型沿用 7 类 + ts/event/role 结构
3. **`__lead` 是 ccteam-managed 系统 role** — 用户改 body ccteam doctor 报警告;但不强制 forbid,留 escape hatch
4. **lead_seed 是 user-turn 不是 system prompt** — 守 CLAUDE.md §三"orchestrator 永不向 session 注入 system prompt"
5. **F92 数据源切换不破现有调用** — `cost_summary` 函数签名不变,内部数据源切;state.json fallback 保留;调用方(F84 budget / F90 web)无需改
6. **artifact-driven mode 不变** — `mode` 字段缺失 = 默认 artifact-driven;V0.4.6 全部 workflow.yaml 文件不需要任何改动就能跑 V0.5.0 binary
