---
audience: contributors
---

# 编排模式 —— 拆分哲学 + 5 模式目录(ccteam-flow 设计依据)

> ⚠️ **本文是 `ccteam-flow` 编排层(推后,未接入当前运行态)的模式设计。**
> 当前产品是 **IM⇄session 路由网关 daemon**(`ccteam start`):它**不跑编排 tick、无 orchestrator 循环**,由用户在 IM 里手动驱动多个 session(见 [tech-design.md](tech-design.md) §2 / [user-manual.md](user-manual.md))。
> 自动多 agent 编排住在独立 crate `ccteam-flow`,**已构建但未接进运行中的 gateway daemon**(见 [tech-design.md](tech-design.md) §7)。本文记录该编排层的模式设计与红线,供其落地时不退基线 —— **不要**把下文当成当前 daemon 的运行方式。

> **角色**:tier-1 全局文档,**面向 contributor**(ccteam 维护者 / 新 workflow 设计者)。日常**用户**用 ccteam 不需要读本文 —— 用户面入口是 [task-to-command.md](task-to-command.md)(决策树)+ [quickstart.md](quickstart.md) + [user-manual.md](user-manual.md)。
>
> 编排层点亮后,所有 workflow 设计、agent 拓扑、自动化迭代**都基于本文的 5 模式 + 拆分哲学**;新加 workflow 模板 / 重做 fix-loop / 拓展非编程领域 team 都先回到本文校准。
>
> **回答两个核心问题**:
> 1. **什么时候该拆 agent?**(§一 设计哲学 — "按上下文拆,不按角色拆")
> 2. **拆完了怎么编排?**(§二 5 种 canonical 模式)
>
> **资料来源**:
> - Anthropic Engineering Blog *"Building Effective Agents"*(canonical 5-pattern taxonomy)
> - 大模型大鱼《如何设计 Multi-Agent》+《超有用的 5 种编排模式》短文
> - 配套研究:`research/omc-orchestration-modes.md`(OMC 8 mode 深拆)+ `research/omc-vs-ccteam-orchestration.md`(prompt-as-orchestrator vs code-as-orchestrator 路线对比)
>
> **跟其他全局文档的关系**:
> - `requirements.md` 给"用户痛点 + 终极目标"
> - **本文给"模式选型字典"** — 解决"该痛点应该用哪种编排模式"
> - `tech-design.md` §7 给编排层承载形态 = `ccteam-flow::Orchestrator` + `ArtifactWatcher` + `WorkflowSpec`(workflow.yaml)—— **推后**,不接进当前运行态
> - `interfaces.md` 给"workflow.yaml schema / Trigger / parallelism" 等具体协议
>
> 编排层落地、加新 team / 新 workflow 时:**先回本文识别走哪条模式**,再到 tech-design §7 / interfaces 找具体怎么写。

---

## 一、设计哲学:按上下文拆,不按角色拆

### 1.1 反模式:角色驱动的拆分

最常见的多 agent 设计直觉是按"职能角色"分:planner / executor / tester / reviewer ……。听起来分工明确,实际制造的是**信息逐层衰减**:

- planner 想清楚的 trade-off,executor 拿到只剩"做 X"指令,丢了"为啥不做 Y"
- executor 调试时积累的 dead-end 信息,tester 看不到,可能重复探
- 每一次 agent 间交接都是 prompt 重新 brief 一遍 context,**重叠信息变成 token 税**

### 1.2 推荐模式:上下文边界驱动

**核心原则**:

> 如果两个子任务有重叠的信息,它们就应该交给同一个 Agent。
> 只有当上下文能真正隔离、接口清晰的时候,才值得拆开。

经典反例(来自原文):**Vibe Coding 里"写功能 + 写测试"应该一个 agent 干完**。因为 testing 需要 implementation context — 拆给两个 agent,反而制造交接成本。

### 1.3 上下文边界判定 checklist

实操时,在拆 agent 之前先过一遍下面的信号:

