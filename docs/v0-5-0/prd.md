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

每次重起会话都要重打;无版本控制;teammate 拓扑漂移;无法在多项目间复用模板。**且**关闭终端后没有长跑可视化,history 滚走就找不回(F96 MVP 已解后半句)。

### 设计原则:Anthropic 是 SoT,ccteam 读不写

实地探测 host `~/.claude/teams/roblog/config.json` 发现 Anthropic 已经把完整 team 元数据落到文件(name / description / leadAgentId / leadSessionId / members[] + 每 member 的 agentId/name/color/agentType/model/prompt/cwd/planModeRequired/backendType/tmuxPaneId/subscriptions)。

→ **ccteam 不复刻 member 拓扑**。workflow.yaml `agent_team.agents:` 是给 lead 看的**启动意图**(初始 brief),`~/.claude/teams/<team_name>/config.json` 是**运行时 SoT**。lead 可中途增删 member、改 model、调 prompt,ccteam 通过 F95 watcher 镜像变化,**绝不写**这份文件。

### 需求
`workflow.yaml` 加 `mode` 顶层字段(`#[serde(default = "default_mode_artifact_driven")]`),取值 `artifact-driven` / `agent-team`。

#### Schema(完整示例)

```yaml
name: flaky-test-debate                # ccteam project slug
mode: agent-team
budget:
  max_cost_usd_per_24h: 10.00

agent_team:
  team_name: flaky-test-debate         # 必填,= ~/.claude/teams/<team_name>/ dir 名
  lead_seed: |                         # user-turn message,不是 system prompt
    Investigate why integration tests in src/auth/ flake intermittently.
    Spawn the suggested teammates below; have them debate competing
    hypotheses; require plan approval before any teammate writes code.
  teammate_mode: in-process            # in-process | tmux | auto;写到 lead 的 env CLAUDE_CODE_TEAMMATE_MODE
  cleanup_on_stop: force-kill          # MVP 只支持 force-kill;F97 加 ask-lead / leave-running
  snapshot_path: .ccteam/team-snapshot.json  # F93 stickiness — workflow.yaml 解析后冻结,team 生命周期内不重读

  # 建议的 teammate 列表(可选)— lead 按 Anthropic 两类 teammate spawn pattern 选择:
  suggested_teammates:
    # ---- definition-backed teammate ----
    # 引用 .claude/agents/<role>.md 文件;Claude 自动:
    #   - 用 frontmatter 的 tools 限制 teammate 工具(SendMessage/TaskCreate 等团队工具始终保留)
    #   - 用 frontmatter 的 model 跑 teammate
    #   - 把 .md body **append** 到 teammate system prompt(不是替换)
    #   - skills + mcpServers frontmatter **不生效**(teammate 走 project/user settings)
    - role: code-reviewer              # → .claude/agents/code-reviewer.md 必须存在
      kind: definition
      spawn_brief: |                   # spawn 时给 teammate 的 task description(append 在 .md body 之后)
        Review the auth changes specifically; security focus.

    # ---- ad-hoc teammate ----
    # 没有对应 .md 文件;lead 临时写完整 prompt(roblog team 全部 5 个 member 都是这类)
    # config.json::members[i].agentType 通常是 "general-purpose"
    - role: db-expert
      kind: ad-hoc
      spawn_brief: |
        You are a PostgreSQL expert investigating auth_sessions table
        for race conditions. Tools: Read, Bash, Grep. Report findings
        via SendMessage to team-lead.
      adhoc_model: sonnet              # ad-hoc 必须自己指定 model(没 .md frontmatter)
      adhoc_color: purple              # 可选,UI 显示用
      adhoc_tools: [Read, Grep, Bash]  # 可选;默认 lead's permission inheritance

  # 全部省略 suggested_teammates 也合法 — lead 完全从 lead_seed 自然语言决定 team 组成。
  # 但 V0.5.0 推荐 declarative 写法以获得 hot-reload 时的 diff 计算 + 跨 workflow 复用。
```

