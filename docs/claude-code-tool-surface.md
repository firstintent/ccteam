# Claude Code 工具触发面 — ccteam 自治编排的能力地图

> 本文档面向 **phase 模板作者**和 **director-claude / orchestrator 设计者**,
> 解决一个核心问题:在 ccteam 长会话里,**谁能触发什么命令、怎么触发**。
>
> 不读这份文档的后果:phase markdown 里写"请用 `/review` 检查代码",运行
> 时模型把 `/review` 当死字符输出 → orchestrator 拿不到 review report →
> 整条流水线静默失败。

---

## 一图概要

```
┌──────────────────────────── Claude Code 的"命令" ────────────────────────────┐
│                                                                              │
│   通道 1:prompt 内自调          通道 2:TUI-only            通道 3:director  │
│   (模型自己发工具调用)          (键盘 / send-keys 输入)    -claude(M1+)    │
│                                                                              │
│   • Agent / Task                  • /exit                   • 跨 phase 路由 │
│   • Skill                         • /clear                  • 元决策(下一  │
│   • MCP tools                     • /compact                  步该 fix 还是 │
│   • 内置 (Read/Edit/Bash/...)     • /reload-plugins           ship)         │
│                                   • /agents                                  │
│                                   • /help                                    │
│                                   • /memory                                  │
│                                   • /btw(idle-aware 注入用) │              │
│   ✅ phase markdown 直接编排      ❌ 模型摸不到             ⏳ M1+ 才上线    │
│   ✅ cache 命中、tmux 可见        ✅ orchestrator send-keys                  │
│   ✅ 便宜、可观测                                                            │
└──────────────────────────────────────────────────────────────────────────────┘
```

**关键事实**:模型把 `/review` 字面写在回答里**完全没效果**。slash command
是 Claude Code TUI 的输入解析器拦截的,只接受**人类键盘输入或 tmux send-keys**,
不接受模型输出的字符串。

---

## 通道 1 — prompt 内自调

### 1.1 `Task` / `Agent` 工具:启动 subagent

子 agent 是 ccteam 调"质量类"命令(`/review` / `/simplify` / 架构方案)的
首选——它们就是 plugin 里 `agents/<name>.md` 的内容,**装了 plugin 后,
模型可以通过 `Task(subagent_type=<name>, ...)` 直接调用**,效果等同于
`/review` 但是模型自发触发的。

#### 1.1.1 内置 subagent

`general-purpose` 是 Claude Code 默认 always-on 的 subagent,任何会话都能
直接 `Task(subagent_type="general-purpose", ...)`。**其它 subagent_type
都来自 plugin / 项目级 `.claude/agents/<name>.md` / 全局
`~/.claude/agents/<name>.md`,没装就调不动**。

要在长会话里**实测当前可用 subagent 列表**:

```
请用 Task 工具,subagent_type="general-purpose",
prompt="ls ~/.claude/agents/ 和当前项目 .claude/agents/(若存在),
还有 ~/.claude/plugins/marketplaces/*/plugins/*/agents/ 下所有
agents 目录,把 name 字段都列出来。"
```

返回的 name 字段就是当前长会话**真实可用**的 `subagent_type` 候选。
不要凭训练记忆假定 `Explore` / `Plan` / `code-reviewer` 等存在 ——
这些都得装 plugin。

#### 1.1.2 plugin 提供的 subagent

需要 `/plugin install <plugin>@claude-plugins-official` 安装后才作为
`subagent_type` 可见。安装后,在长会话里模型可以:

```
Task(subagent_type="code-reviewer", description="review HEAD diff",
     prompt="Review the unstaged diff against CLAUDE.md guidelines.")
```

ccteam 项目可用的 plugin agent 见 §6 工具清单。

#### 1.1.3 phase markdown 里怎么写(可手动验证示例)

把下面这段贴到 `phases/<name>.md` 的 body,模型读到会**自发**调 Task:

````markdown
完成实现后,在提交前必须自检:

请使用 `Task` 工具调用 subagent_type="code-reviewer" 的子 agent,
description="self-review of implement phase",
prompt 内容:`审查本轮 implement 阶段产生的所有未提交 diff,对照
@.ccteam/plan-eng.md 检查是否偏离规划;对照 CLAUDE.md 检查代码风格;
逐文件输出 critical / major / minor 三档问题清单,每档不超过 5 条。`

