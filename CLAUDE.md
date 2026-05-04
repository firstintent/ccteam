# CLAUDE.md — ccteam 实现导引

> 本文档面向**未来开干 ccteam 实现的 Claude 实例**。项目当前**只有设计文档,无代码**。

## 阅读顺序

1. `docs/requirements.md` — 为什么做、为谁做(10 条用户痛点是 PR 验收唯一标准)
2. `docs/tech-design.md` — 怎么做(架构红线、phase 协议、文件协议——已固化,**不要重新选型**)
3. `docs/development-plan.md` — 何时做什么(里程碑任务、依赖图、痛点反向映射、验收门——PR 必须挂某条任务编号)
4. `docs/claude-code-best-practices.md` — Claude Code 官方最佳实践本地副本(下面 §三所有扩展机制都引用此文件具体章节)
5. 本文件 — 用哪些扩展机制实现 ccteam,以及具体复用哪些 `claude-plugins-official` 件

---

## 一、定位:ccteam 是 Claude Code 之上的元工具

**ccteam 不是独立 AI 系统,它是 Claude Code 的编排层。**

- **被编排者**:每个项目一个 Claude Code 长 session,用 tmux 守护、用 hooks 上报、用 MCP 接外部
- **编排者**:Python orchestrator(M0)、Telegram bot(M1)、跨项目记忆 RAG(M3)
- **结论**:**严格遵守 Claude Code 最佳实践会让 ccteam 系统更高效**——任何与最佳实践冲突的设计选择必须有 tech-design.md 中的明确反对论证。两个层面都适用:
  - **开发 ccteam 本身时**(本仓库):每次写代码、写 prompt、改架构都先翻 `docs/claude-code-best-practices.md`
  - **ccteam 运行时编排下游 Claude**(产出项目):phase 协议、注入策略、context 管理都要落到最佳实践原则上

当出现"按最佳实践应该 X 但 ccteam 设计了 Y"时,这是值得 escalate 的设计冲突——回 `docs/tech-design.md` 找论证,找不到就不要做 Y。

---

## 二、不可触碰的架构红线

来自 `docs/tech-design.md`,任何 PR 不得违反。每条都标了对应章节以便回查论证:

- **tmux 长 session,不用 `claude -p`**——cache 复用 + 随时 attach + detach 即守护(§2.2、§6.1;最佳实践 §7.2 worktrees 模式的工程化)
- **文件系统是控制平面**——不接 Linear/GitHub Issues 作状态源(§2.2)
- **`progress.jsonl` 是 orchestrator 唯一状态事实来源**——不解析 tmux 终端输出(§5.5、§6.8;最佳实践 §4.5 hooks deterministic)
- **默认 1M context,超 60% 在 phase 边界 reset**——`/exit` + 新 session + CLAUDE.md 桥接,**不**用 `--resume`(§6.9;最佳实践 §6.2 主动管理 context 的延伸)
- **idle-aware 注入**:`Stop`/`idle_prompt` 后 send-keys;忙时用 `/btw`(§6.9;最佳实践 §6.2 `/btw` 不进 history 是核心机制依赖)
- **永不主动 kill 长 session**——只软告警(5/15/30 min 三档);唯一例外:项目累计 cost > $200 物理上限(§6.8)
- **`--dangerously-skip-permissions` + 项目级容器**——产出的项目专用,**不**等同于本仓库的 `bypassPermissions`(§6.1;最佳实践 §4.2 sandboxing)
- **fix-loop 撞 3 次顶必 escalate,绝不静默重置**(§3.5;最佳实践 §8「correcting over and over」工程化)

---

## 三、Claude Code 扩展机制 → ccteam 组件映射

每种扩展机制对应到 ccteam 的具体设计。**不要发明新机制**——先确认现有机制装不下,再考虑新东西。

### 3.1 CLAUDE.md(持久上下文)

- **本文件**:开发 ccteam 时给 Claude 看
- **每项目自动生成**:`~/projects/<slug>/CLAUDE.md`(tech-design §6.5)——orchestrator 在 plan phase 后写入
  - 内容:slug、当前 phase、plan-eng 摘要、跨项目记忆召回的 top-3 patterns
  - **Context reset 桥梁**(§6.9):reset 前把当前进度追加到这个文件的"当前进度"节,新 session 启动自动加载
- **遵守最佳实践 §4.1 简洁原则**:CLAUDE.md 越长越被忽略;能进 tech-design.md 就别进 CLAUDE.md

### 3.2 Skills(可复用知识与可调用工作流)

- `~/.claude/skills/ccteam-phases/`(tech-design §6.7)——9 个 phase 模板打包成 skill,作为非守护进程模式 fallback
- 已存在的 `.claude/skills/ccgram-messaging/SKILL.md`——agent-to-agent 通讯能力,M3+ 多 orchestrator 协作时复用
- **Skill vs Plugin 边界**:phase prompt 是 skill(纯文本知识);带 hook+agent+命令的整套是 plugin

