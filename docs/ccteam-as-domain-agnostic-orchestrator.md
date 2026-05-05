# ccteam 作为领域无关编排层

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
- **当前代码用"约定假设"代替了"显式参数"**:`orchestrator.rs` 直接写死 dev
  pipeline 的 6-phase DAG,`fix_loop.rs` 直接写死 `TESTS_GREEN`,
  `commands.rs` 直接写死要找的 artifact 名。短期没问题(M0 只有 dev 团队),
  长期挡住了泛化路径——M4.5 之前必须把这些假设迁出 `ccteam-core`。
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
| `state.json` 字段 `slug` / `tmux_session` / `claude_session_id` / `claude_pid` / `phase_state` / `parallelism` | 🟢 | 进程身份与执行栈状态——所有团队共用 |
| `state.json` 字段 `current_phase` | 🟢 | 字段是 mechanism;**取值范围**(`plan-eng` / `implement` / ...)由 team 注入 → 取值是 🟡 |
| `state.json` 字段 `phase_history[]` | 🟢 | 时间序列状态记录,所有团队共用 |
| `state.json` 字段 `cost_used_usd` / `soft_warn_threshold_usd` / `hard_kill_threshold_usd` | 🟢 | $ 与 token 是 LLM 推理的物理事实,不分领域 |
| `state.json` 字段 `context_tokens_used` / `context_reset_threshold_tokens` / `context_reset_count` | 🟢 | claude 进程级别的事实 |
| `state.json` 字段 `last_progress_event_at` / `last_event_type` / `user_attached` / `user_pause_pending` | 🟢 | 调度器时间线事实 |
| `state.json` 字段 `fix_cycle_count` | 🟡 | "fix"是 dev 团队的概念名;mechanism 是"phase 内自循环计数",字段应改名 `auto_loop_iterations` 或挪进 `phase_state` 内嵌(详见 §B.1) |
| `phase_state` 枚举 `in_flight` / `idle` / `fix_locked` | 🟡 | mechanism 通用,但 `fix_locked` 命名假设了"fix 阶段才会自循环"——其它团队的 critic-loop / experiment-loop 也需要同样语义,改名 `auto_locked` 更准 |
| `parallelism` 枚举 `solo` / `agent_team` / `multi_session` | 🟢 | 三档并行规模是抽象的执行栈拓扑,与领域无关 |

### 1.2 progress.jsonl 事件类型

| 事件 | 归类 | 论证 |
|---|---|---|
| `session_start` / `SessionEnd` | 🟢 | claude 进程生命周期 |
| `PreToolUse` / `PostToolUse` / `SubagentStop` | 🟢 | Claude Code hook 透传 |
| `Stop` / `notification` | 🟢 | idle 信号 |
| `phase_inject` / `phase_done` | 🟢 | 调度器→执行体的握手协议 |
| `escalate` | 🟢 | 调度器层语义,**reason 字段内容**与"该升级到哪个修复路径"是 🟡(详见 §1.6 ESCALATE 语法) |
| `phase_milestone` | 🟢 | 自由文本进度标记;团队怎么用是 🔴 |
| `watcher_concern` / `watcher_block` | 🟢 | mechanism;具体哪些 watcher、看什么是 🟡 |
| `context_reset` | 🟢 | 60% 阈值机制 |

### 1.3 phase YAML front matter 字段

| 字段 | 归类 | 论证 |
|---|---|---|
| `name` | 🟢 | 字段是 mechanism;取值是 🟡 |
| `required_inputs` / `required_outputs` | 🟢 | L1 架构约束的 mechanism;具体路径是 🟡 |
| `soft_cost_warn_usd` / `stall_warn_minutes` | 🟢 | 调度器观测阈值 |
| `parallelism` / `agent_team` | 🟢 | 执行栈拓扑 |
| `sub_skills[]` | 🟢 | mechanism;清单内容(`code-reviewer` / ...)是 🟡 |
| `hooks.before` / `hooks.after` | 🟢 | mechanism;脚本路径是 🔴 |
| `tools_required`(M0.5 引入) | 🟢 | 启动期可达性校验的 mechanism;清单内容是 🟡 |
| **新字段** `completion_signal`(由本文档建议) | 🟢 | mechanism;取值(dev=`TESTS_GREEN` / research=`HYPOTHESES_VALIDATED` / ...)是 🟡 |

