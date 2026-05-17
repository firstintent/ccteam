# ccteam interfaces — 精确协议参考

> 本文是 [tech-design.md](./tech-design.md) 的"接口卡"。
>
> - `tech-design.md` 回答 **怎么做**(架构论证、设计权衡)
> - **本文回答 接口确切长什么样**(YAML schema、JSON shape、文件路径、命令签名)
>
> **实现 PR 修改任何对外协议 → 必须同步本文**。架构论证不属于本文,在 tech-design;具体字段定义不属于 tech-design,在本文。

---

## 1. 文件系统布局

### 1.1 全局目录(`~/.ccteam/`)

```
~/.ccteam/
├── config.yaml            # 全局配置(并发上限、信任档位、模型单价表;V0.4.2 F73)
├── inbox/                 # 待 triage 的需求
├── queue/                 # 阶段队列
├── teams/                 # 每个 team 单一目录
│   ├── dev/team.yaml      # 详见 §5.5
│   ├── meta-agent/team.yaml
│   └── <user-team>/
├── templates/             # phase 可 @ 引用的 prompt 片段
├── control/               # 用户 → orchestrator 控制信号(详见 §3.3)
# 跨项目记忆走官方 ~/.claude/CLAUDE.md + ~/.claude/rules/ + per-repo auto-memory,详 tech-design §3.7。
├── progress/
│   ├── <slug>.jsonl       # workflow 项目事件流(详见 §4)
│   └── <slug>/<sid>.jsonl # flex 项目每 session 独立事件流
├── harness/               # Codex CLI adapter 写;Claude session 改读 ~/.claude/jobs/
│   └── <slug>-<sid>.json
├── log/
│   └── <slug>/            # stream-json 归档(可选,调试用)
├── tmux/
│   └── <slug>.layout
└── state/
    └── orchestrator.json
```

**Team 解析三层优先级**(`team_resolver.rs`):

```
const TEAM_SOURCES: &[TeamSource] = &[
    TeamSource::Project,  // <project_dir>/.ccteam/team/team.yaml(per-project override)
    TeamSource::User,     // ~/.config/ccteam/teams/<name>/team.yaml(staging)
                          // + ~/.claude/plugins/marketplaces/*/plugins/<team>/team.yaml
    TeamSource::Repo,     // ~/.ccteam/teams/<name>/team.yaml(shipped seeds)
];
```

First-source-wins,整团维度替换。读容错(yaml 错 → warn + 下一层),写严格。

### 1.2 项目级目录(`~/projects/<team>-<slug>/`)

```
~/projects/<team>-<slug>/
├── src/                          # 实际代码
├── tests/
├── CLAUDE.md                     # 项目级运营手册(自动生成,详见 tech-design §6.5)
├── .ccteam/                      # ccteam 元数据
│   ├── workflow.yaml             # canonical 位置(F83;详见 §17)
│   ├── state.json                # 项目级状态机(详见 §2.1)
│   ├── escalation.md             # 触发用户介入时写这里
│   ├── spawn_requests/           # MCP `spawn_agent` marker 桶(详见 §12.2)
│   ├── stop_signal/              # MCP `stop_agent` marker 桶
│   ├── signal/                   # MCP `signal` marker 桶
│   ├── gate_override/            # MCP `trigger_gate` marker
│   ├── workflow_overrides.json   # MCP `set_parallelism` 覆写
│   ├── sessions/                 # flex-only adhoc session cwd
│   │   └── <sid>/                # 例 claude-1;内含本 session inbox/outbox
│   └── ready                     # SessionStart hook 写出的就绪标记
├── .claude/
│   ├── agents/                   # 每个 workflow agent 一份 `<role>.md`(prompt body 在此,不在 workflow.yaml)
│   │   └── <role>.md
│   └── settings.json             # 详见 §6.1
└── .gitignore
```

### 1.3 Flex adhoc multi-session 布局(`kind: flex`)

`kind: flex` 是手动 session farm:无 workflow.yaml driven 派发,但保留 hooks /
progress.jsonl / cost / silence classifier / web observability。

```
~/projects/<team>-<slug>/
├── .ccteam/
│   ├── state.json                 # master state,含 team_kind/sessions/next_sid_seq
│   └── sessions/
│       ├── claude-1/
│       │   ├── inbox/
│       │   └── outbox/
│       └── claude-2/
└── .claude/settings.json

~/.ccteam/progress/<slug>/
├── claude-1.jsonl
└── claude-2.jsonl
```

`ccteam session add <slug> --harness=claude` 分配单调递增 sid
`<harness>-<n>`、创建 `<project>/.ccteam/sessions/<sid>/`、启动 tmux
`ccteam-<slug>-<sid>` 并写入 `state.json::sessions`。`session rm` 是
唯一自动关闭 harness session 的路径,且必须由用户显式触发。

---

## 2. State 协议

### 2.1 项目级 `state.json`

```json
{
  "slug": "bookmark-mgr-a3f9",
  "team": "dev",
  "team_kind": "workflow",
  "created_at": "2026-05-04T10:23:00Z",
  "tmux_session": "ccteam-bookmark-mgr-a3f9",
  "soft_warn_threshold_usd": 20.0,
  "hard_kill_threshold_usd": 200.0,
  "last_progress_event_at": "2026-05-04T11:23:45Z",
  "last_event_type": "Stop",
  "last_user_interaction_at": "2026-05-04T10:23:00Z",
  "user_attached": false,
  "user_pause_pending": false,
  "sessions": {},
  "next_sid_seq": {}
}
```

**`team` 字段**:指定项目跑哪个团队的 workflow / agents(默认 `dev`)。

**`team_kind` 字段**:`workflow` | `multi_workflow` | `flex`。从 `team.yaml::kind` 缓存到 state,供 hooks 判断 flex 路径。默认 `workflow`,默认值时省略。

**`sessions` / `next_sid_seq` 字段**(flex 项目):master session registry。
`sessions` shape `{ "<sid>": { "harness": "claude", "tmux_session": "ccteam-<slug>-<sid>", "started_at": "...", "pid": 12345|null } }`;
`next_sid_seq` 是每 harness 的下一个编号(删除 session 不递减,sid 不复用)。
workflow 项目保持空对象或省略。

**原子写入**:`.tmp` + `rename`;启动校验 schema,损坏走 backup。

---

## 3. Inbox / Queue / Control 文件协议

### 3.1 Inbox

文件名:`<ISO-timestamp>-<random>.md`,原子写入(先写 `.tmp` 再 `mv`)。

```markdown
---
source: telegram          # telegram | cli | echo
user: rob
created_at: 2026-05-04T10:23:00Z
---

# 想法

做一个本地书签管理器,离线可用,按域名归类,支持搜索。
最好是 PWA,能装到手机。
```

### 3.2 Queue 状态分桶

按 §1.1 所示 `~/.ccteam/queue/<state>/<slug>.md`。状态值与 `state.json.current_phase` 大致对齐(详见 tech-design §3.2 状态机)。

### 3.3 Control(用户 → orchestrator)

```
~/.ccteam/control/
├── reject-<slug>           # 创建文件 = 命令"否决项目 <slug>"
├── pause-all               # 创建文件 = 暂停所有调度
├── pause-<slug>            # 暂停单项目
├── resume-<slug>           # 恢复单项目
├── answer-<slug>.md        # 内容 = 用户对 clarify 问题的回答
├── boost-<slug>            # 提升优先级
└── fork-reply-<slug>.md    # L3 fork 决策回复(M1+;详见 §9.3)
```

orchestrator 每轮(30s)扫描 `control/`,处理后**删除文件**(确保幂等)。

### 3.4 Per-session Inbox / Outbox

channel adapter(Telegram bot 等)↔ session 内 claude 的接入面契约。**adapter 进程内不嵌 LLM**,NL 解析都在 session 内 claude 完成。

**目录布局**:每条 ccteam-managed long session 各有一份 inbox / outbox:`~/projects/<user>-meta/.ccteam/{inbox,outbox}/` 与 `~/projects/<slug>/.ccteam/{inbox,outbox}/`。文件名 `{msg|reply}-<ISO-ts>-<seq>.md`(seq 3 位 zero-padded),原子写入(`.tmp` + `mv`)。

**Inbox schema**(adapter → session):

```markdown
---
schema_version: 1
source: telegram                        # telegram | feishu | slack | terminal | cli | <adapter-name>
source_chat_id: "@rob_personal"         # 可选
source_msg_id: "tg-msg-12345"           # 可选
source_user: rob                        # 必选
created_at: 2026-05-06T10:30:00Z        # 必选
ingested_at: 2026-05-06T10:30:01Z       # 必选
content_type: text                      # text | markdown | image_url | file_path
attachments:                            # 可选
  - kind: image_url
    url: https://...
---

# NL message body
```

**Outbox schema**(session → adapter):

```markdown
---
schema_version: 1
in_reply_to: msg-2026-05-06T103000Z-001.md   # 可选(thread)
in_reply_to_source_msg_id: "tg-msg-12345"    # 可选
target_channels: [telegram]                   # 可选,空 = 推回 source
created_at: 2026-05-06T10:30:45Z
priority: normal                              # normal | high
event_kind: reply                             # reply | progress | escalation | shipped | clarify
---

# NL reply body
```

`event_kind` 决定 adapter 推送优先级(`reply` / `progress` / `escalation` / `shipped` / `clarify`)。

**Adapter 责任边界**:入向写 inbox,出向轮询 outbox 推外部(推送成功后**删除 outbox 文件**,adapter 负责 ack);维护"外部 channel ↔ session" 映射放 adapter 自己;**不允许**解析内容做 NL 判断 / 写 progress.jsonl / 起 LLM 调用。

**Orchestrator 处理 inbox**:inotify watch,新文件 → idle 时 `tmux send-keys` 直接注入,busy 时 `/btw <body>` 排队;处理完成后**删除 inbox 文件** + append `inbox_consumed` 事件到 progress.jsonl。

**Session 写 outbox**:`.ccteam/CLAUDE.md` 显式约定用 Write 工具写到 `outbox/reply-<ts>-<seq>.md`。

§3.1 全局 inbox 是 M0"提想法"入口,M1+ 推荐走 meta-agent session inbox。

---

## 4. Progress.jsonl 事件流

workflow 项目使用一个 `~/.ccteam/progress/<slug>.jsonl`。
flex 项目使用每 session 一个 `~/.ccteam/progress/<slug>/<sid>.jsonl`,读侧按
`ts` 聚合。**这是 orchestrator 唯一的状态事实来源**——tmux 终端输出只给人看,
不解析。

### 4.1 事件类型(完整清单)

progress.jsonl 由两个域共同写入:

**workflow domain**(orchestrator 写,共 7 类 + fix-loop 第 8 类 `escalation`):

```jsonl
{"ts":"2026-05-10T09:00:00Z","event":"workflow_start","workflow":"watcher","slug":"dev-foo"}
{"ts":"2026-05-10T09:00:01Z","event":"agent_spawn","role":"fixer","session_id":"fixer-1","executor":"claude","tmux_session":"ccteam-dev-foo-fixer-1","job_id":"9432490e","slug":"dev-foo"}
{"ts":"2026-05-10T09:00:02Z","event":"artifact_received","role":"fixer","artifact_path":"/abs/path/to/issues/bug.md","slug":"dev-foo"}
{"ts":"2026-05-10T09:00:03Z","event":"gate_triggered","role":"reviewer","forced":false,"threshold_met":true,"slug":"dev-foo"}
{"ts":"2026-05-10T09:01:00Z","event":"agent_done","role":"fixer","session_id":"fixer-1","status":"completed","cost_usd":0.42,"slug":"dev-foo"}
{"ts":"2026-05-10T09:01:01Z","event":"budget_exceeded","role":"reviewer","cost_used_usd":205.13,"budget_limit_usd":200.0,"slug":"dev-foo"}
{"ts":"2026-05-10T09:01:02Z","event":"escalation","kind":"spawn_failed","role":"fixer","consecutive_failures":3,"slug":"dev-foo"}
{"ts":"2026-05-10T09:02:00Z","event":"workflow_done","workflow":"watcher","slug":"dev-foo"}
```

