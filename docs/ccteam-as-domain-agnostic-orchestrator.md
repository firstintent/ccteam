# ccteam 作为领域无关编排层

> **实施状态(2026-05-16,V0.4.6)**:本文档原是 M3 团队抽象的 charter。
> **M3 已落地**(commit `0d88ddc` PR #7 + 后续 M3.x 子 PR ship);**V0.4.0 phase
> 机制彻底拆除,换 workflow.yaml + ArtifactWatcher + thin orchestrator 数据驱
> 动架构**(F60-F69);**V0.4.5 F78 watcher 项目相对路径修复 + F80 phantom
> agent_spawn cleanup**。已 ship 版本:V0.1–V0.4.6(V0.4.0 phase→workflow 重
> 构 / V0.4.1 UX 简化 / V0.4.2 ccteam init + 全局 config.yaml / V0.4.3 slug
> grammar / V0.4.4 任意路径 hook / V0.4.5 watcher + phantom cleanup / V0.4.6
> workflow.yaml `enabled` + 热加载 + budget + `ccteam remove`)。**dev-coupling
> 审计**(`docs/dev-coupling-audit.md`):F1-F91 共 57 条 distinct findings,~88
> 已 close,剩余 F15(M1+ deferred)/ F17(P2 边角)/ F23(spike conditional)/
> V0.4.6 in-flight。**M4 跨项目记忆**走官方 `~/.claude/rules/` + per-repo
> auto-memory(M4.1–M4.4)亦已 ship。本文继续保留作为团队抽象的**永久 charter**
> ——未来再加新团队(marketing / ops / ...)时,§1 责任分界表 / §2 团队扩展契
> 约 / §3 显式拒绝清单仍是**首要参考**;**§1.4 workflow-as-data 红线**(V0.4.6
> 新增)钉死 workflow.yaml 不许写 prompt 字面量。
>
> 本文回答一个问题:**ccteam 当前长得像"一个 dev 团队的编排器",但它的核心
> 编排机制实际上与"做软件"这个领域无关——把 phase 集合换一组,同一台机器
> 应该能跑研究团队、营销团队、运营团队**。本文负责把"编排层 vs 领域层"
> 的责任分界正式钉死,给出未来扩展新团队时的契约,并显式列出**编排层不应
> 替领域决策**的边界。
>
> 阅读位置:本文是 `tech-design.md` / `interfaces.md` 之上的**抽象层 charter**——
> 不替代它们,只是把已经做出来的机制重新归类。任何 PR 引入新机制时,先回
> 本文的 §1 责任分界表问"这是编排层职责还是领域层职责",再决定该写到哪里。
>
> **本文不重新选型**——所有架构决策(tmux 长 session / progress.jsonl 唯一事实
> 来源 / context 60% reset / idle-aware 注入 / ralph-loop fix-cycle 范式 / 三层
> 防御)沿用 tech-design.md。本文只负责把它们标"哪些是 mechanism、哪些是 dev
> 团队的具体填充"。

---

## 0. TL;DR

- **领域无关的部分(mechanism)**:进程拓扑、状态机、文件协议、事件流、
  context 管理、注入策略、cost/stall 软告警、ralph-loop 拦截范式、ESCALATE
  语法外壳、phase YAML schema、subagent/skill/MCP 三类工具触发面、防御层
  的 PASS/CONCERN/BLOCK 三档汇总——这些都不假设你做的是软件。
- **领域特定的部分(domain fill)**:phase 集合、`FIRST_PHASE` / `FIX_PHASE_NAME`
  这种硬编码、fix-loop 完成信号 `TESTS_GREEN`、6 维评分维度、推荐 plugin
  agent 清单、artifact 文件命名(`spec.md` / `plan-eng.md` / ...)、危险命令
  拦截清单、Critic 视角集——这些假设了"你在做软件"。
- **当前代码用"约定假设"代替了"显式参数"**(2026-05-05 原写,V0.4.0 重构后整
  段失效但保留作历史):`orchestrator.rs` 原写死 dev pipeline 的 6-phase DAG,
  `fix_loop.rs` 写死 `TESTS_GREEN`,`commands.rs` 写死要找的 artifact 名。
  M3 把 DAG 提取为 `crates/ccteam-core/src/dag.rs`(从 phase 模板推断);
  F8(`collect_artifacts` 硬编码 artifact 列表)2026-05-07 PR close;
  F1 / F5-F7 / F18(`fix_loop` → `auto_loop` 命名 sweep)2026-05-07 rename PR close。
  **V0.4.0(F60-F69)整套 phase 状态机彻底拆除** —— `orchestrator.rs` 2713 LOC
  → ~820 LOC thin orchestrator,改读 workflow.yaml + ArtifactWatcher fsevents
  事件驱动;`fix_loop.rs` / `auto_loop.rs` 全删;`dag.rs` 全删。`ccteam-core`
  无 dev 字面量,**workflow.yaml 是数据驱动入口**(§2.1)。
- **首个非 dev 团队选 research**——工具面与 dev 重叠最多(同一批 plugin
  agent 复用率高、不需要新 MCP),但**完成信号**(tests pass → "3 个一手数
  据来源经过 Critic 交叉验证")与 **fix-loop 语义**(改代码 → 重设假设 +
  重做 primary)需要彻底重新定义——正好暴露抽象漏洞。

---

## 1. 责任分界表

按"概念 → 是 orchestrator(domain-agnostic)还是 team(team-specific)"逐项归
类。**这张表是 §B 审计代码、§C 起草新团队 phase 集时唯一的判定基准**。

记号:
- 🟢 **orchestrator**:领域无关,代码必须不假设领域语义,可被任何团队复用。
- 🟡 **team-config**:领域特定,但在编排层用**配置 / 数据 / 字符串**承载——
  代码不写死,通过 phase 模板 / `team.yaml` / 推荐清单注入。
- 🔴 **team**:领域特定,**只能在该团队的 phase markdown / agent 文件 /
  hook 脚本里**实现,绝不进入 `ccteam-core`。

### 1.1 状态与协议

| 概念 | 归类 | 论证 |
|---|---|---|
| `state.json` 字段 `slug` / `tmux_session` / `claude_session_id` / `claude_pid` / `parallelism` | 🟢 | 进程身份与执行栈状态——所有团队共用 |
| `state.json` 字段 `current_phase` | 🟢→**V0.4.0+ deprecated** | serde-compat 字段保留,新写不带;F66 thin orchestrator 完全不消费 |
| `state.json` 字段 `phase_history[]` | 🟢→**V0.4.0+ deprecated** | 同上,业务 SoT 改 progress.jsonl |
| `state.json` 字段 `phase_state`(枚举 `in_flight` / `idle` / `fix_locked` / `auto_locked`) | 🟡→**V0.4.0+ deprecated** | V0.4.0 phase 状态机拆除;字段 serde-compat 仅 |
| `state.json` 字段 `cost_used_usd` / `soft_warn_threshold_usd` / `hard_kill_threshold_usd` | 🟢 | $ 与 token 是 LLM 推理的物理事实,不分领域;**V0.4.5 起 cost_used_usd 改从 progress.jsonl agent_spawn 汇总** |
| `state.json` 字段 `context_tokens_used` / `context_reset_threshold_tokens` / `context_reset_count` | 🟢 | claude 进程级别的事实 |
| `state.json` 字段 `last_progress_event_at` / `last_event_type` / `user_attached` / `user_pause_pending` | 🟢 | 调度器时间线事实 |
| `state.json` 字段 `fix_cycle_count` | 🟡→**V0.4.0+ deprecated** | "fix"语义死随 V0.4.0 phase 拆除;serde-compat 仅 |
| `parallelism` 枚举 `solo` / `agent_team` / `multi_session` | 🟢 | 三档并行规模是抽象的执行栈拓扑;V0.4.0+ 由 workflow.yaml `AgentSpec::parallelism` 数据驱动 |
| `state.json` 字段 `sessions{}` / `next_sid_seq{}`(V0.3.1+ flex multi-session) | 🟢 | flex team 多 harness session 状态记录 |

### 1.2 progress.jsonl 事件类型

| 事件 | 归类 | 论证 |
|---|---|---|
| `session_start` / `SessionEnd` | 🟢 | claude 进程生命周期 |
| `PreToolUse` / `PostToolUse` / `SubagentStop` | 🟢 | Claude Code hook 透传 |
| `Stop` / `notification` | 🟢 | idle 信号 |
| `phase_inject` / `phase_done` / `phase_milestone` / `golden_rules_check` | **V0.4.0+ EOL**(F60) | phase 机制 V0.4.0 拆除;旧 progress.jsonl 仍可读但新写不出 |
| `workflow_start` / `agent_spawn` / `agent_done` / `artifact_received` / `gate_triggered` / `budget_exceeded` / `workflow_done` | 🟢(V0.4.0 引入,**7 类业务 event**) | 调度器→执行体的握手协议;`reason` / `role` 字段内容是 🟡 |
| `escalation` | 🟢 | 调度器层语义,**reason 字段内容**与"该升级到哪个修复路径"是 🟡(详见 §1.6 ESCALATE 语法) |
| `watcher_concern` / `watcher_block` | 🟢 | mechanism;具体哪些 watcher、看什么是 🟡 |
| `context_reset` | 🟢 | 60% 阈值机制 |

### 1.3 workflow.yaml schema(V0.4.0+ 主拓扑入口)

**V0.4.0 F63 起,workflow.yaml 是项目拓扑 SoT**;phase YAML 已 EOL(下表 1.3a 仅作历史)。

| 字段 | 归类 | 论证 |
|---|---|---|
| `WorkflowSpec::name` | 🟢 | 字段是 mechanism;取值是 🟡 |
| `WorkflowSpec::description` | 🟢 | 给 meta-agent / UI 看;内容是 🟡 |
| `WorkflowSpec::enabled`(V0.4.6 F82) | 🟢 | 软开关 + 热加载;所有团队共用 |
| `WorkflowSpec::budget`(V0.4.6 F84) | 🟢 | rolling cap;数字由 team / 项目注入 → 取值是 🟡 |
| `WorkflowSpec::agents{role: AgentSpec}`(IndexMap) | 🟢 | 字段 + 顺序语义是 mechanism;role 名是 🟡 |
| `AgentSpec::executor`(`claude` / `codex`) | 🟢 | HarnessAdapter 选择 |
| `AgentSpec::trigger`(`manual` / `schedule` / `gate` / `watch:<path>`)| 🟢 | 4 类触发语义全 mechanism;`watch:` 路径是 🟡 |
| `AgentSpec::parallelism: Option<u32>` | 🟢 | 通用并发上限;仅 `Watch` 触发有意义 |
| `AgentSpec::input` / `AgentSpec::output`(项目相对路径) | 🟢 | 字段名是 mechanism;路径是 🟡 |
| `AgentSpec::interval`(opaque string,V0.4.6 还未 cron 化) | 🟢 | `schedule` 配套;数字是 🟡 |
| `AgentSpec::timeout` + `on_timeout`(`escalate` / `retry` / `skip`) | 🟢 | watchdog 三档语义全通用 |
| **prompt 内容字段** | **明令禁止** | 见 §1.4 workflow-as-data 红线 |

#### 1.3a phase YAML 字段(**V0.4.0+ EOL**,仅作历史)

`team.yaml::kind: workflow`(phase 驱动)在 V0.4.0 终结。下表保留为 V0.1-V0.3 旧字段索引。

| 字段 | 归类 | 论证 |
|---|---|---|
| `name` / `required_inputs` / `required_outputs` / `soft_cost_warn_usd` / `stall_warn_minutes` / `parallelism` / `agent_team` / `sub_skills[]` / `hooks.before` / `hooks.after` / `tools_required` / `completion_signal` | **V0.4.0+ EOL**(F60) | phase YAML 全部废,旧 `phases/` 目录 V0.4.0 PR #1 删 |

### 1.4 Workflow-as-data 红线(V0.4.6 加固)

> **核心红线**:workflow.yaml 是**拓扑数据**——连线 + trigger 类型 + 并发上限 +
> 软开关 + budget,**绝不出现 prompt 字面量**。agent 行为 prompt 住 `.claude/agents/
> <role>.md`(那是 LLM-prompt content 层,**由 Claude Code 加载,不由 ccteam 解析**)。
> ccteam orchestrator **不读 prompt,不路由 prompt,不在状态机里做"如果 prompt
> 提到 X 就 Y"判断**——orchestrator 是文件系统事件调度器,不是 NL 中间件。

**workflow.yaml 字段允许出现什么**:

| 项 | 归类 | 允许? |
|---|---|---|
| 拓扑(`agents{role: ...}` IndexMap) | mechanism | ✅ |
| trigger 类型(`manual` / `schedule` / `gate` / `watch:<path>`) | mechanism | ✅ |
| `enabled: bool` 软开关 + 热加载 | 🟢 orchestrator-level | ✅ |
| `budget: {max_cost_usd_per_24h, max_agent_spawns_per_hour}` | 🟢 orchestrator-level | ✅ |
| `parallelism: u32` 并发上限 | 🟢 orchestrator-level | ✅ |
| `executor: claude|codex` harness 选择 | 🟢 orchestrator-level | ✅ |
| `input` / `output` 路径 | 🟢 数据流契约 | ✅ |
| `timeout` + `on_timeout` watchdog | 🟢 orchestrator-level | ✅ |
| **agent 行为指令 / prompt 文字 / role 描述长文本** | LLM-content 层 | ❌ → `.claude/agents/<role>.md` |
| **决策树 / 状态机分支描述** | LLM-content 层 | ❌ → meta-agent CLAUDE.md or agent.md |
| **"如果 X 就 Y"条件分支字面量** | 工作流条件 | ❌(workflow.yaml 条件分支是 V0.5+ 候选,仍走数据 schema 不写 prompt)|

**违反这条红线的 PR 应被拒收**——orchestrator 解析 prompt 内容等于在 ccteam-core
内嵌 LLM,违反 §3 显式拒绝清单(无新增条目但精神一致)+ Symphony 反模式红线
(channel adapter 进程内不嵌 LLM,§7.4)。`.claude/agents/<role>.md` 是 Claude
Code 的官方 subagent definition 路径,ccteam 只负责文件存在性校验(`ccteam doctor
--validate-workflow`),不负责语义解析。

**工程含义**:
- `crates/ccteam-core/src/workflow.rs::WorkflowSpec` 是上述字段的 SoT,新字段加入
  前先回本节问"这是拓扑数据还是 prompt 内容?"
- `ccteam-creator` skill(V0.4.4)生成 workflow.yaml 时严格按上表;`.claude/agents/
  <role>.md` 由用户/skill 手写,ccteam 不读
- meta-agent dispatch 时**不**解析 `<role>.md` 内容 — 只看 workflow.yaml 拓扑 + 通
  过 `mcp__ccteam__spawn_agent(role=...)` 派单,Claude Code 自行加载 `<role>.md`

### 1.5 orchestrator 决策点(thin orchestrator;F66 V0.4.0)

| 决策点 | 归类 | 论证 |
|---|---|---|
| `decide_tick` 的状态机分支(NoOp / Advance / Escalate / Dispatch) | **V0.4.0+ EOL** | F66 thin orchestrator 改读 workflow.yaml + ArtifactWatcher 事件驱动;`decide_tick` 全删 |
| `next_phase()` / `is_terminal()` / `FIRST_PHASE` / `FIX_PHASE_NAME` / `FIX_LOOP_MAX_ITERATIONS` / `M0_PHASE_DAG` / `completion_signal: "TESTS_GREEN"` | **V0.4.0+ EOL**(F60 phase machinery removal + 2026-05-07 fix→auto rename PR) | 整组 phase 状态机字面量随 V0.4.0 拆除;无 backwards-compat shim |
| `dag.rs::PhaseDag::infer_from_templates` | **V0.4.0+ EOL**(F60) | DAG inference 死随 phase 拆除;workflow.yaml `agents` IndexMap 保留 YAML 声明顺序作 trigger 图,无需 DAG inference |
| `WorkflowSpec::enabled`(V0.4.6 F82)+ 热加载 | 🟢 | mechanism 通用;workflow 实例软开关,所有团队共用 |
| `WorkflowSpec::budget`(V0.4.6 F84)| 🟢 | rolling 24h cost cap + 1h spawn cap;数字由 team / 项目配置 → 取值是 🟡 |
| ArtifactWatcher inotify/fsevents 触发 | 🟢 | 文件系统是控制平面(tech-design §2.2 红线);watch 路径是 🟡 |
| 60% reset 阈值 | 🟢 | claude 进程级事实 |
| idle-aware 注入(Stop/notification → 直注;否则 /btw) | 🟢 | mechanism |
| cost ladder 阈值($20 / $50 / $200) | 🟢 | 默认值是 mechanism;数字本身可由 `~/.ccteam/config.yaml` 与 workflow.yaml `budget` 覆盖 |
| stall ladder(5/15/30 min) | 🟢 | 同上 |

### 1.6 Hook 与文件路径

| 文件 / 路径 | 归类 | 论证 |
|---|---|---|
| `~/.ccteam/{inbox,queue,control,progress}` | 🟢 | 全局编排层布局(M4 后无 `memory/`、V0.4.0 后无 `phases/` — 跨项目记忆走官方 `~/.claude/CLAUDE.md` + `~/.claude/rules/`;V0.4.2+ 加 `~/.ccteam/config.yaml` 全局 SoT 注册表 + V0.4.6 加 `~/.ccteam/projects/<slug>/`(state.json / progress / inbox / control 全在此)|
| `~/projects/<slug>/.ccteam/state.json` | 🟢 | 项目元数据 mechanism |
| `~/projects/<slug>/.ccteam/spec.md` | 🟡 | 字段名是 mechanism;research 团队等价物可能叫 `topic.md` / `brief.md`——应在 team 配置中声明 |
| `~/projects/<slug>/.ccteam/<phase>-report.md` | 🟢 | 命名 pattern 是 mechanism(`<phase>-report.md`);具体哪些 phase 是 🟡 |
| `~/projects/<slug>/.ccteam/escalation.md` / `fix-loop.state.md` | 🟢 | 通用控制文件 |
| `commands.rs::collect_artifacts` 硬编码 artifact 列表 | 🟡 | 应改为"扫 `.ccteam/*.md` 自动列出",或来自 team 配置 |
| `block-push` hook(M1+) | 🔴 | "git push"是 dev 团队的危险命令;research 团队的危险命令可能是"未经审阅就发用户邮件" |
| `security_reminder_hook.py`(plugin) | 🔴 | 完全 dev-specific |
| project CLAUDE.md "不要 git push / 测试不过不算完成" | 🔴 | dev 团队 CLAUDE.md 模板;research 团队需另一份 |

### 1.7 ESCALATE grammar(M0.5.4 引入,本文档加固)

| 语法档 | 归类 | 论证 |
|---|---|---|
| `ESCALATE: REVERT_TO_PHASE <name> — <reason>` | 🟢 | mechanism;`<name>` 取值是 🟡 |
| `ESCALATE: NEED_USER_INPUT — <questions>` | 🟢 | 用户在回路,所有团队共用 |
| `ESCALATE: ABORT — <reason>` | 🟢 | 永久标 failed |
| **建议新增** `ESCALATE: HYPOTHESIS_REJECTED — <reason>` 等团队特化前缀 | 🟡 | mechanism 是"team 可注册自己的前缀";前缀本身是团队语义 |
| 无前缀时降级为 `NEED_USER_INPUT` | 🟢 | mechanism |

### 1.8 防御层(L1 / L2 / L3)与 watcher

| 概念 | 归类 | 论证 |
|---|---|---|
| L1 `required_outputs` 校验机制 | 🟢 | 通用 |
| L1 危险命令拦截**机制** | 🟢 | hook + matcher 是 mechanism |
| L1 危险命令**清单**(git push / rm -rf / deploy 脚本) | 🔴 | dev-specific |
| L2 audit agent 调度 mechanism(PASS/CONCERN/BLOCK 三档输出) | 🟢 | 通用 |
| L2 audit 角色集(architect / critic / designer / security / scope-watcher) | 🔴 | dev 团队的 critic 视角;research 团队需要 method-critic / source-quality-critic / hypothesis-falsifiability-critic |
| L2 cross-cutting watcher(cost-watcher / scope-watcher / drift-detector) | 🟡 | cost-watcher 通用;scope/drift 假设了"有 spec / 有 plan-eng",research 也有等价物(topic / hypothesis)但需重命名 |
| L3 用户 fork 决策 | 🟢 | 通用 |
| 信任档位 `yolo` / `balanced` / `careful` | 🟢 | 通用 |

### 1.9 CLI 命令

| 命令 | 归类 | 论证 |
|---|---|---|
| `ccteam new <brief>` / `ls` / `show` / `start` / `stop` / `attach` / `peek` / `progress` / `pause` / `resume` / `answer` | 🟢 | 都是 mechanism——多一个 `--team=<name>` 参数即可泛化(详见 §3) |
| `ccteam doctor --tool-surface`(M0.5) | 🟢 | 校验 mechanism;校验对象是 🟡 |
| `ccteam doctor --install-recommended-agents` 的**清单** | 🟡 | dev 团队推荐 8 个 plugin agent;research 团队需另一份 |
| ~~`ccteam memory rebuild`~~ | — | M4 简化后无自建索引,该命令不存在;跨项目检索走 Claude session 内官方 `/memory` + 可选 `mcp__*claude-mem*search` |
| `ccteam fork-reply`(M1+) | 🟢 | 通用 |

### 1.10 跨项目记忆

> **2026-05-06 重塑**:M4 不再自建索引/向量库,完全复用 Claude Code 官方机制。
> 详见 `docs/tech-design.md §3.7` + `references/research/claude-code-memory-research.md` §六。

| 概念 | 归类 | 论证 |
|---|---|---|
| `~/.claude/projects/<encoded>/memory/` per-repo auto-memory | 🟢 | 官方机制,Claude 自主写;通用,与 ccteam-core 解耦 |
| `~/.claude/rules/ccteam-lessons-<team>.md` 跨项目共享 | 🟢 | 通用机制(rules + `paths:` frontmatter scope `~/projects/<team>-*`,**F22 修复后 slug 加 team 前缀,scoping 实际生效**);ccteam doctor `--install-memory-bridge`(M4.2)落实文件骨架,retro phase(M4.1)写 marked section |
| `team.yaml.retro_schema[]` 字段定义 | 🟡 | 当前 dev 已填 4 字段,product-research M4.1 必须补;mechanism 是"按 team 定义 retro 字段段落" |
| 召回触发(Seed/verdict phase 启动时官方机制自动注入 rules + 可选 `mcp__*claude-mem*search`) | 🟢 | 通用,LLM 自看 tool surface 决定调不调 claude-mem |
| **跨团队记忆隔离 vs 共享**(本文档新增问题) | 🟢 | rules 文件按 team 分名 + `paths:` frontmatter 按项目目录前缀 scope 实现隔离 |

---

## 2. 团队扩展契约

要在 ccteam 上跑一支新团队,必须交付以下 7 件东西。**少一件,orchestrator 拒绝
启动**(`ccteam doctor --team <name>` 失败 → `ccteam start --team <name>` fail-fast)。

> ✅ **M3 已落地 phase-driven 契约**(2026-05-06);**V0.4.0 重构为 workflow-driven**
> (F60-F69):phase 集合 EOL,改 workflow.yaml + `.claude/agents/<role>.md` 数据驱动。
> `team.yaml` 仍保留(retro_schema / critic_dimensions / escalate_grammar_extensions /
> golden_rules / verdict_schema)作 V0.4.0 后**向后兼容仅**;新团队走 workflow.yaml +
> agents.md。本节继续作为新团队加入的契约骨架——加 marketing / ops 时仍按此清单交付。

### 2.1 Workflow 拓扑(`workflow.yaml` 是数据驱动入口)

V0.4.0 起,新团队**不**再交付 `phases-<team>/` 目录;改交付:

1. **`team.yaml::workflows[]`** — 声明本团队可用的 workflow 实例(team-level registry,
   `~/.ccteam/teams/<team>.yaml`)。每条引用一个 workflow.yaml 模板路径 + 描述。
2. **项目实例 `<project>/.ccteam/workflow.yaml`**(V0.4.6 F83 起从 root 迁入 `.ccteam/`)
   — 由 `ccteam init` / `ccteam new` 或 `ccteam-creator` skill 生成,包含具体 agent
   拓扑 + trigger + parallelism + budget。**完整 schema** 见 §1.3 表;字段表 + 红线见 §1.4。
3. **`.claude/agents/<role>.md`** — 每个 workflow 引用的 role,在 `.claude/agents/`
   下有同名 markdown(Claude Code 官方 subagent definition 路径)。**这是 agent 行为
   SoT**(prompt + tools + role description),由用户 / `ccteam-creator` 手写,
   **ccteam 不读、不解析、不修改**。

**workflow.yaml 骨架示例**(V0.4.6,与 `crates/ccteam-core/src/workflow.rs::WorkflowSpec` 一致):

```yaml
name: dex-ui-autoloop
description: "DEX UI 自循环改 bug pipeline"   # meta-agent / UI 看
enabled: true                                  # F82 软开关
budget:                                        # F84 双 cap
  max_cost_usd_per_24h: 5.00
  max_agent_spawns_per_hour: 100
agents:
  explorer:
    executor: claude
    trigger: watch:.ccteam/issues/             # inotify trigger
    parallelism: 2
    input: .ccteam/issues/
    output: .ccteam/fix-requests/
    timeout: 30m
    on_timeout: escalate
  fixer:
    executor: claude
    trigger: gate                              # 等 trigger_gate MCP
    input: .ccteam/fix-requests/
    output: .ccteam/fixes/
```

约定:
- **agents 是 IndexMap**(YAML 声明顺序保留)→ trigger 图按声明顺序 deterministic build
- **至少一个 agent** 必须有非空 trigger;否则 `ccteam doctor --validate-workflow` fail
- **role 名** 必须与 `.claude/agents/<role>.md` 文件名匹配(orchestrator 启动期 +
  spawn 前各校验一次;缺文件 → fail-fast)
- **trigger 4 类**:`manual`(meta-agent 显式派单)/ `schedule`(V0.4.1 cron,opaque
  interval)/ `gate`(等 `trigger_gate` MCP)/ `watch:<path>`(inotify)
- **数据驱动红线** — workflow.yaml 字段表里**不**允许 prompt 字面量;详见 §1.4

### 2.1a phase 集合(目录与命名,**V0.4.0+ EOL**,仅作历史)

V0.1-V0.3 旧 `phases-<team>/` 约定保留作历史索引;V0.3.2 → V0.4.0 升级路径见
CLAUDE.md §六(`ccteam doctor --migrate-phase-to-workflow`)。新团队**不**用 phase
约定。

### 2.2 `tools_required` 清单

每个 phase 必须显式声明它使用的工具:

```yaml
tools_required:
  subagents: [code-reviewer, code-architect]      # ~/.claude/agents/<name>.md
  skills: [ccteam-control]                          # ~/.claude/skills/<name>/
  mcp: [Telegram, Playwright]                       # 当前 claude 加载的 MCP server
```

orchestrator 启动期(`ccteam start --team research`)枚举三类来源做交叉比对,缺谁
报缺谁 + 给出修复命令。**未声明的工具调用** = silent fail——phase 模板里 `Task(
subagent_type="X")` 但没在 `tools_required.subagents` 里写 X,启动期就拒绝。

### 2.3 Critic 维度定义(替代 dev 6 维 Score)

每团队提供 `team.yaml.critic_dimensions`:

```yaml
# team-research.yaml
critic_dimensions:
  - name: source_diversity
    weight: 0.25
    weak_threshold: 0.4              # 任一维度 ≤ 此值 → 自动 BLOCK 进 fix-cycle
    anti_leniency_strictness: strict # ↓ 见下面 invariant 2
    rubric: |
      0.0 = single source, 1.0 = ≥3 independent primary sources cross-validated
  - name: hypothesis_falsifiability
    weight: 0.20
    weak_threshold: 0.5
    anti_leniency_strictness: strict
    ...
  - name: insight_actionability
    weight: 0.20
    weak_threshold: 0.4
    anti_leniency_strictness: normal
    ...
```

dev 团队的 6 维(Functionality / Quality / Tests / UX / Speed / Docs)只是这套 schema 的
一个具体实例。`anti-leniency` 规则(M4)同样:"至少一维必须 ≤ X"——通用机制,X 由 team
配置。

#### Invariant 1 — `critic_dimensions[]` 是数据,不是 Rust enum

**M4 anti-leniency 实现禁止**:
- 把维度名字写进 `enum CriticDimension { Functionality, Quality, ... }`
- 在 `match arm` 里枚举 `"functionality" => 0.20, "quality" => 0.15`
- 在 `crates/ccteam-core/` 任何位置出现 dev 6 维的字符串字面量

**正确做法**:`team.yaml` 加载到 `Vec<CriticDimensionConfig>` 这种数据类型,所有维度名 /
权重 / 阈值都从配置读。M4 实现 anti-leniency / weak-block 算法只跟 `CriticDimensionConfig`
打交道,不知道也不在乎在跑 dev 还是 research。

**为什么这条要单独立 invariant**:M4 实现者大概率走最快路径——dev 6 维是已经定下来的,
直接写进 enum 看似省事。**但 M3 团队抽象之后,这等于把 dev 假设暴露到 Critic 子系统的
表面**,M3 之后改动会扩散到所有 critic 调用点。这条 invariant 的作用是**预先拒绝那个
省事路径**,让 M4 实现者从一开始就用数据驱动。

#### Invariant 2 — `anti_leniency_strictness` 是 per-dimension 元数据

dev 团队的 critic 受测试退出码兜底(测试失败 = 客观 BLOCK,不需要 critic 主观判断),
所以 dev 维度普遍可以 `normal`。research 团队的 critic 是**纯 LLM 主观判断**——LLM 几
乎总能为任何 research 输出找到话讲(信息密度 / 受众契合 / 数据可信度都是连续值),
"每维度至少一项 CONCERN"的 anti-leniency 在这种场景下**几乎不会拒绝**。所以 research
的核心维度需要 `strict`(更严的拒绝阈值,例如要求 critic 必须给出至少一项 BLOCK 而不
是 CONCERN 才算"批评足够")。

每维度声明自己的严格度:

| 值 | 语义 | 适用场景 |
|---|---|---|
| `lenient` | 至少一项任意级别评注即通过 anti-leniency | dev 的 Docs / UX 这种次要维度 |
| `normal` | 至少一项 CONCERN 或 BLOCK 才算"批评足够" | dev 的多数维度 |
| `strict` | 必须有至少一项 BLOCK,否则 anti-leniency 不通过 | research 的核心维度,LLM 主观判断兜底缺失时 |

**M4 anti-leniency 算法签名应为** `fn check(dims: &[CriticDimensionConfig], scores: &[CriticScore]) -> AntiLeniencyVerdict` —— 严格度从配置读,不是参数也不是全局常量。

#### Invariant 3 — `weak_threshold` 由配置控制,不是常量

理由同 Invariant 1。`crates/ccteam-core/src/score.rs`(M4 引入)**禁止**出现
`const WEAK_THRESHOLD: f32 = 0.4;`——必须从 `CriticDimensionConfig.weak_threshold` 读。

### 2.4 ESCALATE grammar 扩展

每团队声明本团队特有的 ESCALATE 前缀及其路由语义:

```yaml
# team-research.yaml
escalate_prefixes:
  - prefix: HYPOTHESIS_REJECTED
    route: REVERT_TO_PHASE
    target_phase: 02-hypothesis
    reason: "假设被一手数据反驳——回到 hypothesis phase 重设"
  - prefix: SOURCE_UNAVAILABLE
    route: NEED_USER_INPUT
    reason: "关键一手数据来源拿不到——需用户决策替代来源 / 缩范围 / 放弃"
```

orchestrator 解析 `parse-phase-end` 时按团队声明的前缀分发——前缀本身是数据,
分发逻辑是 mechanism。

### 2.5 完成信号定义(**V0.4.0 后向后兼容仅;新 team 改 workflow.yaml + agent.md**)

> V0.4.0 phase 拆除后,"phase completion_signal" 概念失效。新 team 不再声明
> `completion_signal` / `on_loop_exhaust` —— workflow.yaml 通过 trigger 类型 +
> `parallelism` 表达启停语义,`workflow_done` event 由 thin orchestrator 在所有
> `gate` agent 完成时自动写。**本节保留为 V0.1-V0.3 团队的语义参考。**

V0.1-V0.3 phase 驱动时代,每团队必须声明:

1. **何时本项目算 done**——通常是 phase DAG 走到终态 phase 且无 escalate。
2. **每个 phase 的 `completion_signal`** —— ralph-loop / Stop hook 据此判定退出
   vs 重喂。dev 团队 fix phase 用 `TESTS_GREEN`;research synthesis phase 可能
   用 `INSIGHTS_TRIANGULATED`。
3. **自循环 phase 的失败兜底** + `on_loop_exhaust: escalate | abort`。

### 2.6 推荐 plugin / agent 安装清单

每团队提供 `team.yaml.recommended_agents`,`ccteam doctor --install-recommended-agents
--team <name>` 据此 ln -sf。**V0.4.0 后**:**首要交付改为 `.claude/agents/<role>.md`
(workflow.yaml 引用的 role 必须各有同名 md)**;`recommended_agents` 字段仍保留作
V0.1-V0.3 兼容,但新团队**优先**靠 `.claude/agents/` SoT 而非 ccteam-managed lnish。

清单要素(V0.1-V0.3 兼容字段):
- agent 来源(`claude-plugins-official:<plugin>/agents/<name>` / 自带脚本路径)
- 默认挂载 phase(信息性,不强制)
- 用一句话说明何时该被调用(给 LLM 决策时读)

### 2.7 与跨项目记忆(M4)的对接方式

每团队声明:
- **retro phase 输出 schema**:patterns 文件该有哪些字段(dev: tech stack /
  踩过的坑;research: 数据源 / 假设结果 / 方法学反思;...)。
- **召回时 namespace 策略**:
  - `team_only`(默认):跨同 team 的项目召回。dev 项目召回时只看 dev 项目历
    史,research 同理。
  - `cross_team`:允许跨 team 召回——通常 anti-pattern 是有意义的(失败的
    research 项目对 dev 项目也可能有警示),但成功 pattern 互相不一定可迁移。
- **anti-pattern 全局共享**:无论 namespace,REJECT/ABORT 案例对所有团队都召
  回——避免重复犯错。

### 2.8 团队配置文件骨架

```yaml
# ~/.ccteam/teams/<team-name>.yaml(规范化文件名;M3 已落地 — 见 `teams/dev.yaml` / `teams/product-research.yaml`)
name: research
# V0.4.0+ 主字段:
workflows:                                # team-level workflow registry(V0.4.0+)
  - name: idea-validation
    template: workflow-templates/research-validation.yaml
  - name: synthesis
    template: workflow-templates/research-synthesis.yaml
# V0.4.0+ 后向后兼容仅(V0.1-V0.3 phase-driven 字段;新 team 可以不填):
phase_dir: phases-research                # ⚠️ V0.4.0+ EOL,新 team 改 workflow.yaml
entry_phase: 00-topic                     # ⚠️ V0.4.0+ EOL
completion_signal_default: PHASE_DONE     # ⚠️ V0.4.0+ EOL
on_loop_exhaust: escalate                 # ⚠️ V0.4.0+ EOL
critic_dimensions: [...]                  # §2.3 — M5+ Critic 启用时仍读
escalate_prefixes: [...]                  # §2.4 — workflow event escalation 仍读
recommended_agents: [...]                 # §2.6 — V0.4.0+ 主要靠 .claude/agents/<role>.md SoT
recommended_skills: [...]
recommended_mcp: [...]
# M4 走官方 ~/.claude/rules/ + per-repo auto-memory,无 memory_namespace 字段;
# 跨项目隔离通过 rules 文件按 team 分名 + paths: frontmatter scope 项目目录前缀实现。
retro_schema:                             # §2.7
  - field: methodology_used
    type: text
  - field: source_quality
    type: rubric
  - field: would_redo
    type: bool
artifacts:                                # §1.5 改造后从此读
  spec: topic.md
  primary_data_dir: primary/
  final_report: report.md
danger_command_patterns:                  # 替代 dev 的 git push 拦截
  - pattern: 'curl .* mailto:'
    reason: "research 团队不该直接发用户邮件"
  - pattern: 'rm -rf .*/primary/'
    reason: "保护一手数据"
```

---

## 3. 编排层不应承担的职责(显式拒绝清单)

防止后续 PR 在"为了通用"的名义下把领域决策悄悄塞进 `ccteam-core`。**违反这条
清单的 PR 应被拒收**——领域决策不是编排层的扩展点。

### 3.1 不替领域定 done criteria

- ❌ 不在 `ccteam-core` 写"如果测试全绿 + critic PASS 则 done"。
- ✅ 通过 phase DAG 终点 + 终点 phase 的 `completion_signal` 表达。
- 例外:**进程级别的 done**(claude 主动退出 / SessionEnd)是 mechanism,不在此限。

### 3.2 不替领域选 plugin

- ❌ 不在 `ccteam-core` import / 调用具体 plugin 名(`pr-review-toolkit` /
  `code-simplifier` / ...)。
- ✅ 通过 `tools_required` + `recommended_agents`(数据)注入。
- 例外:**ralph-loop / claude-plugins-official 的 hook 范式参考**是设计借鉴,
  不是运行期依赖——`fix_loop.rs` 实现 ralph 范式但不 spawn ralph plugin。

### 3.3 不预设 fix-loop 语义

- ❌ 不假设"自循环只会发生在 fix 阶段"或"自循环的目的就是修代码让测试通过"。
- ✅ phase 模板 `auto_loop: true` + `completion_signal: <SIGIL>` 通用化。
- ❌ 不在 `fix_loop.rs` 文件名 / 类型名里固化 "fix"——改名 `auto_loop.rs` /
  `AutoLoopState`(详见 §B audit)。

### 3.4 不预设质量评分维度

- ❌ 不在 `ccteam-core` 写"6 维评分"的具体维度名。
- ✅ team 配置的 `critic_dimensions[]` 是数据;`anti-leniency` 规则按通用
  schema 实现("至少一维 < weak_threshold 触发 BLOCK")。

### 3.5 不预设危险命令清单

- ❌ 不在 `ccteam-core` 硬编码 `git push.*` / `rm -rf` 这种 matcher。
- ✅ `team.yaml.danger_command_patterns[]` 注入,hook matcher 由 settings.json
  渲染时按 team 注入。
- 论证:research 团队没有 git push 但有"未审就发邮件";marketing 团队的危
  险命令是"直接 publish to social"。

### 3.6 不替领域定 escalation 前缀语义

- ❌ 不在 `parse-phase-end` 里写死 `HYPOTHESIS_REJECTED → REVERT_TO_PHASE 02-hypothesis`。
- ✅ team 配置 `escalate_prefixes[]` 作为路由表;`parse-phase-end` 是查表
  mechanism。

### 3.7 不替领域定记忆字段

- ❌ 不在 `ccteam-core` 假设 retro 含 "tech stack" / "踩过的坑"。
- ✅ team 配置 `retro_schema[]` 决定字段;M4.1 retro phase prompt 按 schema 字段
  生成段落,写入 `~/.claude/rules/ccteam-lessons-<team>.md` marked section
  (M4 走官方 rules 机制,不做向量化也不建索引;详见 tech-design §3.7)。

---

## 4. 现状缺陷(由 §B 审计填充)

> 这一节是 §B 审计的产出落点。审计发现把 dev 团队假设写死在编排层的位置,
> 都按"文件:行号 / 现状描述 / 是否真 dev-specific 判定 / 解耦方案 / 优先级"格
> 式记录在这里。空槽 placeholder 直至 §B 完成。

### 4.1 审计范围

§B 审计将覆盖:
1. `crates/ccteam-core/src/` 全部 9 个文件
2. `crates/ccteam-cli/src/commands.rs` + `main.rs`
3. `crates/ccteam-hooks/src/` 全部 5 个文件
4. `crates/ccteam-core/src/templates/settings.json`(模板)
5. `phases/` 当前 6 个 phase 模板的目录与命名约定
6. `CLAUDE.md` 与 `docs/` 各文档对"ccteam = 开发团队"的措辞

### 4.2 审计输出格式

每条发现固定四要素:

```
- 文件:行号
- 现状:<现在代码长什么样>
- 是否真 dev-specific:<论证;不是一刀切。许多看起来 dev 的字段其实是 mechanism>
- 解耦方案:<重命名 / 提 trait / 加配置 / 不必改>
- 优先级:P0(阻塞泛化) / P1(该做但可后置) / P2(边角)
```

审计完成日期:**2026-05-05**。详细发现见 [`docs/dev-coupling-audit.md`](./dev-coupling-audit.md);本文 §4.3 仅给摘要。

### 4.3 审计发现摘要

详细审计报告(20 条发现 / 文件:行号 / 解耦方案 / 优先级)→
**[`docs/dev-coupling-audit.md`](./dev-coupling-audit.md)**(2026-05-05 完成)。

审计覆盖 `crates/ccteam-core/src/`(9 文件)+ `crates/ccteam-cli/src/`
(2 文件)+ `crates/ccteam-hooks/src/`(5 文件)+ `crates/ccteam-core/src/
templates/settings.json` + `phases/`(6 文件)+ 顶层 `CLAUDE.md` 与 `docs/`。

按优先级分布(**2026-05-16 post-V0.4.6 sweep**;详情见 `docs/dev-coupling-audit.md`):

| 优先级 | 数量 | 编号 | 含义 |
|---|---|---|---|
| **P0 阻塞泛化(剩余)** | 0 | — | F1 已在 2026-05-07 rename PR + V0.4.0 F60 phase 拆除时彻底消解 |
| **P1 剩余** | 2 | F15, F23(conditional) | settings 危险命令模板(M1+)/ 容器 bind-mount spike |
| **P2 边角(剩余)** | 1 | F17 | 测试硬编码 phase 名(V0.4.0 F60 phase 拆除后 N/A) |
| **N/A 已是领域无关** | 2 | F14, F19 | F19 在 M3 docs 落地后重新评估 |
| **已修复** | ~50 | F1-F13 / F18 / F20-F51 / F60-F69 / F80 等 | M3/M4 ship + 2026-05-07 rename PR + V0.2/V0.3/V0.4.0 patch 一波波关闭(详情见 audit 文档) |
| **V0.4.6 in-flight** | 3 | F82, F84, F91+ | workflow.yaml `enabled` 热加载 / budget cap / SPA 配套 |

**§B 元发现(对 §A 的反馈;V0.4.0 update)**:`pub use ... M0_PHASE_DAG, FIRST_PHASE`
原暴露 dev 假设到 lib 接口表面 —— **M3.1 处理**(常量删除,改 `PhaseDag::infer_from_templates`);
**V0.4.0 F60 进一步拆除 `dag.rs` 整文件 + phase 模板系统**,改 `crates/ccteam-core/src/
workflow.rs::WorkflowSpec` 为 lib 主接口。两次 lib API breaking change 均无 backwards-compat
shim,符合 CLAUDE.md §五.3。

审计过程中**没有发现**需要修订 §1 责任分界表或 §2 团队扩展契约的位置——
所有发现都能映射到现有分类。这是抽象切对的好信号。M3/M4 ship 后的 post-sweep
同样无需新分类。

---

## 5. 首个非 dev 团队的选择论证(**M3 已实证;本节压缩**)

> **2026-05-06 落点**:M3 ship 的首个非 dev 团队是 **`product-research`**
> (`teams/product-research.yaml` + 6 phase pipeline),选它(而非泛 research)
> 因为能直接对接 dev pipeline(verdict.md PASS → 派给 dev,REJECT → 终止),
> 首个非 dev 团队就拿到了 multi-team 协作的实证。
>
> **论证浓缩**(详细原文可看 git history):
> - **工具面重叠**:`code-explorer` 在 research 里就是"已有资料探索"agent,plugin
>   ln -sf 复用零改动 → 验证"推荐清单按 team 切换"机制
> - **抽象漏洞暴露**:research 完成信号是判断(非物理事实)→ 逼 `completion_signal`
>   字段化;Critic 评 method 品质 → 逼 `critic_dimensions` 数据化;假设被反驳要回
>   hypothesis 重设 → 逼 ESCALATE grammar §2.4 扩展
> - **marketing / ops 后置**:marketing 需要"发布前用户拍板"mechanism(未做),
>   ops 需要 metric MCP(未做),M5+ 探索
>
> **验证结论**:§1 责任分界表 / §2 团队扩展契约 / §3 显式拒绝清单**均无需修订** —
> product-research 落地全程未让 ccteam-core 出现新 dev 假设。抽象切对了。
> V0.4.0 重构后,workflow.yaml 数据驱动进一步降低了团队扩展门槛(无需再写新
> phase 集 + 新 fix-loop 状态机)。

---

## 6. 里程碑落点建议

**不进 M0 / M0.5 / M1 / M2**——这些里程碑都还在补"dev 团队跑得稳"的洞,现在加
team 抽象只会让两件事都做不好。

### 6.1 M3 — Team Abstraction(本文档对应里程碑;约 2 周)— **已落地 2026-05-06**

> **2026-05-05 reorder**:本文档原提案 "M4.5",但 ABC session 完成后审视发现:跨项
> 目记忆(原 M3)和 Critic agent(原 M4)如果在团队抽象**之前**实施,retro_schema
> 和 critic_dimensions 都会写死 dev 字段,后续 M4.5 落地时被迫推倒重来。**正确顺序
> 是团队抽象**前置**,作为 M3。** `docs/v0-1/docs/v0-1/development-plan.md` 已同步 reorder:
> M3=Team Abstraction、M4=记忆、M5=Critic、M6=Symphony。
>
> **2026-05-06 ship**:M3.1–M3.7 全部 ship(commits `5535cf4` / `c75e092` /
> `6cd8959` / `5a42d84` / `b1c434b` / `ce6541e`);PR #7 合并到 main。验收点:
> `ccteam new --team=product-research "AI 菜谱生成器"` 走通 6-phase happy path
> (M3.5 e2e 测试 + V0.1 user walkthrough);`ccteam new "<brief>"` 默认 `--team=dev`
> 零迁移。M4 跨项目记忆(M4.1–M4.4)随后 ship,走官方 `~/.claude/rules/` +
> per-repo auto-memory(`873aa0a` / `36b6d99` / `62e47a6` / `5e351b1`)。

**唯一验收**(已达成):`ccteam new --team=product-research "<topic>"` 能跑通
happy path,产出最终 verdict 报告;dev 团队的现有项目零迁移成本
(`ccteam new "<brief>"` 默认 `--team=dev` 仍然工作)。

任务清单详见 `docs/v0-1/docs/v0-1/development-plan.md` §5(M3.1 ~ M3.7)。本文档不再重复维护任务
表,只做"为什么这么排"的论证。

### 6.2 为什么不更早?

- **M0/M0.5 还在补 mechanism 漏洞**(idle 注入、ralph-loop、tools_required 校验)
  ——这时把 team 抽象出来等于在抖动地基上加层。
- **M1/M2 在补用户体验**(telegram、Seed Gate、Score)——这些机制在 dev 团队上要
  先稳定,泛化的复杂度才不会被基础不稳放大。

### 6.3 为什么不更晚?(回顾论证 — M3 已 ship,本节作 future memory)

- **M4 跨项目记忆的 retro_schema** 必须从 day 1 团队感知,否则跨项目 lessons
  字段段落布局重写代价高(audit F20 P0)。**结果验证**:M3.1 ship retro_schema
  数据形式;M4.1 retro phase 直接消费 schema 写 `~/.claude/rules/ccteam-lessons-<team>.md`,
  零返工。
- **M5 Critic 的 critic_dimensions** 必须从 day 1 数据驱动,否则 anti-leniency 算法
  写死 dev 6 维,M6 Symphony 多团队协作时返工(§2.3 invariant 1)。当前 M3.2
  已 ship `critic_dimensions` 数据形式 + 校验,等 M5 启动直接读;dev / product-research
  两个 team 都先填空,留给 M5 一并设计。
- 这两条都把 team 抽象推到 M4 / M5 **之前**——即 M3。

### 6.4 风险(回顾 — M3 已 ship,事后实绩)

| 风险 | 触发 | 应对 | 实际结果 |
|---|---|---|---|
| §B 审计 P0 项过多,M3.1 单条堵住整个里程碑 | 审计发现深层耦合(例:fix-loop 状态机假设 dev 流程) | M3.1 拆为多个子 PR,每条 P0 独立 PR;按 §B 优先级排序逐个清 | ✅ M3.1 / M3.2 / M3.3 / M3.4 / M3.5 / M3.6 / M3.7 拆成 7 个子任务并行推;F1 一项延后至 post-M3 仍 P0 |
| dev 团队的 `team-dev.yaml` 反推时和现状不一致 | 写 team-dev.yaml 时发现某些行为靠"巧合"工作,没显式契约 | 反推时逐条对照 §1 责任分界表;"没契约的现状"必须先写到 §1 再纳入配置 | ✅ 未触发;反推 dev.yaml 严格按 §1 / §2 模板 |
| product-research 团队跑通靠的是借用 dev plugin 的能力,而不是真验证了 §2 契约 | phase 模板偷懒,ESCALATE 不用 §2.4 自定义前缀,critic 不用 §2.3 自定义维度 | M3.4 验收时强制要求至少有 1 个自定义 ESCALATE 前缀 + 至少 1 个 dev 没有的 critic 维度 | ✅ product-research 落 3 个自定义 ESCALATE 前缀(MARKET_DUPLICATE / INSUFFICIENT_VALIDATION / LOW_DIFFERENTIATION);critic_dimensions 留给 M5 一并设计 |
| §3 "显式拒绝清单"被 PR 软性绕过 | "为了通用"在 ccteam-core 加 `if team == "research"` | code-review 加规则:`ccteam-core/` 内出现 team 名字符串字面量 = 自动拒收 | ⚠️ 部分:`render_project_claude_md` 有 `match team` 但分支只是 CLAUDE.md 模板内容(数据 not 决策),可接受 |
| M4 / M5 实现时仍被诱惑写死 dev 假设 | 团队抽象上线但 M4 retro / M5 critic 没真用配置驱动 | M4.1 必须读 `team.yaml.retro_schema[]`;M5.1 / M5.2 / M5.3 必须读 `team.yaml.critic_dimensions[]`;两者都纳入 M4 / M5 验收清单 | ✅ M4.1 ship `team-aware retro phase prompts`(`873aa0a`),phases/09-ship.md / phases-product-research/06-verdict.md 均按各自 schema 写;M5 待启 |

---

## 7. Meta-Agent Pattern — ccteam 的最终使用形态

> **2026-05-06 重要更新**:本节原本写 "meta-agent = 用户自己的 daily-driver
> Claude Code 会话"。但这个假设暗含"用户必须坐在电脑前"——不符合现代
> agent 产品(openclaw / hermes / Claude Code 官方 TG)的实践。改成
> **meta-agent = ccteam-managed 常驻 claude 会话,Channel Layer(M2+)
> 是其上层适配器**。Symphony 反模式的红线没动:**channel 适配器进程内
> 不嵌 LLM**,所有 NL 处理都收敛到这一份 meta-agent session。

### 7.0 三层架构定位(对应 tech-design.md §2.1)

```
Channel Layer (M2+)        Telegram / Feishu / Slack adapters
                                          ↕  inbox/outbox 文件协议
User Interaction (M1)      meta-agent session  +  N 个 project sessions
                                          ↕  send-keys / inbox watcher
Orchestration (M0+M0.5)    Rust orchestrator daemon
```

meta-agent 是 User Interaction Layer 的成员之一,**和项目 session 同等地位**——
ccteam-managed 长 tmux 会话,跑 `claude --dangerously-skip-permissions`,装
ccteam-control skill。差别只在生命周期(永不 terminal)与行为模式(事件循
环 vs phase DAG)。

### 7.1 已存在零件的角色对位

| ccteam 设计组件 | 在 meta-agent pattern 里扮演 | 对应里程碑 |
|---|---|---|
| meta-agent 常驻 session(`ccteam-meta-<user>`) | NL 入口 + dispatcher 主体 | **M1.0**(新) |
| inbox/outbox 文件协议 | meta-agent 与 channel layer / 项目 session 之间的接入面 | **M1.1**(改) |
| `ccteam-control` skill | meta-agent 调用 ccteam CLI 的"指挥棒" | M1.8 |
| `ccteam-mcp` MCP server | meta-agent 派单的结构化控制面(替代 shell parse) | M2.8 |
| 跨项目 lessons via `~/.claude/rules/ccteam-lessons-<team>.md` + auto-memory(claude-mem 可选) | meta-agent "上次相似项目"的长期记忆(项目级) | M4(走官方机制,reorder + 2026-05-06 简化后) |
| meta-agent conversation 续航 | meta-agent 对话历史的 reset 桥接 | M4 主路径(`~/.claude/rules/ccteam-lessons-<user>-meta.md` 滚动累积 + auto-memory;无独立 conversation-log) |
| Channel adapters | meta-agent 跨设备 / 跨平台的输入输出代理 | M2+(优先复用开源) |
| 长 tmux session per project | dev / research / ... 团队的"高密度施工工地" | M0.7 |
| 团队抽象(`team.yaml` + `--team` CLI) | meta-agent 派单时选择"派给哪支团队" | M3(本文档对应里程碑) |

**结论**:M1.0 + M1.1(meta-agent + 协议)+ M2.8(`ccteam-mcp`)+ M3(团队抽象)+
M4(跨项目记忆走官方 rules + auto-memory + 可选 claude-mem)五件齐备,meta-agent pattern 完整。

### 7.2 三块 ccteam 现状没显式覆盖的新机制

#### 7.2.1 conversation continuity — meta-agent 的"上次我们聊过"

跨项目记忆(M4)的语义是**项目级**的(已 ship 项目的 retro lessons)。
但 meta-agent 跟用户的**对话历史**(讨论但还没派单的想法、被拒绝的方向、用户偏好)
也吃同一套官方机制 — 不需要独立 conversation-log.md 设计。

**M4 主路径方案**(2026-05-06 简化后):

- **官方 auto-memory 自动捕获**:meta-agent session 也是 ccteam-managed Claude Code session,
  `~/.claude/projects/<meta-encoded>/memory/` 由 Claude 自主累积偏好 / 决策 / 拒绝过的方向
- **跨用户对话累积**:`~/.claude/rules/ccteam-lessons-<user>-meta.md`(同 dev / product-research
  团队同样的 rules 机制,但 namespace 是 `<user>-meta`)。retro 触发时机由 meta-agent
  对话流自决(可由 phase prompt 引导,例如"对话进入新主题前总结上一段")
- **可选**:用户装了 claude-mem,自动 5-hook 捕获就把 meta-agent 对话也覆盖了

不需要 `conversation-log.md` 滚动文件,不需要 60% 阈值压缩 — 都被官方机制取代。
context reset 时官方机制会重新加载 rules,无独立桥接代码。

#### 7.2.2 dispatch protocol — meta-agent 该怎么派单

用户说"做一个 todo app",meta-agent 要做的决策链:

1. **是问答还是项目请求?** —— 问答直接答(meta-agent 自己用工具回答),项目请求才进
   下一步
2. **分类团队类型** —— dev / research / marketing / ops / 综合体(后两个 M5+)
3. **pre-flight clarification** —— 对应 M2 Seed phase 的 CLARIFY,但**在 ccteam 派单
   之前**完成,避免"用户说一句 → ccteam 起 session → Seed 再问一遍"的双重澄清
4. **通过 `ccteam-mcp` 派单** —— `ccteam__new(team="dev", brief="...")`
5. **后续监控** —— `ccteam__progress` / `ccteam__peek` 看进度,关键事件(escalation /
   completion)经 inbox/outbox 协议触达用户(终端 attach 直接看 / channel layer 推到
   外部消息系统)

这套流程是 meta-agent 的核心行为说明书,**M1.0 必须把它写进 meta-agent 的 role
prompt**(`~/projects/<user>-meta/.ccteam/CLAUDE.md` 或类似位置)。

#### 7.2.3 default meta-agent behavior preset — dispatcher not worker

`ccteam-control`(M1.8)是**能力**——meta-agent **能**调度 ccteam。但只有能力
不够,还要有**行为约束**:meta-agent 应该是 dispatcher 不是 worker——**别自己
抄起 Edit 工具开干,先 ccteam_new + 派单**。这是反直觉点:Claude Code 默认行
为是"用户问什么我都自己上手做",meta-agent 模式要它**克制**。

写在 meta-agent role prompt 里(M1.0 交付的一部分):

- **决策树**(§7.2.2 的形式化)
- **明文克制规则**:"识别到项目级请求时,默认通过 `ccteam new` 派单,**不**自己
  写代码 / 不自己跑研究 / 不自己起草营销文案;只有在用户明确说'你直接帮我写 X'
  时才走 worker 路径"
- **对话风格约束**:meta-agent 跟用户对话时**不展示 progress 细节**(那是 ccteam
  CLI / TUI 的活),只汇报里程碑事件

### 7.3 ccteam 自身需要承担的两件事

让 meta-agent pattern 真正落地,ccteam 仓库里要有:

1. **meta-agent role prompt 模板**(M1.0 交付)—— 放在 ccteam 内嵌资源中,
   `bootstrap_project --team meta-agent`(或等价路径)写到
   `~/projects/<user>-meta/.ccteam/CLAUDE.md`。包含 §7.2.2 决策树 + §7.2.3
   行为约束
2. **`ccteam doctor --install-meta-agent`**(M1+)—— 给已有 ccteam 实例创建
   meta-agent session 的工具。等价于 `ccteam new --team=meta-agent --user-handle=<x>`
   的一次性配置助手
3. **在产品文档里把 meta-agent pattern 作为推荐使用方式显式描述** —— 不是
   "你也可以这么用",而是"**这是 ccteam 设计意图的最终形态**"

### 7.4 与 §3 显式拒绝清单的一致性

meta-agent pattern **不违反** §3 任何一条:

- 不是"ccteam 在适配器进程里嵌 LLM"——meta-agent 是 ccteam-managed 长会话,
  跟项目 session 同等地位,**channel adapter 进程内仍然没有 LLM**(Symphony
  反模式的红线没动)
- 不替领域定 done criteria —— meta-agent 把项目派给团队后,完成判定仍由该团队
  的 `team.yaml.completion_signal` 决定(§2.5)
- 不引入新的 LLM 编排层 —— meta-agent 是 ccteam 现有 long session 模式的一个
  实例,不是新概念

如果将来发现某个 meta-agent 行为约束**必须**靠 channel 适配器内嵌 LLM 才能实
现,那是信号:回头审视该约束是不是放错位置了——它该写进 meta-agent 的 role
prompt,而不是适配器代码。

### 7.5 落点回到里程碑

| 里程碑 | meta-agent pattern 进展 |
|---|---|
| **M1** ✅ ship | meta-agent session 上线(M1.0)+ inbox/outbox 协议加固(M1.1)+ ccteam-control skill 装好(M1.8)→ **终端 attach 即可 NL 对话**,无 channel 也能跑 |
| M2 ✅ ship(M2.2 deferred) | `ccteam-mcp` MCP server(M2.5)+ Channel adapter(Telegram 优先,复用开源,**M2 内未做** — 留给后续)→ meta-agent 有结构化派单工具 |
| M3 ✅ ship 2026-05-06 | 团队抽象上线 → meta-agent 派单时能选 `--team`(dev / product-research),M3.7 落 meta-agent dispatch tree |
| M4 ✅ ship 2026-05-06 | 跨项目 lessons via `~/.claude/rules/ccteam-lessons-<team>.md` + per-repo auto-memory(meta-agent 同享) → 完整跨项目记忆,ccteam-core 零检索代码 |
| M5+ | 多团队协作(product-research → dev pipeline)+ 多 channel 收敛 → meta-agent 编排跨团队工作流,跨设备访问 |

---

## 8. 与 CLAUDE.md / tech-design.md / docs/v0-1/development-plan.md 的关系

- **CLAUDE.md** §一"定位:ccteam 是 Claude Code 之上的元工具"是本文档的精神
  上游——把"meta-tool"再往上抽一层,得出"meta-tool of any AI team"。
- **tech-design.md** 是 mechanism 的设计论证;本文档**不**改 mechanism,只
  把它们标"哪些是 mechanism、哪些是 dev fill"。
- **docs/v0-1/development-plan.md** 是任务清单;本文档对应 §5 M3 — Team Abstraction(2026-05-05
  reorder 后,从原提案 M4.5 前移至 M3,见 §6.1 注解)。任务粒度(M3.1 ~ M3.7)由
  development-plan 维护;M3 状态已标 ship。
- **interfaces.md** 是协议字段表;本文档建议在 phase YAML schema 加
  `completion_signal` / `auto_loop` 字段(§2.1),那是 interfaces.md §5.1 的
  扩展——M3.2 提交协议变更时已同步 interfaces.md。

## 9. 本文档维护纪律

1. **任何 PR 引入新机制前**,先确认它是 §1.x 中已分类项的补充还是引入新
   概念。引入新概念必须在 §1 加一行(注明🟢/🟡/🔴),否则无法 review。
2. **§B 审计的发现可以反过来修订 §1 / §2**——发现某条机制的 dev 假设比预
   想深,本文档要更新,不要硬塞 audit 节。
3. **本文档不超过 900 行**——超出说明在重复 tech-design / interfaces;砍
   重复内容,留指针。(M3 reorder + meta-agent pattern → 720+;2026-05-06 M3/M4
   ship 实施状态注解 → 780+;**2026-05-16 V0.4.6 sync**:加 §1.3 workflow.yaml
   schema + §1.4 workflow-as-data 红线 + §2.1 workflow 拓扑重写;同时压缩 §5 论
   证文字 → 860 行,留 40 行 headroom。下一次重大扩展前,**先砍**那些已经过时
   的论证文字 — 例如 §2.5 完成信号已 V0.4.0 向后兼容仅,可进一步压缩)
4. **commit message 用英文,文档内容用中文**(沿袭仓库现状)。
