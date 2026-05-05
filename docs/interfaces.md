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
├── config.yml             # 全局配置(并发上限、API key、bot token、信任档位、模型单价表)
├── inbox/                 # 待 triage 的需求
├── queue/
│   ├── seeding/
│   ├── planning/
│   ├── coding/
│   ├── reviewing/
│   ├── done/
│   └── archive/
├── phases/                # phase 模板(启动时复制到项目)
│   ├── 00-seed.md
│   ├── 01-plan-ceo.md
│   ├── 02-plan-eng.md
│   ├── 03-implement.md
│   ├── 04-test-author.md
│   ├── 05-test-run.md
│   ├── 06-fix.md
│   ├── 07-review.md
│   ├── 08-score.md
│   └── 09-ship.md
├── templates/             # M2.4+: phase 可 @ 引用的 prompt 片段(原生 @,orchestrator 不解析)
│   ├── review-with-user-loop.md
│   └── kickoff-reverse-interview.md
├── control/               # 用户 → orchestrator 控制信号(详见 §3.3)
├── memory/                # 跨项目记忆(M3+)
│   ├── patterns/
│   ├── anti-patterns/
│   └── index.json         # RAG 索引元数据
├── progress/
│   └── <slug>.jsonl       # 每项目一个事件流(hooks 写,inotify 监听;详见 §4)
├── log/
│   └── <slug>/            # stream-json 归档(可选,调试用)
├── tmux/
│   └── <slug>.layout      # 项目 tmux pane 布局模板
└── state/
    └── orchestrator.json  # orchestrator 自身 in-memory 状态的快照
```

### 1.2 项目级目录(`~/projects/<slug>/`)

```
~/projects/<slug>/
├── src/                          # 实际代码
├── tests/
├── package.json / pyproject.toml
├── CLAUDE.md                     # 项目级运营手册(自动生成,详见 tech-design §6.5)
├── .ccteam/                      # ccteam 元数据(git 跟踪)
│   ├── spec.md                   # 用户原始需求 + Seed 后澄清
│   ├── plan-ceo.md
│   ├── plan-eng.md
│   ├── architecture.md
│   ├── implement-report.md
│   ├── test-report.md
│   ├── review-report.md
│   ├── scorecard.md              # M2+
│   ├── code-review.md            # sub-skill 产物示例(详见 §7)
│   ├── state.json                # 项目级状态机(详见 §2.1)
│   ├── escalation.md             # 触发用户介入时写这里
│   ├── fix-loop.state.md         # fix-cycle 内部状态(ralph-loop 风格)
│   └── ready                     # SessionStart hook 写出的就绪标记
├── .claude/
│   └── settings.json             # 详见 §6.1
└── .gitignore
```

### 1.3 Multi-session 项目子模块布局(`parallelism: multi_session`)

仅当 `parallelism: multi_session` 时启用(M3+)。在 §1.2 基础上扩展:

```
~/projects/<slug>/
├── .ccteam/
│   ├── state.json                # master state(项目级,详见 §2.2)
│   ├── interface-contracts.md    # 子模块间接口契约(fan-out 时定下,fan-in 时验证)
│   └── sub-modules/
│       ├── backend-api/
│       │   ├── state.json        # sub-module state(与单 session 一致;详见 §2.3)
│       │   └── progress.jsonl    # 本子模块独立事件流
│       ├── frontend-dashboard/
│       ├── mobile-app/
│       └── docs/
├── backend-api/                  # 子模块代码(独立目录)
├── frontend-dashboard/
├── mobile-app/
└── docs/
```

---

## 2. State 协议

### 2.1 项目级 `state.json`(单 session 项目)

```json
{
  "slug": "bookmark-mgr-a3f9",
  "team": "dev",
  "created_at": "2026-05-04T10:23:00Z",
  "tmux_session": "ccteam-bookmark-mgr-a3f9",
  "claude_session_id": "abc123-def-456",
  "claude_pid": 12345,
  "phase_state": "in_flight",
  "current_phase": "implement",
  "parallelism": "solo",
  "phase_history": [
    {"phase": "seed",     "status": "passed", "duration_s": 90, "cost_usd": 0.12},
    {"phase": "plan-ceo", "status": "passed", "duration_s": 45, "cost_usd": 0.08},
    {"phase": "plan-eng", "status": "passed", "duration_s": 60, "cost_usd": 0.15}
  ],
  "fix_cycle_count": 0,
  "cost_used_usd": 1.23,
  "soft_warn_threshold_usd": 20.0,
  "hard_kill_threshold_usd": 200.0,
  "context_tokens_used": 142000,
  "context_reset_threshold_tokens": 600000,
  "context_reset_count": 0,
  "last_progress_event_at": "2026-05-04T11:23:45Z",
  "last_event_type": "Stop",
  "last_user_interaction_at": "2026-05-04T10:23:00Z",
  "user_attached": false,
  "user_pause_pending": false
}
```

**`phase_state` 枚举**:`in_flight` / `idle` / `fix_locked`(详见 tech-design §3.2)。

**`parallelism` 枚举**:`solo` / `agent_team` / `multi_session`(详见 §5.1 phase schema)。

**`team` 字段**(M3.1 F13):指定项目跑哪个团队的 phase 集合(默认 `dev`,M3.4 加 `research` 等)。serde 默认值 `"dev"`,所以 M3.1 之前写出的 state.json 自动以 dev 团队加载,无需迁移脚本。

**原子写入**:`.tmp` + `rename`;启动校验 schema,损坏走 backup。

### 2.2 Master `state.json`(`parallelism: multi_session`)

在 §2.1 基础上扩展子模块状态摘要:

```json
{
  "slug": "saas-platform-x9f2",
  "parallelism": "multi_session",
  "current_phase": "fan-out",
  "phase_state": "in_flight",
  "sub_modules": {
    "backend-api":          {"current_phase": "implement", "phase_state": "in_flight"},
    "frontend-dashboard":   {"current_phase": "implement", "phase_state": "idle"},
    "mobile-app":           {"current_phase": "test-run",  "phase_state": "in_flight"},
    "docs":                 {"current_phase": "ship",      "phase_state": "idle"}
  },
  "max_sessions_per_project": 4,
  "...": "上述 §2.1 字段全部保留"
}
```

**项目级 phase 序列**:`plan` → `fan-out` → `implement-parallel` → `fan-in` → `review` → `ship`。

### 2.3 Sub-module `state.json`(multi-session 内)

字段与 §2.1 完全相同;只是粒度是子模块而非项目。

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

### 3.4 Per-session Inbox / Outbox(M1.1 — channel layer 接入面契约)

> tech-design §2.1 三层架构里 **Channel Layer ↔ User Interaction Layer** 的
> 接入面契约。channel adapter(Telegram bot 等,M2+)写入 inbox,session 内
> 的 claude 处理后写入 outbox,channel adapter 把 outbox 推回外部消息系统。
> **adapter 进程内不嵌 LLM**——所有 NL 解析都在 session 内的 claude 完成。

#### 3.4.1 目录布局

每条 ccteam-managed long session(meta-agent + 项目 sessions)都有自己的 inbox/outbox:

```
# meta-agent session(M1.0)
~/projects/<user>-meta/.ccteam/
├── inbox/
│   ├── msg-2026-05-06T103000Z-001.md
│   └── msg-2026-05-06T104215Z-002.md
└── outbox/
    ├── reply-2026-05-06T103045Z-001.md
    └── reply-2026-05-06T104230Z-002.md

