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

> **V0.2 M0.17.1 layout shift**:每个 team 整目录,phase markdowns 在
> `teams/<name>/phases/`(原 `phases/` / `phases-product-research/` 仓根铺平)。
> 旧 yaml 的 `phase_dir: phases-<team>` 在 `TeamSpec::parse` 自动重写
> 为 `phases`(legacy compat)。

```
~/.ccteam/
├── config.yml             # 全局配置(并发上限、API key、bot token、信任档位、模型单价表)
├── inbox/                 # 待 triage 的需求
├── queue/                 # 阶段队列
├── teams/                 # **V0.2 M0.17:每个 team 单一目录**
│   ├── dev/
│   │   ├── team.yaml      # 详见 §5.5
│   │   └── phases/        # phase 模板(`team.yaml.phase_dir`,默认 `phases`)
│   │       ├── 02-plan-eng.md
│   │       ├── 03-implement.md
│   │       └── ...
│   ├── product-research/
│   │   ├── team.yaml
│   │   └── phases/
│   ├── meta-agent/
│   │   └── team.yaml      # evergreen,无 phases/(M0.16)
│   └── <user-team>/       # 用户自建 team(staging 经 ~/.config/ccteam/teams/)
├── templates/             # M2.4+: phase 可 @ 引用的 prompt 片段(原生 @,orchestrator 不解析)
│   ├── review-with-user-loop.md
│   └── kickoff-reverse-interview.md
├── control/               # 用户 → orchestrator 控制信号(详见 §3.3)
# 跨项目记忆走官方 ~/.claude/CLAUDE.md + ~/.claude/rules/ + per-repo auto-memory(M4),
# 不在 ~/.ccteam/ 下;详见 tech-design §3.7。
├── progress/
│   └── <slug>.jsonl       # 每项目一个事件流(hooks 写,inotify 监听;详见 §4)
├── harness/               # V0.3.1 F46:Claude Code statusline-command 双写镜像
│   ├── <slug>-<sid>.json  #   每 (slug, sid) 最新 harness snapshot;读侧 = ccteam-web `/sse/harness/...`
│   └── _meta-<handle>.json #  meta-agent 项目(单 session,sid 视作 "default")
├── log/
│   └── <slug>/            # stream-json 归档(可选,调试用)
├── tmux/
│   └── <slug>.layout      # 项目 tmux pane 布局模板
└── state/
    └── orchestrator.json  # orchestrator 自身 in-memory 状态的快照
```

**Team 解析三层优先级**(V0.2 M0.17.3,`team_resolver.rs`):

```
const TEAM_SOURCES: &[TeamSource] = &[
    TeamSource::Project,  // <project_dir>/.ccteam/team/team.yaml(per-project override)
    TeamSource::User,     // ~/.config/ccteam/teams/<name>/team.yaml(staging)
                          // V0.3:+ ~/.claude/plugins/marketplaces/*/plugins/<team>/team.yaml
    TeamSource::Repo,     // ~/.ccteam/teams/<name>/team.yaml(shipped seeds)
];
```

First-source-wins,整团维度替换(撞名 project 完全覆盖 user / repo,**不**字段级合并)。
读容错(yaml 错 → warn + 下一层),写严格(`save_team` 拒绝覆盖现存的不可解析
yaml)。

### 1.2 项目级目录(`~/projects/<team>-<slug>/`)

> **2026-05-06 F22**:项目目录现在带 `<team>-` 前缀(如 `~/projects/dev-todo-cli/`、
> `~/projects/product-research-ai-recipe/`),让 `~/.claude/rules/ccteam-lessons-<team>.md`
> 的 `paths: ~/projects/<team>-*` frontmatter 在 phase Claude session 启动时正确匹配。
> meta-agent 项目仍走 `<handle>-meta` 后缀约定(rules 不 scope 到 meta)。
> 历史项目目录(F22 之前创建)保持原名,通过 state.json `team` 字段识别身份。

