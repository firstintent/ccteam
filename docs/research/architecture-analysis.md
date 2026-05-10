# ccteam 架构分析文档

面向读者：架构师、技术负责人、核心贡献者  
分析对象：当前仓库实现与随仓文档，重点参考 `README.md`、`docs/tech-design.md`、`docs/interfaces.md` 以及 `crates/*` 源码  
分析日期：2026-05-10

---

## 1. 执行摘要

ccteam 是一个构建在 Claude Code 之上的自治项目编排系统。它不是把 LLM 嵌入为服务端智能内核，而是把 Claude Code 作为外部执行会话，通过 Rust 编写的守护进程、tmux 长会话、文件系统协议和 Claude Code hooks 组成一个可恢复、可观测、可扩展的项目流水线。

从架构视角看，项目的核心设计是：

1. **Rust orchestrator 负责确定性编排**：状态机、phase DAG、team 解析、tmux 生命周期、stall/cost/watchdog 等能力集中在 `ccteam-core`。
2. **Claude Code 负责非确定性工作执行**：需求理解、设计、编码、测试修复、研究判断等由 project session 或 meta-agent session 完成。
3. **文件系统是控制面和状态事实源**：`state.json`、`progress.jsonl`、inbox/outbox、team.yaml、phase markdown 构成协议层。
4. **tmux 长会话是执行运行时**：每个项目一个长期 Claude Code 会话，phase 之间复用上下文和 prompt cache。
5. **hooks 是 LLM 会话到 orchestrator 的回写通道**：Claude Code hook 事件被转换为 progress 事件、cost 信息、phase_done/escalate 终止信号和用户决策 outbox。
6. **CLI / MCP / Web 是适配层**：都围绕 `ccteam-core` 暴露控制面，不承担核心状态机职责。

架构整体符合“LLM 做判断与生成，传统程序做边界、状态和恢复”的原则。主要风险集中在文件协议一致性、长会话漂移、hook 语义对 Claude Code 的外部依赖、以及未来 Web/多会话/插件生态扩展时的并发写入治理。

---

## 2. 项目定位

### 2.1 产品目标

ccteam 的目标是把“一句话需求”转化为一个可长期运行、可自动推进、必要时才请求用户介入的项目执行流程。内置团队包括：

| 团队 | 目标 | 典型产物 |
|---|---|---|
| `dev` | 软件项目交付 | 计划、实现、测试、修复、交付报告 |
| `product-research` / `research` | 产品或主题研究 | 市场/可行性/差异化分析、结论 |
| `meta-agent` | 常驻自然语言调度入口 | 派单、查询、决策协调 |
| 用户自定义 team | 自定义 phase pipeline | Claude Code plugin / team.yaml / phase markdown |

### 2.2 非目标

当前架构明确避免以下方向：

- 不在 Rust orchestrator 内嵌 LLM 推理。
- 不用数据库作为主状态存储。
- 不把 Telegram/Slack/Feishu 等 channel 做成业务智能层。
- 不通过解析 tmux 屏幕输出来判断项目状态。
- 不自行实现通用 RAG，跨项目记忆优先复用 Claude Code 官方规则/记忆机制。

---

## 3. 总体架构

### 3.1 逻辑分层

```text
┌─────────────────────────────────────────────────────────────┐
│ Channel Layer                                                │
│ Telegram / Slack / Feishu / Web / daily-driver Claude        │
│ 职责：把外部消息路由到 inbox/outbox 或 MCP/CLI 控制面          │
└───────────────────────────────┬─────────────────────────────┘
                                │
┌───────────────────────────────▼─────────────────────────────┐
│ User Interaction Layer                                        │
│ meta-agent session + project Claude Code sessions             │
│ 职责：自然语言理解、项目执行、phase 内部判断、用户决策协商      │
└───────────────────────────────┬─────────────────────────────┘
                                │
┌───────────────────────────────▼─────────────────────────────┐
│ Orchestration Layer                                           │
│ Rust daemon + filesystem protocol + hooks + tmux wrapper      │
│ 职责：状态机、phase 调度、恢复、告警、成本/停滞检测             │
└─────────────────────────────────────────────────────────────┘
```

