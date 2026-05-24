# 自定义 workflow.yaml(高级用户 / contributor)

> **先读这条**:绝大多数用户**不该编辑 `workflow.yaml`**。默认 UX 是 Claude session 内 `/ccteam` slash — `ccteam-creator` skill 走 NL 对话推断 mode、选 persona、生成 yaml(全程不见文件名)。直接 vim yaml 仅在 (1) 写 contributor PR、(2) skill 不支持的 advanced 字段、(3) debug 时合理。
>
> 本文用内部术语(`HarnessAdapter` / `ThreadEvent` / serde tagged enum)— 假设你愿读 Rust 源码。

## 一、决策表

| 诉求 | 路径 |
|---|---|
| 起新项目 / 新 bot | `/ccteam-creator` skill |
| 改 tone / guardrail / 语言 | 改 `.claude/agents/<role>.md` 正文,不动 yaml |
| 加 reviewer / critic | `/ccteam-creator` 重跑(中断式)|
| 改 budget cap | `/ccteam-control set-budget` |
| **加新 mode / vendor / trigger** | **vim yaml**(本文)|
| **写新 preset PR** | **vim yaml + 加 template**(本文)|

## 二、workflow.yaml 完整 schema

位置:`<project>/.ccteam/workflow.yaml`(canonical;旧 `<project>/workflow.yaml` 仅 fallback)。

```yaml
version: "0.6"                       # 必;迁移时 doctor 警告
mode: chat                           # 必,{ in_proc | bg | chat }(serde tagged enum)
                                     # 决定 HarnessAdapter 路径 + mode-specific 字段集
enabled: true                        # default true

budgets:                             # vendor 拆分
  claude:
    max_cost_usd_per_24h: 5.00       # 撞顶 → budget_exceeded + auto-disable
    max_agent_spawns_per_hour: 100
    max_tokens_per_turn: 200000      # chat 必填,防 100k history 烧穿
    max_cost_per_hour: 1.50          # spike 防
  codex:
    max_cost_usd_per_24h: 2.00
    max_tokens_per_turn: 100000

agents:
  - role: helpful-bot                # 必,匹配 .claude/agents/<role>.md
    vendor: claude                   # default claude;{ claude | codex }(严格小写枚举)
    model: opus-4-7                  # vendor-specific(claude:opus-4-7|sonnet-4-7|haiku;codex:o4|o4-mini)
    trigger: manual                  # { manual | watch:<path> | schedule:<cron> | webhook:<id> }
    parallelism: 1                   # 仅 watch 有意义;manual 强制 1
    timeout: 30m
    on_timeout: escalate             # { escalate | retry | skip }
    max_spawn_depth: 5               # recursion bomb guard;default 5
    # mode: chat 专用字段(其他 mode 写了 ignore + serde warn)
    bot_name: "@helpful_assistant"   # IM handle;default 从 agent_naming 池取
    hop_limit: 3                     # bot-to-bot @ 链路上限;default 3
    compact_every_turns: 50          # default 50,0 = 禁
    compact_every_tokens: 100000     # 双触发,default 100k

artifact_dirs:                       # mode: bg 专用;mode: chat 完全忽略
  - .ccteam/fix-requests/

im_channels:                         # mode: chat 专用;走 openhuman/channels
  - kind: telegram                   # { telegram | slack | discord | email | lark | ... (国内 IM 在 backlog) }
    credentials_ref: default         # ~/.ccteam/im/credentials.json 内 key
    chat_kind: dm                    # { dm | group }
    acl:                             # production must-have
      allow_user_ids: [12345]
      rate_limit_per_user: "10/h"
```

**不许写** `prompt:` / `system_prompt:` / `messages:` — 红线(`tech-design.md §3.3.3`);agent 行为住 `.claude/agents/<role>.md`。

<a name="sensible-defaults"></a>
### 二·1 Sensible defaults — `ccteam probe-project` 探测

`/ccteam-creator` 第一次在仓库内跑时,先调 `ccteam probe-project` 探测项目类型 + 主语言,把结果喂给 yaml 生成 ── 用户拿到的初始 `scope:` 已按项目结构 pre-populate。手编 yaml 的高级用户也可单独跑:

```bash
ccteam probe-project --json
# {"kind":"Monorepo","languages":["Rust","TypeScript"],
#  "probable_scope":["crates/api-core/src","crates/api-cli/src","services/web/src"]}
```

#### 探测启发式(纯文件存在性,不 parse 任何代码)

