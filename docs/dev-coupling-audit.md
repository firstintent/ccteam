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

### V0.2 §6 反模式候选状态(docs/versions/v0-2/prd.md)

PRD V0.2 §6 列了 8 条 ccteam-core 反模式候选清理任务,跟 F-finding
独立编号但同源(都是"领域字面量泄漏到 core"):

| 候选 | 描述 | 状态 |
|---|---|---|
| 1 + 8 | 协议关键字 `PHASE_DONE` / `ESCALATE` 三处镜像 → 单一 source | **2026-05-08 关闭(M0.18):inject prompt template + frontmatter `completion_signal` / `escalate_grammar_ref` 单一 source;phase markdown 正文清理协议关键词;`build_phase_prompt_for_template` 是唯一 protocol literal 拼装位置;详 `docs/versions/v0-2/phase-prompt-architecture.md`** |
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

> **2026-05-16 V0.4.6 docs tier-3 sweep 加**:V0.3.2 起 finding 详细描述 + 修复路径**直接住版本目录** `docs/versions/v0-X-Y/{prd.md,dev-plan.md}`,本文不再 inline 重复(节省维护成本)。本节只给一行索引 + 状态 + 链接。F1-F51 历史 detail 块**保留作 V0.1-V0.3.1 考古**。
>
> **V0.4.0 起 phase 流水线 EOL**:F60 整删 `phases/` 模块 + `golden_rules` + `dag.rs` + `subskill` 等。F1-F33 中大量 finding(`FIX_PHASE_NAME` / `M0_PHASE_DAG` / `current_phase` 等)所**关闭的代码本身 V0.4.0 已经物理删除**,本节不再 backport — 历史描述按"那时怎么想的"保留。

