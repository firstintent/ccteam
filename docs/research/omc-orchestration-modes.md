# OMC 编排模式全谱 + 对 ccteam 未来架构的启发

> **调研对象**:`oh-my-claudecode@4.13.7` 的 8 种编排模式
> **目的**:理解 OMC 为啥需要这么多 mode、各自解决什么问题、composability 怎么做的;给 ccteam 未来从"单 workflow.yaml 驱动"扩展成"多模式编排库"的设计提供参考。
> **配套文档**:[omc-vs-ccteam-orchestration.md](omc-vs-ccteam-orchestration.md)(Team 模式 vs ccteam Rust 编排的深度对比)。本文聚焦"全部 8 种模式的横向对比 + 设计启发"。

---

## 一、八种 mode 速览(按"轴"组织,不按字母)

OMC 的 8 种模式不是平铺的并列关系,是沿着**三条正交轴**组合出来的:

```
                    完整度 (lifecycle coverage)
                      ↑
              autopilot ─────────────────────── 5-phase 全自动 (spec→plan→exec→qa→validate)
                  │ uses
                  ↓
                ralph ─────────────────────── 持久化 + PRD-driven + 验证闭环
                  │ uses
                  ↓
              ultrawork ──────────────────── 纯并行执行(组件,非独立模式)

                    并发模型 (concurrency model)
                      ↑
      team (Claude native) ←─── omc team (tmux CLI)         ccg (一次性 advisor)
      多 agent 共享 task list          多进程 tmux pane          双 LLM advisor 并行
        + 跨 agent 消息                  + 文件契约 IPC          + Claude 合成

                    决策深度 (planning depth)
                      ↑
        ralplan (consensus planning)       plan (interview/direct)        autopilot/ralph (执行)
        Planner→Architect→Critic 循环      Planner 一次出图              不规划,直接干
```

| Mode | 类型 | 入口 | 持久化? | 并行? | 验证闭环? | 决策深度 |
|---|---|---|---|---|---|---|
| **team** | 多 agent 协作 | `/team N:agent "task"` | 文件状态 + lead context | ✅ N teammates | ✅ team-verify/fix | 中(stage routing) |
| **omc team (CLI)** | 多进程 | `omc team start ...` 或 `/omc-teams` | `.omc/state/team/.../events.jsonl` | ✅ tmux panes | ❌ 自己管 | 低(任务级) |
| **ccg** | 多 LLM advisor | `/ccg "task"` | ❌ 一次性 | ✅ codex + gemini 并行 | ❌ | 低(advisor) |
| **autopilot** | 全生命周期 | `/autopilot "idea"` | 多文件 state | ✅(in phase 2/4)| ✅ 5-phase | 高(含 deep-interview hook) |
| **ultrawork** | 并行组件 | `/ulw "tasks"` | ❌ | ✅ Task() 并行 | 轻量(仅 build/test) | 低 |
| **ralph** | 持久化循环 | `/ralph "task"` | `prd.json` + `progress.txt` | ✅(继承 ultrawork)| ✅ PRD 故事级 + reviewer | 中(每故事验证) |
| **pipeline** | 顺序 staged | (legacy,无独立 SKILL) | `.omc/state/pipeline-state.json` | ❌ 顺序 | 自定义 | — |
| **ultrapilot** | 已废弃 | (legacy,autopilot alias) | `.omc/state/ultrapilot-state.json` | — | — | — |

**两个 legacy mode** 只在 `skills/cancel/SKILL.md` 的状态清理列表里出现,没有独立 SKILL.md;Pipeline 概念已被 team-plan/prd/exec/verify/fix 吸收;Ultrapilot 完全被 autopilot 取代。

---

## 二、逐个 mode 深拆

### 2.1 Team(canonical staged pipeline)