### 3.3 MCP(连接外部服务)

tech-design §6.4 已列,按里程碑装:

| MCP | 里程碑 | 用途 |
|---|---|---|
| Telegram bot | M1 | 异步消息入口 + escalation 推送 |
| claude-mem | M3 | 跨项目向量记忆索引 |
| Playwright | 按需 | 前端 E2E,phase 内调用 |
| GitHub | M4+ | PR 创建、issue 同步(优先 `gh` CLI——见最佳实践 §4.3) |

**ccteam 自身不写 MCP server,但应暴露 `ccteam status <slug>` 类查询作为 MCP**——让项目内 Claude 能查"我在哪个 phase / 累计 cost"。

### 3.4 Subagents(隔离上下文,返回总结)

最佳实践 §6.3 的核心 context 节流手段。**不要从零写 subagent**——`claude-plugins-official` 已有现成的(完整路径见 §3.7):

| ccteam 用途 | 直接复用 |
|---|---|
| Plan-Eng 阶段架构方案对比 | `feature-dev/agents/code-architect.md`(产出 3 方案 + 推荐) |
| 项目延续场景的代码库探索 | `feature-dev/agents/code-explorer.md`(并行多个,trace 调用链) |
| Review phase | `pr-review-toolkit/agents/{code-reviewer,silent-failure-hunter,pr-test-analyzer,type-design-analyzer,comment-analyzer}.md` 全 6 个 |
| Review 后简化打磨 | `code-simplifier/agents/code-simplifier.md` |

调用形态:phase prompt 用 `Task` 工具显式 launch,带 `subagent_type`;**并行多个 agent 时单条消息发多 tool call**(最佳实践 §7.2 Writer/Reviewer)。

### 3.5 Agent Teams(同 session 内并行多 agent)

- **何时启用**:phase 内并行(如 implement 同时 backend-dev + frontend-dev),**不是**全局调度。Lead 角色仅限 phase 内——全局调度永远是 orchestrator(tech-design §2.2)
- **如何启用**:`.claude/settings.json` 的 `env` 段设 `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1`(§6.1、§6.3);phase markdown front matter `agent_team:` 列角色
- **跨 session 协作**(M3+):用 `ccgram-messaging` skill 走 inter-agent message bus

### 3.6 Hooks(生命周期触发——ccteam 可观测性命脉)

最佳实践 §4.5「hooks 是 deterministic 的」原则,在 ccteam 体现为「progress.jsonl 是状态唯一事实来源」。完整结构 tech-design §6.2:

| Hook 事件 | ccteam 用途 | 复用来源 |
|---|---|---|
| `SessionStart` | 写 ready 标记 + append `session_start` 到 progress.jsonl | 自写 |
| `Stop` | 解析最后一行 `PHASE_DONE: <name>`/`ESCALATE: <reason>`;append `Stop`(idle 信号) | 自写 |
| `Stop`(fix-loop 模式) | **关键复用**:`ralph-loop/hooks/stop-hook.sh` 的拦截退出 + 重喂 prompt 范式直接抄,作为 fix-cycle 实现 | `ralph-loop/hooks/` |
| `Notification:idle_prompt` | 同样作为 idle 信号 | 自写 |
| `PreToolUse`(通用) | append 工具调用事件;活跃信号(stall 反向判断) | 自写 |
| `PreToolUse`(`Edit\|Write\|MultiEdit`) | 安全模式扫描(GH Actions 注入、secrets) | **直接挂 `security-guidance/hooks/security_reminder_hook.py`** |
| `PostToolUse`(通用) | append + 累加 `cost_used_usd` 与 `context_tokens_used` 到 state.json(§6.9 60% 阈值) | 自写 |
| `PostToolUse`(`Bash:git push.*`) | 拦截危险命令 | 自写 |
| `SubagentStop` | append 事件 | 自写 |
| `SessionEnd` | append——orchestrator 据此判断"reset 完成 vs crash" | 自写 |

**Hook 写作纪律**:
- append 类必须 `async: true`——别拖慢主流程
- 解析 `PHASE_DONE`/`ESCALATE` 的 hook 设 `timeout: 10`,失败要落日志
- hook 脚本放 `~/.ccteam/hooks/`,不放项目目录(避免被 claude 自己改)

### 3.7 Plugins / Marketplaces(选择性调用,不全装)

**默认路径**:`~/.claude/plugins/marketplaces/claude-plugins-official/plugins/`

本机已缓存以下相关 plugin(2026-05 抓取):
```
agent-sdk-dev          claude-md-management   feature-dev          plugin-dev
clangd-lsp             code-modernization     frontend-design      pr-review-toolkit
claude-code-setup      code-review            hookify              ralph-loop
                       code-simplifier        mcp-server-dev       security-guidance
                       commit-commands        plugin-dev           session-report
                                              skill-creator        ……
```