| Finding | 版本 | 状态 | 摘要 |
|---|---|---|---|
| F52-F59 | V0.3.2 | closed | SPA + write-action forms + htmx retirement → `docs/versions/v0-3-2/{prd,dev-plan}.md` |
| F60 | V0.4.0 | closed | phase 全删 → workflow.yaml 架构,见 `docs/versions/v0-4-0/prd.md` §1 |
| F61-F69 | V0.4.0 | closed | 17 MCP 工具 + ArtifactWatcher + thin orchestrator + WorkflowView SPA + claude --bg adapter → `docs/versions/v0-4-0/{prd,dev-plan}.md` |
| F70-F71 | (skip) | — | 编号跳号(V0.4.0 docs 准备阶段保留) |
| F72-F75 | V0.4.2 | closed | `ccteam init` 三合一 + `~/.ccteam/config.yaml` + `doctor --migrate-v041-to-v042` + `ccteam new` thin wrapper → `docs/versions/v0-4-2/prd.md` |
| F76 | V0.4.3 | closed | slug grammar validation → `docs/versions/v0-4-3/README.md` |
| F77 | V0.4.4 | closed | `session_context_from_cwd` walk-up + `paths.project_dir(slug)` 走 config.yaml → `docs/versions/v0-4-4/README.md` |
| F78 | V0.4.5 | closed | watcher 项目相对路径修复 + progress.jsonl 参数对齐 → `docs/versions/v0-4-5/README.md` |
| F79 | (skip) | — | 编号跳号 |
| F80 | V0.4.5 | closed | phantom `agent_spawn` cleanup(`claude_job::probe_job` + synthetic agent_done)→ `docs/versions/v0-4-5/README.md` |
| **F81** | V0.4.6 | **closed** | `ccteam remove <slug>` lifecycle + active-session refusal → `docs/versions/v0-4-6/prd.md` F81 |
| **F82** | V0.4.6 | **closed** | workflow.yaml `enabled` + 热加载(`oneshot::Receiver<CancelReason>` + `WorkflowFileWatcher`)→ `docs/versions/v0-4-6/prd.md` F82 |
| **F83** | V0.4.6 | **closed** | workflow.yaml 默认住 `.ccteam/`(root fallback)+ `doctor --migrate-workflow-to-ccteam-dir` → `docs/versions/v0-4-6/prd.md` F83 |
| **F84** | V0.4.6 | **closed** | `BudgetSpec`(`max_cost_usd_per_24h` / `max_agent_spawns_per_hour`)→ auto-disable workflow → `docs/versions/v0-4-6/prd.md` F84 |
| **F85** | V0.4.6 | **closed** | `~/.claude/jobs/` GC + `doctor --gc-claude-jobs` + daemon 启动 sweep → `docs/versions/v0-4-6/prd.md` F85 |
| **F86** | V0.4.6 | **closed** | daemon graceful shutdown(cancel token + 30s timeout fallback + trigger file `/tmp/ccteam-<user>.shutdown`)→ `docs/versions/v0-4-6/prd.md` F86 |
| **F87** | V0.4.6 | **closed** | clap `allow_hyphen_values` + `disable_help_flag` 在 `send` / `spawn` → `docs/versions/v0-4-6/prd.md` F87 |
| **F88** | V0.4.6 | **closed** | web bearer token 自动 clipboard(xclip → xsel → wl-copy → pbcopy → clip.exe fallback chain)→ `docs/versions/v0-4-6/prd.md` F88 |
| **F89** | V0.4.6 | **closed** | CLI 瘦身:删 V0.3 legacy(`phase` / `decisions` / `watchdog`),`hook` / `mcp-serve` / `spawn` / `send` / `peek` / `attach` / `progress` / `resume` 移到 `ccteam internal <subcmd>` 隐藏分组(老顶层保留 + WARN 到 V0.5)→ `docs/versions/v0-4-6/prd.md` F89 |
| **F90** | V0.4.6 | **closed** | Web WorkflowView 4 新面板(ArtifactQueuePanel / EventsTimelinePanel / FailureInspector / CostSparkline)+ 4 新 API endpoint → `docs/versions/v0-4-6/prd.md` F90 |
| **F91** | V0.4.6 | **closed** | cost SoT 收敛(删 `Hook::CostAccumulate` + `cost_summary` 实时读 `~/.claude/jobs/<id>/state.json::cost_usd_total`;`state.cost_used_usd` 字段 deprecated 但 serde-compat)→ `docs/versions/v0-4-6/prd.md` F91 |
| **F92** | V0.4.7 候选 | **open** | 真 cost 数据源(host `state.json` 没有 `cost_usd_total` 字段 — 真实数据在 `linkScanPath` jsonl event 的 Anthropic `usage` 字段)— 2026-05-16 V0.4.6 E2E 发现 |
| F93-F105 | V0.5.0 / V0.5.1 | closed | 详 `docs/versions/v0-5-0/README.md` + `docs/versions/v0-5-1/README.md`(F92 真 cost + agent-team mode + meta-agent reposition + host E2E SPA 可见性 + `--env` argv bug 等)|
| **F106** | V0.6.0 | ✓ shipped wave-跟随 | 红线表按"模式 × vendor"双轴 scope 重写 — `docs/tech-design.md §0` + CLAUDE.md §三;V0.6.0 ship 后所有 PR 必按双轴 check |
| **F107** | V0.6.0 | ✓ shipped wave 1 | `HarnessAdapter` trait Option C:扩展现有 trait 对齐 Codex `ThreadManager`(5 方法 thread/turn 接口 + `TurnInput` enum + `AgentVendor` enum + `ExecutionMode { InProc \| Bg \| Chat }`)— `crates/ccteam-core/src/harness.rs` |
| **F108** | V0.6.0 | ✓ shipped wave 2 | 模式 3 执行路径 Option C 决策 flip:**tmux 长 session + send-keys -l 直送 user content + dual-track(Claude Code 官方 hooks fast event 通道 + transcript jsonl byte-offset 增量读 → 镜像 ccteam-owned `turns.jsonl`)+ slash 命令透明透传**(综合 ccgram + OMC production 验证;弃上版 `claude -p --resume` + stream-json) |
| **F109** | V0.6.0 | ✓ shipped wave 2 | IM bridge 统一 `openhuman/channels` Rust crate(14+ IM 平台 cargo feature 门控);`claude-plugins-official/telegram` 作 backup transport(`/ccteam-im-setup --transport official-telegram` 切)|
| ~~**F110**~~ | — | **取消** | MCP namespace `ccteam` → `ct` rename **取消**(V0.5 用户肌肉记忆 override 4 字符节省);只保留 F110 "子前缀分组" 部分 → F111 |
| **F111** | V0.6.0 | ✓ shipped wave 3 | MCP 工具子前缀分组(5 group:`workflow_/chat_/advise_/admin_/screenshot`)+ `CCTEAM_DISABLE_TOOLS` group enum + 项目级 `.mcp.json`;server name 保持 `ccteam`;V0.5 用户配置零 break — `crates/ccteam-cli/src/{mcp_serve,mcp_workflow_tools,mcp_chat_tools,mcp_advise_tools,mcp_tool_groups}.rs` |
| **F112** | V0.6.0 | ✓ shipped wave 3 | Codex 集成 Option B 完整:`vendor: AgentVendor { Claude, Codex }` trait 一等公民 + `CodexExecAdapter`(模式 2 `codex exec --json`)+ `CodexAppServerAdapter`(模式 3 UDS JSON-RPC v2)+ 双 pricing table(`crates/ccteam-cost/pricing/{anthropic,openai}.toml`)+ per-vendor budget caps + 4 用户场景(advise vote / auto-critic / quota fallback / `/ccteam-team` Codex critic)|
| **F113** | V0.6.0 | ✓ shipped wave 1-2 | `/ccteam` 总入口 NL dispatcher slash + 5 sub-skill(Solo / Team / Overnight / Pocket / Squad)— `skills/ccteam/`(总入口 + 路由到 sub-skill)|
| **F114** | V0.6.0 | ✓ shipped wave 2 | `ccteam-creator` skill 复活 + NL 自动 mode 推断(用户说"想做个 TG 助理"→ Pocket Assistant preset → Routing 编排 → mode 3 tmux-bot)+ persona 预设库(技术助手 / 写作 / 翻译 / 学习辅导 5 个中文 persona)|
| **F115** | V0.6.0 | ✓ shipped wave 2 | `.ccteam/handoffs/<workflow>/<stage>.md` 决策摘要机制(researcher / writer / reviewer 链 stage 切换时落 markdown,下 stage bot `@读 handoffs/<stage>.md` 重建上下文,**不**再用户在 IM 粘贴)|
| **F116** | V0.6.0 | ✓ shipped wave 2 | `ccteam-imd` 独立 supervisor daemon binary(borrowed OMC `reply-listener.ts` 模式)— `crates/ccteam-imd/`,workspace member,守 openhuman/channels event bus + per-channel adapter task + HarnessAdapter call;替代"ccteam daemon + Anthropic 官方 TG MCP server"双进程 |
| **F117** | V0.6.0 | ✓ shipped wave 2 | `/ccteam-im-setup` 一次性 IM token onboarding skill(TG getMe + getUpdates auto-detect chat_id;后续 chat workflow 自动复用)— `skills/ccteam-im-setup/SKILL.md` |
| **F118** | V0.6.0 | ✓ shipped wave 3 | chat session 失效 last-N turn 重建(ccteam-owned `<project>/.ccteam/chat/<bot>/turns.jsonl` SoT;`recover_last_n_turns` 配置;新 TUI session 起后 submit `[Recovery] previous N turns: ...` turn 重建 context;`progress.jsonl` 写 `chat_session_reset { bot, recovered_turns: N }` event)|
| **F98** | V0.6.1 | ✓ shipped wave 2 | plan-approval ↔ outbox engine — workflow.yaml `agents[*].plan_approval:` block(`enabled` / `outbox` / `timeout_min` / `on_timeout`)+ `crates/ccteam-core/src/plan_approval.rs` pure state machine + 3 progress event(`plan_pending` / `plan_decision` / `plan_timeout`)+ IM round-trip(APPROVE / REJECT [<reason>] / EDIT <comment>)→ `docs/versions/v0-6-1/prd.md §F98` |
| **F119** | V0.6.1 | ✓ shipped wave 1 | `scripts/host-probe/run-probes.sh` daemon-start + `ccteam-imd health` CLI subcommand + `wait_for_health` 防 stale heartbeat 假阳性 + `CCTEAM_PROBE_SKIP_DAEMON_START` env override → `docs/versions/v0-6-1/prd.md §F119` |
| **F120** | V0.6.1 | ✓ shipped wave 1 | overnight-builder host probe full workflow — fake workflow.yaml + fake artifact + `ccteam start` + `wait_for_event agent_done` + assert progress.jsonl → `docs/versions/v0-6-1/prd.md §F120` |
| **F121** | V0.6.1 | ✓ shipped wave 1 | `ccteam doctor --check-pricing-version` — per-vendor 2-line 报告 + 3-state classifier(OK / warn 180d / ERROR 365d)+ `CCTEAM_TEST_NOW` env override(deterministic mock)→ `docs/versions/v0-6-1/prd.md §F121` |
| **F122** | V0.6.1 | ✓ shipped wave 1 | `CodexAppServerAdapter` → `progress.jsonl` bridge — `ProgressBridgeCtx` per-thread + `register_bridge` + `turn/completed` / `turn/failed` / `error` notifications → `agent_done` rows tagged `vendor: codex` + cost 累加进 `cost_24h_by_vendor["codex"]`;闭 V0.6.0 Wave 3 D9 retained risk → `docs/versions/v0-6-1/prd.md §F122` |
| **F123** | V0.6.1 | ✓ shipped wave 3 | 5 demo GIF 录制(asciinema → agg)— `docs/versions/v0-6-0/demos/{30s-solo-sidekick,30s-team-sprint,60s-overnight-builder,30s-pocket-assistant,60s-im-squad}.gif` ≤500KB / 90×30 + demos/README.md recipe → `docs/versions/v0-6-1/prd.md §F123` |
| **F124** | V0.6.1 | ✓ shipped wave 2 | `WorkflowMode::HumanApproval` 第 4 mode(narrow scope)+ orchestrator pick_adapter / dispatch gate / `poll_completions` skip drain for paused agents;CLAUDE.md §三 红线 row `HITL approval state SoT`;协作 F98(F124 owns mode enum + dispatch arm;F98 owns IM round-trip + decision injection)→ `docs/versions/v0-6-1/prd.md §F124` |
| **F125** | V0.6.1 | ✓ shipped wave 1+3 | 全局文档审计 + cross-doc 一致性 sweep + 历史归档清理 — Wave 1 doc-curator(drift grep 全 0 + V0.5 旧 API sweep)+ Wave 3 doc-syncer finalize(本 row 即产出)→ `docs/versions/v0-6-1/prd.md §F125` |
| **F126** | V0.6.1 | ✓ shipped wave 1 | `README.md` EN-only rewrite(80 行,保 3-mode 平等 + 5 preset + 三入口 narrative + 删 Status/版本进展段)+ CLAUDE.md §三 2 红线 row(`root README.md MUST be English` + `README.md 不含版本进展/状态信息`)+ 解释段(版本进展去 `docs/versions/v0-X-Y/README.md`)→ `docs/versions/v0-6-1/prd.md §F126` |
| **F127** | V0.6.1 | ✓ shipped wave 3 | user-manual.md 100% 亲测可用 sweep + 端到端用户操作模拟(扩为 8-path E2E sim:Solo / Team / Overnight / Pocket / IM Squad / Plan-approval / Codex / HITL);ship policy = sim 100% clean 才 tag v0.6.1;sim 发现 bug 在本版修(no V0.6.2 split)→ `docs/versions/v0-6-1/prd.md §F127` |
| **F128** | V0.6.1 | ✓ shipped wave 2 | `/ccteam-control change-persona` + `add-tool` subcommand — `crates/ccteam-core/src/admin_actions.rs` pure file-mutation engine + 2 MCP tool(`admin_change_persona` + `admin_add_tool`,admin group 1→3)+ `skills/ccteam-control/SKILL.md` 子命令文档 + emit `persona_changed` / `tool_added` event → `docs/versions/v0-6-1/prd.md §F128` |
| **F129** | V0.6.1 | ✓ shipped wave 2 | `@ccteam` IM NL admin via meta-agent — `crates/ccteam-imd/src/{inbound, nl_admin}.rs` 检测 `@ccteam <NL>` mention(在 `@<bot>` route 之前)+ 5 keyword admin action(pause / resume / list / cost / stop everything)+ 危险动作 2 步 confirm flow(`stop everything` 二次 CONFIRM);hop_limit 不消耗 → `docs/versions/v0-6-1/prd.md §F129` |
| **F130** | V0.6.1 ship-day | ✓ shipped (in-place patch) | `ccteam-imd` 折入 `ccteam start`(单进程 daemon)— 删 `crates/ccteam-imd/Cargo.toml::[[bin]]` + `src/main.rs`,`crates/ccteam-imd/src/daemon.rs` 新增 `run_daemon_with_shutdown<F>` 接受外部 shutdown future + `run_daemon` 降级为 SIGINT-only wrapper;`crates/ccteam-cli/Cargo.toml` 加 `ccteam-imd` dep,`run_start` 内多一个 `imd_handle` tokio task(镜像 `web_handle` 模式,共享 `watch::channel` shutdown);加 `--no-imd` CLI flag;删 `ccteam daemon {start,stop,status}` 子命令(folded into `ccteam start`,无 shim);probe `daemon_start_snippet` 改 `ccteam start --no-web` + heartbeat 文件 poll;overnight-builder probe 加 `--no-imd` 避免污染 real `$HOME` → ship-day 修复"3 个常驻进程→1 个"用户诉求("僵尸进程排查") |
| **F140** | V0.6.2 | ✓ shipped | per-role 代码 `scope` —— `AgentSpec.scope: Option<PathBuf>` + `AgentSpec::cwd(project_dir)` + `validate_scope`(path-traversal guard);orchestrator `try_spawn_with_prompt` 的 `SpawnCtx.cwd` 由硬编码 `project_dir` 改为 `agent.cwd(project_dir)`。源自 Anthropic《How Claude Code works in large codebases》—— 红线 R3 给 fresh 1M 窗口,但 fresh≠scoped;`scope` 把每次 spawn 的 cwd 钉到子树,收窄爆炸半径。inner/outer harness 分层下 ccteam 只吸收"拓扑"这条 → `docs/versions/v0-6-2/README.md` |
| **F141** | V0.6.2 | ✓ shipped | `ccteam-scan` skill —— 大型代码库导航性体检(只读 audit)。`skills/ccteam-scan/SKILL.md` + `skill.rs` 接线(`CCTEAM_SCAN_SKILL_NAME` / `CCTEAM_SCAN_SKILL_MD` include_str! / `install_ccteam_scan_skill`)+ `ccteam doctor --install-skill` 第 4 个 shipped skill。探测 monorepo 结构 + 为每个子系统建议 F140 `scope:` 值 + 报告 navigability gap;是 `orchestration-patterns.md §1.5` explorer→artifact→editor 模板里 `explorer` role 的一次性交互版。只读 advisory(唯一写动作 = `.ccteam/codebase-scan.md` 报告)→ `docs/versions/v0-6-2/README.md` |
| **F142** | V0.6.3 | ✓ shipped | `Trigger::Schedule` 接真 cron —— 收尾 V0.4.6 stub。新模块 `ccteam-core::cron`(`croner` crate,5 段标准 cron,6/7 段 seconds 形预拒)+ `AgentSpec::interval` → `schedule: Option<String>` 字段 rename + `validate()` 加载时 parse + orchestrator tick 评估 due agent + `ProjectState::schedule_last_fire` per-(project,role) 持久化 + **skip-missed 语义**(daemon down 一次 fire 一次,不补跑、不重启风暴)→ `docs/versions/v0-6-3/README.md` |
| **F143** | V0.6.3 | ✓ shipped | webhook ingress —— `POST /webhook/:project/:token` on the existing `ccteam start` axum web server。per-project 64-hex secret(`<project>/.ccteam/webhook-token`,mode 0600,lazy-gen,constant-time compare via `subtle`)+ 256 KiB body limit → 413 + 鉴权失败 → 401 + valid → 写 `<project>/.ccteam/webhooks/<ts>-<rand>.json`;agent 用现成 `trigger: watch:` 消费,**`Trigger` enum 零改动**;`ccteam-web` 反向依赖 `ccteam-cli` 仍 0 命中 → `docs/versions/v0-6-3/README.md` |
| **F144** | V0.6.3 | ✓ shipped | vendor 接缝 forward-compat —— `ccteam-core::vendor_compat::warn_unknown_vendor_token` 进程级 warn-once helper(`OnceLock<Mutex<HashSet>>`)+ 未知 Claude job state → `JobLiveness::Running`(非终态续 probe,**不**误判 done 留 phantom)+ 未知 `codex exec --json` event / `codex app-server` notification → skip + warn-once。vendor 输出解析全程从「静默降级」升级为「可观测降级」+ 13 回归测试锁死(synthetic future-JSON 不 panic);scope 严格锁定:**只动 vendor-output 结构,不碰 ccteam-owned schema**(与「不做历史迁移」红线无冲突,两类不同数据)→ `docs/versions/v0-6-3/README.md` |
| **F145** | V0.6.3 | ✓ shipped | 跨 session 运行时路由 —— workflow.yaml 顶层 `squad: { leader, members, hop_limit }` 块(成员静态声明 → 「声明式拓扑」红线守);leader 写 `<member>--*.md`(可选 `<member>--h<N>--*` re-route)到 `<project>/.ccteam/squad/`,orchestrator ArtifactWatcher 加 `squad_root` extra-root + `SQUAD_ROUTE_SENTINEL` tag,`handle_squad_route` 按文件名前缀 spawn 对应 member(无 file-body parsing → R3 守);hop_limit 默认 3 → 超限 `escalation`(`kind: squad_hop_limit`)+ 未知 member 前缀 `kind: squad_unknown_target` → `docs/versions/v0-6-3/README.md` |

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