**注意 schema 关键决策**:
- `team_name` 必填,跟 Anthropic dir 1:1 绑定;不同名 ccteam 找不到 SoT
- `suggested_teammates` 可选;省略 = lead 自由决定;给了 = lead 优先按列表 spawn(可微调)
- `kind: definition` vs `kind: ad-hoc` 显式标注 — 让 F95 watcher 知道该 member 是否会有 `.claude/agents/<role>.md` 作为 prompt 来源
- `snapshot_path` 借鉴 OMC `resolved_routing` stickiness — workflow.yaml 改动**不影响**跑着的 team(F82 hot-reload 在 agent-team mode 走 force-restart)
- 不出现 `default_model` / `require_plan_approval` — 留 lead 在 lead_seed 决定

#### Definition-backed vs Ad-hoc 决策树

| 选 definition 用于 | 选 ad-hoc 用于 |
|---|---|
| 跨 workflow 复用的 role(如 `code-reviewer`, `security-reviewer`, `test-engineer`) | 一次性 / 项目特化的 role(如 `roblog-pm`, `flaky-test-debugger`)|
| 想让 ccteam web Topology 链接到 .md 文件 | role 行为完全在 spawn_brief 里写清楚 |
| 想跟 Anthropic `claude /agents` 命令同源管理 | 不想为 one-shot teammate 维护 .md 文件 |
| 想让 frontmatter `tools` 强制约束 | 想 inherit lead's permissions |

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
3. **不写**业务 teammate `.md` — 由用户决定 definition 还是 ad-hoc:
   - 想用 definition-backed 的 role(如 `code-reviewer`)→ 用 `ccteam-creator` skill 在 `.claude/agents/code-reviewer.md` 写 frontmatter + body(沿用 F89 模式)
   - 想用 ad-hoc 的 role → 在 workflow.yaml `suggested_teammates` 里直接写 `kind: ad-hoc` + `spawn_brief`,不开 .md 文件

`__lead.md` 系统 prompt 必须明确告诉 lead 两种 spawn pattern(F93 核心交付):

```markdown
# `__lead.md` body 节选(模板示例)

You are the team lead. Your project is at `<cwd>`. workflow.yaml is at
`.ccteam/workflow.yaml`. Read it to discover `agent_team.suggested_teammates`.

For each entry in suggested_teammates:

- If `kind: definition`:
    - Verify `.claude/agents/<role>.md` exists (project / user / plugin scope).
    - Spawn via Task tool with `subagent_type: "<role>"`. Append the
      `spawn_brief` as additional instructions in the prompt (Claude
      already appends the .md body, you only add task-specific brief).
    - Honor frontmatter tools / model — DO NOT override unless user
      explicitly asks. Note: skills / mcpServers in frontmatter are
      ignored when running as teammate (teammate uses project/user
      settings instead).

- If `kind: ad-hoc`:
    - Generate the full system prompt for this teammate inline,
      combining `spawn_brief` with team protocol boilerplate (see Worker
      Preamble section below — adapted from OMC team SKILL.md).
    - Spawn via Task tool with `subagent_type: "general-purpose"` and
      `model: "<adhoc_model>"` / `tools: <adhoc_tools>` / your generated
      prompt. ccteam web Topology will tag this teammate as "ad-hoc"
      (no .md link).

Worker Preamble (always include in ad-hoc teammate prompts):
- "You are a TEAM WORKER, not a leader. NEVER spawn sub-agents."
- "ALL progress goes via SendMessage to team-lead."
- "When blocked or idle, send `idle_notification` and wait."
- "When done, set task status: completed via TaskUpdate."
- [borrow OMC §"Agent Preamble" 30-line pattern verbatim]

## Plan-first Protocol (CRITICAL — user-in-control)

If `agent_team.auto_spawn_teammates: false` (the default), you MUST
output a team plan as your VERY FIRST assistant message and then STOP.
Do NOT call the Task tool yet.

Template for your first message:
```
TEAM PLAN
=========
Proposed teammates:
  1. <role> (<kind>, model=<X>, color=<Y>) — <one-line brief>
  2. ...

Spawn order: <sequential | parallel>
Plan-approval policy: <require | autonomous>
Rationale: <why these roles, why this composition>

WAITING for user confirmation. Reply with:
  - "go" / "yes" / "approve"  → spawn teammates per plan
  - free text                  → revise plan based on feedback
  - silence (10 min default)   → I will write ESCALATE to outbox
