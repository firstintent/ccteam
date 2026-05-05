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
| A. 安装到全局 agents 目录 | `cp <plugin>/agents/code-reviewer.md ~/.claude/agents/` 后,所有 claude session 的 `Task(subagent_type="code-reviewer")` 都能调 | ✅ 推荐;ccteam M1 `bootstrap_project` 应该批量做这件事 |
| B. `@文件引用` + general-purpose 执行 | phase markdown 里写 "请用 Task(subagent_type='general-purpose'),把 `@~/.claude/plugins/.../agents/code-reviewer.md` 的内容当作 system prompt,review 当前 diff" | ✅ 零安装,但每次都走 general-purpose,损失了 plugin agent 的 model / color / tools 配置 |
| C. orchestrator send-keys 触发 slash command | tmux send-keys `/review-pr <args>`,plugin 自己的 prompt body 在 TUI 上下文里能调它的私有 agent | ⚠️ 阻塞长会话当前 turn,只适合 phase 边界 |

**ccteam 的设计选择**:M0 用方案 B(零安装,文档先行);M1 在 `bootstrap_project`
里加方案 A 的批量复制(`ccteam doctor --install-recommended-agents`);方案 C
留给 director-claude(通道 3)在跨 phase 路由时调用。

#### 1.1.4 验证示例(无需任何 plugin,直接可跑)

```
请用 Task 工具,subagent_type="general-purpose",
description="probe tool surface",
prompt="列出当前工作目录下所有 .md 文件,统计每个文件的行数。"
```

#### 1.1.5 验证示例(方案 B,@ 引用 plugin agent prompt)

```
请用 Task 工具,subagent_type="general-purpose",
description="run code-reviewer on HEAD diff",
prompt="读 @~/.claude/plugins/marketplaces/claude-plugins-official/plugins/pr-review-toolkit/agents/code-reviewer.md
里的 instructions,严格按那份指引,review 当前 git 未暂存 diff,产出
critical / major / minor 三档问题清单。"
```

#### 1.1.6 实测当前会话可用 subagent

不放心或换了机器,现场跑:

```
请用 Task 工具调一个不存在的 subagent_type(如 "probe-test-12345"),
看返回的 "Available agents: ..." 错误清单,即是当前会话所有可调
subagent_type。
```

#### 1.1.2 plugin 提供的 subagent

需要 `/plugin install <plugin>@claude-plugins-official` 安装后才作为
`subagent_type` 可见。安装后,在长会话里模型可以:

```
Task(subagent_type="code-reviewer", description="review HEAD diff",
     prompt="Review the unstaged diff against CLAUDE.md guidelines.")
```

ccteam 项目用 plugin agent 的步骤见 §6.2(必须先 ln -sf 到 `~/.claude/agents/`
才能 Task 调,不是装 plugin 就能用)。

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

#### 1.2.4 Skill 支持中途热加载,这是 ccteam 的重要杠杆