```
~/projects/<team>-<slug>/
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
{"ts":"...","event":"phase_inject","phase":"implement","attachments":[".ccteam/code-review.md"]}
{"ts":"...","event":"PreToolUse","tool":"Edit","path":"src/db.ts"}
{"ts":"...","event":"PostToolUse","tool":"Bash","cmd":"pnpm test","exit_code":0,"duration_ms":4521}
{"ts":"...","event":"phase_milestone","phase":"implement","note":"完成 schema + migration"}
{"ts":"...","event":"subskill_started","phase":"implement","skill":"claude-plugins-official:pr-review-toolkit/agents/code-reviewer.md","trigger":"phase_done"}
{"ts":"...","event":"subskill_done","phase":"implement","skill":"...","output":".ccteam/code-review.md","bytes":4123}
{"ts":"...","event":"phase_done","phase":"implement","duration_s":4521,"cost_usd":2.13}
{"ts":"...","event":"golden_rules_check","phase":"implement","result":"pass","passed":["tests_green"],"skipped":[]}
{"ts":"...","event":"golden_rules_check","phase":"implement","result":"fail","passed":["tests_green"],"violations":[{"rule_id":"no_secrets_in_repo","kind":"pattern","detail":"matched `AWS_SECRET` at .ccteam/implement-report.md:3"}],"skipped":[]}
{"ts":"...","event":"phase_done_pending","phase":"feasibility","open_decisions":["reply-2026-05-06T100000Z-001.md"],"reason":"tech-stack decision deferred"}
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
| `ESCALATE: PHASE_DONE_PENDING — <reason>` | (special — emits standalone `phase_done_pending` event, not `escalate`) | `null` | M3.6 ✅:phase 产出 required_outputs 但部分子任务 defer。Stop hook 从 reason 解析 outbox 文件名(`reply-*.md` / `clarify-*.md` / `escalation-*.md`),写 `event: "phase_done_pending"` 含 `phase` / `open_decisions[]` / `reason` 三字段;orchestrator 走 `TickAction::AdvancePhasePending`,看下 phase `required_inputs` 与 `open_decisions` 静态交集决定 advance / 切 `PhaseState::DonePending` 阻塞(`ccteam resume` 清除) |
| `ESCALATE: <free text>`(无前缀) | `need_user_input` | `null` | 等同显式 NEED_USER_INPUT,reason 是整段文本 |

分隔符:em dash `—`(U+2014)、`--`、` - `(单 dash 必须前后有空格——这是为了不切碎 `plan-eng` 这类 phase 名)。

**phase 模板作者写 ESCALATE 的原则**:能用前缀就用前缀(orchestrator 路由更精确);不确定就裸写文本(降级为 NEED_USER_INPUT)。**不要**把 ESCALATE 当成 RPC 通道来请求 `/exit`、`/reload-plugins` 等 TUI 命令——那是 orchestrator 的监控职责(详见 [docs/claude-code-tool-surface.md §2.2.2](./claude-code-tool-surface.md))。

### 4.2 写入责任

| 事件 | 写入方 |
|---|---|
| `session_start` / `phase_inject` | orchestrator(send-keys 前后直接 append) |
| `PreToolUse` / `PostToolUse` / `phase_milestone` | Claude Code hooks(详见 §6.1) |
| `phase_done` / `escalate` | Stop hook 解析 claude 最后一行(`PHASE_DONE: <phase>` / `ESCALATE: <reason>`) |
| `subskill_started` / `subskill_done` / `subskill_skipped` / `subskill_failed` | M2.1 orchestrator 在 phase_start / phase_done 边界跑 sub-skill;`subskill_done` 含 `output` 与 `bytes`,`subskill_skipped` 含 `reason`(`installed:` 前缀等),`subskill_failed` 含 `error` |
| `user_attach` | PreToolUse hook 检测输入源 |
| `watcher_concern` / `watcher_block` | Cross-cutting watcher 异步子进程(详见 tech-design §6.3 模式 B) |
| `SessionEnd` | SessionEnd hook |

### 4.3 消费方

- **orchestrator**:`inotify` 监听末尾,做状态转移与 stall 检测
- **用户 dashboard pane**:`tail -f progress/<slug>.jsonl | jq -c '.event + ":" + (.tool // .note // "")'`
- **retro phase**(M4;改造现有 ship.md / verdict.md inline retro 段):作为项目历史输入,通过 Claude session `/memory` + `Edit ~/.claude/rules/ccteam-lessons-<team>.md` 写入

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
    pattern: 'AWS_SECRET|sk-[a-zA-Z0-9]{32,}'   # regex 匹配 required_outputs 文件内容 = 违反
                                  # orchestrator 不内置规则,只跑 enforcement;dev / product-research / 等
                                  # 团队各自在 phase YAML 里写需要的 rule_id;空 / 不写 = 不跑
                                  # 执行时机:phase claude 写 phase 完成信号 → orchestrator 跑 phase_done
                                  # sub-skills → 紧接着跑 golden_rules.enforce → 任一 violation = 阻断
                                  # phase_history 标 status: blocked,phase_state 留 Idle,写
                                  # escalation.md;事件 event: golden_rules_check 写 progress.jsonl
                                  # (result: pass | fail)。Pattern 当前对 glob required_outputs(*/?)
                                  # 跳过 + 报 skipped,只对字面路径生效
hooks:                            # phase 级 hook(项目级 hook 在 settings.json,详见 §6)
  before: scripts/snapshot-git.sh
  after: scripts/run-golden-rules.sh

# V0.2 M0.18 inject-prompt frontmatter(docs/v0-2/phase-prompt-architecture.md §4.2)
escalate_grammar_ref: standard    # 异常出口 grammar dialect。`standard` 走四个内置 prefix(REVERT_TO_PHASE / NEED_USER_INPUT / ABORT / INSUFFICIENT_CLARIFICATION);
                                  # 团队特定 prefix 由 team.yaml.escalate_grammar_extensions 注册,与本字段独立
outbox_question_protocol: v1      # 询问用户协议版本。V0.2 仅 `v1`(interfaces §3.4.3 outbox 文件 schema)
inject_directives:                # 可选。orchestrator 拼装 inject prompt 时启用的 segment 列表
                                  # 默认全开:[read_inputs, write_outputs, completion_signal, escalate_grammar, outbox_protocol, auto_loop, decision_mode]
                                  # 设空 list `[]` 关闭所有 segment(escape hatch)
  - read_inputs
  - write_outputs
  - completion_signal
  - escalate_grammar
  - outbox_protocol
  - auto_loop
  - decision_mode
---

# 任务

(prompt body — V0.2 M0.18: 100% 领域内容,不许写 `PHASE_DONE: <name>` /
`ESCALATE:` 等协议关键词;协议由 orchestrator 的 inject prompt 注入,详见
`docs/v0-2/docs/v0-2/phase-prompt-architecture.md` §3 三层架构 / §8 红线)
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
| `subagents` | `Task(subagent_type="<name>")` | (V0.2 M0.20)plugin pipeline 解析:`crates/ccteam-core/src/plugin_resolution.rs::KNOWN_PLUGIN_AGENTS` 命中 + 对应 plugin source 文件存在;或 `~/.claude/agents/<name>.md` 用户自写文件存在;或内置五个之一 |
| `skills` | `Skill(skill="<name>")` | `~/.claude/skills/<name>/SKILL.md` 或 `~/.claude/plugins/marketplaces/*/plugins/*/skills/<name>/SKILL.md` 存在 |
| `mcp` | `mcp__<name>__<tool>` 工具前缀 | `~/.claude.json` 或 `~/.claude/mcp_servers.json` 的 `mcpServers` 含此 key |

实测背景:plugin 装了 plugin 不等于 plugin agent 进 Task 注册表 —— spawned session 必须启用 plugin pipeline(V0.2 M0.20)。`bootstrap_project` 写 `<project>/.claude/settings.json` 时,根据 `tools_required.subagents` 解析 plugin 依赖,自动写入 `enabledPlugins: {"<plugin>@<mkt>": true}`;Claude Code session 启动时 plugin pipeline 加载 + 自动 namespace `<plugin>:<name>`,phase markdown 用裸名 `Task(subagent_type="code-reviewer")` 仍可调。`tools_required.subagents` 列 `code-reviewer` 而 plugin source 又不在 `~/.claude/plugins/marketplaces/` → orchestrator 拒绝启动并提示装 plugin(`claude /plugin add pr-review-toolkit@claude-plugins-official`)。

`bootstrap_project` 已在 §1.2 项目创建路径里自动写 `enabledPlugins` + 占位 skills 目录,所以 happy path 上模板要的工具默认都有;只有用户手工编辑模板加了非推荐工具时才会触发本节的校验失败。

V0.1 → V0.2 升级:V0.1 用户的 `~/.claude/agents/<name>.md` ln -sf 由 `ccteam doctor --migrate-recommended-agents` 一次性清理。

---

### 5.5 `team.yaml` 团队配置(M3.1 / M3.2 / M3.3 / V0.2 M0.16+)

每个团队一份 `team.yaml`,落 `~/.ccteam/teams/<name>/team.yaml`。

- **M3.1** ✅ 落 `name` / `description` / `retro_schema`(M4.1 retro phase 读)
- **M3.2** ✅ 加 `phase_dir` / `verdict_schema` / `escalate_grammar_extensions` /
  `golden_rules`(team-wide 默认)/ `critic_dimensions`(M5 用,M3 留数据形式)
- **M3.3** ✅ orchestrator 启动期扫 `~/.ccteam/teams/<name>/team.yaml`,
  按 `phase_dir` 加载 phases,每团队建 `TeamRuntime { spec, templates, dag }`
- **M3.4** ✅ product-research 团队入仓(`teams/product-research.yaml` +
  `phases-product-research/`)
- **V0.2 M0.16** ✅ 加 `evergreen` / `cost_policy` / `claude_md_template` —
  替代 `if state.team == META_TEAM_NAME` / `match team` ccteam-core 分叉
  (PRD §6.4 candidate 5 + §6.2 candidate 2);新增 `teams/meta-agent.yaml`
  作 evergreen 范例;`memory_bridge` 团队列表改成扫盘(PRD §6.4 candidate 3)
- **V0.2.2 F40** ✅ 加 `aliases: Vec<String>`(默认空),配合 `team_resolver` /
  `Orchestrator::team_runtime` / `team_bundle` / `ensure_team_resolvable` 的
  alias 解析路径,实现"软 rename"(`product-research` → `research` 是 V0.2.2
  首例,`teams/research/team.yaml` 列 `aliases: [product-research]`)。老项目
  state.json::team 字面 / 项目目录名 / 老 rules 文件全不动;`ccteam new --team
  product-research` 仍工作并 stderr warn deprecated。详 PRD §9。
- **V0.3.1 F48** ✅ 加 `kind: TeamKind`(默认 `workflow`),取值
  `workflow | multi_workflow | flex`。`workflow` / `multi_workflow` 是
  phase-driven;`flex` 团队无 phase DAG,orchestrator 不跑 auto_loop / phase
  prompt injection / golden_rules,但 hooks / progress.jsonl / silence
  classifier / cost watcher 仍保留。`ccteam team init <name> --kind flex`
  生成无 `phases/` 的 staging tree。
- **V0.3.1 F47** ✅ 加 `sessions: Vec<DefaultSessionSpec>`(默认空),每条
  `{ sid: String, harness: HarnessKind }`,`HarnessKind = claude | codex`(serde
  `lowercase` rename)。**只对 `kind: flex` 团队有意义**;
  workflow / multi_workflow 团队 parse 不 fail 但忽略。F47 ship trait stub +
  schema;F49 PR(V0.3.1 PR #4)落 `state.json::sessions[]` runtime + adhoc
  `ccteam session add/ls/attach/rm` 命令。详 PRD §F47 + `docs/research/ccteam-codex-integration.md`。

```yaml
# ~/.ccteam/teams/research/team.yaml — 完整字段示例(V0.2.2 起 canonical 名 `research`)
name: research                          # 必填。snake-case [a-z0-9_-]+,与 --team / state.json.team 对齐
aliases: [product-research]             # 可选(V0.2.2 F40)。老项目 state.json::team 仍可解析;同 charset 规则,不能与 name 重叠
kind: workflow                          # V0.3.1 F48。workflow | multi_workflow | flex;默认 workflow
description: |                          # 可选。`ccteam ls --teams`(M3.4)显示
  Product research team —
  kickoff → research → verdict → next-steps;
  用于"判断 idea 值不值得做"场景。
