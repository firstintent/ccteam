# V0.3.1 文档索引

> **V0.3.2 erratum**(2026-05-11):V0.3.1 PRD §10.3 曾把 `CodexAdapter`
> 完整实现 deferred 到 V0.3.2;在 V0.3.2 scope 锁定时(用户 confirm
> Shape A,web UI rewrite only)slip 到 **V0.3.3**。原因见
> [`docs/versions/v0-3-2/prd.md §3.1`](../v0-3-2/prd.md#31-决策---v032-范围--shape-aweb-ui-only)。
> 本 README 中 §10.3 / F47 tail 仍按"deferred to V0.3.2+"读;实际目标
> 版本以 erratum 为准。

V0.3.1 是 V0.3 主线 ship 后的**首个 patch round**(F-numbered findings under
one umbrella),也是 ccteam 的**战略支点版本**:从"phase-driven workflow
orchestrator"扩展为"session-layer farm + observability + cross-project
memory bridge",同时为 Claude Code 落实现、为 OpenAI Codex CLI 预留 trait
stub 接入点。phase 编排演进
**暂停**(无新 phase / Fat Skills 演进机制),改起一支新 team kind `flex`
(空 phase,用户原生方式驱动多 session,ccteam 观测 + 记录 + 提供控制面)。

**状态**:**shipped**(2026-05-10,F46-F51 已合入;F51 ship gate 落档)。

base = `origin/main` `f9baf3f`(V0.3 ship 终点,workspace.version `0.3.0`,
测试 baseline `738/0`)。V0.3.1 ship HEAD 以 `origin/main` 为准;
workspace.version = `0.3.1`,测试 baseline `833/0`。

## 文档清单

| 文件 | 内容 | 何时读 |
|---|---|---|
| [`prd.md`](prd.md) | V0.3.1 PRD — 战略 pivot 背景 + 6 finding(F46-F51)设计 + 已知风险 + V0.4 deferred + PR sequencing | V0.3.1 设计意图源头 |
| [`dev-plan.md`](dev-plan.md) | 6 PR 拆解 + 依赖图 + 红线 grep 矩阵 + worktree subagent briefing 模板(F46 完整,F47-F51 增量化基) | V0.3.1 实施 |
| [`user-manual.md`](user-manual.md) | V0.3.1 用户使用与手动验证手册 — flex team / multi-session / harness snapshot / web UI 逐项验收 | 手动验证 V0.3.1 功能 |
| [`e2e-retro.md`](e2e-retro.md) | F51 ship gate e2e retro — 4 suite 隔离验证、结果、verdict、numbers | V0.3.1 ship 验收 |

## Findings 速查

| F | 范围 | 优先级 | PR # | 状态 |
|---|---|---|---|---|
| **F46** `HarnessAdapter` trait + `ClaudeCodeAdapter` | trait + statusline dual-write + web SSE harness snapshot stream | P0(立 trait,后续都基于它) | 1 | shipped |
| **F47** `CodexAdapter` trait stub + `harness` 字段 | trait shape 完整、方法 `Err(NotImplemented)`、`team.yaml::sessions[].harness` 字段 + CLI flag | P1(forward-compat) | 2 | shipped |
| **F48** `kind: flex` team kind | team factory `--kind=flex`、orchestrator behavior gating(`auto_loop` / `golden_rules` / `phase_inject` off,silence/cost/hooks/progress/memory on)、`team.yaml::kind` schema | P0(基础) | 3 | shipped |
| **F49** Adhoc multi-session primitives | `ccteam session {add,ls,attach,rm}` CLI、per-session subdir layout、tmux `<slug>-<sid>` 命名、混合 harness 共存、progress 子路径 | P0 | 4 | shipped |
| **F50** Web 层更新 | dashboard `kind` 列、flex 详情页 per-session 卡片(harness badge)、`/session/<slug>/<sid>` 详情、SSE filter by sid | P1 | 5 | shipped |
| **F51** Chore + ship gate | workspace.version 0.3.0 → 0.3.1、CLAUDE.md baseline 回填、e2e for flex multi-session、`docs/versions/v0-3-1/e2e-retro.md`、README 更新 | P0 | 6 | shipped |

## F46-F51 ship summary

- **F46**:抽出 `HarnessAdapter`,把 Claude Code 现有路径收束到
  `ClaudeCodeAdapter`,同时保留 statusline 兼容字段与 web SSE harness snapshot。
- **F47**:落 `CodexAdapter` stub 与 `harness` 字段,协议先到位;所有真实 Codex
  spawn / ingest / hook surface 返回明确 `NotImplemented`。
- **F48**:新增 `kind: flex`,关闭 phase 驱动行为,保留 hooks / progress /
  cost / memory / silence 观测能力。
- **F49**:新增 adhoc multi-session primitives,以 `sessions/<sid>/`、sid 映射、
  tmux `<slug>-<sid>`、per-session progress path 支撑 flex 多 session。
- **F50**:web UI 展示 kind / session cards / harness badge / session detail,并支持
  SSE by sid filter。
- **F51**:版本 bump、ship gate e2e retro、README 出货状态同步;最终测试 baseline
  `833/0`。

## 关键设计决策

详 `prd.md §1` 战略背景 / §2 范围 / §10 不在范围:

- **三 team kind 并存**(prd §3 F48):`workflow`(默认,V0.1+)/ `multi_workflow`
  (V0.2 起 `parallelism: multi_session` 团队;数据驱动,无需新名)/ `flex`
  (V0.3.1 新增,空 phase,用户驱动 session)。`team.yaml::kind` 字段缺省
  `workflow` 保 V0.1/V0.2/V0.3 yaml 解析不变。
- **`kind` 与 `parallelism` 正交**:`kind` 是**team 级**字段,`parallelism`
  仍是**phase 级**字段。`flex` 团队无 phase 所以 `parallelism` 不适用。
- **HarnessAdapter trait**(prd §3 F46):`ccteam-core::harness` 模块,
  `ClaudeCodeAdapter` 全实现,`CodexAdapter` 全 stub(spawn / ingest 返
  `Err(NotImplemented { harness, reason })`,reason 指向 PRD §F47)。
- **adhoc session 协议正交于 multi_session**:flex 用**新轻量 session 注册**
  (`~/projects/<team>-<slug>/.ccteam/sessions/<sid>/` 子目录 + master `state.json::sessions`
  字段记 sid → harness 映射),**不复用** `parallelism: multi_session` 的
  master + 预定义 sub-module 拓扑。
- **sid 格式**:`<harness>-<n>`(`claude-1` / `codex-2` ...);全项目内单调
  递增,删后不复用。
- **tmux 命名**:`ccteam-<slug>-<sid>`(单 session 项目仍 `ccteam-<slug>`
  无 sid 后缀,V0.3 兼容)。
- **progress.jsonl scoping**:flex 项目走 `~/.ccteam/progress/<slug>/<sid>.jsonl`
  子目录(单文件每 session);非 flex 项目继续 `~/.ccteam/progress/<slug>.jsonl`
  flat 命名(M0 协议保持)。
- **flex 团队 orchestrator 行为**:`auto_loop` / `phase prompt inject` /
  `golden_rules` 三条 **off**(无 phase 可触发);`silence_classifier` / cost
  watcher / hooks(progress_append / cost_accumulate / parse_phase_end)/
  cross-project memory bridge **on**。
- **永不主动 kill**:`ccteam session rm <slug> <sid>` 是**唯一**显式用户授权
  kill 路径;cost / silence / stale 不触发 kill(CLAUDE.md §三 红线)。

## V0.3.2 / V0.4 deferred 项

详 `prd.md §10`:

- **CodexAdapter 完整实现**(spawn / ingest / hook surface)— V0.3.2,路线见
  [`docs/research/ccteam-codex-integration.md`](../research/ccteam-codex-integration.md)
- **flex workflow promotion / demotion UX**(把累积事件提升为冻结 phase /
  随时拆掉)— V0.3.2 / V0.4(Fat Skills evolution path,详
  [`docs/research/thin-harness-fat-skills-architecture-improvement.md`](../research/thin-harness-fat-skills-architecture-improvement.md) §6.1)
- **flex_workflows.yaml 持久化 schema** — V0.3.2 / V0.4
- **CC → Codex review 自动派单 pipeline**(implement phase_done 自动起 codex
  exec sidecar)— `docs/research/ccteam-codex-integration.md` M2 路线
- **harness snapshot 历史 archive**(retro 用)— V0.4
- **subagent live progress**(Claude Code 上游 API 出来后) — V0.4

## 跟其他文档关系

- 主仓 `CLAUDE.md` §一 baseline 已由 F51 回填(0.3.0 → 0.3.1,V0.3.1 milestone
  行,738 → 833);§三 红线 V0.3.1 不动(progress.jsonl SoT / 永不主动 kill /
  ccteam-core 无 team 名字面量);§六 易踩坑加 V0.3 → V0.3.1 升级注
- `docs/tech-design.md` §3.3 / §6.3 / §6.11 — F48 / F49 ship 后增补 flex /
  HarnessAdapter / 多 session 段
- `docs/interfaces.md` §1.3(扩 flex layout)/ §2.1(state.json `sessions` 字段)/
  §5.5(team.yaml `kind` + `sessions[].harness` 字段)/ §15(web routes harness SSE +
  session detail)— F46-F50 每 PR 增量补
- `docs/dev-coupling-audit.md` F46-F51 — F51 标记 close 状态
- `docs/versions/v0-2/README.md` "已 ship V0.3" 段已更新为 "已 ship V0.3 + V0.3.1"
- `docs/research/v0-3-1-harness-adapter-plan.md` — V0.3 ship 期临时记录,
  本 PRD §1 / §3 F46 是其正式版,临时文件留 git history
- `docs/research/ccteam-codex-integration.md` — V0.3.1 F47 trait stub
  setup;V0.3.2 Codex real implementation 走该研究 doc 的 M1-M5 路线
- `docs/research/thin-harness-fat-skills-architecture-improvement.md` —
  战略论证;V0.3.1 是 Fat Skills evolution 的**起点**(空 flex 是 phase
  fat-skill 的 fat-skill 演进起点)
- `docs/requirements.md` 痛点 1(久不写代码)+ 痛点 11(探索性 idea)—
  V0.3.1 主映射;新增"手动 + 渐进固化"工作姿态作 V0.3.1 自带

## 配套(F51 PR)

- `Cargo.toml::workspace.package.version` `"0.3.0"` → `"0.3.1"`。
- `CLAUDE.md` §一 baseline 表格已回填(workspace.version + 833/0 +
  V0.3.1 milestone 行)。
- `docs/versions/v0-3-1/e2e-retro.md` 已落档。