| 信号 | 倾向 |
|---|---|
| 两个子任务读相同 spec / artifact 子集 | **不拆** |
| 子任务 A 的全部输出是子任务 B 的全部输入 | **不拆**(同 agent 内顺序做) |
| 子任务间只交换"结果摘要"(不交换中间状态) | 可拆 |
| 子任务用完全不相干的工具集(写代码 vs UI 设计 vs 检索文献) | 倾向拆,但**先**判断信息重叠 |
| 子任务需要不同 provider / model tier(Haiku 判别 vs Opus 设计) | 倾向拆(物理隔离) |
| 子任务并发跑能省真实墙钟时间 | 倾向拆(性能驱动) |
| 子任务里有"独立可验证"的产出(test pass / lint clean / artifact 文件) | 倾向拆(便于 evaluator-optimizer 模式) |

### 1.4 为什么这条在 Claude Code 上下文里特别重要

- Claude Code 默认 1M context,**单 agent 装得下的边界比想象大**
- phase 边界 reset / context compact 会丢决策依据 — OMC 的 `.omc/handoffs/<stage>.md` 是为这个痛点贴的创可贴;**一开始按"重叠原则"少拆,handoff 本身就少**
- Subagent / Task() 调用本身有启动成本(prompt 重新 brief),拆得越细,这部分越烧
- 编排层架构红线:文件系统是控制平面、`progress.jsonl` SoT —— 拆 agent 就要新增 trigger / artifact / event,**拆是有成本的,不要 default 拆**

### 1.5 大型代码库:`scope` 切口 + explorer→artifact→editor

**分层认知**:ccteam 是 outer harness,底层 Claude Code / Codex 是 inner harness。Anthropic《How Claude Code works in large codebases》整篇讲的是 **inner harness**(CLAUDE.md / hooks / skills / plugins / LSP / within-session subagent)—— 那是项目仓库 + Claude Code 自己的职责,**编排层不重复造**。编排层不可替代的价值,是 inner harness 结构上看不见的东西:**拓扑**。

**那篇文章对 ccteam 的唯一真空白**:文章压在一句 —— "Claude 的能力 = 找到正确 context 的能力;太多则退化,太少则盲目"。inner harness 管的是单 agent 窗口内的 context;编排层管的是**跨 agent 的 context —— 靠切口**。红线 R4"每次 spawn = fresh 1M context"给的是干净窗口,但 **fresh ≠ scoped**:一个干净的 1M 窗口对着百万行的仓库根,光"找路"就烧穿预算。R4 给干净窗口,scoping 给一个小的东西去看 —— 编排层需要把两者都给齐。

落地是**一条代码 + 一个模板**:

1. **`scope` 切口(代码)** —— `AgentSpec.scope`(`docs/interfaces.md` §17.2)把每次 spawn 的 cwd 钉到与该 role 相关的子树。这是**纯拓扑决策**:inner harness 只看见自己一个 session,结构上做不到;只有 spawn 多 agent 的编排层能给每个 agent 定切口。Claude Code 仍自动向上 walk 目录树、加载沿途 `CLAUDE.md`,root context 不丢。

2. **explorer→artifact→editor(模板)** —— 文章把 subagent 列为对抗 context 约束的核武器:"read-only subagent 画子系统地图、findings 落文件,主 agent 拿全图再编辑"。**这正是编排层的 artifact-driven 拓扑**:explorer role(read-only、宽 scope)→ 写 codebase-map artifact → editor role(窄 scope)trigger 在该 artifact 上消费。编排层天生就是这条 subagent 建议的多-agent 泛化版。大代码库的 workflow 默认就该是这个切分,而**不是一个胖 agent 端到端**。

```yaml
# 大代码库模板:explorer 画图 → editor 按图改(各锁 scope)
name: large-codebase
agents:
  explorer:                       # read-only 调研,产出子系统地图
    trigger: manual
    scope: services/payments      # cwd 锁这一子树(仍 walk-up 加载 root CLAUDE.md)
    output: .ccteam/maps          # 地图 artifact 落这里
  editor:                         # 拿地图,窄 scope 落编辑
    trigger: watch:.ccteam/maps   # explorer 写完地图即触发
    scope: services/payments
    input: .ccteam/maps
```

