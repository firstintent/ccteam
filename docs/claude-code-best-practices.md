# Claude Code 最佳实践（本地参考）

> **来源**：https://code.claude.com/docs/en/best-practices
> **抓取时间**：2026-05-05
> **用途**：ccteam 构建在 Claude Code 之上，本文件作为"产品自身（开发 ccteam 时）"与"产品产出（ccteam 编排的下游 Claude 实例）"两个层面的设计依据。
>
> 本文件**不是原文搬运**，是按 ccteam 视角重组的要点抽取。需要原文细节请回链查看。

---

## 0. 核心约束（贯穿全文档）

> **Claude 的 context window 会快速填满，性能随之下降。所有最佳实践基本都是在管理这一约束。**

context 包括：每条消息、每次文件读取、每次命令输出。一次 debug 或代码库探索可能消耗几万 token。当 context 满时，Claude 会"忘记"早先指令、犯更多错误。

**关键观测点**：
- 用 status line 持续追踪 context 使用率
- 参考 `/en/context-window`、`/en/statusline`、`/en/costs#reduce-token-usage`

> **ccteam 映射**：详见 tech-design.md §6.9(每个 agent session 独立 context,跨 agent 隔离即天然 reset 单位)。

---

## 1. 给 Claude 一个验证自己工作的方法

> **官方原话："This is the single highest-leverage thing you can do."**

### 三种策略

| 策略 | Bad | Good |
|---|---|---|
| 提供验证标准 | "实现一个验证邮箱的函数" | "写 `validateEmail`。测试用例：`a@b.com` true，`invalid` false，`a@.com` false。实现后运行测试" |
| UI 视觉验证 | "让 dashboard 好看一点" | "[paste 截图] 实现这个设计。截图结果对比原图，列差异并修" |
| 修根因不修症状 | "build 失败了" | "build 失败信息：[paste]。修复并验证 build 通过。修根因，不要压制错误" |

UI 验证可用 [Claude in Chrome](/en/chrome) 扩展（自动开 tab 测 UI 迭代）。

> **ccteam 映射**:requirements.md 痛点 3 + tech-design.md §3.6「测试即验收」直接来自此原则;role.md 产物需写可执行验证条目,下游 `verifier` role 通过 watch trigger 闭环。

---

## 2. 先探索，再规划，再编码

> **官方原话："Separate research and planning from implementation to avoid solving the wrong problem."**

### 推荐 4-step workflow

| 阶段 | 动作 | Claude 模式 |
|---|---|---|
| **Explore** | 进入 plan mode，读文件、回答问题，不改文件 | plan mode |
| **Plan** | 让 Claude 写详细实现计划。`Ctrl+G` 可在编辑器里直接改 plan | plan mode |
| **Implement** | 退出 plan mode，对照 plan 执行 + 跑测试 | default mode |
| **Commit** | 让 Claude 写 commit 消息 + 开 PR | default mode |

### 何时跳过 plan mode

- 范围明确、改动小（typo、加 log、改名）→ 直接做
- 一句话能描述 diff → 跳过 plan
- 跨多文件、不熟悉的代码、方案不确定 → **必须** plan

> **ccteam 映射**:explore/plan/implement 分离由 `workflow.yaml` 多 role 拓扑 + artifact 文件传递实现(如 `explorer` → `planner` → `implementer`,每个独立 `claude --bg` session 天然 context 隔离)。详 tech-design.md §3.3。

---

## 3. 在 prompt 中提供具体上下文

| 策略 | Bad | Good |
|---|---|---|
| **Scope the task** | "为 foo.py 加测试" | "为 foo.py 写测试，覆盖用户登出场景的 edge case。不用 mock" |
| **Point to sources** | "为啥 ExecutionFactory 的 API 这么怪" | "看 ExecutionFactory 的 git history，总结它的 API 怎么演化成现在这样" |
| **Reference patterns** | "加个日历组件" | "看 home page 的现有 widget（如 HotDogWidget.php），按相同模式实现日历 widget。不用新库" |
| **描述症状** | "修登录 bug" | "用户反映 session 超时后登录失败。检查 src/auth/，特别是 token refresh。先写一个失败的测试复现，再修" |

