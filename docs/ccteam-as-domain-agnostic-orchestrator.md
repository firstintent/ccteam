# ccteam 作为领域无关编排层

> **本文档是团队抽象的永久 charter**。回答一个问题:**ccteam 表面上像"一个 dev
> 团队的工具",但核心机制实际与"做软件"领域无关——换一组团队内容,同一套机制
> 应该能跑研究团队、营销团队、运营团队**。
>
> **职责单一**:本文把"机制层 vs 领域层"的责任分界钉死,给出扩展新团队的契约,
> 并显式列出**机制层不应替领域决策**的边界。**不**重复 `tech-design.md` /
> `interfaces.md` 的架构与协议细节——所有架构决策(gateway daemon / HarnessAdapter
> 执行 / chat⇄project⇄session / progress.jsonl 唯一事实来源 / context 管理 / 三层
> 防御)沿用 tech-design.md。
>
> 阅读位置:`tech-design.md` / `interfaces.md` 之上的**抽象层 charter**——任何 PR
> 引入新机制前,先回本文的 §1 责任分界表问"这是机制层职责还是领域层职责",再决
> 定该写到哪里。

---

## 0. TL;DR

ccteam 的能力分两层,**两层都是领域无关的机制**,都靠团队内容填领域语义:

- **当前在跑的机制**:`ccteam start` 起的 **IM⇄session 路由网关 daemon**(不 tick、
  无 orchestrator 循环)+ `HarnessAdapter` 执行层(Claude=tmux send-keys + transcript
  + PreToolUse hook;Codex=app-server JSON-RPC)× `ProcessBackend`(tmux / inproc /
  remote)+ 核心三元 **chat ⇄ project ⇄ session**。任何团队的任何 vendor session 都
  走同一条 inbound→submit→outbound 链,不假设你做的是软件。
- **推后的自动编排机制(`ccteam-flow` 层)**:workflow.yaml 拓扑 + 文件系统事件触发
  + 状态机 + progress.jsonl 上的 7 类编排 event(progress.jsonl 文件本身是当前 gateway
  hook sink 就在写的 state SoT,见 §1.2)+ context 管理 + cost/stall 软告警 + ESCALATE
  语法外壳 + 防御层 PASS/CONCERN/BLOCK 三档汇总。`Orchestrator` / `ArtifactWatcher` 住
  在 `ccteam-flow` crate,**`ccteam start` 当前不构造它**——但它的 schema 与契约仍是
  charter 范围内的机制,本文照管。

**团队内容** = `.claude/agents/<role>.md` 内容、Critic 评分维度、推荐 plugin agent 清
单、artifact 文件命名、危险命令拦截清单——这些假设你在做软件,住团队侧。机制/领域
分界一旦切对,扩展非 dev 团队只需替换团队内容,不动 Rust 代码;本文 §1 责任分界表 /
§2 团队扩展契约就是这条分界的成文版。

---

## 1. 责任分界表

记号(跨当前 gateway/harness 路径与推后 `ccteam-flow` 编排路径通用):
- 🟢 **mechanism**:领域无关,代码必须不假设领域语义,可被任何团队复用。
- 🟡 **team-config**:领域特定,但在机制层用**配置 / 数据 / 字符串**承载——代码不
  写死,通过 workflow.yaml / `team.yaml` / 推荐清单注入。
- 🔴 **team**:领域特定,**只能在该团队的 agent markdown / hook 脚本**里实现,绝不
  进入 `ccteam-core`。

### 1.1 状态与协议

`<project>/.ccteam/state.json` 全部字段(详 interfaces.md)— 🟢 mechanism。论证:
项目身份、team / team_kind、parallelism、flex `sessions{}` 注册表都是物理事实或调度
事实,所有团队共用。字段随实现演进,charter 关心的不是具体字段名,而是"state.json
描述机制事实、不描述领域语义"这条不变量——具体 schema 以 interfaces.md 为权威。
chat ⇄ project ⇄ session 三元也属 🟢:chat = IM 入口、project = 已 init 的目录、
session = 可续上下文的 agent 会话,三者关系与团队领域无关。