**克制(不吸收的)**:文章关于 CLAUDE.md 内容生成 / LSP / `permissions.deny` / 结构化搜索 MCP 的建议,编排层一律不吸收 —— 那些是 inner harness / 项目仓库的职责;代笔 `CLAUDE.md` 内容还会撞"no prompt injection"红线。编排层顶多在 `ccteam doctor` 里**提示**缺失,不**拥有**。

---

## 二、五种编排模式 —— 在 Claude Code 上的具体形态

下表 5 种模式来自 Anthropic "Building Effective Agents" canonical taxonomy(也是上文短文 §2 列的 5 种)。本节把每种模式映射到 **Claude Code 原语 + OMC 落地 + 编排层(ccteam-flow)落地**。

### 2.1 Prompt Chaining(链式调用)

- **定义**:任务按顺序执行,每一步输出 = 下一步输入。适合有严格先后依赖、每步都是 quality gate 的流程。
- **Claude Code 原语**:
  - 单 session 内连续指令(最朴素)
  - 主 agent 在 conversation turn 里依次 spawn `Task(subagent_type=...)` —— 串行,等结果再 spawn 下一个
  - 文件 / artifact 接力:agent B 的 trigger 是 agent A 写的 artifact
- **OMC 落地**:`team-plan → team-prd → team-exec → team-verify → team-fix` 是 staged chaining;每 stage 间写 `.omc/handoffs/<stage>.md`(10-20 行,decisions/rejected/risks/files/remaining)避免 context compact 丢决策
- **编排层落地**:`workflow.yaml` 中 `trigger: watch:<path>` 把 agent 编织成有向边;`ccteam-flow::Orchestrator` 按 artifact 顺序 spawn 后继 agent
- **何时用**:有严格 quality gate(plan 不过不能 exec)、产出可序列化为文件(plan.md / spec.md)、不需要并发

### 2.2 Routing(意图路由)

- **定义**:用一个判别器判断任务该交给谁。简单任务给便宜快速的模型,复杂任务给能力强的。
- **Claude Code 原语**:
  - 主 agent 在 conversation turn 内 LLM 推理选 `subagent_type`(最常见,但**不决定论**)
  - Skill 注入的 routing 表(SKILL.md 写死 "符合特征 X → 调 agent Y")
  - Hook 拦截 prompt 关键词,自动 prepend skill / slash command
- **OMC 落地**:
  - `stage-router.ts`(任务特征 + 风险 → role:planner / executor / architect / critic)
  - `role-router.ts`(role + cost mode + provider availability → 具体 provider/model tier)
  - **resolved routing snapshot** 模式:`TeamCreate` 时一次性解析并冻结,运行时不重读 yaml — 保证 stickiness
  - Cost 降级:opus → sonnet → haiku;`team-verify` 强约束 ≥ sonnet
- **编排层落地**:`workflow.yaml` 的 `agents.<name>.role` 是**静态路由**(写死);动态路由由 meta-agent 在对话里临时调度承载
- **何时用**:任务复杂度悬殊大、有不同特长 agent、想分级别省钱

### 2.3 Parallelization(并行化)

两个子模式,Claude Code 上的实现路径不同:

#### 2.3.1 投票(同任务多次跑取最优)

- **Claude Code 原语**:主 agent 同一 turn 内多个 `Task()` call 并发 spawn,prompt 相同(或微扰),结果合成
- **OMC 落地**:`ccg`(Claude-Codex-Gemini tri-model advisor)是这个的极致 — 把同一问题拆成 codex 侧重 + gemini 侧重两个 advisor prompt,并行调,Claude 合成"双方同意 / 冲突 / 选谁 + 为啥"
- **编排层落地**:**待补语法** —— `workflow.yaml` 当前 schema 表达不了"同 prompt fork N 次然后合成",这是编排层的设计目标(见 §五 fan-out)
- **何时用**:质量优先、单 LLM 一次生成不靠谱、需要"多视角"保险