### 1.4 orchestrator 决策点(`orchestrator.rs` / `fix_loop.rs`)

| 决策点 | 归类 | 论证 |
|---|---|---|
| `decide_tick` 的状态机分支(NoOp / Advance / Escalate / Dispatch) | 🟢 | 通用状态转移 |
| `next_phase()` —— 给当前 phase 找下一个 phase | 🟢 | 查表 mechanism;**表本身**(`M0_PHASE_DAG`)是 🟡——必须可换 |
| `is_terminal()` —— 判断终态 | 🟡 | 当前实现把 `ship` passed 当终态;mechanism 应该改为"DAG 终点 + escalated"——`ship` 是 dev 团队的终点名 |
| `FIRST_PHASE = "plan-eng"` | 🟡 | 应改为"DAG 第一个节点",让 team 配置注入 |
| `FIX_PHASE_NAME = "fix"` 触发 fix-loop | 🟡 | mechanism 是"某些 phase 标记为 'auto-loop',进入时写 ralph state"——phase 模板用 `auto_loop: true` 字段触发更通用 |
| `FIX_LOOP_MAX_ITERATIONS = 3` | 🟢 | 通用上限;团队可在 phase 模板里覆盖 |
| `completion_signal: "TESTS_GREEN"`(`fix_loop::FixLoopState::new`) | 🟡 | 应来自 phase 模板 `completion_signal` 字段,而非硬编码 |
| 60% reset 阈值 | 🟢 | claude 进程级事实 |
| idle-aware 注入(Stop/notification → 直注;否则 /btw) | 🟢 | mechanism |
| cost ladder 阈值($20 / $50 / $200) | 🟢 | 默认值是 mechanism;数字本身可由 `~/.ccteam/config.yml` 与 phase 模板覆盖 |
| stall ladder(5/15/30 min) | 🟢 | 同上 |

### 1.5 Hook 与文件路径

| 文件 / 路径 | 归类 | 论证 |
|---|---|---|
| `~/.ccteam/{inbox,queue,control,phases,progress,memory}` | 🟢 | 全局编排层布局 |
| `~/projects/<slug>/.ccteam/state.json` | 🟢 | 项目元数据 mechanism |
| `~/projects/<slug>/.ccteam/spec.md` | 🟡 | 字段名是 mechanism;research 团队等价物可能叫 `topic.md` / `brief.md`——应在 team 配置中声明 |
| `~/projects/<slug>/.ccteam/<phase>-report.md` | 🟢 | 命名 pattern 是 mechanism(`<phase>-report.md`);具体哪些 phase 是 🟡 |
| `~/projects/<slug>/.ccteam/escalation.md` / `fix-loop.state.md` | 🟢 | 通用控制文件 |
| `commands.rs::collect_artifacts` 硬编码 artifact 列表 | 🟡 | 应改为"扫 `.ccteam/*.md` 自动列出",或来自 team 配置 |
| `block-push` hook(M1+) | 🔴 | "git push"是 dev 团队的危险命令;research 团队的危险命令可能是"未经审阅就发用户邮件" |
| `security_reminder_hook.py`(plugin) | 🔴 | 完全 dev-specific |
| project CLAUDE.md "不要 git push / 测试不过不算完成" | 🔴 | dev 团队 CLAUDE.md 模板;research 团队需另一份 |

### 1.6 ESCALATE grammar(M0.5.4 引入,本文档加固)

