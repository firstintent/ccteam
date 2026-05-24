# Presets Reference — 5 preset 内部映射

> **Audience**:已经用过 [quickstart](../quickstart.md) 和 [recipes](../recipes.md) 的 power user / contributor。本文档展示 5 个 user-facing preset 在内部映射到哪些 execution mode / orchestration pattern / yaml schema,以及每字段的 default。日常使用者不需要看本文档 — `ccteam-creator` skill 自动生成所有这些字段。
>
> **关系到的内部文档**:
> - `docs/tech-design.md` §2.1(`HarnessAdapter` trait,5 方法)
> - `docs/architecture/orchestration-patterns.md` §一(5 编排模式 × 3 执行模式 × 2 vendor = 30 cell)
> - `docs/interfaces.md`(`workflow.yaml` schema 权威)

---

## 一、5 preset 内部映射总表

| Preset | 执行模式 | 主要编排模式 | spawn 原语 | 默认 vendor | 默认 cost cap |
|---|---|---|---|---|---|
| **Solo Sidekick** | mode 1 in-proc | Routing(单 agent)| `Task(subagent_type=...)` | Claude | parent session 共享 |
| **Team Sprint** | mode 1 in-proc | Orchestrator-Worker(+ optional Codex critic) | `Task(subagent_type=..., team_name=..., name=...)` × N | Claude + auto-critic Codex | parent session 共享 |
| **Overnight Builder** | mode 2 bg artifact-driven | Chaining + Evaluator-Optimizer 内圈 | `claude --bg --agent <role>` per spawn | Claude + auto-reviewer Codex | `max_cost_usd_per_24h: 5.0` |
| **Pocket Assistant** | mode 3 chat (DM) | Routing(单 bot)| `claude -p --resume <sid> --input-format stream-json` | Claude(Codex critic 隐式 second-opinion 可开) | `max_cost_usd_per_24h: 1.0` |
| **IM Squad** | mode 3 chat (group + bot-to-bot @) | Orchestrator-Worker + Routing(@-mention)| 同上,N 个 long-running session | Claude(per-bot vendor 可异)| `max_cost_usd_per_24h: 2.0` |

trait 层:全部走 `HarnessAdapter::{start_thread, submit_turn, events, resume_thread, close_thread}` 5 方法;`vendor: AgentVendor { Claude, Codex }` enum 决定具体 adapter(`ClaudeBgAdapter` / `ClaudeStreamJsonAdapter` / `CodexExecAdapter` / `CodexAppServerAdapter`)。

---

## 二、Solo Sidekick 内部 schema

User entry:`/ccteam-team 1 "task"` 或 `/ccteam <NL>` 推断单人小事 → 实际是单 `Task` call,**不写 yaml**。

无 `.ccteam/workflow.yaml` 落盘(parent Claude session 内瞬时,session 关即丢)。配置在 skill prompt 内。

---

## 三、Team Sprint 内部 schema

User entry:`/ccteam-team N "task"` 或 `/ccteam-team N:executor "task"` 显式指定 worker 类型。

`.ccteam/workflow.yaml`(由 `ccteam-creator` 生成):

```yaml
version: 0.6
mode: in_proc                          # 内部枚举,user 不见
preset: team_sprint
agents:
  - role: lead
    spawn: implicit                    # 即 parent session
  - role: executor
    parallelism: 3                     # user 指定的 N
    vendor: claude                     # default
  - role: critic
    vendor: codex                      # auto-detect:codex binary in PATH + auth ok → 自动设置
    optional: true                     # 没装 codex 则跳过此 agent

triggers:
  - kind: manual                       # user `go` 触发
    target: lead

budgets:
  parent_session_inherit: true         # 不单独 cap,跟 parent context
```

`vendor: codex` 自动设置规则:`ccteam-creator` skill `Phase 3` 检测 (a) `which codex` 成功 (b) `codex auth status` ok (c) role name ∈ {critic, reviewer, code-reviewer, security-reviewer, architect}。三条全满足才设置。

---

## 四、Overnight Builder 内部 schema

User entry:`/ccteam-creator "夜跑 X"` 推断 timeline = long-running + presence = unattended → mode 2。

`.ccteam/workflow.yaml`:

```yaml
version: 0.6
mode: bg                               # 内部枚举,user 不见
preset: overnight_builder
agents:
  - role: test-runner
    vendor: claude
    spawn_command: claude --bg --agent test-runner
    triggers:
      - kind: artifact_received
        watch: .ccteam/inbox/test/
      - kind: cron
        schedule: "0 22 * * *"        # 22:00 dispatch
  - role: fixer
    vendor: claude
    triggers:
      - kind: artifact_received
        watch: .ccteam/outbox/test-fail/
  - role: releaser
    vendor: claude
    triggers:
      - kind: artifact_received
        watch: .ccteam/outbox/test-pass/
  - role: reviewer
    vendor: codex                      # auto-critic
    optional: true
    triggers:
      - kind: artifact_received
        watch: .ccteam/outbox/fix/

budgets:
  claude:
    max_cost_usd_per_24h: 5.0
  codex:
    max_cost_usd_per_24h: 2.0

fix_loop:
  max_attempts: 3                      # 撞 3 次必 escalate(R6 红线)
  on_max_exceeded: escalate            # 写 escalation event,通过 IM 推用户

notification:
  channel: ~/.ccteam/im/credentials.json   # 走 /ccteam-im-setup 落地的 token
  on_events: [escalation, workflow_done, budget_exceeded]
```

trigger 类型详 `docs/interfaces.md` §3:
- `artifact_received { watch: <dir> }` — inotify 监听目录新文件
- `cron { schedule: <spec> }` — 时刻触发
- `manual` — user 在 Claude session 里手动 `/ccteam-control trigger <agent>`
- `workflow_event { kind: <event> }` — 监听 progress.jsonl 业务事件

---

## 五、Pocket Assistant 内部 schema

User entry:`/ccteam-creator "TG 私聊助手"` 推断 presence = "IM 私聊" → mode 3 chat DM。

`.ccteam/workflow.yaml`:

```yaml
version: 0.6
mode: chat                             # 内部枚举,user 不见
preset: pocket_assistant
im:
  channel_id_path: ~/.ccteam-im/channels/<slug>.json    # 内部 ref,user 不见
  transport: openhuman                                   # default;可 official-telegram override
agents:
  - role: helpful-bot
    persona: tech_helper_zh            # personas/tech_helper/zh/ 目录预填
    vendor: claude
    spawn_command: claude -p --resume <sid?> --input-format stream-json
    bot_name: auto                     # 从 agent_naming.rs 50 nickname 池取
    compact_every_turns: 50            # default
    hop_limit: 3                       # bot-to-bot @ 上限(R6 红线扩展)
    second_opinion:
      enabled: auto                    # 检测 codex available → 启用
      vendor: codex
      trigger: on_uncertain            # bot 自己判断"我不太确定" → 跑 Codex critic

budgets:
  claude:
    max_cost_usd_per_24h: 1.0
  codex:
    max_cost_usd_per_24h: 0.5

session_id_persist: ~/.ccteam/im/<slug>/<bot>/session-id
turns_jsonl: <project>/.ccteam/chat/<bot>/turns.jsonl    # ccteam-owned conversation SoT
```

`turns_jsonl` 格式(每 turn 一行):

```jsonl
{"turn_id":"...","ts":"...","user":"...","assistant":"...","usage":{"input_tokens":...,"output_tokens":...,"cache_read_input_tokens":...},"tool_calls":[...],"vendor":"claude"}
```

`session_id_persist` 文件丢失 / Anthropic 内部 jsonl 改格式 → fail-open + 从 `turns_jsonl` last-N turn 重建 conversation。

---

## 六、IM Squad 内部 schema

User entry:`/ccteam-creator "TG 群组 N 个 bot"` 推断 presence = "IM 群多 bot" → mode 3 chat group。

`.ccteam/workflow.yaml`:

```yaml
version: 0.6
mode: chat
preset: im_squad
im:
  channel_id_path: ~/.ccteam-im/channels/<slug>.json
  transport: openhuman
  group_mode: true                     # 群组 + bot-to-bot @ 路由
agents:
  - role: helpful-bot
    persona: tech_helper_zh
    bot_name: newton                   # 从 nickname 池取(或 user 指定)
    hop_limit: 3                       # 同上
    vendor: claude
  - role: critic-bot
    persona: code_critic_zh
    bot_name: curie
    hop_limit: 3
    vendor: codex                      # auto-critic 路径自动设置
    optional: true

bot_to_bot:
  routing: at_mention                  # @ critic-bot 才进入 critic 处理
  max_hop_per_chain: 3                 # 与 hop_limit 一致;不允许 bot 链超 3 跳
  hop_escalate_to: user                # 撞 limit → @ user 介入

budgets:
  claude:
    max_cost_usd_per_24h: 2.0
  codex:
    max_cost_usd_per_24h: 1.0
```