读取它的返回总结,把 critical 项处理完再 PHASE_DONE。
````

**验证方法**:在长会话里把上面那段当 prompt 发出去,观察模型是否发出
`Task(subagent_type=...)` 工具调用。如果看到 Task 调用并拿到结果,说明
通道 1 通畅。

#### 1.1.4 验证示例(无需任何 plugin 安装,直接可跑)

```
请用 Task 工具,subagent_type="general-purpose",
description="probe tool surface",
prompt="列出当前工作目录下所有 .md 文件,统计每个文件的行数。"
```

---

### 1.2 `Skill` 工具:激活 skill

Skill 是 Claude Code 的"可调用知识包" —— 比 subagent 轻,适合"按规程办事"
的场景(写 commit、生成 release notes 等)。

```
Skill(skill="<name>", args="<optional-args>")
```

`skill` 必须是当前会话**已加载** skill 列表里的名字(系统会通过
system-reminder 列出可用 skill)。**不要凭训练记忆猜 skill 名字** ——
Claude Code 会拒绝并报 InputValidationError。

#### 1.2.1 验证示例

ccteam 仓库已装 `ccgram-messaging` skill。在长会话里:

```
请用 Skill 工具调用 ccgram-messaging skill,
args="status" (或它支持的任意参数)。
```

观察模型是否发出 `Skill(skill="ccgram-messaging", ...)` 调用。

#### 1.2.2 与自定义 `/<skill>` slash command 的关系

很多 skill 同时注册成 slash command(用户键入 `/<name>`)。**模型不能
通过 `/<name>` 触发,只能通过 `Skill` 工具触发**——这是同一个 skill,
两个入口。phase markdown 让模型走 `Skill` 工具入口即可。

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
| `ccteam-mcp`(自建) | M2 | `mcp__ccteam__ls` / `__show` / `__new` 等 |

#### 1.3.2 验证示例(假设已装 Playwright MCP)

```
请用 mcp__plugin_playwright_playwright__browser_navigate 工具
打开 https://example.com,然后用 mcp__..._browser_snapshot 截屏。
```

---

### 1.4 内置工具

`Bash` / `Read` / `Edit` / `Write` / `Grep` / `Glob` / `WebFetch` /
`WebSearch` / `TodoWrite` / `NotebookEdit` 等。phase markdown 里通常
不需要显式说"用 Bash 工具"——模型会自己挑。**但有几个场景值得显式约束**:

- 让模型用 `Bash("gh pr create ...")` 而不是手搓 PR 模板 → 让 GitHub
  CLI 兜底
- 让模型用 `Read` 而不是 `Bash("cat ...")` → 用对的工具
- 让模型用 `Grep` 而不是 `Bash("grep ...")` → 速度更快

---

## 通道 2 — TUI-only(只能 orchestrator send-keys)

下面这些 slash command **模型完全摸不到**——把字面字符串 `/exit` 写进
回答里,Claude Code 不会把它解析回命令,只会显示成普通文本。**phase
markdown 里写这些 = 静默失败**。

| 命令 | 用途 | 谁该触发 |
|---|---|---|
| `/exit` | 退出当前会话 | orchestrator(context reset 时) |
| `/clear` | 清空当前 turn 的 context(不退出会话) | orchestrator(context 高位时) |
| `/compact` | 压缩历史 | orchestrator(可选,M0 用 `/exit` 替代) |
| `/btw <text>` | 忙时排队下一条消息 | orchestrator(idle-aware 注入,见 §6.9) |
| `/reload-plugins` | 重载 plugin 配置 | orchestrator(装/卸 plugin 后) |
| `/agents` | 查看可用 subagent 列表 | 人(调试用) |
| `/memory` | 编辑 CLAUDE.md | 人 / orchestrator |
| `/help` | 帮助 | 人 |

### 2.1 orchestrator 怎么触发

`crates/ccteam-core/src/tmux.rs` 的 `send_keys()`——把字面字符串送进
tmux 第一个 pane 再发 Enter。orchestrator 已经在用这条路:

- 注入 phase prompt(idle 时直接 send-keys,忙时套 `/btw`)
- context reset(send-keys `/exit` → wait_for_ready 新 session)