- **定位**:核心推荐路径,**Claude Code native team tools 包装**(TeamCreate / Task(team_name, name) / SendMessage / TaskList / TaskUpdate / TeamDelete)
- **Stage 序列**:`team-plan → team-prd → team-exec → team-verify → team-fix(loop)`
- **每个 stage 路由不同的 specialist agent**(routing 表见 SKILL.md §"Stage Agent Routing");用户传 `N:agent-type` 只覆盖 team-exec 的 worker 类型,其它 stage 由 lead 按风险/复杂度动态选
- **Handoff 机制**:每个 stage 完成时写 `.omc/handoffs/<stage>.md`(decisions/rejected/risks/files/remaining),避免 lead context compact 时丢决策上下文 —— **这是 OMC 独有,ccteam 没有等价物**
- **Fix-loop 上限**:`max_fix_loops`(默认 3),撞顶就 terminal failed,不无限循环
- **状态 SoT**:`~/.claude/teams/<name>/config.json` + `~/.claude/tasks/<name>/<id>.json` + `.omc/state/team/<name>/events.jsonl` + `.omc/handoffs/<stage>.md` + lead conversation context(**分散**)
- **复合**:`/team ralph "task"` 把 team pipeline 包进 ralph 持久化外圈;cancel 协议会级联

### 2.2 omc team(CLI 多进程)

- **定位**:**进程级**并行 —— tmux pane × N 跑真实的 `claude` / `codex` / `gemini` CLI 二进制
- **入口**:`omc team start --team-name foo --agent-types codex,gemini,claude --tasks '...'`,SKILL.md 形态是 `/omc-teams N:codex "task"`
- **与 Team 模式的关键区别**:
  - Team 模式的"teammates"是 Claude Code 内部 Task subagent(同进程内 LLM session);
  - omc team 的"workers"是**外部 OS 进程**,通过 tmux pane + 文件契约(`prompt_file` → `output_file`)通信,**不能用 SendMessage/TaskList**
- **基础设施**:`src/team/runtime.ts`(legacy v1)+ `runtime-v2.ts`(event-driven default)+ `runtime-cli.ts`(detached node 守护)+ `tmux-session.ts`
- **解决的问题**:需要 cross-provider 混搭(`codex` 做架构 + `gemini` 做 UI)、或需要**进程隔离**(每个 worker 独立 git worktree,不抢同一份 file tree)
- **缺点**:CLI worker 是 one-shot,死了就死了,不参与 team messaging;lead 必须手动管 lifecycle(spawn → 监视 → 收 output → mark done)

### 2.3 ccg(Claude-Codex-Gemini tri-model advisor)

- **定位**:一次性"双外脑咨询" —— 不是 team / orchestrator,是**让 Claude 同时问 codex 和 gemini 然后自己合成**
- **流程**:
  1. Claude 把请求拆成两个 advisor prompt(codex 侧重架构/后端;gemini 侧重 UX/设计)
  2. 通过 `omc ask codex "..."` 和 `omc ask gemini "..."` 并行调
  3. 收 artifact 文件(`.omc/artifacts/ask/codex-*.md` + `gemini-*.md`)
  4. Claude 合成成一个最终答复,列出"双方同意 / 双方冲突 / 选哪个 + 为啥"
- **不是编排,是"咨询模式"**:不写代码、不修改文件、不持久化、不验证;只回答问题
- **degrade 策略**:任一 CLI 缺失,继续用 available 的 + Claude 自己;两个都缺则只用 Claude 并明确告知

### 2.4 Autopilot(end-to-end autonomous)

- **定位**:把"一句话 idea"展开成"working code"的全自动管线
- **5 个 phase**:
  1. **Phase 0 — Expansion**:Analyst+Architect 把模糊 idea 展成 spec(`.omc/autopilot/spec.md`)。**特殊跳过**:若已有 `.omc/specs/deep-interview-*.md` 或 `.omc/plans/ralplan-*.md` 则跳过到 phase 1 或 phase 2
  2. **Phase 1 — Planning**:Architect 出 plan,Critic 校验(`.omc/plans/autopilot-impl.md`)
  3. **Phase 2 — Execution**:**调用 Ralph + Ultrawork**(嵌套!)
  4. **Phase 3 — QA**:**调用 UltraQA** cycle(build/lint/test/fix loop,最多 5 cycles,相同错连续 3 次就停)
  5. **Phase 4 — Validation**:三个独立 reviewer 并行(architect 看完整度 / security-reviewer 看漏洞 / code-reviewer 看质量),**全部 approve 才算过**
