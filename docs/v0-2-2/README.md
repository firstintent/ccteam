# V0.2.2 文档索引

V0.2.2 是首个**单独目录的 patch 版本**(V0.2.1 折在 `v0-2/e2e-retro.md`)。
本轮 7 finding(F34-F40)+ 3 配套,7 PR sequencing,实现量超 V0.2.1 dust patch
量级,docs 单独维护。

base = `origin/main` `170f5a8`(V0.2.1 ship);测试 baseline 511/0。

## 文档清单

| 文件 | 内容 | 何时读 |
|---|---|---|
| [`prd.md`](prd.md) | V0.2.2 PRD — 13 节,7 finding 设计 + PR sequencing + 验收 | V0.2.2 设计意图源头 |
| [`dev-plan.md`](dev-plan.md) | 7 PR milestone 拆解 + worktree 分支 + subagent briefing 模板 | V0.2.2 实施 |
| [`feedback.md`](feedback.md) | 2026-05-08 用户原始反馈 6 条原文(F34-F37 来源) | F34-F37 设计依据回看 |

## Findings 速查

| F | 性质 | 优先级 | PR # |
|---|---|---|---|
| F34 slug 命名控制 | UX 关键 | P1 | 2(依赖 #1)|
| F35 silence classifier | ship-blocker(auto-loop bug)| P0 | 3(独立)|
| F36 send-keys subagent guard | ship-blocker(/btw 路由 bug)| P0 | 4(软依赖 #3)|
| F37 meta-agent 决策树加固 | UX 关键 | P1 | 2(同 F34) |
| F38 终端截图 PNG | UX 增强 | P2 | 5(软依赖 #3)|
| F39 `cct` 短前缀约定 sweep | cleanup | P3 | **1**(先 merge,机械 rename)|
| F40 team 名缩短 + alias | cleanup | P3 | 6(独立)|

## 配套

- **Cargo workspace.version**:`0.0.1` → `0.2.2`(retroactive 修正)
- **CLAUDE.md §五**:加 patch 开发流程小节(已落档,2026-05-09)
- **`docs/README.md`**:加 patch 目录约定(已落档,2026-05-09)

## 跟其他文档关系

- 主仓 `CLAUDE.md` §三 红线 + §四 Skills 行 已加 cct 约定(F39 驱动)
- 完整 PR sequencing + 冲突点矩阵 见 `prd.md §10`
- 跨版本 SoT(`tech-design` / `interfaces`)在 V0.2.2 ship 各 PR merge 时同步更新
