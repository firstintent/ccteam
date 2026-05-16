# V0.4.6 — 项目生命周期 + workflow 热加载

> 文档目录入口。read PRD + dev-plan 之后再动代码。

## 目标(用户 2026-05-16 ask)

1. **从项目中删除 ccteam**(`ccteam remove`)— 把项目从 `~/.ccteam/config.yaml::projects[]` 摘掉,可选清项目内 `.ccteam/` + `.claude/agents/` + workflow.yaml;同时**热剔除**已 roster 的 event_loop(V0.4.5 daemon 不支持,旧 loop 留在 JoinSet 里跑直到 daemon 重启)。
2. **workflow.yaml 加 loop 开关 + 热加载**:`enabled: true/false` 顶层字段;daemon 监听 workflow.yaml mtime 变化,改了就**优雅终止** 老 event_loop + 用新 spec 重启,无需 daemon stop/start。
3. **workflow.yaml 位置移到 `.ccteam/workflow.yaml`**:从项目根移到 `.ccteam/workflow.yaml`,跟 `state.json` / `config.json` / artifact dirs 一起,语义统一。

## 触发场景

V0.4.5 在 host 落地后发现的痛点:
- 已 roster 的 dev-hot-reload-test 项目想停 → 只能 daemon 重启(影响所有项目)
- workflow.yaml 改了一行 trigger 想生效 → 也只能 daemon 重启
- 项目目录被 rename / 删除 / 移动,config.yaml::projects[] 不会自动跟 → 残留孤儿条目(已见过:v042-scenarioA / v042-scenarioB / dev-v042test daemon log WARN)

## 文档

| 文件 | 内容 |
|---|---|
| `prd.md` | 3 个 finding 的产品需求 + 验收标准 |
| `dev-plan.md` | 实现路径、文件改动、迁移策略 |

## 与 V0.4.5 的关系

V0.4.5 已 ship(F77 hook walk-up + F78 watcher path + F80 phantom cleanup)。V0.4.6 三个 finding 都是 V0.4.5 落地暴露的运维痛点的根治,**不修 V0.4.5 已 ship 的红线**。