- **嵌套关系**:`autopilot ⊃ ralph ⊃ ultrawork`,SKILL.md 直接画了这个图(`skills/ultrawork/SKILL.md:128-141`)
- **3-stage pipeline 钩子**:`/deep-interview "vague" → /ralplan --direct → /autopilot`,前面阶段的 artifact 让后面阶段直接跳过自己的 phase 0/1

### 2.5 Ultrawork(parallel execution engine)

- **定位**:**不是独立 mode**,是被其他 mode(ralph / autopilot)嵌套调用的**并行组件**
- **核心**:Fire N 个 Task() 并行,不等;按 tier(haiku/sonnet/opus)路由
- **没有持久化、没有验证闭环、没有 stop condition**(单次跑完就完);**长操作用 `run_in_background: true`**
- **解决的问题**:防止 LLM 把独立任务串行跑(常见 anti-pattern:写完一个 Task 等结果再写下一个)
- **被独立调用的场景**:用户明确说"我自己管完成,只要你并行" —— 否则推荐用 ralph 或 autopilot

### 2.6 Ralph(persistent verify-fix loop)

- **定位**:**PRD-driven 持久化循环**;"不到完成不收工"
- **核心机制**:
  - 启动时强制生成/校验 `prd.json`(per-session 路径 `.omc/state/sessions/<sid>/prd.json`),里面是 user stories + acceptance criteria
  - **关键步骤 1c**:auto-generated PRD 的"Implementation is complete"等模板化 criteria **必须被改写成 task-specific**(否则就是 "PRD theater")
  - 每个 story 单独 implement → verify(每条 criterion 跑真实 build/test/lint)→ mark `passes: true`
  - 全部 stories `passes: true` 后,**reviewer verification**(默认 architect,可 `--critic=critic` 或 `--critic=codex`)
  - **强制 deslop pass**(除非 `--no-deslop`):approval 后调 `ai-slop-cleaner` skill 清理 AI slop
  - **post-deslop regression**:slop cleanup 完再跑一遍测试
  - **anti-pattern 警告**:Step 7 reviewer APPROVED 不算完,必须继续 7.5 → 7.6 → 8 在同一 turn(SKILL.md 反复强调"polite-stop anti-pattern")
- **state**:`prd.json` + `progress.txt`(per-iteration learnings)+ `state_write(mode="ralph", ...)`
- **continuation hook**:`The boulder never stops` —— stop hook 强制 lead 不能轻易停手(类似 ccteam progress.jsonl 但靠 hook 注入 system reminder 实现)

### 2.7 Pipeline(legacy,顺序 staged)

- **状态**:无独立 SKILL.md;只在 `skills/cancel/SKILL.md` 的 state 清理列表里出现(`.omc/state/pipeline-state.json`)
- **含义**:已经被 team-plan/prd/exec/verify/fix 的 staged pipeline 吸收;`team` mode 本身就是 pipeline 模式
- **历史价值**:V3 时代有独立 pipeline mode,V4 native team 重写后融合进 team

### 2.8 Ultrapilot(legacy,autopilot 别名)

- **状态**:废弃,`skills/cancel/SKILL.md` 称为 "deprecated compatibility mode (autopilot pipeline alias)"
- **state 文件保留**:`.omc/state/ultrapilot-state.json` + `.omc/state/ultrapilot-ownership.json` 仍能被 cancel 清掉,backward compat
- **教训**:OMC 演进时**老 mode 改名不强删**,留 state 清理钩子兼容老用户

---

## 三、Composability —— OMC 的真正杀手锏

OMC 不是把 8 个 mode 平铺给用户挑,而是设计了"低层组件 + 高层管线 + 顺序拼接"三层 composition:

### 3.1 组件嵌套(skill 内调用 skill / Task)

```
autopilot (lifecycle)
  └─ Phase 2 calls → ralph (persistence)
                       └─ Step 7 verification calls → architect / critic / codex agents
                       └─ Step 7.5 calls → ai-slop-cleaner skill
                       └─ Each story implementation calls → ultrawork (parallel)
                                                              └─ Task(executor, haiku|sonnet|opus)
  └─ Phase 3 calls → ultraqa (QA cycling)
                       └─ Task(qa-tester)
  └─ Phase 4 spawns parallel → Task(architect), Task(security-reviewer), Task(code-reviewer)
```

> **重要约束**:SKILL nesting 在某些情况下不被 Claude Code 支持(ccg 明确说 "Skill nesting is not supported in Claude Code. Always use the direct CLI path"),所以 OMC 用 `Bash → omc ask codex/gemini` 绕开。**这是个有意思的边界 —— 设计 composition 时要预判哪些可以 skill-nest、哪些必须降级到 CLI 或 Task subagent**。

### 3.2 顺序管线(artifact 接力)

```
/deep-interview "vague idea"
   └─ writes .omc/specs/deep-interview-*.md (ambiguity ≤ 20%)
       ↓
/ralplan --direct
   └─ Planner→Architect→Critic loop, writes .omc/plans/ralplan-*.md (consensus 通过)
       ↓
/autopilot
   └─ detects existing ralplan artifact → SKIP Phase 0+1 → directly Phase 2 (Execution)
```

**关键设计**:每个 mode 入口都**先检测前置 artifact 是否存在**,存在就跳过自己内部对应的 stage。这种 "artifact 接力 + 阶段跳过" 给用户**渐进式投入**的体验:既可以一句话 `/autopilot`,也可以走完整三段管线拿到更高质量。

### 3.3 横切修饰符(/team ralph)

`team` + `ralph` 关键字共现时,SKILL.md `<team_pipeline>` 触发 "team + ralph composition":

- team pipeline 内圈跑 plan→prd→exec→verify→fix
- ralph 外圈包一层 architect verification + 迭代重启(`max_iterations`)
- 两个 state 互相 cross-reference(`state_write(mode="team", linked_ralph=true)` + `state_write(mode="ralph", linked_team=true)`)
- cancellation 级联(cancel 一个清两个)

### 3.4 统一 cancel(`skills/cancel/SKILL.md`,387 行)

**唯一一个 cancel 入口管所有 mode** —— 387 行的 SKILL.md,每种 mode 都有专属的 state 文件清理 + 进程清理 + 资源释放路径,**包括 legacy 的 ultrapilot/pipeline**。设计取舍:不让用户记多个 cancel 命令,统一一个 entry point。

---

## 四、对 ccteam 未来编排模式架构的启发

### 4.1 当前 ccteam(V0.4.0)的编排模型只有一种

ccteam 现在的设计是:

- **一种执行模型**:Rust orchestrator 读 `workflow.yaml` → ArtifactWatcher inotify 监听 → 按 trigger spawn agent
- **一种状态 SoT**:`progress.jsonl` append-only,7 类业务 event
- **一种 agent 抽象**:`.claude/agents/<role>.md`(claude --bg --agent <role>)+ 可选 codex tmux
- **用户接口**:meta-agent + 17 个 mcp tools

**这是"workflow-driven 单一编排模式"**。强:决定论 / 测试性 / 低 token cost / 长时间运行 / 多项目并行。弱:用户得先写 workflow.yaml,**没法用一句话 "/autopilot build me a habit tracker" 直接跑起来**。

### 4.2 ccteam 缺的是"模式分层"和"一句话入口"

OMC 8 种 mode 满足的需求频谱,ccteam 目前只覆盖中间一段(对应 team 模式):