### 1.2 progress.jsonl 事件类型

`<project>/.ccteam/progress.jsonl` 是**业务状态唯一事实来源**(🟢 mechanism 红线)。
当前 gateway 的 hook sink 把 Claude / Codex 事件归一后写入它;推后的 `ccteam-flow`
层在此之上再叠 7 类编排业务 event(`workflow_start` / `agent_spawn` / `agent_done` /
`artifact_received` / `gate_triggered` / `budget_exceeded` / `workflow_done`)+ 进程生
命周期 + Claude Code hook 透传 + `escalation` / `watcher_concern` / `watcher_block` /
`context_reset`。这些事件 schema 全部 🟢 mechanism(见 interfaces.md);**仅 `reason`
/ `role` 字段内容**是 🟡(团队语义,见 §1.6)。

### 1.3 workflow.yaml schema(`ccteam-flow` 拓扑入口)

> `WorkflowSpec` 住 `crates/ccteam-flow/src/workflow.rs`(**不**在 `ccteam-core`)。
> `ccteam init` 落一份 `.ccteam/workflow.yaml` scaffold;当前 gateway 不消费它,字段
> 归类仍属 charter 机制范围。

| 字段 | 归类 | 论证 |
|---|---|---|
| `WorkflowSpec::name` | 🟢 | 字段是 mechanism;取值是 🟡 |
| `WorkflowSpec::description` | 🟢 | 给 meta-agent / UI 看;内容是 🟡 |
| `WorkflowSpec::enabled` | 🟢 | 软开关 + 热加载;所有团队共用 |
| `WorkflowSpec::budget` | 🟢 | rolling cap;数字由 team / 项目注入 → 取值是 🟡 |
| `WorkflowSpec::agents{role: AgentSpec}`(IndexMap) | 🟢 | 字段 + 顺序语义是 mechanism;role 名是 🟡 |
| `AgentSpec::executor`(`claude` / `codex`) | 🟢 | HarnessAdapter 选择 |
| `AgentSpec::trigger`(`manual` / `schedule` / `gate` / `watch:<path>`)| 🟢 | 4 类触发语义全 mechanism;`watch:` 路径是 🟡 |
| `AgentSpec::parallelism: Option<u32>` | 🟢 | 通用并发上限;仅 `Watch` 触发有意义 |
| `AgentSpec::input` / `AgentSpec::output` | 🟢 | 字段名是 mechanism;路径是 🟡 |
| `AgentSpec::interval` | 🟢 | `schedule` 配套;数字是 🟡 |
| `AgentSpec::timeout` + `on_timeout`(`escalate` / `retry` / `skip`) | 🟢 | watchdog 三档语义全通用 |
| **prompt 内容字段** | **明令禁止** | 见 §1.4 workflow-as-data 红线 |

### 1.4 Workflow-as-data 红线

> **核心红线**:workflow.yaml 是**拓扑数据**——连线 + trigger 类型 + 并发上限 +
> 软开关 + budget,**绝不出现 prompt 字面量**。agent 行为 prompt 住
> `.claude/agents/<role>.md`(LLM-prompt content 层,**由 Claude Code 加载,不由
> ccteam 解析**)。ccteam **不读 prompt,不路由 prompt,不在状态机里做"如果 prompt
> 提到 X 就 Y"判断**——编排层是文件系统事件调度器,不是 NL 中间件。

**允许**(拓扑数据 + flow-level 控制):拓扑 IndexMap、trigger 类型、`enabled` /
`budget` / `parallelism` / `executor` / `input` / `output` / `timeout` + `on_timeout`
字段(各字段语义见 §1.3 表)。

**禁止**(LLM-content 层 → `.claude/agents/<role>.md`):agent 行为指令 / prompt 文字
/ role 描述长文本、决策树 / 状态机分支描述、"如果 X 就 Y"条件分支字面量。