# 项目 session
~/projects/<slug>/.ccteam/
├── inbox/                # 同 schema,M1.1 起接受 NL 注入
└── outbox/               # 同 schema,session 写出可被外部消费的回应
```

文件名:`{msg|reply}-<ISO-timestamp>-<seq>.md`(timestamp 紧凑去冒号,seq 是
3 位 zero-padded 序号)。原子写入(`.tmp` 先写再 `mv`)。

#### 3.4.2 Inbox 文件 schema

```markdown
---
schema_version: 1                      # 协议演进时升;M1 = 1
source: telegram                        # telegram | feishu | slack | terminal | cli | <adapter-name>
source_chat_id: "@rob_personal"         # 可选:外部 channel 的会话标识(用于回推路由)
source_msg_id: "tg-msg-12345"           # 可选:外部消息 ID(用于回推时引用)
source_user: rob                        # 必选:外部 channel 上的用户标识
created_at: 2026-05-06T10:30:00Z        # 必选:消息发起时间(以 channel 为准)
ingested_at: 2026-05-06T10:30:01Z       # 必选:adapter 写入 inbox 时间
content_type: text                      # text | markdown | image_url | file_path(M2+)
attachments:                            # 可选:多媒体附件(M2+)
  - kind: image_url
    url: https://...
---

# NL message body

做一个本地书签管理器,离线可用,按域名归类。
```

**必选字段**:`schema_version` / `source` / `source_user` / `created_at` /
`ingested_at` / `content_type`。其它**全部可选**——adapter 知道什么填什么。

#### 3.4.3 Outbox 文件 schema

```markdown
---
schema_version: 1
in_reply_to: msg-2026-05-06T103000Z-001.md   # 可选:对应 inbox 文件名(adapter 用来 thread)
in_reply_to_source_msg_id: "tg-msg-12345"    # 可选:外部 msg id,adapter reply 时用
target_channels:                              # 可选:adapter 路由提示(空 = 推回 source)
  - telegram
created_at: 2026-05-06T10:30:45Z              # session 写出时间
priority: normal                              # normal | high(escalation 用 high)
event_kind: reply                             # reply | progress | escalation | shipped | clarify
---

# NL reply body