| 需求 | OMC 怎么做 | ccteam 当前 | 缺口 |
|---|---|---|---|
| 一句话变 working code | autopilot | ❌ 必须先写 workflow.yaml | **高层管线 mode** |
| 一次性双外脑咨询 | ccg | ❌ | **advisor mode** |
| 多 agent 协作完成大任务 | team | ✅(V0.4.0) | — |
| 跨 provider 进程级并行 | omc team CLI | 🚧 codex 通过 tmux 半实现 | **provider executor adapter** |
| "干完为止"的持久化循环 | ralph | ⚠️ workflow.yaml 可以表达但语义弱 | **persistence + budget loop** |
| 纯并行执行小批工作 | ultrawork | ⚠️ workflow.yaml parallelism cap 表达,但启动开销大 | **lightweight ad-hoc** |
| 强结构化 consensus planning | ralplan | ❌ | **planning mode** |
| 顺序 staged 流程 | pipeline(被 team 吸收) | ✅(本质就是 workflow.yaml 的边) | — |

### 4.3 三个具体设计启发(优先级排序)

#### 启发 1:在 Rust orchestrator 之上加一个"mode 入口层",不重写架构

**反模式**:把 ccteam 改成"也是 prompt-as-orchestrator"。失去 Rust orchestrator 的决定论 / 测试性 / 低 token 优势。

**推荐模式**:**"mode 是 workflow.yaml 的生成器"**。每种 mode 就是一个 `workflow.yaml` 模板 + 入口 CLI,运行时仍跑同一套 Rust orchestrator。

```
ccteam autopilot "build a habit tracker CLI"
   ↓
   1. meta-agent 把 idea 跑 analyst+architect 两个 ad-hoc Claude session,生成 spec.md
   2. ccteam 用 spec 选 autopilot.workflow.yaml 模板,实例化成 project workflow.yaml
   3. ccteam start <project> — 走标准 orchestrator
   4. orchestrator 完工后 meta-agent 跑 validation phase(三个并行 reviewer agent)
```

**关键**:autopilot/ralph/ultrawork 不是 Rust orchestrator 的特殊代码路径,**而是 meta-agent 端的 workflow.yaml composer + 启动脚本**。Rust orchestrator 保持 thin,**workflow.yaml 是 mode 的 IR**(intermediate representation)。

这样:
- 新增 mode = 写一个 workflow.yaml 模板 + 一个 meta-agent skill,**不动 Rust**
- 测试 mode = 测 workflow.yaml 生成正确性 + run E2E,**不需要测 LLM 行为**
- 多 mode 共存 = `~/.ccteam/mode-templates/<mode>.yaml.tmpl`

#### 启发 2:把 OMC 的 "artifact 接力 + stage skip" 模式吸收进 workflow.yaml

OMC 的 `/deep-interview → /ralplan → /autopilot` 三段管线,每个 mode 入口检测前置 artifact 自动跳阶段,这是**渐进式 quality gate**。

ccteam 可以这样表达:

```yaml
# workflow.yaml
inputs:
  spec_path: ".ccteam/specs/spec.md"
  plan_path: ".ccteam/plans/plan.md"

agents:
  - name: analyst
    if: "!file_exists(${spec_path})"  # 已有 spec 就跳过
    output: ${spec_path}
  - name: architect
    if: "!file_exists(${plan_path})"
    inputs: [${spec_path}]
    output: ${plan_path}
  - name: executor
    parallelism: 3
    inputs: [${plan_path}]
    triggers: [artifact_received:${plan_path}]
```

**好处**:
- 既支持"一句话 autopilot"(从零跑 analyst → architect → executor)
- 也支持"用户已经手写了 spec.md / plan.md,跳过前置只跑 executor"
- F62 的 condition expression(`if: artifact_count > N`)正在 V0.4.1 候选里,**这条启发可以 align 到那个工作流**

#### 启发 3:Handoff 文档机制 —— ccteam meta-agent 跨项目切换的痛点

OMC 的 `.omc/handoffs/<stage>.md`(10-20 行,decisions/rejected/risks/files/remaining)解决了"lead context compact 后阶段决策丢失"的问题。

