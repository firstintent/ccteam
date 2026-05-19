# ccteam 作为领域无关编排层

> **本文档是团队抽象的永久 charter**(V0.4.6 同步)。回答一个问题:**ccteam
> 当前像"一个 dev 团队的编排器",但核心编排机制实际与"做软件"领域无关——
> 把 workflow 拓扑换一组,同一台机器应该能跑研究团队、营销团队、运营团队**。
>
> **职责单一**:本文负责把"编排层 vs 领域层"的责任分界钉死,给出未来扩展新
> 团队的契约,并显式列出**编排层不应替领域决策**的边界。**不**重复
> `tech-design.md` / `interfaces.md` 的架构与协议细节——所有架构决策(tmux 长
> session / progress.jsonl 唯一事实来源 / context 60% reset / idle-aware 注入 /
> 三层防御)沿用 tech-design.md。
>
> 阅读位置:`tech-design.md` / `interfaces.md` 之上的**抽象层 charter**——任何
> PR 引入新机制前,先回本文的 §1 责任分界表问"这是编排层职责还是领域层职
> 责",再决定该写到哪里。

---

## 0. TL;DR

- **领域无关(mechanism)**:进程拓扑、状态机、文件协议、事件流、context 管理、
  注入策略、cost/stall 软告警、ESCALATE 语法外壳、workflow.yaml schema、
  subagent/skill/MCP 三类工具触发面、防御层 PASS/CONCERN/BLOCK 三档汇总——这
  些都不假设你做的是软件。
- **领域特定(domain fill)**:`.claude/agents/<role>.md` 内容、Critic 评分维度、
  推荐 plugin agent 清单、artifact 文件命名、危险命令拦截清单——这些假设你在
  做软件。
- **首个非 dev 团队(product-research)2026-05-06 已 ship**:验证了 §1 责任分界
  表 / §2 团队扩展契约无需修订。V0.4.0 重构(workflow.yaml + ArtifactWatcher +
  thin orchestrator)进一步降低团队扩展门槛。

---

## 1. 责任分界表

记号:
- 🟢 **orchestrator**:领域无关,代码必须不假设领域语义,可被任何团队复用。
- 🟡 **team-config**:领域特定,但在编排层用**配置 / 数据 / 字符串**承载——代
  码不写死,通过 workflow.yaml / `team.yaml` / 推荐清单注入。
- 🔴 **team**:领域特定,**只能在该团队的 agent markdown / hook 脚本**里实现,
  绝不进入 `ccteam-core`。

### 1.1 状态与协议

`state.json` 全部字段(详 interfaces.md §2)— 🟢 mechanism。论证:进程身份、执
行栈状态、cost / token / context 物理事实、调度器时间线事实,所有团队共用。
`parallelism` 枚举(`solo` / `agent_team` / `multi_session`)+ flex
`sessions{}` / `next_sid_seq{}` 同样 🟢;V0.4.0+ 由 workflow.yaml
`AgentSpec::parallelism` 数据驱动。

### 1.2 progress.jsonl 事件类型

7 类业务 event(`workflow_start` / `agent_spawn` / `agent_done` /
`artifact_received` / `gate_triggered` / `budget_exceeded` / `workflow_done`)+
进程生命周期(`session_start` / `SessionEnd`)+ Claude Code hook 透传
(`PreToolUse` / `PostToolUse` / `SubagentStop` / `Stop` / `notification`)+
`escalation` / `watcher_concern` / `watcher_block` / `context_reset` 全部 🟢
mechanism;事件 schema 见 interfaces.md §4。**仅 `reason` / `role` 字段内容**
是 🟡(团队语义,见 §1.6)。

### 1.3 workflow.yaml schema(V0.4.0+ 主拓扑入口)

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
| `AgentSpec::input` / `AgentSpec::output` | 🟢 | 字段名是 mechanism;路径是 🟡 |
| `AgentSpec::interval` | 🟢 | `schedule` 配套;数字是 🟡 |
| `AgentSpec::timeout` + `on_timeout`(`escalate` / `retry` / `skip`) | 🟢 | watchdog 三档语义全通用 |
| **prompt 内容字段** | **明令禁止** | 见 §1.4 workflow-as-data 红线 |