**例外**：探索阶段、可承受走偏时，模糊 prompt 反而能挖到你想不到的问题（如 "what would you improve in this file?"）。

### 提供丰富上下文的方式

- **`@` 引用文件**：让 Claude 自己读，不要自己复述
- **直接粘图**：复制 / 拖拽
- **给 URL**：用 `/permissions` 把常用域名加白
- **管道喂数据**：`cat error.log | claude`
- **让 Claude 自己拉**：告诉它用 Bash / MCP / Read 自取

> **ccteam 映射**:role.md 用 `@<file>` 引用大段背景(role.md 越短 cache 越好);agent 通过 `claude --bg --agent <role>` 启动,prompt = role.md,orchestrator 只通过 env(`CCTEAM_INPUT`/`CCTEAM_OUTPUT`)告诉 artifact 路径。

---

## 4. 配置环境（关键扩展机制）

完整概览：`/en/features-overview`。

### 4.1 写好 CLAUDE.md

> 用 `/init` 命令生成起步骨架，再迭代。

CLAUDE.md 在每次会话开头加载。包括：Bash 命令、code style、workflow 规则。

**简洁原则**：每行问"删了它会不会让 Claude 出错？"——不会就删。**Bloated CLAUDE.md 会让 Claude 忽略真正重要的指令**。

| ✅ 应包括 | ❌ 不应包括 |
|---|---|
| Claude 猜不到的 Bash 命令 | 读代码就能搞清的 |
| 与默认不同的 code style | 标准语言惯例 |
| 测试指令 / 偏好的 test runner | 详细 API 文档（应放链接） |
| Repo 礼仪（branch 命名、PR 约定） | 频繁变化的信息 |
| 项目特有的架构决策 | 长篇解释或教程 |
| 开发环境怪癖（必需 env vars） | 文件级代码描述 |
| 常见 gotcha / 非显然行为 | 自明的"写干净代码"类废话 |

**调试 CLAUDE.md**：
- Claude 总是忽略某条规则 → 文件太长，规则被淹没 → 修剪
- Claude 反复问 CLAUDE.md 已写过的问题 → 表述歧义 → 改
- 加 "IMPORTANT" / "YOU MUST" 强调可提升遵从度

**导入语法**：
```markdown
@README.md      # 项目概览
@docs/git.md    # 子文档
@~/.claude/personal.md  # 个人覆盖
```

**位置**：
- `~/.claude/CLAUDE.md` — 全局
- `./CLAUDE.md` — 项目（入 git，团队共享）
- `./CLAUDE.local.md` — 个人项目笔记（gitignore）
- 父目录：monorepo 自动加载
- 子目录：按需加载

> **ccteam 映射**：本仓库的 CLAUDE.md 自身要遵守「精简」原则——能进 tech-design.md 就别进 CLAUDE.md。每项目自动生成的 `~/projects/<slug>/CLAUDE.md`（tech-design §6.5）也要按这套规则模板化。

### 4.2 配置 permissions

减少打断的三条路：

- **Auto mode**：分类器模型审命令，只拦风险（scope escalation、未知基础设施、敌意内容）。"信任方向但不想点每步"时用
- **Permission allowlists**：白名单具体工具（如 `npm run lint`、`git commit`）
- **Sandboxing**：OS 级隔离，限制文件系统 / 网络

参考：`/en/permission-modes`、`/en/permissions`、`/en/sandboxing`。

> **ccteam 映射**：tech-design §6.1「`--dangerously-skip-permissions` + 项目级容器」=「sandboxing + 全放行」组合——给最终用户产出的项目用；本仓库自己开发用 `bypassPermissions`，语义不同（参见 CLAUDE.md §六）。

### 4.3 用 CLI 工具

> 告诉 Claude 用 `gh`、`aws`、`gcloud`、`sentry-cli` 等 CLI 与外部服务交互——CLI 是最 context-efficient 的方式。教 Claude 学新 CLI:`Use 'foo-cli-tool --help' to learn about foo tool, then use it to solve A, B, C.`

