# V0.4.0 文档索引

> **状态**：**planning**（2026-05-14，设计已 lock，待实施）。
> base = V0.3.2 ship 终点（`origin/main`）；workspace.version 起点 `0.3.2`。

V0.4.0 是一次**架构级重构**，核心目标：

1. **删除 phase 模板系统**——ccteam 不再预定义任何领域 workflow；phase 模板
   与 Claude Code 内置任务规划能力竞争，是根本错误的方向。
2. **引入 Workflow-as-Agent-Network**——用户用极简 `workflow.yaml` 声明
   agent 拓扑（无 prompt，只有连线）；每个 agent 的行为由 `.claude/agents/<role>.md`
   + Claude Code 自身决定。
3. **Harness 极薄**——ClaudeCodeAdapter = `claude --bg --agent <role>` + 读
   `~/.claude/jobs/<id>/state.json`；利用 Agent View 原生 session 管理，
   ccteam 不重建监控层。
4. **Token-maxxing**——Meta-agent 常驻协调，100 个 agent 并发，人只在 goal
   输入和最终 review 时出现。

## §3 架构核心决策（2026-05-14 lock）

| 决策 | 内容 |
|---|---|
| Workflow 定义 | `workflow.yaml` agent 拓扑；两种来源：用户手写 / meta-agent 动态生成 |
| Agent 角色 | `.claude/agents/<role>.md` subagent 文件；ccteam 零注入 prompt |
| CC session 宿主 | `claude --bg --agent <role>`（supervisor 接管）；Agent View 做监控 |
| Codex session 宿主 | tmux（通用容器，Codex 无 Agent View 等价物）|
| Inter-agent 通信 | 文件系统 artifact 目录（架构红线不变）|
| 触发机制 | inotify artifact watcher → 触发下游 agent |
| 人工介入点 | 显式 `gate` trigger + meta-agent escalation；非 gate 点全自动 |
| Phase 机制 | **全部删除**（phases.rs、inject_directives、golden_rules 等，~3500 LOC）|

## Findings 速查

| F | 范围 | 依赖 | 状态 |
|---|---|---|---|
| **F60** | Phase machinery removal（删 phases.rs、inject_directives、golden_rules 等）| — | pending |
| **F61** | ClaudeCodeAdapter thin refactor（claude --bg --agent + state.json）| — | pending |
| **F62** | Real CodexAdapter（吸收 V0.3.3 deferred）| — | pending |
| **F63** | workflow.yaml schema + parser | F60 | pending |
| **F64** | Artifact watcher（inotify-based trigger）| F63 | pending |
| **F65** | Meta-agent MCP tools（7 新工具）| F63 + F64 | pending |
| **F66** | Thin orchestrator（替换 2713 LOC phase 状态机）| F63–F65 | pending |
| **F67** | Progress tracking refactor（business state SoT）| F66 | pending |
| **F68** | ccteam-web v0.4.0 adaptation（Agent View + workflow 视图）| F61 + F67 | pending |
| **F69** | Example workflows + e2e + ship gate | F60–F68 | pending |

并行机会：F60 / F61 / F62 三路同时起步。

## 文档清单

| 文件 | 内容 | 状态 |
|---|---|---|
| [`prd.md`](prd.md) | V0.4.0 PRD — 背景 + 架构哲学 + 核心抽象 + 三层架构 + 新组件规格 + 示例 workflow + 迁移路径 + 红线 | locked |
| [`dev-plan.md`](dev-plan.md) | F60-F69 subagent briefing 模板 + 红线 grep 矩阵 + 依赖图 + ship gate | locked |

## 关键设计决策

详见 [`prd.md`](prd.md)：

- **workflow.yaml 里没有一行 prompt**——agent 行为完全由 `.claude/agents/*.md` 决定
- **Agent View 不重建**——`claude agents` 是 CC session 监控；ccteam-web 负责 project 上下文层
- **tmux 保留作 Codex 宿主**——Codex 无 Agent View 等价物，tmux 是跨执行环境通用容器
- **5 个核心概念**：Workflow / Agent / Artifact / Meta-agent / Gate

## V0.3.3 deferred 项处理

原 V0.3.3 计划（CodexAdapter、flex workflow promotion、htmx 清理）：

- **CodexAdapter 完整实现** → 吸收进 V0.4.0 F62
- **flex workflow promotion** → V0.4.0 架构重写后该概念已不存在；workflow.yaml 取代
- **legacy htmx assets 清理** → F68 ccteam-web 适配时一并清理
- **V0.3.3 作为单独 patch round 取消**；V0.4.0 直接接续 V0.3.2

## 跟其他文档关系

- `CLAUDE.md §一` — V0.4.0 ship 后回填 baseline 行
- `docs/interfaces.md` — F63 workflow.yaml schema / F65 新 MCP 工具签名 同步
- `docs/tech-design.md` — F66 thin orchestrator 落档后 §3 / §6 更新
- `docs/dev-coupling-audit.md` — F60-F69 各 entry 同步