### 1.4 Workflow-as-data 红线(V0.4.6 加固)

> **核心红线**:workflow.yaml 是**拓扑数据**——连线 + trigger 类型 + 并发上限 +
> 软开关 + budget,**绝不出现 prompt 字面量**。agent 行为 prompt 住
> `.claude/agents/<role>.md`(LLM-prompt content 层,**由 Claude Code 加载,不
> 由 ccteam 解析**)。ccteam orchestrator **不读 prompt,不路由 prompt,不在状
> 态机里做"如果 prompt 提到 X 就 Y"判断**——orchestrator 是文件系统事件调度
> 器,不是 NL 中间件。

**允许**(拓扑数据 + orchestrator-level 控制):拓扑 IndexMap、trigger 类型、
`enabled` / `budget` / `parallelism` / `executor` / `input` / `output` /
`timeout` + `on_timeout` 字段(各字段语义见 §1.3 表)。

**禁止**(LLM-content 层 → `.claude/agents/<role>.md`):agent 行为指令 / prompt
文字 / role 描述长文本、决策树 / 状态机分支描述、"如果 X 就 Y"条件分支字面量。

**违反这条红线的 PR 应被拒收**——orchestrator 解析 prompt 内容等于在
ccteam-core 内嵌 LLM,违反 §3 显式拒绝清单 + Symphony 反模式红线(channel
adapter 进程内不嵌 LLM)。`.claude/agents/<role>.md` 是 Claude Code 官方 subagent
definition 路径,ccteam 只负责文件存在性校验(`ccteam doctor
--validate-workflow`),不负责语义解析。`crates/ccteam-core/src/workflow.rs::
WorkflowSpec` 是 SoT,新字段加入前先回本节问"这是拓扑数据还是 prompt 内容?"
meta-agent dispatch 时**不**解析 `<role>.md` 内容 — 只看 workflow.yaml 拓扑 +
通过 `mcp__ccteam__workflow_spawn_agent(role=...)` 派单。

### 1.5 orchestrator 决策点(thin orchestrator)

| 决策点 | 归类 | 论证 |
|---|---|---|
| `WorkflowSpec::enabled` + 热加载 | 🟢 | mechanism 通用;workflow 实例软开关,所有团队共用 |
| `WorkflowSpec::budget` | 🟢 | rolling 24h cost cap + 1h spawn cap;数字由 team / 项目配置 → 取值是 🟡 |
| ArtifactWatcher inotify/fsevents 触发 | 🟢 | 文件系统是控制平面(tech-design §2.2 红线);watch 路径是 🟡 |
| 60% reset 阈值 | 🟢 | claude 进程级事实 |
| idle-aware 注入(Stop/notification → 直注;否则 /btw) | 🟢 | mechanism |
| cost ladder 阈值($20 / $50 / $200) | 🟢 | 默认值是 mechanism;数字本身可由 `~/.ccteam/config.yaml` 与 workflow.yaml `budget` 覆盖 |
| stall ladder(5/15/30 min) | 🟢 | 同上 |

### 1.6 Hook 与文件路径

| 文件 / 路径 | 归类 | 论证 |
|---|---|---|
| `~/.ccteam/{inbox,queue,control,progress}` + `~/.ccteam/config.yaml`(V0.4.2+)+ `~/.ccteam/projects/<slug>/`(V0.4.6+) | 🟢 | 全局编排层布局 |
| `~/projects/<slug>/.ccteam/state.json` | 🟢 | 项目元数据 mechanism |
| `~/projects/<slug>/.ccteam/spec.md` | 🟡 | 字段名是 mechanism;research 团队等价物可能叫 `topic.md` / `brief.md`——应在 team 配置中声明 |
| `~/projects/<slug>/.ccteam/escalation.md` | 🟢 | 通用控制文件 |
| `commands.rs::collect_artifacts` artifact 列表 | 🟡 | 应"扫 `.ccteam/*.md` 自动列出",或来自 team 配置 |
| `block-push` hook(M1+) | 🔴 | "git push"是 dev 团队的危险命令;research 团队的危险命令可能是"未经审阅就发用户邮件" |
| `security_reminder_hook.py`(plugin) | 🔴 | 完全 dev-specific |
| project CLAUDE.md "不要 git push / 测试不过不算完成" | 🔴 | dev 团队 CLAUDE.md 模板;research 团队需另一份 |