字段语义:

| event | 必有字段 | 选填字段 | 写入时机 |
|---|---|---|---|
| `workflow_start` | `workflow` (`WorkflowSpec::name`), `slug`, `ts` | — | `Orchestrator::run_project` 入口,加载 workflow.yaml 成功后 |
| `agent_spawn` | `role`, `session_id`, `executor` (`claude`\|`codex`), `slug`, `ts` | `tmux_session` (claude 用 `ccteam-<slug>-<sid>` 占位;codex 写真名),`job_id` (Claude Code `--bg` 返回的短 hash,如 `"9432490e"`,codex 行为 `null`) | `HarnessAdapter::spawn_session` 返回 `Ok(handle)` 后 |
| `agent_done` | `role`, `session_id`, `status` (`completed`\|`stopped`\|`error`\|`killed`), `cost_usd` (f64;无 cost 时 `0.0`), `slug`, `ts` | — | (a) `session_state_path` 文件 `status` ∈ {`stopped`, `completed`, `error`} 时,poll 一次;(b) `poll_completions` 发现 progress.jsonl 含 open `agent_spawn` 但其 `job_id` 对应的 `~/.claude/jobs/<id>/state.json` 已 terminal,orchestrator 合成 `agent_done`,`status: "killed"` 用于 SIGKILL 死亡的 phantom row(防止 web UI 显示僵尸 running) |
| `artifact_received` | `role`, `artifact_path` (abs), `slug`, `ts` | — | `ArtifactWatcher` 通过 mpsc 投递 `ArtifactEvent` 后,orchestrator 立刻 append(spawn 决策之前) |
| `gate_triggered` | `role`, `forced` (bool), `threshold_met` (bool), `slug`, `ts` | — | `check_gates` 解锁 Gate 时;`forced=true` 表示 `.ccteam/gate_override/<role>` marker 触发 |
| `budget_exceeded` | `role`, `cost_used_usd` (f64), `budget_limit_usd` (f64), `slug`, `ts` | — | `try_spawn` 内 budget guard 拦截 spawn 时(运行 session 永不被 kill) |
| `workflow_done` | `workflow`, `slug`, `ts` | `reason` (见下) | 所有 Gate role 都进入 Fired 状态且无 running session 时,幂等 emit 一次;cancel-token 路径写出时 reason 必填 |
| `escalation` | `kind` (`spawn_failed` 等), `role`, `consecutive_failures` (u32), `slug`, `ts` | — | `bump_fail_count` 每次 +1;`>= MAX_CONSECUTIVE_SPAWN_FAILURES` 时另发 `send_btw_escalation` 到 meta-agent inbox |

**`workflow_done.reason` 枚举**(`CancelReason::as_str`,
`crates/ccteam-core/src/orchestrator.rs`):

| reason | 触发 |
|---|---|
| (空) | check_workflow_done 自然完成(所有 Gate Fired + 无 running session) |
| `disabled` | `workflow.yaml::enabled: false` 热改 → cancel-token → 写 done |
| `removed` | `ccteam remove <slug>` → unroster → cancel-token |
| `reloaded` | agents 拓扑突变,老 loop 退场 + 新 loop 起 |
| `shutdown` | `ccteam stop` / SIGTERM / `/tmp/ccteam-<user>.shutdown` trigger graceful shutdown |
| `budget_exceeded` | `budget.max_cost_usd_per_24h` 或 `max_agent_spawns_per_hour` trip → 自动 `enabled=false` → cancel-token |

`budget_exceeded` 事件先于 `workflow_done reason="budget_exceeded"` emit 一行
`{"event":"budget_exceeded","role":<trigger_role|null>,"cost_used_usd":<sum_24h>,"budget_limit_usd":<cap>,"slug":...}`(budget guard 在 spawn 之外也消费它判 24h 滑窗 cost / 1h spawn count cap)。

**team domain**(V0.5.0 F95 + F94 — Anthropic Agent Teams 镜像,共 6 类):

```jsonl
{"ts":"...","event":"team_member_joined","team_name":"roblog","teammate_name":"pm","agent_id":"...","agent_type":"general-purpose","model":"sonnet","color":"orange","cwd":"/home/.../roblog","backend_type":"in-process","definition_backed":false,"started_at":"...","ts":"..."}
{"ts":"...","event":"team_member_left","team_name":"roblog","teammate_name":"pm","ts":"..."}
{"ts":"...","event":"team_message_sent","team_name":"roblog","from":"pm","to":"team-lead","text_truncated":"...","msg_ts":"...","color":"orange","read":false,"ts":"..."}
{"ts":"...","event":"team_task_created","team_name":"roblog","task_id":"42","title":"do thing","assignee":"pm","dependencies":[],"ts":"..."}
{"ts":"...","event":"team_task_completed","team_name":"roblog","task_id":"42","result_summary":"done","completed_at":"...","ts":"..."}
{"ts":"...","event":"team_teammate_idle","team_name":"roblog","teammate_name":"pm","idle_reason":"available","idle_since":"...","ts":"..."}
```

| event | 必有字段 | 选填字段 | 写入时机 |
|---|---|---|---|
| `team_member_joined` | `team_name`, `teammate_name`, `agent_id`, `agent_type`, `model`, `cwd`, `backend_type`, `definition_backed`, `started_at`, `ts` | `color` | F95 `AgentTeamsWatcher` 检测到 `~/.claude/teams/<team>/config.json::members[]` 新增条目;cold-start 时每个 member emit 一次 |
| `team_member_left` | `team_name`, `teammate_name`, `ts` | — | F95 watcher 检测到 `members[]` 中条目消失 |
| `team_message_sent` | `team_name`, `from`, `to`, `text_truncated`, `msg_ts`, `ts` | `color`, `read` | F95 watcher 检测到 `~/.claude/teams/<team>/inboxes/<teammate>.json` 追加新 message |
| `team_task_created` | `team_name`, `task_id`, `title`, `ts` | `assignee`, `dependencies` | 优先 F94 hook(`TaskCreated`,advanced path);F95 watcher fallback `~/.claude/tasks/<team>/<id>.json::status: pending` |
| `team_task_completed` | `team_name`, `task_id`, `completed_at`, `ts` | `result_summary` | 优先 F94 hook(`TaskCompleted`);F95 watcher fallback `status: completed` 变化 |
| `team_teammate_idle` | `team_name`, `teammate_name`, `idle_since`, `ts` | `idle_reason` | F94 hook only(`TeammateIdle`)— Anthropic 的 idle 是内存状态,watcher 拿不到 |

来源优先级:
- F94 hook(`TaskCreated` / `TaskCompleted` / `TeammateIdle`) — 仅 F93b advanced path 装(`ccteam init --mode agent-team` 用 `settings.agent-team.json`)
- F95 watcher 全局 fallback(对所有 host `~/.claude/teams/`) — primary path(`/ccteam:team` skill)的唯一来源
- F94 hook 失败 → F95 watcher 接管 `team_task_*`(`team_teammate_idle` 没 fallback)

**hook domain**(Claude Code / Codex hook 写;详见 §6.2):

```jsonl
{"ts":"2026-05-10T09:00:00Z","event":"PreToolUse","tool":"Edit","path":"src/lib.rs"}
{"ts":"...","event":"PostToolUse","tool":"Bash","cmd":"pnpm test","exit_code":0,"duration_ms":4521}
{"ts":"...","event":"SubagentStop"}
{"ts":"...","event":"Stop"}
{"ts":"...","event":"SessionEnd","reason":"context_reset"}
{"ts":"...","event":"notification"}
{"ts":"...","event":"user_attach","detected_by":"PreToolUse-input-source"}
```

(`session_start` 由 orchestrator 写;`SubagentStop` / `Stop` / `SessionEnd` /
`notification` 由 `idle_aware_message` / `is_idle` / `subagent_active` 等 idle
探测消费,见 `progress.rs`。)

### 4.2 写入责任

| 事件 | 写入方 |
|---|---|
| `workflow_start` / `workflow_done` | orchestrator(`Orchestrator::run_project` 入口 / `check_workflow_done` 守门) |
| `agent_spawn` | orchestrator(`HarnessAdapter::spawn_session` 返回 Ok 后) |
| `agent_done` | orchestrator(`poll_completions` 检测到 session state.json `status` ∈ {`completed`, `stopped`, `error`}) |
| `artifact_received` | orchestrator(`ArtifactWatcher` mpsc 投递后) |
| `gate_triggered` | orchestrator(`check_gates` 释放 Gate 时) |
| `budget_exceeded` | orchestrator(`try_spawn` budget guard) |
| `escalation` | orchestrator(`bump_fail_count`,fix-loop 3-strike 与 `spawn_session` 持续失败) |
| `team_member_joined` / `team_member_left` / `team_message_sent` | F95 `AgentTeamsWatcher`(watcher only;Anthropic 没对应 hook surface) |
| `team_task_created` / `team_task_completed` | F94 hook 优先(advanced path),F95 watcher fallback |
| `team_teammate_idle` | F94 hook only(`TeammateIdle`,仅 advanced path 装) |
| `session_start` / `PreToolUse` / `PostToolUse` / `SubagentStop` / `Stop` / `SessionEnd` / `notification` / `user_attach` | Claude Code / Codex hooks 与启动器(详见 §6.2) |

### 4.3 消费方

- **orchestrator** 自身(读 progress.jsonl 用于 budget guard / fix-loop 计数 /
  workflow_done 幂等保护):见 `ccteam_core::progress::read_all_events`
  与 `Orchestrator::cumulative_cost_from_progress`
- **`ccteam_core::progress`**:暴露 `workflow_cost_total` /
  `current_agent_sessions` / `escalation_count` 三组聚合,
  pure-function 接口供 web 与 meta-agent 查询(见 §4.4 + `queries::workflow_summary`)
- **`ccteam_core::queries::workflow_summary`**:合并 workflow.yaml 规格与
  progress.jsonl 事件,生成 `WorkflowSummary { workflow_name, agents[],
  artifact_counts, total_cost_usd, escalation_count, gate_states }`;SPA 消费
- **MCP `observe_agents`**:读 `state.json::sessions` 列出运行 session;cost /
  status 字段读 `agent_done` 事件
- **用户 dashboard pane**:`tail -f progress/<slug>.jsonl | jq -c '.event + ":" + (.role // .tool // "")'`
- **retro / lessons writer**:作为项目历史输入,通过 Claude session
  `/memory` + `Edit ~/.claude/rules/ccteam-lessons-<team>.md` 写入

### 4.4 Stream-json 归档(可选)

用 hook 把 `--output-format stream-json` 内容旁路归档到 `~/.ccteam/log/<slug>/`,仅供事后调试,**不参与状态判定**。

---

## 5. Phase 模板 schema(V0.4.0 F60 EOL)

Phase 模型 V0.4.0 F60 已删除,替换为 workflow.yaml + 事件驱动 agent 拓扑;详见 §17 `workflow.yaml` schema。

---

## 6. Hooks 配置 schema

### 6.1 项目 `.claude/settings.json` 完整模板

所有 hook 都是 `ccteam` 单 binary 的 `internal hook <subcmd>` 子命令——零运行时依赖,与 orchestrator 共享 serde schema。

```json
{
  "permissions": {
    "allow": ["*"],
    "deny": ["WebFetch(url:https://*.bank.com/*)"]
  },
  "env": {
    "CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS": "1"
  },
  "hooks": {
    "SessionStart": [
      {
        "hooks": [
          {"type": "command", "command": "ccteam internal hook load-context", "timeout": 5},
          {"type": "command", "command": "ccteam internal hook progress-append session_start", "async": true}
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          {"type": "command", "command": "ccteam internal hook progress-append Stop", "async": true}
        ]
      }
    ],
    "Notification": [
      {
        "matcher": "idle_prompt|permission_prompt",
        "hooks": [
          {"type": "command", "command": "ccteam internal hook progress-append notification", "async": true}
        ]
      }
    ],
    "PreToolUse": [
      {
        "hooks": [
          {"type": "command", "command": "ccteam internal hook progress-append PreToolUse", "async": true}
        ]
      }
    ],
    "PostToolUse": [
      {
        "hooks": [
          {"type": "command", "command": "ccteam internal hook progress-append PostToolUse", "async": true}
        ]
      },
      {
        "matcher": "Bash:git push.*",
        "hooks": [
          {"type": "command", "command": "ccteam internal hook block-push"}
        ]
      }
    ],
    "SubagentStop": [
      {
        "hooks": [
          {"type": "command", "command": "ccteam internal hook progress-append SubagentStop", "async": true}
        ]
      }
    ],
    "SessionEnd": [
      {
        "hooks": [
          {"type": "command", "command": "ccteam internal hook progress-append SessionEnd", "async": true}
        ]
      }
    ]
  }
}
```