**核心原则:plugin 是参考实现,不是依赖,选择性调用**

不要把所有 plugin 全装到产出项目里——三种调用粒度按需选:

| 粒度 | 何时用 | 怎么用 |
|---|---|---|
| **直接 `@文件引用`**(零安装) | phase 模板里只用某个 agent 或 hook 脚本 | `@~/.claude/plugins/marketplaces/claude-plugins-official/plugins/feature-dev/agents/code-architect.md`——phase prompt 里 inline 引用,Claude 读后即用 |
| **拷贝到项目**(冻结版本) | 该组件在 ccteam 协议下需要小改(如换 progress.jsonl 路径) | `cp` 到 `~/projects/<slug>/.claude/agents/` 后改;**不在原位改**(会污染缓存) |
| **整 plugin 安装**(完整能力) | 项目要用某 plugin 的所有 commands/agents/hooks/MCP | `/plugin install <name>@claude-plugins-official`——M2/M3 才考虑 |

**ccteam 自身最终也是 plugin**:`.claude-plugin/plugin.json` + `commands/` + `agents/` + `hooks/` + `skills/ccteam-phases/` + `.mcp.json`(注册 ccteam status 查询)。M2/M3 的事,M0 先跑通本地脚本。

**实现 phase 模板时的检查清单**:
1. 在 `~/.claude/plugins/marketplaces/claude-plugins-official/plugins/` 下 grep 看有没有现成
2. 有 → 用 `@文件引用`,别复制内容
3. 没有 → 才自写;自写完想想能不能贡献回 plugin 形态

---

## 四、目录现状与 M0 待建

```
ccteam/
├── docs/
│   ├── requirements.md          ✅ 已定稿
│   ├── tech-design.md            ✅ 已定稿(架构红线 + 协议)
│   ├── development-plan.md       ✅ 任务级开发计划(M0 active)
│   └── claude-code-best-practices.md   ✅ 官方最佳实践本地副本
├── .claude/
│   ├── settings.json             ✅ 全权限放行(bypassPermissions),开发本仓库专用
│   └── skills/
│       └── ccgram-messaging/     ✅ 已装(M3+ 用)
├── LICENSE                       ✅
└── (M0 待建)
    ├── orchestrator/             ← Python asyncio 主循环
    ├── phases/                   ← 9 个 phase markdown(00-seed.md … 09-ship.md)
    ├── hooks/                    ← progress-append.sh / parse-phase-end.sh / cost-accumulate.sh
    ├── cli/                      ← ccteam new / status / attach / resume 等
    └── tmux/                     ← layout 模板
```

M0 完整任务清单见 `docs/development-plan.md` §2(任务编号 M0.1–M0.15、依赖、可执行验收)。**勾选标准**:每条都能映射回 requirements §二某条痛点。

---

## 五、PR / 实现纪律

1. **每个 PR 必须能映射到三件事**:
   - `requirements.md` §二的某条痛点(写 PR 描述,例:`痛点 4`)
   - `tech-design.md` 某章节(说明对应组件,例:`tech-design §3.5`)
   - `development-plan.md` 某条任务(例:`Closes M0.12`)
   - 三个都不能映射 → backlog,不合主线(tech-design §11)
2. **commit message 用英语**——与现有 `prd & tech docs` / `Initial commit` 风格一致(文档与 phase prompt 用中文)
3. **不写 backwards-compat shim**——M0 没有"老版本",任何向后兼容代码都是过度设计
4. **优先编辑现有文件,不轻易新建**——尤其 phase 模板,先看 `claude-plugins-official/feature-dev/commands/feature-dev.md` 能不能直接 `@引用`(§3.7 检查清单)
5. **测试不过不算完成**——ccteam 自己产品要兑现的承诺,自己开发也得遵守(最佳实践 §1)
6. **大需求时让 Claude 反向面试自己**(最佳实践 §5.2)——别让自己手写完整 spec

---

## 六、易踩的坑

- **不要给 ccteam 自己加 ccteam 风格的 hook/orchestrator**——循环引用导致排错地狱。本仓库用 Claude Code 默认行为开发,只有产出物(`~/projects/<slug>/`)才挂 ccteam hook
- **`.claude/settings.json` 的 `bypassPermissions` 是开发态便利**——产品形态是 `--dangerously-skip-permissions` + 容器隔离,语义不同(最佳实践 §4.2 三选一)
- **phase prompt 别写太长**——单条 send-keys 装得下;复杂内容用 `@文件引用`(最佳实践 §3「@ 引用文件」)
- **`claude-plugins-official` 是参考实现,不是依赖**——别 vendor 一份;实现时按 §3.7 三种粒度选合适的
- **本文件不超过 250 行**——CLAUDE.md 越长 cache 越贵,Claude 越忽略(最佳实践 §4.1 + §8「over-specified CLAUDE.md」)
