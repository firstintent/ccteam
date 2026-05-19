# Claude Code 工具触发面 — ccteam 自治编排的能力地图

> 本文档面向 **`workflow.yaml` / `.claude/agents/<role>.md` 作者**和 **meta-agent / orchestrator 设计者**,
> 解决一个核心问题:在 ccteam 编排的 Claude Code session 里,**谁能触发什么命令、怎么触发**。
>
> 不读这份文档的后果:role.md 里写"请用 `/review` 检查代码",运行时模型把 `/review` 当死字符输出 → orchestrator 拿不到 review report → 整条 workflow 静默失败。
>
> Claude Code 工具触发面与 ccteam workflow.yaml 的映射详见文末《workflow.yaml trigger ↔ Claude Code 工具触发面》。

---

## 一图概要

```
┌──────────────────────────── Claude Code 的"命令" ────────────────────────────┐
│                                                                              │
│   通道 1:prompt 内自调          通道 2:TUI-only            通道 3:meta-agent│
│   (模型自己发工具调用)          (键盘 / send-keys 输入)    (V0.4.0+)        │
│                                                                              │
│   • Agent / Task                  • /exit                   • workflow 编排  │
│   • Skill                         • /clear                  • spawn/stop_agent│
│   • MCP tools                     • /compact                • signal / gate  │
│   • 内置 (Read/Edit/Bash/...)     • /reload-plugins         • set_parallelism│
│                                   • /agents                                  │
│                                   • /help                                    │
│                                   • /memory                                  │
│                                   • /btw(meta-agent → signal) │             │
│   ✅ role.md 直接编排             ❌ 模型摸不到             ✅ 17 个 MCP 工具│
│   ✅ cache 命中                   ✅ orchestrator send-keys ✅ 事件驱动      │
│   ✅ 便宜、可观测                                                            │
└──────────────────────────────────────────────────────────────────────────────┘
```

**关键事实**:模型把 `/review` 字面写在回答里**完全没效果**。slash command
是 Claude Code TUI 的输入解析器拦截的,只接受**人类键盘输入或 tmux send-keys**,
不接受模型输出的字符串。V0.4.0+ 里 `claude --bg --agent <role>` 是无 TUI
的后台 session,**通道 2 的 slash command 在 bg session 上下文里不存在**——
所有控制路径必须走通道 1(role 自调工具)或通道 3(meta-agent 调 MCP 工具)。

---

## 通道 1 — prompt 内自调

### 1.1 `Task` / `Agent` 工具:启动 subagent

子 agent 是 ccteam 调"质量类"工作(代码审查、架构方案、并行探索)
的首选——但 **subagent 必须先在全局 / 项目级 agents 目录里注册**,
模型才能通过 `Task(subagent_type=<name>, ...)` 调到。光装 plugin
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