```

After outputting the plan, do NOT call Task or any other action tool.
Wait for the next user-turn message. User can reply via:
  - `ccteam attach <slug>` and typing directly (interactive)
  - `ccteam send <slug> "go"` (async, written to your inbox)
  - V0.5.x F98: web SPA "Approve plan" button (writes to outbox)

Only after the user replies with approval (or explicit revision instructions
followed by another approval round) MAY you start calling Task.

If `agent_team.auto_spawn_teammates: true`, skip this protocol — proceed
directly to spawning per lead_seed + suggested_teammates.
```

#### orchestrator 行为 — user-controlled spawn flow

`ccteam start <slug>` 改为 **user-in-control** UX(不是无声后台启动):

```
$ ccteam start flaky-test-debate
  ✓ Loaded .ccteam/workflow.yaml — mode=agent-team, team_name=flaky-test-debate
  ✓ Loaded .claude/agents/__lead.md — model=sonnet, tools=[...]
  ✓ Suggested teammates: 3 definition + 2 ad-hoc

  About to spawn lead session:
    claude --bg --agent __lead \
      --env CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1 \
      --env CLAUDE_CODE_TEAMMATE_MODE=in-process
    Initial user-turn (lead_seed):
      "<truncated to 200 char>"

  Proceed? [Y/n/attach]
```

3 choice:
- **`Y`(默认)**:spawn → 打印 attach 命令 + web URL → daemon 接管。User-in-control:用户**明确确认**了 spawn 动作,不是 ccteam 单方决定。
- **`n`**:取消,什么都不动。
- **`attach`**:spawn 后 ccteam 进程 exec `claude attach <lead-session-id>`,用户立刻进入 lead 交互 session 看初始化。`Ctrl+B D`(in-process)/ `←` 空 prompt(claude native)detach 后 daemon 接管,lead 继续 bg 跑。

Spawn 完成后输出:
```
  ✓ Lead session spawned: 8e4bab09-...
  Manage the team with:
    ccteam attach flaky-test-debate     # re-attach interactive
    ccteam send flaky-test-debate "..."  # message without attaching
    ccteam web                          # http://localhost:7331/teams/flaky-test-debate
    ccteam stop flaky-test-debate       # cleanup team + lead
```

CLI flags 跳过确认(脚本化):
- `--no-confirm` / `-y`:跳过 prompt,默认 `Y` 行为
- `--attach`:跳过 prompt,直接走 `attach` 行为
- `--dry-run`:只打印将执行的命令 + suggested teammates,不 spawn

Orchestrator 内部:
- 解析 `mode: agent-team` → 跳过 ArtifactWatcher 安装,改装"lead 单 session 看护"
- spawn lead:`claude --bg --agent __lead --env CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1 --env CLAUDE_CODE_TEAMMATE_MODE=<mode>` + 把 `lead_seed` 作为初始 user-turn message 写 lead 的 `~/.claude/jobs/<id>/inputs/`(或 stdin pipe)
- lead 启动后 plan-first protocol 默认开 — lead 输出 plan 后**停**等用户回复
- lead 自己 spawn teammate(plan 批准后);ccteam 不干预 teammate 拓扑
- lead 退出(状态 done / failed)→ workflow_done event reason="lead_exited"

#### `ccteam attach <slug>` 新命令(F89 用户面命令族补)

```
$ ccteam attach flaky-test-debate
  ✓ Lead session id: 8e4bab09-...
  ✓ Status: waiting_for_plan_approval (since 2 min ago)
  Attaching... (Ctrl+B D or ← on empty prompt to detach back to background)

  [interactive Claude session UI]
```

实现:
- 读 `<project>/.ccteam/team-snapshot.json::lead_session_id`(F93 snapshot)
- exec `claude attach <id>`(官方 Agent View 命令)
- artifact-driven mode 跑此命令 → friendly error "artifact-driven mode has no single lead; use `ccteam show <slug>` to list active sessions"