### 2.2 phase markdown 应该怎么"间接"触发它们

**不要直接写斜杠命令**。如果 phase 真需要 reset / reload,正确做法是
**ESCALATE 给 orchestrator**:

```
ESCALATE: 当前 context 已 70%,继续做 fix phase 风险高,请 orchestrator
触发 context reset 后重新调度 fix。
```

orchestrator 的 escalate 处理逻辑(M1+)读到这条,做 send-keys `/exit`
+ 新 session,而不是让长会话内的 Claude 自己尝试。

---

## 通道 3 — director-claude(M1+ 计划)

### 3.1 解决什么问题

通道 1 只能让长会话内的 Claude **在当前 phase 内**做工具决策——它
看不到"下一步该跑哪个 phase"这个层面。phase DAG 在 M0 是写死的
(plan-eng → implement → ...),但真实工作里有很多分支:

- 测试只挂 1 条:跳过 fix-loop 直接 ship?
- review 里发现架构问题:回 plan-eng 还是局部改?
- spec 改了:从头重跑还是只跑增量?

这些路由决策**不属于任何单个 phase**——它们是 phase 之间的元决策。

### 3.2 设计形态(草案,等用户拍板)

- **触发**:每次 `phase_done` / `escalate` 事件被 hook 写入 progress.jsonl
  之后,orchestrator 在派发下一 phase 之前,先跑一个**短命 claude**
  (类似 M1 的 cost-watcher / drift-detector)
- **输入**:project 当前 state.json + progress.jsonl 尾部 + 上一 phase
  的产物文件
- **输出**:一个**结构化决策事件**,`event: "director_decision"`,字段:
  - `next_phase`:下一阶段名(可以是 DAG 里的下一个,也可以是回退 / 跳跃)
  - `inject_extra`:可选,要追加在下一 phase prompt 前的额外指令
    (例:"先 `/review` 再做 ship",`/review` 由 send-keys 注入)
  - `rationale`:一句话理由,落 progress.jsonl
- **约束**:决策必须落 progress.jsonl(不写暗状态);跑完即退(不持有
  上下文);最多 30 秒(避免拖慢主流程)

### 3.3 与通道 1、2 的边界

| 决策类型 | 谁来 |
|---|---|
| 当前 phase 里要不要调 code-reviewer / code-simplifier | 通道 1(长会话内 Claude 自决) |
| 要不要 `/exit` reset / `/reload-plugins` | 通道 2(orchestrator,看 cost / context 阈值机械触发) |
| 下一 phase 走 fix 还是 ship,要不要 inject `/review` 之类 TUI 命令 | 通道 3(director-claude) |

### 3.4 与 sub_skills 的边界(M2)

sub_skills 是 phase front matter 里**声明式**指定的 plugin 触发,固定:
"phase X 完了一定 trigger Y"。director-claude 是**条件式**:"看了 X
的产出,决定是不是 trigger Y、以及触不触发 Z"。两者不冲突——sub_skills
管"惯例必走的路",director 管"按情况选路"。

### 3.5 不做什么

- ❌ 不替长会话内 Claude 做工具选择(那是通道 1 的职责,放到外层就丢了 cache)
- ❌ 不持有 phase 之间的内存状态(progress.jsonl 才是 truth source)
- ❌ 不参与 cost / stall 监控(那是 cost-watcher / stall-watcher 的活)

---

## 工具清单 — phase 模板作者参考

### 6.1 默认可用的 subagent

仅 `general-purpose` 是 Claude Code 内置且永远可用。其余都需要装 plugin
或在项目本地 `.claude/agents/` 自定义。

| `subagent_type` | 来源 | 适用场景 |
|---|---|---|
| `general-purpose` | Claude Code 内置 | 兜底,任何复杂任务 |

### 6.2 推荐安装的 plugin subagent(claude-plugins-official)

需要先 `/plugin install <plugin>@claude-plugins-official`(M2 起 ccteam
自动批量安装到产出项目)。

