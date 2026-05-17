# ccteam docs 索引

文档分三类(CLAUDE.md §二 文档维护规则):

1. **全局文档**(本目录根)— 每 session 起手装入上下文;与代码并列**唯一真理来源**;每版本 ship 后必更新
2. **版本归档**(`v0-x-x/` 子目录)— ship 后冻结,历史溯源,按需加载
3. **扩展研究**(`research/` + `references/research/`)— 探索性,不更新,按需加载

## 全局文档(根目录)

| 文件 | 角色 | 何时读 |
|---|---|---|
| [`requirements.md`](requirements.md) | 15 痛点(13 用户痛点 + 2 个 V1.0.0 终极目标) | 验收基准;PR 描述映射用 |
| [`orchestration-patterns.md`](orchestration-patterns.md) | 5 模式编排目录 + 拆分哲学(ccteam 后续迭代的模式选型字典) | 加 workflow 模板 / 设计新 finding / 拓展新领域 team 前 |
| [`tech-design.md`](tech-design.md) | 架构 SoT(workflow.yaml + ArtifactWatcher + thin orchestrator 怎么实现 5 模式) | 改架构前必看 |
| [`interfaces.md`](interfaces.md) | 协议 SoT(YAML / JSON / CLI / hooks / state) | 改 schema 必同步 |
| [`dev-coupling-audit.md`](dev-coupling-audit.md) | F-finding 累积(跨版本) | 改 `ccteam-core` 前 |
| [`ccteam-as-domain-agnostic-orchestrator.md`](ccteam-as-domain-agnostic-orchestrator.md) | team 泛化 charter | 加新 team 前 |
| [`claude-code-best-practices.md`](claude-code-best-practices.md) | Claude Code 实践参考 | 改 agent prompt / hooks / context 管理时 |
| [`claude-code-tool-surface.md`](claude-code-tool-surface.md) | Claude Code 工具表参考 | 改 workflow.yaml + `.claude/agents/<role>.md` 时 |

## 扩展研究(`research/`)

| 文件 | 内容 |
|---|---|
| [`research/architecture-analysis.md`](research/architecture-analysis.md) | 架构分析(面向架构师快速理解当前实现) |
| [`research/thin-harness-fat-skills-architecture-improvement.md`](research/thin-harness-fat-skills-architecture-improvement.md) | Thin Harness + Fat Skills 架构改进建议 |
| [`research/ccteam-codex-integration.md`](research/ccteam-codex-integration.md) | Codex 作为控制端 / sidecar / worker 的路径 |
| [`research/ccteam-ast-grep-integration.md`](research/ccteam-ast-grep-integration.md) | ast-grep 集成分析(结构搜索 / 规则 / codemod) |
| [`research/omc-orchestration-modes.md`](research/omc-orchestration-modes.md) | OMC 8 mode 全谱调研(为 [`../orchestration-patterns.md`](orchestration-patterns.md) 提供素材) |
| [`research/omc-vs-ccteam-orchestration.md`](research/omc-vs-ccteam-orchestration.md) | prompt-as-orchestrator vs code-as-orchestrator 对比 |
| [`research/omc-team-comparison.md`](research/omc-team-comparison.md) | OMC team SKILL.md vs Anthropic Agent Teams 实测 schema vs ccteam V0.5.0 落地对照(V0.5.0 立项依据)|

## 版本归档(`v0-x-x/`)