收到了。我已经用 `ccteam new` 派单给 dev 团队,slug = bookmark-mgr-a3f9。
预计 30 分钟内 plan-eng 完成,有 escalation 我会同步。
```

`event_kind` 决定 adapter 推送优先级:
- `reply` — 普通对话回应
- `progress` — phase 推进里程碑(adapter 可降级为静音通知)
- `escalation` — 需用户决策(adapter 必须可见提醒)
- `shipped` — 项目终态(adapter 可绑前缀 emoji)
- `clarify` — phase 内 CLARIFY 问题(adapter 应保持线程上下文)

#### 3.4.4 Adapter 的责任边界

channel adapter(M2+ 各自实现):

1. **入向**:订阅外部消息,翻译成 §3.4.2 schema,原子写入对应 session 的 inbox
2. **出向**:轮询(或 inotify watch)对应 session 的 outbox,翻译 §3.4.3 schema
   推到外部消息系统;**推送成功后删除 outbox 文件**(adapter 负责 ack)
3. **路由**:adapter 维护"外部 channel 上下文 ↔ session"映射(例:Telegram chat
   id ↔ slug);**映射状态存 adapter 自己的持久化里**,ccteam 不关心
4. **错误重试**:外部系统不可达时,outbox 文件保留,adapter 重连后追传

**adapter 不允许做的事**:
- 解析 inbox/outbox 内容做语义判断(那是 session 内 claude 的活)
- 写 progress.jsonl 或其他 ccteam 状态文件
- 起任何 LLM 调用(Symphony 反模式禁止)

#### 3.4.5 Orchestrator 怎么处理 inbox

orchestrator 在 session inbox 上挂 inotify。新文件落地时:

1. 读 inbox 文件,提取 body
2. 检查对应 session 的 idle 状态(progress.jsonl 末尾事件,见 [tool-surface §2.2.1](./claude-code-tool-surface.md))
3. **idle**:`tmux send-keys` 直接注入 body
4. **busy**:用 `/btw <body>` 注入(claude 内部排队,phase 跑完处理)
5. 处理完成后**删除 inbox 文件**(orchestrator 负责 ack)
6. 追加事件 `{"event":"inbox_consumed","msg_file":"...","session":"..."}` 到
   progress.jsonl

#### 3.4.6 Session 内 claude 怎么写 outbox

meta-agent session 与项目 session 的 role prompt(`.ccteam/CLAUDE.md`)显式写
"产出对外消息时用 Write 工具写到 `outbox/reply-<ts>-<seq>.md`,字段按
interfaces §3.4.3"。具体写哪些事件:

- meta-agent:每条对用户的 NL 回复
- 项目 session:phase_done / escalation / cost-watcher 告警(由 phase 模板的
  `outbox_on_phase_done` 字段控制,M3 团队抽象时可定制 per-team)

#### 3.4.7 与 §3.1 全局 inbox 的关系

§3.1 全局 `~/.ccteam/inbox/<ts>.md` 是 M0 的"提想法"入口,**M1 之后保留作为
备用路径**——用户可以不通过 meta-agent / channel,直接 `echo` 文件到全局 inbox
让 orchestrator 起项目。M1+ 推荐路径是通过 meta-agent session 的 inbox。

---

## 4. Progress.jsonl 事件流

每个项目一个 `~/.ccteam/progress/<slug>.jsonl`。**这是 orchestrator 唯一的状态事实来源**——tmux 终端输出只给人看,不解析。

### 4.1 事件类型(完整清单)

```jsonl
{"ts":"2026-05-04T11:23:00Z","event":"session_start","tmux_session":"ccteam-bookmark-mgr-a3f9"}
{"ts":"...","event":"phase_inject","phase":"implement"}
{"ts":"...","event":"PreToolUse","tool":"Edit","path":"src/db.ts"}
{"ts":"...","event":"PostToolUse","tool":"Bash","cmd":"pnpm test","exit_code":0,"duration_ms":4521}
{"ts":"...","event":"phase_milestone","phase":"implement","note":"完成 schema + migration"}
{"ts":"...","event":"phase_done","phase":"implement","duration_s":4521,"cost_usd":2.13}
{"ts":"...","event":"escalate","kind":"need_user_input","target_phase":null,"reason":"db migration 不可调和","cycle":3}
{"ts":"...","event":"escalate","kind":"revert","target_phase":"plan-eng","reason":"fix-loop 撞顶,根因在选型"}
{"ts":"...","event":"escalate","kind":"abort","target_phase":null,"reason":"超出 ccteam 当前能力,人工接手"}
{"ts":"...","event":"user_attach","detected_by":"PreToolUse-input-source"}
{"ts":"...","event":"watcher_concern","watcher":"scope-watcher","note":"添加了云同步,超出 spec"}
{"ts":"...","event":"watcher_block","watcher":"cost-watcher","note":"项目累计 $200 触发硬上限"}
{"ts":"...","event":"SessionEnd","reason":"context_reset"}
```

### 4.1.1 ESCALATE grammar(M0.5.4)

`Stop` hook 在 claude 最后一行匹配前缀,解析为下面三档之一,落 `event: "escalate"` 时附带 `kind` 与可选 `target_phase`。**纯字符串前缀匹配,orchestrator 不读自然语言**(详见 [docs/claude-code-tool-surface.md §2.2.3](./claude-code-tool-surface.md))。

| ESCALATE 末行 | `kind` | `target_phase` | orchestrator 行为(M0/M1) |
|---|---|---|---|
| `ESCALATE: REVERT_TO_PHASE <phase> — <reason>` | `revert` | `<phase>` | M1+:set current_phase=`<phase>`,phase_state=Idle,re-dispatch;M0 仍走通用 escalation(写 escalation.md,等用户) |
| `ESCALATE: NEED_USER_INPUT — <questions>` | `need_user_input` | `null` | 写 escalation.md,inbox 等用户 |
| `ESCALATE: ABORT — <reason>` | `abort` | `null` | 项目永久标 escalated,M0 等同 NEED_USER_INPUT |
| `ESCALATE: INSUFFICIENT_CLARIFICATION — <last_question>` | `insufficient_clarification` | `null` | M2.3+:phase 已撞 `max_clarify_rounds` 上限,best-effort artifact 已产出;orchestrator 写 escalation.md,outbox `event_kind: escalation`,等用户决定继续 / 接受 / abort(详见 §5.6.2) |
| `ESCALATE: PHASE_DONE_PENDING — <reason>` | `phase_done_pending` | `null` | M3.6+:phase 部分完成,某些子任务 defer 到 decisions queue;orchestrator 切 `PhaseState::DonePending`,下 phase 启动检查 pending(详见 development-plan §5 M3.6) |
| `ESCALATE: <free text>`(无前缀) | `need_user_input` | `null` | 等同显式 NEED_USER_INPUT,reason 是整段文本 |

分隔符:em dash `—`(U+2014)、`--`、` - `(单 dash 必须前后有空格——这是为了不切碎 `plan-eng` 这类 phase 名)。

**phase 模板作者写 ESCALATE 的原则**:能用前缀就用前缀(orchestrator 路由更精确);不确定就裸写文本(降级为 NEED_USER_INPUT)。**不要**把 ESCALATE 当成 RPC 通道来请求 `/exit`、`/reload-plugins` 等 TUI 命令——那是 orchestrator 的监控职责(详见 [docs/claude-code-tool-surface.md §2.2.2](./claude-code-tool-surface.md))。

### 4.2 写入责任

| 事件 | 写入方 |
|---|---|
| `session_start` / `phase_inject` | orchestrator(send-keys 前后直接 append) |
| `PreToolUse` / `PostToolUse` / `phase_milestone` | Claude Code hooks(详见 §6.1) |
| `phase_done` / `escalate` | Stop hook 解析 claude 最后一行(`PHASE_DONE: <phase>` / `ESCALATE: <reason>`) |
| `user_attach` | PreToolUse hook 检测输入源 |
| `watcher_concern` / `watcher_block` | Cross-cutting watcher 异步子进程(详见 tech-design §6.3 模式 B) |
| `SessionEnd` | SessionEnd hook |

### 4.3 消费方

- **orchestrator**:`inotify` 监听末尾,做状态转移与 stall 检测
- **用户 dashboard pane**:`tail -f progress/<slug>.jsonl | jq -c '.event + ":" + (.tool // .note // "")'`
- **retro phase**(M3):作为项目历史输入

### 4.4 Stream-json 归档(可选)

用 hook 把 `--output-format stream-json` 内容旁路归档到 `~/.ccteam/log/<slug>/`,仅供事后调试,**不参与状态判定**。

---

## 5. Phase 模板 schema

### 5.1 YAML front matter 完整字段

```yaml
---
name: implement                   # phase 名,必须与文件名 (03-implement.md) 一致
required_inputs:                  # 必读上游产物;orchestrator 验证存在性
  - .ccteam/plan-eng.md
  - .ccteam/architecture.md
required_outputs:                 # 必产出物;Stop 前 hook 验证;缺则不视为 phase_done
  - src/**/*
  - .ccteam/implement-report.md
soft_cost_warn_usd: 5.0           # 仅告警,不打断
stall_warn_minutes: 5             # 1× warn / 3× suspicious / 6× escalate 三档(分钟);
                                  # `5` → 5/15/30 分钟。research 04-primary 用 60 → 60/180/360
                                  # 缺省时退回 5/15/30(代码常量,见 stall.rs)
parallelism: solo                 # solo | agent_team | multi_session(详见 tech-design §3.3、§6.3、§6.11)
                                  # M0 仅支持 solo;M2 支持 agent_team;M3 支持 multi_session
                                  # subagent 不在此声明——任何 agent 都可 ad-hoc 通过 Task 工具启动
agent_team:                       # 仅当 parallelism: agent_team 时生效
  - role: backend-dev
  - role: frontend-dev
  - role: reviewer