### 3.2 进程视图

```text
用户终端 / Claude MCP client
        │
        ├── ccteam CLI：new / ls / show / resume / doctor / web / mcp-serve
        │
        ├── ccteam start：orchestrator daemon
        │       ├── notify watcher：监听 ~/.ccteam/progress/
        │       ├── 30s tick loop：扫描项目状态、调度 phase
        │       └── tmux wrapper：创建/注入/探测 Claude Code session
        │
        ├── tmux session：ccteam-<slug>
        │       └── claude --dangerously-skip-permissions --model ...
        │
        └── Claude Code hooks
                └── ccteam hook <progress|parse-phase-end|cost|...>
```

### 3.3 数据视图

主要持久化数据分为三类：

| 类别 | 路径 | 作用 |
|---|---|---|
| 全局控制面 | `~/.ccteam/` | team、phase、progress、control、state、log |
| 项目工作区 | `~/projects/<team>-<slug>/` | 代码、项目级 `CLAUDE.md`、`.ccteam/state.json`、产物 |
| Claude Code 集成面 | `~/.claude/` / 项目 `.claude/` | skills、MCP 配置、rules、settings.json hooks |

架构上，`progress.jsonl` 与项目级 `state.json` 是最关键的两个状态事实：

- `progress.jsonl`：hook 追加的事件流，表达 Claude Code 会话中发生了什么。
- `state.json`：orchestrator 可恢复的项目状态快照，表达系统认为项目处于哪个 phase、是否可调度、成本/上下文/暂停等状态。

---

## 4. Workspace 与模块划分

仓库是 Rust workspace，包含四个 crate：

| Crate | 类型 | 责任 |
|---|---|---|
| `ccteam-core` | library | 编排内核、状态协议、team/phase 解析、tmux、watchdog、项目引导、MCP/Web 共用 action |
| `ccteam-cli` | binary | CLI 入口、daemon 启动、hook 子命令分发、MCP stdio server、Web server 启动 |
| `ccteam-hooks` | library | Claude Code hook handler：progress、phase end 解析、cost、AskUserQuestion 拦截等 |
| `ccteam-web` | library | V0.3 Web UI scaffold，目前提供 axum router 与 `/health` |

### 4.1 `ccteam-core`

`ccteam-core` 是架构中心。它导出大量跨适配层共享的协议和能力，典型模块如下：

| 模块 | 职责 |
|---|---|
| `orchestrator.rs` | 主循环、tick 决策、phase 调度、progress 消费、stall/cost/auto-loop 处理 |
| `state.rs` | 项目 `state.json` schema、原子保存、`.bak` 恢复 |
| `team.rs` | `team.yaml` schema，team 级 golden rules、retro schema、cost policy 等 |
| `team_resolver.rs` | project/user/repo 三层 team 解析 |
| `phases.rs` / `dag.rs` | phase markdown frontmatter 与 DAG 推导 |
| `projects.rs` | 项目目录 bootstrap、slug 生成、信任设置 |
| `tmux.rs` | tmux session 封装、pane capture、pid 探测 |
| `actions.rs` | pause/resume/send/inject 等可被 CLI/MCP/Web 复用的写操作 |
| `watchdog.rs` / `stall.rs` / `cost.rs` | 观测与告警分类 |
| `templates.rs` | 内置 team/phase/helper/settings 模板落盘 |
| `tool_surface.rs` / `plugin_resolution.rs` | Claude Code skills/agents/plugins/MCP 依赖面检查 |
| `screenshot.rs` | tmux pane 到 PNG 的纯 Rust 渲染链路 |

### 4.2 `ccteam-cli`

`ccteam-cli` 是二进制入口，核心职责是把用户命令翻译成 `ccteam-core` 操作。它包含：