**违反这条红线的 PR 应被拒收**——编排层解析 prompt 内容等于在进程内嵌 LLM,违反
§3 显式拒绝清单 + no-prompt-injection 红线(IM 路径同样守:agent 行为住
`.claude/agents/<role>.md`,gateway **不向 tmux pane 注入 system prompt**,
`/compact` `/new` `/clear` 完全透传)。`.claude/agents/<role>.md` 是 Claude Code
官方 subagent definition 路径,ccteam 只负责文件存在性校验(`ccteam doctor`),不负
责语义解析。新字段加入前先回本节问"这是拓扑数据还是 prompt 内容?"meta-agent
dispatch 时**不**解析 `<role>.md` 内容 — 只看拓扑 + 通过
`mcp__ccteam__workflow_spawn_agent(role=...)` 派单。

### 1.5 编排决策点(thin orchestrator,`ccteam-flow` 层)

> 当前 gateway daemon 不跑这些循环;归类作为机制契约由本文照管,具体阈值以
> tech-design.md 为权威。

| 决策点 | 归类 | 论证 |
|---|---|---|
| `WorkflowSpec::enabled` + 热加载 | 🟢 | mechanism 通用;workflow 实例软开关,所有团队共用 |
| `WorkflowSpec::budget` | 🟢 | rolling 24h cost cap + spawn cap;数字由 team / 项目配置 → 取值是 🟡 |
| `ArtifactWatcher` inotify/fsevents 触发 | 🟢 | 文件系统是控制平面(tech-design 红线);watch 路径是 🟡 |
| context reset 阈值 | 🟢 | claude 进程级事实;具体阈值见 tech-design |
| idle-aware 注入 | 🟢 | mechanism |
| cost ladder 阈值 | 🟢 | 默认值是 mechanism;数字可由 `~/.ccteam/config.yaml` 与 workflow.yaml `budget` 覆盖 |
| stall ladder | 🟢 | 同上 |

### 1.6 Hook 与文件路径

| 文件 / 路径 | 归类 | 论证 |
|---|---|---|
| `~/.ccteam/` 全局布局(home / config / run socket / im credentials / ledger) | 🟢 | 全局机制层布局;详 user-manual.md / interfaces.md |
| `<project>/.ccteam/state.json` | 🟢 | 项目元数据 mechanism |
| `<project>/.ccteam/{agents,skills}` + `.claude/agents/` | 🟢 | `ccteam init` 落的中立机制布局;`.ccteam/skills/.gitkeep` 预留项目自有 skill 扩展 |
| `<project>/.ccteam/chat/<bot>/turns.jsonl` | 🟢 | chat-mode 对话原文(ccteam-owned,不依赖 Anthropic 内部 `~/.claude/projects/`) |
| 项目内 spec / brief artifact(如 `spec.md`) | 🟡 | 字段名是 mechanism;research 团队等价物可能叫 `topic.md` / `brief.md`——应在 team 配置中声明 |
| `escalation.md` 等控制文件 | 🟢 | 通用控制文件 |
| artifact 自动收集列表 | 🟡 | 应"扫 `.ccteam/*.md` 自动列出",或来自 team 配置 |
| `block-push` hook | 🔴 | "git push"是 dev 团队的危险命令;research 团队的危险命令可能是"未经审阅就发用户邮件" |
| `security_reminder_hook.py`(plugin) | 🔴 | 完全 dev-specific |
| project CLAUDE.md "不要 git push / 测试不过不算完成" | 🔴 | dev 团队 CLAUDE.md 模板;research 团队需另一份 |

### 1.7 ESCALATE grammar(`ccteam-flow` 层语法外壳)

| 语法档 | 归类 | 论证 |
|---|---|---|
| `ESCALATE: REVERT_TO_PHASE <name> — <reason>` | 🟢 | mechanism;`<name>` 取值是 🟡 |
| `ESCALATE: NEED_USER_INPUT — <questions>` | 🟢 | 用户在回路,所有团队共用 |
| `ESCALATE: ABORT — <reason>` | 🟢 | 永久标 failed |
| 团队特化前缀(如 `HYPOTHESIS_REJECTED`) | 🟡 | mechanism 是"team 可注册自己的前缀";前缀本身是团队语义 |
| 无前缀时降级为 `NEED_USER_INPUT` | 🟢 | mechanism |