V0.4.6 起 audit 文档**只列 open finding** + V0.3.2+ 索引(见 §"V0.3.2+ 索引");已 close finding 的详细描述住版本 dir(`docs/versions/v0-X-Y/{prd,dev-plan}.md`)。CLAUDE.md §五.3 "Pre-v1.0 不留技术债"原则:本节不保留任何已 close finding 的描述。

### F15 — settings.json 模板未含危险命令拦截(P1)

- **位置**:`crates/ccteam-core/src/templates/settings.json`(`PostToolUse` matcher)
- **状态**:M0 模板无 `Bash:git push.*` 拦截;`block-push` hook 还没实现。
- **触发时机**:M1+ 实装 `block-push` 时,team.yaml 加 `danger_command_patterns: [{ pattern, reason }]`,`render_project_settings` 按 team 参数注入 matcher。**不**直接写 `Bash:git push.*` 字面量。

### F17 — 测试用例硬编码 dev phase 名(P2)

- **位置**:`crates/ccteam-core/tests/state_machine_test.rs`(V0.4.0 后已大部分迁移,残留 V0.1 测试需 sweep)
- **状态**:V0.4.0 phase EOL 后这些测试本身大部分已删。残留的应跟 V0.5 ralph-loop / clippy sweep 一起 rename + 移到 `tests/team-dev/`。