ccteam meta-agent 的等价痛点:**meta-agent 切到另一个项目 / 自己 compact / 重启 Claude session 后,丢失了"为啥 workflow.yaml 是这样写的"的决策上下文**。

可借鉴方案:
- 每次 meta-agent 通过 `ccteam new` / `ccteam edit workflow.yaml` 做关键决策时,自动 append 一行到 `~/projects/<team>-<slug>/.ccteam/handoffs.md`
- format:`<timestamp> | <stage> | <decision> | <rejected alts> | <risk>`
- meta-agent 下次启动时 `ccteam status` 把 handoffs.md 最近 10 行注入 meta-agent context
- 这是**轻量等价物**,不需要每个 stage 一份独立 md

### 4.4 五个次级启发(短答,可选)

5. **统一 cancel 入口**:ccteam 已经有 `ccteam stop <slug>`,但要确保它能清理 mode-specific state(autopilot phase state、ralph PRD state、ad-hoc Claude session 等)。OMC `skills/cancel/SKILL.md` 387 行是个反面教材:**state proliferation 不收敛会让 cancel 越来越复杂**;ccteam 应该坚持**所有 state 都进 progress.jsonl**,cancel 只动一种文件。

6. **"polite-stop anti-pattern" 是真实风险**:OMC ralph SKILL.md 反复警告 lead Claude 不要在 reviewer approved 后 "礼貌停手"。ccteam meta-agent 在长任务里也可能有 —— V0.4.1 可以给 meta-agent 的 ccteam-control skill 加一条 "agent_done event 不等于 workflow_done event;workflow_done 才是终止信号"。

7. **CLI worker 文件契约 比 终端解析靠谱**:OMC `cli-worker-contract.ts` 让 codex/gemini 写固定路径 verdict 文件,lead 只读文件不解析 tmux 输出。ccteam V0.4.1 codex executor 标准化(F62)抄这个模式;**永远不要让 orchestrator 跟终端 stdout 打交道**(架构红线已经写了,具体实现要 align)。

8. **Resolved routing snapshot 模式**:OMC `stage-router.ts::buildResolvedRoutingSnapshot()` 在 TeamCreate 时一次性解析所有 role 的 provider/model 路由,存进 `TeamConfig.resolved_routing`,之后所有 worker spawn / scale-up / restart 都读 snapshot,**保证 stickiness**。ccteam 多 provider 混搭(V0.4.1+)抄这个模式 —— `workflow.yaml` 解析时一次性 resolve,运行时不重读 yaml。

9. **Provider fallback 要 loud,不要 silent**:OMC `buildLaunchArgs()` 在 codex/gemini CLI 缺失时**显式 throw + SendMessage warning**,降级到 Claude;ccteam 多 provider 时也要这样,**永不静默降级**(架构红线 §三"fix-loop 撞 3 次顶必 escalate 绝不静默重置"的同源原则)。

---

## 五、推荐 ccteam 未来模式路线图(只列出"该有 vs 不该有")

**该考虑加的(按 priority)**:

| 优先级 | Mode 名 | 实现路径 | 说明 |
|---|---|---|---|
| P0 | `ccteam autopilot "<idea>"` | meta-agent skill + workflow.yaml 模板 | 一句话入口,覆盖 OMC autopilot 用户群 |
| P0 | `ccteam ask <provider> "<q>"` | 单进程 ad-hoc CLI(不进 orchestrator)| 等价 `omc ask`,advisor 模式底座 |
| P1 | `ccteam ccg "<task>"` | meta-agent skill 调 `ccteam ask codex` + `ccteam ask gemini` 然后合成 | 不进 orchestrator,纯 advisor 合成 |
| P1 | `ccteam ralph "<task>"` 或 workflow.yaml `mode: persistence` 字段 | workflow.yaml 加 budget cap + reviewer gate | 等价 OMC ralph,fix-loop ≤ 3 ✔ |
| P2 | `ccteam plan --consensus "<task>"` | meta-agent 跑 planner→architect→critic 三 agent 顺序 | 等价 ralplan |

