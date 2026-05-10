# V0.2.2 文档索引

V0.2.2 是首个**单独目录的 patch 版本**(V0.2.1 折在 `v0-2/e2e-retro.md`)。
本轮 7 finding(F34-F40)+ 3 配套 + e2e retro F41-F43 + reverse-rollback F44,
共 8 PR sequencing(F44 为 PR #8,反向 F39),docs 单独维护。

**已 ship**(2026-05-09):base 起点 = `origin/main` `170f5a8`(V0.2.1);
ship 终点 = workspace `0.2.2` 起点(`Cargo.toml::workspace.package.version`);
测试 baseline 起点 511 / ship 628(+117 测试 across 7 PRs,0 退步)。

**Reverse-rollback 补丁**(2026-05-10,PR #8):F44 反向 F39 cct convention
sweep — F39 选 `cct` 二进制名时未发现 Ubuntu `proj-bin` 已占用 `/usr/bin/cct`
(PROJ Coordinate Conversion / GIS 工具),`~/.local/bin/cct` 在标准 PATH
上前置会静默 shadow 系统工具。F44 把所有 F39 改动逐项反向(binary、skill、
Rust API、placeholder、docs sweep、CLAUDE.md 红线/Skills 行/迁移条目),并加
F39 → F44 反向迁移逻辑(`ccteam doctor --install-skill` 自动检测 + 清旧 `cct-*`
skill dir + rewrite settings.json 老 hook 路径)。Test count 628 → 631
(+2 反向迁移测试)。

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
| F39 `cct` 短前缀约定 sweep | cleanup | P3 | **1**(先 merge,机械 rename;**已被 F44 反向**)|
| F40 team 名缩短 + alias | cleanup | P3 | 6(独立)|
| F44 反向 F39 cct convention sweep | bugfix(silent-shadow footgun) | P0 | 8(单独 PR;`/usr/bin/cct` namespace 碰撞)|

## 配套

- **Cargo workspace.version**:`0.0.1` → `0.2.2`(retroactive 修正)
- **CLAUDE.md §五**:加 patch 开发流程小节(已落档,2026-05-09)
- **`docs/README.md`**:加 patch 目录约定(已落档,2026-05-09)

## 跟其他文档关系

- 主仓 `CLAUDE.md` §三 红线(F39 cct 约定那条已 F44 删除)+ §四 Skills 行(skill 名回到 `ccteam-*`,F44 同步)+ §六 反向迁移条目(F44 加)
- 完整 PR sequencing + 冲突点矩阵 见 `prd.md §10`
- 跨版本 SoT(`tech-design` / `interfaces`)在 V0.2.2 ship 各 PR merge 时同步更新
