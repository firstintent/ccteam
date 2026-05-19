# OMC team vs ccteam V0.5.0 agent-team mode

> **角色**:研究笔记。读完后回 `docs/versions/v0-5-0/prd.md` 看 ccteam 怎么落地。**不更新**(按 ccteam 文档维护三类规则:research 是探索性,不进 SoT)。
>
> **资料**:
> - `references/omc/skills/team/SKILL.md` 1040 行 — `Yeachan-Heo/oh-my-claudecode@main:skills/team/SKILL.md`(2026-05-17 镜像)
> - `references/claude-code/teams-samples/config-roblog.json` + `inbox-team-lead.json` — host roblog team 实测(2026-05-17)

---

## 一、OMC team 架构骨架(1040 行 SKILL.md 浓缩)

```
/team N:agent-type "task"
  └─ TeamCreate (lead = current session)
     └─ team-plan → team-prd → team-exec → team-verify → team-fix(loop, max=3)
        └─ 每 stage 末写 .omc/handoffs/<stage>.md(Decided/Rejected/Risks/Files/Remaining)
        └─ 每 stage 用专门 role(planner=opus / executor=sonnet / verifier=sonnet)
        └─ TaskCreate × N(lead 预分配 owner 防 race)
        └─ Task(team_name, name) × N(spawn teammate via native Task tool)
        └─ Monitor:SendMessage 自动到达 + TaskList 轮询 + 5min watchdog
        └─ Shutdown:30s timeout per teammate + cleanup-orphans.mjs
```

State 三份并存:
- **Anthropic SoT**:`~/.claude/teams/<>/config.json` + `~/.claude/tasks/<>/*.json`(只读)
- **OMC SoT**:`.omc/state/team-state.json`(current_phase / fix_loop_count / max_fix_loops / linked_ralph / stage_history)
- **决策溯源**:`.omc/handoffs/<stage>.md`(stage 间传递,survives cancel)

通信:纯 `SendMessage`/`broadcast`/`shutdown_request`,无私有 mailbox。

---

## 二、Anthropic Agent Teams 实测 schema(host roblog team probe)

### `~/.claude/teams/<team_name>/config.json`

```json
{
  "name": "roblog",
  "description": "...",
  "createdAt": 1778938873365,
  "leadAgentId": "team-lead@roblog",
  "leadSessionId": "<uuid>",
  "members": [
    {
      "agentId": "team-lead@roblog",
      "name": "team-lead",
      "agentType": "team-lead",      // 特殊值,标识 lead
      "model": "deepseek-v4-pro[1m]",// 任意字符串,支持非 Anthropic 模型
      "joinedAt": 1778938873365,
      "tmuxPaneId": "",               // 空 = in-process,否则是真实 tmux pane id
      "cwd": "/home/rob/projects/blog",
      "subscriptions": []             // 订阅 topology(空 = 接收所有)
    },
    {
      "agentId": "<role>@<team>",
      "name": "<role>",
      "color": "blue|green|yellow|purple|...",
      "agentType": "general-purpose|<subagent-name>",
      "model": "sonnet",
      "prompt": "<完整 system prompt,可达 KB 级>",   // ⚠️ 别塞密钥
      "cwd": "/home/rob/projects/blog",
      "tmuxPaneId": "in-process|<pane-id>",
      "subscriptions": [],
      "joinedAt": ...,
      "backendType": "in-process|tmux",  // 跟 tmuxPaneId 冗余但更明确
      "planModeRequired": false
    },
    ...
  ]
}
```

### `~/.claude/teams/<team_name>/inboxes/<teammate>.json`

**单 JSON 文件 per 收件人,数组 of messages**(不是目录):

```json
[
  {
    "from": "frontend-dev",
    "text": "{\"type\":\"idle_notification\",\"from\":\"frontend-dev\",\"timestamp\":\"2026-05-16T13:45:19.594Z\",\"idleReason\":\"available\"}",
    "timestamp": "2026-05-16T13:45:19.594Z",
    "color": "green",
    "read": true
  },
  {
    "from": "reviewer",
    "text": "reviewer 已就绪...",
    "timestamp": "2026-05-16T13:46:39.238Z",
    "color": "yellow",
    "read": true
  }
]
```

关键字段:
- `from`:发送方 teammate name
- `text`:消息体 — **plain text 或 JSON-stringified 系统消息**(`idle_notification` / 等等)
- `timestamp`:ISO-8601
- `color`:发送方 color(denormalized 方便 UI)
- `read`:**Anthropic 跟踪已读状态** — ccteam 可读不可写

### `~/.claude/tasks/<team_name>/`

- `<id>.json` 每个 task 一个文件,id 自增数字 string("1", "2", ...)
- `.highwatermark` byte cursor(per-team 增量读取标记,2 字节)
- `.lock` 并发锁(空文件,fcntl)

---

## 三、对照表:OMC vs Anthropic vs ccteam V0.5.0