### F23 — 容器 bind-mount `~/.claude/rules/`(P1 conditional)

- **位置**:N/A(spike 验证)
- **状态**:F22 修复后 spike §4 已解锁,等谁跑一次容器内 `--dangerously-skip-permissions` 验证 `~/.claude/rules/*.md` 是否被 Claude Code 当 context 注入。spike 失败才升 P0;详 `docs/versions/v0-1/m4-spike.md` §4。

### F92 — cost 数据源真相(V0.4.7 候选)

- **位置**:`crates/ccteam-core/src/queries.rs::cost_summary` + `claude_job::probe_state_json`
- **状态**:V0.4.6 F91 收敛 cost SoT 到 `~/.claude/jobs/<id>/state.json::cost_usd_total` — 但 host probe 显示 cliVersion 2.1.143 的 state.json **没有这字段**!真实 cost / token / model / rate 数据在 `state.json::linkScanPath` 指向的 jsonl 文件里(每个 Anthropic API event 的 `usage` 字段)。
- **修复方向**:V0.4.7 加 `claude_usage` 模块,parse linkScanPath jsonl tail 聚合 → `CostSummary` 扩展含 token / model / rate。F84 budget 同时支持 token-based caps。F90 SPA 加 token / model / rate sparkline。
- **优先级**:**P1**(影响 web UI cost 显示 + budget enforce 准确性)。V0.4.6 e2e 实测发现。