### 6.2 Hook 事件用途

| Hook | 作用 |
|---|---|
| `SessionStart` | 写 ready 标记;append `session_start` 事件 |
| `Stop` | append `Stop` 事件(idle 信号) |
| `Notification:idle_prompt` | claude 显式等待用户输入 → idle 信号 |
| `Notification:permission_prompt` | 不应出现(`--dangerously-skip-permissions` 兜底);出现说明配置失效 |
| `PreToolUse`(通用) | append 工具调用事件;活跃信号(stall 检测反向判断) |
| `PostToolUse`(通用) | append 事件 |
| `PostToolUse(Bash matcher)` | 拦截危险命令(`git push` / `rm -rf /` / deploy 脚本) |
| `SubagentStop` | 子 agent 退出 |
| `SessionEnd` | claude 进程退出 → orchestrator 知道 reset 完成 vs crash |

---

## 7. Sub-skill 调度 schema(V0.4.0 F60 EOL)

V0.4.0 F60 起 sub-skill 调度统一收敛到 Claude Code 原生 `Task` / `Skill` / `mcp__*` 工具,由 `.claude/agents/<role>.md` 自决;orchestrator 不编排。

---

## 8. Multi-session per project 协议

### 8.1 V0.4.0+ workflow.yaml 多 session fan-out

```yaml
# <project>/.ccteam/workflow.yaml(V0.4.6 F83)
name: ui-quality-loop
agents:
  explorer:
    trigger: manual
    output: .ccteam/issues
  fixer:
    trigger: watch:.ccteam/issues   # 监 dir 新文件
    parallelism: 10                 # 每文件一个 session,上限 10 并发
    input: .ccteam/issues
    output: .ccteam/fixes
  reviewer:
    trigger: watch:.ccteam/fixes
    parallelism: 3
```

- `parallelism > 1` **必须** `trigger: watch:` 配合(`WorkflowSpec::validate`
  §17.4 校验);`manual` / `schedule` / `gate` 单实例
- 每新 artifact(`.ccteam/issues/<file>.md`)触发一个新 session,**不**抢占
  老 session;到达 parallelism 上限时排队
- session 之间通过 artifact 输出/输入路径串联,**没**「master / sub-module」
  概念

### 8.2 Tmux session 命名

V0.4.0+ Claude session 走 `claude --bg --agent <role>`(非 tmux);只有
Codex CLI adapter session(F62 候选)走 tmux,命名 `ccteam-<slug>-<role>-<sid>`。
V0.3.1 flex session 仍 `ccteam-<slug>-<sid>`(详见 §1.4)。

### 8.3 资源约束

```yaml
# ~/.ccteam/config.yaml(V0.4.2 F73)
# workflow.yaml 内置 per-agent parallelism cap;config.yaml 仅留全局上限
max_total_sessions: 12              # 全局上限(跨所有项目)
hard_cost_kill_per_project_usd: 200 # CLAUDE.md §三红线物理上限
# V0.4.6 F84:per-project budget 在 workflow.yaml::budget 段(§17),不在此
```

V0.4.6 F84 起 per-workflow `budget.max_cost_usd_per_24h` /
`max_agent_spawns_per_hour` 是首选 cap(per-project 软档),物理 $200 上限是 daemon-wide 兜底。

---

## 9. Defense in Depth 输出协议(L2 / L3)

详见 tech-design §3.6 三层防御协议。本节仅定义对外输出格式。

### 9.1 audit agent 三档 verdict

每个 audit agent(architect / critic / designer / security / scope-watcher)输出:

```markdown
---
verdict: PASS | CONCERN | BLOCK
confidence: 0.0-1.0
audit_role: architect
---

## Findings
- (具体发现)

## Suggestion
- (修改建议;BLOCK 时必须给出 actionable diff 描述)
```

orchestrator 综合所有 audit 的 verdict:
- 全 PASS → 自动通过
- 任意 BLOCK → 进入 fix-cycle(详见 tech-design §3.5)或转 L3
- 有 CONCERN 但无 BLOCK → 按信任档位决定(yolo/balanced 通过;careful 上推 L3)

### 9.2 Cross-cutting watcher 输出格式(`progress.jsonl` 事件)

```jsonl
{"ts":"...","event":"watcher_pass","watcher":"cost-watcher","cost_usd":3.21}
{"ts":"...","event":"watcher_concern","watcher":"scope-watcher","note":"添加了 plan-eng 未声明的云同步特性"}
{"ts":"...","event":"watcher_block","watcher":"cost-watcher","note":"项目累计 $200 触发硬上限","action":"kill_and_escalate"}
```

### 9.3 L3 telegram fork 决策消息格式

```
📋 项目 <slug>:<phase> agent 议事拍不了板
方案摘要: ...
各 agent 立场:
  - architect: PASS
  - scope-watcher: BLOCK("加了云同步,超 spec")
  - critic: CONCERN("接口设计可改进")
  - designer: PASS

[A] approve: 接受当前实现,继续
[B] tweak: <reply 一句话调整>(例:"去掉云同步")
[C] reject: 退回 plan-eng 重做

(24h 不响应自动 A approve;careful 模式不超时)
```

用户回复(走 telegram bot 或 control 文件):
- `A` → 写 `~/.ccteam/control/fork-reply-<slug>.md` 内容 `A`
- `B <内容>` → 文件内容为 `B\n<内容>`
- `C` → 文件内容为 `C`

orchestrator 检测后注入下一 phase prompt 或回退到上一 phase。

### 9.4 信任档位(`config.yml`)

```yaml
trust_mode: balanced              # yolo | balanced | careful
                                  # yolo:    L3 永不弹(仅 L1 BLOCK 时 escalate)
                                  # balanced(默认): L3 仅在 L2 投票分裂时弹
                                  # careful: 任何 CONCERN 都弹
fork_timeout_hours: 24            # L3 默认通过超时(careful 模式忽略)
```

---

## 10. CLI 命令签名

### 10.1 启动 / 停止

```bash
ccteam start                           # 启动 orchestrator + web UI(默认 127.0.0.1:7331)
ccteam start --no-web                  # 只跑 orchestrator
ccteam start --no-clipboard            # 不尝试把 web bearer token 复制到 clipboard
ccteam stop                            # 优雅停机(保留 tmux session)
ccteam internal mcp-serve              # 作为 ccteam-mcp 跑 stdio MCP 协议(详见 §12)
```

`ccteam stop` 行为详见 §10.6 末。

### 10.2 提交需求

`ccteam new <slug>` 是 `ccteam init --in <projects_root>/<team>-<slug>/` 的 thin wrapper。`ccteam init` 在已有 git repo 上原地装(cwd 为目标)。

```bash
ccteam init                                      # cwd 安装(slug = cwd basename, team = dev)
ccteam init --slug myapp --team dev              # cwd 安装,显式 slug + team
ccteam init --in /work/repos/myapp               # 在 /work/repos/myapp 安装
ccteam new myapp --team dev                      # `ccteam init --in <projects_root>/dev-myapp/`
ccteam init --force                              # 重跑 cwd 时全覆盖 workflow.yaml + agents
ccteam init --reset-agents                       # 重跑 cwd 时只重写 .claude/agents/*.md
```

slug 决定:`ccteam init` 无 `--slug` 时取 cwd basename;`--slug NAME` / `ccteam new SLUG` 显式;team-prefix 自动补(`ccteam new myapp --team dev` → `~/projects/dev-myapp/`)。

### 10.3 查询状态

```bash
ccteam ls                              # 所有项目状态(human 表格)
ccteam ls --format json                # JSON 输出(给 LLM / 脚本用)
ccteam show <slug>                     # 单项目详情(含 session 状态、cost、最近 progress)
ccteam show <slug> --format json       # JSON 输出
ccteam status                          # V0.4.1:一屏 daemon 健康 + 所有项目 age + 最近 N progress events + web token
ccteam internal progress <slug> --tail # V0.4.6 F89:实时 tail progress.jsonl(老 `ccteam progress` 仍工作 + WARN)
```

**`--format json` 是强制项**——所有查询命令必须支持,让"用户自带 claude"路径通过 Bash 工具调时无需解析表格。

#### `ccteam ls --format json` schema

```json
{
  "projects": [
    {
      "slug": "bookmark-mgr-a3f9",
      "current_phase": "implement",
      "phase_state": "in_flight",
      "cost_used_usd": 1.23,
      "context_tokens_used": 412000,
      "tmux_session": "ccteam-bookmark-mgr-a3f9",
      "user_attached": false,
      "age_seconds": 13500,
      "last_event_ts": "2026-05-04T15:32:00Z",
      "stall_level": "ok"
    }
  ],
  "orchestrator": {
    "running": true,
    "active_count": 1,
    "max_concurrent": 3
  }
}
```

#### `ccteam show <slug> --format json` schema

§2.1 state.json 全量 + 派生字段(`recent_events`:progress.jsonl 末 50 条;`artifacts`:workflow.yaml 声明的 input/output dir 摘要;`stall`:`{level, silent_seconds}`;`recommendations`:operator hint 列表)。

### 10.4 进入项目 / 控制

顶层用户日常命令(其余移到 `ccteam internal`,详见 §10.6 末):

```bash
ccteam init                            # 项目安装/刷新(详见 §10.2)
ccteam new <slug> --team dev           # init 的 thin wrapper
ccteam start                           # 启动 daemon + web
ccteam stop                            # 优雅停机(详见 §10.1)
ccteam ls / show / status              # 查询(详见 §10.3)
ccteam pause <slug>                    # 暂停项目(不杀 session;走 `state.user_pause_pending=true`)
ccteam remove <slug> [--purge] [--dry-run] [--force]   # un-roster 项目(详见 §10.X remove)
ccteam doctor [flags]                  # 维护(详见 §10.6)
ccteam team / session                  # team factory / flex session 管理
ccteam web                             # 单独跑 web(`ccteam start` 默认带)
```

### 10.5 控制子命令

控制子命令(`attach` / `peek` / `progress` / `resume` / `send` / `spawn`)统一藏到 `ccteam internal <subcmd>`(详见 §10.6 末)。

### 10.6 维护

```bash
# 跨项目记忆走 Claude session 内官方机制:/memory 查 auto-memory,
# 直接编辑 ~/.claude/rules/ccteam-lessons-<team>.md 看 / 改跨项目 lessons。
ccteam doctor                                     # 体检:列出可用 mode flags
ccteam doctor --tool-surface                      # tool surface 交叉表(对 .claude/agents/ 检查)
ccteam doctor --install-skill                     # 写 ccteam-control skill
ccteam doctor --install-meta-agent                # 创建 meta-agent 项目
ccteam doctor --install-mcp                       # 在 ~/.claude.json 注册 mcpServers.ccteam(详见 §12)
ccteam doctor --install-all                       # = --install-mcp + --install-skill + --install-meta-agent
ccteam doctor --install-memory-bridge             # 写 ~/.claude/rules/ccteam-lessons-<team>.md 占位
ccteam doctor --reset-shipped-teams [--force]     # 从 in-binary bundle 重写 shipped team seeds
ccteam doctor --validate-team <name>              # 校验 team.yaml
ccteam doctor --screenshot-smoke <slug>           # 端到端 vt100 + imageproc 验证
ccteam doctor --migrate-workflow-to-ccteam-dir [--apply]
                                                  # 把根上 workflow.yaml 移到 `.ccteam/workflow.yaml`(默认 dry-run)
ccteam doctor --gc-claude-jobs [--apply]          # GC `~/.claude/jobs/<id>/` 已 terminal 且 > 7 天的目录;默认 dry-run
ccteam doctor --update-hooks [--dry-run]          # 扫所有项目 settings.json,改写顶层 `ccteam hook ...` 为 `ccteam internal hook ...`
ccteam web --bind 127.0.0.1:7331                  # web UI(详见 §15 + §16);`ccteam start` 默认带 web
ccteam web --bind 0.0.0.0:7331 [--no-auth]        # LAN 模式;非 loopback 默认强 token 鉴权
ccteam web --token-file <path>                    # 自定义 token 文件路径(默认 ~/.ccteam/web-token)
```

