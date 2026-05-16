# V0.4.6 — 用户痛点根治 + 项目生命周期 + 运维收敛

> 文档目录入口。read PRD + dev-plan 之后再动代码。

## 三大用户痛点(2026-05-16 ask)

1. **ccteam 太复杂**:CLI 20 子命令,V0.3 phase-时代 legacy(`phase` / `decisions` / `watchdog scan`)+ 内部命令(`hook` / `mcp-serve` / `spawn` / `send` / `peek`)混在用户面上,`--help` 看一坨。**F89** 砍。
2. **Web 简陋**:WorkflowView 只有 agent cards,看不到 artifact queue / events timeline / failure log / cost trend。**F90** 增强(tmux/SessionDetail 残留**保留** — Codex tmux adapter 将复用,不是 dead code)。
3. **预算错误**:`state.cost_used_usd` 由 `Hook::CostAccumulate` 累加 + F66 `agent_done.cost_usd` + F80 `claude_job::probe_job` 三个来源,任一 hook miss 就漂。**F91** 把 cost SoT 收敛到 `~/.claude/jobs/<id>/state.json::cost_usd_total`,删 hook 累加路径。

## 项目生命周期(2026-05-16 ask,F81-F83 原版)

1. **从项目中删除 ccteam**(`ccteam remove`)— 把项目从 `~/.ccteam/config.yaml::projects[]` 摘掉,可选清项目内 `.ccteam/` + `.claude/agents/` + workflow.yaml;**热剔除**已 roster 的 event_loop(V0.4.5 daemon 不支持)。**F81**
2. **workflow.yaml `enabled` + 热加载**:`enabled: true/false` 顶层字段;daemon 监听 workflow.yaml mtime,改了就**优雅终止**老 event_loop + 用新 spec 重启,无需 daemon stop/start。**F82**
3. **workflow.yaml 位置移到 `.ccteam/`**:跟 `state.json` / `config.json` / artifact dirs 一起,语义统一,自然 gitignored。**F83**

## 运维收敛(V0.4.5 落地后暴露)

4. **F84**:`max_cost_usd_per_24h` budget cap → 超阈值自动 `enabled: false` + workflow_done reason="budget_exceeded"(对应 dex-ui 4h $1.10 自激励事件)
5. **F85**:`~/.claude/jobs/` GC(host 残留 289 entries,terminated > 7 天的 state.json 自动清)
6. **F86**:daemon graceful shutdown(SIGTERM → 所有 event_loop 走 cancel token + workflow_done reason="shutdown",F82 cancellation token 顺手做)
7. **F87**:clap `allow_hyphen_values`(`ccteam send-keys "--help"` 之类)
8. **F88**:web bearer token 自动 clipboard(`ccteam start` 后 xclip / pbcopy 跨平台 fallback)

## 文档

| 文件 | 内容 |
|---|---|
| `user-manual.md` | **V0.4.6 用户使用手册** — 简明命令清单 + 升级路径 + workflow.yaml 写法 + 故障排除 |
| `prd.md` | 11 个 finding 的产品需求 + 验收标准 |
| `dev-plan.md` | 实现路径、文件改动、迁移策略、测试矩阵 |

## 与 V0.4.5 的关系

V0.4.5 已 ship(F77 hook walk-up + F78 watcher path + F80 phantom cleanup)。V0.4.6 是 V0.4.5 落地暴露的用户体验 + 运维痛点根治,**不修 V0.4.5 已 ship 的红线**。