| 语法档 | 归类 | 论证 |
|---|---|---|
| `ESCALATE: REVERT_TO_PHASE <name> — <reason>` | 🟢 | mechanism;`<name>` 取值是 🟡 |
| `ESCALATE: NEED_USER_INPUT — <questions>` | 🟢 | 用户在回路,所有团队共用 |
| `ESCALATE: ABORT — <reason>` | 🟢 | 永久标 failed |
| **建议新增** `ESCALATE: HYPOTHESIS_REJECTED — <reason>` 等团队特化前缀 | 🟡 | mechanism 是"team 可注册自己的前缀";前缀本身是团队语义 |
| 无前缀时降级为 `NEED_USER_INPUT` | 🟢 | mechanism |

### 1.7 防御层(L1 / L2 / L3)与 watcher

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

### 1.8 CLI 命令

| 命令 | 归类 | 论证 |
|---|---|---|
| `ccteam new <brief>` / `ls` / `show` / `start` / `stop` / `attach` / `peek` / `progress` / `pause` / `resume` / `answer` | 🟢 | 都是 mechanism——多一个 `--team=<name>` 参数即可泛化(详见 §3) |
| `ccteam doctor --tool-surface`(M0.5) | 🟢 | 校验 mechanism;校验对象是 🟡 |
| `ccteam doctor --install-recommended-agents` 的**清单** | 🟡 | dev 团队推荐 8 个 plugin agent;research 团队需另一份 |
| `ccteam memory rebuild`(M3) | 🟢 | RAG mechanism |
| `ccteam fork-reply`(M1+) | 🟢 | 通用 |

### 1.9 跨项目记忆

| 概念 | 归类 | 论证 |
|---|---|---|
| `~/.ccteam/memory/{patterns,anti-patterns,index.json}` 布局 | 🟢 | 通用 |
| pattern 文件字段(tech stack / 踩过的坑 / 成功设计 / 不要再做) | 🟡 | 当前是 dev 团队字段;mechanism 是"按 team 自定义 retro 字段" |
| 召回触发(Seed phase RAG) | 🟢 | 通用 |
| **跨团队记忆隔离 vs 共享**(本文档新增问题) | 🟢 | mechanism 应支持按 team namespace 隔离,默认隔离,允许配置共享(详见 §2.7) |

---

## 2. 团队扩展契约

要在 ccteam 上跑一支新团队,必须交付以下 7 件东西。**少一件,orchestrator 拒绝
启动**(`ccteam doctor --team <name>` 失败 → `ccteam start --team <name>` fail-fast)。

> ✋ M4.5 才会真正实现这套契约。本节先把契约钉死,后续 PR 按它实现 `team.yaml` 与
> `--team` 参数。

### 2.1 phase 集合(目录与命名)

```
phases-<team>/
├── 00-<entry>.md
├── 01-<...>.md
├── ...
└── NN-<terminal>.md
```

约定:
- **目录命名** `phases-<team>/`(`phases-dev/` / `phases-research/` / `phases-marketing/`)。
  当前主线 `phases/` 在迁移期 alias 为 `phases-dev/`(详见 §B 审计建议)。
- **文件命名**:`NN-<phase-name>.md`,`NN` 是排序前缀(orchestrator 按文件名排序
  推断 happy-path DAG;非线性 DAG 由 `next_phase` 字段在 front matter 显式声明)。
- **front matter 必含字段**:
  - `name`(必须与文件名 `<phase-name>` 一致)
  - `required_inputs[]` / `required_outputs[]`
  - `parallelism: solo | agent_team | multi_session`
  - **新增** `completion_signal: <SIGIL>`——本 phase 视作"内部循环成功完成"
    时 claude 必须输出的 token(替代 `fix_loop.rs` 里硬编码的 `TESTS_GREEN`)
  - **新增** `auto_loop: bool`(默认 `false`)——`true` 时进入 phase 自动写
    `<project>/.ccteam/auto-loop.state.md`(取代 `fix-loop.state.md`),Stop hook
    按 ralph 范式拦截重喂;`max_iterations` 与 `completion_signal` 来自本字段
  - `tools_required: { subagents, skills, mcp }`(M0.5 已有)
- 至少有一个 phase 标记为终态(下一节点 `next_phase: ~`),作为 ship 等价物。
- 至少有一个 retro phase——为跨项目记忆提供产出(详见 §2.6)。

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

