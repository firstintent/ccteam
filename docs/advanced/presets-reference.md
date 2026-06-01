# Presets Reference —— preset 到 workflow.yaml 的内部映射

> **读者**:已经用过 [quickstart](../quickstart.md) 和 [recipes](../recipes.md) 的 power user / contributor。本文展示 `ccteam-creator` 的 5 个 preset 各自渲染出什么 `workflow.yaml`——映射到哪个执行模式(`mode`)、哪个 vendor(`executor`)、各字段的默认值。日常使用者**不需要**看本文:`ccteam-creator` skill 通过 NL 对话自动推断并生成全部字段。

---

## 〇、先分清「当前」与「推后」

> ⚠️ **`workflow.yaml` 的运行态语义住在 `ccteam-flow` 编排层(推后,未接入当前 gateway daemon)。**
>
> 当前产品是 **IM⇄session 路由网关 daemon**(`ccteam start`):它**不读 `workflow.yaml`、不跑编排 tick、无 orchestrator 循环**(见 [orchestration-patterns.md](../orchestration-patterns.md) 顶部 banner + [user-manual.md](../user-manual.md))。事实上 `ccteam-im` / `ccteam-harness` 都不依赖 `ccteam-flow`;运行中的 gateway 用 `BotRegistration` + `/new claude|codex <handle>` 命令决定 spawn 哪个 vendor,**不**从 yaml 读 `mode` / `executor` / `trigger` / `squad` 等字段。
>
> 因此本文绝大多数字段(`mode` 分发、`executor`→spawn、`trigger:`/`watch:`、`squad:` 路由、`budget` 触顶、`scope`→cwd、fix-loop、`agent_team` lead)描述的是**编排层落地后**的行为,**不是**当前 daemon 的运行方式。
>
> **当前真实在跑的薄切面**(本文里只有这几块属于当前态):
> 1. **`ccteam-creator` 脚手架流程**——在你的 Claude session 内跑,推断 preset、选 persona、渲染并写出 `<project>/.ccteam/workflow.yaml`;
> 2. **persona 文件安装**——把 prefab 拷成 `.claude/agents/<role>.md`(**只是落文件**;当前 gateway 的 `/new claude` 起的是 `claude --name <...>` TUI,不带 `--agent`,不会把这个 subagent 定义当成 session 的 system prompt——运行时按 persona 加载随编排层落地,见 §〇 末);
> 3. **`chat_register_bot` 把 bot handle 落库**——gateway 路由 `@<handle>` 靠这条记录,不靠 yaml;
> 4. **`ccteam probe-project`**——`scope` sensible defaults 的探测 CLI(见 [customize-workflow.md](customize-workflow.md) §二·1)。
>
> 一句话:`ccteam-creator` **写** yaml 是当前态,gateway **读** yaml 是推后态。本文教你看懂它写出来的东西,以及编排层点亮后这些字段会怎么生效。

权威 schema 见 `crates/ccteam-flow/src/workflow.rs`(`WorkflowSpec`)+ [interfaces.md](../interfaces.md);preset 渲染层见 `crates/ccteam-core/src/templates/workflow_templates/`。

---

## 一、5 preset 总表

`ccteam-creator` 的 preset(`crates/ccteam-core/src/templates/workflow_templates/mod.rs::Preset`)用 kebab-case 命名,对应一个 `*.yaml` 模板文件:

| preset(yaml 渲染名)| 人话标签 | 渲染出的 `mode` | 默认 `executor`(vendor)| 默认 budget cap | 模板文件 |
|---|---|---|---|---|---|
| `inproc-solo` | Solo Sidekick | `agent-team` | claude | 无(继承父 session)| `inproc-solo.yaml` |
| `inproc-team` | Team Sprint | `agent-team` | claude(critic 可 auto-Codex)| 无(继承父 session)| `inproc-team.yaml` |
| `bg-overnight` | Overnight Builder | `artifact-driven` | claude(critic 可 auto-Codex)| `max_cost_usd_per_24h: 10.00` | `bg-overnight.yaml` |
| `chat-pocket` | Pocket Assistant | `chat` | claude | `max_cost_usd_per_24h: 5.00` | `chat-pocket.yaml` |
| `chat-squad` | IM Squad | `chat` | claude(per-bot 可异)| `max_cost_usd_per_24h: 20.00` | `chat-squad.yaml` |

要点:

- **`mode` 只有 4 个值**(`WorkflowMode` 枚举,kebab-case):`artifact-driven`(默认)/ `agent-team` / `chat` / `human-approval`。`ccteam-creator` 的 5 preset 只用到前三个;`human-approval` 不在任何 preset 里,要手写。
- **vendor 是概念,`executor:` 是 yaml 字段名**(取值 `claude` / `codex`,严格小写;省略默认 `claude`)。本文后续凡说"vendor"指概念,凡写 `executor:` 指 yaml key。
- **没有** `version:` / `vendor:` / `preset:` 这些字段——`preset` 仅是 `ccteam-creator` 内部选模板的入参,不写进 yaml(yaml 的 `name:` 才是 workflow 标识)。

---

## 二、Solo Sidekick —— `inproc-solo`(mode `agent-team`)

单个 in-process teammate,跟用户当前 Claude session 并跑。`agent-team` 模式下 lead = 用户当前 session,通过 Anthropic 原生 `TeamCreate` + `Task` 工具驱动。

渲染产物(占位符已用默认 ctx 填好示意):

```yaml
name: demo-workflow
description: |
  Solo Sidekick — one in-process teammate alongside the user's session.

mode: agent-team

agent_team:
  team_name: demo-workflow
  lead_seed: |
    <用户原话 brief>

    Output a TEAM PLAN with one teammate (role: executor),
    then STOP and wait for `go` / `yes` / `approve` before calling Task.
  teammate_mode: in-process
  cleanup_on_stop: ask-lead
  snapshot_path: .ccteam/team-snapshot.json
  auto_spawn_teammates: false
  suggested_teammates:
    - role: executor
      kind: definition
      spawn_brief: |
        <per-task 指令>

agents: {}
```

注意 `agent-team` 模式下 `agents:` 是**空 map**——lead 在运行时通过 `suggested_teammates` + `lead_seed` 决定 team 组成,不是声明式 roster。`lead_seed` 是**首条 user-turn 消息**,不是 system prompt(红线:永不注入 system prompt)。

---

## 三、Team Sprint —— `inproc-team`(mode `agent-team`)

N 个 in-process teammate 并行,Orchestrator-Worker 模式。与 `inproc-solo` 同结构,差别在 `lead_seed` 要求 lead 把任务拆成 `{{worker_count}}`(默认 3)个并行子任务,且 `suggested_teammates` 预置 `executor` + `critic` 两类:

```yaml
mode: agent-team

agent_team:
  team_name: demo-workflow
  lead_seed: |
    <用户原话 brief>

    Decompose the task into 3 parallel subtasks. Output a TEAM PLAN
    listing each teammate's role + spawn_brief, then STOP and wait for
    `go` / `yes` / `approve` before calling Task.
  teammate_mode: in-process
  cleanup_on_stop: ask-lead
  snapshot_path: .ccteam/team-snapshot.json
  auto_spawn_teammates: false
  suggested_teammates:
    - role: executor
      kind: definition
      spawn_brief: |
        Pick one of the decomposed subtasks the team-lead assigns to you.
        Apply edits, run tests, report results via SendMessage to team-lead.
    - role: critic
      kind: definition
      spawn_brief: |
        Review the executor teammates' output. Flag regressions, style
        drift, missed cases. Report via SendMessage to team-lead.

agents: {}
```

`critic` 这类角色满足 Codex auto-critic 条件时,`ccteam-creator` 会把它 vendored 到 Codex(见 §七)。

`agent_team` 块的字段(`crates/ccteam-flow/src/workflow.rs::AgentTeamSpec`):

| 字段 | 默认 | 说明 |
|---|---|---|
| `team_name` | 必填 | = Anthropic `~/.claude/teams/<team_name>/` 目录名;`[a-z0-9_-]` |
| `lead_seed` | 必填 | 首条 user-turn 消息(非 system prompt)|
| `teammate_mode` | `in-process` | `in-process` / `tmux` / `auto` |
| `cleanup_on_stop` | `force-kill` | `force-kill` / `ask-lead` / `leave-running` |
| `snapshot_path` | `.ccteam/team-snapshot.json` | yaml 在 spawn 时冻结到此,运行中改 yaml 不影响在跑 team |
| `auto_spawn_teammates` | `false` | Plan-first:`false` 时 lead 必须等用户 `go`/`yes`/`approve` 才 spawn |
| `suggested_teammates[]` | 空 = lead 全权决定 | 每项 `{ role, kind, spawn_brief, adhoc_model?, adhoc_color?, adhoc_tools? }`;`kind: ad-hoc` 必须带 `adhoc_model` |

---

## 四、Overnight Builder —— `bg-overnight`(mode `artifact-driven`)

唯一渲染成 `artifact-driven` 的 preset:无人值守、由文件系统控制平面(`ArtifactWatcher` + `trigger: watch:`)驱动 spawn。