sub_skills:                       # 替人编排的 sub-skill(详见 §7)
  - skill: "claude-plugins-official:pr-review-toolkit/agents/code-reviewer"
    trigger: phase_done           # phase_start | phase_done(M0/M2 仅这两档)
    output_to: .ccteam/code-review.md
tools_required:                   # M0.5+:phase 内会调用的工具,orchestrator 启动期校验(详见 §5.4)
  subagents:                      # `Task(subagent_type="...")` 调到的 subagent 名
    - code-reviewer               # 内置五个不必列(general-purpose / Explore / Plan / claude-code-guide / statusline-setup)
  skills:                         # `Skill(skill="...")` 调到的 skill 名
    - some-skill
  mcp:                            # `mcp__<server>__<tool>` 引用的 MCP server 名
    - playwright
auto_loop: false                  # 默认 false。true 时 orchestrator 派发后交棒给 Stop hook,
                                  # 由 hook 反复重喂 prompt 直到 `completion_signal` 出现或撞 `auto_loop_max_iterations`
                                  # (M3.1:dev 的 fix phase 设 true,research 的 04-primary 也会设 true;mechanism 与 phase 名无关)
auto_loop_max_iterations: 3       # 自循环硬上限,默认 3。auto_loop=false 时忽略
completion_signal: TESTS_GREEN    # 自循环退出信号(子串匹配)。auto_loop=true 时必填且非空,
                                  # auto_loop=false 时可省略
next_on_done: implement           # 可选。`phase_done` 后跳转目标。省略 → 走拓扑序下一相
                                  # (M3.1 F2:从 PhaseTemplate 列表的文件名顺序推 DAG;
                                  # 末尾相省略 next_on_done = 终点节点 = is_terminal_phase)
next_on_escalate: null            # 可选。`escalate` 后静态 revert 目标。省略(null)= 项目终态 escalated
                                  # (M0.5.4 ESCALATE: REVERT_TO_PHASE 语法在事件 target_phase 字段独立路由)
decision_mode: hybrid             # M2.3+:sync | async | hybrid。默认 hybrid。详见 §5.6
                                  # sync   = phase 内用 AskUserQuestion 阻塞式问用户(用户必然在场)
                                  # async  = phase 写 outbox event_kind=clarify,不阻塞,可配合 PHASE_DONE_PENDING (M3.6)
                                  # hybrid = 先试 AskUserQuestion 1-2 分钟超时降级 outbox(默认推荐)
max_clarify_rounds: 3             # M2.3+:phase 内多轮 CLARIFY 硬上限,默认 3。超限 phase 强制基于现有
                                  # 信息产出 best-effort artifact + ESCALATE INSUFFICIENT_CLARIFICATION
                                  # 让用户决定继续追问 / 接受 best-effort / abort。详见 §5.6
golden_rules:                     # M2.3+:phase 级硬约束 enforcement(after hook 之外的 plugin 化路径)
  - rule_id: tests_green          # 规则 ID,落 progress.jsonl 用
    cmd: cargo test --workspace   # 任选 cmd | pattern;cmd 退出码非 0 = 违反 = 阻断 phase_done
  - rule_id: no_secrets_in_repo
    pattern: 'AWS_SECRET|sk-[a-zA-Z0-9]{32,}'   # regex 匹配 staged diff 任一行 = 违反
                                  # orchestrator 不内置规则,只跑 enforcement;dev / product-research / 等
                                  # 团队各自在 phase YAML 里写需要的 rule_id;空 / 不写 = 不跑
hooks:                            # phase 级 hook(项目级 hook 在 settings.json,详见 §6)
  before: scripts/snapshot-git.sh
  after: scripts/run-golden-rules.sh
---

# 任务

(prompt body...)
```

### 5.2 dev team phase 列表(M3 后 team-aware)

> **2026-05-06 重构**:原列表 9 phase(含 `00-seed.md` 与 `08-score.md`)是把
> 价值判断(idea 是否值得做)与质量判断(构建做得好不好)塞进 dev pipeline
> 的产物。讨论后:
> - `00-seed.md` → 提取到 product-research team(M3.4 落地于 `phases-product-research/`)
> - `08-score.md` → 整体删除;硬质量交给 fix-loop(M0)+ phase YAML
>   `golden_rules`(M2.3),软质量交给 M5 Critic 独立 team
>
> dev team 当前 phase 集合(`phases/`):

```
02-plan-eng.md         # 技术规划(multi_session 项目在此输出子模块清单 + interface-contracts.md)
03-implement.md        # 代码实现
04-test-author.md      # 测试编写
05-test-run.md         # 跑测试,输出报告
06-fix.md              # 修 bug(在 fix-cycle 中循环;auto_loop=true;详见 tech-design §3.5)
09-ship.md             # 提交、产文档、收尾
```

可选(尚未实现,按需补):

```
01-plan-ceo.md         # 产品规划(可选;若需 PM 视角再加)
07-review.md           # 代码审查(可选;M2.1 sub-skill 触发 plugin code-reviewer 即可覆盖)
```

> 编号断档(00 / 01 / 07 / 08 缺失)有意保留,M3.1 F2 已落 `Dag::from_templates`
> 按文件名顺序推 DAG,断档不影响顺序;不重新编号是为了 git 历史与 phase 名稳定。

### 5.3 Verdict phase 输出格式(通用)

> **2026-05-06 reframe**:原标题"Phase 0 Seed verdict 输出格式"假设 Seed 在 dev
> pipeline 内。Seed 提取到 product-research team 后,verdict schema 本身**作为通用
> 协议保留**——任何团队的 phase 想做"PASS / CONCERN / REJECT / CLARIFY"判断都用
> 这个格式。当前已知使用方:product-research team 的 `verdict` phase(M3.4)。

verdict-emitting phase 末尾必须输出固定 markdown,orchestrator 解析 YAML front matter 决定走向:

```markdown
---
verdict: PASS | CONCERN | REJECT | CLARIFY
confidence: 0.0-1.0
---

## 市场分析
(已有竞品 / 用户量 / 替代方案)

## 技术可行性
(核心难点 / 依赖 / 估算工作量)

## 商业可行性(按需)
...