`ccteam-cli/src/commands.rs::run_attach` 加分支判断:
```rust
match workflow.mode {
    WorkflowMode::AgentTeam => {
        let lead_id = read_lead_session_id(slug)?;
        Command::new("claude").args(["attach", &lead_id]).exec();
    }
    WorkflowMode::ArtifactDriven => bail!("artifact-driven mode has no lead"),
}
```

#### Schema 加一项

```yaml
agent_team:
  ...
  auto_spawn_teammates: false   # 默认 false → lead 走 plan-first protocol(用户确认才动)
                                # true → lead 看 lead_seed 自决直接 spawn(熟用户 + 确定性任务)
```

红线:即使 `auto_spawn_teammates: true`,lead 仍需在 `.ccteam/outbox/team-bootstrap-<ts>.md` 写一份"团队已起,以下为 spawn 列表" 给用户事后追溯;web SPA Topology 把这视为 audit log。

### 验收
1. `ccteam init --mode agent-team my-debate` 生成 4 个文件:`.ccteam/workflow.yaml`(mode=agent-team)+ `.claude/agents/__lead.md` + `.ccteam/inbox/`(空目录,沿用)+ 注册到 `~/.ccteam/config.yaml`
2. `ccteam start my-debate` 触发 confirm prompt(`[Y/n/attach]`);`Y` → spawn + 打印 attach 命令;`attach` → spawn + 直接 exec `claude attach <id>`;`n` → 取消无副作用
3. `--no-confirm` / `-y` 跳过 prompt;`--attach` 直接走 attach 路径;`--dry-run` 只打印不 spawn
4. spawn 后 `ccteam show my-debate` 显示 lead session id + state=`waiting_for_plan_approval`(plan-first protocol)
5. `ccteam attach my-debate` 解析 lead_session_id 并 exec `claude attach <id>`;artifact-driven mode 报 friendly error
6. **plan-first 验证**:lead spawn 后 5 分钟内 attach,lead 第一条 message 必须是"TEAM PLAN ===" 格式且 `~/.claude/teams/<>/config.json::members[]` 只含 lead 一个 entry(teammate 没起)
7. 用户回复 `go` / `ccteam send my-debate "go"` → lead 收到后开始 spawn teammate;`~/.claude/teams/<>/config.json` 5s 内出现新 members
8. `auto_spawn_teammates: true` workflow.yaml → lead 不停 plan,直接 spawn,但 `.ccteam/outbox/team-bootstrap-<ts>.md` 文件落地(audit log)
9. lead spawn teammate 后 `~/.claude/teams/my-debate/config.json` 出现,members 数 == workflow.yaml suggested_teammates 数 + 1(lead)
10. teammate 跑 `claude --bg` 在自己 `~/.claude/jobs/<id>/` 起 process(F92 cost 跟踪到位)
11. workflow.yaml 改 `enabled: false`(F82 hot-reload)→ lead 收 cancel token graceful exit + workflow_done reason="disabled"
12. `mode` 字段缺失 → 解析为 `artifact-driven`,完全向后兼容 V0.4.6

### 红线
- `__lead` 是 ccteam-managed role;**用户不应该写自己的 `__lead.md`**;`ccteam doctor --validate-team` 警告若发现用户改了 `__lead.md` body
- `lead_seed` 是 user-turn message,**不是 system prompt** — 守 CLAUDE.md §三 "永不向 session 注入 system prompt"
- **User-in-control 红线**:`ccteam start` 默认 confirm prompt;脚本化要显式 `-y`。不允许"silent spawn"或"启动时无任何输出后直接进入 daemon"。
- **Plan-first 红线**:`auto_spawn_teammates: false` 是默认值;改 true 必须显式声明(不能 omit 字段隐式 true)。`__lead.md` body 必须包含 plan-first 协议(`ccteam doctor --validate-team` 检查 hash)。

---

## F94 — Agent Teams 3 hook 镜像 + 精度提升

### 痛点
F95 watcher 能拿到 4 类 event(member_joined/left, message_sent, task_created/completed),但**`team_teammate_idle` watcher 拿不到** — Anthropic 的 idle 是 in-memory 状态,只通过 `TeammateIdle` hook 通知。idle 是 lead 判断"team 整体卡住"的关键信号(所有 teammate 都 idle 而 task 没完 → lead 该干预)。