```yaml
name: demo-workflow
description: |
  Overnight Builder — artifact-driven QA fix-loop running unattended.
  Triggers: planner → executor → critic → planner (loop until done).

mode: artifact-driven

budget:
  max_cost_usd_per_24h: 10.00

agents:
  planner:
    trigger: manual
    scope: src                  # 由 probe-project sensible defaults 填入(见下)
  executor:
    trigger: watch:.ccteam/inbox/executor
    scope: src
  critic:
    trigger: watch:.ccteam/inbox/critic
    scope: src
```

- **`trigger:` 是单数标量**,取值 `manual` / `schedule` / `gate` / `watch:<path>`(`watch` 的路径必须项目相对,非空)。**不是** `triggers: [...]` 数组。
- `scope:` 是 `ccteam probe-project` 探测出的子目录(见 [customize-workflow.md](customize-workflow.md) §二·1);未探测到时模板渲染为空(无 `scope:` 行)。
- `critic` 角色满足条件会被 vendored 到 Codex(§七)。

> 模板当前在 `agents.<role>` 下渲染的并发字段写作 `max_parallel: 1`,但 `WorkflowSpec` 的真实字段名是 `parallelism`;`serde_yaml` 不拒绝未知字段,故 `max_parallel` 会被**静默忽略**(等同未设并发上限)。手编 yaml 控制并发请用 `parallelism:`(见 [customize-workflow.md](customize-workflow.md) §六)。**这是模板里的一处 no-op 字段名,不影响 `parallelism` 的语义。**

---

## 五、Pocket Assistant —— `chat-pocket`(mode `chat`)

单 bot,绑一个 IM 私聊。这是当前 gateway daemon **直接服务**的对象类型——用户在 IM 里 `/new claude <handle>` 起的就是 chat-mode session(见 [user-manual.md](../user-manual.md) §1–2)。

```yaml
name: demo-workflow
description: |
  Pocket Assistant — single bot DM on telegram. Long-running chat
  session with last-N turn replay on session loss.

mode: chat

chat:
  bot_name: "@demo_bot"
  compact_every_turns: 20
  hop_limit: 1
  recover_last_n_turns: 8
  chat_acl:
    allow_users:
      - "123456789"

budget:
  max_cost_usd_per_24h: 5.00

agents:
  tech-helper:
    trigger: manual
```

`chat:` 块字段(`crates/ccteam-flow/src/workflow.rs::ChatSpec`):

| 字段 | 默认 | 说明 |
|---|---|---|
| `bot_name` | None(creator 从命名池取)| bot handle = agent role,映射 `.claude/agents/<bot>.md` |
| `compact_every_turns` | None(让 Claude 自动 compact)| 多少 turn 后发 `/compact` |
| `hop_limit` | 3 | bot-to-bot `@` 链上限;必须 ≥ 1;pocket 模板用 1(单 bot 无需协作)|
| `recover_last_n_turns` | 20 | session-id 丢失时从 `turns.jsonl` 回放的 turn 数 |
| `chat_acl` | None(放行任意 IM 用户)| `{ allow_users: [...], allow_groups: [...] }` |
| `turn_timeout_sec` | 90 | 每 turn watchdog;`1×` 提示"还在跑",`2×` 提示"卡住";**从不 kill**;必须 ≥ 1 |

对话原文落 `<project>/.ccteam/chat/<bot>/turns.jsonl`(ccteam-owned SoT,不依赖 Anthropic 内部 `~/.claude/projects/`)。这是**运行态行为**,由 gateway 的 chat 路径写,**不是 yaml 字段**——别在 yaml 里写 `turns_jsonl:` / `session_id_persist:`,它们不存在。

---

## 六、IM Squad —— `chat-squad`(mode `chat`)

多 bot,绑一个 IM 群组,bot 之间可 `@` 互相协作(`hop_limit` 深度内)。同样是当前 gateway 直接服务的 chat-mode 对象。

```yaml
name: demo-workflow
description: |
  IM Squad — multiple bots in a telegram group room with bot-to-bot
  @ addressing. Useful for "ask team" style consultations.

mode: chat

chat:
  bot_name: "@lead_bot"
  compact_every_turns: 30
  hop_limit: 3
  recover_last_n_turns: 12
  chat_acl:
    allow_groups:
      - "-100123456789"

budget:
  max_cost_usd_per_24h: 20.00

agents:
  lead:
    trigger: manual
  critic:
    trigger: manual
```