phase_dir: phases                        # 默认 `phases`。phase 模板 markdown 所在目录(相对 team_dir)

# M3.4 verdict-emitting phase 名 list。对应 §5.3 通用 verdict schema。
verdict_schema:
  - verdict

# M3.2: team-specific ESCALATE 前缀。Stop hook 看到 `ESCALATE: <prefix>` 时
# 走对应 `route` 分支。前缀本身是数据,不在 ccteam-core 写死(strategic §3.6)。
escalate_grammar_extensions:
  - prefix: MARKET_DUPLICATE
    route: abort                          # revert_to_phase | need_user_input | abort
    reason: "target market saturated; idea duplicates an existing free / widely-used tool"
  - prefix: INSUFFICIENT_VALIDATION
    route: need_user_input
    reason: "could not collect enough validation data within the round budget"
  - prefix: LOW_DIFFERENTIATION
    route: revert_to_phase
    target_phase: kickoff
    reason: "no sustainable differentiation; revert to kickoff to rethink"

# M3.2 / V0.2 M0.18: team-wide default golden_rules。Phase YAML
# `golden_rules` 优先 — phase 不写时回退到 team.yaml 默认。
#
# V0.2 M0.18 schema(docs/v0-2/phase-prompt-architecture.md §6 拆分):
# - `protocol`:协议级红线(orchestrator 处理)
#   - `enforce: cmd_check`(默认):cmd / pattern 在 phase_done 边界跑
#   - `enforce: prompt_directive`:directive 文本注入 inject prompt
# - `domain`:业务级偏好(prompt-only,不跑 enforcement)
#
# **legacy compat**:M3.x 平铺 list 的 `Vec<{rule_id, cmd|pattern}>` 自动
# 反序列化为 `protocol` + `enforce: cmd_check`(serde alias),无需迁移。
golden_rules:
  protocol:
    - rule_id: tests_green
      enforce: cmd_check
      cmd: cargo test --workspace
    - rule_id: outbox_only
      enforce: prompt_directive
      directive: "询问用户唯一合法出口是 outbox,禁用 AskUserQuestion / 纯文本"
  domain:
    - rule_id: prefer_small_pr
      directive: "PR 控制在 500 行以内,大改动拆 stack"

# M3.2 / M5: critic 维度配置(strategic doc §2.3 invariant 1 — 数据,非常量)。
# M3 留 schema 形式,M5 才真正消费。dev / product-research 当前都留空。
critic_dimensions: []