### 1.8 防御层(L1 / L2 / L3)与 watcher(`ccteam-flow` 层)

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

当前用户面命令(详 user-manual.md)是机制,与团队领域无关:

| 命令 | 归类 | 论证 |
|---|---|---|
| `ccteam init` / `start` / `stop` / `status` / `web` | 🟢 | gateway + 项目初始化机制,所有团队共用 |
| `ccteam doctor`(`--verify-mcp` / `--check-cost-orphan`)| 🟢 | 校验 mechanism;校验对象是 🟡 |
| 推荐 plugin agent 安装**清单** | 🟡 | dev 团队推荐若干 plugin agent;research 团队需另一份 |
| `ccteam-flow` 派工 / progress 等命令 | 🟢 | 推后编排层 CLI;归类同上,多一个 `--team=<name>` 维度即可泛化(详 §3) |

### 1.10 跨项目记忆

> 不自建索引/向量库,完全复用 Claude Code 官方机制。详见 `docs/tech-design.md`。

| 概念 | 归类 | 论证 |
|---|---|---|
| per-repo auto-memory(官方 `~/.claude/projects/<encoded>/memory/`) | 🟢 | 官方机制,Claude 自主写;通用,与 ccteam-core 解耦 |
| `~/.claude/rules/ccteam-lessons-<team>.md` 跨项目共享 | 🟢 | 通用机制(rules + `paths:` frontmatter scope `~/projects/<team>-*`,slug 强制 team 前缀,scoping 实际生效)|
| `team.yaml.retro_schema[]` 字段定义 | 🟡 | dev 已填若干字段,research 须自填;mechanism 是"按 team 定义 retro 字段段落" |
| 召回触发 | 🟢 | 官方机制自动注入 rules + 可选 `mcp__*claude-mem*search`,LLM 自看 tool surface 决定 |
| 跨团队记忆隔离 vs 共享 | 🟢 | rules 文件按 team 分名 + `paths:` frontmatter 按项目目录前缀 scope 隔离 |

---

## 2. 团队扩展契约

要在 ccteam 上跑一支新团队,必须交付以下内容。少一件,自动编排层拒绝构建拓扑
(`ccteam doctor` 校验失败 → fail-fast)。

### 2.1 Workflow 拓扑(`workflow.yaml` 是数据驱动入口)

新团队交付:

1. **`team.yaml::workflows[]`** — 声明本团队可用的 workflow 实例(team-level
   registry,`~/.ccteam/teams/<team>.yaml`)。每条引用一个 workflow.yaml 模板路径 +
   描述。
2. **项目实例 `<project>/.ccteam/workflow.yaml`** — 由 `ccteam init` 或
   `ccteam-creator` skill 生成,包含具体 agent 拓扑 + trigger + parallelism +
   budget。完整 schema 见 §1.3;字段红线见 §1.4。
3. **`.claude/agents/<role>.md`** — 每个 workflow 引用的 role,在 `.claude/agents/`
   下有同名 markdown(Claude Code 官方 subagent definition 路径)。**这是 agent 行为
   SoT**(prompt + tools + role description),由用户 / `ccteam-creator` 手写,
   **ccteam 不读、不解析、不修改**。