- `chat:` 块只承载 `primary_bot_handle`(主 bot);其余 bot 由 `agents:` map 下的多个 role 体现,每个 role 一个 `.claude/agents/<role>.md`。
- bot 之间靠 `@<handle>` 互相寻址。handle 不是 yaml 写死的——`ccteam-creator` 调 `chat_register_bot` 自动从 scientist 命名池 mint,落进 `BotRegistration.chat_handle`,**gateway 路由靠这条记录**(当前态)。yaml 里可选 `agents.<role>.chat_handle:` 来 pin 一个固定 handle override。
- `hop_limit: 3` 是 bot-to-bot 链路上限(撞 limit → escalate,对应 fix-loop 撞 3 次必 escalate 红线在 chat 模式的延伸)。

---

## 七、Codex auto-critic(`executor: codex` 注入)

`ccteam-creator` 在 Phase 3.5 检测:当匹配到的 persona 是 critic 类角色(`code-critic` / `reviewer` / `code-reviewer` / `pr-reviewer` / `architect`,或 tag 含 `critic` / `second-opinion`),且本机 Codex 可用且已登录时,把该 role vendored 到 Codex:

```bash
ccteam doctor --check-codex-auto-critic
# stdout 末行: {"available": true|false, "exit_code": 0|2|3, ...}
# exit 0 → 注入 agents.<role>.executor: codex
# exit 2/3 → silent fallback 到 claude(不注入)
```

注入后 yaml 里只多一行:

```yaml
agents:
  critic:
    trigger: watch:.ccteam/inbox/critic
    executor: codex            # auto-critic 注入;不可用时为 claude(省略)
    model: o4-mini             # 可选;model 是自由字符串,非枚举
```

`model:` 是**自由字符串**(如 `claude-opus-4-7` / `gpt-5-codex` / `o4-mini`),省略 = vendor 默认 model。对话全程用户不编辑 yaml,也看不到 `executor: codex`——只在 PROJECT PLAN 里看到一行 "Codex critic: auto-enabled"。Codex 集成深拆见 [multi-llm-codex.md](multi-llm-codex.md)。

---

## 八、跨 preset 通用字段

下列 `WorkflowSpec` 顶层字段所有 preset 共享(均 `#[serde(default)]`,可省):

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `name` | str | 必填 | workflow 标识(= yaml 文件里唯一标识,**非** `version`)|
| `description` | str | 省略 | 给 meta-agent / UI 看的一句话 |
| `mode` | enum | `artifact-driven` | `artifact-driven` / `agent-team` / `chat` / `human-approval` |
| `enabled` | bool | `true` | `false` 让编排层跳过这个 workflow(state / progress / artifact 目录保留)|
| `budget.max_cost_usd_per_24h` | f64 | preset-specific | 滚动 24h 成本顶;触顶 → `budget_exceeded` + auto-disable |
| `budget.max_agent_spawns_per_hour` | u32 | 省略 | 滚动 1h spawn 速率顶(防自激 runaway)|

`agents.<role>` 级字段:`executor`(`claude`/`codex`)/ `model`(自由 str)/ `trigger`(必填)/ `scope`(相对路径)/ `parallelism`(仅 `watch:` 有意义)/ `input` / `output` / `schedule`(`trigger: schedule` 时必填的 5 字段 cron)/ `timeout` / `on_timeout`(`escalate`/`retry`/`skip`)/ `plan_approval` / `chat_handle`。完整 schema + 校验规则见 [customize-workflow.md](customize-workflow.md) §二。

> `budget` 是**扁平单块**(`{ max_cost_usd_per_24h, max_agent_spawns_per_hour }`),5 个模板都用这个形态。`WorkflowSpec` 另有一个 per-vendor 预算字段(`claude` / `codex` 分账)仍是内部过渡 key,**不**作为用户面字段记录;要 per-vendor 分账请参照 [interfaces.md](../interfaces.md) 的当前 schema,别照搬旧文档里 `budgets.claude.X` 写法(那不匹配 schema)。

---

## 九、相关文档

- 用户面入门:[quickstart.md](../quickstart.md) · [user-manual.md](../user-manual.md)
- 配方:[recipes.md](../recipes.md)
- 自定义 / 手编 yaml:[customize-workflow.md](customize-workflow.md)
- Codex 集成深拆:[multi-llm-codex.md](multi-llm-codex.md)
- 编排层设计(推后):[orchestration-patterns.md](../orchestration-patterns.md)
- schema 权威:`crates/ccteam-flow/src/workflow.rs` + [interfaces.md](../interfaces.md)
- 模板权威:`crates/ccteam-core/src/templates/workflow_templates/`