#### 2.3.2 分段(独立子任务同时推)

- **Claude Code 原语**:主 agent 同一 turn 内多个 `Task()` call 处理不同子任务,**真独立**(无共享 mutable 状态)
- **OMC 落地**:
  - `ultrawork`(并行执行引擎,非独立 mode,被 ralph / autopilot 嵌套调用)— "fire N 个 Task() 不等"
  - `git-worktree.ts` 给每个 worker 独立 worktree(`omc-team/<team>/<worker>` 分支),避免共享文件树冲突
- **编排层落地**:`workflow.yaml` `parallelism: N` 字段;`ccteam-flow::Orchestrator` 按 artifact event 触发独立 agent
- **何时用**:子任务真独立、墙钟时间是瓶颈、文件系统冲突可隔离(worktree)

> **共同陷阱**:LLM 经常把可并行的任务串行跑(spawn 一个等结果再 spawn 下一个)。这是反模式 — 必须显式在同一 turn 多 Task() 调用。OMC 的 ultrawork SKILL.md 反复强调这点。

### 2.4 Orchestrator-Worker(编排者-执行者)

- **定义**:一个主 Agent 负责拆任务、派活、收结果。**最主流的架构**,Sub-agents 和 Agent Teams 在生产环境的默认形态。
- **Claude Code 原语**:`TeamCreate` + `TaskCreate` × N + `Task(subagent_type, team_name, name)` × N + `SendMessage` + `TaskList` 轮询(Claude Code native team tools)
- **两条实现路线**(深度对比见 `omc-vs-ccteam-orchestration.md`):
  - **prompt-as-orchestrator(LLM-driven)**:主 agent 自己读 SKILL.md 当剧本,LLM 推理控制流。OMC 路线。强:灵活、低实施成本、单 chat 内可看可改。弱:每次决策烧 lead session token、非决定论、context compact 后丢状态
  - **code-as-orchestrator(deterministic)**:外置进程(Rust binary 等)读 workflow.yaml + watch artifact,**LLM 不在控制平面**。编排层路线。强:决定论、可测、长跑无状态、低 token。弱:用户得先写 workflow.yaml、灵活度低
- **OMC 落地**:`skills/team/SKILL.md` 1040 行 + `src/team/` 20 kLOC substrate;control flow 100% 在 SKILL.md(LLM 在 conversation turn 里推理)
- **编排层落地**:`crates/ccteam-flow` Rust orchestrator + `workflow.yaml` 声明 + `progress.jsonl` SoT;control flow 100% 在 Rust 代码
- **何时用**:任务大、子任务多、要长跑(选 code-as-orchestrator 路线)/ 要快速迭代多 provider hybrid(选 OMC 路线)
- **squad 路由(artifact-driven 模式的细化,非新 mode)**:workflow.yaml 顶层 `squad: { leader, members, hop_limit }` 块让 orchestrator-worker 的 worker 选择从**静态接线**(只能写死的 `output:` 目录)扩到**运行时 dispatch**(leader 写 `<member>--*.md` 进 `.ccteam/squad/`,ArtifactWatcher 按文件名前缀 spawn 对应 member)。同一条 artifact-as-control-plane 红线 + 同样的 code-as-orchestrator 形态;`members:` 静态声明保证拓扑可审计、不开 prompt-injection 面;`hop_limit`(默认 3)按 R7 红线发 `escalation` 事件兜底 routing 回路。**不是第 6 种 mode** —— 是 Orchestrator-Worker 在编排层上的「动态 dispatch」分支。

### 2.5 Evaluator-Optimizer(生成-评估循环)