| 信号 | 推断 |
|---|---|
| `Cargo.toml` 内 `workspace.members` | Monorepo (Rust) |
| `package.json` 内 `workspaces` 或 `pnpm-workspace.yaml` | Monorepo (TS) |
| `go.work` | Monorepo (Go) |
| 单 `Cargo.toml` / `package.json` / `pyproject.toml` | SingleRepo |
| 只有 `*.md` + 无 source dir | DocsOnly |
| 只有 `*.sh` / `*.py` script | ScriptsOnly |

#### `probable_scope` 截断规则

monorepo 探测出 10+ crate 时,按 LOC 排序取 top-3(fallback alphabetical),避免 scope 默认值过宽。用户对 prompt 内直说"scope 改成 X"即可 override。

#### preset × probe 矩阵(sensible defaults)

| preset | probe = Monorepo | probe = SingleRepo | probe = DocsOnly |
|---|---|---|---|
| `bg-overnight` | scope = top-3 workspace members | scope = src/, tests/ | scope = docs/ |
| `inproc-team` | per-role scope 按 monorepo 子树分发 | per-role 同 scope | scope = docs/ |
| `chat-pocket` | bot mention scope = repo root | 同 | 同 |

不在范围:跨语言栈完整 template library(只针对当前 5 个 preset 的填充值);LLM-assisted 完整 role auto-gen。漏判时 user prompt override probe 结果。

## 三、5 preset 的 yaml diff(从 ccteam-creator 默认产物出发)

`ccteam-creator` 模板在 `crates/ccteam-core/src/templates/workflow_templates/{pocket,squad,sprint,builder,solo}.yaml`。

### Pocket → IM Squad(单 bot → 多 bot 群组)

```diff
 agents:
   - role: helpful-bot
+  - role: critic-bot
+    vendor: codex                    # auto-critic 路径自动设置(creator Phase 3.5)
+    bot_name: "@critic_newton"       # 自动从 agent_naming 池
 im_channels:
   - kind: telegram
+    chat_kind: group
```

### Pocket → Solo Sidekick(chat → in_proc)

```diff
-mode: chat
+mode: in_proc
 agents:
   - role: helpful-bot
-    bot_name: "@helpful_assistant"
-    hop_limit: 3
-im_channels: ...                     # 整段删
```

### Pocket → Overnight Builder(chat → bg)

```diff
-mode: chat
+mode: bg
 agents:
   - role: builder
     trigger: watch:.ccteam/inbox/
     parallelism: 3
+artifact_dirs: [.ccteam/inbox/, .ccteam/done/]
-im_channels: ...                     # 整段删
```

### 加 Codex critic(任意 preset)

```diff
 agents:
   - role: main
     vendor: claude
+  - role: critic
+    vendor: codex
+    model: o4-mini
+    trigger: watch:.ccteam/main-output/
```

`/ccteam doctor --check-codex` 验装 + auth;缺则报错,不静默 fallback。

## 四、`.claude/agents/<role>.md` frontmatter

```markdown
---
name: helpful-bot                    # 必,匹配 workflow.yaml::agents.role
description: 中文技术助手,Claude Code best practices 范围内答问
tools: [Read, Grep, Edit, Bash]      # Claude Code subagent 白名单;漏写 = 用不上
model: opus-4-7
vendor: claude                       # 与 workflow.yaml::vendor 必须一致(doctor warn)
tone: 礼貌、技术准、不啰嗦
guardrails:
  - 拒绝 rm -rf / curl 管道 sudo 类高危命令
---

# System Prompt 正文(LLM 自读)
你是一个中文技术助手 ...
```

**坑**:`vendor: codex` 时 `tools:` 字段静默 ignored(Codex 自有 tool surface),但仍要写以保 schema valid。

## 五、自定义 persona(从 prefab 改)

prefab 在 `skills/ccteam-creator/personas/<name>/`,目录结构:

```
personas/tech-assistant-zh/
├── agent.md.tmpl                    # .claude/agents/<role>.md 模板,{{role}} {{tone_overrides}} 插槽
├── meta.toml                        # { tags, supported_modes, recommended_vendor, language }
└── README.md
```

加新 prefab:

```bash
cp -r skills/ccteam-creator/personas/tech-assistant-zh \
      skills/ccteam-creator/personas/my-custom-bot
vim skills/ccteam-creator/personas/my-custom-bot/{agent.md.tmpl,meta.toml}
# 不需要改 Rust — skill 启动扫 personas/* 自动 pick
```

**用户自定义 persona 在 backlog** — 本节是 contributor 加内置 prefab 路径;用户写 markdown persona 是未来 feature。

## 六、Trigger 类型