#### `ccteam stop` 行为契约

CLI 写 `/tmp/ccteam-<user>.shutdown` trigger 文件 → daemon 主循环 select 检到 → 触发 `shutdown_token: tokio::sync::Notify` → cancel 所有 event_loop(F82 cancel-token,每个 loop 写 `workflow_done reason="shutdown"`)→ JoinSet `join_all()` 等所有 task 退出;30s timeout → `abort_all()` + log WARN。SIGTERM / SIGINT 等价 trigger(systemd / docker stop 兼容)。**不杀任何 tmux session**(CLAUDE.md §三红线);`ccteam start` 下次启动 `ensure_session` 自动 reattach。

---

#### `ccteam remove <slug>` 行为契约

```bash
ccteam remove <slug>                   # config-only deregister(等价"abandon")
ccteam remove <slug> --purge           # 同时删 <project>/.ccteam/ + <project>/.claude/agents/ + workflow.yaml
ccteam remove <slug> --dry-run         # 打印要改的,不动文件
ccteam remove <slug> --force           # 跳过红线 refusal(慎用)
```

**Always 步骤**:

1. 从 `~/.ccteam/config.yaml::projects[]` 删该 slug
2. 走 F82 `unroster_project(slug)` 告知 daemon 热剔除(写 `workflow_done
   reason="removed"`)
3. 删 `~/.ccteam/progress/<slug>.jsonl`(或 flex 变体目录)、
   `~/.ccteam/inbox/<slug>/`、`~/.ccteam/control/<slug>/`(如有)

**`--purge` 额外**:`rm -rf <project>/.ccteam/` + `<project>/.claude/agents/` +
`<project>/workflow.yaml` + `<project>/.ccteam/workflow.yaml`(F83)。
**不动业务代码**(项目根的其他文件)。

**红线 refusal**(`--force` 跳过):

- 项目里有活的 tmux session(`tmux ls | grep ccteam-<slug>`)→ refuse + 提示
- 项目里有活的 claude bg job(`~/.claude/jobs/<id>/state.json::cwd == <project>` 且
  `state == working`)→ refuse + 提示
- 项目里有正在跑的 `agent_spawn` 没匹配 `agent_done`(progress.jsonl tail)
  → refuse + 提示 `ccteam show <slug>`
- **永远不删 `<project>/.env`**(用户密钥)

---

#### `ccteam internal` 隐藏子命令

非用户日常子命令藏到 `ccteam internal <subcmd>`:

```bash
ccteam internal hook <subcmd>          # Hook handlers(progress-append / load-context;Claude Code settings.json 调)
ccteam internal mcp-serve              # MCP server stdio JSON-RPC(`mcpServers.ccteam` 入口)
ccteam internal attach <slug>          # tmux attach(Codex CLI 路径)
ccteam internal peek <slug>            # tmux capture-pane 一次性看
ccteam internal progress <slug> [--tail]   # tail progress.jsonl
ccteam internal resume <slug>          # 恢复 paused 项目
ccteam internal send <slug> "..." [-r <role>] [--no-spawn]   # 写 inbox
ccteam internal spawn <slug> <role> ["prompt"]   # MCP `spawn_agent` 的 CLI 镜像
```

---

## 11. `ccteam-control` skill