| 维度 | OMC team(prompt-as-orchestrator)| Anthropic Agent Teams(native)| ccteam V0.5.0 |
|---|---|---|---|
| **控制权** | LLM(SKILL.md 是剧本) | LLM(lead session) | code(Rust orchestrator)+ LLM(__lead session) |
| **Lead 来源** | 用户 invoke `/team` 的当前 session | 用户 invoke 自然语言或 Claude 提议 | ccteam spawn 的 `claude --bg --agent __lead` 长跑 session(F93)|
| **Team 配置** | `.omc/state/team-state.json`(自行维护) | `~/.claude/teams/<>/config.json`(Anthropic 写) | workflow.yaml(意图)+ Anthropic config.json(运行时 SoT,F95 watcher 读) |
| **Stage 模型** | 固定 5-stage pipeline(plan/prd/exec/verify/fix) | 无,完全自由 | 无,完全自由(workflow.yaml lead_seed 决定形状) |
| **Routing** | resolved snapshot @ TeamCreate(per-role provider/model)| 无,lead 临时决定 | F93 snapshot_path 借鉴 OMC stickiness |
| **Handoff 机制** | `.omc/handoffs/<stage>.md` 10-20 行 | 无 | **V0.5.x 可选加**(借 OMC,不强制) |
| **Mailbox** | 纯 `SendMessage` | `inboxes/<teammate>.json` 数组 + `read` 状态 | F95 watcher 读,F96 web Mailbox Stream 展示 + **未读高亮** |
| **Fix loop** | `max_fix_loops: 3` 默认,terminal failed | 无 | CLAUDE.md §三 fix_counts 3-strike 已有 |
| **Worktree** | `.omc/worktrees/<team>/<worker>` + `omc-team/<>/<>` branch | `~/.claude/.claude/worktrees/<...>/`(agent view 自动) | V0.5.x 可加,MVP 不做 |
| **Shutdown** | BLOCKING 30s + `cleanup-orphans.mjs` | `shutdown_request/response` + `TeamDelete` | F97 `cleanup_on_stop: force-kill` MVP + orphan scan V0.5.x |
| **Hybrid workers** | codex + gemini + claude(tmux pane)| 仅 Claude(experimental)| 仅 Claude(MVP);codex/gemini 进 V0.5.x roadmap |
| **可视化** | **无**(SKILL.md 1040 行纯 CLI) | **无**(in-process Shift+Down + tmux 分屏,关终端就丢)| **F96 web Teams tab + 3 面板**(差异化卖点)|
| **长跑** | lead 是用户 session,session 关就停 | 同 OMC | F93 `__lead` 是 `claude --bg`,daemon 看护,24×7 |

---

## 四、ccteam V0.5.0 借鉴清单

5 处直接采用 OMC pattern(详 PRD):

| 借鉴点 | 出处 | ccteam 落地 |
|---|---|---|
| 30 行 Worker Preamble("你是 TEAM WORKER 不是 leader,never spawn sub-agent")| OMC `## Agent Preamble` | `__lead.md` 系统 prompt 嵌入 worker prompt 生成 boilerplate(中文化)|
| Resolved routing snapshot(stickiness)| OMC `## Per-Role Provider & Model Routing` | F93 `agent_team.snapshot_path: .ccteam/team-snapshot.json`,workflow.yaml 改不影响跑着的 team |
| max_fix_loops: 3 default | OMC `### Verify/Fix Loop and Stop Conditions` | ccteam `fix_counts` 已 3-strike escalate(`CLAUDE.md §三`)|
| Pre-assign task owner 防 race | OMC `### Phase 4: Create Tasks` 末段 | `__lead.md` prompt 提示 lead 用 TaskUpdate 预分配 |
| orphan scan(team name 匹配的 process,config 已 TeamDelete)| OMC `cleanup-orphans.mjs` | F97 `ccteam doctor --gc-orphan-teammates` MVP 加 |

3 处 ccteam 差异化(刻意不抄):

| 差异 | OMC 做法 | ccteam V0.5.0 做法 | 原因 |
|---|---|---|---|
| 不固化 stage pipeline | 5-stage 固定 | shape-agnostic — lead_seed 决定 | agent-team mode 不只是 pipeline,还有 debate / parallel review / vote 等 |
| 不开第二份 state 文件 | `.omc/state/team-state.json` 单独维护 | 全进 `progress.jsonl`(沿用 CLAUDE.md §三 SoT 红线)| 守一致性换可维护性;OMC `cancel/SKILL.md` 387 行 state proliferation 是反面教材 |
| 不调 Haiku 总结 message text | OMC 不调 | ccteam 也不调,直接截前 200 char | 避免 cost 累积;消息原文已在 inboxes/ 文件可点开看 |

---

## 五、Anthropic Agent Teams 跟 OMC 的区别

OMC team SKILL.md 在 Anthropic Agent Teams 发布**之后**重写,放弃 SQLite-based swarm 转用 native Agent Teams API(`TeamCreate` / `TaskCreate` / `Task(team_name, name)` / `SendMessage`),所以 OMC 自己就是"Agent Teams 之上的脚本"。

但 OMC 加了几层:
1. **Stage pipeline 强约束**:plan/prd/exec/verify/fix 5-stage 是规范,不是建议
2. **OMC state file 跟 Anthropic state 并存**:`.omc/state/team-state.json` 跟 `~/.claude/teams/<>/config.json` 双 SoT
3. **Handoff 文件**:`.omc/handoffs/<stage>.md` Anthropic 没有
4. **Resolved routing snapshot**:`~/.config/claude-omc/config.jsonc::team.roleRouting` 解析后 freeze
5. **Hybrid worker(codex/gemini tmux)**:Anthropic 只 Claude;OMC 加 tmux pane 包装

ccteam V0.5.0 学习的是**这几层**(尤其 worker preamble + snapshot + orphan scan),**不学** stage pipeline 强约束(过度规范化)+ 双 SoT(违反单一 SoT 红线)。

---

## 六、引用资料

- 完整 OMC SKILL.md 原文:`references/omc/skills/team/SKILL.md`(gitignored,2026-05-17 镜像)
- Anthropic 实测 sample:
  - `references/claude-code/teams-samples/config-roblog.json`(team config 完整)
  - `references/claude-code/teams-samples/inbox-team-lead.json`(team-lead 收件箱,39 条消息)
- ccteam V0.5.0 PRD:`docs/versions/v0-5-0/prd.md`(借鉴 / 差异 落实)
- ccteam 设计哲学:`docs/orchestration-patterns.md`(5 模式 + 拆分原则)