- 项目生命周期：`new`、`ls`、`show`、`resume`、`attach`、`peek`、`progress`
- 系统生命周期：`init`、`start`、`stop`、`doctor`
- 集成面：`mcp-serve`、`web`
- 团队管理：`team init/publish/validate`
- Hook 分发：`hook ...`

架构上 CLI 不应拥有独立业务状态。当前实现基本符合这个边界，`commands.rs` 中的写操作也已逐步下沉到 `ccteam_core::actions`，便于 MCP/Web 复用。

### 4.3 `ccteam-hooks`

hooks 是从 Claude Code runtime 到 ccteam 状态平面的桥：

| Hook handler | 作用 |
|---|---|
| `progress_append` | 记录 Claude Code 生命周期事件 |
| `parse_phase_end` | 从输出中解析 `phase_done` / `escalate` / `phase_done_pending` |
| `cost_accumulate` | 累积成本事件 |
| `intercept_ask_decision` | 拦截 `AskUserQuestion` 并写入结构化决策 outbox |
| `load_context` | session 启动时加载上下文 |
| `transcript` | transcript 相关辅助 |

该 crate 共享 `ccteam-core` schema，降低 hook 写出的事件与 orchestrator 读取逻辑不一致的概率。

### 4.4 `ccteam-web`

Web crate 当前处于 V0.3 M5.0 scaffold：

- 使用 `axum`。
- 现阶段仅提供 `/health`。
- `ServeOpts` 已预留 `no_auth`、`token_file`，为后续受限写操作和 token auth 稳定接口形状。

从架构角度，Web 应继续作为薄适配层，复用 `ccteam_core::actions`，避免复制 CLI 的写入逻辑。

---

## 5. 核心运行流程

### 5.1 创建项目

```text
ccteam new "<request>" --team dev
        │
        ├── 确认 team 可解析
        ├── 决定 slug
        ├── bootstrap_project()
        │       ├── 创建 ~/projects/<team>-<slug>/
        │       ├── 写入 .ccteam/spec.md
        │       ├── 写入 .ccteam/state.json
        │       ├── 写入项目 CLAUDE.md / .claude/settings.json
        │       └── 初始化 git / ignore / hooks 配置
        └── 返回 slug
```

slug 策略有多级：

1. 用户显式 `--slug`。
2. 可选 Claude Code smart suggestion。
3. 确定性 `slugify_brief()` fallback。

### 5.2 Orchestrator 调度

```text
ccteam start
        │
        ├── 加载 ~/.ccteam/teams/<name>/team.yaml
        ├── 加载 phase markdown 并构建 DAG
        ├── 检查 tool surface
        ├── 恢复已有项目 state.json 和 tmux session
        └── tick loop
                ├── 读取 progress 最近终止事件
                ├── decide_tick()
                │       ├── InFlight + phase_done → AdvancePhase
                │       ├── InFlight + escalate → Escalated
                │       ├── Idle → DispatchPhase
                │       └── terminal / DonePending → NoOp
                ├── 必要时 tmux send-keys 注入 phase prompt
                ├── 必要时保存 state.json
                └── 处理 stall/cost/pending inject/inbox
```

`decide_tick_from_events()` 是关键纯函数。它让状态迁移逻辑可测试，也把副作用留在 `process_project` 等外层流程。

### 5.3 Phase 执行闭环

```text
orchestrator 注入 phase prompt
        │
        ▼
Claude Code session 执行 phase markdown
        │
        ├── 写代码 / 调工具 / 调 subagent
        ├── hooks 持续追加 progress/cost
        └── 最终输出结构化终止信号
                ├── PHASE_DONE
                ├── ESCALATE
                └── PHASE_DONE_PENDING
        │
        ▼
parse_phase_end hook 写 progress.jsonl
        │
        ▼
orchestrator 下一 tick 推进状态
```

这一设计的关键点是：orchestrator 不理解 phase 产物内容，只识别少量结构化终止事件和 team/phase 声明的 required IO。