- **定义**:一个 Agent 生成,另一个 Agent 评估打分,循环迭代直到达标。适合质量优先、一次生成不靠谱的场景。
- **Claude Code 原语**:
  - `Task(subagent_type=reviewer)` + 循环条件(SKILL.md 写 "verify 失败 → fix → 再 verify")
  - Hook 注入"don't politely stop" 提醒(OMC `ralph` 的 boulder-never-stops 机制)
- **OMC 落地**:
  - `team-verify → team-fix → team-verify` loop(`max_fix_loops: 3`)
  - `autopilot` Phase 4:三个独立 reviewer 并行(architect / security-reviewer / code-reviewer),**全 approve 才算过**
  - `ralph` reviewer verification(默认 architect,可 `--critic=critic` 或 `--critic=codex`)+ 强制 `ai-slop-cleaner` deslop pass + post-deslop regression test
- **编排层落地**:`workflow.yaml` `budget.fix_loop_attempts` + `escalation` event;`ccteam-flow::Orchestrator` 物理执行 cap(LLM 不能绕过)
- **关键架构红线**(CLAUDE.md §三):**fix-loop 撞 3 次顶必 escalate,绝不静默重置**。Evaluator-Optimizer 没有 hard cap 就是死循环烧钱机
- **次要风险:"polite-stop anti-pattern"**:OMC ralph SKILL.md 反复警告 lead 在 reviewer APPROVED 后不要"礼貌停手";Claude 有"任务完成就停"的强 prior。**hook + system reminder 是技术抗体**,prompt 自律不够
- **何时用**:质量优先、人工 review 太贵、有可机器评估的标准(test pass / lint clean / build green)

---

## 三、Claude Code 原语速查表

| 模式 | Claude Code native 原语 | OMC 落地 | 编排层(ccteam-flow)落地 |
|---|---|---|---|
| Chaining | `Task` subagent 串行 + artifact 接力 | `team-plan → team-prd → ...` + handoffs.md | `workflow.yaml` artifact 边 |
| Routing | LLM 推理 `subagent_type` / Skill 内置路由表 | `role-router` + `stage-router` + resolved snapshot | meta-agent 选 role(静态) |
| Parallel (vote) | 多 `Task` 同 turn fork 同 prompt | `ccg`(codex + gemini + Claude 合成) | **待补语法**(`fan_out: { count, merge: vote }`) |
| Parallel (segment) | 多 `Task` 同 turn 不同 prompt + worktree 隔离 | `ultrawork` + `git-worktree.ts` | `workflow.yaml` `parallelism: N` |
| Orchestrator-Worker | `TeamCreate` + `TaskCreate` × N + `Task(team_name, name)` × N | `skills/team/SKILL.md` 1040 行(prompt-as-orchestrator) | `ccteam-flow` orchestrator + `workflow.yaml`(code-as-orchestrator) |
| Evaluator-Optimizer | `Task(reviewer)` + 循环 + hook 抗 polite-stop | `team-verify/fix` loop + `ralph` PRD reviewer + `autopilot` Phase 4 三 reviewer 并行 | `budget.fix_loop_attempts` + `escalation` event(Rust hard cap) |

---

## 四、Composability —— 模式之间的拼装规则

5 个模式不是平铺并列,真实生产形态是组合的。OMC 的杀手锏正是 composability,有三种拼法:

### 4.1 嵌套(模式包模式)

```
autopilot (lifecycle, 整体 = Orchestrator-Worker)
  └─ Phase 2 = ralph (Evaluator-Optimizer + Persistence)
                └─ each story = ultrawork (Parallel segment)
                                  └─ Task() × N
  └─ Phase 4 = Parallel vote(architect + security-reviewer + code-reviewer 并发 review)
```

每层选不同模式 — 外层管 lifecycle,中层管 quality loop,内层管并发执行。

### 4.2 顺序管线 + artifact 接力

```
/deep-interview "vague idea"
   └─ writes .omc/specs/deep-interview-*.md
       ↓
/ralplan --direct
   └─ Planner→Architect→Critic loop, writes .omc/plans/ralplan-*.md
       ↓
/autopilot
   └─ detects existing ralplan → SKIP Phase 0+1 → directly Phase 2
```