且 hook 比 watcher 快 — task created/completed 通过 hook 是 0-延迟,通过 watcher 要等 inotify event(通常 <100ms 但偶有 lag)。

### 需求
`crates/ccteam-core/src/templates/settings.json` 加 3 hook,**仅 ccteam-spawned `__lead` session 的项目装**(F93 工厂条件渲染);interactive team(用户自起)没装 hook,ccteam 自动走 F95 watcher fallback,功能 degrade 但不挂。

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

`progress.jsonl` 6 类 team event(F95 提供 5 类 + 本 F94 加 1 类):

| event | 优先来源 | fallback | 备注 |
|---|---|---|---|
| `team_member_joined` | F95 config.json watcher | — | watcher only(没对应 hook)|
| `team_member_left` | F95 config.json watcher | — | watcher only |
| `team_message_sent` | F95 inboxes/<teammate>.json watcher | — | watcher only |
| `team_task_created` | F94 `TaskCreated` hook | F95 tasks/ watcher | hook 优先,缺失 fallback |
| `team_task_completed` | F94 `TaskCompleted` hook | F95 tasks/ watcher 检测 status 变化 | hook 优先,缺失 fallback |
| `team_teammate_idle` | F94 `TeammateIdle` hook | — | hook only(watcher 拿不到 in-memory idle 信号)|

`ccteam-core/src/orchestrator.rs::Event` enum 加 6 变体(`#[serde(rename = "team_*")]`)。

### 验收
1. ccteam-managed agent-team workflow(F93 spawn 出的 __lead session)→ `.claude/settings.json` 含 3 个新 hook
2. interactive team(用户自起,F93 不参与)→ `.claude/settings.json` 不含新 hook;F95 watcher 仍能拿 5 类 event
3. lead `TaskCreate` → `progress.jsonl` 出现 `team_task_created` 一行,延迟 <50ms(对比 watcher ~100-200ms)
4. teammate `TeammateIdle` → `team_teammate_idle` event 出现;F95 watcher 不会重复 emit(去重 by event_id + ts)
5. hook 失败(测试时 deliberately 杀 hook process)→ F95 watcher fallback 接管 `team_task_created` / `team_task_completed`(idle 没 fallback,degrade)
6. 6 个 event 全在 `interfaces.md §6.4` event 表更新
7. 老 7 event(F60+)不破:agent-team hook 失败不影响 artifact-driven workflow

---

## F95 — ArtifactWatcher 扩展 — 读取 `~/.claude/teams/` SoT(MVP 核心)

### 痛点
官方 Agent Teams 已经把完整 team 元数据落到文件,但没有可视化/长跑监听机制 — 关闭终端就看不到。ccteam 直接读这些文件就能做长跑可视化,**无需**等 F93 factory / F94 hook 注入。

### 设计原则
`~/.claude/teams/<>/config.json` + `inboxes/` + `~/.claude/tasks/<>/` = **Anthropic SoT**;ccteam **只读**镜像到 `progress.jsonl`(ccteam SoT)。

### Anthropic 实地 schema(host roblog team probe)

**`~/.claude/teams/<team_name>/config.json`** — 团队元 SoT:
```json
{
  "name": "roblog",
  "description": "...",
  "createdAt": <epoch_ms>,
  "leadAgentId": "team-lead@roblog",
  "leadSessionId": "<uuid>",
  "members": [
    {
      "agentId": "<role>@<team>",
      "name": "<role>",
      "color": "blue|green|yellow|purple|...",
      "agentType": "team-lead|general-purpose|<subagent-name>",
      "model": "sonnet|opus|<arbitrary-string>",
      "prompt": "<full-system-prompt-string>",
      "cwd": "<absolute-path>",
      "tmuxPaneId": "in-process|<pane-id>",
      "subscriptions": ["<teammate-name>", ...],
      "joinedAt": <epoch_ms>,
      "backendType": "in-process|tmux",
      "planModeRequired": false
    }
  ]
}
```