# M4.1 retro phase 字段定义。空 = 该团队无 retro。
retro_schema:
  - field: market_signals
    description: Top market signals collected
    kind: list                            # 默认 list。可选 text(单段叙述)

# V0.2 M0.16 — evergreen 标记 + cost 政策(PRD §6.4 candidate 5)。
# 默认 evergreen=false / cost_policy=KillAt(None)(沿用 M3 行为)。
# meta-agent / V0.3 watchdog / reviewer agent 应设 evergreen=true +
# cost_policy={kind: none}。
evergreen: false
cost_policy:
  kind: kill_at        # none | kill_at
  threshold_usd: ~     # KillAt(None) 回退到 state.hard_kill_threshold_usd

# V0.2 M0.16 — auto-managed `<project>/CLAUDE.md` body。{slug} / {team}
# bootstrap 时替换。空字符串走通用 fallback(不烧 dev / research 假设)。
# Replaces ccteam-core::projects::render_project_claude_md 的 match team。
claude_md_template: |
  # CLAUDE.md (auto-managed by ccteam)
  ...

# V0.3.1 F47/F48 — flex 团队的默认 session 列表(workflow / multi_workflow 团队
# 该字段忽略,不 fail);F49 PR 落 runtime path
# (`ccteam session add/ls/attach/rm` 实际写 state.json::sessions[])。
# 每条 `harness:` ∈ {claude, codex},缺省 `claude`;`sid:` 必填。
# 详 PRD §F47 + docs/research/ccteam-codex-integration.md。
sessions:
  - sid: claude-1
    harness: claude
  - sid: codex-1
    harness: codex
```

`DefaultSessionSpec` 字段表(V0.3.1 F47):

| 字段       | 类型           | 默认       | 说明 |
|------------|----------------|------------|------|
| `sid`      | `String`       | 必填       | session id slug。F49 派生 tmux session 名 `ccteam-<slug>-<sid>` 与 `<harness-dir>/<slug>-<sid>.json` dual-write 目标 |
| `harness`  | `HarnessKind`  | `claude`   | `claude` → `ClaudeCodeAdapter`(V0.3.1 完整);`codex` → `CodexAdapter`(V0.3.1 stub,V0.3.2 实现)|

`#[serde(deny_unknown_fields)]` 在 `DefaultSessionSpec` 启用,typo `sd:` 等 fail-loud。
`HarnessKind` `#[serde(rename_all = "lowercase")]`,未知 variant fail-loud(prevents `harness: anthropic` 等静默 fallback)。

**校验**(`TeamSpec::validate` 在 parse 时执行):
- `name` 非空,只允许 ascii 小写 / 数字 / `-` / `_`
- `aliases[*]` 各 alias 同 `name` 的 charset 规则;不能为空、不能重复、不能与 `name` 自身重叠(V0.2.2 F40)
- `kind` 缺省为 `workflow`;`workflow | multi_workflow` 保持 phase-driven
  行为;`flex` 团队跳过 phase DAG / auto_loop / phase prompt / golden_rules
  machinery,但保留 observability。`flex` 不允许 `golden_rules` /
  `escalate_grammar_extensions` / custom `phase_dir` / phase-boundary
  schema(`retro_schema` / `verdict_schema`)。
- `phase_dir` 对 phase-driven 团队必须非空、相对路径、不含 `..`;flex 可
  保留默认 `phases` 字段但 orchestrator / doctor 不加载或校验该目录
- `retro_schema[*].field` 非空,**不允许重复**(防 schema 字段重名 — M4.1 retro 写入跨项目 lessons 文件时按 field 名映射段落)
- `escalate_grammar_extensions[*].prefix` 非空、唯一;
  `route: revert_to_phase` 必须带 `target_phase`
- `golden_rules.protocol[*]` rule_id 唯一非空;
  `enforce: cmd_check` 必须 `cmd | pattern` 二选一(同 phase YAML);
  `enforce: prompt_directive` 必须 `directive` 非空
- `golden_rules.domain[*]` rule_id 唯一非空;`directive` 必须非空
- `verdict_schema[*]` 非空
- `critic_dimensions[*].name` 非空、唯一
- V0.2 M0.16:`evergreen` / `cost_policy` / `claude_md_template` 都 serde-default,
  现存 yaml 不需 migration;`evergreen=true` 团队走
  `Orchestrator::process_meta_project`(事件循环 + 上下文重置),
  `phase_dir` 不需存在;`cost_policy=None` 跳过 cost 阶梯;
  `cost_policy=KillAt(threshold)` 用 yaml 阈值覆盖
  `state.hard_kill_threshold_usd`
- V0.3.1 F47:`sessions: Vec<DefaultSessionSpec>` serde-default 空,V0.1/V0.2/V0.3
  yaml 解析不变;`harness: claude | codex` 严格 enum(未知 variant fail-loud);
  `DefaultSessionSpec` `deny_unknown_fields`(typo `sd:` fail-loud)。语义校验
  (sid 唯一 / `claude-N` vs `codex-N` 命名约定 / kind: flex 强约束)F49 PR 落,
  F47 只校验 schema 解析

**实现位置**:`crates/ccteam-core/src/team.rs`(`TeamSpec` / `RetroFieldSpec` /
`RetroFieldKind` / `CriticDimensionSpec` / `CriticStrictness` /
`EscalateGrammarExtension` / `EscalateRoute` / V0.3.1 F47:`DefaultSessionSpec` /
`HarnessKind`),通过 `ccteam_core::TeamSpec::load(path)` 暴露。orchestrator 启动期扫描 +
加载在 `Orchestrator::new`(`load_team_runtimes`)。

#### 5.5.1 Plugin manifest 兼容字段(V0.2 M0.22 team factory)

工厂产物的 staging 树 (`~/.config/ccteam/teams/<name>/`) 是合法的
**Claude Code plugin**,顶层布局:

```text
<staging>/
  .claude-plugin/
    plugin.json                       # Claude Code plugin manifest(严格 schema)
  team.yaml                           # ccteam team 配置(plugin loader unknown,zod strip)
  phases/
    01-<phase>.md                     # 同 §5.1 frontmatter
    ...
  README.md
  agents/      (可选,plugin 自带 subagent)
  commands/    (可选)
  skills/      (可选)
  hooks/hooks.json   (可选)
  .mcp.json    (可选,plugin 自带 MCP server)
```

