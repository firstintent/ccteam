# ccteam docs 索引

文档分两类:**跨版本 SoT**(根目录,长期维护)+ **版本归档**(`v<major>-<minor>/` 子目录)。

## 跨版本 SoT(根目录,长期维护)

| 文件 | 角色 | 何时读 |
|---|---|---|
| [`requirements.md`](requirements.md) | 13 痛点的不可变源 | 验收基准;PR 描述映射用 |
| [`tech-design.md`](tech-design.md) | 架构 SoT | 改架构前必看 |
| [`interfaces.md`](interfaces.md) | 协议 SoT(YAML / JSON / CLI / hooks / state) | 改 schema 必同步 |
| [`dev-coupling-audit.md`](dev-coupling-audit.md) | F-finding 累积(跨版本) | 改 ccteam-core 之前 |
| [`ccteam-as-domain-agnostic-orchestrator.md`](ccteam-as-domain-agnostic-orchestrator.md) | team 泛化 charter | 加新 team 之前 |
| [`claude-code-best-practices.md`](claude-code-best-practices.md) | Claude Code 实践参考 | 改 phase prompt / hooks / context 时 |
| [`claude-code-tool-surface.md`](claude-code-tool-surface.md) | Claude Code 工具表参考 | 改 phase YAML `tools_required` / sub-skill 时 |

## 版本归档

每发布一个版本,该版本所有规划 / 设计 / retro / userguide 都归档到 `v<major>-<minor>/` 子目录。

| 版本 | 入口 | 状态 |
|---|---|---|
| **V0.1** | [`v0-1/README.md`](v0-1/README.md) | 已 ship(M0-M4.4)。历史归档 |
| **V0.2** | [`v0-2/README.md`](v0-2/README.md) | 已 ship(M0.16-M0.23)。历史归档 |
| V0.3 | (未启动) | 候选方向见 `v0-2/README.md` "V0.3 deferred" 段 |

## 文档维护规约

**版本化文档归档**:
- 每个版本发布后,PRD / dev-plan / 各 design 子文档 / retro / userguide / e2e 报告 → 该版本的 `v<major>-<minor>/`
- 版本目录加一份 `README.md` 索引 + V0.3+ deferred 项,确保每条 deferred 都有归宿

**跨版本 SoT 持续维护**:
- 改架构 / 协议 / 红线 → tech-design.md / interfaces.md / requirements.md 同步,版本归档不重复维护
- F-finding 累加进 dev-coupling-audit.md(跨版本通用),关闭时标 PR + 版本

**禁忌**:
- 不再维护"全局 development-plan.md"(已归档 V0.1,V0.2 起每版本独立 dev-plan)
- 不在根目录囤"未来版本意图"(V0.3 候选放 v0-2/README.md 末尾,V0.3 启动时迁到 v0-3/prd.md)
- 不在版本归档目录里改跨版本 SoT(双 source 对齐难)

**新增 V0.3 时的步骤**:
1. `mkdir docs/v0-3/`
2. 把 v0-2/README.md "V0.3 deferred" 段迁到 v0-3/prd.md
3. 写 v0-3/dev-plan.md / v0-3/README.md
4. 跨版本 SoT(tech-design / interfaces)在 V0.3 ship 各 milestone 时同步更新
5. V0.3 ship 后,v0-3/ 进入"历史归档"状态
