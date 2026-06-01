# 自定义 workflow.yaml(高级用户 / contributor)

> **先读这条**:绝大多数用户**不该编辑 `workflow.yaml`**。默认 UX 是 Claude session 内 `/ccteam` slash —— `ccteam-creator` skill 走 NL 对话推断 `mode`、选 persona、渲染并写出 yaml(全程不见文件名)。直接 vim yaml 仅在 (1) 写 contributor PR、(2) skill 不支持的 advanced 字段、(3) debug 时合理。
>
> 本文用内部术语(`WorkflowSpec` / `HarnessAdapter` / serde 枚举)—— 假设你愿读 Rust 源码。schema 权威是 `crates/ccteam-flow/src/workflow.rs`。

## 〇、当前态 vs 推后态(先分清这条)

> ⚠️ **`workflow.yaml` 的运行态语义住在 `ccteam-flow` 编排层,该层推后、未接入当前 gateway daemon。**
>
> 当前产品 `ccteam start` 是 **IM⇄session 路由网关**:它**不读 `workflow.yaml`**(`ccteam-im` / `ccteam-harness` 都不依赖 `ccteam-flow`),不跑编排 tick。运行中的 gateway 用 `BotRegistration` + IM 里 `/new claude|codex <handle>` 命令决定 spawn,**不**从 yaml 读 `mode` / `executor` / `trigger` / `squad` 等字段。
>
> 所以本文教的所有 yaml 字段,其**运行时效果**(`mode` 分发、`trigger:`/`watch:` 触发、`squad:` 路由、`scope`→cwd、`budget` 触顶、`plan_approval` HITL、`agent_team` lead、handoff 注入)都属于**编排层落地后**的行为。
>
> **当前真实在跑的薄切面**:
> - `ccteam-creator` **渲染并写出** `<project>/.ccteam/workflow.yaml`(脚手架,在你的 Claude session 内跑);
> - persona prefab **拷成文件** `.claude/agents/<role>.md`(只是落文件;当前 gateway 的 `/new claude` 起 `claude --name <...>` TUI,不带 `--agent`,不会自动把该 subagent 定义当成 session system prompt——按 persona 加载随编排层落地);
> - `chat_register_bot` 把 bot handle 落库,gateway 路由 `@<handle>` 靠它;
> - `ccteam probe-project` 探测 sensible `scope` 默认(§二·1)。
>
> 一句话:**写** yaml 是当前态,**读并据之编排** 是推后态。手编 yaml 在编排层点亮前不会改变 gateway 行为(除非你正用 `ccteam start <slug>` 的 legacy agent-team 路径或 `ccteam-flow` 测试)。

## 一、决策表

| 诉求 | 路径 |
|---|---|
| 起新项目 / 新 bot | `/ccteam-creator` skill |
| 改 tone / guardrail / 语言 | 改 `.claude/agents/<role>.md` 正文,不动 yaml |
| 加 reviewer / critic | `/ccteam-creator` 重跑(中断式)|
| 改 budget cap | 改 `workflow.yaml::budget`(或 `/ccteam-control`)|
| **加新 mode / executor / trigger** | **vim yaml**(本文)|
| **写新 preset PR** | **vim yaml + 加 template**(本文 + `workflow_templates/`)|

## 二、workflow.yaml 完整 schema

位置:`<project>/.ccteam/workflow.yaml`(canonical;旧 `<project>/workflow.yaml` 仅 discovery fallback)。权威是 `crates/ccteam-flow/src/workflow.rs::WorkflowSpec`。