### F102 — F80 stale-spawn 漏 `state=working` 卡死(V0.4.7 候选)

- **位置**:`crates/ccteam-core/src/claude_job.rs::probe_state_json` + `orchestrator.rs::poll_completions` F80 stale-spawn cleanup
- **症状**:dex-ui qa-autoloop 实测,daemon 重启后 11 个 claude `--bg-spare` worker 在 OS 仍存活(`ps -ef | grep bg-spare` cwd=dex-ui 都列出),但其 state.json **`state: "working"` 且 `updatedAt` 冻结于 daemon 上次活跃时刻**(75+ min 前)。F80 现行 cleanup 只在 `state.json::probe_state_json` 返回 `JobLiveness::Terminal` 时合成 `agent_done`;`state=working` 一律视作 `Running`,无 staleness 判断 → progress.jsonl 永远缺这些 spawn 的 done 事件 → web UI 显示 ghost-running 长期不消。手动 `kill -TERM <pid>` + 手写 `agent_done` 才恢复一致(本次 F102 现场)。
- **触发场景**:daemon 重启 / SIGKILL / pty-host parent 死亡时,claude `--bg-spare` worker 可孤儿存活但停止心跳 state.json。同样可能在 claude 内部死锁 / 长 hang 时复现。
- **修复方向**(refined per 2026-05-17 live observation):**不能只看 state.json::updatedAt** — 实测 cliVersion 2.1.143 下,agent 在长 phase 期间 state.json `state=working, detail="starting…"` 一冻就是 5+ min,但 progress.jsonl 同期持续有 `PreToolUse` / `PostToolUse` 事件。state.json 只在 claude lifecycle event 时更新,不在 tool use 时更新。单纯阈值会**误杀**活 agent。
- **正确判据**:`probe_state_json` + 跨表查 progress.jsonl:
  - 对每个 open `agent_spawn`(无匹配 `agent_done`),取 `spawn.ts`
  - 在同 slug 的 progress.jsonl 找 ts > `spawn.ts` 的任意 `PreToolUse` / `PostToolUse` / `Stop` hook 事件
  - **有**且最新 ts < 30 min 前 → Running
  - **无**任何 hook event > spawn.ts 持续 30 min → 视作 stuck,合成 `agent_done{status: "killed"}`
  - 阈值可项目级配(`workflow.yaml::stale_no_progress_minutes`,默认 30)