### 1.7 ESCALATE grammar

| 语法档 | 归类 | 论证 |
|---|---|---|
| `ESCALATE: REVERT_TO_PHASE <name> — <reason>` | 🟢 | mechanism;`<name>` 取值是 🟡 |
| `ESCALATE: NEED_USER_INPUT — <questions>` | 🟢 | 用户在回路,所有团队共用 |
| `ESCALATE: ABORT — <reason>` | 🟢 | 永久标 failed |
| 团队特化前缀(如 `HYPOTHESIS_REJECTED`) | 🟡 | mechanism 是"team 可注册自己的前缀";前缀本身是团队语义 |
| 无前缀时降级为 `NEED_USER_INPUT` | 🟢 | mechanism |

### 1.8 防御层(L1 / L2 / L3)与 watcher

| 概念 | 归类 | 论证 |
|---|---|---|
| L1 `required_outputs` 校验机制 | 🟢 | 通用 |
| L1 危险命令拦截**机制** | 🟢 | hook + matcher 是 mechanism |
| L1 危险命令**清单**(git push / rm -rf / deploy 脚本) | 🔴 | dev-specific |
| L2 audit agent 调度 mechanism(PASS/CONCERN/BLOCK 三档输出) | 🟢 | 通用 |
| L2 audit 角色集(architect / critic / designer / security / scope-watcher) | 🔴 | dev 团队的 critic 视角;research 团队需要 method-critic / source-quality-critic 等 |
| L2 cross-cutting watcher(cost-watcher / scope-watcher / drift-detector) | 🟡 | cost-watcher 通用;scope/drift 假设了"有 spec / 有 plan-eng",research 也有等价物但需重命名 |
| L3 用户 fork 决策 | 🟢 | 通用 |
| 信任档位 `yolo` / `balanced` / `careful` | 🟢 | 通用 |

### 1.9 CLI 命令

| 命令 | 归类 | 论证 |
|---|---|---|
| `ccteam new <brief>` / `ls` / `show` / `start` / `stop` / `attach` / `peek` / `progress` / `pause` / `resume` / `answer` | 🟢 | 都是 mechanism——多一个 `--team=<name>` 参数即可泛化(详见 §3) |
| `ccteam doctor --tool-surface` | 🟢 | 校验 mechanism;校验对象是 🟡 |
| `ccteam doctor --install-recommended-agents` 的**清单** | 🟡 | dev 团队推荐 8 个 plugin agent;research 团队需另一份 |
| `ccteam fork-reply`(M1+) | 🟢 | 通用 |

### 1.10 跨项目记忆

> M4 不再自建索引/向量库,完全复用 Claude Code 官方机制。详见
> `docs/tech-design.md §3.7`。

| 概念 | 归类 | 论证 |
|---|---|---|
| `~/.claude/projects/<encoded>/memory/` per-repo auto-memory | 🟢 | 官方机制,Claude 自主写;通用,与 ccteam-core 解耦 |
| `~/.claude/rules/ccteam-lessons-<team>.md` 跨项目共享 | 🟢 | 通用机制(rules + `paths:` frontmatter scope `~/projects/<team>-*`,F22 后 slug 加 team 前缀,scoping 实际生效)|
| `team.yaml.retro_schema[]` 字段定义 | 🟡 | dev 已填 4 字段,product-research 必须补;mechanism 是"按 team 定义 retro 字段段落" |
| 召回触发 | 🟢 | 官方机制自动注入 rules + 可选 `mcp__*claude-mem*search`,LLM 自看 tool surface 决定 |
| 跨团队记忆隔离 vs 共享 | 🟢 | rules 文件按 team 分名 + `paths:` frontmatter 按项目目录前缀 scope 实现隔离 |