```yaml
name: helpful-bot-demo               # 必;workflow 标识(无 version 字段)
description: 一句话说明              # 可选

mode: chat                           # 可选,默认 artifact-driven
                                     # { artifact-driven | agent-team | chat | human-approval }
                                     # kebab-case serde 枚举;决定 mode-specific 块
enabled: true                        # 可选,默认 true

budget:                              # 可选,扁平单块
  max_cost_usd_per_24h: 5.00         # 触顶 → budget_exceeded + auto-disable
  max_agent_spawns_per_hour: 100     # 滚动 1h spawn 速率顶(防自激)

agents:                              # role → AgentSpec;IndexMap 保序
  helpful-bot:                       # role 名仅 [a-z0-9_-];匹配 .claude/agents/<role>.md
    executor: claude                 # 可选,默认 claude;{ claude | codex }(严格小写)
    model: claude-opus-4-7           # 可选,自由字符串;省略 = vendor 默认 model
    trigger: manual                  # 必;{ manual | schedule | gate | watch:<path> }(单数标量)
    parallelism: 1                   # 可选;> 1 仅 watch: 合法;schedule/gate/manual 强制单实例
    scope: src                       # 可选;相对项目根的子目录,spawn 的 cwd;禁绝对路径 + 禁 ..
    input: .ccteam/inbox/            # 可选;CCTEAM_INPUT env;gate trigger 必填
    output: .ccteam/outbox/          # 可选;CCTEAM_OUTPUT env
    schedule: "0 22 * * *"           # trigger: schedule 时必填,5 字段 cron
    timeout: 30m                     # 可选
    on_timeout: escalate             # 可选;{ escalate | retry | skip }
    chat_handle: curie               # 可选;mode: chat;@curie 而非 @helpful-bot
    plan_approval:                   # 可选;HITL 门(见 §plan-approval)
      enabled: true
      outbox: telegram
      timeout_min: 60
      on_timeout: escalate

# --- mode: chat 专用块 ---
chat:                                # 可选;mode: chat;省略则全用默认值
  bot_name: helpful-bot              # 可选;默认从 creator 命名池取
  compact_every_turns: 50            # 可选;None = 让 Claude 自动 compact
  hop_limit: 3                       # 默认 3;必须 >= 1;bot-to-bot @ 链上限
  recover_last_n_turns: 20           # 默认 20;session-id 丢失时回放 turn 数
  turn_timeout_sec: 90               # 默认 90;每 turn watchdog;从不 kill;必须 >= 1
  chat_acl:                          # 可选;默认 None = 放行任意 IM 用户/群
    allow_users: ["12345"]
    allow_groups: ["-100123"]

# --- mode: agent-team 专用块 ---
agent_team:                          # mode: agent-team 必填;详见 §agent-team
  team_name: my-team                 # = ~/.claude/teams/<team_name>/
  lead_seed: |                       # 首条 user-turn 消息(非 system prompt)
    <团队任务>
  teammate_mode: in-process          # in-process | tmux | auto
  cleanup_on_stop: ask-lead          # force-kill | ask-lead | leave-running
  auto_spawn_teammates: false        # Plan-first:false 时 lead 等用户批准才 spawn

# --- 可选:静态 squad 运行时路由(仅 artifact-driven / human-approval)---
squad:
  leader: coordinator                # 必须是 agents: 里已声明的 role
  members: [backend, frontend, docs] # 每个都必须在 agents: 里已声明;非空
  hop_limit: 3                       # 默认 3;必须 >= 1
```

**不许写** `prompt:` / `system_prompt:` / `messages:` —— 红线(`crates/ccteam-flow/src/workflow.rs` 模块头);agent 行为住 `.claude/agents/<role>.md`。

**注意 schema 边界**:`serde_yaml` 不拒绝未知字段。下列在旧文档出现过但**不在 `WorkflowSpec` 里**,写了会被静默忽略(no-op):`version:` / `vendor:`(用 `executor:`)/ `preset:` / `im:` / `im_channels:` / `triggers: [...]`(数组形态)/ `second_opinion:` / `bot_to_bot:` / `notification:` / `budget_override:` / `session_id_persist:` / `turns_jsonl:` / `max_parallel:`(真名 `parallelism`)。

<a name="sensible-defaults"></a>
### 二·1 Sensible defaults —— `ccteam probe-project` 探测

`/ccteam-creator` 第一次在仓库内跑时先调 `ccteam probe-project` 探测项目类型 + 主语言,把结果喂给 yaml 渲染 —— 你拿到的初始 `scope:` 已按项目结构 pre-populate。手编 yaml 的高级用户也可单独跑:

```bash
ccteam probe-project --json
# {"kind":"monorepo","languages":["rust","typescript"],
#  "has_tests":true,
#  "probable_scope":["crates/foo/src","crates/bar/src","services/web/src"]}
```

`kind` 取值小写:`monorepo` / `single-repo` / `docs-only` / `scripts-only` / `empty`;`languages` 亦小写。

#### 探测启发式(纯文件存在性,不 parse 任何代码)

| 信号 | 推断 `kind` |
|---|---|
| `Cargo.toml` 内 `workspace.members` | `monorepo` (rust) |
| `package.json` 内 `workspaces` 或 `pnpm-workspace.yaml` | `monorepo` (typescript) |
| `go.work` | `monorepo` (go) |
| 单 `Cargo.toml` / `package.json` / `pyproject.toml` | `single-repo` |
| 只有 `*.md` + 无 source dir | `docs-only` |
| 只有 `*.sh` / `*.py` script | `scripts-only` |