**该 NOT 加的**:

| Mode | 为啥不加 |
|---|---|
| ultrawork(独立) | ccteam workflow.yaml `parallelism:` 字段已经表达;不需要单独 mode |
| omc team CLI(独立) | ccteam 的 codex executor adapter(F62)会吃掉这块需求;不需要单独 mode |
| pipeline(独立) | ccteam workflow.yaml 本身就是 staged graph;明确说 "ccteam IS the pipeline" |
| ultrapilot | 别建,从一开始就避免做 mode alias |

---

## 六、要点速回

1. **OMC 8 个 mode 不是 8 套独立逻辑,是 3 条正交轴的组合**:lifecycle 完整度(ultrawork ⊂ ralph ⊂ autopilot)+ 并发模型(team / omc team CLI / ccg)+ 决策深度(plan / ralplan)。Pipeline 和 Ultrapilot 是 legacy,只保留 state 清理钩子。

2. **OMC 设计的真正杀手锏不是 mode 数量,是 composability**:
   - 组件嵌套(autopilot 内部调 ralph 调 ultrawork)
   - 顺序管线 + artifact 接力(`/deep-interview → /ralplan → /autopilot`,后面 mode 检测前置 artifact 自动跳 phase)
   - 横切修饰符(`/team ralph` 把两个 mode 拼起来,state cross-reference)
   - 统一 cancel(`skills/cancel/SKILL.md` 387 行管所有 mode 状态)

3. **对 ccteam 的核心启发**:
   - **不要重写 Rust orchestrator,加一层 "mode = workflow.yaml 模板生成器"**(meta-agent 端实现,Rust 保持 thin)
   - **吸收 artifact 接力 + stage skip**(workflow.yaml 加 `if: !file_exists(...)`)
   - **加 handoff 文档机制**(`.ccteam/handoffs.md` 单文件追加,meta-agent 切项目时恢复上下文)
   - **优先加 autopilot / ask / ccg / ralph 4 个高层 mode**,跳过 ultrawork / omc-team-cli / pipeline / ultrapilot

4. **OMC 已经踩过的坑别再踩**:cancel state proliferation(8 个 mode → 387 行 cancel)、SKILL nesting 限制(skill 内不能调 skill,只能走 CLI 或 Task)、polite-stop anti-pattern(reviewer approved 之后 lead 容易礼貌停手 → 必须用 hook 强约束 continue)。

---

## 七、引用来源

| 论点 | 证据位置 |
|---|---|
| Team 5-stage pipeline + handoff | `oh-my-claudecode/skills/team/SKILL.md:93-191`(staged pipeline + handoffs) |
| omc team CLI runtime | `skills/omc-teams/SKILL.md` + `src/team/runtime-v2.ts` + `src/team/runtime-cli.ts` |
| ccg 双 advisor 合成 | `skills/ccg/SKILL.md:29-83` |
| Autopilot 5 phase + 3-stage 接力 | `skills/autopilot/SKILL.md:39-104` + `:172-189` |
| Ultrawork 是组件 + 嵌套关系图 | `skills/ultrawork/SKILL.md:128-141` |
| Ralph PRD-driven + deslop + boulder anti-pattern | `skills/ralph/SKILL.md:38-205` |
| Pipeline / Ultrapilot legacy state | `skills/cancel/SKILL.md`(`.omc/state/ultrapilot-state.json` / `pipeline-state.json` 清理) |
| Resolved routing snapshot | `src/team/stage-router.ts::buildResolvedRoutingSnapshot()` |
| CLI worker contract(文件契约 vs 终端解析) | `src/team/cli-worker-contract.ts` |
| ccteam V0.4.0 当前架构 | `CLAUDE.md §一/§三` + `docs/v0-4-0/README.md` |
| ccteam 三层架构红线 | `CLAUDE.md §三`(progress.jsonl SoT、文件系统控制平面、escalate 不静默) |