### 2.5 完成信号定义(替代 dev "tests pass")

每团队必须声明:

1. **何时本项目算 done**——通常是 phase DAG 走到终态 phase 且无 escalate。
2. **每个 phase 的 `completion_signal`** —— ralph-loop / Stop hook 据此判定退出
   vs 重喂。dev 团队 fix phase 用 `TESTS_GREEN`;research synthesis phase 可能
   用 `INSIGHTS_TRIANGULATED`。
3. **自循环 phase 的失败兜底**——例如 research 的 primary 数据收集 phase 在
   max_iterations 仍未拿到 ≥3 来源时,orchestrator 是 escalate 给用户、还是
   自动放弃这个项目。每团队在 `team.yaml` 选 `on_loop_exhaust: escalate | abort`。

### 2.6 推荐 plugin / agent 安装清单

每团队提供 `team.yaml.recommended_agents`,`ccteam doctor --install-recommended-agents
--team <name>` 据此 ln -sf。dev 团队的 8 个 agent(M0.5.1)只是 dev 实例;research
团队需要的可能完全不同(详见 §C 起草 phase 集时给出的初始清单)。

清单要素:
- agent 来源(`claude-plugins-official:<plugin>/agents/<name>` / 自带脚本路径)
- 默认挂载 phase(信息性,不强制)
- 用一句话说明何时该被调用(给 LLM 决策时读)

### 2.7 与跨项目记忆(M3)的对接方式

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
# ~/.ccteam/teams/<team-name>.yaml(规范化文件名;M4.5 实现)
name: research
phase_dir: phases-research                # 相对 ccteam 安装目录
entry_phase: 00-topic                     # 等价 dev 的 plan-eng
critic_dimensions: [...]                  # §2.3
escalate_prefixes: [...]                  # §2.4
completion_signal_default: PHASE_DONE     # phase 没声明时的兜底
on_loop_exhaust: escalate                 # §2.5
recommended_agents: [...]                 # §2.6
recommended_skills: [...]
recommended_mcp: [...]
memory_namespace: team_only               # §2.7
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
- ✅ team 配置 `retro_schema[]` 决定字段;RAG 索引按 schema 类型决定做向量化
  vs 关键字索引。

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

按优先级分布:

| 优先级 | 数量 | 编号 | 含义 |
|---|---|---|---|
| **P0 阻塞泛化** | 6 | F1, F2, F3, F4, F12, F13 | 不解耦无法跑非 dev 团队;M4.5.1 必须清完 |
| **P1 该做但可后置** | 10 | F5, F6, F7, F8, F9, F10, F11, F15, F19, F20 | M4.5 中后段或 M4.6 内做完;延后会让命名内外冲突或文档失真 |
| **P2 边角** | 3 | F16, F17, F18 | 跟 P0/P1 同 PR 顺手做;不阻塞功能 |
| **N/A 已是领域无关** | 1 | F14 | 显式排除,避免后续误判 |

**P0 关键路径**:F1(`auto_loop` 字段)+ F2(DAG 由 phase 模板推断)+ F3
(`FIRST_PHASE` 改 DAG entry node)+ F4(`is_terminal` 改 DAG 终点判断)+
F12(CLI `--team`)+ F13(`state.json.team` 字段)——这 6 条解耦后,
ccteam-core 即可跑非 dev 团队。

**§B 元发现(对 §A 的反馈)**:`pub use ... M0_PHASE_DAG, FIRST_PHASE`
在 `crates/ccteam-core/src/lib.rs:21` 把 dev 假设暴露到 lib 接口表面——
M4.5.1 是一次 lib API breaking change。按 CLAUDE.md §五.3 不写 backwards-
compat shim,直接换。这条已写进 §6.3 风险表与详审 §F2。

审计过程中**没有发现**需要修订 §1 责任分界表或 §2 团队扩展契约的位置——
所有发现都能映射到现有分类。这是抽象切对的好信号。