让用户在自己的 Claude Code session 里调度 ccteam。架构论证见 [tech-design.md §3.8 / §6.7](./tech-design.md#38-用户接口层)。

**安装位置**:`~/.claude/skills/ccteam-control/SKILL.md`,通过 `ccteam doctor --install-skill` 写入。所有 claude session 自动可见。

**SKILL.md frontmatter**:`name: ccteam-control`,`allowed-tools: [Bash]`,`description` 字段必须明确"何时激活"(Claude Code 用 description 做 skill 选择决策)。

**SKILL body 必含**:能力清单(§10 CLI 命令摘录)/ 典型工作流 / 决策原则(`attach` vs `peek` vs `pause`)/ 不能做什么(不能替用户 attach tty,不能直接编辑 `.ccteam/` 元数据)。

与 ccteam-mcp 的关系:skill body 推荐"优先用 `mcp__ccteam__*` tools,fallback 到 Bash CLI(`--format json`)"。

### 11.5 Meta-agent role prompt

`ccteam doctor --install-meta-agent <user>` 落地两件事:

1. **项目骨架** `~/projects/<user>-meta/` —— 通过 `bootstrap_project(team=meta-agent)`
   生成,然后把 `state.json.tmux_session` 改成 `ccteam-meta-<user>`(注:与项目
   slug 派生的 `ccteam-<slug>` 区分,避免视觉混淆)。
2. **role prompt** `~/projects/<user>-meta/CLAUDE.md` —— 内嵌模板渲染,
   `<user>` 与生成时间替换。**必含 7 节**:你是谁 / 决策树 / 克制规则 /
   派单工具 / 监控规则 / inbox / outbox。

orchestrator 识别 `state.team == "meta-agent"` 走 `process_meta_project` 分支:

- 不跑 phase DAG;`current_phase` 永远空,`phase_state` 永远 `Idle`
- 仅做 `ensure_session`(常驻 tmux)+ `process_session_inbox`(吸收外部消息)
- context 超 60% 时仍走 `reset_context` 桥接 CLAUDE.md(M1.4);跨项目记忆走
  M4 主路径(`~/.claude/rules/ccteam-lessons-<user>-meta.md` 滚动累积 + auto-memory)

`MAX_CONCURRENT_PROJECTS = 3`(M1.2 锁定常量)只对常规项目生效;meta session
**永远不计入并发上限**。

---

## 12. `ccteam-mcp` MCP server(M2+)

把 ccteam 状态查询暴露为 MCP structured tool。架构论证见 [tech-design.md §6.4](./tech-design.md#64-mcp-servers)。

### 12.1 注册方式

```json
// ~/.claude.json 或 ~/.claude/mcp_servers.json
{
  "mcpServers": {
    "ccteam": {
      "command": "ccteam",
      "args": ["mcp-serve"],
      "env": {}
    }
  }
}
```

由 `ccteam doctor --install-mcp` 写入(M2 release)。`ccteam mcp-serve` 是 binary 子命令,stdio 协议。

### 12.2 暴露的 tool 清单(M2.5 起 9 tool;V0.2.2 F38 起 10 tool;V0.4.0 F65 起 17 tool)

| Tool 名 | 对应 CLI / 行为 | 入参 | 返回 |
|---|---|---|---|
| `ccteam__ls` | `ccteam ls --format json` | `{}` | §10.3 ls JSON schema(扩 `team`) |
| `ccteam__show` | `ccteam show <slug> --format json` | `{slug: string}` | §10.3 show JSON schema |
| `ccteam__new` | `ccteam new "..."` | `{prompt: string, team?: string}` | `{slug: string, workspace: string}` |
| `ccteam__peek` | `ccteam peek <slug>` | `{slug: string}` | tmux capture-pane stdout 字符串 |
| `ccteam__progress` | `ccteam progress <slug>` | `{slug: string, last_n?: number}` | `{events: [...]}` |
| `ccteam__pause` | 设 `state.user_pause_pending=true` | `{slug: string}` | `{ok: bool, slug: string, user_pause_pending: bool}` |
| `ccteam__resume` | `ccteam resume <slug>` | `{slug: string}` | `{ok: bool, slug: string}` |
| `ccteam__send_to_session`(M2.5 新)| 原子写 `<session>/.ccteam/inbox/msg-<ts>-NNN.md`(§3.4.2)| `{session: string, body: string, content_type?: "text"\|"markdown"}` | `{ok: bool, session: string, inbox_file: string}` |
| `ccteam__inject_decision`(M2.5 新)| 构造 ESCALATE-shape payload(§4.1.1),走 `send_to_session` 落 inbox | `{slug: string, escalate_kind: "revert_to_phase"\|"need_user_input"\|"abort"\|"insufficient_clarification"\|"phase_done_pending", args?: {target_phase?: string, reason?: string}}` | `{ok: bool, slug: string, inbox_file: string}` |
| `ccteam__screenshot`(V0.2.2 F38)| `tmux capture-pane -e` → `vt100::Parser` → `imageproc` → 写 `<project>/.ccteam/screenshots/<utc>.png` | `{slug: string, lines?: number}`(`lines` 默认 50) | 成功:`{ok: true, slug: string, path: string}`;graceful degrade:`{ok: false, slug: string, reason: string}` |
| `ccteam__spawn_agent`(V0.4.0 F65)| 在 `<project>/.ccteam/spawn_requests/<role>-<ts>.json` 写 spawn marker;F66 orchestrator 每 tick 消费 | `{slug: string, role: string, overrides?: object}` | `{ok: bool, slug, role, session_id, marker, note}` |
| `ccteam__stop_agent`(V0.4.0 F65)| 在 `<project>/.ccteam/stop_signal/<role>_<sid>` 写 soft-stop marker;`session_id` 为空 = 停该 role 所有 session(filename 用 `__all__` 占位)| `{slug: string, role: string, session_id?: string}` | `{ok: bool, slug, role, session_id, marker, note}` |
| `ccteam__observe_agents`(V0.4.0 F65)| 一次性读 `state.json::sessions`(V0.3.1 F49 registry);F66 会扩展 record 加 `role`/`status` | `{slug: string}` | `{slug, agents: [{session_id, role, harness, tmux_session, started_at, pid, status}]}` |
| `ccteam__signal`(V0.4.0 F65)| `pause`/`resume`/`interrupt` → `<project>/.ccteam/signal/<role>_<sid>` marker(F66 转 SIGSTOP/SIGCONT/SIGINT);`btw` → `actions::send_to_session_with` 走 inbox | `{slug: string, role: string, session_id?: string, signal: "pause"\|"resume"\|"btw"\|"interrupt", message?: string}` | `{ok: bool, slug, role, session_id, signal, marker/inbox_file}` |
| `ccteam__set_parallelism`(V0.4.0 F65)| 原子合并写 `<project>/.ccteam/workflow_overrides.json`(F66 每 tick reload);1≤N≤50 | `{slug: string, role: string, parallelism: integer}` | `{ok: bool, slug, role, parallelism, overrides_file}` |
| `ccteam__trigger_gate`(V0.4.0 F65)| 写 `<project>/.ccteam/gate_override/<role>`;`force=true` instruct F66 跳过 input-satisfaction check | `{slug: string, role: string, force?: boolean}` | `{ok: bool, slug, role, force, marker, note}` |
| `ccteam__get_artifact_summary`(V0.4.0 F65)| stat-only(O(n) on inode,不读 file 内容)遍历 `workflow.yaml` 所有 agent 的 `input`/`output` 目录 | `{slug: string}` | `{slug, artifacts: {<dir>: {count, latest, latest_mtime, size_bytes, exists}}}` |

V0.2.2 F38 红线:`screenshot` 是**只读**(daemon-independent),与 `peek` 同档,失败永不阻塞主路径(catch_unwind 兜 vt100/imageproc panic;tmux/font/IO 失败一律 `Ok(None)` → `{ok:false, reason}`)。截图字节流仅用于渲染,**不进入** `progress.jsonl` / `state.json` / state machine(CLAUDE.md §三红线"永不解析 tmux 终端输出")。字体走 vendored JetBrains Mono Regular(OFL,见 `LICENSES.md`),`CCTEAM_SCREENSHOT_FONT_TTF` env 可运行时覆盖(eg 切到 CJK / emoji 覆盖字体)。`ccteam doctor --screenshot-smoke <slug>` 跑端到端验证。

`send_to_session` / `inject_decision` 是 M2.5 增量(meta-agent 主消费者):
让 meta-agent 把用户的回复 / 决策推送回项目 session,**adapter 进程内不做
任何 NL 解析 / LLM 调用**,Symphony 反模式禁止(tech-design §3.1)。

`ccteam__inject_decision` 内部是 `send_to_session` 的 thin wrapper —— 把
`escalate_kind` 翻成 markdown payload(显式标记 `**META-AGENT DECISION**`
+ ESCALATE shape),然后走同一条 inbox 路径。

**V0.4.0 F65 7 个新工具**(meta-agent workflow control):底层是**文件系统控制平面**——
每个 mutating tool 写一个 marker 文件到 `<project>/.ccteam/<bucket>/`,F66 thin
orchestrator 每 tick 扫桶 → 执行操作 → 删 marker。文件桶清单:`spawn_requests/`、
`stop_signal/`、`signal/`、`gate_override/`、`workflow_overrides.json`。`observe_agents` /
`get_artifact_summary` 是**纯只读**(无 daemon 依赖),`spawn_agent` / `stop_agent` /
`signal` / `set_parallelism` / `trigger_gate` 走 `require_healthy_daemon()` gate(死
orchestrator 不会静默吞 marker)。schema/handler 实现在 `crates/ccteam-cli/src/mcp_workflow_tools.rs`。

### 12.3 不暴露的(M2 显式排除)

- `ccteam attach` — tty 交互,MCP 协议不适合
- `ccteam start / stop` — orchestrator 生命周期管理是 ops 决策,不让 LLM 误调
- ~~`ccteam memory rebuild`~~ — M4 简化后无自建索引,该命令不存在
- `ccteam doctor --install-*` — 单机配置变更,走 CLI(避免 MCP server 给 LLM 改 ~/.claude.json 的能力)

### 12.4 双消费者

| 消费者 | 用途 | 配置位置 |
|---|---|---|
| 用户自带 claude session(主) | 用户在任意目录开 claude → 通过 MCP 管 ccteam(详见 tech-design §3.8) | `~/.claude.json`(全局) |
| 项目级 claude(次) | phase 内自查"我在哪个 phase / 累计多少 cost" | `~/projects/<slug>/.mcp.json`(项目级) |

---

## 13. 关键文件路径速查

| 路径 | 用途 |
|---|---|
| `~/.ccteam/config.yml` | 全局配置(并发、阈值、信任档位、模型单价) |
| `~/.ccteam/watchdog.yaml` | V0.2 M0.21:translation-only watchdog 阈值 + notify_mode(详见 §12.5) |
| `~/.ccteam/inbox/` | 用户提需求 |
| `~/.ccteam/queue/<state>/` | 项目状态分桶 |
| `~/.ccteam/control/` | 用户 → orchestrator 控制信号(详见 §3.3) |
| `~/.ccteam/phases/` | phase 模板(详见 §5) |
| `~/.ccteam/templates/` | M2.4+:phase 可 @ 引用的 prompt 片段(`review-with-user-loop.md` / `kickoff-reverse-interview.md`) |
| `~/.claude/rules/ccteam-lessons-<team>.md` | 跨项目 lessons(M4 走官方 rules 机制;`<team>` ∈ {dev, product-research, ...}) |
| `~/.claude/projects/<encoded>/memory/` | 每项目 auto-memory(官方机制,Claude 自主写) |
| `~/.ccteam/progress/<slug>.jsonl` | 结构化事件流(详见 §4) |
| `~/.ccteam/log/<slug>/` | stream-json 归档(可选,调试用) |
| `~/.ccteam/tmux/<slug>.layout` | 项目 tmux pane 布局模板 |
| `~/.ccteam/state/orchestrator.json` | orchestrator 自身快照 |
| `~/projects/<team>-<slug>/.ccteam/` | 项目元数据(详见 §1.2;F22 后带 team 前缀) |
| `~/projects/<team>-<slug>/.ccteam/workflow.yaml` | V0.4.6 F83:workflow.yaml canonical 位置(详见 §17) |
| `~/projects/<team>-<slug>/workflow.yaml` | V0.4.0–V0.4.5 legacy fallback 位置(V0.5 删) |
| `~/projects/<team>-<slug>/.claude/agents/<role>.md` | V0.4.0 F63:每个 workflow agent role 的 prompt body(workflow.yaml 不带 prompt 字段) |
| `~/projects/<team>-<slug>/CLAUDE.md` | 自动生成的项目运营手册 |
| `~/projects/<slug>/.claude/settings.json` | 项目级 Claude Code 配置(详见 §6.1) |
| `~/projects/<slug>/.ccteam/sub-modules/<name>/` | multi-session 子模块元数据(M3+,V0.4.0 F60 后 EOL;详见 §1.3) |
| `~/.claude/jobs/<job_id>/state.json` | V0.4.0+ Claude `--bg` 真值:cost / status / cwd / context;F80 phantom cleanup + F85 GC + F91 cost SoT 都消费 |
| `~/.claude/jobs/<job_id>/output.log` | V0.4.0+ Claude `--bg` stdout/stderr;F90 Failure Inspector 读尾部 |
| `/tmp/ccteam-<user>.shutdown` | V0.4.6 F86:`ccteam stop` 写此文件 trigger graceful shutdown |

---

## 14. `ccteam-core` lib API 草案(M0 占位)

> **稳定性**:M0 起以 lib crate 提供,API **内部 unstable**——cli / hook / orchestrator 在同一 workspace 共用,可随时改;**M3 ratatui TUI 上线时定为 1.0**(届时被外部前端依赖,需要语义化版本与兼容承诺)。
>
> **三种前端共用此 API**:CLI(M0)/ ratatui TUI(M3+)/ web dashboard(M4+)都通过 `ccteam-core` 读写状态——与 §12 `ccteam-mcp` 是**同一套数据模型的两种 wire 方式**(lib API in-process / MCP stdio JSON-RPC)。

### 14.1 核心 API 函数签名

```rust
// crates/ccteam-core/src/lib.rs(示意签名,实际可调整)

/// 读单项目状态(对应 §2.1 state.json 的全量结构 + 派生字段;§10.3 `ccteam show --format json` 的内核)。
pub fn get_state(slug: &str) -> Result<ProjectState, CoreError>;

/// 列所有项目摘要(§10.3 `ccteam ls --format json` 的内核)。
pub fn list_projects() -> Result<Vec<ProjectSummary>, CoreError>;

/// 提交控制信号——写 §3.3 `~/.ccteam/control/` 文件,orchestrator 下一轮扫到生效。
/// `ControlSignal` 枚举覆盖 reject / pause / resume / answer / boost / fork-reply。
pub fn submit_control(slug: &str, signal: ControlSignal) -> Result<(), CoreError>;

/// 一次性读取 progress.jsonl 末尾 N 条事件(§10.3 `ccteam progress --tail` 的非流式入口)。
pub fn tail_progress(slug: &str, last_n: usize) -> Result<Vec<Event>, CoreError>;

/// 流式订阅 progress.jsonl(`tokio` Stream;inotify 监听末尾)。
/// TUI / web dashboard 实时事件推送的主接口。
pub fn attach_progress(slug: &str) -> Result<impl Stream<Item = Result<Event, CoreError>>, CoreError>;

/// 提交新需求(对应 §10.2 `ccteam new`),返回分配的 slug 与项目目录。
pub fn submit_inbox(spec: InboxSpec) -> Result<NewProjectHandle, CoreError>;

/// 一次性 capture 项目 tmux pane 当前屏(§10.4 `ccteam peek` 的内核;不 attach)。
pub fn peek_pane(slug: &str, lines: Option<usize>) -> Result<PaneCapture, CoreError>;
```

### 14.2 数据模型与 wire 格式对应

| `ccteam-core` 类型 | wire 格式(CLI `--format json` / MCP tool 返回) | 来源章节 |
|---|---|---|
| `ProjectState` | `ccteam show --format json` 全量 | §2.1 + §10.3 |
| `ProjectSummary` | `ccteam ls --format json` `projects[]` 元素 | §10.3 |
| `Event` | progress.jsonl 单行 | §4.1 |
| `ControlSignal` | `~/.ccteam/control/` 文件命名约定 | §3.3 |
| `InboxSpec` | `~/.ccteam/inbox/*.md` front matter + body | §3.1 |

新增前端**不应**直接读写文件系统——所有状态访问统一走 `ccteam-core`,确保 §6.1 hook 的 schema 与前端读端单一事实来源。

### 14.3 与 `ccteam-mcp` 的关系

§12 `ccteam-mcp` 的每个 tool 都是 `ccteam-core` 函数的 stdio JSON-RPC 包装:

| MCP tool(§12.2) | `ccteam-core` 函数 |
|---|---|
| `ccteam__ls` | `list_projects()` |
| `ccteam__show` | `get_state(slug)` |
| `ccteam__new` | `submit_inbox(spec)` |
| `ccteam__peek` | `peek_pane(slug, lines)` |
| `ccteam__progress` | `tail_progress(slug, last_n)` |
| `ccteam__pause` / `ccteam__resume` | `submit_control(slug, Pause/Resume)` |

→ M2 实现 `ccteam-mcp` 时是**薄壳**,不复制业务逻辑。

---

## 15. Web UI 路由(V0.3 已 ship)

> V0.3 起 `ccteam web --bind <addr>` 暴露本地 / 局域网 web UI。路由分两组:
> **stateless**(健康探针)+ **stateful**(消费 `CcteamPaths` 的 dashboard /
> 项目详情 / 静态资源 / SSE / 截图 / 写动作)。`auth_layer` middleware 包
> stateful 组(loopback bind 默认 disabled,非 loopback 默认 enabled)。

### 15.1 路由表

| Method | Path | 状态码 | Content-Type | 说明 |
|---|---|---|---|---|
| `GET` | `/health` | 200 | `application/json` | liveness:`{"status":"ok","version":"<crate>"}`;auth 例外 |
| `GET` | `/` | 200 | `text/html; charset=utf-8` | 项目列表 dashboard;空时 fallback 文案 `No projects` |
| `GET` | `/project/{slug}` | 200 / 404 | `text/html; charset=utf-8` / `text/plain` | 项目详情(state JSON / recent events / outbox / pane snapshot);未知 slug → 404 + plain text `project not found: <slug>` |
| `GET` | `/session/{slug}/{sid}` | 200 / 404 | `text/html; charset=utf-8` / `text/plain` | V0.3.1 F50:flex session 详情(per-session events / harness snapshot / sid-scoped controls);未知 sid → 404 |
| `GET` | `/assets/{file}` | 200 / 404 | `application/javascript; charset=utf-8`(htmx / htmx-ext-sse / xterm)/ `text/css; charset=utf-8`(style / xterm)/ `text/plain`(404) | vendored 静态资源;`Cache-Control: public, max-age=31536000, immutable`;白名单 = `htmx.min.js` / `htmx-ext-sse.js` / `xterm.js` / `xterm.css` / `style.css`,其他 file → 404 |
| `GET` | `/sse/all` | 200 | `text/event-stream` | 全局 SSE 流:每条 progress.jsonl 写入推一帧 |
| `GET` | `/sse/project/{slug}` | 200 | `text/event-stream` | per-slug SSE 流:server-side 过滤,只发该 slug 事件 |
| `GET` | `/sse/project/{slug}/{sid}` | 200 | `text/event-stream` | V0.3.1 F50:per-flex-session SSE 流:只发该 `(slug,sid)` 事件 |
| `GET` | `/sse/harness/{slug}` | 200 | `text/event-stream` | V0.3.1 F46:per-slug harness 状态流,该 slug 下所有 sid 的 statusline snapshot |
| `GET` | `/sse/harness/{slug}/{sid}` | 200 | `text/event-stream` | V0.3.1 F46:per-(slug,sid) harness 状态流,只发该 session 的 snapshot |
| `GET` | `/api/{slug}/pane-snapshot.ansi` | 200 / 504 | `application/octet-stream`(200)/ `text/plain`(504) | 按需 tmux ANSI snapshot;xterm.js 浏览器端只读渲染;headers:`x-ccteam-pane-rows` / `x-ccteam-pane-cols`;tmux 无 session → 504 |
| `GET` | `/api/{slug}/{sid}/pane-snapshot.ansi` | 200 / 404 / 504 | `application/octet-stream`(200)/ `text/plain`(404 / 504) | V0.3.1 F50:按 `state.json::sessions[{sid}].tmux_session` 捕获 flex session pane |
| `GET` | `/screenshot/{slug}.png` | 200 / 404 / 504 | `image/png`(200)/ `text/plain`(404 / 504) | 按需 PNG 截图;F38 unavailable / tmux 无 session → 504 + 文本 reason;非 `<slug>.png` 路径 → 404 |
| `GET` | `/screenshot/{slug}-{sid}.png` | 200 / 404 / 504 | `image/png`(200)/ `text/plain`(404 / 504) | V0.3.1 F50:flex session PNG 截图;`sid` 通过 master `state.json::sessions` 解析,slug 含 hyphen 兼容 |
| `POST` | `/api/{slug}/btw` | 303 / 400 / 4xx | `text/plain`(error) | `text=<urlencoded, 1..=4000>`;详 §15.7 |
| `POST` | `/api/{slug}/{sid}/btw` | 303 / 400 / 404 | `text/plain`(error) | V0.3.1 F50:`text=<urlencoded, 1..=4000>`,写 flex session 私有 inbox |
| `POST` | `/api/{slug}/inject_decision` | 303 / 400 / 5xx | `text/plain`(error) | `path=<absolute>&body=<urlencoded, 1..=8000>`;详 §15.7 |
| `POST` | `/api/{slug}/pause` | 303 / 4xx | `text/plain`(error) | 空 body;详 §15.7 |
| `POST` | `/api/{slug}/{sid}/pause` | 303 / 4xx | `text/plain`(error) | V0.3.1 F50:sid 校验后走 project-level pause,返回 `/session/{slug}/{sid}` |
| `POST` | `/api/{slug}/resume` | 303 / 4xx | `text/plain`(error) | 空 body;详 §15.7 |
| `POST` | `/api/{slug}/{sid}/resume` | 303 / 4xx | `text/plain`(error) | V0.3.1 F50:sid 校验后走 project-level resume,返回 `/session/{slug}/{sid}` |

### 15.2 dashboard 行(`/` 表格)

每行字段映射 `ccteam_core::ProjectSummary` + 派生:

| 列 | 来源 |
|---|---|
| Slug | `state.slug`(linked 到 `/project/<slug>`)|
| Team | `state.team` |
| Kind | `state.team_kind`(`workflow` / `multi_workflow` / `flex`) |
| Phase | `state.current_phase`(空时 `—`;`kind: flex` 不显示 workflow phase)|
| Last event | `Utc::now() - state.last_progress_event_at` 的 humanized 时长(`30s ago` / `15m ago` / `2h ago`)|
| Status | `silence_classifier::classify(events, silent_seconds, default thresholds)` 之一:`healthy` / `terminal` / `subagent` / `runaway` / `tool-hung` / `limbo` |
| Cost | `state.cost_used_usd`,`%.2f` 格式 |

**红线**:status badge 是**只读** label。即使分类是 `PostStopLimbo` /
`SubagentRunaway`,web 层 **不**调 `LimboAction::from` / 重新注入 / 软告警 —
仅 orchestrator 持续走 F35 副作用路径(详 `silence_classifier.rs` 头注 +
CLAUDE.md §三 read-only 红线)。

### 15.3 project 详情(`/project/<slug>`)

按渲染顺序:

1. **Header**:slug / team / current_phase / status badge / cost。
2. **State**:`serde_json::to_string_pretty(&state)` 进 `<pre>`(完整
   `state.json`,折叠 / 滚动由浏览器决定)。
3. **Recent events**:`collect_recent_events(paths, slug, 200)` 供 status badge
   只读分类;页面表格仅渲染最后 10 条,避免大日志卡住浏览器。
   每条 progress.jsonl,渲染 `ts` / `event` / `detail`(`detail` = `tool=<X>`
   / `phase=<X>` / `kind=<X>` / `count=<N>` 之一,前者优先)。
4. **Outbox**:`SessionMailbox::list_outbox` 后 reverse + truncate 20。
   每条 `<li>` 渲染 `event_kind`(lowercase debug 字面量 — `progress` /
   `reply` / `escalation` / `shipped` / `clarify`)+ `created_at` RFC3339 +
   `filename` + body 头 200 char。front-matter 解析失败渲染
   `(unparseable)` 占位,不 5xx。
5. **Pane snapshot**:页面加载即拉一次 `/api/<slug>/pane-snapshot.ansi`,
   由 vendored `@xterm/xterm` 浏览器端只读渲染;下方 button 触发
   cache-busting `?t=<ms>` 重新 fetch。`/screenshot/<slug>.png` 保留为 PNG
   fallback。**不 polling**(PRD §5.2.5)。
6. **Live events**:页面内嵌 `EventSource('/sse/project/<slug>')` 订阅 SSE
   流,新 `progress` 事件 prepend 到 `#events-tbody`,client-side 滑动窗口
   截到 10 行;`reconnect_hint` 帧出现 → 1s 后 `location.reload()`。
7. **写动作 forms**:`/btw`(自由文本 textarea,1..=4000)+
   `/inject_decision`(structured path/body)+ `/pause` / `/resume` 按钮 —
   全部 htmx `hx-post`,服务端 303 → 浏览器跟回详情页;详 §15.7 + §15.8。

`kind: flex` 项目详情页不渲染 workflow state / phase controls,改为:

- **Sessions grid**:`state.json::sessions` 每个 sid 一张卡,显示 harness
  badge / tmux session / status / last event / cost,点击进入
  `/session/<slug>/<sid>`。
- **Session screenshots**:卡片 PNG 使用 `/screenshot/<slug>-<sid>.png`;
  sid 通过 master state 校验,不靠字符串切 slug。
- **Live events**:项目页仍订阅 `/sse/project/<slug>` 聚合该 flex 项目所有
  session progress stream。

### 15.3.1 flex session 详情(`/session/<slug>/<sid>`)

仅对 `kind: flex` 项目有效。读路径按 sid 收敛:

1. **Header**:slug / sid / team / harness / tmux session / status / cost。
2. **Harness snapshot**:读 `~/.ccteam/harness/<slug>-<sid>.json`,并订阅
   `/sse/harness/<slug>/<sid>` 实时更新。
3. **Pane snapshot**:页面加载和手动 refresh 拉
   `/api/<slug>/<sid>/pane-snapshot.ansi`;PNG fallback 是
   `/screenshot/<slug>-<sid>.png`。
4. **Recent events**:只读
   `~/.ccteam/progress/<slug>/<sid>.jsonl`,页面 SSE 订阅
   `/sse/project/<slug>/<sid>`。
5. **写动作 forms**:`/api/<slug>/<sid>/btw` 写 session 私有 inbox;
   pause / resume sid 路由只做 sid 校验,然后沿用 project-level 控制状态。

### 15.4 静态资源协议

- `htmx.min.js` ← `crates/ccteam-web/assets/htmx.min.js`(htmx 2.0.4
  upstream snapshot,BSD-2-Clause;详 `LICENSES.md`)
- `htmx-ext-sse.js` ← `crates/ccteam-web/assets/htmx-ext-sse.js`(htmx 2.x
  SSE extension upstream snapshot,BSD-2-Clause)
- `xterm.js` / `xterm.css` ← `crates/ccteam-web/assets/xterm.*`
  (`@xterm/xterm` 6.0.0 upstream snapshot,MIT;详 `LICENSES.md`)
- `style.css` ← `crates/ccteam-web/assets/style.css`(本仓自写 ~4 KB,
  monospace + dark-mode-friendly,含 live-dot + terminal panel)

这些静态资源均通过 `include_bytes!` 编译期打包进 `ccteam` binary;`ccteam web` 自包含
启动,无 npm / Vite / build toolchain 依赖,模仿 V0.2.2 F38 vendored TTF
模式。`Cache-Control: public, max-age=31536000, immutable` — 同一 binary
版本下 bytes 永不变,新版 binary 释放后自然 ID 变更触发 cache miss。

### 15.5 数据流红线

- **`progress.jsonl` 是 SoT**:web 层一律走
  `ccteam_core::collect_recent_events(paths, slug, n)`(V0.3 M5.1 起从
  `ccteam-cli::commands` promote 到 `ccteam-core::queries` —
  `dev-coupling-audit.md` F45 / `prd.md` §4),**不解析 tmux 输出**(F38
  截图通过 `ccteam_core::render_screenshot` 在 M5.2 加,内部已 vt100 化,
  不算字符串解析)。`/api/<slug>/pane-snapshot.ansi` 只把 tmux ANSI bytes
  作为浏览器端 xterm 渲染输入返回,不进入任何状态机。M5.2 SSE watcher
  同样只读 progress.jsonl,**不接 tmux**。
- **`ccteam-web` MUST NOT depend on `ccteam-cli`**:`cargo tree -p
  ccteam-web | grep ccteam-cli` 必须 0 命中(`tests/dep_graph_test.rs`
  锁红线)。dashboard / project / assets / sse / screenshot handler 全部
  依赖 `ccteam-core::{queries, ProjectState, SessionMailbox,
  render_screenshot, ...}` 的 public surface。
- **永远不主动 kill**:web 层(包含写动作)不发 SIGINT / Ctrl-C /
  `tmux kill-session`,只走 `ccteam_core::actions::*`(M5.0 promote)走
  inbox + state.json 控制平面。
- **截图不 polling**:`/api/<slug>/pane-snapshot.ansi` 与
  `/screenshot/<slug>.png` 仅在用户 click / 页面加载时同步调一次,
  `Cache-Control: no-cache, must-revalidate`;tmux capture / F38 渲染都是
  按需观测面,polling 烧 CPU / bandwidth。

### 15.6 SSE wire format(M5.2)

三个 progress SSE endpoint(`/sse/all` + `/sse/project/<slug>` +
`/sse/project/<slug>/<sid>`)wire 格式完全一致;session endpoint 额外按
`ProgressUpdate.sid` 过滤:

```
event: progress
data: {"slug":"dev-foo","ts":"2026-05-10T12:34:56Z","event":"PostToolUse","tool":"Read",...}

event: progress
data: {"slug":"dev-foo","ts":"2026-05-10T12:34:57Z","event":"phase_done","phase":"plan-eng",...}

: keepalive

event: reconnect_hint
data: {"type":"reconnect_hint","reason":"Lagged(1024)"}

```

字段约定:

- `event:` 名固定 `progress`(future-proof — 新 event 类型加新 `event:` 名)。
- `data:` 是**单行 JSON**(SSE 协议要求),内容 = 原 progress.jsonl 行
  + server 注入的 `slug` 字段;flex 嵌套进度文件
  `<slug>/<sid>.jsonl` 还会注入 `sid`。若原行恰巧也含这些字段,server
  端覆盖之,以 watcher 解析的文件路径为准。
- `: keepalive` 注释行 15s 周期发出(axum `Sse::keep_alive` 默认),
  防 nginx / 反向代理默认 60s 空闲超时。
- `event: reconnect_hint` 出现 = 此 SSE 订阅者落后超过 broadcast 容量
  (1024 帧),server 主动断流;client 收到 → 关闭 EventSource,1s 后
  reload(htmx-ext-sse / vanilla EventSource 都 auto-reconnect)。

**watcher 拓扑**:`crates/ccteam-web/src/watcher.rs` 起一条专用 OS 线程,
单个 `notify::RecommendedWatcher` recursive 监 `~/.ccteam/progress/`;
每检测到 `<slug>.jsonl` 或 `<slug>/<sid>.jsonl` 的 Modify / Create 事件
→ 维护 per-file 字节
watermark(`HashMap<PathBuf, u64>`,Mutex 保护)→ 读 appended bytes →
按行 parse + JSON 校验 → 推 `tokio::sync::broadcast::Sender<ProgressUpdate>`
(capacity = `1024` 字面量,PRD §5.2.2 + dev-plan §8 grep red line)。
两个 SSE handler 都 `bus.subscribe()` 同一 broadcast。

**watermark 启动语义**:server 启动时,扫 `~/.ccteam/progress/` 全部
现存 `<slug>.jsonl`,记录当前文件大小作为 watermark 起点 — **不重放
历史**(连接进来的客户端只看到 connect-time 之后的事件,M5.4 retro 可
评估加历史回放选项,V0.3 不做)。新创建的文件(Create 事件)从 offset 0
开始读 — 创建本身已经发生在 server 启动后,这些字节是「实时」的。

**file rotation**:文件 size 缩小(truncate / rename)→ watermark 重置
为 0,replay 全部内容,记 `tracing::warn!`(罕见,主要为防御 corner case)。

### 15.6.1 Harness SSE wire format(V0.3.1 F46)

`/sse/harness/<slug>` 与 `/sse/harness/<slug>/<sid>` 共享同一格式 ——
`<slug>` endpoint 不过滤 sid,`<slug>/<sid>` endpoint server-side
filter 到精确 (slug, sid) 对:

```
event: harness_snapshot
data: {"slug":"dev-foo","sid":"claude-1","snapshot":{"harness":"claude-code","model_display_name":"Claude Sonnet 4.5","context_used_pct":12,"cost_usd_total":0.42,"rate_limit_pct":7,"cwd":"/home/u/projects/dev-foo","raw":{...},"captured_at":"2026-05-11T12:34:56Z"}}

: keepalive

event: reconnect_hint
data: {"type":"reconnect_hint","reason":"Lagged(1024)"}

```

字段约定:

- `event:` 名固定 `harness_snapshot`(future-proof — Codex / 其他 harness
  接入时仍走同名;`snapshot.harness` 区分语义)。
- `data:` 是**单行 JSON envelope**:`{slug, sid, snapshot}`。`snapshot` =
  完整 `ccteam_core::HarnessSnapshot` 序列化(详 §16),包含
  `model_display_name` / `context_used_pct` / `cost_usd_total` /
  `rate_limit_pct` / `cwd` / `raw` / `captured_at`。
- keep-alive + `reconnect_hint` 行为与 §15.6 一致(15s `:` 注释,
  Lagged → 主动断流)。

**watcher 拓扑**:`crates/ccteam-web/src/watcher.rs` 起一条 *sibling* OS
线程(单独于 progress watcher),`notify::RecommendedWatcher` non-recursive
监 `~/.ccteam/harness/`;每检测到 `<slug>-<sid>.json` 的 Modify / Create
事件 → 读完整文件 → `serde_json::from_str::<HarnessSnapshot>` → 推
`tokio::sync::broadcast::Sender<HarnessSnapshotEvent>`(独立 channel,
capacity = 1024)。文件名拆分规则:`_meta-<handle>` → ("_meta-<handle>",
"default");否则 `(slug)-(claude|codex)-N` 右向左匹配;不匹配的文件名
丢弃 + warn。`<name>.json.tmp` 跳过(写入侧 atomic tmp+rename)。

**单源真值红线**:harness/ 文件**仅供展示**。orchestrator state machine
**永远不读**该目录 — `progress.jsonl` 仍是唯一控制平面 SoT(详 PRD §3.3 +
CLAUDE.md §三)。

### 15.7 Write-action POST endpoints(M5.3)

V0.3 M5.3 加四个 form-encoded POST 端点,全部走
`ccteam-core::actions::*`(`docs/dev-coupling-audit.md` F45 close):

| Method | Path | Body(`application/x-www-form-urlencoded`) | 状态码 | 说明 |
|---|---|---|---|---|
| `POST` | `/api/{slug}/btw` | `text=<urlencoded, 1..=4000 chars>` | 303 / 400 / 5xx | 调 `actions::send_to_session(paths, slug, text)`;成功 → `Location: /project/<slug>` |
| `POST` | `/api/{slug}/inject_decision` | `path=<absolute, 必须前缀 ~/projects/<slug>/.ccteam/>&body=<urlencoded, 1..=8000 chars>` | 303 / 400 / 5xx | 调 `actions::inject_decision(paths, slug, DecisionInput)`;`path` 含 `..` 组件 / 非绝对 / 不在 `<project>/.ccteam/` 之下 → 400 |
| `POST` | `/api/{slug}/pause` | (空)| 303 / 4xx | 调 `actions::pause(paths, slug)` — 设 `state.user_pause_pending=true`,**不**杀 tmux session(CLAUDE.md §三 红线)|
| `POST` | `/api/{slug}/resume` | (空)| 303 / 4xx | 调 `actions::resume(paths, slug)` — 清 `user_pause_pending` + `user_attached`,`phase_state` 回 `Idle`,归档 `escalation.md`(若存在)|

**成功语义**:全部 303 See Other → `/project/<slug>`,浏览器自动跟随
回详情页;form 提交流程 = 用户写完 → submit → 303 → 重新加载详情页
看 inbox 落地 / state.json 翻位。

**输入校验红线**:

- `text` / `body` 长度上下界(`1..=4000` / `1..=8000`)在 route boundary
  做,超长直接 400;**不**让 `actions::send_to_session` 自己挡(actions
  层是 policy-free helper,长度策略归 channel 层)。
- `inject_decision` `path` 必须满足:`is_absolute()` + 不含 `Component::ParentDir`
  + `starts_with(paths.project_ccteam_dir(slug))`。这三条共同抗
  `~/projects/<slug>/.ccteam/../../../etc/passwd` 与裸 `/etc/passwd`。

**错误语义**:

- 400 + plain-text reason:输入校验失败、unknown slug(`actions::*`
  返 `no project / session named ...`)
- 5xx + plain-text:`inject_decision` 写盘失败 / `pause` / `resume`
  状态机错误(罕见)
- 写动作 handler **永不** 5xx 静默 — handler 内 `tracing::warn!`
  + 客户端拿到具体 reason,方便 dogfood debug

### 15.8 鉴权(M5.3)

ccteam web 默认走 token 鉴权,启动时根据 bind 地址决定:

| bind | `--no-auth` | enabled | token |
|---|---|---|---|
| `127.0.0.1:*` / `[::1]:*` | false | false(loopback 信任)| 不生成 |
| `127.0.0.1:*` / `[::1]:*` | true | false | 不生成 |
| 非 loopback(`0.0.0.0` / LAN IP)| false | **true**(默认开)| `~/.ccteam/web-token` 生成或读 |
| 非 loopback | true | false(显式 opt-out)| 不生成,stderr 大字 LAN-RCE 警告 + 5s Ctrl-C 倒计时 |

#### 15.8.1 Token 文件协议

- 路径:`~/.ccteam/web-token`(可经 `--token-file <path>` 覆盖)
- 内容:32-byte 随机 → lowercase hex(64 ASCII chars)+ trailing `\n`(load 时 `trim()`)
- 文件 mode:**`0o600`**(Unix `OpenOptions::create_new` + `mode(0o600)`)
- 已存在 + mode != 0600 → stderr 警告 + 继续加载(不 fail-closed,
  避免 dogfood 期间因为 umask race 锁死)
- 删除 + 重启 → 自动重生,token 不可重放

#### 15.8.2 Wire format

- 客户端 header:`Authorization: Bearer ccteam:<hex>`(constant-time
  比对,`subtle::ConstantTimeEq`,长度先短路再 ct_eq;长度泄露不影响
  threat model 因为 hex 长度公开 = 64)
- 浏览器首次访问可走 URL shim:`?token=ccteam:<hex>`,middleware:
  1. 验证 token,
  2. `Set-Cookie: ccteam_token=<hex>; HttpOnly; SameSite=Strict; Path=/`,
  3. 303 redirect 到去掉 `token` 参数的相同 URL
- 后续 GET / SSE 自动携带 cookie;auth_layer 优先 Bearer header,
  其次 cookie value(constant-time 同上)

#### 15.8.3 鉴权范围

- **整体权限**(用户 2026-05-10 决策,无 read/write 拆分):enabled 时
  所有 stateful router 路径(`/`, `/project/<slug>`, `/sse/...`,
  `/screenshot/...`, `/api/.../...`)都需要 Bearer header 或 cookie。
- **`/health` 例外**:留给 ops 监控,response body 仅
  `{status, version}`,无 project 状态泄漏。
- 非 loopback `--no-auth` 启动时 stderr 输出红色 ANSI 警告:
  `WARNING: --no-auth on non-loopback bind = LAN-wide RCE on
  bypassPermissions sessions.` + 5s 倒计时(`ServeOpts::no_auth_grace_secs`
  = `Some(5)` 默认,集成测试传 `Some(0)` 跳过 grace)。

#### 15.8.4 CSRF 防御

写动作 POST 必须 carries `Authorization: Bearer` header,即使
session cookie 有效。理由:

- 浏览器跨域 form-submit 不会附加 `Authorization`(header allowlist),
- 跨域 fetch / XHR 触发 CORS preflight,server 默认拒,
- 同源 form-submit 由 `project.html` inline JS 注入 Bearer header
  (token 从 server-side 模板渲染进 JS 字符串字面量),

→ 攻击者跨域 form 即使 cookie 自动附带也**无法**生成有效 POST。

XSS tradeoff:token 出现在 HTML attribute 而非纯 HttpOnly cookie。
被 XSS'd 时攻击者本来就能同源 fetch + 自动 cookie,所以 inline 不
增加 threat surface。

---

## 16. JSON API v1(V0.3.2 F52 引入,V0.4.6 F90 扩展)

> SPA(F53+)消费的等价 JSON 端点。askama HTML 路径 V0.3.2 F59 起
> 301 → SPA(legacy 兼容期保留 redirect)。共享 `auth_layer` 中间件
> (§15.8)。V0.4.6 F90 加 4 个新端点:`artifact_queue` / `cost_history` /
> `sessions/active` / `jobs/<job_id>/log`(WorkflowView 增强消费,详 §16.1)。

### 16.1 端点表

| Method | Path | Response | 说明 |
|---|---|---|---|
| GET | `/api/v1/projects` | `Vec<DashboardRow>` | dashboard 行的纯 JSON 序列化(§15.2 字段集) |
| GET | `/api/v1/projects/{slug}` | `ProjectSummary` | §16.2;§15.3 / §15.3.1 数据的 SPA 形态 |
| GET | `/api/v1/projects/{slug}/sessions/{sid}` | `SessionDetail` | §16.3;flex session detail 数据(`team_kind != flex` ⇒ 404) |
| GET | `/api/v1/projects/{slug}/artifact_queue` | `Vec<ArtifactQueueEntry>` | V0.4.6 F90:每个 `Trigger::Watch` 路径的 `{dir, count, oldest_age_s, newest_filename}`,实时 `fs::read_dir`(WorkflowView Artifact Queue 面板)|
| GET | `/api/v1/projects/{slug}/artifact_status` | `Vec<ArtifactStatusGroup>` | 扫 `<project>/.ccteam/<dir>/*.json` 按 top-level `.status` 字段分组计数(`{dir, total, counts: {status -> count}}`);跳过 `*-requests` / `*.archived` / `rules` / `inbox` / `outbox` / `spawn_requests` / 隐藏目录。零 schema 假设——任何带 `status` 字段的 JSON artifact 都被通用计数(WorkflowView Artifact Status 面板,对位旧 moongpt-harness 的 issue/PR/backlog 计数视图)|
| GET | `/api/v1/projects/{slug}/cost_history?window=24h\|7d` | `Vec<{hour_ts, cost_usd}>` | V0.4.6 F90:cost trend mini sparkline 数据源;按小时桶聚合 `progress.jsonl::agent_done.cost_usd`(F91 cost SoT) |
| GET | `/api/v1/projects/{slug}/sessions/active` | `Vec<ActiveSession>` | per-role live session 列表(`{role, job_id, started_at, cwd, cost_usd, model, context_remaining_pct}`),WorkflowView agent card 展开消费。`model` 取自 `state.json::respawnFlags[--model]`;`context_remaining_pct` 由 `linkScanPath` JSONL 最末 `message.usage` 之和 / model context window 推算(`[1m]` 后缀 → 1M tokens,其他 → 200K) |
| GET | `/api/v1/projects/{slug}/jobs/{job_id}/log?tail=200` | `{job_id, log: string, lines}` | V0.4.6 F90:read-only 读 `~/.claude/jobs/<job_id>/output.log` 尾部;errored agent card 点击 Failure Inspector 用 |
| GET | `/api/v1/auth/token` | `AuthToken` | §16.4;SPA bootstrap 检查 token 是否需要 |

未知 slug / 非 flex 项目 / 未知 sid / 未知 job_id ⇒ `404` + `{"error": "<msg>"}`(`Content-Type: application/json`)。

### 16.2 `ProjectSummary`

JSON shape:

```jsonc
{
  "slug": "demo",
  "team": "dev",
  "kind": "workflow",        // workflow | multi_workflow | flex
  "is_flex": false,
  "current_phase": "implement",
  "badge_class": "badge-ok",
  "badge_label": "ok",
  "cost_label": "0.42",      // formatted "{:.2}"
  "created_at": "2026-05-12T17:08:00+00:00",  // RFC3339
  "sessions": [],            // SessionCard[] (flex 项目才非空,§15.3.1 字段一致)
  "state": { ... },          // 完整 ProjectState (serde_json::Value;非 pretty-printed)
  "events": [ {"ts":"...","event":"PostToolUse","detail":"tool=Edit"}, ... ],
  "outbox": [ {"filename":"...","kind":"...","created_at":"...","preview":"..."}, ... ],
  "decision_candidates": ["/absolute/path/decision-x.md", ...]
}
```

与 askama [`ProjectTemplate`] 对比的两点差异(故意):

1. `state` 是结构化 JSON,**不是** `state_json_pretty` 字符串。SPA 自决定缩进 / 高亮。
2. **不含** `auth_enabled` / `auth_wire_token` 字段;token 单独走 §16.4 endpoint,
   避免在列表 / 详情 JSON 中泄露。

### 16.3 `SessionDetail`

JSON shape(只 flex 项目):

```jsonc
{
  "slug": "flex-demo",
  "sid": "claude-1",
  "team": "flex",
  "kind": "flex",
  "harness": "claude",       // claude | codex
  "harness_class": "harness-claude",
  "tmux_session": "ccteam-flex-demo-claude-1",
  "started_at": "2026-05-12T17:08:00+00:00",
  "status_class": "badge-ok",
  "status_label": "ok",
  "cost_label": "1.23",
  "events": [...],
  "outbox": [...],
  "decision_candidates": [...],
  "harness_snapshot": {
    "model": "Claude Opus 4.7",
    "context_used_pct": "17%",
    "cost_usd_total": "1.23",
    "rate_limit_pct": "4%",
    "captured_at": "2026-05-12T17:08:30+00:00"
  }
}
```

`harness_snapshot` 字段:Claude session 通过 `claude_job::probe_job` 读 `~/.claude/jobs/<id>/state.json` 即时构造;Codex session 走 `~/.ccteam/harness/<slug>-<sid>.json`(Codex CLI adapter 自写);文件 / job 不存在时 `null`。

### 16.4 `AuthToken`

`GET /api/v1/auth/token`:

```jsonc
// auth.enabled == false (loopback default):
{"wire_token": null}

// auth.enabled == true:
{"wire_token": "ccteam:<hex>"}
```

SPA 用此值判断是否需要 token-entry flow。auth_layer 仍 gates 此 endpoint;
浏览器首次访问需走 `?token=ccteam:<hex>` URL-shim cookie 路径(§15.8.2)。

### 16.5 写动作端点(content-type 协商)

§15.7 中 7 条 POST endpoint(`/api/{slug}/{btw,inject_decision,pause,resume}`
+ `/api/{slug}/{sid}/{btw,pause,resume}`)V0.3.2 起接受双 content-type:

| `Content-Type` | 成功响应 | 失败响应 |
|---|---|---|
| `application/x-www-form-urlencoded`(默认 / 现有 htmx 路径) | `303 See Other` → `/project/<slug>` 或 `/session/<slug>/<sid>` | 4xx/5xx + 纯文本 body |
| `application/json` | `200 OK` + `{"ok":true}` | 4xx/5xx + `{"ok":false,"error":"<msg>"}` |

无 `Content-Type` header(pause / resume 旧 form 走 form-encoded 空 body)
默认走 form 分支,保持现有 htmx 契约。

### 16.6 不破坏的红线

- progress.jsonl 仍是 orchestrator 状态唯一事实来源;JSON API 仅 read +
  写动作,不解析 / 不回写。
- token 不出现于 `/api/v1/projects` / `/api/v1/projects/{slug}` /
  `/api/v1/projects/{slug}/sessions/{sid}` JSON;仅 `/api/v1/auth/token` 暴露。
- HTML 路径 §15 完全不动;F59 才下线。

---

## 17. `workflow.yaml` schema(V0.4.0 F63 引入,V0.4.6 F82/F83/F84 扩展)

> 完整解析器:`crates/ccteam-core/src/workflow.rs`(`WorkflowSpec` /
> `AgentSpec` / `BudgetSpec` / `Trigger` / `Executor` / `OnTimeout` /
> `WorkflowError`)。`WorkflowSpec::load_for_project(<dir>)`:**V0.4.6
> F83 起 canonical 位置是 `<dir>/.ccteam/workflow.yaml`**,优先读;
> fallback `<dir>/workflow.yaml`(V0.4.0–V0.4.5 legacy,V0.5 删)。
> `ccteam init` / `ccteam new` V0.4.6 起生成到 `.ccteam/` 下;
> `ccteam doctor --migrate-workflow-to-ccteam-dir --apply` 把旧根位置文件
> 一次性迁过去。

### 17.1 顶层字段

```yaml
name: <string>            # 必填。workflow 标识(项目内唯一)
description: <string>     # 可选。供 meta-agent / web UI 展示
enabled: <bool>           # V0.4.6 F82:可选,默认 true。false → daemon 跳过 roster + 热改时优雅 cancel 老 loop(写 `workflow_done reason="disabled"`)。`true`(默认值)序列化时省略(只 opt-out 行 `enabled: false` 渲染)
budget:                   # V0.4.6 F84:可选,默认 None(no-op)。详 §17.2.1
  max_cost_usd_per_24h: <f64>      # 滑窗 24h cost cap;sum(progress.jsonl::agent_done.cost_usd 24h 内) >= 此值 → trip
  max_agent_spawns_per_hour: <u32> # 滑窗 1h spawn rate cap;count(agent_spawn 1h 内) >= 此值 → trip
agents:                   # 必填。map<role-name, AgentSpec>;非空
  <role-name>: <AgentSpec>
  ...
```

- `agents` 解析为 `IndexMap<String, AgentSpec>`,**保留 YAML 声明顺序**——
  trigger graph 构建 / 日志输出 / fixture round-trip 都依赖此顺序确定性。
- `role-name` 只允许 `[a-z0-9_-]`,长度 ≥ 1;**必须**对应
  `.claude/agents/<role>.md`(orchestrator 启动时校验,F66 落地)。
- **V0.4.6 F82 热加载**:daemon 为每个 rostered 项目装 inotify watch on
  workflow.yaml 文件本身;mtime + hash 变化时解析新 spec → diff 老 spec
  → `enabled: false` 触发 cancel-token(`workflow_done reason="disabled"`),
  `agents` 拓扑突变触发 cancel + 重启(`reason="reloaded"`);syntax error
  保留老 loop 不动(fail-safe)。

### 17.1.1 V0.4.6 F84 `BudgetSpec` 字段

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `max_cost_usd_per_24h` | `Option<f64>` | `None`(不报) | 滑窗 24h cost cap;trip 时写 `budget_exceeded` 事件 + 自动 `enabled: false`(走 F82 cancel-token,`workflow_done reason="budget_exceeded"`)|
| `max_agent_spawns_per_hour` | `Option<u32>` | `None`(不报) | 滑窗 1h spawn count cap;防 self-excitation runaway(2026-05-16 dex-ui explorer 自激励 4h 烧 $1.10 实证)。trip 同上 |

`BudgetSpec::None`(`budget` 字段不写)等价 V0.4.5 行为(no budget cap)。
F84 budget guard 在 `try_spawn` 入口跑;cost 数据源走 F91 cost SoT(`agent_done.cost_usd` 24h 聚合 + active `~/.claude/jobs/<id>/state.json::cost_usd_total`)。

### 17.2 `AgentSpec` 字段

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `executor` | `claude` \| `codex` | `claude` | 选择哪个 harness 二进制(F61 ClaudeCodeAdapter / F62 CodexAdapter)|
| `trigger` | scalar string | 必填 | 见 §17.3 |
| `parallelism` | `u32` | `None`(等价 1) | 同时最多多少个 session 实例。`> 1` **仅** `watch:` 合法 |
| `input` | path | `None` | artifact 输入目录(相对项目根),F64 watcher 派发时通过 `CCTEAM_INPUT` env 注入 spawned harness |
| `output` | path | `None` | artifact 输出目录,通过 `CCTEAM_OUTPUT` 注入 |
| `interval` | duration string | `None` | 仅 `trigger: schedule` 有效(V0.4.0 占位,V0.4.1 接 cron)|
| `timeout` | duration string | `None` | 单 session 软超时(F64+ watchdog 消费)|
| `on_timeout` | `escalate` \| `retry` \| `skip` | `None`(等价 `escalate`) | 超时动作 |

**红线(schema 级 hard error)**:`workflow.yaml` 内**不允许**出现
`prompt:` / `system_prompt:` / `messages:` 字段——所有 prompt 内容
住在 `.claude/agents/<role>.md`,不进 workflow.yaml。

### 17.3 `trigger` 标量字符串语法

| 形式 | `Trigger` 变体 | 语义 |
|---|---|---|
| `manual` | `Trigger::Manual` | meta-agent 或用户显式 `ccteam trigger <role>` 才派发 |
| `schedule` | `Trigger::Schedule` | 定时(V0.4.0 stub:meta-agent 手动触发占位;V0.4.1 接 `interval`)|
| `gate` | `Trigger::Gate` | 等 `trigger_gate` MCP 调用解锁(必须有 `input`)|
| `watch:<path>` | `Trigger::Watch(PathBuf)` | F64 inotify watcher 监 `<path>` 新文件 → 派发 |

### 17.4 校验规则(`WorkflowSpec::validate`)

1. `agents` map 非空。
2. role 名字符集 `[a-z0-9_-]`,非空。
3. `trigger: watch:<path>` 的 `<path>` 非空(`watch:` 单独 → `ValidationFailed`)。
4. `trigger: gate` 必须有 `input`。
5. `parallelism > 1` 只允许 `watch:` trigger;`schedule` / `gate` / `manual` 单实例。

### 17.5 `WorkflowError` 变体

| 变体 | 触发 |
|---|---|
| `NotFound(PathBuf)` | `load_for_project` 两处都不存在 |
| `ReadFailed(io::Error)` | 文件系统读失败(权限 / EIO 等)|
| `ParseFailed(serde_yaml::Error)` | YAML 语法 / 未知 enum 变体(如 `executor: unknown`)|
| `ValidationFailed(String)` | 上述 5 条结构校验失败,String 携带 role + 原因 |

### 17.6 Fixture 参考

- `crates/ccteam-core/tests/fixtures/workflow-ui-quality-loop.yaml` ——
  4 个 agent(`explorer` manual / `fixer` watch+parallel=10 /
  `reviewer` watch codex / `shipper` gate)
- `crates/ccteam-core/tests/fixtures/workflow-research-loop.yaml` ——
  2 个 agent(`claw` manual / `evaluator` watch+parallel=5)

### 17.7 不破坏的红线

- workflow.rs 是**纯数据 + 校验**:不写 `progress.jsonl`,不动 tmux,
  不接 MCP。F64 watcher / F65 MCP / F66 orchestrator 才接 IO。
- 不出现 team 名字面量(无 `"dev"` / `"qa"` / `"ccteam"` 等;test 文本
  除外)。F66 orchestrator 通过 role 名(用户定义数据)驱动调度。
