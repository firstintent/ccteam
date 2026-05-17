# dev-team 耦合点审计

> 本文是 [ccteam-as-domain-agnostic-orchestrator.md](./ccteam-as-domain-agnostic-orchestrator.md)
> §B 步骤的产出。审计当前代码,把"假设了 dev 团队"的位置逐条钉死,给
> M3(team abstraction 里程碑;2026-05-05 reorder 前曾标 M4.5)提供修复路线。
>
> **审计日期**:2026-05-05
> **审计基线**:strategic doc §1 责任分界表(domain-agnostic vs team fill 的判定)
> **审计范围**:`crates/ccteam-core/src/`(9 文件)+ `crates/ccteam-cli/src/`
> (2 文件)+ `crates/ccteam-hooks/src/`(5 文件)+
> `crates/ccteam-core/src/templates/settings.json` + `phases/`(6 文件)+
> 顶层 `CLAUDE.md` 与 `docs/`
>
> 每条发现固定四要素:
> - **文件:行号**
> - **现状描述**
> - **是否真 dev-specific**(论证,不是一刀切)
> - **解耦方案**(改名 / 提 trait / 加配置 / 不必改)
> - **优先级**(P0 阻塞泛化 / P1 该做但可后置 / P2 边角 / N/A 已是领域无关)

---

## 摘要