> **ccteam 映射**:接 GitHub 时直接用 `gh` 而不是 raw API。

### 4.4 接 MCP

`claude mcp add` 接外部工具(Notion、Figma、数据库)。让 Claude 能从 issue tracker 读需求、查 DB、看监控、读设计、自动化 workflow。

> **ccteam 映射**:tech-design §6.4 列了推荐 MCP(telegram / claude-mem / playwright / github + 自建 ccteam-mcp 17 工具)。

### 4.5 设置 hooks

> **官方原话："Hooks are deterministic and guarantee the action happens."**

- CLAUDE.md 是 advisory（Claude 可能忽略）
- Hooks 是 deterministic（必然执行）

让 Claude 帮写 hook：「写一个 hook，每次文件编辑后跑 eslint」/「写一个 hook 阻止写 migrations 目录」。`.claude/settings.json` 直接编辑，`/hooks` 浏览。

> **ccteam 映射**:详见 interfaces.md §6 — progress.jsonl 写入、escalation 解析、危险命令拦截全是 hook,ccteam 可观测性的命脉。

### 4.6 创建 skills

`.claude/skills/<name>/SKILL.md`。Claude 自动在相关时应用,或 `/skill-name` 显式调用。frontmatter 支持 `disable-model-invocation: true`(有副作用时仅手动触发)。

> **ccteam 映射**:自带 skill 在 repo 根 `skills/`(`ccteam-control` / `ccteam-team-author` / `ccteam-project-creator` / `ccteam-creator`),meta-agent 操作 ccteam 的对话向导。

### 4.7 创建 subagent

`.claude/agents/<name>.md`：自有 context、自有 tool 集、不污染主对话。frontmatter:`name` / `description` / `tools` / `model`。显式调用:"Use a subagent to review this code for security issues."

> **ccteam 映射**:feature-dev / pr-review-toolkit 的 agent 通过 `enabledPlugins` 复用(详 tool-surface.md §6.2)。

### 4.8 装 plugins

`/plugin` 浏览市场。Plugin 打包了 skills + hooks + subagents + MCP。