## 决策
- PASS 时:产物足够,直接交付(product-research:产 next-steps.md 建议派 dev)
- CONCERN 时:可推进但有保留;rationale 列具体担忧(M2.3+ 起支持)
- REJECT 时:列举具体理由(已有 X、成本不可持续、用户量级 < N)
- CLARIFY 时:**只**提一个问题(prompt 显式约束;`max_clarify_rounds` 见 §5.6)
```

`verdict` 与 ESCALATE 的关系:

- `verdict: PASS` → phase 正常 PHASE_DONE,走 `next_on_done`
- `verdict: CONCERN` → phase 正常 PHASE_DONE,但 outbox 写 `event_kind: progress` 提醒用户(下游 phase 不阻塞)
- `verdict: REJECT` → phase ESCALATE,前缀 `ABORT`(M0.5.4),orchestrator 转项目终态
- `verdict: CLARIFY` → phase 写 outbox `event_kind: clarify`,按 `decision_mode` 走(§5.6);多轮上限 `max_clarify_rounds`

---

### 5.4 `tools_required` 字段语义(M0.5+)

声明 phase 模板里会用到的工具,orchestrator 启动时枚举本机可达项 + 交叉比对,缺谁报谁 + 给修复命令(`ccteam start` 直接 fail-fast,除非加 `--skip-tool-check`)。

| 子字段 | 名字来源 | "可达"判定 |
|---|---|---|
| `subagents` | `Task(subagent_type="<name>")` | `~/.claude/agents/<name>.md` 存在(不要列内置五个,但列了无害) |
| `skills` | `Skill(skill="<name>")` | `~/.claude/skills/<name>/SKILL.md` 或 `~/.claude/plugins/marketplaces/*/plugins/*/skills/<name>/SKILL.md` 存在 |
| `mcp` | `mcp__<name>__<tool>` 工具前缀 | `~/.claude.json` 或 `~/.claude/mcp_servers.json` 的 `mcpServers` 含此 key |

实测背景:plugin 装了 plugin 不等于 plugin agent 进 Task 注册表 —— 必须 ln -sf 到 `~/.claude/agents/` 才行(详见 [docs/claude-code-tool-surface.md §1.1.2 / §1.2.5](./claude-code-tool-surface.md))。所以 `tools_required.subagents` 列 `code-reviewer` 而 `~/.claude/agents/code-reviewer.md` 不存在 → orchestrator 拒绝启动并给出 `ccteam doctor --install-recommended-agents` 修复命令。

`bootstrap_project` 已在 §1.2 项目创建路径里自动 ln -sf 八个推荐 agent + 占位 skills 目录,所以 happy path 上模板要的工具默认都有;只有用户手工编辑模板加了非推荐工具时才会触发本节的校验失败。

---

### 5.5 `team.yaml` 团队配置(M3.1+)

每个团队一份 `team.yaml`,M3.4 落到 `~/.ccteam/teams/<name>/team.yaml`。**M3.1 只交付数据形式 + 解析,无运行时调用**——`retro_schema` 字段供 M4.1 retro phase 实现读取,从 day 1 就能写出团队特定字段(避免 RAG 索引重建,详见 dev-coupling-audit.md F20)。

```yaml
# ~/.ccteam/teams/dev/team.yaml(M3.4 路径)
name: dev                              # 必填。snake-case [a-z0-9_-]+,与 --team / state.json.team 对齐
description: Software development team  # 可选。`ccteam ls --teams`(M3.4)显示
retro_schema:                           # 可选。retro phase 字段定义。空 = 该团队无 retro
  - field: tech_stack                   # 必填。snake_case;markdown 子节标题 + RAG tag(改名 = 索引失效)
    description: Languages, frameworks, key libraries used
    kind: list                          # 默认 list。可选 text(单段叙述)
  - field: pitfalls
    description: Mistakes / surprises to avoid next time
  - field: successful_designs
    description: Design choices that paid off
  - field: do_not_do_again
    description: Anti-patterns observed
```

**research 团队示例**(对比 dev 字段差异):

```yaml
name: research
retro_schema:
  - field: methodology
    description: Methods used for data collection / analysis
  - field: data_sources
    description: Sources consulted (URLs, papers, datasets)
  - field: findings
    description: Top-N conclusions
  - field: open_questions
    description: Things needing follow-up
  - field: summary
    description: Narrative recap of the research
    kind: text