25 条发现(2026-05-05 加 F21、升级 F20 P1→P0,共增 1 条;2026-05-06 修复 F21;
2026-05-06 M4.4 spike 加 F22 P0 + F23 P1 conditional;2026-05-06 修复 F22;
**2026-05-06 post-M3/M4 sweep**:F2/F3/F4/F9/F10/F11/F12/F13/F20 由 M3 团队
抽象 + M4 跨项目记忆批量关闭;**2026-05-07 fix_loop → auto_loop rename batch**:
F1/F5/F6/F7/F8/F18 由独立 PR 一波关闭;**2026-05-08 V0.2 M0.23**:加 F24 + F25
P0 + 同 PR 关闭;**2026-05-08 V0.2 e2e retro**:加 F26-F33 八条 V0.2.1 候选;
**2026-05-08 V0.2.1 patch**:F26-F33 全部修复;
**2026-05-09 V0.2.2 patch**:加 F34-F40 七条用户反馈 + 命名 sweep + UX 增强,跨 7 PR 全部修复;
**2026-05-09 V0.2.2 e2e retro patch**:4-suite 并行 e2e 验证,撞 F41 (P1) + F42 (P1) + F43 (P2),同 PR 一波修;
**2026-05-10 V0.2.2 F44 反向回滚**:`/usr/bin/cct` namespace 碰撞驱动整体反向 F39,F44 单 PR 覆盖;
**2026-05-10 V0.3 doc-only kickoff**:加 F45 P1(write helper promote ccteam-cli → ccteam-core::actions,M5.0 关键解耦),实施在 V0.3 PR #1 / #4);**2026-05-10 V0.3 PR #1 ship**:F45 promote 部分修复(actions 模块 + mcp_serve wrapper 透传 + dep_graph 自检测试落地),仍待 M5.3 写动作 endpoint 消费才整体 close;**2026-05-10 V0.3 PR #4 ship**:F45 **整体 close**(M5.3 写动作 endpoint + token auth + URL-shim cookie + path-traversal 守卫全部 ship);**2026-05-10 V0.3.1 doc-only kickoff**:加 F46-F51 六条(战略 pivot:flex team kind + adhoc multi-session + HarnessAdapter trait + CodexAdapter stub + web flex 适配 + ship gate);**2026-05-10 V0.3.1 ship**:F46-F51 全部 close,workspace.version 0.3.1,833/0 测试;分布:

| 优先级 | 数量 | 编号 |
|---|---|---|
| **P0 阻塞泛化(剩余)** | 0 | — |
| **P1 该做但可后置(剩余)** | 2 | F15(M1+ block-push 时做)、F23(conditional;待 spike 重跑) |
| **P2 边角(剩余)** | 1 | F17 |
| **V0.3.1 待 ship** | 0 | — |
| **N/A 已是领域无关** | 2 | F14, F19(M3 docs sweep 后)|
| **已修复** | 44 | F1 / F5 / F6 / F7 / F18(2026-05-07 rename PR;F1 触发逻辑实际早 M3.1 已切到 template.auto_loop,本 PR 完成命名层 sweep)、F2 / F3 / F4(M3.1 dag.rs)、F8(2026-05-07 directory scan)、F9 / F10 / F11(M3.4 team-aware bootstrap;F11 dev 仍裸 `phases/` 但非阻塞)、F12 / F13(M3.3 `--team` CLI + `state.team`)、F16(M3.4 phase 模板 team 化)、F20(M3.1+M3.4 retro_schema 数据形式 + product-research 填字段 + M4.1 phase 消费)、F21(@a5fb21d)、F22(PR #12)、**F24 / F25(2026-05-08 M0.23 PR)**、**F26 / F27 / F28 / F29 / F30 / F31 / F32 / F33(2026-05-08 V0.2.1 patch)**、**F34 / F35 / F36 / F37 / F38 / F39 / F40(2026-05-09 V0.2.2 patch — 7 finding 跨 7 PR)**、**F41 / F42 / F43(2026-05-09 V0.2.2 e2e retro patch)**、**F46 / F47 / F48 / F49 / F50 / F51(2026-05-10 V0.3.1 patch)** |

### V0.2 §6 反模式候选状态(docs/v0-2/prd.md)

PRD V0.2 §6 列了 8 条 ccteam-core 反模式候选清理任务,跟 F-finding
独立编号但同源(都是"领域字面量泄漏到 core"):

| 候选 | 描述 | 状态 |
|---|---|---|
| 1 + 8 | 协议关键字 `PHASE_DONE` / `ESCALATE` 三处镜像 → 单一 source | **2026-05-08 关闭(M0.18):inject prompt template + frontmatter `completion_signal` / `escalate_grammar_ref` 单一 source;phase markdown 正文清理协议关键词;`build_phase_prompt_for_template` 是唯一 protocol literal 拼装位置;详 `docs/v0-2/phase-prompt-architecture.md`** |
| 2 | `render_project_claude_md` `match team` 写死 | **2026-05-07 关闭(M0.16.3)** |
| 3 | `TEAM_BUNDLES` 编译时常量 → seed-only | **2026-05-07 关闭(M0.16.2)** |
| 5 | meta-agent `if team == META_TEAM_NAME` 5 处分叉 | **2026-05-07 关闭(M0.16.1)** |
| 7 | `RECOMMENDED_AGENTS` ln -sf 8 plugin agent | **2026-05-08 关闭(M0.20)** — 改 in-memory plugin pipeline,`bootstrap_project` 写 `enabledPlugins` 到 spawned session settings.json;`ccteam doctor --migrate-recommended-agents` 清理 V0.1 残留 ln -sf |
| 4 | `golden_rules` layered merge | V0.3 deferred |
| 6 | `pre_trust_project` 写 `~/.claude.json` | V0.3 deferred |

**剩余 P0 关键路径**:**只剩 F1**(`auto_loop` 字段已在 phase YAML 里加了
[M3.1],orchestrator 仍按 `FIX_PHASE_NAME` 字符串触发 `FixLoopState`——需
切到读 `template.auto_loop`)。完成后 ccteam-core 可彻底放弃 "fix" 这个名字。

**元发现(2026-05-05 写)**:`pub use ... M0_PHASE_DAG, FIRST_PHASE`(`crates/
ccteam-core/src/lib.rs:21`)把 dev 假设暴露到 lib 接口表面——**已在 M3.1 落地**
(dag.rs 替代 M0_PHASE_DAG / FIRST_PHASE,lib API breaking change 已发生)。

**对 §A 的反馈**:审计过程中没有发现需要修订 strategic doc §1 责任分界表
或 §2 团队扩展契约的位置——所有发现都能映射到现有分类。这是抽象切对的
好信号。M3 落地后的 post-sweep 同样没发现需要新分类。

---

## V0.3.2+ 索引(F52-F91)

> **2026-05-16 V0.4.6 docs tier-3 sweep 加**:V0.3.2 起 finding 详细描述 + 修复路径**直接住版本目录** `docs/v0-X-Y/{prd.md,dev-plan.md}`,本文不再 inline 重复(节省维护成本)。本节只给一行索引 + 状态 + 链接。F1-F51 历史 detail 块**保留作 V0.1-V0.3.1 考古**。
>
> **V0.4.0 起 phase 流水线 EOL**:F60 整删 `phases/` 模块 + `golden_rules` + `dag.rs` + `subskill` 等。F1-F33 中大量 finding(`FIX_PHASE_NAME` / `M0_PHASE_DAG` / `current_phase` 等)所**关闭的代码本身 V0.4.0 已经物理删除**,本节不再 backport — 历史描述按"那时怎么想的"保留。

| Finding | 版本 | 状态 | 摘要 |
|---|---|---|---|
| F52-F59 | V0.3.2 | closed | SPA + write-action forms + htmx retirement → `docs/v0-3-2/{prd,dev-plan}.md` |
| F60 | V0.4.0 | closed | phase 全删 → workflow.yaml 架构,见 `docs/v0-4-0/prd.md` §1 |
| F61-F69 | V0.4.0 | closed | 17 MCP 工具 + ArtifactWatcher + thin orchestrator + WorkflowView SPA + claude --bg adapter → `docs/v0-4-0/{prd,dev-plan}.md` |
| F70-F71 | (skip) | — | 编号跳号(V0.4.0 docs 准备阶段保留) |
| F72-F75 | V0.4.2 | closed | `ccteam init` 三合一 + `~/.ccteam/config.yaml` + `doctor --migrate-v041-to-v042` + `ccteam new` thin wrapper → `docs/v0-4-2/prd.md` |
| F76 | V0.4.3 | closed | slug grammar validation → `docs/v0-4-3/README.md` |
| F77 | V0.4.4 | closed | `session_context_from_cwd` walk-up + `paths.project_dir(slug)` 走 config.yaml → `docs/v0-4-4/README.md` |
| F78 | V0.4.5 | closed | watcher 项目相对路径修复 + progress.jsonl 参数对齐 → `docs/v0-4-5/README.md` |
| F79 | (skip) | — | 编号跳号 |
| F80 | V0.4.5 | closed | phantom `agent_spawn` cleanup(`claude_job::probe_job` + synthetic agent_done)→ `docs/v0-4-5/README.md` |
| **F81** | V0.4.6 | **closed** | `ccteam remove <slug>` lifecycle + active-session refusal → `docs/v0-4-6/prd.md` F81 |
| **F82** | V0.4.6 | **closed** | workflow.yaml `enabled` + 热加载(`oneshot::Receiver<CancelReason>` + `WorkflowFileWatcher`)→ `docs/v0-4-6/prd.md` F82 |
| **F83** | V0.4.6 | **closed** | workflow.yaml 默认住 `.ccteam/`(root fallback)+ `doctor --migrate-workflow-to-ccteam-dir` → `docs/v0-4-6/prd.md` F83 |
| **F84** | V0.4.6 | **closed** | `BudgetSpec`(`max_cost_usd_per_24h` / `max_agent_spawns_per_hour`)→ auto-disable workflow → `docs/v0-4-6/prd.md` F84 |
| **F85** | V0.4.6 | **closed** | `~/.claude/jobs/` GC + `doctor --gc-claude-jobs` + daemon 启动 sweep → `docs/v0-4-6/prd.md` F85 |
| **F86** | V0.4.6 | **closed** | daemon graceful shutdown(cancel token + 30s timeout fallback + trigger file `/tmp/ccteam-<user>.shutdown`)→ `docs/v0-4-6/prd.md` F86 |
| **F87** | V0.4.6 | **closed** | clap `allow_hyphen_values` + `disable_help_flag` 在 `send` / `spawn` → `docs/v0-4-6/prd.md` F87 |
| **F88** | V0.4.6 | **closed** | web bearer token 自动 clipboard(xclip → xsel → wl-copy → pbcopy → clip.exe fallback chain)→ `docs/v0-4-6/prd.md` F88 |
| **F89** | V0.4.6 | **closed** | CLI 瘦身:删 V0.3 legacy(`phase` / `decisions` / `watchdog`),`hook` / `mcp-serve` / `spawn` / `send` / `peek` / `attach` / `progress` / `resume` 移到 `ccteam internal <subcmd>` 隐藏分组(老顶层保留 + WARN 到 V0.5)→ `docs/v0-4-6/prd.md` F89 |
| **F90** | V0.4.6 | **closed** | Web WorkflowView 4 新面板(ArtifactQueuePanel / EventsTimelinePanel / FailureInspector / CostSparkline)+ 4 新 API endpoint → `docs/v0-4-6/prd.md` F90 |
| **F91** | V0.4.6 | **closed** | cost SoT 收敛(删 `Hook::CostAccumulate` + `cost_summary` 实时读 `~/.claude/jobs/<id>/state.json::cost_usd_total`;`state.cost_used_usd` 字段 deprecated 但 serde-compat)→ `docs/v0-4-6/prd.md` F91 |
| **F92** | V0.4.7 候选 | **open** | 真 cost 数据源(host `state.json` 没有 `cost_usd_total` 字段 — 真实数据在 `linkScanPath` jsonl event 的 Anthropic `usage` 字段)— 2026-05-16 V0.4.6 E2E 发现 |

## V0.4.6 摘要更新

| 优先级 | 数量 | 编号 |
|---|---|---|
| **P0 阻塞泛化(剩余)** | 0 | — |
| **P1 该做但可后置(剩余)** | 3 | F15(M1+ block-push 时做)、F17、F23(conditional;待 spike 重跑) |
| **V0.4.7 候选** | 1 | F92(真 cost 数据源)|
| **N/A 已是领域无关** | 2 | F14, F19 |
| **已修复**(F1-F91 + V0.2 §6 反模式 8 条)| **~85** | 见上表 + V0.2 §6 候选状态表 |

---


## 当前 open finding(V0.4.6)

V0.4.6 起 audit 文档**只列 open finding** + V0.3.2+ 索引(见 §"V0.3.2+ 索引");已 close finding 的详细描述住版本 dir(`docs/v0-X-Y/{prd,dev-plan}.md`)。CLAUDE.md §五.3 "Pre-v1.0 不留技术债"原则:本节不保留任何已 close finding 的描述。

### F15 — settings.json 模板未含危险命令拦截(P1)

- **位置**:`crates/ccteam-core/src/templates/settings.json`(`PostToolUse` matcher)
- **状态**:M0 模板无 `Bash:git push.*` 拦截;`block-push` hook 还没实现。
- **触发时机**:M1+ 实装 `block-push` 时,team.yaml 加 `danger_command_patterns: [{ pattern, reason }]`,`render_project_settings` 按 team 参数注入 matcher。**不**直接写 `Bash:git push.*` 字面量。

### F17 — 测试用例硬编码 dev phase 名(P2)

- **位置**:`crates/ccteam-core/tests/state_machine_test.rs`(V0.4.0 后已大部分迁移,残留 V0.1 测试需 sweep)
- **状态**:V0.4.0 phase EOL 后这些测试本身大部分已删。残留的应跟 V0.5 ralph-loop / clippy sweep 一起 rename + 移到 `tests/team-dev/`。

### F23 — 容器 bind-mount `~/.claude/rules/`(P1 conditional)

- **位置**:N/A(spike 验证)
- **状态**:F22 修复后 spike §4 已解锁,等谁跑一次容器内 `--dangerously-skip-permissions` 验证 `~/.claude/rules/*.md` 是否被 Claude Code 当 context 注入。spike 失败才升 P0;详 `docs/v0-1/m4-spike.md` §4。

### F92 — cost 数据源真相(V0.4.7 候选)

- **位置**:`crates/ccteam-core/src/queries.rs::cost_summary` + `claude_job::probe_state_json`
- **状态**:V0.4.6 F91 收敛 cost SoT 到 `~/.claude/jobs/<id>/state.json::cost_usd_total` — 但 host probe 显示 cliVersion 2.1.143 的 state.json **没有这字段**!真实 cost / token / model / rate 数据在 `state.json::linkScanPath` 指向的 jsonl 文件里(每个 Anthropic API event 的 `usage` 字段)。
- **修复方向**:V0.4.7 加 `claude_usage` 模块,parse linkScanPath jsonl tail 聚合 → `CostSummary` 扩展含 token / model / rate。F84 budget 同时支持 token-based caps。F90 SPA 加 token / model / rate sparkline。
- **优先级**:**P1**(影响 web UI cost 显示 + budget enforce 准确性)。V0.4.6 e2e 实测发现。

### F102 — F80 stale-spawn 漏 `state=working` 卡死(V0.4.7 候选)

- **位置**:`crates/ccteam-core/src/claude_job.rs::probe_state_json` + `orchestrator.rs::poll_completions` F80 stale-spawn cleanup
- **症状**:dex-ui qa-autoloop 实测,daemon 重启后 11 个 claude `--bg-spare` worker 在 OS 仍存活(`ps -ef | grep bg-spare` cwd=dex-ui 都列出),但其 state.json **`state: "working"` 且 `updatedAt` 冻结于 daemon 上次活跃时刻**(75+ min 前)。F80 现行 cleanup 只在 `state.json::probe_state_json` 返回 `JobLiveness::Terminal` 时合成 `agent_done`;`state=working` 一律视作 `Running`,无 staleness 判断 → progress.jsonl 永远缺这些 spawn 的 done 事件 → web UI 显示 ghost-running 长期不消。手动 `kill -TERM <pid>` + 手写 `agent_done` 才恢复一致(本次 F102 现场)。
- **触发场景**:daemon 重启 / SIGKILL / pty-host parent 死亡时,claude `--bg-spare` worker 可孤儿存活但停止心跳 state.json。同样可能在 claude 内部死锁 / 长 hang 时复现。
- **修复方向**:`probe_state_json` 加第三档判据:
  - `state=working && updatedAt > stale_threshold(默认 5 min)` → `JobLiveness::Terminal{status: "killed", cost_usd: 0.0}`(treat as crashed)
  - 可选增强:同时 `kill -0 <pid>` 探活(需 claude state.json 暴露 PID;现行无,需 claude-code 改 schema 或我们 patch)。先靠 updatedAt 阈值足够。
- **测试**:加 unit 覆盖:write `state: working, updatedAt: now-10min` 的 state.json → probe 返回 Terminal{killed}。orchestrator_thin_test 加 integration:模拟 stuck worker → poll_completions 后 progress.jsonl 有合成 agent_done。
- **优先级**:**P1**(任何 daemon 重启都暴露;影响 UI 一致性 + parallelism 计数 + cost 双重计入)。V0.4.6 dex-ui qa-autoloop 实测踩到。
- **关联**:V0.4.6 `5da83dc` parallelism race fix 修了**race 路径**;F102 修**daemon-restart-leftover 路径**。两者互补,合并解决 dex-ui 长期"幽灵 session"问题。

---

## 历史(F1-F91)

完整 finding 历史 detail 已**移到版本 dir**(`docs/v0-X-Y/{prd,dev-plan}.md`),按 ship 时间顺序索引在上面 "V0.3.2+ 索引" 表 + V0.2 §6 反模式状态表里。本文不再 inline 重复(CLAUDE.md §二 三类文档维护规则 + §五.3 "Pre-v1.0 不留技术债")。

V0.1-V0.3.1 时代的 finding 详细 audit 文本(F1-F51)2026-05-16 删除前最后版本见 git history(`107ccb2` 之前);其论点已被 V0.4.0 F60 phase 删除 + V0.4.0+ workflow.yaml 架构整体 supersede。