(2026-05-05 在 Claude Code 长会话里跑 `Task(subagent_type="code-reviewer")`
得到 "Available agents: claude-code-guide, Explore, general-purpose, Plan,
statusline-setup" 实测确认。)

#### 1.1.2 plugin agent **不会**自动进 Task 注册表 —— 这是关键陷阱

虽然 `~/.claude/plugins/marketplaces/claude-plugins-official/plugins/<plugin>/agents/<name>.md`
路径下确实有 `code-reviewer` / `code-architect` / `code-simplifier` 等 agent
文件,**`/plugin install <plugin>@claude-plugins-official` 之后,模型在长会话
里调 `Task(subagent_type="code-reviewer")` 仍然会拿到**:

```
Error: Agent type 'code-reviewer' not found.
Available agents: claude-code-guide, Explore, general-purpose, Plan, statusline-setup
```

plugin 里的 agent 是**该 plugin 自己的 slash command body 内部使用的私有
agent**(`pr-review-toolkit/commands/review-pr.md` 里写 `allowed-tools: [Task, ...]`
然后由 plugin 的 prompt 引导 Task 去找对应 agent)—— 它们只在那条 slash
command 触发后的特殊上下文里可调,**不进全局 Task subagent 注册表**。

#### 1.1.3 想让 plugin agent 在 ccteam 长会话里被自治调用 —— 三条路

| 方案 | 怎么做 | 评价 |
|---|---|---|
| A. 启用 in-memory plugin pipeline | spawned session 的 `<project>/.claude/settings.json` 写 `enabledPlugins: {"<plugin>@<mkt>": true}`,Claude Code 自动 namespace `<plugin>:<name>`,role.md 用裸名仍可调 | ✅ ccteam 选定路径;`bootstrap_project` 自动据 workflow.yaml + role.md 写 |
| B. `@文件引用` + general-purpose 执行 | role.md 里写 "请用 Task(subagent_type='general-purpose'),把 `@~/.claude/plugins/.../agents/code-reviewer.md` 当 system prompt" | ✅ 零安装兜底,但损失 plugin agent 的 model / color / tools 配置 |
| C. orchestrator send-keys 触发 slash command | tmux send-keys `/review-pr <args>`,plugin 自己的 prompt body 在 TUI 上下文里能调它的私有 agent | ⚠️ 只在 Codex executor(tmux)适用 |

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
**不在那张清单里的名字一律不能调 ——** `Skill(skill="X")` 会以
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
属于通道 2(TUI-only),要触发只能 orchestrator send-keys。**这是 plugin
agent 在自治流水线里难用的真正原因**。

#### 1.2.4 Skill 支持中途热加载,但 agent 不热加载

| 项 | 热加载? | 含义 |
|---|---|---|
| `~/.claude/skills/`、`<project>/.claude/skills/` 下的 SKILL.md | ✅ 实时监听([官方文档](https://code.claude.com/docs/en/skills.md#live-change-detection)) | 新增 / 修改 / 删除立即生效;ccteam 可按 role 懒注入 skill |
| `~/.claude/agents/<name>.md` | ❌ 会话启动时一次性扫描 | 必须 startup 前就位;ccteam 走 `enabledPlugins` 路径,不再 ln -sf |
| Plugin 文件 | ❌ 中途不能装 | 已装 plugin 可用 `/reload-plugins` 不丢 context 刷新(仅 Codex tmux session)|

**Bootstrap 必须预创建空目录**(`~/.claude/skills/`、`<project>/.claude/skills/`):会话启动时若 skills 顶层目录不存在,后来创建不会被监听 → 需要重启或 `/reload-plugins`。

**V0.4.0+ 优势**:每个 agent role 是独立短命 session——新增 role 只需写新 `.claude/agents/<role>.md` + 更新 workflow.yaml,orchestrator hot-reload 检测后下一次 trigger 即生效,无需重启。

#### 1.2.5 bootstrap_project 执行顺序

```
ccteam new <brief>
  ├─ 1. 创建 ~/projects/<team>-<slug>/ 目录与子目录
  ├─ 2. 写 workflow.yaml / .claude/agents/<role>.md / CLAUDE.md / settings.json (含 enabledPlugins)
  ├─ 3. 写 ~/.claude.json 的 hasTrustDialogAccepted
  ├─ 4. mkdir -p ~/.claude/skills/ <project>/.claude/skills/(占位让监听挂上)
  └─ 5. orchestrator 启动 ArtifactWatcher,等 trigger 满足时 spawn `claude --bg --agent <role>`
```

第 4 步**必须**在第 5 步之前。

#### 1.2.6 `Task` ≠ `TaskCreate` — 容易踩的命名坑

`Task` / `Agent`(参数 `subagent_type` / `description` / `prompt`)启动 subagent 跑任务;`TaskCreate`(参数 `subject` / `description` / `activeForm`)在任务管理列表里创建 todo,**完全不同的工具**。role.md 写 prompt 时明确说 "用 Task 工具(Agent 调度工具,不是 TaskCreate 任务管理工具),传 subagent_type=..."。

---

### 1.3 MCP 工具

MCP server 注册的 tool 在模型看来就是普通工具,工具名形如
`mcp__<server>__<tool>`。

#### 1.3.1 ccteam 项目相关 MCP server(见 tech-design §6.4)

| Server | 里程碑 | 主要 tool 命名 |
|---|---|---|
| Telegram bot | M1 | `mcp__telegram__send_message` 等 |
| `claude-mem` | M3 | `mcp__plugin_claude-mem_mcp-search__search` 等 |
| Playwright | 按需 | `mcp__plugin_playwright_playwright__browser_*` |
| GitHub | M4+ | 优先 `gh` CLI(见最佳实践 §4.3) |
| `ccteam-mcp`(自建) | M2 / V0.4.0 | **17 个工具** `mcp__ccteam__admin_ls` / `show` / `new` / `spawn_agent` / `stop_agent` / `observe_agents` / `signal` / `set_parallelism` / `trigger_gate` / `get_artifact_summary` 等(见 `docs/versions/v0-4-0/prd.md §6.3` 完整清单) |

#### 1.3.2 验证示例(假设已装 Playwright MCP)

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

## 通道 2 — TUI-only(只能 orchestrator send-keys)

下面这些 slash command **模型完全摸不到**——把字面字符串 `/exit` 写进
回答里,Claude Code 不会把它解析回命令,只会显示成普通文本。**role.md
里写这些 = 静默失败**。

**V0.4.0+ 适用范围限制**:Claude bg agent(`claude --bg --agent <role>`)
**没有 TUI**——通道 2 在这种 session 上下文里完全不存在。通道 2 仅适用于
**Codex executor**(tmux 容器)+ meta-agent / 调试用 long-running session。

| 命令 | 用途 | 谁该触发 |
|---|---|---|
| `/exit` | 退出当前会话 | orchestrator(context reset 时) |
| `/clear` | 清空当前 turn 的 context(不退出会话) | orchestrator(context 高位时) |
| `/compact` | 压缩历史 | orchestrator(可选,M0 用 `/exit` 替代) |
| `/btw <text>` | 忙时排队下一条消息 | orchestrator(idle-aware 注入,详 tech-design §6.9) |
| `/reload-plugins` | 重载 plugin 配置 | orchestrator(装/卸 plugin 后) |
| `/agents` | 查看可用 subagent 列表 | 人(调试用) |
| `/memory` | 编辑 CLAUDE.md | 人 / orchestrator |
| `/help` | 帮助 | 人 |

### 2.1 orchestrator 怎么触发

`crates/ccteam-core/src/tmux.rs` 的 `send_keys()`——把字面字符串送进 tmux 第一个 pane 再发 Enter。仅 Codex executor 容器适用(运维注入 + 调试)。**Claude agent 不走 tmux**:`claude --bg --agent <role>` 由 supervisor 接管,生命周期通过 `~/.claude/jobs/<job_id>/state.json` + 文件系统 artifact 控制。

### 2.2 role.md 不应请求通道 2 命令

`claude --bg --agent` 没 TUI,role 也无"phase prompt 注入"这条路径。role 如需协调下一步,通过**产 artifact 文件**让下游 role 的 `trigger: watch:<dir>` 触发,或**写 escalation event** 到 progress.jsonl 让 meta-agent 决策。

| 谁触发通道 2 命令 | 怎么触发 |
|---|---|
| **orchestrator deterministic 监控** | 读 progress.jsonl 跨阈值采取行动(cost > $200 → 硬终止;通过 supervisor 终结 `--bg` session) |
| **orchestrator 安装态变化**(仅 Codex tmux) | 装/卸 plugin 后 send-keys `/reload-plugins` |
| **meta-agent / 用户** | 调 `mcp__ccteam__workflow_spawn_agent` / `signal` / `trigger_gate` / `set_parallelism` |
| **人** | tmux attach 手动键入(Codex)、`ccteam web` UI、meta-agent 对话 |

**ESCALATE 的真正用途** — 只有人 / meta-agent 能决定的事(spec 不清、技术选型卡住、外部依赖缺失),通过 `escalation` event 写入 progress.jsonl(reason 自由文本)。orchestrator 不解析 reason 内容;meta-agent 用 `mcp__ccteam__get_progress` / `observe_agents` 读到后自然语言决策,调 `spawn_agent` / `signal` / `trigger_gate`。

```
✅ ESCALATE: spec.md 仅含 "mdeditor",无法做技术选型。需澄清:(1)平台?(2)用户?(3)场景?(4)约束?
✅ ESCALATE: auto-loop 已撞 3 轮顶,根本原因疑似 planner role 选型错误,建议 meta-agent spawn 新 planner 重做。
❌ ESCALATE: 当前 context 已 70%,请 reset(单 agent session 独立 context,supervisor 决定回收,role 不请求)
❌ ESCALATE: 请 send-keys /reload-plugins(role 不该指挥 orchestrator)
```

---

## 通道 3 — meta-agent(事件驱动 + MCP 工具)

通道 1 让 spawn 的 role agent 在自己的 session 内做工具决策——但用户需要一个**跨 workflow / 跨 session 的对话面**:看进度、调并发、起新 role、解锁 gate、escalation 处置。这是 **meta-agent**(常驻 ccteam-managed claude session,装 `ccteam` MCP server)的职责。

**17 个 mcp__ccteam__\* 工具完整清单见 interfaces.md §12.2**;高频:`spawn_agent` / `stop_agent` / `observe_agents` / `signal` / `set_parallelism` / `trigger_gate` / `get_artifact_summary`。约束:meta-agent 所有操作落 progress.jsonl(`agent_spawn` / `gate_triggered` 等 event);不持有 workflow 状态(`workflow.yaml` 是 SoT)。

### 3.1 与通道 1、2 的边界

| 决策类型 | 谁来 | 前置条件 |
|---|---|---|
| role 内调 code-reviewer / code-simplifier 等 subagent | 通道 1(spawn 的 role 自决) | spawned session `enabledPlugins` 启用对应 plugin(见 §6.2) |
| Codex tmux session 的 `/exit` reset / `/reload-plugins` | 通道 2(仅 Codex executor) | Codex agent 而非 Claude bg agent |
| 起 / 停 agent、调 parallelism、解锁 gate、escalation 处置 | 通道 3(meta-agent + MCP 工具) | meta-agent 装 ccteam-mcp 17 工具 |

**重要架构后果**:Plugin agent 不是"装了 plugin 就能调"——spawned session 必须显式 enable plugin pipeline 才进通道 1。`bootstrap_project` 据 workflow.yaml + role.md 解析依赖,自动写 `enabledPlugins` 到 `<project>/.claude/settings.json`;session 启动时 plugin pipeline 加载 + namespace,role.md 用 `Task(subagent_type=...)` 即可调。

### 3.2 与 workflow.yaml gate trigger 的关系

gate trigger 是 workflow.yaml 里**声明式**的人工节点——某 role 的 `trigger: gate` 表示该 role 等 meta-agent / 人调 `trigger_gate` 才起。把"哪些点必须人决策"显式写进 workflow 拓扑,不需要 LLM 决策路由。

### 3.3 不做什么

- ❌ 不替 spawn role 内的 Claude 做工具选择(那是通道 1 的职责)
- ❌ 不持有 workflow 状态(`workflow.yaml` + progress.jsonl 才是 SoT)
- ❌ 不参与 cost / stall 监控(orchestrator deterministic 监控的活)

---

## 工具清单 — workflow.yaml + role.md 作者参考

### 6.1 默认可用的 subagent(Task 直接可调)

| `subagent_type` | 来源 | 适用场景 |
|---|---|---|
| `general-purpose` | Claude Code 内置 | 兜底,任何复杂任务 |
| `Explore` | Claude Code 内置 | 只读快速搜索 |
| `Plan` | Claude Code 内置 | 实现计划设计 |
| `claude-code-guide` | Claude Code 内置 | Claude Code 用法咨询 |
| `statusline-setup` | Claude Code 内置 | 配置 status line |

### 6.2 想做 plugin 级别 review / simplify / 架构方案 — 用 plugin pipeline

`pr-review-toolkit` / `feature-dev` / `code-simplifier` 等 plugin 里的 agent **装了 plugin 也不能直接 Task 调**(见 §1.1.2),除非 enable plugin pipeline。`bootstrap_project` 据 workflow.yaml + role.md 解析依赖的 Claude Code plugin(静态映射表 `crates/ccteam-core/src/plugin_resolution.rs`),写 `<project>/.claude/settings.json` 的 `enabledPlugins`:

```jsonc
{"enabledPlugins": {"pr-review-toolkit@claude-plugins-official": true, "feature-dev@claude-plugins-official": true, "code-simplifier@claude-plugins-official": true}}
```

session 启动时 plugin pipeline 自动加载 enabled plugin,**namespace 加 `<plugin>:` 前缀**(eg `pr-review-toolkit:code-reviewer`);role.md 用裸名 `Task(subagent_type="code-reviewer")` 仍可调,pipeline 自匹配。

| 来源 plugin | agent 文件 → subagent_type | ccteam 用例 |
|---|---|---|
| `feature-dev` | `code-architect` / `code-explorer` | `planner` / `explorer` role |
| `pr-review-toolkit` | `code-reviewer` / `silent-failure-hunter` / `pr-test-analyzer` / `type-design-analyzer` / `comment-analyzer` | `implementer` / `reviewer` role |
| `code-simplifier` | `code-simplifier` | `polisher` role |

**用户需先装上游 plugin**:`claude /plugin add <plugin>@claude-plugins-official`(只一次,user level)。**关键约束**:enabledPlugins 必须在 session 启动时已写好(执行顺序见 §1.2.5)。

### 6.3 推荐挂的 hook + MCP server

完整 hook 表(progress.jsonl 写入、escalation 解析、cost 累计、危险命令拦截等)详见 **interfaces.md §6**。完整 MCP server 注册 + 工具清单详见 **tech-design.md §6.4 + interfaces.md §12**。本文不重复维护。

### 6.4 role.md 引用语法速查

| 目的 | 写法 |
|---|---|
| 引用某个文件让模型读 | `@spec.md` / `@$CCTEAM_INPUT/<artifact>` |
| 引用 plugin 里某个 agent 文件让模型按里面规程办 | `@~/.claude/plugins/marketplaces/claude-plugins-official/plugins/feature-dev/agents/code-architect.md` |
| 让模型主动 launch subagent(默认 5 个) | "请使用 Task 工具,subagent_type='general-purpose'/'Explore'/'Plan'/'claude-code-guide'/'statusline-setup',..." |
| 让模型主动 launch plugin subagent(必须先 §6.2 enable plugin pipeline) | "请使用 Task 工具,subagent_type='code-reviewer',..." |
| 模型按 plugin agent 规程办但不显式调 subagent | "请读 `@~/.claude/plugins/.../agents/code-reviewer.md`,严格按其指引 review 当前 diff" |
| 让模型用 skill | "请使用 Skill 工具调用 <name> skill" |
| 让模型用 MCP tool | "请使用 mcp__\<server>__\<tool> 工具,..." |
| 让模型产 artifact 触发下游 role | "完成后写到 `$CCTEAM_OUTPUT/<file>`"(watch trigger 自动驱动下游) |
| 异常上报 | 写 `escalation` event(progress.jsonl),meta-agent 看到自决策 |

### 6.5 怎么发现新工具

人工开发时**不要凭训练记忆猜工具名**。在长会话里检查可用工具:

- 看每个 system-reminder 块开头的 available-skills 列表
- 调试期 `Task(subagent_type="general-purpose", prompt="列出会话里所有 mcp__ 开头工具 + 所有 subagent_type")` 把返回回填本文档 §6
- `ccteam doctor` 汇报当前可见的 plugin / agent / MCP server,并和 workflow.yaml + role.md 依赖做交叉检查

---

## 附:常见误用与对策

| 现象 | 根因 | 对策 |
|---|---|---|
| role.md 写 "请 `/review`",模型却没有动作 | 模型摸不到 slash command | 改为 "请用 Task 工具调 code-reviewer subagent" + §6.2 plugin pipeline 启用 |
| `Task(subagent_type="code-reviewer")` 报 "Agent type not found, Available: general-purpose, Explore, Plan, claude-code-guide, statusline-setup" | **装了 plugin 不等于 Task 能调它的 agent**(spawned session 没启用 plugin pipeline) | spawned session 的 `<project>/.claude/settings.json` 加 `enabledPlugins: {"<plugin>@<mkt>": true}`(ccteam V0.4.0+ 自动据 workflow.yaml + role.md 写);临时兜底用方案 B(`@文件引用` + Task general-purpose) |
| `Skill(skill="review-pr")` 报 InputValidationError | `review-pr` 是 plugin 的 slash command 不是 Skill;`commands/<name>.md` 文件不被 Skill 工具识别 | role.md 别让模型自己调;Codex agent 走 send-keys |
| `Skill(skill="X")` 报 InputValidationError | skill 名字写错 / 当前会话没加载到 system-reminder 列表 | §1.2.2 的探针实测当前可调 skill |
| `mcp__foo__bar` 报工具不存在 | MCP server 没连 | 检查项目 `.mcp.json` + `ccteam doctor` |
| 模型在回答里写了 `/exit` 但会话没退 | TUI-only 命令模型摸不到 | V0.4.0+ Claude bg session 没 TUI,role 应通过产 artifact 触发下游 / escalate event 协调 |
| Claude bg session 跑飞 / 一直不结束 | role.md 缺 completion criterion;或 `timeout` 字段没设 | role.md 明确 "done 条件";workflow.yaml 为该 role 配 `timeout` + `on_timeout: escalate` |

---

## workflow.yaml trigger ↔ Claude Code 工具触发面

ccteam 把"何时起 agent"的决策放在 **workflow.yaml trigger**,本节解释这层抽象怎么落到 Claude Code 的工具触发面上。

### 三种 trigger 的工具触发后果

| trigger 类型 | 触发条件 | spawn 时 ccteam 注入 env | role.md 应该用的工具 |
|---|---|---|---|
| `watch:<path>` | inotify 检测到 `<path>` 下新文件写入完成(`IN_CLOSE_WRITE`,200ms debounce) | `CCTEAM_INPUT=<path>`(role.md 用 `Read` / `Glob` 扫输入)、`CCTEAM_OUTPUT=<output>` | 通常 `Read $CCTEAM_INPUT/<file>` + 处理 + `Write $CCTEAM_OUTPUT/<artifact>` |
| `schedule` | 定时(`interval: 5m` 等)或 meta-agent 调 `spawn_agent` 主动触发 | `CCTEAM_OUTPUT=<output>` (input 通常没意义) | 自主任务(crawler / monitor 类),用 `Bash` / `WebFetch` 拉数据,`Write` 出 artifact |
| `gate` | meta-agent / 人调 `mcp__ccteam__workflow_trigger_gate(gate_name, slug)` 后才起 | `CCTEAM_OUTPUT=<output>` + `CCTEAM_INPUT=<input>`(若声明) | 通常做"最终把关"(ship / publish 类),输出落地 + 通过 `Bash` 调外部命令(`gh pr create` / `npm publish` 等) |

### spawn 时的完整 env 注入(V0.4.0 实测)

```
CCTEAM_PROJECT_SLUG=<team>-<slug>
CCTEAM_INPUT=<project_root>/<workflow.yaml::agents.<role>.input>
CCTEAM_OUTPUT=<project_root>/<workflow.yaml::agents.<role>.output>
CCTEAM_JOB_ID=<uuid>
CCTEAM_ROLE=<role>
```

role.md 里**直接引用** `$CCTEAM_INPUT` / `$CCTEAM_OUTPUT` 比硬编码路径好——
项目名 / 路径 schema 升级时 role.md 无需改。

### artifact 通信(role 之间的"消息")

V0.4.0 红线:role 之间**只通过文件系统 artifact 通信**,不用 MCP 直接 RPC。
具体做法:

- upstream role `Write $CCTEAM_OUTPUT/<filename>` → 关文件
- ArtifactWatcher 200ms 后发 `ArtifactEvent` → orchestrator 检查
  `parallelism` → spawn downstream role(其 `trigger: watch:<那个 output 目录>`)
- downstream role 启动时拿到 `CCTEAM_INPUT=<刚才那个 output 目录>`,`Read` /
  `Glob` 自己消费

这就是为什么 role.md 应该用 `Read` / `Glob` / `Write` 这几个内置工具操作
artifact —— 这些工具直接和 Claude Code prompt cache 友好,且 ArtifactWatcher
依赖文件系统事件,不依赖 MCP。

### 与通道 1/2/3 的对应

| workflow.yaml 概念 | 对应工具触发面 |
|---|---|
| `workflow.yaml::agents.<role>.executor: claude` | spawn `claude --bg --agent <role>`;**通道 1** 全开(role 可调 Task / Skill / MCP / 内置),**通道 2 不可用**(没 TUI) |
| `workflow.yaml::agents.<role>.executor: codex` | spawn tmux + codex;**通道 1** 部分开(看 Codex 支持哪些工具),**通道 2 可用**(tmux send-keys) |
| `workflow.yaml::agents.<role>.trigger: gate` | 由 **通道 3**(meta-agent + `mcp__ccteam__workflow_trigger_gate`)解锁后才起 |
| role 产 artifact → 下游 `trigger: watch:*` | 完全 orchestrator deterministic(ArtifactWatcher),**不经任何 LLM 决策** |
| escalation event 落 progress.jsonl | **通道 3** meta-agent 用 `observe_agents` / `get_progress` 读到自决策 |

详 `docs/versions/v0-4-0/prd.md §6` 完整架构 + `docs/interfaces.md` workflow.yaml schema。