---

## 5. 首个非 dev 团队的选择论证

候选:research / marketing / ops。

**选 research 优先**,理由:

### 5.1 工具面与 dev 重叠最多——验证 mechanism 复用率

- `code-explorer` 在 research 里就是"已有资料探索"的 agent——同一个 plugin
  agent 直接 ln -sf,只换上下文 prompt。
- `pr-test-analyzer` / `silent-failure-hunter` 在 research 里没有等价物——验证
  "推荐 agent 清单按 team 切换"机制能正确识别"这个 agent 不该装到 research"。
- 不需要新 MCP——dev 已有的 Playwright / GitHub MCP 不是必须;Telegram bot
  (M1+) research 同样用得上。
- 结论:research 不引入"全新工具栈"的复杂度,纯粹考验"已有工具该不该换名 /
  换组合"的抽象——正适合验证 §2 团队扩展契约。

### 5.2 难点集中在"完成信号"和"Critic 维度"——暴露抽象漏洞

- dev 完成信号是物理事实(测试退出码 = 0);research 完成信号是判断("3 个一手
  来源经过交叉验证")——逼迫 `completion_signal` 字段从硬编码 `TESTS_GREEN` 升
  级为 phase 模板可注入。
- dev 6 维 Critic 评 code 品质;research Critic 评 method 品质——逼迫 critic_dimensions
  从硬编码升级为 team 配置。
- dev fix-loop 重喂修代码;research 假设被反驳要回 hypothesis phase 重设——逼迫
  `FixLoopState` 从"复读 prompt"升级为"phase DAG 上的 revert 路由",
  正好对应 §2.4 ESCALATE grammar 扩展。

### 5.3 marketing / ops 留到 research 验证之后

- **marketing**:涉及外部副作用(发邮件 / publish 帖子),需要更严格的 L1 危险
  命令拦截 + 用户预审环节;现在 ccteam 还没把"发布前必经用户拍板"做成 mechanism。
  让 research 先暴露 §2.4 ESCALATE grammar 抽象漏洞,再做 marketing。
- **ops**:on-call / 故障响应类工作,完成信号是"系统恢复 / SLO 达标"——需要
  external monitor 接入(监控指标),ccteam 当前没有 metric MCP。可作 M5+ 探索。

### 5.4 验证目标(research 团队 ship 后,本文档 §1/§2 应被回填)

- §1 责任分界表:每条 🟡 的 phase 模板 / team 配置注入路径已实证可行
- §2 团队扩展契约:7 件东西每件都对 research 团队有具体填充
- §3 显式拒绝清单:research 团队没有让任何条目松动
- §4 审计发现:对应 P0 项已在 research 上线前修复

---

## 6. 里程碑落点建议

**不进 M0 / M0.5 / M1 / M2 / M3 / M4**——这些里程碑都还在补"dev 团队跑得稳"
的洞,现在加 team 抽象只会让两件事都做不好。

### 6.1 M4.5 — Team Abstraction(本文档对应里程碑;约 2 周)

**唯一验收**:`ccteam new --team=research "<topic>"` 能跑通 happy path,
产出最终研究报告;dev 团队的现有项目零迁移成本(`ccteam new "<brief>"` 默
认 `--team=dev` 仍然工作)。

任务草案(M4.5 起):

| # | 任务 | 验收 | 依赖 |
|---|---|---|---|
| M4.5.1 | §B 审计的 P0 项目全部修复 | `cargo test` 通过;dev pipeline happy path 不变 | M4 |
| M4.5.2 | `team.yaml` schema + 解析 | `ccteam doctor --team dev` 列出 dev 团队当前的 7 件契约都从配置读 | M4.5.1 |
| M4.5.3 | `ccteam new --team <name>` / `start --team <name>` | `--team` 缺省值 `dev`(向后兼容);非 dev team 启动需 team.yaml 存在 | M4.5.2 |
| M4.5.4 | `phases-research/` 起草(§C 产出)纳入仓库 | research 团队 phase DAG 通过 `validate_team` 校验 | M4.5.2 + §C |
| M4.5.5 | 跨项目记忆 namespace 化 | dev 项目召回不污染 research 项目;anti-pattern 仍跨 team 共享 | M3 |
| M4.5.6 | research 团队端到端 happy path | 起一个真实 research 项目跑到 ship | M4.5.1–5 |

### 6.2 为什么不更早?

- **M0/M0.5 还在补 mechanism 漏洞**(idle 注入、ralph-loop、tools_required 校
  验)——这时把 team 抽象出来等于在抖动地基上加层。
- **M1/M2 在补用户体验**(telegram、Seed Gate、Score)——这些机制在 dev 团队
  上要先稳定,Critic 维度 / fork 决策的形态成熟了再泛化。
- **M3 才上线跨项目记忆**——namespace 隔离设计(§2.7)依赖 M3 的 RAG。
- **M4 critic agent 闭环 + anti-leniency** 后,§2.3 critic_dimensions 抽象
  的语义才稳定——M4 之前抽,会出现"抽象出来后又改"的反复。
- 因此 M4.5 是最早能不返工地承接"team 扩展"的窗口。

### 6.3 风险

| 风险 | 触发 | 应对 |
|---|---|---|
| §B 审计 P0 项过多,M4.5.1 单条堵住整个里程碑 | 审计发现深层耦合(例:fix-loop 状态机假设 dev 流程) | M4.5.1 拆为多个子 PR,每条 P0 独立 PR;按 §B 优先级排序逐个清 |
| dev 团队的 `team-dev.yaml` 反推时和现状不一致 | 写 team-dev.yaml 时发现某些行为靠"巧合"工作,没显式契约 | 反推时逐条对照 §1 责任分界表;"没契约的现状"必须先写到 §1 再纳入配置 |
| research 团队跑通靠的是借用 dev plugin 的能力,而不是真验证了 §2 契约 | research phase 模板偷懒,ESCALATE 不用 §2.4 自定义前缀,critic 不用 §2.3 自定义维度 | M4.5.4 验收时强制要求 research 至少有 1 个自定义 ESCALATE 前缀 + 至少 1 个 dev 没有的 critic 维度 |
| §3 "显式拒绝清单"被 PR 软性绕过 | "为了通用"在 ccteam-core 加 `if team == "research"` | code-review 加规则:`ccteam-core/` 内出现 team 名字符串字面量 = 自动拒收 |

---

## 7. Meta-Agent Pattern — ccteam 的最终使用形态

ccteam 当前是"开发团队的编排层",泛化后是"任意 AI 团队的编排层"。但**用户怎么调度
这些团队**?如果用户每次都直接 `ccteam new --team=<name> "<brief>"` 在终端里手敲,
ccteam 就只是一个命令行工具,损失了"对话式驱动 AI 工作流"的体验。

正确形态是 **meta-agent pattern**:用户的日常 Claude Code 会话本身就是 meta-agent,
它跟用户聊需求、决定派单 / 不派单、调度多个 ccteam-managed team session 干高密度
工作、汇总结果再跟用户对话。

**关键点**:meta-agent **不是 ccteam 内嵌的另一个 LLM 层** —— 那是 tech-design §10
显式拒绝的 Symphony 多层 agent 反模式。meta-agent **就是用户自己已经在跑的那个 daily
driver Claude Code 会话**,装上 ccteam 提供的几个集成件之后,自动具备 meta-agent 角色。

### 7.1 已存在零件的角色对位

| ccteam 设计组件 | 在 meta-agent pattern 里扮演 | 对应里程碑 |
|---|---|---|
| `ccteam-control` skill | meta-agent 在任何目录都能调用 ccteam 的"指挥棒" | M1.8 |
| `ccteam-mcp` MCP server | meta-agent 派单的结构化控制面(`ls` / `show` / `new` / `peek` / `progress` / `pause` / `resume`) | M2.8 |
| 跨项目记忆 RAG / claude-mem MCP | meta-agent "上次相似项目"的长期记忆(项目级) | M4(记忆里程碑;reorder 后) |
| 长 tmux session per project | dev / research / ... 团队的"高密度施工工地" | M0.7 |
| 团队抽象(`team.yaml` + `--team` CLI) | meta-agent 派单时选择"派给哪支团队" | M3(本文档对应里程碑) |

**结论**:M3(团队抽象)+ M4(跨项目记忆)+ M2(`ccteam-mcp`)+ M1(`ccteam-control`)
四件齐备,meta-agent pattern 自动可达。**ccteam 不需要单独立"meta-agent 里程碑"**——
这四个本来就要做。

### 7.2 三块 ccteam 现状没显式覆盖的新机制

#### 7.2.1 conversation continuity — meta-agent 的"上次我们聊过"

跨项目记忆(M4)的语义是**项目级**的(已 ship 项目的 retro / pattern / anti-pattern)。
但 meta-agent 跟用户的**对话历史**(讨论但还没派单的想法、被拒绝的方向、用户偏好)
没有现成存放位置。

**候选解法**(M4 实现 RAG 时一并考虑):

- **(a) `claude-mem` MCP 在 user namespace** —— RAG index 加一个 `user/` 命名空间,
  专门存 meta-agent 对话摘要。每轮对话结束后 meta-agent 主动写一条 summary
- **(b) 主动落 `~/.ccteam/meta/conversation-log.md`** —— 滚动 markdown,每次 meta-agent
  会话开始时读,结束前追加。简单但会膨胀
- **(c) user-level CLAUDE.md** —— 让用户的 daily driver claude 在 `~/.claude/CLAUDE.md`
  自动加一段 ccteam 上下文(当前活跃项目列表 / 最近决策),每次会话启动自动加载

**M4 实施时挑一个**——不要拖到 M5。具体哪个最优依赖于 M4 RAG 实现细节。

#### 7.2.2 dispatch protocol formalization — meta-agent 该怎么派单

用户说"做一个 todo app",meta-agent 要做的决策链:

1. **是问答还是项目请求?** —— 问答直接答(meta-agent 自己用工具回答),项目请求才进
   下一步
2. **分类团队类型** —— dev / research / marketing / ops / 综合体(后两个 M5+)
3. **pre-flight clarification** —— 对应 M2 Seed phase 的 CLARIFY,但**在 ccteam 派单
   之前**完成,避免"用户说一句 → ccteam 起 session → Seed 再问一遍"的双重澄清
4. **通过 `ccteam-mcp` 派单** —— `ccteam__new(team="dev", brief="...")`
5. **后续监控** —— `ccteam__progress` / `ccteam__peek` 看进度,关键事件(escalation /
   completion)由 telegram bot push 给用户

这套流程是个具体的 **prompt + tool-call 组合**,可以打包成 skill。建议在 M2 完成
`ccteam-mcp` 之后立项 `ccteam-dispatch` skill(详见 §7.3)。

#### 7.2.3 default meta-agent behavior preset

`ccteam-control`(M1.8)是**能力 skill** —— 让用户的 claude **能**调度 ccteam。但只
有能力不够,还要有**行为约束** —— 当用户提项目级请求时,meta-agent 应该是 dispatcher
不是 worker:**别自己抄起 Edit 工具开干,先 ccteam_new + 派单**。这是个反直觉点:
Claude Code 默认行为是"用户问什么我都自己上手做",meta-agent 模式要它**克制**。

打包成 `ccteam-dispatch` skill,内容包括:

- **决策树**(§7.2.2 的形式化)
- **明文克制规则**:"识别到项目级请求时,默认通过 ccteam-mcp 派单,而不是自己写代码 /
  自己跑研究 / 自己起草营销文案;只有在用户明确说'你直接帮我写 X'时才走 worker 路径"
- **对话风格约束**:meta-agent 跟用户对话时**不展示 progress 细节**(那是 ccteam 的
  CLI / TUI 的活),只汇报里程碑事件

### 7.3 ccteam 自身需要承担的两件事

虽然 meta-agent **跑在用户的 claude 而不是 ccteam 内部**,但 ccteam 仍然要负责两件事
让 meta-agent pattern 真正落地:

1. **打包并发行 meta-agent skill 集** —— `ccteam-control` + `ccteam-dispatch` + 推荐
   的 `claude-mem` 配置,`ccteam doctor --install-meta-agent` 一键装到用户的
   `~/.claude/skills/`。降低用户进入 meta-agent 模式的成本
2. **在文档里把 meta-agent pattern 作为 ccteam 的推荐使用方式显式描述** —— 不是"你也
   可以这么用",而是"**这是 ccteam 设计意图的最终形态**"。M2 文档化(`ccteam-mcp`
   发行配套)、M3 完善(团队抽象之后才有完整派单菜单)、M4 收尾(meta-agent
   conversation continuity 落地)