```

**校验**(`TeamSpec::validate` 在 parse 时执行):
- `name` 非空,只允许 ascii 小写 / 数字 / `-` / `_`
- `retro_schema[*].field` 非空,**不允许重复**(防 RAG 索引冲突)

**M3.1 实现位置**:`crates/ccteam-core/src/team.rs`(`TeamSpec` / `RetroFieldSpec` / `RetroFieldKind`),通过 `ccteam_core::TeamSpec::load(path)` 暴露。当前 ccteam binary 不读这个文件——**M3.4 加载逻辑、M4.1 retro phase 读 schema**。

---

### 5.6 `decision_mode` 与 `max_clarify_rounds` 语义(M2.3+)

> phase 内"用户决策点"的 UX 协议。两种用户姿态(在线 vs 离线)需要不同 phase 行为,
> 用 `decision_mode` 字段一处选择。多轮 CLARIFY 用 `max_clarify_rounds` 防失控。

#### 5.6.1 三种 mode 行为

| mode | phase 内行为 | 何时阻塞 | 用户姿态假设 |
|---|---|---|---|
| `sync` | 用 `AskUserQuestion` 工具直接问;phase 阻塞等回答 | 一直阻塞到回答 | 用户必然 `tmux attach` 到 project session 或 meta session |
| `async` | 写 outbox `event_kind: clarify`,继续做能做的事;若全部依赖该决策 → 写 `PHASE_DONE_PENDING`(M3.6+)| 仅在所有剩余工作都依赖该决策时阻塞 | 用户可能离线几小时;批量决策 |
| `hybrid` | 先试 `AskUserQuestion`,1-2 分钟超时降级 `async` 路径 | 短时阻塞后转 async | **默认推荐**——同时支持两种姿态 |

实施约束:

- `decision_mode: sync` —— phase prompt 必须显式调 `AskUserQuestion`;orchestrator 检测到该 phase idle 不计 stall(因为等用户是合理的),`stall_warn_minutes` 退化为"最大耐心"
- `decision_mode: async` —— phase prompt 必须显式 Write 到 `~/projects/<slug>/.ccteam/outbox/clarify-<ts>-<n>.md`(schema 见 §3.4.3);**M3.6 之前 async 等同于"phase 直接 ESCALATE NEED_USER_INPUT 阻塞",M3.6 起支持真 PHASE_DONE_PENDING**
- `decision_mode: hybrid` —— phase prompt 含两段 conditional(伪码:"如果 AskUserQuestion 在 X 秒内有响应就用,否则降级 outbox");X 由 phase 内 timeout 控制,orchestrator 不参与

#### 5.6.2 `max_clarify_rounds` 行为

phase 内累计 CLARIFY 轮次(每个 outbox `event_kind: clarify` 文件 + 对应 inbox answer 算一轮)。达到上限:

1. phase 必须基于现有信息产出 best-effort artifact(写 `required_outputs` 列出的产物)
2. ESCALATE 前缀 `INSUFFICIENT_CLARIFICATION`(M0.5.4 grammar 扩展)
3. ESCALATE 事件 `args.rounds_used` 写实际轮次,`args.last_question` 写最后一问
4. orchestrator 写 `~/projects/<slug>/.ccteam/escalation.md`,meta-agent / channel layer 通过 outbox 通知用户
5. 用户决定:① 注入更多上下文继续追问;② 接受 best-effort artifact,phase 视为 PHASE_DONE;③ ABORT 项目

**默认 `max_clarify_rounds: 3`**。verdict phase / 反向面试 phase(`@kickoff-reverse-interview`,M2.4)可适当调高(5-7);常规 phase 应当少于 3。

#### 5.6.3 与 outbox `event_kind` 枚举的对齐

§3.4.3 outbox event_kind 枚举:`reply | progress | escalation | shipped | clarify`。`decision_mode` 字段决定 phase 内**写哪种** event_kind:

- `sync` mode → 不写任何 outbox(用 AskUserQuestion 直接对话)
- `async` / `hybrid` mode → 写 `clarify`(还想问)或 `escalation`(过 max_clarify_rounds 或 verdict=REJECT)

#### 5.6.4 与 `ccteam decisions` CLI 的关系(M1 收尾增量)

`ccteam decisions` 扫所有 `~/projects/*/.ccteam/outbox/*.md` 过滤 `event_kind: clarify | escalation`,聚合成跨项目决策队列。**用户在 meta session attach 时,meta-agent 主动汇报队列**(role prompt 启发,M1.0 已落)。

---

## 6. Hooks 配置 schema

### 6.1 项目 `.claude/settings.json` 完整模板

> **D1 备注**:所有 hook 都是 `ccteam` 单 binary 的子命令(`ccteam hook <name>`),不再是独立 bash/python 脚本——零运行时依赖,与 orchestrator 共享 serde schema。debug 时可手动跑 `ccteam hook <subcmd>` 喂 stdin JSON(详见 §10.6)。
>
> **M0 备注**:M0.4 渲染的模板是下面 JSON 的一个真子集——**不含** `PostToolUse(Bash:git push.*)` 拦截分支(M1+ 才补上 `block-push` 子命令实现)。M0 渲染源在 `crates/ccteam-core/src/templates/settings.json`。

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
          {"type": "command", "command": "ccteam hook load-context", "timeout": 5},
          {"type": "command", "command": "ccteam hook progress-append session_start", "async": true}
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          {"type": "command", "command": "ccteam hook parse-phase-end", "timeout": 10},
          {"type": "command", "command": "ccteam hook progress-append Stop", "async": true}
        ]
      }
    ],
    "Notification": [
      {
        "matcher": "idle_prompt|permission_prompt",
        "hooks": [
          {"type": "command", "command": "ccteam hook progress-append notification", "async": true}
        ]
      }
    ],
    "PreToolUse": [
      {
        "hooks": [
          {"type": "command", "command": "ccteam hook progress-append PreToolUse", "async": true}
        ]
      }
    ],
    "PostToolUse": [
      {
        "hooks": [
          {"type": "command", "command": "ccteam hook progress-append PostToolUse", "async": true},
          {"type": "command", "command": "ccteam hook cost-accumulate", "async": true}
        ]
      },
      {
        "matcher": "Bash:git push.*",
        "hooks": [
          {"type": "command", "command": "ccteam hook block-push"}
        ]
      }
    ],
    "SubagentStop": [
      {
        "hooks": [
          {"type": "command", "command": "ccteam hook progress-append SubagentStop", "async": true}
        ]
      }
    ],
    "SessionEnd": [
      {
        "hooks": [
          {"type": "command", "command": "ccteam hook progress-append SessionEnd", "async": true}
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
| `Stop` | 解析最后一行 `PHASE_DONE` / `ESCALATE`;append `Stop` 事件(idle 信号);若 fix-loop.state.md 存在则按 ralph-loop 范式拦截重喂 |
| `Notification:idle_prompt` | claude 显式等待用户输入 → idle 信号 |
| `Notification:permission_prompt` | 不应出现(`--dangerously-skip-permissions` 兜底);出现说明配置失效 |
| `PreToolUse` | append 工具调用事件;活跃信号(stall 检测反向判断) |
| `PostToolUse`(通用) | append 事件;`cost-accumulate.sh` 累加 token / cost 到 `state.json` |
| `PostToolUse(Bash matcher)` | 拦截危险命令(`git push` / `rm -rf /` / deploy 脚本) |
| `SubagentStop` | 子 agent 退出(仅 Agent Teams phase 内相关) |
| `SessionEnd` | claude 进程退出 → orchestrator 知道 reset 完成 vs crash |

### 6.3 `cost-accumulate` 子命令工作原理

**关键事实**:Claude Code **不**在 hook 输入直接给 `cost_usd`,必须从 `transcript_path` 自算:

1. hook stdin 的 JSON 含 `transcript_path`(Claude Code 的 session JSONL 路径)
2. Rust 实现 tail 该 JSONL 最后一条 `role:assistant` 记录(`tokio::fs` + 行级反向扫)
3. `serde_json` 解析 `message.usage.{input_tokens, cache_read_input_tokens, cache_creation_input_tokens, output_tokens}` 字段
4. 按 `~/.ccteam/config.yml` 的 `model_rates` 表算成本增量
5. 原子地累加到 `state.json.cost_used_usd` 与 `state.json.context_tokens_used`(`.tmp` + `rename`)
6. 后者驱动 tech-design §6.9 的 60% reset 阈值

字段名参考 `claude-plugins-official/session-report/skills/session-report/analyze-sessions.mjs`(JS 实现可对照,但 ccteam 落地在 `crates/ccteam-cli` 的 `hook cost-accumulate` 子命令中,纯 Rust)。

`async: true` 必须设——同步阻塞会拖慢 PostToolUse 路径。

---

## 7. Sub-skill 调度 schema

### 7.1 phase front matter 的 `sub_skills` 字段

```yaml
sub_skills:
  - skill: "claude-plugins-official:pr-review-toolkit/agents/code-reviewer"
    trigger: phase_done
    output_to: .ccteam/code-review.md
  - skill: "claude-plugins-official:security-guidance/hooks/security_reminder_hook.py"
    trigger: phase_start
    output_to: .ccteam/security-precheck.md
```

### 7.2 Trigger 时机(M0/M2 仅两档)

| Trigger | 何时跑 | 实现 |
|---|---|---|
| `phase_start` | phase prompt 注入前 | orchestrator 把 skill 内容前置到 prompt(或异步先跑产出文件供 phase 引用) |
| `phase_done` | claude 输出 `PHASE_DONE` 后 | orchestrator 在状态机转移前调用 skill,产物落到 `output_to` |

**M0/M2 不引入 `before_done` 之类需 Stop hook 拦截的 trigger**(详见 tech-design §6.10)。

### 7.3 复用粒度三档与 `skill:` 路径前缀

| 路径前缀 | 粒度 | 含义 |
|---|---|---|
| `claude-plugins-official:<plugin>/<path>` | 直接 `@文件引用` | 零安装,phase 模板里 inline 引用 |
| `local:<path>` | 拷贝到项目 | 冻结版本,改不影响原仓库 |
| `installed:<plugin>/<command>` | 整 plugin 安装 | M2/M3 才考虑;`/plugin install <name>` 后调用 |

orchestrator 解析时按前缀分发实现路径。

### 7.4 产物自动接力

orchestrator 在调度下一 phase 时:
1. 扫上一 phase 全部 `sub_skills.output_to` 路径
2. 把这些路径作为下一 phase prompt 的 `@文件引用` 自动追加
3. 下一 phase claude 自动读到上游 audit / review 产物

### 7.5 `skill_intent.yaml`(M3+ 新插件挂载)

社区新插件提供:

```yaml
# ~/.claude/plugins/<plugin>/skill_intent.yaml
suggested_phases:
  - phase: ship
    trigger: phase_done
    rationale: "OWASP/STRIDE 安全审计应在 ship 前必跑"
  - phase: review
    trigger: phase_done
    rationale: "深度代码 review 与浅 review 互补"
default_output_to: .ccteam/{plugin}-output.md
```

ccteam Seed phase 后扫一次 `~/.claude/plugins/.../skill_intent.yaml`,按 `suggested_phases` 自动加进对应 phase 模板的 `sub_skills` 列表。

---

## 8. Multi-session per project 协议(M3)

### 8.1 Tmux session 命名

```
ccteam-<slug>                        # master session(编排)
ccteam-<slug>--backend-api           # 子模块 session
ccteam-<slug>--frontend-dashboard
ccteam-<slug>--mobile-app
ccteam-<slug>--docs
```

双连字符 `--` 分隔 slug 与 sub-module,避免与 slug 内部连字符歧义。

### 8.2 Fan-out / Fan-in 状态转移

```
master phase: plan-eng
  └─→ 输出 interface-contracts.md + sub-modules 清单 → master phase: fan-out
fan-out:
  └─→ orchestrator 起 N 个 sub-session(每 session 跑独立 9-phase 流)
  └─→ master phase: implement-parallel
implement-parallel:
  └─→ master inotify 监听所有 sub-module progress.jsonl
  └─→ 所有 sub-module 都到达 review phase 时 → master phase: fan-in
fan-in:
  └─→ master session 起来跑 review(读所有子产物 + 验证 contracts 满足)
  └─→ master phase: ship
```

### 8.3 资源约束

```yaml
# ~/.ccteam/config.yml
max_sessions_per_project: 4         # 默认;可项目级覆盖
max_total_sessions: 12              # 全局上限(跨所有项目)
max_subagents_per_phase: 5
hard_cost_kill_per_project_usd: 200 # multi-session 总和 = master + sum(sub-modules)
```

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
ccteam start                           # 启动 orchestrator(前台)
ccteam start --daemon                  # 启动并 daemonize
ccteam stop                            # 优雅停机(保留 tmux session)
ccteam stop --kill-sessions            # 同时关闭所有 tmux session(慎用)
ccteam mcp-serve                       # M2+:作为 ccteam-mcp 跑 stdio MCP 协议(详见 §12)
```

### 10.2 提交需求

```bash
ccteam new "需求文本"
ccteam new -f spec.md                  # 从文件提
ccteam new --mode yolo "需求"          # 覆盖默认 trust_mode
```

### 10.3 查询状态

```bash
ccteam ls                              # 所有项目状态(human 表格)
ccteam ls --format json                # JSON 输出(给 LLM / 脚本用)
ccteam show <slug>                     # 单项目详情(含 session 状态、cost、最近 progress)
ccteam show <slug> --format json       # JSON 输出
ccteam progress <slug> --tail          # 实时 tail progress.jsonl
ccteam progress <slug> --phase implement  # 看特定 phase 事件
```

**`--format json` 是 M0 强制项**——所有查询命令必须支持,以让"用户自带 claude"路径(详见 [tech-design.md §3.8](./tech-design.md#38-用户接口层))通过 Bash 工具调时无需解析表格。

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

是 §2.1 项目级 state.json 的全量 + 派生字段:

```json
{
  "state": { /* §2.1 state.json 全量 */ },
  "phase_history": [
    {"phase": "00-seed", "verdict": "PASS", "duration_s": 90, "cost_usd": 0.12},
    {"phase": "01-plan-ceo", "completed_at": "...", "cost_usd": 0.31}
  ],
  "recent_events": [ /* progress.jsonl 末尾 50 条 */ ],
  "artifacts": {
    "spec": ".ccteam/spec.md",
    "plan_eng": ".ccteam/plan-eng.md",
    "implement_report": ".ccteam/implement-report.md"
  },
  "stall": {"level": "ok", "silent_seconds": 23},
  "recommendations": [
    "若 cost > $50,考虑 attach 检查"
  ]
}
```

### 10.4 进入项目

```bash
ccteam attach <slug>                   # tmux attach 到项目 master session
ccteam attach <slug> --sub backend-api # multi-session 项目 attach 到子模块 session(M3+)
ccteam peek <slug>                     # tmux capture-pane 一次性看当前屏,不 attach
```

### 10.5 控制

```bash
ccteam reject <slug>                   # 否决
ccteam pause <slug>                    # 暂停(不杀 session)
ccteam resume <slug>                   # 恢复自动调度(接管 attach 后的暂停)
ccteam answer <slug> "回答内容"          # 响应 clarify
ccteam fork-reply <slug> A             # L3 fork 决策(M1+;A/B/C)
ccteam fork-reply <slug> B "去掉云同步"  # tweak
ccteam kick <slug>                     # 软重启项目 session(claude --resume)
```

### 10.6 维护

```bash
ccteam memory ls                                  # 看跨项目记忆(M3+)
ccteam memory rebuild                             # 重建索引
ccteam config edit                                # 改全局配置
ccteam doctor                                     # 体检:列出可用 mode flags
ccteam doctor --install-recommended-agents        # M0.5.5 ln -sf 8 个 plugin agent
ccteam doctor --tool-surface                      # M0.5.6 phase tools_required 交叉表
ccteam doctor --install-skill                     # M1.8 写 ccteam-control skill
ccteam doctor --install-meta-agent <user-handle>  # M1.0 创建 meta-agent 项目(含 --install-skill)
ccteam hook <subcmd>                              # debug:手动跑 hook(读 stdin JSON,写 stdout);
                                                  # subcmd ∈ {progress-append, parse-phase-end,
                                                  # cost-accumulate, load-context, block-push}