`bot_to_bot.routing` 选项:
- `at_mention` — 只有显式 @ 才转发(默认)
- `keyword` — agent prompt 内置关键词触发(在 backlog)
- `pubsub` — 任何 bot 发言所有其他 bot 都看到(在 backlog,且限小群组)

`hop_escalate_to`:`user` / `lead` / `none`(撞 limit 自动停)

---

## 七、跨 preset 通用字段

下列字段所有 preset 共享(`#[serde(default)]`):

| 字段 | 类型 | default | 说明 |
|---|---|---|---|
| `version` | str | "0.6" | yaml schema 版本 |
| `vendor` | enum | `claude` | `claude` / `codex`;per-agent 可异 |
| `budgets.<vendor>.max_cost_usd_per_24h` | f64 | preset-specific | 触顶 ccteam 自动 stop + IM 通知 |
| `fix_loop.max_attempts` | u32 | 3 | R6 红线,绝不静默重置 |
| `fix_loop.on_max_exceeded` | enum | `escalate` | `escalate` / `accept`(后者仅 dev 测试) |
| `agent.optional` | bool | `false` | 缺依赖(vendor binary 没装等)→ 跳过该 agent 而非 abort |
| `agent.max_spawn_depth` | u32 | 5 | recursion bomb guard(借 Codex `agent_max_depth`)|

---

## 八、IM transport 选项

`im.transport` 字段:

| 值 | 形态 | provider 支持 |
|---|---|---|
| `openhuman`(default)| `ccteam-imd` daemon + `openhuman/channels` crate | Telegram(stable);Slack / Discord(provider Rust 就位,onboarding skill 在 backlog);Lark / DingTalk / QQ / WeChat(feature gate,onboarding 在 backlog)|
| `official-telegram` | `claude --channels plugin:telegram@claude-plugins-official` | Telegram only |

切换走 `/ccteam-im-setup --transport <name>`,不让用户编辑 yaml。两 path 互斥(同 bot token 不可同时绑两条)。

---

## 九、Override 例子

`ccteam-creator` 生成的 yaml 是默认值,power user 可手改。改完 `/ccteam-control reload <slug>` 触发 hot-reload(部分字段)或 cold-reload(影响 topology 的字段)。

### 例 1:Pocket Assistant 切换 second-opinion 关闭

```yaml
# 原 auto
second_opinion:
  enabled: auto
# 改为强制关
second_opinion:
  enabled: false
```

### 例 2:Overnight Builder 加 security-reviewer 阶段

```yaml
agents:
  # ... 原 4 agent ...
  - role: security-reviewer
    vendor: codex
    triggers:
      - kind: artifact_received
        watch: .ccteam/outbox/release/   # release 后再过一次 security
```

### 例 3:IM Squad 给某 bot 单独提预算

```yaml
budgets:
  claude:
    max_cost_usd_per_24h: 5.0          # 群里聊得多
  codex:
    max_cost_usd_per_24h: 2.0
agents:
  - role: critic-bot
    budget_override:                    # 单 bot override
      claude.max_cost_usd_per_24h: 0.5  # critic 只拿小份额
```

### 例 4:同一 preset 切默认 vendor

```yaml
agents:
  - role: helpful-bot
    vendor: codex                       # 整 bot 跑 codex 而非 claude(罕见;通常自动 detect 路径不到这里)
```

---

## 十、相关文档

- 用户面入门:[quickstart.md](../quickstart.md)
- 8 个 ready 配方:[recipes.md](../recipes.md)
- 5 preset 用户文档:[user-manual.md](../user-manual.md)
- 自定义改造:[advanced/customize-workflow.md](customize-workflow.md)
- Codex 集成深拆:[advanced/multi-llm-codex.md](multi-llm-codex.md)
- 架构权威:`docs/tech-design.md` + `docs/interfaces.md` + `docs/orchestration-patterns.md`