### 5.4 用户决策闭环

```text
Claude Code 想问用户
        │
        ├── AskUserQuestion 被 hook 拦截
        ├── 写入 .ccteam/outbox/*.md
        ├── project 进入 pending / escalation / done_pending
        ├── meta-agent 或用户通过 CLI/MCP 注入决策
        └── orchestrator 恢复调度
```

这使“用户不在线”成为正常状态，而不是异常状态。

---

## 6. 关键协议

### 6.1 `ProjectState`

项目状态 schema 位于 `crates/ccteam-core/src/state.rs`。关键字段：

| 字段 | 含义 |
|---|---|
| `slug` / `team` | 项目标识与 team 路由 |
| `tmux_session` / `claude_pid` / `claude_session_id` | 运行时会话绑定 |
| `phase_state` | `idle` / `in_flight` / `auto_locked` / `done_pending` |
| `current_phase` | 当前 phase |
| `parallelism` | `solo` / `agent_team` / `multi_session` |
| `phase_history` | 已完成 phase 记录 |
| `cost_used_usd` | 成本累计 |
| `context_tokens_used` / `context_reset_count` | 上下文管理 |
| `user_pause_pending` | 用户暂停调度 |

持久化使用“写临时文件 + rename”的原子写入策略，并保留一层 `.bak`，这是文件状态机架构的关键可靠性措施。

### 6.2 `TeamSpec`

`team.yaml` schema 位于 `crates/ccteam-core/src/team.rs`。它让团队扩展主要变成配置问题：

| 能力 | 说明 |
|---|---|
| `phase_dir` | 指向 phase markdown 目录 |
| `retro_schema` | 控制跨项目经验沉淀结构 |
| `golden_rules` | protocol/domain 两类规则，支持命令检查或 prompt directive |
| `critic_dimensions` | 为后续 critic/scorecard 留数据结构 |
| `escalate_grammar_extensions` | team 自定义 escalate 语法 |
| `cost_policy` | 常规项目与 evergreen team 的成本策略差异 |
| `evergreen` | meta-agent 等常驻 team 不走常规 phase DAG |

### 6.3 Team 解析优先级

解析顺序是：

```text
1. Project: <project_dir>/.ccteam/team/team.yaml
2. User:    ~/.config/ccteam/teams/<name>/team.yaml
3. Repo:    ~/.ccteam/teams/<name>/team.yaml
```

这是“整团替换”，不是字段级 merge。该策略简单、可解释，但要求自定义 team 作者维护完整 team 定义。

### 6.4 `progress.jsonl`

`progress.jsonl` 是 hook 追加事件流。orchestrator 从中寻找与当前 phase 匹配的终止事件：

- `phase_done`
- `phase_done_pending`
- `escalate`

由于 Claude Code 可能在 `Stop` 后追加 `SubagentStop`，当前实现不是只看最后一条事件，而是在近期事件切片中找“当前 phase 的最新终止事件”。这是对真实 hook 序列的必要适配。

---

## 7. 架构质量属性分析

### 7.1 可恢复性

优势：

- 项目状态和事件流落在文件系统，可进程重启恢复。
- tmux 会话独立于 orchestrator 存活。
- `state.json` 原子写入并保留 `.bak`。
- `ccteam start` 可重新扫描并接管已有项目。

风险：

- 文件系统协议分散在多个目录，缺少统一事务边界。
- progress 事件追加与 state 保存之间可能存在短暂不一致。
- 如果 hook 未执行或 Claude Code hook schema 变化，orchestrator 可能长时间停在 `in_flight`，只能由 stall/watchdog 兜底。

### 7.2 可观测性

优势：

- `ccteam show`、`progress`、`peek`、`attach` 覆盖状态、事件、终端画面。
- watchdog 把 daemon down、cost、phase duration、needs_attention 等异常转换为 meta-agent 通知。
- screenshot 管线提供终端画面 PNG，方便 Web 或远程通道展示。