**`~/.claude/teams/<team_name>/inboxes/<teammate>.json`** — per-teammate 收件箱(**单 JSON 文件,不是目录**):
```json
[
  {
    "from": "<sender-teammate-name>",
    "text": "<message-body>",      // 纯文本或 JSON-stringified 系统消息(idle_notification 等)
    "timestamp": "<ISO-8601>",
    "color": "<sender-color>",     // denormalized 方便 UI
    "read": <bool>                  // Anthropic 跟踪已读状态
  },
  ...
]
```

**`~/.claude/tasks/<team_name>/`** — 任务列表:
- `<id>.json` 每个 task 一个文件(自增数字 id)
- `.highwatermark` byte cursor(per-task 增量读取标记)
- `.lock` 并发锁

### 需求
扩展 `crates/ccteam-core/src/artifact_watcher.rs`,加 **agent-teams discovery + watch** 层(MVP **全局扫所有 ~/.claude/teams/<>/**,不绑 ccteam workflow):

1. **discovery**:daemon 启动时 + 周期扫(60s)`~/.claude/teams/*/config.json`,发现新 team → 加 watch;team 整目录消失(TeamDelete)→ remove watch
2. **per-team watch target**:
   - `~/.claude/teams/<name>/config.json`(file watch)→ diff `members[]` by agentId → emit `team_member_joined` / `team_member_left`(payload 含 member 全字段:name/color/model/agentType/cwd/backendType 等)
   - `~/.claude/teams/<name>/inboxes/<teammate>.json`(file watch per teammate)→ 解析 JSON array,跟上次快照 diff(by timestamp)→ 新增 message → emit `team_message_sent`(`{ team_name, from, to: <teammate>, text_truncated: <200char>, ts, color }`)
   - `~/.claude/tasks/<name>/*.json`(dir watch,新文件 + modify)→ 解析后 emit `team_task_created`(status==pending) / `team_task_completed`(status==completed)
3. **只读**:绝不写 `~/.claude/teams/` 或 `~/.claude/tasks/`;官方明确警告"don't edit by hand, overwritten on state update"
4. **错误韧性**:任一文件 schema 解析失败 → WARN 一次,该 team degrade 到 mtime-only(仍在 web 列表,但事件 emit 暂停);恢复后自动 resume
5. **完整 read 已读状态**:`read: bool` 字段传给 web,Mailbox UI 高亮未读消息(差异化 vs 官方 in-process 模式自动标已读)
6. **idle_notification 解析**:`text` 字段可能是 JSON-stringified 系统消息(实测含 `{"type":"idle_notification", "from":"...", "idleReason":"available"}`)— ccteam 识别这类系统消息分流到 `team_teammate_idle` event(不进 mailbox stream,进 Topology 状态徽章)

### `progress.jsonl` event 镜像

5 类 team event(F95 全部 watcher-emit;F94 加第 6 类 `team_teammate_idle`):

| event | 来源 | payload |
|---|---|---|
| `team_member_joined` | F95 config.json diff | `{ team_name, teammate_name, agent_id, agent_type, model, color, cwd, backend_type, definition_backed, started_at }` |
| `team_member_left` | F95 config.json diff(member 消失)| `{ team_name, teammate_name }` |
| `team_message_sent` | F95 inboxes/ 新文件 | `{ team_name, from, to, text_truncated, ts, color }` |
| `team_task_created` | F95 tasks/ 新文件(status: pending) | `{ team_name, task_id, title, assignee?, dependencies[] }` |
| `team_task_completed` | F95 tasks/ modify(status: completed) | `{ team_name, task_id, result_summary?, completed_at }` |

**`definition_backed` 字段计算逻辑**(F95 emit `team_member_joined` 时):
- 若 `config.json::members[i].agentType` 不是 `"general-purpose"` 且 `"team-lead"` 之外的值(如 `"code-reviewer"` / `"security-reviewer"`)— 视为 definition-backed,字段 = `true`
- 否则(`agentType` ∈ {`general-purpose`, `team-lead`})— ad-hoc 或 lead,字段 = `false`
- F96 Web Topology 用此字段决定显示"↗ definition link"还是"📝 ad-hoc badge"

`ccteam-core/src/orchestrator.rs::Event` enum 加 5 变体(`#[serde(rename = "team_*")]`)。

`ccteam-core/src/orchestrator.rs::Event` enum 加 5 变体(`#[serde(rename = "team_*")]`)。

### 验收
1. daemon 启动后,`~/.claude/teams/` 有 N 个 team dir → daemon log 出现 `discovered N agent teams`,inotify watch list 加 N 条
2. host roblog team(已存在,4 members)→ ccteam web `/api/v1/teams` 立刻列出 roblog;web `/teams/roblog` 详情页 5s 内 render 4 members
3. lead 在某 team 加新 member → 5s 内 ccteam `progress.jsonl` 出现 `team_member_joined`
4. teammate 间 SendMessage 落 inboxes/ → 5s 内 `team_message_sent` 出现
5. `~/.claude/teams/<>/config.json` schema 改 → 解析失败 WARN 一次,team 仍在 web 列表(degrade 到 mtime-only)
6. 新 team 创建(`~/.claude/teams/<new-name>/` 出现)→ 60s discovery 周期内被加 watch,无需重启 daemon

---

## F96 — Web SPA 3 新面板(覆盖所有 host teams,不止 ccteam-managed)

### 痛点
Agent Teams 自带 UI(in-process Shift+Down 切换 / tmux 分屏)在长跑场景下不好用:
- 关掉终端就看不到团队状态
- 历史 task 完成后从 list 滚走
- 多 team 并跑没有总览
- mailbox 消息没法回看
- 跨设备 / 远程访问完全没有

这是 ccteam 跟 OMC 等竞品的核心差异化卖点(OMC 团队 1040 行 SKILL.md 完全没可视化层)。

### 需求
ccteam web SPA **独立于 workflow 项目**,加一个 **Teams 顶级 tab**(沿用 V0.4.6 的 Projects/Sessions/Settings tab 结构):

```
ccteam web header tabs:
  Projects  Teams  Sessions  Settings
                ↑ 新
```

`Teams` tab 列出 host 上所有 `~/.claude/teams/<>/` 发现的 team(F95 discovery 提供),每项点进去看详情页(3 面板):

#### 1. Team Topology 面板
节点图:lead 居中,N teammate 围绕(数据源 = `config.json::members[]`)。每节点显示:
- `name`(Anthropic config member.name)
- 头像色(`config.json::members[].color`)
- `model`(`config.json::members[].model` — 直接读,可能是 `sonnet`/`opus`/自定义 `deepseek-v4-pro[1m]`)
- `agentType`(`team-lead` / `general-purpose` / 自定义 subagent name)
- **prompt 来源徽章**(关键差异化):
  - `agentType ∈ {"general-purpose", "team-lead"}` → 显示 "📝 **ad-hoc**" badge,click 展开 inline prompt(从 config.json::members[i].prompt 读,KB 级,展开为 modal)
  - `agentType` 是其他值(如 `code-reviewer`)→ 显示 "↗ **definition**:`.claude/agents/<agentType>.md`" 超链接,click 在 web SPA 打开 .md 文件渲染(支持 frontmatter 解析 + body markdown render)+ 标注 "skills / mcpServers fields not applied when running as teammate"
- 当前 activity(从 `~/.claude/jobs/<leadSessionId>/state.json` Haiku summary 同源)
- cost(F92 数据源 — `linkScanPath` jsonl 算)
- 状态色:`backendType == "in-process"` → 蓝;`backendType == "tmux"` → 绿;config 中消失 → 灰;最近 30s `idle_notification` → 黄
- `cwd` 文本(member working dir,长路径截尾)

边线 = `members[].subscriptions[]`(Anthropic schema 已有,默认空表示订阅 lead);hover 显示最近 3 条 from-this-node 的消息(从 inboxes/ 反向拉)。

**ad-hoc vs definition 数据流**:
- ad-hoc:整 prompt 在 config.json 里,F96 直接 render(无外部依赖)
- definition:config.json 只存 agentType + spawn 时 lead 给的 task brief,完整 prompt 在 `.claude/agents/<role>.md`,F96 要新加 `GET /api/v1/teams/<name>/member/<n>/definition` endpoint 返回 .md 文件内容(query subagent scope:project → user → plugin → managed)
- definition .md 文件不存在(被删了等错位场景)→ Topology 显示 "↗ ⚠️ definition file missing: `.claude/agents/<agentType>.md`",team 仍可观察但 prompt 来源不明

#### 2. Task Board 面板
Kanban 3 列:Pending / In Progress / Completed(数据源 = `~/.claude/tasks/<team>/*.json`)。每个 task 卡显示:
- task title(JSON `title` / `subject` 字段)
- assignee teammate(JSON `owner` / `assignee` 字段)
- 依赖图标(`dependencies[]` 或 `blockedBy[]` 非空)
- 创建时间 + 完成时间(`status` 状态机:pending → in_progress → completed)
- 头像色取自 owner 在 config.json 的 color

点击 task 卡 → 展开 task `description` + 历史相关 message(filter mailbox by task_id,若 message text 含 `#<task_id>`)。

#### 3. Mailbox Stream 面板
时间线,from → to 消息流(数据源 = 所有 `~/.claude/teams/<>/inboxes/<teammate>.json` JSON array,按 timestamp merge sort)。**实测 schema**:
```json
{ "from": "<teammate>", "text": "...", "timestamp": "ISO-8601", "color": "<sender-color>", "read": <bool> }
```

UI 行为:
- **未读高亮**:`read: false` 行加边框,Mailbox tab badge 显示未读数
- **idle_notification 分流**:`text` 字段是 JSON-stringified `{type: idle_notification}` 系统消息 → 不进 Mailbox Stream,改用 Topology 节点徽章
- 按 teammate 对过滤(from / to 任一)
- 按时间倒序 / 正序
- 搜索 message text 关键词
- 折叠老消息(>1h)
- **不**改 Anthropic 的 `read` 状态(只读)— 想标已读需用户走 native Claude Code attach 流

#### API endpoints

新加 5 个 ccteam web API:
- `GET /api/v1/teams` — 列出 host 所有 team(从 F95 discovery)
- `GET /api/v1/teams/<name>` — 单 team 详情(config + tasks 计数 + 最近消息)
- `GET /api/v1/teams/<name>/tasks` — 全 task 列表
- `GET /api/v1/teams/<name>/inbox?teammate=<n>&since=<ts>` — mailbox 拉取(支持 since cursor)
- `GET /api/v1/teams/<name>/member/<teammate_name>/definition` — 仅 definition-backed teammate 返回 `.claude/agents/<agentType>.md` 文件内容(parsed: `{frontmatter: {...}, body: <markdown>, skills_not_applied: [...], mcp_servers_not_applied: [...]}`);ad-hoc 直接 404 + 引导调 inbox 取 inline prompt

#### SSE wiring
新加 SSE channel:`/api/v1/teams/<name>/events`,推送 6 类 team event(F95 提供 5 类 + F94 提供 `team_teammate_idle`)— 镜像到 progress.jsonl 后由 web backend forward。

### 验收
1. host 跑过 agent team(如 roblog,5 members 全 ad-hoc)→ ccteam web `/teams` tab 显示 roblog 卡片
2. 点 roblog 进详情页 → Team Topology 5s 内 render 5 节点;**所有节点显示 "📝 ad-hoc" badge**(agentType 均为 `general-purpose` / `team-lead`)
3. 起一个测试 team 用 definition-backed teammate(workflow.yaml `kind: definition` + `.claude/agents/code-reviewer.md` 存在)→ Topology 该节点显示 "↗ definition" 链接,click 打开 modal render .md frontmatter + body
4. definition .md 文件被删除 → Topology 节点显示 warning "definition file missing"
5. roblog tasks/ 加文件 → Task Board 5s 内出现新 task 卡(Pending 列)
6. teammate 间 SendMessage 落 inboxes/ → Mailbox Stream 5s 内出现消息;`read: false` 高亮
7. host 同时有 2+ agent team → `/teams` tab 列两个,不串味
8. ccteam-managed agent-team workflow(F93 起的)+ host interactive team(用户自起)同时存在 → 两者都在 `/teams` tab(无区分,**ccteam 视所有 team 为平等可视化对象**)
9. SSE 断线 → 面板"reconnecting...",5s 内自动重连
10. 跨浏览器:Chrome / Firefox / Safari 最新版 + iOS Safari(响应式)

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
