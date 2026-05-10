# V0.2 文档索引

V0.2 已 ship(`origin/main = 503269a`,8 个 milestone M0.16-M0.23,497/0 测试)。
本目录收纳 V0.2 全部相关文档。

> **已 ship V0.3**(2026-05-10,`docs/v0-3/`):web UI dashboard + SSE +
> 写动作 + token auth(5 milestone M5.0-M5.4,新 crate `ccteam-web`,
> workspace.version 0.3.0,738 测试)。**+ V0.3.1 起始(drafting,
> `docs/v0-3-1/`)**:战略 pivot — flex team kind + adhoc multi-session +
> HarnessAdapter trait + CodexAdapter stub(F46-F51,6 finding 跨 6 PR,
> workspace.version 目标 0.3.1)。下方 "V0.3 deferred 项" 仍 deferred(V0.3
> web UI / V0.3.1 session farm 都是新增主线,不替代列出的 deferred 项;
> V0.4 候选见 [`docs/v0-3/prd.md §10`](../v0-3/prd.md) +
> [`docs/v0-3-1/prd.md §10`](../v0-3-1/prd.md))。

## 文档清单

| 文件 | 内容 | 何时读 |
|---|---|---|
| [`prd.md`](prd.md) | V0.2 PRD — 9 个 §: 背景 / 自循环 / watchdog / team 工厂(plugin 模型) / 团队布局 + phase prompt 架构 / ccteam-core 反模式重构 / 已知未决项 / 验收 / 不在范围 | V0.2 设计意图源头 |
| [`dev-plan.md`](dev-plan.md) | 8 milestone(M0.16-M0.23)拆分 + 依赖图 + PR 顺序 + 红线 grep + 文档同步矩阵 | V0.2 实施 / 后续维护 |
| [`alignment-review.md`](alignment-review.md) | Claude Code 哲学 8 条 + plugin/marketplace 机制 + hooks lifecycle + layered settings + ccteam 反模式 audit。基于 5 路并行 fork 综合。**PRD 各章节的设计依据** | V0.2 撞设计墙时回看 |
| [`phase-prompt-architecture.md`](phase-prompt-architecture.md) | V0.2 §5.3 D 方案完整设计:三层架构(frontmatter / orchestrator inject prompt / 正文)/ 字段全集 / 模板拼装 / 改造前后示例 | M0.18 实施 / 后续 phase 协议变更 |
| [`team-factory-userguide.md`](team-factory-userguide.md) | M0.22 用户指南:`ccteam team init/publish` 用法 + plugin 格式说明 | 给新建 team 的用户 |

## V0.3 deferred 项(从 V0.2 PRD §7 + V0.2 PR ⚠ 整理)

设计 / 决策已 deferred 给 V0.3+(见 `prd.md §7`):

- 候选 4:`golden_rules` layered merge(team default + phase override 字段级合并)
- 候选 6:`pre_trust_project` 写 `~/.claude.json` → 项目级 settings.json
- watchdog 升级到 Critic agent(M5)整合
- Conditional / lazy phase activation via `paths:` glob(借鉴 Claude Code skills `paths:` 字段)
- Team 重命名(`dev` → `software-development` 等领域命名;牵扯 state.json / slug 前缀过广)
  — **部分 ship 在 V0.2.2 F40**:`product-research` → `research` 通过 `team.yaml::aliases` 软迁移
  (`docs/v0-2-2/prd.md §9`);`dev` 已经短,V0.2.2 PRD 未列入,仍 deferred
- M0.20 `KNOWN_PLUGIN_AGENTS` → runtime discovery(walk `~/.claude/plugins/marketplaces/*/plugins/*/agents/`)
- M0.21 watchdog cron timer(M2+ channel layer 后)
- M0.22 `dependencies`(team-plugin 间复用)+ `userConfig`(用户填表)实施
- M0.22 doctor zod schema 强化校验
- 反编译细节深挖(eg async hook stdout JSON / bundled skill 解压)

## 跟其他文档关系

- 主仓 `CLAUDE.md` §三 红线指向本目录的设计依据(尤其 plugin pipeline / 自循环 / 协议外移)
- `docs/tech-design.md` §3.5 / §3.9 / §6.4 / §6.9 / §6.12 / §1 红线表 — V0.2 ship 后已同步
- `docs/interfaces.md` §5.1 / §5.5 / §5.5.1 / §6 / §6.2.1 / §10.5 / §12.5 / §13 — V0.2 ship 后已同步
- `docs/dev-coupling-audit.md` — V0.2 §6 候选 1/2/3/5/7/8 全 closed,候选 4/6 V0.3 deferred