| 版本 | 入口 | 状态 |
|---|---|---|
| **V0.1** | [`v0-1/README.md`](v0-1/README.md) | M0-M4.4。phase 流水线 + 跨项目记忆原始版本 |
| **V0.2** | [`v0-2/README.md`](v0-2/README.md) | M0.16-M0.23。plugin pipeline + team-factory |
| **V0.2.2** | [`v0-2-2/README.md`](v0-2-2/README.md) | F34-F40。`ccteam-project-creator` skill |
| **V0.3** | [`v0-3/README.md`](v0-3/README.md) | M5。Web UI + JSON API + `kind: flex` team |
| **V0.3.1** | [`v0-3-1/README.md`](v0-3-1/README.md) | F46-F51。flex team + HarnessAdapter spike |
| **V0.3.2** | [`v0-3-2/README.md`](v0-3-2/README.md) | F52-F59。SPA + write-action forms + htmx retirement |
| **V0.4.0** | [`v0-4-0/README.md`](v0-4-0/README.md) | F60-F69。phase 全删 + workflow.yaml + artifact watcher + thin orchestrator + 17 MCP 工具 + WorkflowView SPA |
| **V0.4.1** | [`v0-4-1/`](v0-4-1/) | UX 简化 patch(`start` 合并 web、`send`/`spawn` CLI、handle 删、daemon hot-reload、MCP 退出 deadlock fix) |
| **V0.4.2** | [`v0-4-2/`](v0-4-2/) | F72-F75。`ccteam init` 三合一 + `~/.ccteam/config.yaml` 全局 SoT + `doctor --migrate-v041-to-v042` + `ccteam new` thin wrapper |
| **V0.4.3** | [`v0-4-3/README.md`](v0-4-3/README.md) | F76。slug grammar validation + collision wording 优化 |
| **V0.4.4** | [`v0-4-4/README.md`](v0-4-4/README.md) | F77。`session_context_from_cwd` walk-up + `paths.project_dir(slug)` 走 config.yaml registry |
| **V0.4.5** | [`v0-4-5/README.md`](v0-4-5/README.md) | F78 + F80。watcher 项目相对路径修复 + phantom agent_spawn cleanup |
| **V0.4.6** | [`v0-4-6/README.md`](v0-4-6/README.md) | F81-F91 (11 个 finding)。lifecycle / 用户痛点根治 / 运维收敛 — **当前 ship 版本** |
| **V0.5.0** | [`v0-5-0/README.md`](v0-5-0/README.md) | F92-F96。真 cost 数据源 + Agent Teams 集成(workflow.yaml mode + `__lead` role + 3 hook 镜像 + 3 web 面板)— **doc-first 立项中**,代码未动 |

## 文档维护规约

### 三类的更新节奏

- **全局文档** — 每版本 ship 后**必更新**:
  - 改协议(YAML 字段 / JSON shape / 文件路径 / CLI 签名 / hooks)→ 同步 `interfaces.md`
  - 改架构(orchestrator 流 / Phase 模型 / 团队拓扑)→ 同步 `tech-design.md`
  - 加 finding → 同步 `dev-coupling-audit.md`(关闭时标 PR + 版本)
- **版本归档** — ship 后**冻结**。即使发现回头看错了,也写在下个版本的归档里更正,**不动旧版本目录**。
- **研究文档** — **不更新**。引用过时不算 bug,价值在于"那时怎么想的"。

### 版本归档的标准内容

- `README.md` — 入口 + 概述 + finding 列表 + 与上版本关系
- `prd.md` — 产品需求 + 痛点 + 验收标准
- `dev-plan.md` — 实现路径 + 文件改动 + 测试矩阵 + 迁移策略
- `user-manual.md` — 用户操作手册(简明命令清单 + 升级路径 + 故障排除)
- 可选:`e2e-retro.md` / `deploy-verify.md` / `feedback.md` / `migration-guide.md`

### 新版本启动步骤

1. `mkdir docs/v0-X-Y/`(版本号严格按 `vMAJOR-MINOR-PATCH`)
2. 把上版本 `README.md` 末尾的 "当前 next" / "V0.X 候选" 段迁到本版本 `prd.md`
3. 写 `dev-plan.md` 后再动代码(doc-first 原则,见 CLAUDE.md §五 "Patch 版本开发流程")
4. 版本 ship 后,本目录的 4 个全局 SoT 文档同步更新(如有协议/架构变化)
5. 当前版本 README.md "当前 next" 段记下个版本候选

### 禁忌

- **不再维护"全局 development-plan.md"** — V0.2 起每版本独立 `dev-plan.md`(V0.1 老 development-plan.md 在 `v0-1/` 历史归档)
- **不在版本归档里改全局 SoT** — 双 source 对齐难
- **不在 `research/` 改实现细节** — 那是研究笔记;实现细节属于全局 SoT