| Trigger | 语义 | 并发 |
|---|---|---|
| `manual` | `mcp__ccteam__workflow_spawn_agent` / chat user 触发 | 强制 1 |
| `watch:<path>` | inotify(Linux) / fsevents(macOS),**项目相对路径** | `parallelism` 上限内并发 |
| `schedule:<cron>` | tokio_cron_scheduler;cron string(5 / 6 字段)| `parallelism` 上限内 |
| `webhook:<id>` | HTTP POST ingress(daemon 内 axum 路由 `/webhook/<id>`)| `parallelism` 上限内 |

## 七、Handoff 模板自定义

每 stage / fix-loop iteration 完,hook prompt 当前 agent 落 `.ccteam/handoffs/<workflow-slug>/stage-<N>-<role>.md`(10-30 行);后续 spawn prompt 自动 `{{include_prev_handoffs}}` 注入。

默认模板 `crates/ccteam-core/src/templates/handoff.md.tmpl`:

```markdown
# Stage {{stage_n}}: {{stage_name}}
**Decided**: {{#bullets}}
**Rejected**: {{#bullets}}
**Risks**: {{#bullets}}
**Files changed**: {{#files_with_why}}
**Remaining**: {{#bullets}}
```

自定义:仓内 fork 此模板,改 `crates/ccteam-core/src/handoff.rs::HANDOFF_TEMPLATE` 常量。**workflow.yaml 不暴露模板路径** — 全项目共享一个 template;per-project 不同走 PR 改源。

## 八、`ccteam new --no-interactive`(绕过 skill dialogue)

```bash
ccteam new my-bots \
  --no-interactive \
  --mode chat \
  --preset pocket \                  # { pocket | squad | sprint | builder | solo }
  --persona tech-assistant-zh \
  --vendor claude \
  --with-codex-critic \              # 等价 ccteam-creator Phase 2 自动 critic
  --im-platform telegram \
  --skip-im-setup                    # token 已落 ~/.ccteam/im/credentials.json

vim ~/projects/my-bots/.ccteam/workflow.yaml
ccteam start my-bots
```

**用途**:CI e2e probe / ansible / nix 自动化 / 反复迭代新 preset 时。**不**调 `/ccteam-im-setup`(token 必须已存);**不**走 mode-inferrer LLM 兜底(rule miss 直接报错)。

## 九、常见陷阱

1. **yaml 缩进** — list item 用 `-` + 2 空格;tab `serde_yaml` 直接 reject。
2. **`mode` 是 serde tagged enum** — 改 mode 必须**同时**改 mode-specific 字段。`mode: bg` 留 `bot_name`/`hop_limit` → 静默 ignore;`mode: chat` 漏 `im_channels` → schema error,daemon 不起。
3. **`vendor` 严格小写枚举** — `vendor: openai` ✗ / `vendor: anthropic` ✗;只 `{ claude | codex }`。
4. **`vendor` agent.md vs workflow.yaml 不一致** — doctor warn 不 error;运行时**按 workflow.yaml 走**,agent.md 字段静默 ignore。
5. **`watch:<path>` 是项目相对路径** — `watch:/abs/path` schema error。
6. **`tools:` 列 `mcp__ccteam__*` 无效** — 该工具组给 meta-agent;普通 bot 无 permission。反向 `CCTEAM_DISABLE_TOOLS` env(group enum)才是禁用入口。
7. **`max_spawn_depth: 0` ≠ 无限,是禁 spawn** — 想"无限"删字段走 default 5,或 `u32::MAX`。
8. **`compact_every_tokens` 不含 cached input** — cumulative input + output,**不算** cached(implementation detail)。
9. **Trait 接口契约不能突破** — yaml schema 只是 `HarnessAdapter` trait 的 surface 投影;trait 不支持的字段(eg. mode 1 in_proc 写 `im_channels`)加了也是死字段。改 yaml 不能让 `submit_turn` 跳过 `start_thread`、不能让 `events()` 无 `ThreadStarted`。

## 十、看实际跑的是什么

```bash
/ccteam-control show-workflow <slug>          # 推荐,渲染 yaml + 注释
cat ~/projects/<team>-<slug>/.ccteam/workflow.yaml
tail -f ~/projects/<team>-<slug>/.ccteam/progress.jsonl | jq 'select(.event=="agent_spawn") | {role, vendor, mode}'
```

任何 yaml 改动 daemon **不需重启**(热加载);下次 spawn 即生效。**已在跑的 thread 不受影响**(红线:永不主动 kill);要应用到现有 chat bot,显式 `/ccteam-control restart-bot <role>`。