类型语言项目装 [code intelligence plugin](https://code.claude.com/docs/en/discover-plugins#code-intelligence)：精准符号导航 + 编辑后自动错误检测。

> **ccteam 映射**:`claude-plugins-official` 仍是 ccteam 推荐的 agent / hook / skill 复用源;spawned project session 通过 `enabledPlugins` 启用 plugin。详 CLAUDE.md §3.7 + tool-surface.md §6.2。

### 何时用哪个扩展机制（决策参考）

参见 `/en/features-overview#match-features-to-your-goal`。

---

## 5. 高效沟通

### 5.1 问代码库问题

像问资深工程师一样问 Claude：

- "logging 怎么工作？"
- "怎么加新的 API endpoint？"
- "foo.rs 134 行的 `async move { ... }` 是干啥的？"
- "`CustomerOnboardingFlowImpl` 处理了哪些 edge case？"
- "为啥 333 行调 `foo()` 而不是 `bar()`？"

> Claude Code 是有效的 onboarding 工具，降低团队新人 ramp-up 时间。

### 5.2 让 Claude 反过来面试你

> 大需求时,让 Claude 用 `AskUserQuestion` 工具反向面试你。**官方推荐 prompt**:`I want to build [brief]. Interview me in detail using the AskUserQuestion tool. Ask about technical implementation, UI/UX, edge cases, concerns, and tradeoffs. Don't ask obvious questions, dig into the hard parts. Keep interviewing until we've covered everything, then write a complete spec to SPEC.md.`

完成后**起新会话**用 spec 实现——干净 context。

> **ccteam 映射**:反向面试由 **meta-agent** 承担——用户模糊需求时 meta-agent 用 `AskUserQuestion` 澄清,确认后再用 `mcp__ccteam__new` 起项目(详 `skills/ccteam-creator/`)。

---

## 6. 管理会话

### 6.1 早纠偏、勤纠偏

- **`Esc`**：中断 Claude，保留 context 重定向
- **`Esc Esc` / `/rewind`**：rewind 菜单——恢复对话或代码到任一 checkpoint
- **"Undo that"**：让 Claude 自己撤销
- **`/clear`**：不相关任务间清 context

> **如果同一问题纠正超过两次，context 已被失败方案污染——`/clear`，重写一个更具体的 prompt。**

### 6.2 主动管理 context

- 任务间 `/clear`
- auto compact 触发时 Claude 自动总结代码模式 / 文件状态 / 关键决策
- 更精细控制：`/compact <instructions>`，如 `/compact Focus on the API changes`
- 部分压缩：`Esc Esc` 选 checkpoint → "Summarize from here"
- CLAUDE.md 里加压缩偏好：`"When compacting, always preserve the full list of modified files and any test commands"`
- 临时问题用 `/btw`：答案在 dismissible overlay，**不进 context history**

> **ccteam 映射**:`/btw` 由 meta-agent 用 `mcp__ccteam__signal` 调,等价于"给某 agent 投递侧带消息";context reset 单位 = 每个 agent session,supervisor 在 `agent_done` 时决定回收。

### 6.3 用 subagent 做调查

> Subagent 在隔离 context 里探索,回主对话只带 summary——最强 context 节流手段。例:`Use subagents to investigate how our authentication system handles token refresh`;实现后 `use a subagent to review this code for edge cases`。

> **ccteam 映射**:CLAUDE.md §3.4 复用清单全靠这条——research / review 类工作必走 subagent。

### 6.4 Checkpoints 与 rewind

每次 Claude 动作前自动 checkpoint。`Esc Esc` / `/rewind` 打开菜单：恢复对话、代码、二者皆可，或从某点开始 summarize。**Checkpoint 跨会话保留**——关终端再开还能 rewind。

⚠️ **Checkpoint 只跟踪 Claude 自己的改动，不替代 git。**

### 6.5 续会话

- `claude --continue` 继续最近会话
- `claude --resume` 选会话
- `/rename oauth-migration` 命名会话——像 git branch 一样管理

> **ccteam 映射**：tech-design §6.1「极端情况——session 必须重启」走 `--resume` 全量恢复对话历史；普通的 60% reset 反而**不**用 `--resume`（目的是清空 context）。这两条要分清楚。

---

## 7. 自动化与规模化

### 7.1 非交互模式

```bash
claude -p "Explain what this project does"
claude -p "List all API endpoints" --output-format json
claude -p "Analyze this log file" --output-format stream-json
```

CI / pre-commit hook / 脚本里的标准用法。

> **ccteam 映射**：tech-design §6.1 明确**不用** `claude -p`——失去 attach、cache 反复冷启动。这是 ccteam 与一般 CC 用法的关键差异。但内部某些纯一次性查询（如 ccteam status MCP 的子查询）可以用。

### 7.2 多会话并行

- **Worktrees**：隔离 git checkout，多 CLI session 不冲突
- **Desktop app**：可视化管理多 session，每个一个 worktree
- **Claude Code on the web**：Anthropic 托管云上跑（VM 隔离）
- **Agent teams**：多 session 自动协调，共享任务 + messaging + team lead

并行还能解锁质量工作流。**Writer/Reviewer 模式**：

| Session A (Writer) | Session B (Reviewer) |
|---|---|
| Implement a rate limiter | （等） |
| （等） | Review @src/middleware/rateLimiter.ts. Look for edge cases, race conditions, consistency... |
| Here's the review feedback: [B output]. Address these issues. | |

测试反向：Session A 写测试 → Session B 写代码让测试过。

> **ccteam 映射**:Writer/Reviewer 模式 = workflow.yaml 写两个 role(`writer` 产 `drafts/` + `reviewer` `trigger: watch:drafts/` 产 `reviews/`),thin orchestrator 按 artifact-trigger 自动驱动。Agent Teams 概念已并入 workflow.yaml 拓扑。

### 7.3 跨文件 fan-out

大批量迁移 / 分析:`for file in $(cat files.txt); do claude -p "Migrate $file ..." --allowedTools "Edit,Bash(git commit *)"; done`。**`--allowedTools` 限制无人值守能力面。** 先在 2-3 个文件上调 prompt 再放量。也可 `claude -p "<prompt>" --output-format json | your_command` 接管道。

> **ccteam 映射**:M5「自动任务分解」(一句话需求 → 拆成 N 子项目)的底层执行模式。

### 7.4 Auto mode 自主跑

```bash
claude --permission-mode auto -p "fix all lint errors"
```

非交互（`-p`）模式下，分类器反复拦截 → auto mode 直接 abort（无人可降级）。阈值见 `/en/permission-modes#when-auto-mode-falls-back`。

---

## 8. 常见失败模式

| 模式 | 症状 | 对策 |
|---|---|---|
| **The kitchen sink session** | 切换无关任务，context 充满无关信息 | `/clear` 切任务 |
| **Correcting over and over** | 反复纠正，context 被失败方案污染 | 2 次失败纠正 → `/clear` + 重写 prompt |
| **The over-specified CLAUDE.md** | 文件太长，重要规则被淹没 | 修剪。能不写就不写 |
| **The trust-then-verify gap** | 看起来对、edge case 崩 | **必须**给验证手段 |
| **The infinite exploration** | 让 Claude "调研"未限范围，读爆 context | 限范围 + 用 subagent |

> **ccteam 映射**:5 条对应 requirements.md 痛点 4–7。工程化对策:每 agent 独立 session(根治 kitchen-sink)、3-strike escalate → meta-agent / 用户(根治 correcting-over)、workflow.yaml 极简无 prompt(根治 over-specified)、`.claude/agents/*.md` 显式列 acceptance(根治 trust-then-verify)、artifact-trigger 限定 watch 路径(根治 infinite exploration)。

---

## 9. 培养直觉

> 本指南的模式是起点不是教条。

什么时候反着来：
- 深陷一个复杂问题、history 有价值 → 让 context 累积
- 任务是探索性的 → 跳过 plan
- 想看 Claude 怎么解读 → 用模糊 prompt

**注意 Claude 表现好 / 差的差异**：好时记住 prompt 结构、context、模式；差时问为什么——context 太杂？prompt 太糊？任务太大？

> **ccteam 映射**：requirements §五「成功判定」与本节同源——产品成败不看代码行数 / agent 数量，看用户参与时间、并行项目数、跨项目沉淀效果。

---

## 10. 相关资源

- [How Claude Code works](https://code.claude.com/docs/en/how-claude-code-works) — agentic loop、tools、context 管理
- [Extend Claude Code](https://code.claude.com/docs/en/features-overview) — skills、hooks、MCP、subagents、plugins
- [Common workflows](https://code.claude.com/docs/en/common-workflows) — debugging、testing、PR 等具体配方
- [CLAUDE.md](https://code.claude.com/docs/en/memory) — 项目约定与持久 context
- 官方文档索引：https://code.claude.com/docs/llms.txt

---

## 附录 A：ccteam 设计决策 → 最佳实践对照速查

| ccteam 设计 | 来自最佳实践哪条 |
|---|---|
| `workflow.yaml` agent 拓扑 + artifact-trigger | §2 先探索再规划再编码 |
| 测试即验收 / role.md 写产物验证 | §1 给验证方法 |
| `@文件引用` 注入 role.md(越短 cache 越好) | §3 @ 引用文件 |
| 项目级 CLAUDE.md 自动生成 | §4.1 + §6.2 压缩偏好 |
| `--dangerously-skip-permissions` + 容器 | §4.2 sandboxing |
| progress.jsonl + business-event hook | §4.5 hooks deterministic |
| 复用 plugin 的 agent / hook / skill via `enabledPlugins` | §4.7 + §4.8 |
| meta-agent 调 `signal` 投递侧带消息 | §6.2 `/btw` 不进 history |
| `claude --bg --agent` + supervisor | §7.2 parallel sessions worktrees |
| meta-agent CLARIFY 反向面试 | §5.2 让 Claude 反向面试 |
| 3-strike escalate + meta-agent | §8 correcting over and over |

---

**维护**：本文件半年抓一次（见顶部抓取时间），或在 ccteam 触发"为啥跟最佳实践不一致"的争议时回链查证。