改进空间：

- 目前 observability 主要是面向人读的 CLI/文件；未来 Web/SSE 需要统一 projection model。
- 建议为事件类型建立更严格的版本化 schema，避免后续 UI 依赖弱 JSON 约定。

### 7.3 扩展性

优势：

- team/phase 插件化，新增团队不需要改 orchestrator 主逻辑。
- CLI/MCP/Web 都能复用 `ccteam-core`。
- Channel layer 被设计为 dumb router，避免外部 IM 适配器侵入业务。

风险：

- `ccteam-core` 目前承担职责较多，长期可能变成“协议、运行时、模板、运维工具”的大包。
- `commands.rs` 中仍有部分 CLI 逻辑较厚，Web 写操作上线后需要继续下沉到 `actions.rs`。
- 多 session 并发扩展会放大文件锁、状态合并、跨模块接口契约校验的复杂度。

### 7.4 安全性

优势：

- Web 默认绑定 loopback。
- 后续 token auth 字段已预留。
- tool surface doctor 能检查 Claude Code skills/agents/MCP 配置。

显著风险：

- 项目会话默认使用 `claude --dangerously-skip-permissions`，安全边界主要依赖项目目录、用户环境和 Claude Code 配置。
- phase 允许模型执行命令，属于高信任本地自动化系统。
- MCP 暴露写操作，必须严格控制可调用客户端和参数验证。
- 自定义 team/plugin 如果来自不可信来源，可能把危险指令写入 phase markdown 或 settings。

建议：

- Web 非 loopback 绑定时强制 token auth，而不是仅 warning。
- MCP 写操作维持最小能力集，并加入审计日志。
- 对 team/plugin 增加 “dangerous capability manifest” 或 doctor 风险提示。

### 7.5 性能与成本

优势：

- tmux 长会话复用 Claude Code 上下文和 prompt cache，避免每个 phase 冷启动。
- Rust daemon 开销低。
- notify watcher + tick loop 结合，避免纯轮询。

风险：

- 长 session 上下文膨胀可能导致漂移、成本上升或上下文上限风险。
- 多项目并行默认硬编码为 3，后续需要配置化和基于资源/成本的调度策略。
- hook 事件过多时，progress JSONL 读取和切片搜索需要关注上限策略。

---

## 8. 关键架构决策评价

### 8.1 使用文件系统作为控制面

评价：适合本地开发者工具。它降低部署复杂度，方便调试和恢复，也符合 tmux/Claude Code 本地运行模式。

代价：

- 缺少天然事务和并发控制。
- schema 演进依赖兼容代码和文档纪律。
- 多进程同时写同一路径时需要非常明确的 ownership。

### 8.2 使用 tmux 长会话

评价：这是项目成立的核心决策。它让用户可以 attach 观察或干预，也让 Claude Code 会话跨 phase 延续。

代价：

- 会话状态部分存在 Claude Code/tmux 内部，不能完全由 Rust 复现。
- 长时间运行后需要 context reset 和 session health 策略。
- 自动注入 prompt 依赖 TUI 就绪时机，因此实现中存在 ready marker 与 warmup。

### 8.3 Orchestrator 不内嵌 LLM

评价：正确。Rust 层只做确定性编排，降低不可测性和安全风险。

代价：

- 复杂语义判断都要通过 Claude Code session 或 phase prompt 表达。
- 当 LLM 没有遵循结构化协议时，系统需要 hook/auto-loop/stall 兜底。

### 8.4 Team/phase 配置化

评价：这是产品扩展性的核心。team.yaml + phase markdown 让工作流可配置、可发布、可复用。

代价：

- 配置本身变成“代码”，需要 validate、lint、版本化、兼容策略。
- phase markdown 中的协议 literal 容易和 Rust schema 脱节。

---

## 9. 主要风险清单