- **替代/补充**:让 ccteam 的 hook 自身写一个 per-session liveness 文件(`~/.ccteam/heartbeats/<sid>`),每次 PreToolUse touch 一下。orchestrator 看 mtime。比绕 progress.jsonl 干净,但加新文件协议;先看 progress-jsonl 跨表方案能不能直接落。
- **测试**:unit 覆盖 `probe_with_progress_cross_ref`(stub progress events for sid → 期望 Running)+ integration:模拟 spawn 但永不写 hook event,> 30 min 后 poll_completions 合成 done。
- **优先级**:**P1**(任何 daemon 重启都暴露;影响 UI 一致性 + parallelism 计数 + cost 双重计入)。V0.4.6 dex-ui qa-autoloop 实测踩到。
- **关联**:V0.4.6 `5da83dc` parallelism race fix 修了**race 路径**;F102 修**daemon-restart-leftover 路径**。两者互补,合并解决 dex-ui 长期"幽灵 session"问题。

---

## 历史(F1-F91)

完整 finding 历史 detail 已**移到版本 dir**(`docs/versions/v0-X-Y/{prd,dev-plan}.md`),按 ship 时间顺序索引在上面 "V0.3.2+ 索引" 表 + V0.2 §6 反模式状态表里。本文不再 inline 重复(CLAUDE.md §二 三类文档维护规则 + §五.3 "Pre-v1.0 不留技术债")。

V0.1-V0.3.1 时代的 finding 详细 audit 文本(F1-F51)2026-05-16 删除前最后版本见 git history(`107ccb2` 之前);其论点已被 V0.4.0 F60 phase 删除 + V0.4.0+ workflow.yaml 架构整体 supersede。
