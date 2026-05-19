# V0.1 文档归档

V0.1 时代(M0 - M4.4 + F22 fix)所有规划 / 设计 / retro / 实操文档。
此目录是**历史归档**;V0.1 已 ship,本目录文件仅作为历史决策依据,不再
更新。

## 文档清单

| 文件 | 内容 |
|---|---|
| [`development-plan.md`](development-plan.md) | V0.1 milestone tracker(M0 / M0.5 / M1 / M2 / M2.3 / M3 / M4.1-M4.4)。任务 / 依赖 / 验收 / 痛点反向映射。**V0.2 起不再维护此文件**,新版本各自 `v<major>-<minor>/dev-plan.md` |
| [`user-quickstart.md`](user-quickstart.md) | V0.1 用户操作指南(meta-agent flow + dev / product-research e2e walkthrough) |
| [`m0-retro.md`](m0-retro.md) | M0 ship 后 retro 报告 |
| [`m2-agent-team-spike.md`](m2-agent-team-spike.md) | M2.2 agent_team 永久 deferred 的 spike 决策 — Claude Code 当时无 first-class CLI surface 给 phase 内多角色并行 |
| [`m4-spike.md`](m4-spike.md) | M4 跨项目记忆 spike(2026-05-06)— 简化方向(官方 rules + 可选 claude-mem)落地依据 |
| [`e2e.md`](e2e.md) | V0.1 全流程 e2e 测试报告(2026-05-06) |

## V0.1 → V0.2 主要变化

详见 `docs/versions/v0-2/prd.md` §1 背景 + `docs/versions/v0-2/alignment-review.md`:

- 协议关键字 `PHASE_DONE` / `ESCALATE` 三处镜像 → 单一 source(orchestrator inject prompt + frontmatter)
- ln -sf 8 个 plugin agent → spawned session `enabledPlugins`(plugin pipeline)
- meta-agent `if team == META_TEAM_NAME` 5 处分叉 → `TeamSpec.evergreen` flag
- `TEAM_BUNDLES` 编译时常量 → seed-on-bootstrap(磁盘扫描)
- `render_project_claude_md` `match team` → `team.yaml.claude_md_template`
- phase markdown 协议指令外移 → orchestrator inject prompt 拼装(D 方案)
- auto_loop default-on + Stop hook self-loop(exit-2 + stop_hook_active 防递归)
- AskUserQuestion PreToolUse hook 拦截
- 团队布局 `phases-<team>/` → `teams/<name>/phases/`(三层 TEAM_SOURCES first-source-wins)
- daemon health supervision + 1M context 默认 + send_to_session fail-loud
- meta-agent 升 watchdog(translation 层)
- team factory(plugin 模型)+ `ccteam team init|publish`

如需 V0.1 时代的工作模式 / 实操流程参考,文件仍保留可读;但任何"现在该怎么做"的判断依据请走 `tech-design.md` / `interfaces.md` 当前版本 + `docs/versions/v0-2/`。