| 风险 | 影响 | 当前缓解 | 建议 |
|---|---|---|---|
| Claude Code hook schema 或行为变化 | phase 无法推进或误推进 | hook crate + tests + stall | 给 hook payload 建兼容层和 fixture 回归集 |
| `state.json` 与 `progress.jsonl` 不一致 | 状态显示错误、重复注入 | 原子写、terminal event 检索 | 增加 reconciliation 命令和启动自检报告 |
| 长会话上下文漂移 | 质量下降、成本上升 | context threshold/reset 字段 | 明确 reset 策略的 SLO 和失败路径 |
| 自定义 team 指令危险 | 本地环境被破坏 | doctor/tool surface 部分检查 | 增加 team capability 声明和风险分级 |
| MCP/Web 写操作扩大攻击面 | 非预期项目控制 | loopback 默认、token 字段预留 | 非 loopback 强制 auth，写操作审计 |
| `ccteam-core` 持续膨胀 | 维护成本上升 | 模块化文件划分 | 后续拆分 protocol/runtime/integration 子模块边界 |
| 多 session fan-out 状态合并 | 数据竞争、接口漂移 | schema 已预留 | 在实现前先定义 sub-module ownership 和 fan-in validator |

---

## 10. 演进建议

### 10.1 短期

1. **建立协议 fixture 测试集**  
   为 Claude Code hook payload、progress terminal events、team.yaml、phase frontmatter 建立固定 fixture，降低外部升级风险。

2. **统一 actions 层**  
   将所有会被 CLI/MCP/Web 复用的写操作集中到 `ccteam-core::actions`，CLI 只做参数解析和输出格式化。

3. **补充 architecture decision records**  
   对文件系统控制面、tmux 长会话、无内嵌 LLM、team 整团替换、progress.jsonl 作为事件流等决策写 ADR。

4. **启动自检增强**  
   `ccteam start` 启动时输出项目级 reconciliation 摘要：孤儿 tmux、孤儿 state、progress 末尾状态与 state 不匹配、缺失 hooks。

### 10.2 中期

1. **Web projection model**  
   不让 Web 直接拼多个文件。建议在 core 中提供只读 projection：project summary、timeline、decision queue、health summary。

2. **事件 schema 版本化**  
   为 progress event 增加显式 schema version 和 typed enum，弱化散落的 `serde_json::Value` 字段访问。

3. **调度策略配置化**  
   将 `MAX_CONCURRENT_PROJECTS = 3` 演进为全局/每 team 配置，支持 cost、优先级、资源压力感知。

4. **team/plugin 信任模型**  
   为自定义 team 引入来源、签名或本地 trust prompt；doctor 显示危险能力。

### 10.3 长期

1. **多 session 项目正式化**  
   在实现 `multi_session` 前，先定义 master/sub-module 状态机、接口契约、fan-in 冲突解决和回滚策略。

2. **外部 channel 标准适配层**  
   把 Telegram/Slack/Feishu 等统一收敛到 inbox/outbox 协议，不允许 channel 内嵌 LLM 或项目业务判断。

3. **可插拔持久化但保留文件协议**  
   当前本地文件模式适合个人开发者；如果面向团队部署，可在不破坏文件协议的前提下增加 SQLite/HTTP projection。

---

## 11. 架构师关注点结论

ccteam 的架构选择非常明确：用传统程序维护系统边界，用 Claude Code 执行开放式智能任务。这个方向比“把整个编排也交给 LLM”更可控，也比“每个 phase 启一个短进程”更符合长期自治项目的运行要求。

当前实现最值得保持的架构红线是：

- orchestrator 不内嵌 LLM。
- progress/state 文件协议是状态事实源。
- channel adapter 不承载业务语义。
- CLI/MCP/Web 只做适配，核心写逻辑回到 `ccteam-core`。
- 新 team 通过 schema 和 phase 扩展，而不是在 orchestrator 中写 team name 分支。

最需要持续投入的方向是协议治理、hook 兼容测试、写操作统一、安全边界和 Web/多 session 扩展前的状态 ownership 设计。