---

## 2. 团队扩展契约

要在 ccteam 上跑一支新团队,必须交付以下 7 件东西。**少一件,orchestrator 拒绝
启动**(`ccteam doctor --team <name>` 失败 → `ccteam start --team <name>`
fail-fast)。

### 2.1 Workflow 拓扑(`workflow.yaml` 是数据驱动入口)

新团队交付:

1. **`team.yaml::workflows[]`** — 声明本团队可用的 workflow 实例(team-level
   registry,`~/.ccteam/teams/<team>.yaml`)。每条引用一个 workflow.yaml 模板路
   径 + 描述。
2. **项目实例 `<project>/.ccteam/workflow.yaml`** — 由 `ccteam init` /
   `ccteam new` 或 `ccteam-creator` skill 生成,包含具体 agent 拓扑 + trigger +
   parallelism + budget。完整 schema 见 §1.3;字段红线见 §1.4。
3. **`.claude/agents/<role>.md`** — 每个 workflow 引用的 role,在
   `.claude/agents/` 下有同名 markdown(Claude Code 官方 subagent definition 路
   径)。**这是 agent 行为 SoT**(prompt + tools + role description),由用户 /
   `ccteam-creator` 手写,**ccteam 不读、不解析、不修改**。

**workflow.yaml 骨架示例**(与 `crates/ccteam-core/src/workflow.rs::WorkflowSpec`
一致):

```yaml
name: dex-ui-autoloop
description: "DEX UI 自循环改 bug pipeline"
enabled: true
budget:
  max_cost_usd_per_24h: 5.00
  max_agent_spawns_per_hour: 100
agents:
  explorer:
    executor: claude
    trigger: watch:.ccteam/issues/
    parallelism: 2
    input: .ccteam/issues/
    output: .ccteam/fix-requests/
    timeout: 30m
    on_timeout: escalate
  fixer:
    executor: claude
    trigger: gate
    input: .ccteam/fix-requests/
    output: .ccteam/fixes/
```

约定:
- **agents 是 IndexMap**(YAML 声明顺序保留)→ trigger 图按声明顺序
  deterministic build
- **至少一个 agent** 必须有非空 trigger;否则 `ccteam doctor --validate-workflow` fail
- **role 名** 必须与 `.claude/agents/<role>.md` 文件名匹配(orchestrator 启动期
  + spawn 前各校验一次;缺文件 → fail-fast)
- **trigger 4 类**:`manual`(meta-agent 显式派单)/ `schedule`(cron,opaque
  interval)/ `gate`(等 `trigger_gate` MCP)/ `watch:<path>`(inotify)
- **数据驱动红线** — workflow.yaml 字段表里**不**允许 prompt 字面量;详见 §1.4

### 2.2 `tools_required` 清单

每个 agent role 必须显式声明它使用的工具:

```yaml
tools_required:
  subagents: [code-reviewer, code-architect]
  skills: [ccteam-control]
  mcp: [Telegram, Playwright]
```

orchestrator 启动期(`ccteam start --team research`)枚举三类来源做交叉比对,缺
谁报缺谁 + 给出修复命令。**未声明的工具调用** = silent fail。

### 2.3 Critic 维度定义(替代 dev 6 维 Score)

每团队提供 `team.yaml.critic_dimensions`:每条维度 = `name` + `weight` +
`weak_threshold`(任一维度 ≤ 此值 → 自动 BLOCK)+ `anti_leniency_strictness`
(`lenient` / `normal` / `strict`)+ `rubric`(0-1 评分指南)。dev 团队的 6 维
(Functionality / Quality / Tests / UX / Speed / Docs)只是这套 schema 的一个具
体实例。

**三条 invariant**(M5 实现红线):

