# Claude Code 工具触发面 — ccteam 的能力地图

> 本文档面向 **agent / persona 行为作者**(`.claude/agents/<role>.md`)和
> **集成 ccteam MCP 工具的 meta-agent 设计者**,解决一个核心问题:在 ccteam
> 路由到的 Claude / Codex session 里,**谁能触发什么命令、怎么触发**。
>
> 当前架构:`ccteam start` 是一个常驻的 **IM⇄session 路由网关 daemon**(不 tick、
> 不跑编排循环)。daemon 把 IM 消息路由到真实 session;执行层按 vendor 选最合适的
> harness ── **Claude = tmux TUI session**(`send-keys -l` 注入 + transcript 增量读 +
> `PreToolUse` hook);**Codex = app-server JSON-RPC**。MCP 表面对 Claude / Codex
> plugin 和自动化开放,可经 stdio(`ccteam internal mcp-serve`)或 daemon 的 Unix socket
> (`~/.ccteam/run/mcp.sock`,line-delimited JSON-RPC)接入。
>
> 不读这份文档的后果:role.md 里写"请用 `/review` 检查代码",运行时模型把
> `/review` 当死字符输出 → 不产生任何动作 → 行为静默失败。
>
> 多 agent 自治编排(`ccteam-flow`:ArtifactWatcher 文件系统控制平面、`claude --bg`
> 后台 spawn、workflow.yaml trigger 图、meta-agent `workflow_*` 循环)是**推后的编排层**。
> 本文遇到这层一律标注「推后的编排层」,与 interfaces.md §12.2 对 `workflow_*` 的口径一致。
> 文末《workflow.yaml trigger ↔ Claude Code 工具触发面》整理推后层的映射,供未来恢复时参考。

---

## 一图概要

```
┌──────────────────────────── Claude Code / Codex 的"命令" ─────────────────────┐
│                                                                              │
│   通道 1:prompt 内自调          通道 2:slash 命令          通道 3:meta-agent│
│   (模型自己发工具调用)          (键盘 / gateway 注入)      (MCP 工具)       │
│                                                                              │
│   • Agent / Task                  • /compact /clear /new    • chat_* 桥      │
│   • Skill                         • /review(Codex 映射)    • advise_* 第二意见│
│   • MCP tools                     • /memory /agents /help   • admin_* 改 persona│
│   • 内置 (Read/Edit/Bash/...)     • /exit                   • screenshot 只读截图│
│                                                             • workflow_*(推后层)│
│   ✅ role.md 直接编排             ✅ IM 用户 @handle 注入   ✅ MCP 工具表        │
│   ✅ cache 命中                   ✅ gateway send-keys/RPC  ✅ 状态落 progress  │
│   ✅ 便宜、可观测                 ❌ 模型自己摸不到                              │
└──────────────────────────────────────────────────────────────────────────────┘
```

**关键事实**:模型把 `/review` 字面写在回答里**完全没效果**。slash command
是 Claude Code TUI 的输入解析器(或 Codex 的 RPC 映射)拦截的,只接受
**人类键盘输入 / gateway 注入**,不接受模型输出的字符串。所以所有控制路径必须走
**通道 1**(role 自调工具)或**通道 3**(meta-agent / 自动化调 MCP 工具);
通道 2 的 slash 命令由 **IM 用户**(`@handle /compact`)经 gateway 投递,
**不**由模型自己发起。

---

## 通道 1 — prompt 内自调

### 1.1 `Task` / `Agent` 工具:启动 subagent

子 agent 是调"质量类"工作(代码审查、架构方案、并行探索)的首选 ──
但 **subagent 必须先在全局 / 项目级 agents 目录里注册或经 plugin pipeline 启用**,
模型才能通过 `Task(subagent_type=<name>, ...)` 调到。光把 plugin 装到 marketplace
**不够**(见 §1.1.2 实测)。

#### 1.1.1 内置 subagent — 经实测确认的 5 个

Claude Code 默认 always-on,**任何会话**都能直接 `Task(subagent_type=...)` 调:

| `subagent_type` | 用途 | 工具面 |
|---|---|---|
| `general-purpose` | 兜底:多步研究、跨文件搜索、复杂任务 | 全工具 |
| `Explore` | 只读快速搜索:`find`、`grep`、定义/引用查找 | 只读 |
| `Plan` | 软件架构师:产出实现计划 | 只读 + 计划工具 |
| `claude-code-guide` | 回答 Claude Code / Agent SDK / Anthropic API 用法 | Bash / Read / WebFetch / WebSearch |
| `statusline-setup` | 配置 status line | Read / Edit |

(在 Claude Code 长会话里跑 `Task(subagent_type="code-reviewer")` 得到
"Available agents: claude-code-guide, Explore, general-purpose, Plan,
statusline-setup" 实测确认。)

#### 1.1.2 plugin agent **不会**自动进 Task 注册表 —— 这是关键陷阱

虽然 `~/.claude/plugins/marketplaces/claude-plugins-official/plugins/<plugin>/agents/<name>.md`
路径下确实有 `code-reviewer` / `code-architect` / `code-simplifier` 等 agent
文件,**仅把 plugin marketplace `add` 进来之后,模型在长会话里调
`Task(subagent_type="code-reviewer")` 仍然会拿到**:

```
Error: Agent type 'code-reviewer' not found.
Available agents: claude-code-guide, Explore, general-purpose, Plan, statusline-setup
```

plugin 里的 agent 默认是**该 plugin 自己的 slash command body 内部使用的私有
agent**(`pr-review-toolkit/commands/review-pr.md` 里写 `allowed-tools: [Task, ...]`
然后由 plugin 的 prompt 引导 Task 去找对应 agent)── 它们只在那条 slash
command 触发后的特殊上下文里可调,**不进全局 Task subagent 注册表**。

#### 1.1.3 让 plugin agent 在长会话里可被自治调用 —— 走 `enabledPlugins`

当前唯一机制:在该 session 的 `<project>/.claude/settings.json` 写
`enabledPlugins`,Claude Code 启动时把列出的 plugin **自动加载进 in-memory
pipeline 并加 `<plugin>:` namespace 前缀**;role.md 用裸名 `Task(subagent_type=...)`
仍可调,pipeline 自匹配。**不再有「绕 Task 注册表的三路线 workaround」**:
要么 plugin 在 `enabledPlugins` 里(可自治调用),要么不在(只能由人 / IM 用户
经 slash command 触发该 plugin 自己的 command body)。

```jsonc
// <project>/.claude/settings.json
{"enabledPlugins": {"pr-review-toolkit@claude-plugins-official": true}}
```

> 当前 `ccteam init` 落地的 `<project>/.claude/settings.json` 写的是**空**
> `enabledPlugins` 集合;按 role 行为自动解析依赖并回填 `enabledPlugins` 的
> 编排层属**推后的编排层**,尚未在当前 daemon 路径接通。需要 plugin agent 时,
> 手工在 settings.json 写上对应条目即可。

#### 1.1.4 实测当前会话可用 subagent

```
请用 Task 工具调一个不存在的 subagent_type(如 "probe-test-12345"),
看返回的 "Available agents: ..." 错误清单。
```

---

### 1.2 `Skill` 工具:激活 skill

Skill 是 Claude Code 的"可调用知识包" —— 适合"按规程办事"的场景。

```
Skill(skill="<name>", args="<optional-args>")
```

#### 1.2.1 关键约束:只调 system-reminder 里列出来的 skill

Claude Code 会在每次会话里通过 system-reminder 列当前可用 skill 名单。
**不在那张清单里的名字一律不能调 ──** `Skill(skill="X")` 会以
InputValidationError 失败。不要凭训练记忆或文件路径猜 skill 名字。

#### 1.2.2 怎么实测当前会话能调哪些 skill

最稳的探针:

```
请用 Skill 工具调用一个不存在的 skill 名(如 "probe-test-12345"),
看返回的错误信息或用户可见 skills 列表,确认当前会话真实可调的 skill。
```

或:

```
请用 Bash 工具跑 `ls ~/.claude/skills/ ~/.claude/plugins/marketplaces/*/plugins/*/skills/`,
列出所有 skill 文件 —— **这只是文件存在性,不代表 Skill 工具可调**;
最终判定还是看 system-reminder 那张清单。
```

#### 1.2.3 plugin 的 `commands/<name>.md` 不是 Skill,是 slash command

**非常容易踩**:`pr-review-toolkit/commands/review-pr.md` 不是 skill,
是 slash command;`Skill(skill="review-pr")` 会报错。slash command
属于通道 2,模型自己摸不到,要触发只能由人 / IM 用户经 gateway 投递。
**这是 plugin agent 在自治路径里难用的真正原因**。

#### 1.2.4 Skill 支持中途热加载,但 agent 不热加载

| 项 | 热加载? | 含义 |
|---|---|---|
| `~/.claude/skills/`、`<project>/.claude/skills/` 下的 SKILL.md | ✅ 实时监听([官方文档](https://code.claude.com/docs/en/skills.md#live-change-detection)) | 新增 / 修改 / 删除立即生效;可按 role 懒注入 skill |
| `~/.claude/agents/<name>.md`、`<project>/.claude/agents/<role>.md` | ❌ 会话启动时一次性扫描 | 必须 session 启动前就位;改了 persona 要下次 turn / 新 session 才生效 |
| Plugin 文件 | ❌ 中途不能装 | 已装 plugin 可用 `/reload-plugins` 不丢 context 刷新(仅 Claude tmux session)|

**预创建空目录的坑**:会话启动时若 skills 顶层目录(`~/.claude/skills/`、
`<project>/.claude/skills/`)不存在,后来创建不会被监听 → 需要重启或
`/reload-plugins`。`ccteam init` 已预留 `.ccteam/skills/.gitkeep` 供项目自有
skill 扩展。

#### 1.2.5 `Task` ≠ `TaskCreate` — 容易踩的命名坑

`Task` / `Agent`(参数 `subagent_type` / `description` / `prompt`)启动 subagent 跑任务;`TaskCreate`(参数 `subject` / `description` / `activeForm`)在任务管理列表里创建 todo,**完全不同的工具**。role.md 写 prompt 时明确说 "用 Task 工具(Agent 调度工具,不是 TaskCreate 任务管理工具),传 subagent_type=..."。

---

### 1.3 MCP 工具

MCP server 注册的 tool 在模型看来就是普通工具,工具名形如
`mcp__<server>__<tool>`。

#### 1.3.1 ccteam 项目相关 MCP server

| Server | 角色 | 主要 tool 命名 |
|---|---|---|
| `ccteam`(自建) | gateway / 自动化接口 | `mcp__ccteam__{workflow_,chat_,advise_,admin_,screenshot}*`(详 §1.3.2)|
| Telegram bot | IM 出站 | `mcp__telegram__send_message` 等 |
| `claude-mem` | 记忆检索(可选,LLM 自看 surface 决定调否)| `mcp__plugin_claude-mem_mcp-search__search` 等 |
| Playwright | 浏览器自动化(按需)| `mcp__plugin_playwright_playwright__browser_*` |
| GitHub | 代码托管 | 优先 `gh` CLI(见最佳实践 §4.3) |

#### 1.3.2 `ccteam` MCP 工具表面 —— **5 group,以 `ccteam doctor --verify-mcp` 为准**

`ccteam` server 暴露的工具按 5 个 group 子前缀分组:
`mcp__ccteam__{workflow_,chat_,advise_,admin_,screenshot}*`。
**活跃工具数随实现演进,不在本文硬编码 ── 以
`ccteam doctor --verify-mcp` 的自检为权威**(它做 stub-counter parity,
有 stub 漂移则 exit 1),完整 tool 清单 + 入参 / 返回 schema 见 **interfaces.md §12.2**。

| Group | 当前 / 推后 | 用途 | 高频 tool |
|---|---|---|---|
| `chat_` | 当前 IM 路由路径活跃 | 注册 / 注销 bot、给 bot mailbox 发消息、查对话历史、重置 chat session | `chat_register_bot` / `chat_send_input` / `chat_history` / `chat_list_bots` / `chat_reset` |
| `advise_` | 当前活跃 | Claude + Codex 并行第二意见(投票汇总 / N-of-N 平行)| `advise_vote` / `advise_parallel` |
| `admin_` | 当前活跃 | 改 persona、给 agent 加工具、列管理状态(daemon 只做文件 mutate,不调 LLM)| `admin_change_persona` / `admin_add_tool` / `admin_ls` |
| `screenshot` | 当前活跃(只读)| `tmux capture-pane` → vt100 → png 渲染;失败永不阻塞主路径 | `screenshot` |
| `workflow_` | **推后的编排层** | 写文件系统 marker 桶(spawn / stop agent、signal、set_parallelism、trigger_gate 等),服务尚未接通的 `ccteam-flow` 自治编排 | `workflow_show` / `workflow_spawn_agent` / `workflow_trigger_gate` / `workflow_observe_agents` |

> `workflow_*` 工具**当前会写出 marker 文件**,但消费这些 marker 的编排循环属
> 推后的编排层 ── 调用它们不会触发当前 gateway 自治起 agent。当前 IM 路由的
> 入站走 IM gateway(`/pair`、`/cd`、`/new`、`@handle`,详 user-manual)与
> chat-mode bot mailbox(`chat_send_input`)。

`CCTEAM_DISABLE_TOOLS` 按 group enum(非 glob,防 typo)关组,
eg `CCTEAM_DISABLE_TOOLS=advise,chat`。

#### 1.3.3 验证示例(假设已装 Playwright MCP)

```
请用 mcp__plugin_playwright_playwright__browser_navigate 工具
打开 https://example.com,然后用 mcp__..._browser_snapshot 截屏。
```

---

### 1.4 内置工具

`Bash` / `Read` / `Edit` / `Write` / `Grep` / `Glob` / `WebFetch` /
`WebSearch` / `TodoWrite` / `NotebookEdit` 等。role.md 里通常
不需要显式说"用 Bash 工具"——模型会自己挑。**但有几个场景值得显式约束**:

- 让模型用 `Bash("gh pr create ...")` 而不是手搓 PR 模板 → 让 GitHub
  CLI 兜底
- 让模型用 `Read` 而不是 `Bash("cat ...")` → 用对的工具
- 让模型用 `Grep` 而不是 `Bash("grep ...")` → 速度更快

---

## 通道 2 — slash 命令(由人 / IM 用户经 gateway 投递)

下面这些 slash command **模型自己完全摸不到**——把字面字符串 `/compact` 写进
回答里,Claude Code 不会把它解析回命令,只会显示成普通文本。**role.md
里写这些让模型"自己执行" = 静默失败**。

**先区分两类 slash**(很多人在这里混淆):

1. **gateway-router 命令** ── `/pair`、`/cd`、`/new`、`/use`、`/sessions`、
   `/projects`。这些**由 gateway 自己拦截**(不投递进任何 session):管 chat /
   project / session 的生命周期与切换。`/new claude <handle>` /
   `/new codex <handle>` **创建**一个新 session(带 vendor + handle 参数),
   **不是**把某个 session 的 context 清掉。详见 user-manual §1。
2. **转发进 session 的 slash** ── `@handle /compact` 这类,gateway 才会投递进
   目标 session,按 vendor 选投递方式。本节其余部分讲的是这一类。

转发类 slash 的**真实投递路径**是:IM 用户发 `@handle /compact`,gateway 按目标
session 的 vendor 投递 ──

- **Claude(tmux TUI session)**:gateway `send-keys -l` 把 slash **按字面透传送进
  tmux pane** 再发 Enter,**不过滤、不改写**(ccteam 不认识 team 私有 slash)。
  `/compact`、`/clear`、`/new` 是合法 turn 控制,不是 kill。
- **Codex(app-server)**:Codex **没有 slash-command surface**;gateway 只把已知
  的两条**映射成 JSON-RPC** ── `/compact` → `thread/compact/start`,
  `/review` → `review/start`;其余 slash 走普通 `turn/start` 当文本提交。

| 转发类命令 | 用途 | Claude(tmux)| Codex(app-server)|
|---|---|---|---|
| `/compact` | 压缩历史 | send-keys 字面透传 | `thread/compact/start` |
| `/review` | 触发审查 | send-keys 字面透传 | `review/start` |
| `/clear` | 清空当前 context(不退会话)| send-keys 字面透传 | 无原生映射 ── 当文本提交 |
| `/memory` | 编辑 CLAUDE.md | send-keys 字面透传 | 无原生映射 |
| `/agents` | 查看可用 subagent 列表 | send-keys 字面透传 | 无原生映射 |
| `/reload-plugins` | 重载 plugin 配置(装/卸 plugin 后)| send-keys 字面透传 | 无原生映射 |
| `/exit` | 退出当前会话 | send-keys 字面透传 | 无原生映射 |
| `/help` | 帮助 | send-keys 字面透传 | 无原生映射 |

> 注:除 `/compact`、`/review` 外,Codex 适配器对 slash 无原生 RPC 映射,会把
> 整串当普通用户文本经 `turn/start` 提交(`codex has no slash-command surface`)。
> Claude 侧则一律字面透传,由 Claude Code TUI 自己解析。

### 2.1 gateway 怎么投递

Claude session 的 slash 投递走 tmux `send-keys -l`(把字面字符串送进目标
pane 再发 Enter,透明透传)。Codex session 走 app-server JSON-RPC(仅
`/compact`、`/review` 有原生映射)。两条路都由 **IM 入站消息**驱动
(`@handle <text>` / `@handle /<command>`),由 gateway 根据 session 的 vendor
选投递方式。**这不是编排器在 tick 里自动发,而是 IM 用户的显式指令。**

### 2.2 role.md / persona 不应让模型"自己发" slash 命令

模型无法自己投递通道 2 命令。persona(`.claude/agents/<role>.md`)如需协调,
应通过**调工具**(通道 1)或让 IM 用户 / meta-agent 经通道 3 介入。需要
context reset / compact 时,这是 **IM 用户**的合法 turn 控制(`@handle /compact`),
或 meta-agent 经 `chat_reset` MCP 工具触发,**不是** persona 内部能发起的动作。

```
✅ IM 用户:@reviewer /compact     ── 合法 turn 控制,gateway 投递
✅ meta-agent:调 chat_reset(workflow_slug, role)  ── 经 MCP 重置 chat session
❌ persona.md 写:"context 高位时请自己执行 /compact"  ── 模型摸不到 slash,静默失败
❌ persona.md 写:"请 send-keys /reload-plugins"      ── persona 不指挥 gateway
```

---

## 通道 3 — meta-agent(MCP 工具)

通道 1 让 session 内的模型做工具决策——但用户 / 运维需要一个**跨 project /
跨 session 的对话面**:看进度、改 persona、起 / 重置 bot、要第二意见、截图。
这是 **meta-agent**(装 `ccteam` MCP server 的 Claude / Codex session,
通常经 `ccteam-control` skill 驱动)的职责。

完整工具清单 + schema 见 **interfaces.md §12.2**;工具数以
`ccteam doctor --verify-mcp` 为权威。当前活跃的高频工具:

- `chat_register_bot` / `chat_unregister_bot` / `chat_list_bots`:管 bot 生命周期
- `chat_send_input`:写 router-shaped envelope 进 bot mailbox(**不**向 tmux pane
  注入 system prompt ── CLAUDE.md §三 红线)
- `chat_history`:tail `<project>/.ccteam/chat/<role>/turns.jsonl`
- `chat_reset`:重置某 bot 的 chat session(archive turns + clear cursor)
- `advise_vote` / `advise_parallel`:Claude + Codex 第二意见
- `admin_change_persona` / `admin_add_tool` / `admin_ls`:管理类
- `screenshot`:只读终端截图

约束:meta-agent 操作落 progress.jsonl 业务事件(`persona_changed` /
`tool_added` 等);**不持有 session 状态**。`workflow_*` group 的工具
(spawn / stop / signal / gate)虽可调,但消费它们的编排循环属**推后的编排层**。

### 3.1 与通道 1、2 的边界

| 决策类型 | 谁来 | 前置条件 |
|---|---|---|
| session 内调 code-reviewer / code-simplifier 等 subagent | 通道 1(session 内模型自决)| session `enabledPlugins` 启用对应 plugin(见 §1.1.3)|
| chat / project / session 生命周期(`/pair` `/cd` `/new` `/use` `/sessions`)| gateway-router 命令(gateway 自拦截,不进 session)| chat 已 `/pair` |
| `/compact` / `/clear` / `/review` 等转发类 turn 控制 | 通道 2(IM 用户 `@handle /<cmd>`,gateway 转发进 session)| 目标 session 已存在 |
| 改 persona、重置 bot、要第二意见、截图、管理状态 | 通道 3(meta-agent + MCP 工具)| meta-agent 装 `ccteam` MCP server |
| 自治起 / 停 agent、调 parallelism、解锁 gate | **推后的编排层**(`workflow_*` marker → `ccteam-flow`)| 编排循环接通后 |

### 3.2 不做什么

- ❌ 不替 session 内的模型做工具选择(那是通道 1 的职责)
- ❌ 不向 tmux pane 注入 system prompt(persona 住 `.claude/agents/<role>.md`)
- ❌ 不持有会话状态;对话原文 SoT 是 `turns.jsonl`,业务事件 SoT 是 `progress.jsonl`

---

## 工具清单 — persona / 自动化作者参考

### 6.1 默认可用的 subagent(Task 直接可调)

| `subagent_type` | 来源 | 适用场景 |
|---|---|---|
| `general-purpose` | Claude Code 内置 | 兜底,任何复杂任务 |
| `Explore` | Claude Code 内置 | 只读快速搜索 |
| `Plan` | Claude Code 内置 | 实现计划设计 |
| `claude-code-guide` | Claude Code 内置 | Claude Code 用法咨询 |
| `statusline-setup` | Claude Code 内置 | 配置 status line |

### 6.2 想做 plugin 级别 review / simplify / 架构方案 — 用 `enabledPlugins`

`pr-review-toolkit` / `feature-dev` / `code-simplifier` 等 plugin 里的 agent
**装了 plugin 也不能直接 Task 调**(见 §1.1.2),除非该 session 的
`<project>/.claude/settings.json` 把对应 plugin 列进 `enabledPlugins`:

```jsonc
{"enabledPlugins": {"pr-review-toolkit@claude-plugins-official": true, "feature-dev@claude-plugins-official": true, "code-simplifier@claude-plugins-official": true}}
```

session 启动时 Claude Code 自动加载 enabled plugin,**namespace 加 `<plugin>:`
前缀**(eg `pr-review-toolkit:code-reviewer`);role.md 用裸名
`Task(subagent_type="code-reviewer")` 仍可调,pipeline 自匹配。

| 来源 plugin | agent 文件 → subagent_type | 典型用例 |
|---|---|---|
| `feature-dev` | `code-architect` / `code-explorer` | 方案设计 / 代码探索 role |
| `pr-review-toolkit` | `code-reviewer` / `silent-failure-hunter` / `pr-test-analyzer` / `type-design-analyzer` / `comment-analyzer` | 实现 / 审查 role |
| `code-simplifier` | `code-simplifier` | 收尾 / 简化 role |

**用户需先把上游 plugin marketplace `add` 进来**:
`claude /plugin marketplace add claude-plugins-official`(只一次,user level)。
**关键约束**:`enabledPlugins` 必须在 session 启动时已写好(改了要重启 session
或 `/reload-plugins`)。当前 `ccteam init` 落地空 `enabledPlugins`;按 role
自动解析回填属推后的编排层,需手工写。

### 6.3 推荐挂的 hook + MCP server

完整 hook 表(progress.jsonl 写入、cost 累计、`PreToolUse` 批准门占位等)详见
**interfaces.md §6**。完整 MCP server 注册 + 工具清单详见 **interfaces.md §12**。
本文不重复维护。

### 6.4 persona / role.md 引用语法速查

| 目的 | 写法 |
|---|---|
| 引用某个文件让模型读 | `@spec.md` / `@<abs-path>` |
| 引用 plugin 里某个 agent 文件让模型按里面规程办 | `@~/.claude/plugins/marketplaces/claude-plugins-official/plugins/feature-dev/agents/code-architect.md` |
| 让模型主动 launch 内置 subagent | "请使用 Task 工具,subagent_type='general-purpose'/'Explore'/'Plan'/'claude-code-guide'/'statusline-setup',..." |
| 让模型主动 launch plugin subagent(必须先 §6.2 写 `enabledPlugins`)| "请使用 Task 工具,subagent_type='code-reviewer',..." |
| 模型按 plugin agent 规程办但不显式调 subagent | "请读 `@~/.claude/plugins/.../agents/code-reviewer.md`,严格按其指引 review 当前 diff" |
| 让模型用 skill | "请使用 Skill 工具调用 <name> skill" |
| 让模型用 MCP tool | "请使用 mcp__\<server>__\<tool> 工具,..." |

### 6.5 怎么发现新工具

人工开发时**不要凭训练记忆猜工具名**。在长会话里检查可用工具:

- 看每个 system-reminder 块开头的 available-skills 列表
- 调试期 `Task(subagent_type="general-purpose", prompt="列出会话里所有 mcp__ 开头工具 + 所有 subagent_type")` 把返回回填本文档 §6
- `ccteam doctor` 汇报当前可见的 plugin / agent / MCP server
- `ccteam doctor --verify-mcp` 汇报 `ccteam` MCP 工具表面(active / stub / 分组计数)

---

## 附:常见误用与对策

| 现象 | 根因 | 对策 |
|---|---|---|
| persona 写 "请 `/review`",模型却没有动作 | 模型摸不到 slash command | 改为 "请用 Task 工具调 code-reviewer subagent" + §6.2 写 `enabledPlugins`;或由 IM 用户 `@handle /review` 经 gateway 投递 |
| `Task(subagent_type="code-reviewer")` 报 "Agent type not found, Available: general-purpose, Explore, Plan, claude-code-guide, statusline-setup" | **装了 plugin 不等于 Task 能调它的 agent**(session 没启用 plugin pipeline)| 该 session 的 `<project>/.claude/settings.json` 加 `enabledPlugins: {"<plugin>@<mkt>": true}` |
| `Skill(skill="review-pr")` 报 InputValidationError | `review-pr` 是 plugin 的 slash command 不是 Skill;`commands/<name>.md` 文件不被 Skill 工具识别 | persona 别让模型自己调;由 IM 用户经 gateway send-keys |
| `Skill(skill="X")` 报 InputValidationError | skill 名字写错 / 当前会话没加载到 system-reminder 列表 | §1.2.2 的探针实测当前可调 skill |
| `mcp__foo__bar` 报工具不存在 | MCP server 没连 | 检查项目 `.mcp.json` + `ccteam doctor` |
| 模型在回答里写了 `/compact` 但会话没压缩 | 模型摸不到 slash 命令 | 由 IM 用户 `@handle /compact`(gateway 投递)或 meta-agent 调 `chat_reset` |

---

## workflow.yaml trigger ↔ Claude Code 工具触发面(推后的编排层参考)

> 本节描述**推后的编排层**(`ccteam-flow`)── ArtifactWatcher 文件系统控制平面 +
> workflow.yaml trigger 图。当前 gateway daemon 不跑这层;保留此节供未来恢复
> `ccteam-flow` 时参考,口径与 interfaces.md §3 对推后层的标注一致。

### workflow mode 术语

workflow.yaml 的 `mode:` 字段(kebab-case,默认 `artifact-driven`):

| `mode:` | 含义 |
|---|---|
| `artifact-driven` | 默认 ── ArtifactWatcher + trigger 图驱动多 role 协作 |
| `chat` | 长跑 chat bot session(tmux / app-server),IM 用户逐 turn 对话 |
| `human-approval` | artifact-driven roster + 每个 `agent_done` 后的人在环 gate |
| `agent-team` | 单个 ccteam-managed lead session(Anthropic Agent Teams)|

**对用户**:不要再用"mode 1 / 2 / 3"这种序号说法,直接用 kebab-case 名字。

### 三种 trigger 的工具触发后果(推后层)

| trigger 类型 | 触发条件 | spawn 时注入 env | role.md 应该用的工具 |
|---|---|---|---|
| `watch:<path>` | inotify 检测到 `<path>` 下新文件写入完成(`IN_CLOSE_WRITE`,debounce) | `CCTEAM_INPUT=<path>`、`CCTEAM_OUTPUT=<output>` | 通常 `Read $CCTEAM_INPUT/<file>` + 处理 + `Write $CCTEAM_OUTPUT/<artifact>` |
| `schedule` | cron 触发(`AgentSpec::schedule` 5 段 cron,`croner` crate),也可被 `workflow_spawn_agent` 显式主触发 | `CCTEAM_OUTPUT=<output>` | 自主任务(crawler / monitor 类),用 `Bash` / `WebFetch` 拉数据,`Write` 出 artifact |
| `gate` | 调 `mcp__ccteam__workflow_trigger_gate(role, slug)` 后才起 | `CCTEAM_OUTPUT` + `CCTEAM_INPUT`(若声明)| 通常做"最终把关"(ship / publish 类),`Bash` 调外部命令(`gh pr create` / `npm publish` 等)|

### spawn 时的 env 注入(推后层)

```
CCTEAM_PROJECT_SLUG=<team>-<slug>
CCTEAM_INPUT=<project_root>/<workflow.yaml::agents.<role>.input>
CCTEAM_OUTPUT=<project_root>/<workflow.yaml::agents.<role>.output>
CCTEAM_JOB_ID=<uuid>
CCTEAM_ROLE=<role>
```

role.md 里**直接引用** `$CCTEAM_INPUT` / `$CCTEAM_OUTPUT` 比硬编码路径好。

### artifact 通信(推后层 role 之间的"消息")

红线:推后层 role 之间**只通过文件系统 artifact 通信**,不用 MCP 直接 RPC。

- upstream role `Write $CCTEAM_OUTPUT/<filename>` → 关文件
- ArtifactWatcher 发 `ArtifactEvent` → 检查 `parallelism` → spawn downstream role
- downstream role 启动时拿到 `CCTEAM_INPUT=<刚才那个 output 目录>`,`Read` /
  `Glob` 自己消费

这也是为什么 role.md 应该用 `Read` / `Glob` / `Write` 这几个内置工具操作
artifact —— 这些工具直接和 Claude Code prompt cache 友好,且 ArtifactWatcher
依赖文件系统事件,不依赖 MCP。

### 与通道 1/2/3 的对应(推后层)

| workflow.yaml 概念 | 对应工具触发面 |
|---|---|
| `agents.<role>.vendor: claude`(推后层 spawn)| `claude --bg --agent <role>`(无 TUI);**通道 1** 全开(role 可调 Task / Skill / MCP / 内置)|
| `agents.<role>.vendor: codex`(推后层 spawn)| `codex exec` / app-server;**通道 1** 部分开(看 Codex 支持哪些工具)|
| `agents.<role>.trigger: gate` | 由 `mcp__ccteam__workflow_trigger_gate` 解锁后才起(marker → 推后层消费)|
| role 产 artifact → 下游 `trigger: watch:*` | ArtifactWatcher deterministic,**不经任何 LLM 决策** |
| 顶层 `squad: { leader, members, hop_limit }` + leader 写 `<member>--*.md` 进 `.ccteam/squad/` | ArtifactWatcher 按文件名前缀 dispatch 到 `members:` 声明的 role;超 `hop_limit` 发 `escalation` 事件 `kind: squad_hop_limit`,未知 member 前缀发 `squad_unknown_target` |
| escalation event 落 progress.jsonl | meta-agent 用 `workflow_observe_agents` / `workflow_progress` 读到自决策 |

详 `docs/interfaces.md` workflow.yaml schema(§3 标注推后的编排层)。