#### `probable_scope` 截断规则

monorepo 探测出 10+ crate 时,按 LOC 排序取 top(fallback alphabetical),避免 scope 默认值过宽。用户对 prompt 内直说"scope 改成 X"即可 override。

#### preset × probe 矩阵(sensible defaults)

| preset | probe = monorepo | probe = single-repo | probe = docs-only |
|---|---|---|---|
| `bg-overnight` | `scope: <probable_scope[0]>`(top-LOC member)| `scope: src` | `scope: docs` |
| `inproc-solo` / `inproc-team` | scope 经 `suggested_teammates` 提示分发 | 同 | 同 |
| `chat-pocket` / `chat-squad` | 无 scope(chat bot 跑项目根)| 同 | 同 |

`bg-overnight` 把 `scope:` overlay 进 `agents.<role>`(`apply_probe_defaults`);`agent-team` preset 渲染 `agents: {}`,scope 走 `suggested_teammates`;chat preset 不设 scope。不在范围:跨语言栈完整 template library、LLM-assisted 完整 role auto-gen。漏判时 user prompt override probe 结果。

## 三、5 preset 的 yaml diff(从 ccteam-creator 默认产物出发)

`ccteam-creator` 模板在 `crates/ccteam-core/src/templates/workflow_templates/{inproc-solo,inproc-team,bg-overnight,chat-pocket,chat-squad}.yaml`,选模板靠 `mode_inferrer` + persona pick。preset → `mode` 映射见 [presets-reference.md](presets-reference.md) §一。

### chat-pocket → chat-squad(单 bot → 多 bot 群组)

```diff
 chat:
-  hop_limit: 1
+  hop_limit: 3
   chat_acl:
-    allow_users:
-      - "<owner_chat_id>"
+    allow_groups:
+      - "<group_chat_id>"
 agents:
   tech-helper:
     trigger: manual
+  critic:
+    trigger: manual
+    executor: codex                  # critic 类角色 + codex 可用 → auto 注入(creator Phase 3.5)
```

### chat-pocket → inproc-solo(chat → agent-team)

```diff
-mode: chat
+mode: agent-team
-chat: ...                            # 整段删
-agents:
-  tech-helper:
-    trigger: manual
+agent_team:
+  team_name: <slug>
+  lead_seed: |
+    <brief>
+  teammate_mode: in-process
+  auto_spawn_teammates: false
+  suggested_teammates:
+    - role: tech-helper
+      kind: definition
+      spawn_brief: |
+        <per-task 指令>
+agents: {}                           # agent-team 模式 agents 为空
```

### chat-pocket → bg-overnight(chat → artifact-driven)

```diff
-mode: chat
+mode: artifact-driven
-chat: ...                            # 整段删
+budget:
+  max_cost_usd_per_24h: 10.00
 agents:
-  tech-helper:
-    trigger: manual
+  planner:
+    trigger: manual
+    scope: src
+  executor:
+    trigger: watch:.ccteam/inbox/executor
+    scope: src
```

### 加 Codex critic(artifact-driven 任意 preset)

```diff
 agents:
   main:
     executor: claude
+  critic:
+    executor: codex
+    model: o4-mini
+    trigger: watch:.ccteam/main-output/
```

`ccteam doctor --check-codex-auto-critic` 验装 + auth;exit 0 注入 `executor: codex`,exit 2/3 silent fallback 到 `claude`(不静默假装成功)。

## 四、`.claude/agents/<role>.md` frontmatter

```markdown
---
name: helpful-bot                    # 必,匹配 workflow.yaml::agents.<role>
description: 中文技术助手,Claude Code best practices 范围内答问
tools: [Read, Grep, Edit, Bash]      # Claude Code subagent 白名单;漏写 = 用不上
model: claude-opus-4-7               # 自由字符串
---

# System Prompt 正文(LLM 自读)
你是一个中文技术助手 ...
```

**坑**:`executor: codex` 时 `.claude/agents/<role>.md` 的 `tools:` 字段无意义(Codex 自有 tool surface)。agent 行为正文是 system prompt 唯一落点;yaml 不重复声明 vendor 之外的行为字段。

## 五、自定义 persona(从 prefab 改)

prefab 在 `skills/ccteam-creator/personas/<id>/`,目录结构:

```
personas/tech-helper/
├── en/role.md                       # 英文 persona body → .claude/agents/<role>.md
├── zh/role.md                       # 中文 persona body
personas/manifest.toml               # 注册表:[[persona]] { id, label_en, label_zh,
                                     #   description, tags, default_mode, codex_eligible }
```