### 7.4 与 §3 显式拒绝清单的一致性

meta-agent pattern **不违反** §3 任何一条:

- 不是"ccteam 内嵌 meta-agent"——meta-agent 跑在用户的 claude 进程里,**与 ccteam
  orchestrator 完全异进程**
- 不替领域定 done criteria —— meta-agent 把项目派给团队后,完成判定仍由该团队的
  `team.yaml.completion_signal` 决定(§2.5)
- 不引入新的 LLM 编排层 —— meta-agent 是用户已有的 claude,ccteam 只是给它装更好的
  工具集

如果将来发现某个 meta-agent 行为约束**必须**靠 ccteam 内嵌某个 LLM 才能实现,那是
信号:回头审视该约束是不是放错位置了——它可能本来就该是某个 phase 模板的事,而
不是 meta-agent 的事。

### 7.5 落点回到里程碑

| 里程碑 | meta-agent pattern 进展 |
|---|---|
| M1 | `ccteam-control` skill 上线 → meta-agent 有了"指挥棒" |
| M2 | `ccteam-mcp` MCP server 上线 + `ccteam-dispatch` skill 起草 → meta-agent 有了结构化派单工具 |
| M3 | 团队抽象上线 → meta-agent 派单时能选 `--team` |
| M4 | 跨项目记忆 + conversation continuity 落地 → meta-agent 有了完整记忆 |
| M5+ | 多团队协作(research → dev pipeline)→ meta-agent 能编排跨团队工作流 |