**workflow.yaml 骨架示例**(与 `crates/ccteam-flow/src/workflow.rs::WorkflowSpec`
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
    trigger: gate         # 等 explorer 产出后 trigger_gate MCP 放行
    input: .ccteam/fix-requests/
    output: .ccteam/fixes/
```

约定:
- **agents 是 IndexMap**(YAML 声明顺序保留)→ trigger 图按声明顺序 deterministic build
- **至少一个 agent** 必须有非空 trigger;否则校验 fail
- **role 名** 必须与 `.claude/agents/<role>.md` 文件名匹配(构建期 + spawn 前各校验
  一次;缺文件 → fail-fast)
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

构建期枚举三类来源做交叉比对,缺谁报缺谁 + 给出修复命令。**未声明的工具调用** =
silent fail。

### 2.3 Critic 维度定义(替代 dev 多维 Score)

每团队提供 `team.yaml.critic_dimensions`:每条维度 = `name` + `weight` +
`weak_threshold`(任一维度 ≤ 此值 → 自动 BLOCK)+ `anti_leniency_strictness`
(`lenient` / `normal` / `strict`)+ `rubric`(0-1 评分指南)。dev 团队的维度
(Functionality / Quality / Tests / UX / Speed / Docs)只是这套 schema 的一个具体实例。

**三条 invariant**:

1. **`critic_dimensions[]` 是数据,不是 Rust enum** — `crates/ccteam-core/` 不得出现
   dev 维度字面量;`team.yaml` 加载到 `Vec<CriticDimensionConfig>`,维度名 / 权重 / 阈
   值全从配置读。
2. **`anti_leniency_strictness` 是 per-dimension 元数据** — dev critic 有测试退出码兜
   底故普遍 `normal`;research critic 是纯 LLM 主观判断,核心维度需 `strict`。
3. **`weak_threshold` 由配置控制,不是常量** — `score.rs` 不得硬编码,从
   `CriticDimensionConfig.weak_threshold` 读。

### 2.4 ESCALATE grammar 扩展

每团队声明 `team.yaml.escalate_prefixes[]` — 每条 = `prefix` + `route`
(`REVERT_TO_PHASE` / `NEED_USER_INPUT` / `ABORT`)+ `target_phase` + `reason`。
例:research 团队的 `HYPOTHESIS_REJECTED → REVERT_TO_PHASE 02-hypothesis`、
`SOURCE_UNAVAILABLE → NEED_USER_INPUT`。前缀本身是数据,分发逻辑是 mechanism。

### 2.5 推荐 plugin / agent 安装清单

每团队提供 `team.yaml.recommended_agents`;**首要交付是 `.claude/agents/<role>.md`**
(workflow.yaml 引用的 role 必须各有同名 md);`recommended_agents` 字段作 hint 用,
不强制。

清单要素:
- agent 来源(`claude-plugins-official:<plugin>/agents/<name>` / 自带脚本路径)
- 默认挂载 role(信息性,不强制)
- 一句话说明何时该被调用(给 LLM 决策时读)

### 2.6 与跨项目记忆的对接方式

每团队声明:
- **retro 输出 schema**:patterns 文件该有哪些字段(dev: tech stack / 踩过的坑;
  research: 数据源 / 假设结果 / 方法学反思;...)。
- **召回 namespace 策略**:`team_only`(默认,跨同 team 召回)/ `cross_team`(允许跨
  team 召回——anti-pattern 跨域有意义,成功 pattern 不一定可迁移)。
- **anti-pattern 全局共享**:REJECT/ABORT 案例对所有团队都召回——避免重复犯错。

### 2.7 团队配置文件骨架

`~/.ccteam/teams/<team-name>.yaml` 顶级字段:`name` / `workflows[]`(指向
workflow.yaml 模板)/ `critic_dimensions[]`(§2.3)/ `escalate_prefixes[]`(§2.4)/
`recommended_agents` / `recommended_skills` / `recommended_mcp`(§2.5)/
`retro_schema[]`(§2.6)/ `artifacts{spec, primary_data_dir, final_report}` /
`danger_command_patterns[]`。完整 schema 见 interfaces.md;现有 reference impl
`teams/dev.yaml` + `teams/product-research.yaml`。跨项目记忆走官方 `~/.claude/rules/`
+ per-repo auto-memory,无 `memory_namespace` 字段(§1.10 / §2.6)。

---

## 3. 机制层不应承担的职责(显式拒绝清单)

防止后续 PR 在"为了通用"的名义下把领域决策悄悄塞进 `ccteam-core`。**违反这条清单
的 PR 应被拒收**——领域决策不是机制层的扩展点。

### 3.1 不替领域定 done criteria

- ❌ 不在 `ccteam-core` 写"如果测试全绿 + critic PASS 则 done"。
- ✅ 通过 workflow 终态 + role.md 内的可执行验证表达。
- 例外:**进程级别的 done**(claude 主动退出 / SessionEnd)是 mechanism,不在此限。

### 3.2 不替领域选 plugin

- ❌ 不在 `ccteam-core` import / 调用具体 plugin 名(`pr-review-toolkit` / `code-simplifier` / ...)。
- ✅ 通过 `tools_required` + `recommended_agents`(数据)注入。

### 3.3 不预设质量评分维度

- ❌ 不在 `ccteam-core` 写具体维度名。
- ✅ team 配置的 `critic_dimensions[]` 是数据;`anti-leniency` 规则按通用 schema 实现
  ("至少一维 < weak_threshold 触发 BLOCK")。

### 3.4 不预设危险命令清单

- ❌ 不在 `ccteam-core` 硬编码 `git push.*` / `rm -rf` 这种 matcher。
- ✅ `team.yaml.danger_command_patterns[]` 注入,hook matcher 由 settings.json 渲染时
  按 team 注入。
- 论证:research 团队没有 git push 但有"未审就发邮件";marketing 团队的危险命令是
  "直接 publish to social"。

### 3.5 不替领域定 escalation 前缀语义

- ❌ 不在 escalation handler 里写死 `HYPOTHESIS_REJECTED → REVERT_TO_PHASE 02-hypothesis`。
- ✅ team 配置 `escalate_prefixes[]` 作为路由表;handler 是查表 mechanism。

### 3.6 不替领域定记忆字段

- ❌ 不在 `ccteam-core` 假设 retro 含 "tech stack" / "踩过的坑"。
- ✅ team 配置 `retro_schema[]` 决定字段;retro phase prompt 按 schema 字段生成段落,
  写入 `~/.claude/rules/ccteam-lessons-<team>.md` marked section(详见 tech-design.md)。

### 3.7 不向 session 注入领域 prompt

- ❌ gateway / 编排层不向 tmux pane 或 app-server 注入 agent 行为 / system prompt。
- ✅ agent 行为住 `.claude/agents/<role>.md`,由 Claude Code 官方机制加载;ccteam 只路
  由用户原文消息,`/compact` `/new` `/clear` 完全透传(no-prompt-injection 跨 mode 红
  线,chat / IM 路径同样守)。

---

## 4. 现状缺陷

dev 耦合的逐条审计 → **[`docs/dev-coupling-audit.md`](./dev-coupling-audit.md)**。审计
过程**没有发现**需要修订 §1 责任分界表或 §2 团队扩展契约的位置——所有发现都能映射
到现有分类。这是抽象切对的好信号。

---

## 5. Meta-Agent Pattern — 与 §3 拒绝清单的一致性

meta-agent 架构详见 **`tech-design.md`**(三层架构 + dispatch protocol +
dispatcher-not-worker preset)。本节只回答 charter 关心的问题:**meta-agent pattern
是否违反 §3 显式拒绝清单?**

不违反:

- **不是"ccteam 在进程内嵌 LLM"** —— meta-agent 是 ccteam-managed 长会话,跟项目
  session 同等地位,**gateway / channel adapter 进程内仍然没有 LLM**(no-prompt-injection
  红线没动)
- **不替领域定 done criteria** —— meta-agent 把项目派给团队后,完成判定仍由该团队的
  workflow 决定
- **不引入新的 LLM 编排层** —— meta-agent 是 ccteam 现有 long session / chat 模式的一
  个实例,不是新概念

如果将来发现某个 meta-agent 行为约束**必须**靠适配器内嵌 LLM 才能实现,那是信号:回
头审视该约束是不是放错位置了——它该写进 meta-agent 的 role prompt,而不是适配器代码。

---

## 6. 本文档维护纪律

1. **任何 PR 引入新机制前**,先确认它是 §1.x 中已分类项的补充还是引入新概念。引入新
   概念必须在 §1 加一行(注明🟢/🟡/🔴),否则无法 review。
2. **dev-coupling-audit.md 的发现可以反过来修订 §1 / §2**——发现某条机制的 dev 假设比
   预想深,本文档要更新,不要硬塞 audit 节。
3. **本文档不超过 400 行**——超出说明在重复 tech-design / interfaces;砍重复内容,留
   指针。
4. **commit message 用英文,文档内容用中文**(沿袭仓库现状)。