Claude Code 对 `~/.claude/skills/`、项目 `.claude/skills/`、`--add-dir`
所加目录实时监听:**新增 / 修改 / 删除 SKILL.md 文件,当前会话立即生效**,
不需重启,不丢 context([官方文档](https://code.claude.com/docs/en/skills.md#live-change-detection))。

**唯一例外**:会话启动时那个 skills 顶层目录如果还不存在,后来才创建,
不会被监听 → 需要重启或 `/reload-plugins`。所以 `bootstrap_project`
应该**预创建空目录**:`~/.claude/skills/`、`<project>/.claude/skills/`,
即使一开始没文件。

**对 ccteam 的架构含义**:
- ccteam M1+ 可以做**按 phase 懒注入 skill** —— 比如 `review` phase 触发
  前,orchestrator 把一份"针对当前 phase 定制的 review skill"写进
  `<project>/.claude/skills/phase-review/SKILL.md`,长会话立即可调。
- 不需要为了"让模型用某个能力"重启长会话或破坏 prompt cache。
- Plugin **不能**中途装(需要重启);已装 plugin 可以通过 send-keys
  `/reload-plugins` 不丢 context 地刷新它的 skills/agents/hooks/MCP/LSP。
- Agent 文件(`~/.claude/agents/<name>.md`)文档没明说是否实时监听 ——
  **需要实测**,见下面探针。

#### 1.2.5 实测结果:agent **不**热加载,但 startup 时 ln 进去就能用

**完整验证链(2026-05-05,真长会话实测)**:

| 步骤 | 状态 | 结果 |
|---|---|---|
| ① 仅装 plugin,长会话调 `Task(subagent_type="code-reviewer")` | ❌ | `Agent type 'code-reviewer' not found. Available agents: claude-code-guide, Explore, general-purpose, Plan, statusline-setup` |
| ② 不退出会话,另一终端 `ln -sf <plugin>/agents/code-reviewer.md ~/.claude/agents/` | — | 软链已建,但当前会话不感知 |
| ③ 同会话(未 `/exit`、未 `/reload-plugins`),重跑 Task 调用 | ❌ | 仍然 `Agent type 'code-reviewer' not found`,Available 列表没变 |
| ④ `/exit` 长会话 + 重新启动 claude(此时 `~/.claude/agents/code-reviewer.md` 已就位) | — | — |
| ⑤ 重启后会话再调 `Task(subagent_type="code-reviewer")` | ✅ | code-reviewer 正常初始化、执行、返回响应 |

**最终结论**:
- Skill 的 SKILL.md 文件**实时监听**,中途加生效(§1.2.4)
- `~/.claude/agents/<name>.md` **会话启动时一次性扫描**,**中途加不生效**;但 startup 时已存在的 agent 文件**正常被识别**——所以 M1 `bootstrap_project` 在 `tmux new-session` 之前 ln -sf 这条路**完全可行**(已实测确认)

#### 1.2.6 给 ccteam M1 `bootstrap_project` 的强约束

既然 agent 必须 startup 前注册,`bootstrap_project` 的执行顺序必须是:

```
ccteam new <brief>
  ├─ 1. 创建 ~/projects/<slug>/ 目录与子目录
  ├─ 2. 写 spec.md / CLAUDE.md / phase 模板 / settings.json
  ├─ 3. 写 ~/.claude.json 的 hasTrustDialogAccepted
  ├─ 4. **ln -sf 推荐 plugin agents 到 ~/.claude/agents/**(M1 新增)
  ├─ 5. mkdir -p ~/.claude/skills/ <project>/.claude/skills/(占位让监听挂上)
  └─ 6. 由 orchestrator ensure_session 触发 tmux new-session(claude TUI 启动)
```

第 4 步**必须**在第 6 步之前;第 5 步是为了让 skill 后续懒注入能命中
(§1.2.4 实时监听只对会话启动时已存在的目录生效)。

如果运行中需要新增 agent(M2 director-claude 决定切到一个新 agent),
**只有两条路**:

- **重启长会话**(`/exit` + 新 session)—— 不丢 progress.jsonl 但丢
  prompt cache、丢 in-flight context
- **send-keys `/reload-plugins`** —— 不丢 cache,但会把已装 plugin 全部
  reload 一遍(plugin 状态回 idle)

两条都贵,所以 M1 优先把"会用到的全部 agent"在 `bootstrap_project`
阶段一次性 ln 齐。

#### 1.2.7 `Task` ≠ `TaskCreate` — 容易踩的命名坑

实测时模型在第二次调用走错了工具 — 用了 `TaskCreate` 而不是 `Task`/
`Agent`。这两个是**完全不同的工具**:

| 工具 | 作用 | 关键参数 |
|---|---|---|
| `Task` / `Agent` | 启动一个 subagent 跑一个任务 | `subagent_type`, `description`, `prompt` |
| `TaskCreate` | 在任务管理列表里**创建一条 todo**(不是启动 agent) | `subject`, `description`, `activeForm` |

phase markdown 写 prompt 时要明确说 "用 `Task` 工具" 或 "用 `Agent` 工具"
(取决于当前会话哪个名字暴露出来——两个名字其实是同一个工具的别名)。
**不要写"用 TaskCreate"——那是另一个工具,会创建 todo 而不是 launch agent**。
也避免写"用 Task tool"再加上 `subject` / `description` 参数,模型可能挑错。
最稳的写法:

```
请用 Task 工具(注意是 Agent 调度工具,不是 TaskCreate 任务管理工具),
传 subagent_type="<name>", description="<short>", prompt="<full body>"。
```

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

**纠正前文一个误导**:之前写过"phase 用 ESCALATE 请 orchestrator 做
`/exit` reset"。这是错的——**orchestrator 是 Rust 程序,没有 agent,
读 ESCALATE 只能字符串匹配,看不懂自然语言**。所以"请 orchestrator 触发
context reset"这种文字它压根理解不了。

正确分工是这样的:

#### 2.2.1 通道 2 命令的真正触发源

| 谁触发 | 怎么触发 | 例子 |
|---|---|---|
| **orchestrator deterministic 监控** | 读 progress.jsonl 里 PostToolUse hook 累加的 `context_tokens_used` / `cost_used_usd`,跨阈值就 send-keys | context > 60% → `/exit` + 新 session(tech-design §6.9);cost > $200 → 硬终止 |
| **orchestrator 安装态变化** | 装/卸 plugin 后 send-keys `/reload-plugins` | M2 自动安装 agent 时 |
| **director-claude(M1+)** | 短命 claude 解读 progress.jsonl,产出结构化决策事件,orchestrator 据事件 send-keys | 决定下 phase 前先 send `/review-pr`(plugin slash command) |
| **人** | tmux attach 手动键入 | 调试用 |

#### 2.2.2 ESCALATE 的真正用途 — 用户决策回路,不是命令请求

phase 该用 ESCALATE 的场景是**只有人能决定的事**:spec 不清、关键技术
选型卡住、外部依赖缺失。**不是用来请求 TUI 命令**——那是 orchestrator
自己的监控职责。

```
✅ ESCALATE: spec.md 仅含 "mdeditor",无法做技术选型。需澄清:
   (1) 目标平台?(2) 目标用户?(3) 核心场景?(4) 关键约束?

✅ ESCALATE: fix-loop 已撞 3 轮顶,根本原因疑似 plan-eng 阶段技术选型
   错误,建议人工 review 后回退到 plan-eng 重做。

❌ ESCALATE: 当前 context 已 70%,请 reset
   (orchestrator 看 context_tokens_used 自决,不需要 phase 请求)

❌ ESCALATE: 请 send-keys /reload-plugins
   (phase 不该指挥 orchestrator 做哪条命令)
```

#### 2.2.3 ESCALATE 的字符串语法约定

`crates/ccteam-hooks/src/parse_phase_end.rs` 现在认的是:

```
ESCALATE: <reason — 自由文本>
```

orchestrator 默认行为(M0):写 escalation event,phase 标 escalated,停掉
自动调度。**不解析 reason 内容**;reason 是给人看的(M0 inbox / M1
Telegram)。**M1+ director-claude 才解读 reason 决定下一步路由**——但即
使解读了,output 也是结构化 `director_decision` 事件,orchestrator 仍
做的是字符串路由(看 `next_phase` 字段),不解读 reason。

如果未来要给 ESCALATE 加结构化指令通道(让 phase 显式请求"回退到
plan-eng"),正确做法是**扩协议**:

```
ESCALATE: REVERT_TO_PHASE plan-eng — fix-loop 撞顶,根因在选型
ESCALATE: NEED_USER_INPUT — spec 不清,问题:[...]
ESCALATE: ABORT — 超出 ccteam 当前能力,人工接手
```

orchestrator 字符串匹配前缀(`REVERT_TO_PHASE` / `NEED_USER_INPUT` /
`ABORT`)做对应路由——**仍然是 dumb 路由,仍然不需要 LLM 解读**。这
扩展未来想做时,要同步 update 三处:`parse-phase-end` 解析、`interfaces.md`
ESCALATE 语法节、phase 模板里的 ESCALATE 写法示例。M0 不需要,先用自由
文本。

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

| 决策类型 | 谁来 | 前置条件 |
|---|---|---|
| 当前 phase 里要不要调 code-reviewer / code-simplifier | 通道 1(长会话内 Claude 自决) | agent 文件已 ln 到 `~/.claude/agents/`(见 §6.2) |
| 要不要 `/exit` reset / `/reload-plugins` | 通道 2(orchestrator,看 cost / context 阈值机械触发) | — |
| 下一 phase 走 fix 还是 ship,要不要 inject `/review-pr` 等 TUI 命令 | 通道 3(director-claude) | M1+ |

**重要的架构后果**:Plugin agent 不是"装了 plugin 就能调"——必须额外做一步
agents 目录注册才进通道 1。这意味着:**ccteam M1 的 `bootstrap_project`
必须把 §6.2 那段 ln -sf 加进去**,否则所有"phase 让模型自己调 code-reviewer"
的设计在产出项目里都跑不通,只能 fallback 到通道 3 的 send-keys `/review-pr`。
这条已记入 development-plan(M1.x)。

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

### 6.1 默认可用的 subagent(Task 直接可调)

| `subagent_type` | 来源 | 适用场景 |
|---|---|---|
| `general-purpose` | Claude Code 内置 | 兜底,任何复杂任务 |
| `Explore` | Claude Code 内置 | 只读快速搜索 |
| `Plan` | Claude Code 内置 | 实现计划设计 |
| `claude-code-guide` | Claude Code 内置 | Claude Code 用法咨询 |
| `statusline-setup` | Claude Code 内置 | 配置 status line |

### 6.2 想做 plugin 级别 review / simplify / 架构方案 — 必须做安装步骤

`pr-review-toolkit` / `feature-dev` / `code-simplifier` 等 plugin 里的 agent
**装了 plugin 也不能直接 Task 调**(见 §1.1.2)。要在 ccteam 自治调用,
需要在 ccteam `bootstrap_project`(或一次性手工)里执行下面这步:

```bash
# 把 plugin agent 文件软链/拷到全局 agents 目录,Claude Code 会扫这里
# 自动注册成 subagent_type。
# 显式列出每个文件 —— **不要用 pr-review-toolkit/agents/*.md 这种 glob**:
# 该目录下也有 code-simplifier.md,会跟下面 code-simplifier plugin 的同名文
# 件抢同一个 ~/.claude/agents/code-simplifier.md target,后写盖前写,得到
# 哪个版本不确定。
mkdir -p ~/.claude/agents
PLUGIN_ROOT=~/.claude/plugins/marketplaces/claude-plugins-official/plugins
for src in \
  "$PLUGIN_ROOT/pr-review-toolkit/agents/code-reviewer.md" \
  "$PLUGIN_ROOT/pr-review-toolkit/agents/silent-failure-hunter.md" \
  "$PLUGIN_ROOT/pr-review-toolkit/agents/pr-test-analyzer.md" \
  "$PLUGIN_ROOT/pr-review-toolkit/agents/type-design-analyzer.md" \
  "$PLUGIN_ROOT/pr-review-toolkit/agents/comment-analyzer.md" \
  "$PLUGIN_ROOT/feature-dev/agents/code-architect.md" \
  "$PLUGIN_ROOT/feature-dev/agents/code-explorer.md" \
  "$PLUGIN_ROOT/code-simplifier/agents/code-simplifier.md"
do
  ln -sf "$src" "$HOME/.claude/agents/$(basename "$src")"
done
```

(`code-simplifier` 取自 `code-simplifier` plugin 的版本——它是该 plugin 的
正主;`pr-review-toolkit` 里的 `code-simplifier.md` 是该 plugin 内部使用的
副本,生产代码 M0.5.1 实测 reviewer catch 了这个重名 target 冲突,显式枚举
是正确做法。)

之后用 `Task(subagent_type="probe-XXX")` 探当前可用列表 —— 应该多出
`code-reviewer`、`code-architect`、`code-explorer`、`code-simplifier`、
`silent-failure-hunter`、`pr-test-analyzer`、`type-design-analyzer`、
`comment-analyzer`。

| 来源 plugin | agent 文件 → subagent_type | ccteam 用例 |
|---|---|---|
| `feature-dev` | `code-architect` | plan-eng |
| `feature-dev` | `code-explorer` | 项目延续场景的 plan-eng |
| `pr-review-toolkit` | `code-reviewer` | implement / review phase |
| `pr-review-toolkit` | `silent-failure-hunter` | review phase |
| `pr-review-toolkit` | `pr-test-analyzer` | review phase |
| `pr-review-toolkit` | `type-design-analyzer` | review phase |
| `pr-review-toolkit` | `comment-analyzer` | review phase |
| `code-simplifier` | `code-simplifier` | review 后打磨 |

**ccteam 路线图**:M1 起 `bootstrap_project` 在 `tmux new-session` **之前**
自动做 ln -sf(执行顺序见 §1.2.6);可单独通过 `ccteam doctor
--install-recommended-agents` 给老项目补做。在 M1 落地前,phase 模板用
§1.1.3 方案 B(`@文件引用` + Task general-purpose)兜底。

**关键约束**(已实测确认):agent 文件必须在 claude session 启动时已存在 ——
中途 ln -sf 不生效(§1.2.5)。这条不能违反。

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
| 让模型主动 launch subagent(默认 5 个) | "请使用 Task 工具,subagent_type='general-purpose'/'Explore'/'Plan'/'claude-code-guide'/'statusline-setup',..." |
| 让模型主动 launch plugin subagent(必须先 §6.2 ln -sf) | "请使用 Task 工具,subagent_type='code-reviewer',..." |
| 模型按 plugin agent 规程办但不显式调 subagent | "请读 `@~/.claude/plugins/.../agents/code-reviewer.md`,严格按其指引 review 当前 diff" |
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
| phase markdown 写 "请 `/review`",模型却没有动作 | 模型摸不到 slash command | 改为 "请用 Task 工具调 code-reviewer subagent" + §6.2 安装 |
| `Task(subagent_type="code-reviewer")` 报 "Agent type not found, Available: general-purpose, Explore, Plan, claude-code-guide, statusline-setup" | **装了 plugin 不等于 Task 能调它的 agent**(plugin agent 文件不进 Task 全局注册表) | 跑 §6.2 那段 ln -sf,把 plugin agent 文件链到 `~/.claude/agents/`;或临时用方案 B(`@文件引用` + Task general-purpose) |
| `Skill(skill="review-pr")` 报 InputValidationError | `review-pr` 是 plugin 的 slash command 不是 Skill;`commands/<name>.md` 文件不被 Skill 工具识别 | 这条只能走通道 2(orchestrator send-keys `/review-pr`),phase markdown 别让模型自己调 |
| `Skill(skill="X")` 报 InputValidationError | skill 名字写错 / 当前会话没加载到 system-reminder 列表 | §1.2.2 的探针实测当前可调 skill |
| `mcp__foo__bar` 报工具不存在 | MCP server 没连 | 检查项目 `.mcp.json` + `ccteam doctor` |
| 模型在回答里写了 `/exit` 但会话没退 | TUI-only 命令模型摸不到 | 改成 ESCALATE 或让 orchestrator send-keys |
| 长会话 context 涨到 80% 才发现没 reset | 通道 2 没自动触发 | orchestrator 60% 阈值要在 PostToolUse hook 里检查(已在 tech-design §6.9) |