```

#### `ccteam stop` 行为契约(M1.5)

`ccteam stop` 通过 `~/.ccteam/state/orchestrator.pid` 找到正在跑的
orchestrator,发 SIGTERM。**不杀任何 tmux session**——`ccteam start`
下次启动时通过 `discover_projects` + `ensure_session` 自动 reattach
所有活跃 session(meta + 项目)。pidfile 由 `ccteam start` 写入,
退出时清理;若 pidfile 指向的 PID 已死,`ccteam start` 自动重新认领。

---

## 11. `ccteam-control` skill(M1+)

让用户在自己的 Claude Code session 里调度 ccteam。架构论证见 [tech-design.md §3.8 / §6.7](./tech-design.md#38-用户接口层)。

### 11.1 安装位置

```
~/.claude/skills/ccteam-control/
└── SKILL.md
```

由 ccteam M1 release 通过 `ccteam doctor --install-skill` 写入,或手动 `cp` from binary unpack。装一次,所有 claude session 自动可见。

### 11.2 SKILL.md 字段约定

```yaml
---
name: ccteam-control
description: |
  Manage ccteam projects from any Claude Code session.
  Use when the user asks about ccteam status, wants to start a new ccteam project,
  needs to inspect / pause / resume an active ccteam project, or asks for advice on
  how to intervene when a project is stuck.