1. **`critic_dimensions[]` 是数据,不是 Rust enum** — `crates/ccteam-core/` 不
   得出现 dev 6 维的字符串字面量;`team.yaml` 加载到
   `Vec<CriticDimensionConfig>`,所有维度名 / 权重 / 阈值都从配置读
2. **`anti_leniency_strictness` 是 per-dimension 元数据** — dev critic 受测试退
   出码兜底所以维度普遍 `normal`;research critic 是纯 LLM 主观判断,核心维度需
   要 `strict`(必须有至少一项 BLOCK 才算"批评足够")。算法签名:
   `fn check(dims: &[CriticDimensionConfig], scores: &[CriticScore]) -> AntiLeniencyVerdict`
3. **`weak_threshold` 由配置控制,不是常量** — `crates/ccteam-core/src/score.rs`
   不得出现 `const WEAK_THRESHOLD: f32 = 0.4;`,必须从 `CriticDimensionConfig.weak_threshold` 读

### 2.4 ESCALATE grammar 扩展

每团队声明 `team.yaml.escalate_prefixes[]` — 每条 = `prefix` + `route`
(`REVERT_TO_PHASE` / `NEED_USER_INPUT` / `ABORT`)+ `target_phase` + `reason`。
例:research 团队的 `HYPOTHESIS_REJECTED → REVERT_TO_PHASE 02-hypothesis`、
`SOURCE_UNAVAILABLE → NEED_USER_INPUT`。前缀本身是数据,分发逻辑是 mechanism。

### 2.5 推荐 plugin / agent 安装清单

每团队提供 `team.yaml.recommended_agents`;**V0.4.0+ 首要交付改为
`.claude/agents/<role>.md`**(workflow.yaml 引用的 role 必须各有同名 md);
`recommended_agents` 字段作 hint 用,不强制。

清单要素:
- agent 来源(`claude-plugins-official:<plugin>/agents/<name>` / 自带脚本路径)
- 默认挂载 role(信息性,不强制)
- 一句话说明何时该被调用(给 LLM 决策时读)

### 2.6 与跨项目记忆(M4)的对接方式

每团队声明:
- **retro 输出 schema**:patterns 文件该有哪些字段(dev: tech stack / 踩过的
  坑;research: 数据源 / 假设结果 / 方法学反思;...)。
- **召回时 namespace 策略**:
  - `team_only`(默认):跨同 team 的项目召回
  - `cross_team`:允许跨 team 召回(anti-pattern 跨域有意义,成功 pattern 不一
    定可迁移)
- **anti-pattern 全局共享**:REJECT/ABORT 案例对所有团队都召回——避免重复犯
  错。

### 2.7 团队配置文件骨架

`~/.ccteam/teams/<team-name>.yaml` 顶级字段:`name` / `workflows[]`(指向
workflow.yaml 模板)/ `critic_dimensions[]`(§2.3)/ `escalate_prefixes[]`
(§2.4)/ `recommended_agents` / `recommended_skills` / `recommended_mcp`
(§2.5)/ `retro_schema[]`(§2.6)/ `artifacts{spec, primary_data_dir,
final_report}` / `danger_command_patterns[]`。完整 schema 见 interfaces.md §5.5;
现有 reference impl `teams/dev.yaml` + `teams/product-research.yaml`。

> M4 走官方 `~/.claude/rules/` + per-repo auto-memory,**无** `memory_namespace`
> 字段;跨项目隔离通过 rules 文件按 team 分名 + `paths:` frontmatter 实现。

---

## 3. 编排层不应承担的职责(显式拒绝清单)

防止后续 PR 在"为了通用"的名义下把领域决策悄悄塞进 `ccteam-core`。**违反这条
清单的 PR 应被拒收**——领域决策不是编排层的扩展点。

### 3.1 不替领域定 done criteria

- ❌ 不在 `ccteam-core` 写"如果测试全绿 + critic PASS 则 done"。
- ✅ 通过 workflow 终态 + role.md 内的可执行验证表达。
- 例外:**进程级别的 done**(claude 主动退出 / SessionEnd)是 mechanism,不在此限。

### 3.2 不替领域选 plugin