---

## 8. 与 CLAUDE.md / tech-design.md / development-plan.md 的关系

- **CLAUDE.md** §一"定位:ccteam 是 Claude Code 之上的元工具"是本文档的精神
  上游——把"meta-tool"再往上抽一层,得出"meta-tool of any AI team"。
- **tech-design.md** 是 mechanism 的设计论证;本文档**不**改 mechanism,只
  把它们标"哪些是 mechanism、哪些是 dev fill"。
- **development-plan.md** 是任务清单;本文档建议在 §6 加 M4.5 里程碑,具体
  任务粒度由 development-plan §X 维护(待 M4.5 真正启动时拆)。
- **interfaces.md** 是协议字段表;本文档建议在 phase YAML schema 加
  `completion_signal` / `auto_loop` 字段(§2.1),那是 interfaces.md §5.1 的
  扩展——M4.5.2 提交协议变更时必须同步 interfaces.md。

## 9. 本文档维护纪律

1. **任何 PR 引入新机制前**,先确认它是 §1.x 中已分类项的补充还是引入新
   概念。引入新概念必须在 §1 加一行(注明🟢/🟡/🔴),否则无法 review。
2. **§B 审计的发现可以反过来修订 §1 / §2**——发现某条机制的 dev 假设比预
   想深,本文档要更新,不要硬塞 audit 节。
3. **本文档不超过 800 行**——超出说明在重复 tech-design / interfaces;砍
   重复内容,留指针。(M3 reorder + meta-agent pattern 落地后从 558 → 720+,
   仍在阈值内。下一次重大扩展前,先看能不能砍掉哪节)
4. **commit message 用英文,文档内容用中文**(沿袭仓库现状)。