allowed-tools: [Bash]
---

(SKILL body)
```

`description` 字段必须明确"何时激活"——Claude Code 用 description 做 skill 选择决策。

### 11.3 SKILL body 必含章节

| 章节 | 内容 |
|---|---|
| **能力清单** | 所有可调 CLI 命令(从 §10 摘录,标注 `--format json` 是默认偏好) |
| **典型工作流** | 跨项目汇报 / 立项前多轮澄清 / 卡住诊断三类场景的 step-by-step |
| **决策原则** | 何时建议 `attach`(用户想介入)vs `peek`(只看不动)vs `pause`(暂停后再决定) |
| **不能做什么** | 不能替用户 attach(tty 交互);不能直接编辑 `~/projects/<slug>/.ccteam/` 元数据(走 control 文件协议) |

### 11.4 与 ccteam-mcp(M2+)的关系

M1 时 skill 让 claude 用 Bash 工具 + `--format json` 调 CLI。M2 ccteam-mcp 上线后:

- skill 仍保留——是 claude 发现"原来可以管 ccteam"的引导层
- skill body 改为推荐"优先用 mcp__ccteam__* tools,fallback 到 Bash"
- 老的 Bash 调用方式仍兼容(--format json 永不下线)

### 11.5 Meta-agent role prompt(M1.0)

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
- context 超 60% 时仍走 `reset_context` 桥接 CLAUDE.md(M1.4),M4.6 升级为
  完整 conversation continuity

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

### 12.2 暴露的 tool 清单(M2 起步集)

| Tool 名 | 对应 CLI | 入参 | 返回 |
|---|---|---|---|
| `ccteam__ls` | `ccteam ls --format json` | `{}` | §10.3 ls JSON schema |
| `ccteam__show` | `ccteam show <slug> --format json` | `{slug: string}` | §10.3 show JSON schema |
| `ccteam__new` | `ccteam new "..."` | `{prompt: string, priority?: "low"\|"normal"\|"high", mode?: "yolo"\|"balanced"\|"careful"}` | `{slug: string, workspace: string}` |
| `ccteam__peek` | `ccteam peek <slug>` | `{slug: string, lines?: number}` | `{capture: string, ts: string}` |
| `ccteam__progress` | `ccteam progress <slug>` | `{slug: string, phase?: string, last_n?: number}` | `{events: [...]}` |
| `ccteam__pause` | `ccteam pause <slug>` | `{slug: string}` | `{ok: bool}` |
| `ccteam__resume` | `ccteam resume <slug>` | `{slug: string}` | `{ok: bool}` |

### 12.3 不暴露的(M2 显式排除)

- `ccteam attach` — tty 交互,MCP 协议不适合
- `ccteam start / stop` — orchestrator 生命周期管理是 ops 决策,不让 LLM 误调
- `ccteam memory rebuild` — 重操作,走 CLI

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
| `~/.ccteam/inbox/` | 用户提需求 |
| `~/.ccteam/queue/<state>/` | 项目状态分桶 |
| `~/.ccteam/control/` | 用户 → orchestrator 控制信号(详见 §3.3) |
| `~/.ccteam/phases/` | phase 模板(详见 §5) |
| `~/.ccteam/templates/` | M2.4+:phase 可 @ 引用的 prompt 片段(`review-with-user-loop.md` / `kickoff-reverse-interview.md`) |
| `~/.ccteam/memory/` | 跨项目记忆(M3+) |
| `~/.ccteam/progress/<slug>.jsonl` | 结构化事件流(详见 §4) |
| `~/.ccteam/log/<slug>/` | stream-json 归档(可选,调试用) |
| `~/.ccteam/tmux/<slug>.layout` | 项目 tmux pane 布局模板 |
| `~/.ccteam/state/orchestrator.json` | orchestrator 自身快照 |
| `~/projects/<slug>/.ccteam/` | 项目元数据(详见 §1.2) |
| `~/projects/<slug>/CLAUDE.md` | 自动生成的项目运营手册 |
| `~/projects/<slug>/.claude/settings.json` | 项目级 Claude Code 配置(详见 §6.1) |
| `~/projects/<slug>/.ccteam/sub-modules/<name>/` | multi-session 子模块元数据(M3+;详见 §1.3) |

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