**关键设计**:每个 mode 入口**先检测前置 artifact 是否存在**,存在就跳过自己内部对应的 phase。给用户**渐进式投入**体验 — 既能一句话 `/autopilot`,也能走完整三段管线拿更高质量。

### 4.3 横切修饰符

`/team ralph "task"` — 一行命令复合两个模式:

- 内圈:`team` chaining(plan/prd/exec/verify/fix)
- 外圈:`ralph` evaluator-optimizer(reviewer + 迭代重启 `max_iterations`)
- state 互相 cross-reference(`linked_ralph: true` / `linked_team: true`)
- cancel 级联(cancel 一个清两个)

### 4.4 反面教材:统一 cancel 的 state proliferation

OMC `skills/cancel/SKILL.md` **387 行**管所有 mode 状态清理 — 每加一个 mode,cancel 涨一段。这是**复杂度税**:模式越多、composition 越自由,cancel/resume 路径越爆炸。

编排层的对策(架构红线):**所有 state 进 `progress.jsonl` 一种文件**,cancel 只动一种东西。代价:模式表达力受限,**用一致性换可维护性**。

---

## 五、编排层如何表达 5 模式 + 设计目标

编排层通过 `workflow.yaml` + Trigger 4 类 + parallelism 字段表达 5 模式如下;**未补齐的语法即编排层的设计目标**:

| 模式 | 编排层如何表达 | 设计目标(待补语法) |
|---|---|---|
| **Chaining** | `Trigger::Watch(<dir>)` 接力 + artifact 文件 = 串行管道 | ✅ 充分,无需改 |
| **Routing** | 静态:`workflow.yaml` 写死 role;运行时 dispatch:`squad: { leader, members, hop_limit }` —— leader 运行时挑 member,membership 仍静态;动态 spawn:meta-agent + MCP `spawn_agent` 临时调度 | **仍缺 LLM-driven router sugar**:`agent.router: <expr>` 让 orchestrator 解析后 LLM 推理选 agent(squad 是声明集合内的 leader dispatch,不替代任意 expr 路由) |
| **Parallelization (segment)** | `agent.parallelism: u32` + `Trigger::Watch(<dir>)` fan-out = 多文件并行 | ✅ 充分(每个新 artifact 触发独立 session) |
| **Parallelization (vote)** | **当前 schema 未表达** | 加 `agent.fan_out: { count: 3, merge: vote\|best\|concat }` 语法 — 同 prompt fork N 个,merge 策略由 orchestrator 实现 |
| **Orchestrator-Worker** | meta-agent + `mcp__ccteam__*` 工具 = orchestrator 层;workflow.yaml 各 agent = worker 层 | ✅ 充分,这是编排层主流形态 |
| **Evaluator-Optimizer** | `fix_counts` 3-strike → escalation 是隐含的 evaluator-optimizer | **缺显式 sugar**:加 `agent.evaluates: <target>` + `max_iterations: N` + `on_max_exceeded: escalate\|accept` 让 reviewer 多轮可声明 |

**两条横向设计方向**:

1. **"按上下文拆"反推 `ccteam-creator` skill dialogue** — 用户写 workflow.yaml 容易按 role 直觉拆(planner / builder / tester / reviewer)。`ccteam-creator` skill 应在 dialogue 中强制走 §1.3 checklist:"两个 agent 间有多少信息重叠?"重叠 >50% 就合并 — 默认行为应是 monolithic agent + subagent 内 ad-hoc 拆,不是 workflow.yaml 切 N 个 role。

2. **Composability**(编排层最弱) — 当前必须重写整个 workflow.yaml 才能复合两个模式。设计 `workflow.yaml::extends: <path>` + override 语法,让常见 composition(ralph-shell / vote-merger / evaluator-loop)模板化。OMC 的 `/team ralph` 一行命令复合两个模式是参考形态。