| 来源 plugin | `subagent_type` | 用途 | ccteam 用例 |
|---|---|---|---|
| `feature-dev` | `code-architect` | 产出 3 方案 + 推荐架构 | plan-eng |
| `feature-dev` | `code-explorer` | 并行探索代码库 | 项目延续场景的 plan-eng |
| `feature-dev` | `code-reviewer` | 代码审查 | implement / review phase |
| `pr-review-toolkit` | `code-reviewer` | PR 级审查 | review phase |
| `pr-review-toolkit` | `silent-failure-hunter` | 找静默失败点 | review phase |
| `pr-review-toolkit` | `pr-test-analyzer` | 测试覆盖分析 | review phase |
| `pr-review-toolkit` | `type-design-analyzer` | 类型设计审查 | review phase |
| `pr-review-toolkit` | `comment-analyzer` | 注释质量审查 | review phase |
| `code-simplifier` | `code-simplifier` | 代码简化 | review 后打磨 |

### 6.3 推荐挂的 hook(plugin 提供)

| 来源 plugin | hook 文件 | ccteam 挂位置 |
|---|---|---|
| `security-guidance` | `hooks/security_reminder_hook.py` | PreToolUse(Edit\|Write\|MultiEdit) |
| `ralph-loop` | `hooks/stop-hook.sh` 范式参考 | fix-loop ralph 模式(M0.12 已抄) |

### 6.4 推荐 MCP server(项目级 `.mcp.json`)

| Server | 里程碑 | 用途 | 关键 tool |
|---|---|---|---|
| Telegram bot | M1 | 异步消息 + escalation 推送 | `send_message` |
| Playwright | 按需 | 前端 E2E | `browser_navigate` / `browser_click` / `browser_snapshot` |
| `claude-mem` | M3 | 跨项目记忆 | `mcp-search__search` / `__get_observations` |
| GitHub | M4+ | 倾向用 `gh` CLI 替代 | — |
| `ccteam-mcp` | M2(自建) | 用户自带 claude 调度 ccteam | `ls` / `show` / `new` / `peek` / `progress` |

### 6.5 phase markdown 引用语法速查

| 目的 | 写法 |
|---|---|
| 引用某个文件让模型读 | `@.ccteam/spec.md` |
| 引用 plugin 里某个 agent 文件让模型按里面规程办 | `@~/.claude/plugins/marketplaces/claude-plugins-official/plugins/feature-dev/agents/code-architect.md` |
| 让模型主动 launch subagent | "请使用 Task 工具,subagent_type='code-reviewer',..." |
| 让模型用 skill | "请使用 Skill 工具调用 <name> skill" |
| 让模型用 MCP tool | "请使用 mcp__\<server>__\<tool> 工具,..." |
| 让 orchestrator 触发 TUI 命令 | 在 phase 末尾 ESCALATE,告诉 orchestrator 该做什么 |

### 6.6 怎么发现新工具

人工开发时**不要凭训练记忆猜工具名**。在长会话里检查可用工具:

- 看每个 system-reminder 块开头的 available-skills 列表
- 在 phase 调试期,跑一次 `Task(subagent_type="general-purpose",
  prompt="列出你这个会话里能调的所有 mcp__ 开头的工具,以及所有
  subagent_type 列表")`,把返回结果存到 `docs/claude-code-tool-surface.md` §6 更新
- ccteam 自身的 `ccteam doctor`(M1+)会汇报当前可见的 plugin / agent /
  MCP server,并和 phase 模板里的依赖做交叉检查

---

## 附:常见误用与对策

| 现象 | 根因 | 对策 |
|---|---|---|
| phase markdown 写 "请 `/review`",模型却没有动作 | 模型摸不到 slash command | 改为 "请用 Task 工具调 code-reviewer subagent" |
| `Task(subagent_type="code-reviewer")` 报 unknown subagent type | plugin 没装到当前会话 | 在产出项目装 plugin(M2 自动 / 现在手动 `/plugin install`) |
| `Skill(skill="X")` 报 InputValidationError | skill 名字写错 / 没加载 | 看 system-reminder 里的 available-skills 真实列表 |
| `mcp__foo__bar` 报工具不存在 | MCP server 没连 | 检查项目 `.mcp.json` + `ccteam doctor` |
| 模型在回答里写了 `/exit` 但会话没退 | TUI-only 命令模型摸不到 | 改成 ESCALATE 或让 orchestrator send-keys |
| 长会话 context 涨到 80% 才发现没 reset | 通道 2 没自动触发 | orchestrator 60% 阈值要在 PostToolUse hook 里检查(已在 §6.9) |