现有 prefab:`tech-helper` / `writing-assistant` / `translator` / `tutor` / `project-lead` / `code-critic` / `customer-support`。

加新 prefab:

```bash
cp -r skills/ccteam-creator/personas/tech-helper \
      skills/ccteam-creator/personas/my-custom-bot
$EDITOR skills/ccteam-creator/personas/my-custom-bot/{en/role.md,zh/role.md}
# 在 personas/manifest.toml 里加一个 [[persona]] 条目(id = "my-custom-bot")
```

`ccteam-creator` Phase 3 读 `manifest.toml`,按 `description` + `tags` LLM-match 用户意图。中文意图默认取 `zh/role.md`,否则 `en/role.md`。**用户自定义 persona 在 backlog** —— 本节是 contributor 加内置 prefab 的路径。

## 六、Trigger 类型

`trigger:` 是单数 YAML 标量(`crates/ccteam-flow/src/workflow.rs::Trigger`):

| Trigger | 语义 | 并发 |
|---|---|---|
| `manual` | 显式触发(meta-agent / chat 用户)| 强制单实例 |
| `schedule` | 5 字段 cron(在同 role 的 `schedule:` 字段里),skip-missed 语义 | `parallelism` 上限内 |
| `gate` | 等 `trigger_gate` MCP 调用释放;**必须配 `input:`** | 强制单实例 |
| `watch:<path>` | inotify(Linux)/ fsevents(macOS),**项目相对路径**,非空 | `parallelism` 上限内并发 |

`parallelism > 1` 仅 `watch:` 合法;`schedule` / `gate` / `manual` 强制单实例(`validate()` 会拒)。`schedule` trigger 不带合法 `schedule:` cron 表达式直接 workflow 加载失败(不静默永不触发)。

## 七、Squad 静态运行时路由

`squad:` 块(`SquadSpec`)在 artifact-driven roster 之上加一层运行时分发:`leader` 角色在运行时决定哪个 `member` 处理子任务,而不是写死的 `output:` 目录。

```yaml
squad:
  leader: coordinator
  members: [backend, frontend, docs]
  hop_limit: 3
```

规则(`validate_squad`):

- **只在 `mode: artifact-driven` / `human-approval` 合法**(`chat` / `agent-team` 没有静态 roster 可路由)。
- `leader` 和每个 `members[]` 都必须是 `agents:` 里已声明的 role(成员集**静态声明**,可审计)。
- 路由协议:leader 写文件名 `<member>--<rest>.md` 到 `.ccteam/squad/`;watcher 按**文件名前缀**(非文件体)spawn 对应 member,不解析正文、不注入 prompt。
- `leader→member→leader` 环由 `hop_limit` 封顶:文件名带 hop 计数(`<member>--h<N>--<rest>.md`),撞 limit → `escalation` 事件(对应 fix-loop 撞 3 次必 escalate 红线)。`hop_limit` 必须 ≥ 1。

## 八、Plan-approval HITL 门

任意 agent 加 `plan_approval` 块,把它的 plan markdown 变成需用户批准的门(`crates/ccteam-flow/src/plan_approval.rs`):

```yaml
agents:
  reviewer:
    trigger: watch:.ccteam/main-output/
    plan_approval:
      enabled: true
      outbox: telegram                # plan 推哪个 IM outbox
      timeout_min: 60
      on_timeout: escalate            # 超时行为
```

agent 写 `<project>/.ccteam/plans/<agent>-*.md` 后暂停,等用户在 IM 回 APPROVE / REJECT / EDIT,或 `timeout_min` 到点触发 `on_timeout`。这是 per-agent 字段;与顶层 `mode: human-approval`(每个 `agent_done` 后都 gate)正交。

## 九、agent-team mode 与 lead_seed

`mode: agent-team` 渲染 `agents: {}`,改用 `agent_team` 块声明一个 ccteam 托管的 `__lead` Claude session(`AgentTeamSpec`,字段表见 [presets-reference.md](presets-reference.md) §三)。`inproc-solo` / `inproc-team` 两个 preset 走这条路。要点:

- `team_name` = Anthropic `~/.claude/teams/<team_name>/` 目录名,`[a-z0-9_-]`。
- `lead_seed` 是**首条 user-turn 消息**,不是 system prompt(`__lead.md` 系统 prompt 跨所有 workflow 固定)。
- `auto_spawn_teammates: false`(默认)= Plan-first:lead 先出 TEAM PLAN,等用户 `go`/`yes`/`approve` 才调 `Task`。
- `suggested_teammates[]` 可空(lead 全权决定)或预置;`kind: definition` 配 `.claude/agents/<role>.md`,`kind: ad-hoc` 必须带 `adhoc_model`。
- yaml 在 spawn 时冻结到 `snapshot_path`(默认 `.ccteam/team-snapshot.json`),运行中改 yaml 不影响在跑 team。

## 十、`.claude/agents/` 是 vendor 一致性的副本

`workflow.yaml::agents.<role>.executor` 决定运行时用哪个 vendor 的 harness;`.claude/agents/<role>.md` 承载 role 行为正文。两处都写 model 时以 yaml 为准。`executor: codex` 时 role 仍要有对应的 `.claude/agents/<role>.md`(其 `tools:` 字段对 Codex 无意义,但 markdown 正文仍是该 role 的 prompt 来源)。

## 十一、常见陷阱

1. **yaml 缩进** —— list item 用 `-` + 2 空格;tab 被 `serde_yaml` 直接 reject。
2. **`mode` 是 kebab-case serde 枚举** —— `{ artifact-driven | agent-team | chat | human-approval }`。`in_proc` / `bg` / `in-proc` 都不是合法值。改 mode 必须同时改 mode-specific 块(`mode: chat` 漏 `chat:` 走全默认还能起;`mode: agent-team` 漏 `agent_team:` 直接校验失败)。
3. **`executor` 严格小写枚举** —— `{ claude | codex }`;`vendor:` / `openai` / `anthropic` 全无效(`vendor:` 会被静默忽略)。
4. **未知字段静默忽略** —— `serde_yaml` 不 deny unknown;拼错字段名(如 `max_parallel` 应为 `parallelism`、`triggers` 应为 `trigger`)不会报错,只是 no-op。改完用 `ccteam-flow` 测试或 `WorkflowSpec::load` round-trip 自检。
5. **`watch:<path>` 是项目相对路径** —— `watch:/abs/path` 会被 `validate()` 拒;空 `watch:` 也拒。
6. **`scope` 禁绝对路径 + 禁 `..`** —— path-traversal guard,只能指向项目根内子树;省略 = 项目根。
7. **`hop_limit` / `turn_timeout_sec` 必须 ≥ 1** —— 0 被校验拒(没有"禁用"语义;要默认就省略字段)。
8. **`parallelism > 1` 仅 `watch:`** —— `schedule` / `gate` / `manual` 强制单实例。
9. **`gate` trigger 必须配 `input:`** —— 否则 gate 释放时没东西交接,校验失败。
10. **squad 只在 artifact-driven / human-approval** —— `chat` / `agent-team` 写了 `squad:` 校验失败(无静态 roster)。

## 十二、绕过 skill dialogue 手起项目

`ccteam new <slug> [--team <team>]` 只做最小脚手架(建项目目录 + `.ccteam/` 结构);它**不**带 `--preset` / `--persona` / `--mode` 等 flag,也不渲染带 persona 的 workflow.yaml。要完整脚手架(preset → persona → workflow.yaml → bot 注册)仍走 `/ccteam-creator` skill。

CI / 自动化场景下纯手编:

```bash
ccteam new my-bots                   # 建 ~/projects/<team>-my-bots/(默认 team=dev)
$EDITOR ~/projects/dev-my-bots/.ccteam/workflow.yaml   # 按 §二 schema 手写
```

preset 模板可作起点参考(`crates/ccteam-core/src/templates/workflow_templates/*.yaml`),把占位符 `{{...}}` 替换成实值即可。

> 提醒(回 §〇):写出 yaml 是当前态,但 gateway 据之编排是推后态——手编完 yaml,当前 daemon 仍按 IM 里的 `/new` 命令 + `BotRegistration` 跑 chat session,不会自动按 `mode: artifact-driven` / `agent-team` 启动编排。

## 十三、看实际跑的是什么

```bash
cat ~/projects/<slug>/.ccteam/workflow.yaml
tail -f ~/projects/<slug>/.ccteam/progress.jsonl
```

`progress.jsonl` 是业务事件 SoT;chat 对话原文走 `<project>/.ccteam/chat/<bot>/turns.jsonl`(ccteam-owned,不依赖 Anthropic 内部 `~/.claude/projects/`)。gateway / session 运维(`ccteam status` / `doctor` / 重启续接)见 [user-manual.md](../user-manual.md) §4。