`plugin.json` 字段集(借鉴 `~/.claude/plugins/marketplaces/claude-plugins-official/`
所有实例的实际 schema):

```json
{
  "name": "my-team",
  "description": "Custom marketing-research team",
  "author": {
    "name": "Alice",
    "email": "alice@example.com"
  },
  "version": "0.1.0"
}
```

- **`name`** 必填,ascii lower / digit / `-` / `_`(与 `team.yaml.name`
  一致;工厂在 `init_team_staging` 强制 lock-step)。doubles as plugin
  目录名。
- **`description`** 必填非空,一行。
- **`author.name`** 必填非空。**`author.email`** 可选。
- **`version`** 可选(`claude-plugins-official/explanatory-output-style`
  ship,其他不 ship)。

**ccteam 不写 plugin loader 的额外字段**(`hooks` / `mcpServers` /
`enabledPlugins` / `userConfig` / `dependencies`)。这些是 Claude
Code plugin 标准,工厂产物在需要时手工补齐(V0.2 不自动 emit;
V0.3 candidate)。

**`team.yaml` 在 plugin 根目录的去向**:Claude Code plugin loader
读 `.claude-plugin/plugin.json` 时按 zod schema 校,unknown 顶级文件
被忽略(默认 strip,见 `docs/v0-2/alignment-review.md`
§2.7)。`team.yaml` 不会污染 plugin 加载,ccteam 通过
`team_resolver::resolve_team` 直接读。

**校验**(`ccteam doctor --validate-team <name>`,M0.22.4 在 M0.18.5
基础上扩展):
1. `.claude-plugin/plugin.json` 解析 + `PluginManifest::validate`
   (name / description / author.name 非空,name 字符集合法)
2. `team.yaml` 解析 + `TeamSpec::validate`(同 §5.5)
3. **`plugin.json.name` 必须等于 `team.yaml.name`** — 工厂强制
   lock-step;手工编辑漂移 → `[FAIL]`