- ❌ 不在 `ccteam-core` import / 调用具体 plugin 名(`pr-review-toolkit` / `code-simplifier` / ...)。
- ✅ 通过 `tools_required` + `recommended_agents`(数据)注入。

### 3.3 不预设质量评分维度

- ❌ 不在 `ccteam-core` 写"6 维评分"的具体维度名。
- ✅ team 配置的 `critic_dimensions[]` 是数据;`anti-leniency` 规则按通用 schema
  实现("至少一维 < weak_threshold 触发 BLOCK")。

### 3.4 不预设危险命令清单

- ❌ 不在 `ccteam-core` 硬编码 `git push.*` / `rm -rf` 这种 matcher。
- ✅ `team.yaml.danger_command_patterns[]` 注入,hook matcher 由 settings.json
  渲染时按 team 注入。
- 论证:research 团队没有 git push 但有"未审就发邮件";marketing 团队的危险命
  令是"直接 publish to social"。

### 3.5 不替领域定 escalation 前缀语义

- ❌ 不在 escalation handler 里写死 `HYPOTHESIS_REJECTED → REVERT_TO_PHASE 02-hypothesis`。
- ✅ team 配置 `escalate_prefixes[]` 作为路由表;handler 是查表 mechanism。

### 3.6 不替领域定记忆字段

- ❌ 不在 `ccteam-core` 假设 retro 含 "tech stack" / "踩过的坑"。
- ✅ team 配置 `retro_schema[]` 决定字段;retro phase prompt 按 schema 字段生成段
  落,写入 `~/.claude/rules/ccteam-lessons-<team>.md` marked section(详见
  tech-design §3.7)。

---

## 4. 现状缺陷(指向 dev-coupling-audit.md)

详细审计报告 → **[`docs/dev-coupling-audit.md`](./dev-coupling-audit.md)**
(F1-F91+ findings;P0 = 0 剩余,P1 = 2 剩余 / F15 settings 危险命令 + F23 容器
bind-mount spike,P2 = 1 剩余 / F17 测试硬编码 phase 名,N/A = 2 / F14, F19,~
50 已修复)。

审计过程**没有发现**需要修订 §1 责任分界表或 §2 团队扩展契约的位置——所有发
现都能映射到现有分类。这是抽象切对的好信号。

---

## 5. Meta-Agent Pattern — 与 §3 拒绝清单的一致性

meta-agent 架构详见 **`tech-design.md §3.8 用户接口层`**(三层架构 + dispatch
protocol + dispatcher-not-worker preset)。本节只回答 charter 关心的问题:
**meta-agent pattern 是否违反 §3 显式拒绝清单?**

不违反:

- **不是"ccteam 在适配器进程里嵌 LLM"** —— meta-agent 是 ccteam-managed 长会
  话,跟项目 session 同等地位,**channel adapter 进程内仍然没有 LLM**
  (Symphony 反模式红线没动)
- **不替领域定 done criteria** —— meta-agent 把项目派给团队后,完成判定仍由该
  团队的 workflow 决定
- **不引入新的 LLM 编排层** —— meta-agent 是 ccteam 现有 long session 模式的一
  个实例,不是新概念

如果将来发现某个 meta-agent 行为约束**必须**靠 channel 适配器内嵌 LLM 才能实
现,那是信号:回头审视该约束是不是放错位置了——它该写进 meta-agent 的 role
prompt,而不是适配器代码。

---

## 6. 本文档维护纪律

1. **任何 PR 引入新机制前**,先确认它是 §1.x 中已分类项的补充还是引入新概念。
   引入新概念必须在 §1 加一行(注明🟢/🟡/🔴),否则无法 review。
2. **dev-coupling-audit.md 的发现可以反过来修订 §1 / §2**——发现某条机制的 dev
   假设比预想深,本文档要更新,不要硬塞 audit 节。
3. **本文档不超过 400 行**——超出说明在重复 tech-design / interfaces;砍重复内
   容,留指针。
4. **commit message 用英文,文档内容用中文**(沿袭仓库现状)。