**架构红线**(本文 §1.4 + §4.4 引申):
- 5 模式可叠加但 state 必须收敛 — 编排层所有 state 进 `progress.jsonl`,**绝不**为新模式开新 state 文件(OMC `cancel/SKILL.md` 387 行膨胀是反面教材)
- Evaluator-Optimizer 必须 hard cap — `fix_counts` 3-strike escalation 是架构红线,任何新增 evaluator 模式遵守同样 cap
- "按上下文拆"是不引入 phase 概念的根本论证 — workflow.yaml event-driven 才是更原生表达,不要走回头路

---

## 六、要点速回

1. **拆 agent 前先问"信息能不能真隔离"** — 重叠多就同一 agent。不要按 role 思维 default 拆;Vibe Coding 里"写功能 + 写测试"应该一个 agent 干完
2. **5 个 canonical 模式覆盖 95% 编排需求** — Chaining / Routing / Parallelization(vote+segment) / Orchestrator-Worker / Evaluator-Optimizer;新需求先映射到这 5 个再说
3. **Orchestrator-Worker 是主流;两条实现路线不是技术选择,是控制权位置选择** — prompt-as-orchestrator(灵活、token 贵、不决定论)vs code-as-orchestrator(决定论、长跑、低 token)。详对比见 `omc-vs-ccteam-orchestration.md`
4. **Composability 比 mode 数量重要** — OMC 真正杀手锏是嵌套 + 接力 + 修饰符三种拼装,不是 8 个 mode 本身。设计新 mode 前先想:能不能用现有模式组合表达?
5. **Evaluator-Optimizer 必须 hard cap** — Claude Code 架构红线:fix-loop 撞 3 次必 escalate,绝不静默重置。`polite-stop anti-pattern` 是真实风险,hook 抗体比 prompt 自律靠谱
6. **State proliferation 是 composability 的隐藏成本** — OMC 387 行 cancel SKILL.md 是反面教材;编排层的"所有 state 进 progress.jsonl"红线是用一致性换可维护性

---

## 七、引用来源

| 论点 | 出处 |
|---|---|
| 按上下文拆不按角色拆 + checklist | 「如何设计 Multi-Agent」(2026-05 大模型大鱼公众号) |
| 5 种编排模式分类 + 用例描述 | 「超有用的 5 种编排模式」(同上) |
| Canonical 5-pattern 术语 | Anthropic Engineering Blog "Building Effective Agents"(2024-12) |
| OMC 8 mode 全谱 + composability 三层 | [`research/omc-orchestration-modes.md`](research/omc-orchestration-modes.md) §一 §三 |
| OMC vs ccteam 编排架构两条路线 | [`research/omc-vs-ccteam-orchestration.md`](research/omc-vs-ccteam-orchestration.md) §四 |
| OMC team 7-phase pipeline + 5-stage routing 表 | `oh-my-claudecode` `skills/team/SKILL.md` |
| OMC `.omc/handoffs/<stage>.md` handoff 机制 | 同上 §"Stage Handoff Convention" |
| `team ralph` composition(横切修饰符) | 同上 §"Team + Ralph Composition" |
| OMC `ccg` vote / advisor 模式 | `skills/ccg/SKILL.md`(详 `omc-orchestration-modes.md` §2.3) |
| OMC `ultrawork` parallel + worktree 隔离 | `skills/ultrawork/SKILL.md` + `src/team/git-worktree.ts` |
| OMC `ralph` boulder-never-stops + polite-stop 抗体 | `skills/ralph/SKILL.md`(详 `omc-orchestration-modes.md` §2.6) |
| ccteam-flow workflow.yaml + orchestrator | `CLAUDE.md §〇/§一` + `tech-design.md` §7 |
| ccteam fix-loop hard cap 红线 | `CLAUDE.md §三` |
| ccteam event-driven(非 phase)拓扑论证 | `CLAUDE.md §〇/§三` |