**实现位置**:`crates/ccteam-core/src/team_factory.rs`(`PluginManifest` /
`PluginAuthor` / `init_team_staging` / `validate_staged_team` /
`publish_team`)。CLI 入口在 `crates/ccteam-cli/src/team_factory_cli.rs`
`ccteam team {init,publish}`。

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
- `decision_mode: async` —— phase prompt 必须显式 Write 到 `~/projects/<slug>/.ccteam/outbox/clarify-<ts>-<n>.md`(schema 见 §3.4.3);M3.6+ 支持真 PHASE_DONE_PENDING(phase 不被全 block,仅依赖该决策的下游 phase 等)
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
      },
      {
        "matcher": "AskUserQuestion",
        "hooks": [
          {"type": "command", "command": "ccteam hook intercept-ask", "timeout": 5}
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
| `Stop` | 解析最后一行 `PHASE_DONE` / `ESCALATE`;append `Stop` 事件(idle 信号);若 fix-loop.state.md 存在按 ralph-loop 范式拦截重喂;**V0.2 M0.19**:三档兜底——前两档都没命中且 outbox 没新文件 → exit 2 + stderr 强制续聊,`stop_hook_active=true` 时写 `needs_attention.outbox.json` 不再 block |
| `Notification:idle_prompt` | claude 显式等待用户输入 → idle 信号 |
| `Notification:permission_prompt` | 不应出现(`--dangerously-skip-permissions` 兜底);出现说明配置失效 |
| `PreToolUse`(通用) | append 工具调用事件;活跃信号(stall 检测反向判断) |
| `PreToolUse(matcher: AskUserQuestion)` | **V0.2 M0.19.3**:`ccteam hook intercept-ask` 返回 `permissionDecision: deny`,assistant 改写 outbox。机制详见 `docs/v0-2/alignment-review.md` §3.2 |
| `PostToolUse`(通用) | append 事件;`cost-accumulate.sh` 累加 token / cost 到 `state.json` |
| `PostToolUse(Bash matcher)` | 拦截危险命令(`git push` / `rm -rf /` / deploy 脚本) |
| `SubagentStop` | 子 agent 退出(仅 Agent Teams phase 内相关) |
| `SessionEnd` | claude 进程退出 → orchestrator 知道 reset 完成 vs crash |

#### 6.2.1 `parse-phase-end` 状态机(V0.2 M0.19 三档兜底)

```
Stop fires
  │
  ▼
auto-loop.state.md 存在?
  │ yes ──→ 读 state,decide()
  │           │
  │           ├─ Reinject → ParseDecision::Block { reason }(stdout JSON)
  │           └─ AllowExit + 撞顶 → emit escalate;Continue
  │ no
  ▼
last_assistant_message 末行 PHASE_DONE / ESCALATE?
  │ yes ──→ append 对应 progress 事件;Continue
  │ no
  ▼
<project>/.ccteam/outbox/ 有 phase_inject ts 之后的 clarify-* / escalation-* / reply-*?
  │ yes ──→ Continue(orchestrator 决策队列接力)
  │ no
  ▼
stop_hook_active == true?
  │ yes ──→ 写 needs_attention.outbox.json;Continue(L3 fail-safe 防递归)
  │ no  ──→ ParseDecision::BlockMissingOutput { stderr }
            (CLI dispatcher 写 stderr,exit 2;Claude Code 把 stderr 当 blockingError 注入下一轮)
```

`needs_attention.outbox.json` schema(两个 writer,共享 schema):

```json
{
  "schema_version": 1,
  "ts": "<RFC3339>",
  "slug": "<slug>",
  "reason": "<short human description>",

  // M0.19 Stop hook L3 fail-safe writer fields (recursion guard).
  // Optional — F35 writer omits these.
  "last_assistant_message": "<原始末段 assistant 文本>",
  "pane_tail": "<tmux capture-pane 末 30 行(legacy 字段名;F35 复用 ccteam_pane_tail)>",

  // V0.2.2 F35 silence-classifier enriched fields. Optional —
  // M0.19 L3 writer omits these. Meta-agent role prompt §7.0
  // surfaces them as propose-confirm options.
  "event_kind": "escalation",
  "priority": "high",
  "ccteam_classification": "mid_tool_hung",        // 见 SilenceClass 枚举
  "ccteam_silent_seconds": 900,
  "ccteam_last_event": {                            // F35 progress.jsonl 末事件摘要
    "ts": "<RFC3339>",
    "event": "PreToolUse",
    "tool": "Read"                                  // 仅 PreToolUse / PostToolUse 含
  },
  "ccteam_pane_tail": "<tmux capture-pane 末 30 行;仅 surface,不进 orchestrator 状态机>",
  "body": "<NL 翻译给 meta-agent / 用户的描述>"
}
```

**`ccteam_classification` 枚举值**(F35 silence_classifier `SilenceClass`,
PRD §4.2.1 表;F36 timeout 沿用同 schema):

- `subagent_runaway` — `PreToolUse(tool=Task)` 后 ≥ phase escalate 阈值,无 SubagentStop
- `mid_tool_hung` — `PreToolUse(tool != Task)` 后 ≥ phase warn 阈值,无 PostToolUse
- `limbo_capped` — F35 deterministic re-inject 已重试 cap 次(默认 1)仍未恢复
- `post_stop_limbo` / `inject_limbo` — 罕见(orchestrator 通常已 re-inject 1 次,
  这两类只在 cap 之前一过性出现)
- `inject_defer_timeout` — F36 send-keys subagent guard 已 defer 超 `max_defer_minutes`
  (默认 10)仍未真发(子 agent 一直未停);见 §6.2.3 `pending-inject.json`

`Healthy` / `Terminal` / `SubagentBusy` 不写 outbox(F35 deterministic 判定为不需要
干预)。

**两个 writer 共存**:M0.19 L3 fail-safe 写 `pane_tail` /
`last_assistant_message`;F35 silence classifier 写 `ccteam_*` 字段族(包括
`ccteam_pane_tail`)+ `body`。watchdog (M0.21) 读所有字段并向 meta-agent surface;
后写覆盖前写(原子 `<file>.tmp` + rename)。

**字段分工**:`reason` = 单行短描述(grep 友好,日志可摘);`body` = NL 段落
(meta-agent §7.0 翻译模板的输入,可含选项 a/b/c)。两者都由 F35 writer 写,
M0.19 L3 writer 只写 `reason`。

#### 6.2.2 `limbo-retry-count.json` schema(V0.2.2 F35)

`<project>/.ccteam/limbo-retry-count.json`:F35 silence classifier 的 per-phase
deterministic re-inject 计数器。`MAX_LIMBO_RETRY = 1`(`PostStopLimbo` /
`InjectLimbo` 类只重试 1 次,超 cap 转 enriched escalate)。phase 推进时
orchestrator 重置(写入新 `phase` + `count: 0`)。

```json
{
  "phase": "implement",
  "count": 1,
  "last_at": "2026-05-09T10:00:00Z"
}
```

**生命周期**:phase 进入 → 计数器不存在 / 0;触发 limbo + re-inject 成功 →
`count = 1` + `last_at` 更新;再触发 limbo → cap 已满,写 enriched outbox 改记
`ccteam_classification: "limbo_capped"`,**不**再 re-inject。phase 推进 →
`reset_retry_count(path, &new_phase)` 重写为 `count: 0` + `phase = new_phase`。

**红线**:cap = 1 是 F35 在底层 `auto_loop` 3-cap 之上的额外兜底;两层叠加之后
撞顶必 enriched escalate(CLAUDE.md "fix-loop 撞 3 次顶必 escalate,绝不静默
重置")。

#### 6.2.3 `pending-inject.json` schema(V0.2.2 F36)

`<project>/.ccteam/pending-inject.json`:F36 send-keys subagent guard 的
deferred phase-inject 记录。`Orchestrator::dispatch_phase_with_state` 检测到
`progress::subagent_active(events) == true`(`PreToolUse(tool=Task)` 还有未配
对的 `SubagentStop`)时不发 send-keys / 不写 `phase_inject` 事件,改落盘本文件;
orchestrator daemon tick 后续在 SubagentStop 真到达 + 不再 active 时真发并删本
文件。**单文件覆盖,不积累队列** — 每个项目同时只有一条 deferred phase 待发,
新的覆盖旧的(`<file>.json.tmp` + rename 原子写)。

```json
{
  "schema_version": 1,
  "slug": "dev-x",
  "phase": "implement",
  "attachments": [".ccteam/code-review.md"],
  "enqueued_at": "2026-05-09T10:00:00Z",
  "max_defer_minutes": 10
}
```

**生命周期**:

1. dispatch 检测 active subagent → save record(`enqueued_at = now`)
2. 后续 tick:
   - 仍 active + 未 timeout → no-op,等下次 SubagentStop event
   - 不再 active + 未 timeout → 真发 `dispatch_phase_with_state`,delete record
   - timeout(`now - enqueued_at >= max_defer_minutes`) → 写 enriched outbox
     `ccteam_classification: "inject_defer_timeout"`,delete record(主路径不挂)
3. evergreen / meta-agent 项目 (`is_evergreen()`) 早 return,跳过 guard 与 drain

**与 F35 协同**(PRD §5.3):
- F36 主路径主动 defer;`InjectLimbo` 类(phase_inject 后 ≥ warn 阈值无 follow-up)
  是 F36 race 漏接的兜底重发(F35 deterministic re-inject)
- F35 `attempt_limbo_reinject` 检测到 pending-inject 在飞 → 跳过本次 retry,
  不烧 `MAX_LIMBO_RETRY` 预算(F36 drain 路径独立兜底)

`ccteam hook intercept-ask` 返回的 PreToolUse 决策(`hooks.ts:608-625`):

```json
{
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "deny",
    "permissionDecisionReason": "本 phase 应自决,不能用 AskUserQuestion ... 改写 .ccteam/outbox/clarify-<ts>.md ..."
  }
}
```

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
ccteam new -f spec.md                            # 从文件提
ccteam new --mode yolo "需求"                    # 覆盖默认 trust_mode

# V0.2.2 F34 — slug 决策四层:
ccteam new --slug ccteam-ui --team dev "..."     # Tier 1:显式 slug(B2 前缀语义);
                                              #   `dev-ccteam-ui` 或 verbatim
ccteam new --no-auto-slug "..."                  # Tier 4:跳过 LLM 智能 fallback,
                                              #   走 deterministic `slugify_brief`
ccteam new --auto-slug-model claude-sonnet-4-5-20251001 "..."
                                              # Tier 3:override 默认 haiku
                                              #   (env CCTEAM_AUTO_SLUG=off 全局禁)
```

**slug 决定优先级**(PRD `docs/v0-2-2/prd.md` §3.2):

1. **Tier 1**:`--slug` 显式(零延迟,确定;B2 prefix — 已带 team prefix verbatim)
2. **Tier 2**:meta-agent 派单工作流的 `ccteam-project-creator` skill 推荐 + 用户确认(零额外 LLM call)
3. **Tier 3**:`ccteam new` 不带 `--slug` 时 shell-out `claude -p haiku`(2-5s,~$0.0001)+ Y/n,非 tty auto-accept
4. **Tier 4**:LLM 不可用 / `--no-auto-slug` / env `CCTEAM_AUTO_SLUG=off` → `slugify_brief()` deterministic 兜底

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
ccteam decisions                       # M1 收尾增量:跨项目决策队列(扫 outbox event_kind: clarify|escalation)
ccteam decisions --format json         # JSON 输出(meta-agent 主消费;详见 §5.6.4)
ccteam watchdog scan                   # V0.2 M0.21 translation-only 健康巡检(详见 §12.5)
ccteam watchdog scan --format json     # 结构化输出(meta-agent 主消费)
ccteam watchdog scan --push --user <handle>  # 把 alert 写进 meta-agent outbox
ccteam fork-reply <slug> A             # L3 fork 决策(M1+;A/B/C)
ccteam fork-reply <slug> B "去掉云同步"  # tweak
ccteam kick <slug>                     # 软重启项目 session(claude --resume)
```

### 10.6 维护

```bash
# 跨项目记忆走 Claude session 内官方机制(M4):/memory 命令查 auto-memory,
# 直接编辑 ~/.claude/rules/ccteam-lessons-<team>.md 看 / 改跨项目 lessons。
# ccteam 不提供 memory 子命令(无自建索引,无东西可 rebuild)。
ccteam config edit                                # 改全局配置
ccteam doctor                                     # 体检:列出可用 mode flags
ccteam doctor --tool-surface                      # phase tools_required 交叉表(plugin pipeline 感知,V0.2 M0.20)
ccteam doctor --install-skill                     # M1.8 写 ccteam-control skill
ccteam doctor --install-meta-agent <user-handle>  # M1.0 创建 meta-agent 项目(含 --install-skill)
ccteam doctor --install-mcp                       # M2.5 在 ~/.claude.json 注册 mcpServers.ccteam(详见 §12)
ccteam doctor --install-memory-bridge             # M4.2 写 ~/.claude/rules/ccteam-lessons-{dev,product-research}.md 占位 + paths frontmatter scope
ccteam doctor --migrate-recommended-agents        # V0.2 M0.20 一次性清理 V0.1 ln -sf 残留 (~/.claude/agents/ 下指向 marketplace 的 symlink)
ccteam hook <subcmd>                              # debug:手动跑 hook(读 stdin JSON,写 stdout);
                                                  # subcmd ∈ {progress-append, parse-phase-end,
                                                  # cost-accumulate, load-context, block-push}
ccteam web --bind 127.0.0.1:7331                  # V0.3 web UI(详见 §15;`/` dashboard /
                                                  # `/project/<slug>` / `/assets/{file}` /
                                                  # `/sse/{all,project/<slug>}` / `/screenshot/<slug>.png` /
                                                  # `POST /api/<slug>/{btw,inject_decision,pause,resume}`)
ccteam web --bind 0.0.0.0:7331 [--no-auth]        # 同上,LAN 模式;非 loopback 默认强 token 鉴权,
                                                  # `--no-auth` 5s 倒计时 + stderr warn 后照样 listen
ccteam web --token-file <path>                    # 自定义 token 文件路径(默认 ~/.ccteam/web-token)
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

### 12.2 暴露的 tool 清单(M2.5 起 9 tool;V0.2.2 F38 起 10 tool)

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

V0.2.2 F38 红线:`screenshot` 是**只读**(daemon-independent),与 `peek` 同档,失败永不阻塞主路径(catch_unwind 兜 vt100/imageproc panic;tmux/font/IO 失败一律 `Ok(None)` → `{ok:false, reason}`)。截图字节流仅用于渲染,**不进入** `progress.jsonl` / `state.json` / state machine(CLAUDE.md §三红线"永不解析 tmux 终端输出")。字体走 vendored JetBrains Mono Regular(OFL,见 `LICENSES.md`),`CCTEAM_SCREENSHOT_FONT_TTF` env 可运行时覆盖(eg 切到 CJK / emoji 覆盖字体)。`ccteam doctor --screenshot-smoke <slug>` 跑端到端验证。

`send_to_session` / `inject_decision` 是 M2.5 增量(meta-agent 主消费者):
让 meta-agent 把用户的回复 / 决策推送回项目 session,**adapter 进程内不做
任何 NL 解析 / LLM 调用**,Symphony 反模式禁止(tech-design §3.1)。

`ccteam__inject_decision` 内部是 `send_to_session` 的 thin wrapper —— 把
`escalate_kind` 翻成 markdown payload(显式标记 `**META-AGENT DECISION**`
+ ESCALATE shape),然后走同一条 inbox 路径。

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

## 12.5 Watchdog `watchdog.yaml` schema(V0.2 M0.21)

> **架构沿革**:V0.2 把"低层信号 → meta-agent NL 通知"独立成 watchdog
> 角色(tech-design §3.9)。watchdog 是 **translation only** 层:读 4 个数据源
> + 用户阈值 → 产出 NL alert 写 meta-agent 自己的 outbox。**不动 orchestrator
> 状态**(零写入 progress.jsonl / state.json / control / inbox)。

### 12.5.1 文件位置

`~/.ccteam/watchdog.yaml`(用户级,全局生效)。文件不存在 ⇒ 全字段走默认。
解析失败 ⇒ fail-loud(用户配置坏不静默回 default)。

### 12.5.2 字段

| 字段 | 类型 | 默认 | 含义 |
|---|---|---|---|
| `notify_on_cycle_count` | `u32` | `2` | `<project>/.ccteam/auto-loop.state.md::iteration` 达到此值即 alert(默认 = 通常 cap 3 - 1) |
| `notify_on_phase_cost_usd` | `Option<f64>` | `None` | `state.json::cost_used_usd` 超此 USD 数即 alert;`None` ⇒ 不报 cost |
| `notify_on_phase_duration_min` | `Option<u32>` | `None` | 当前 phase 距 `last_progress_event_at` 超此分钟即 alert;`None` ⇒ 不报 |
| `notify_mode` | `quiet \| normal \| verbose` | `normal` | 见 §12.5.3 |

### 12.5.3 `notify_mode` 语义

- `quiet` — 仅放行 `cost_overrun` + `daemon_down`(钱 / 守护死必报);静默
  `auto_loop_cycle` / `phase_duration_overrun` / `needs_attention`
- `normal` — 默认;每个 alert kind 每次扫描发一次
- `verbose` — 同 `normal` + 不去重 `needs_attention`(用于 debug,不推荐生产)

### 12.5.4 Alert 输出契约

`ccteam watchdog scan --format json` 输出 schema:

```json
{
  "alerts": [
    {
      "kind": "needs_attention | auto_loop_cycle | cost_overrun | phase_duration_overrun | daemon_down",
      "slug": "<team>-<slug> | null",
      "message": "human-readable NL",
      "emitted_at": "RFC3339",
      "details": { "/* alert-kind specific */": "..." }
    }
  ],
  "config": { /* echoed WatchdogConfig */ }
}
```

`--push --user <handle>` 时,每条 alert 还原成 `<project>/<handle>-meta/.ccteam/outbox/reply-<ts>-<NNN>.md`(§3.4.3 outbox schema):

| Alert kind | `event_kind` | `priority` |
|---|---|---|
| `daemon_down` / `cost_overrun` / `needs_attention` | `escalation` | `high` |
| `auto_loop_cycle` / `phase_duration_overrun` | `progress` | `normal` |

### 12.5.5 调用约束(translation only)

- watchdog 读以下文件;**不写**它们任意一个:
  - `~/.ccteam/state/orchestrator.heartbeat`(只 stat mtime)
  - `<project>/.ccteam/state.json`
  - `<project>/.ccteam/auto-loop.state.md`
  - `<project>/.ccteam/needs_attention.outbox.json`
- watchdog 唯一**写**目标:`~/projects/<handle>-meta/.ccteam/outbox/reply-*.md`
- `ccteam-core::orchestrator` 模块 grep `watchdog` 必为 **0 次**(核心红线)

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
| `~/projects/<team>-<slug>/CLAUDE.md` | 自动生成的项目运营手册 |
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
| `GET` | `/assets/{file}` | 200 / 404 | `application/javascript; charset=utf-8`(htmx / htmx-ext-sse / xterm)/ `text/css; charset=utf-8`(style / xterm)/ `text/plain`(404) | vendored 静态资源;`Cache-Control: public, max-age=31536000, immutable`;白名单 = `htmx.min.js` / `htmx-ext-sse.js` / `xterm.js` / `xterm.css` / `style.css`,其他 file → 404 |
| `GET` | `/sse/all` | 200 | `text/event-stream` | 全局 SSE 流:每条 progress.jsonl 写入推一帧 |
| `GET` | `/sse/project/{slug}` | 200 | `text/event-stream` | per-slug SSE 流:server-side 过滤,只发该 slug 事件 |
| `GET` | `/sse/harness/{slug}` | 200 | `text/event-stream` | V0.3.1 F46:per-slug harness 状态流,该 slug 下所有 sid 的 statusline snapshot |
| `GET` | `/sse/harness/{slug}/{sid}` | 200 | `text/event-stream` | V0.3.1 F46:per-(slug,sid) harness 状态流,只发该 session 的 snapshot |
| `GET` | `/api/{slug}/pane-snapshot.ansi` | 200 / 504 | `application/octet-stream`(200)/ `text/plain`(504) | 按需 tmux ANSI snapshot;xterm.js 浏览器端只读渲染;headers:`x-ccteam-pane-rows` / `x-ccteam-pane-cols`;tmux 无 session → 504 |
| `GET` | `/screenshot/{slug}.png` | 200 / 404 / 504 | `image/png`(200)/ `text/plain`(404 / 504) | 按需 PNG 截图;F38 unavailable / tmux 无 session → 504 + 文本 reason;非 `<slug>.png` 路径 → 404 |
| `POST` | `/api/{slug}/btw` | 303 / 400 / 4xx | `text/plain`(error) | `text=<urlencoded, 1..=4000>`;详 §15.7 |
| `POST` | `/api/{slug}/inject_decision` | 303 / 400 / 5xx | `text/plain`(error) | `path=<absolute>&body=<urlencoded, 1..=8000>`;详 §15.7 |
| `POST` | `/api/{slug}/pause` | 303 / 4xx | `text/plain`(error) | 空 body;详 §15.7 |
| `POST` | `/api/{slug}/resume` | 303 / 4xx | `text/plain`(error) | 空 body;详 §15.7 |

### 15.2 dashboard 行(`/` 表格)

每行字段映射 `ccteam_core::ProjectSummary` + 派生:

| 列 | 来源 |
|---|---|
| Slug | `state.slug`(linked 到 `/project/<slug>`)|
| Team | `state.team` |
| Phase | `state.current_phase`(空时 `—`)|
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

两个 SSE endpoint(`/sse/all` + `/sse/project/<slug>`)wire 格式完全一致:

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
  + server 注入的 `slug` 字段(client-side 路由依赖此字段;若原行恰巧
  也含 `slug`,server 端覆盖之,以 watcher 解析的 filename 为准)。
- `: keepalive` 注释行 15s 周期发出(axum `Sse::keep_alive` 默认),
  防 nginx / 反向代理默认 60s 空闲超时。
- `event: reconnect_hint` 出现 = 此 SSE 订阅者落后超过 broadcast 容量
  (1024 帧),server 主动断流;client 收到 → 关闭 EventSource,1s 后
  reload(htmx-ext-sse / vanilla EventSource 都 auto-reconnect)。

**watcher 拓扑**:`crates/ccteam-web/src/watcher.rs` 起一条专用 OS 线程,
单个 `notify::RecommendedWatcher` recursive 监 `~/.ccteam/progress/`;
每检测到 `<slug>.jsonl` 的 Modify / Create 事件 → 维护 per-file 字节
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
