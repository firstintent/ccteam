# ccteam docs 索引

文档分三类(维护规则见本页末「文档维护规约」与 CLAUDE.md §二):

1. **全局文档**(本目录根)— 每 session 起手装入上下文;与代码并列**唯一真理来源(SoT)**;每版本 ship 后必更新。
2. **版本归档**(`versions/v0-x-x/` 子目录)— ship 后冻结,历史溯源,按需加载。
3. **扩展研究**(`research/` 子目录)— 探索性,不更新,按需加载。

## 全局文档(根目录)

### 内部 SoT / 参考

| 文件 | 角色 | 何时读 |
|---|---|---|
| [`requirements.md`](requirements.md) | 15 痛点验收基准 | 验收基准;PR 描述映射用 |
| [`tech-design.md`](tech-design.md) | 架构 SoT(gateway daemon + HarnessAdapter×ProcessBackend + chat⇄project⇄session) | 改架构前必看 |
| [`interfaces.md`](interfaces.md) | 协议 SoT(CLI / JSON / state / progress.jsonl / hooks / web 路由) | 改 schema / CLI / hooks 必同步 |
| [`orchestration-patterns.md`](orchestration-patterns.md) | 推后的 `ccteam-flow` 编排层模式设计 | 设计编排层 / 拓展新领域 team 前 |
| [`ccteam-as-domain-agnostic-orchestrator.md`](ccteam-as-domain-agnostic-orchestrator.md) | team 泛化 charter | 加新 team / 改红线时 |
| [`dev-coupling-audit.md`](dev-coupling-audit.md) | dev 内部 F-finding 账本 | 改 `ccteam-core` 前;记新发现 |
| [`claude-code-best-practices.md`](claude-code-best-practices.md) | Claude Code 实践参考 | 改 agent prompt / hooks / context 管理时 |
| [`claude-code-tool-surface.md`](claude-code-tool-surface.md) | Claude Code 工具表参考 | 改 workflow.yaml + `.claude/agents/<role>.md` 时 |

### 用户面

| 文件 | 角色 | 何时读 |
|---|---|---|
| [`quickstart.md`](quickstart.md) | 5–10 分钟上手 | 第一次装 / 跑 ccteam |
| [`user-manual.md`](user-manual.md) | 日常操作手册 | install → run → use → operate 全流程 |
| [`troubleshooting.md`](troubleshooting.md) | 故障排查 | 出问题时按症状查 |
| [`recipes.md`](recipes.md) | 开箱即用配方 | 想照搬常见场景配置 |
| [`task-to-command.md`](task-to-command.md) | 意图 → 命令决策树 | 不确定该用哪个命令 / skill 时 |

进阶配置在 [`advanced/`](advanced/):`customize-workflow.md`(定制 workflow)/ `presets-reference.md`(预设清单)/ `multi-llm-codex.md`(多 LLM 与 Codex 接入)。

## 扩展研究(`research/`)

探索性笔记与对照调研,**不随版本更新**,价值在于「那时怎么想的」。按需翻阅 [`research/`](research/) 目录,不在此逐篇索引。

## 版本归档(`versions/v0-x-x/`)

版本归档在 `versions/v0-x-x/` 子目录(每版本独立 `README` + `prd` + `dev-plan`),按需查阅;**不在此索引逐版列举**(避免漂移)。

## 文档维护规约

### 三类的更新节奏

- **全局文档** — 每版本 ship 后**必更新**:
  - 改协议(CLI 签名 / JSON shape / state / progress.jsonl / 文件路径 / hooks / web 路由)→ 同步 [`interfaces.md`](interfaces.md)
  - 改架构(daemon / HarnessAdapter / ProcessBackend / chat⇄project⇄session 模型)→ 同步 [`tech-design.md`](tech-design.md)
  - 加 / 关 finding → 同步 [`dev-coupling-audit.md`](dev-coupling-audit.md)
  - 改用户可见能力 → 同步对应用户面文档(quickstart / user-manual / troubleshooting / recipes / task-to-command)
- **版本归档** — ship 后**冻结**。即使回头看错了,也写进下个版本的归档里更正,**不动旧版本目录**。
- **研究文档** — **不更新**。引用过时不算 bug。

### 版本归档的标准内容

- `README.md` — 入口 + 概述 + 本版能力 + 与上版本关系
- `prd.md` — 产品需求 + 痛点 + 验收标准
- `dev-plan.md` — 实现路径 + 文件改动 + 测试矩阵
- 可选:`user-manual.md` / `e2e-retro.md` / `deploy-verify.md` / `feedback.md` 等

### 禁忌

- **不维护全局 `development-plan.md`** — 每版本独立 `dev-plan.md`;旧的全局 development-plan 已归档进对应版本目录。
- **不在版本归档里改全局 SoT** — 双 source 对齐难;全局真相只住根目录。
- **不在 `research/` 改实现细节** — 那是研究笔记;实现细节属于全局 SoT。
