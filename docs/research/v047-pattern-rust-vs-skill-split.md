# V0.4.7 模式扩展:Rust schema vs Skill/Agent prompt 分层决策

> **目的**:为 V0.4.7 候选的四项模式扩展(`extends` / `evaluator-optimizer sugar` / `fan_out` / dynamic routing)提供一份明确的分层决策依据,直接喂给 dev-plan。
>
> **配套文档**:
> - [`claude-code-orchestration-patterns.md`](claude-code-orchestration-patterns.md) — 5 模式 catalog 与拆分哲学(本文 §三 在 catalog §四 启发 1-5 基础上做选型决策)
> - [`omc-orchestration-modes.md`](omc-orchestration-modes.md) — OMC 8 mode 全谱与 composability 三层
> - [`omc-vs-ccteam-orchestration.md`](omc-vs-ccteam-orchestration.md) — prompt-as-orchestrator vs code-as-orchestrator 路线对比
>
> **本文聚焦**:每项扩展"放 Rust 还是放 Skill/agent prompt"的决策原则 + V0.4.7 优先级 + 三个隐藏陷阱。

---

## 一、核心原则:结构性 → schema,语义性 → prompt

ccteam 架构红线:**code-as-orchestrator 保持瘦,LLM 做 LLM 的事**。

**判定规则**:

| 性质 | 落点 | 例子 |
|---|---|---|
| 结构性(配置 / 拓扑 / 计数 / 时限 / hard cap) | Rust schema(`workflow.yaml` 字段 + orchestrator 实现) | parallelism / triggers / max_iterations / fail_counts 3-strike |
| 语义性(看任务内容 / 判断 / 内容合成) | agent.md / skill prompt(LLM 上下文里做) | "这个任务该谁干" / "这个 generation 够好吗" / "这 N 个输出怎么合" |

**第二阶推论**:**agent.md 写的 pattern 比 workflow.yaml 写的 pattern 更 portable across 用户层**。

- `router agent.md` 三种用户层(主会话 / Skill / Rust 编排)都能调
- `workflow.yaml::fan_out:` 只能 Rust 编排层用

→ **拿不准时偏向 agent.md**。schema 是"只服务 Rust 编排层"的承诺;prompt 是"三层都吃"的杠杆。只在"结构性 + 只服务 Rust 骨架"时才加 schema。

---

## 二、模式 × 用户层 2D 矩阵

5 个 canonical 模式在三个用户层各活一次。V0.4.7 改动只动**最右列**(Rust 编排能表达的模式空间)。

| 模式 \ 用户层 | 主会话直编 | Skill 编排 | Rust 编排 |
|---|---|---|---|
| **Chaining** | LLM 顺序 spawn Task() | SKILL "phase 1 → 2 → 3" | `triggers: [artifact_received:...]` ✅ |
| **Routing 静态** | "用 architect 处理 X" | SKILL §stage-routing 表 | `agents.<name>.role` ✅ |
| **Routing 动态** | LLM 在 chat 自选 subagent | SKILL "看 X 选 Y" | **→ router agent.md + spawn_agent MCP(P0)** |
| **Parallel segment** | 同 turn 多 Task() | SKILL 调 ultrawork | `parallelism: N` ✅ |
| **Parallel vote** | LLM 同 turn fork + 自合成 | OMC `ccg` | **→ `fan_out: {count, merge}`(P3)** |
| **Orchestrator-Worker** | TeamCreate + Task | SKILL 1040 行剧本 | meta-agent + 17 MCP ✅ |
| **Evaluator-Optimizer** | LLM 自检重试 | OMC team-verify/fix loop | **→ `evaluates / max_iterations` sugar(P2)** |
| **Composability** | "再 ralph 跑一遍" | `/team ralph` 修饰符 | **→ `workflow.yaml::extends`(P1)** |

矩阵的含义:每行三个格子**互不替代**,只是同一个模式在不同生命周期长度的用户层各活一次。V0.4.7 不是"重做"已有模式,是"让 Rust 编排层多表达 3-4 个模式"。

---

## 三、四项候选逐个决策

### 3.1 Composability `workflow.yaml::extends` ─→ **Rust**

- **性质**:结构性(配置层 mixin)
- **决策**:Rust schema
- **理由**:跟 LLM 完全无关,纯 config layer concern。`WorkflowSpec::load_for_project` 读 parent + deep-merge override。**~100 LOC**(parser + 测试)
- **预期效果**:常见 composition(team + ralph shell / autopilot + deslop)可模板化,用户不必重写整个 workflow.yaml

**Override 语义**(必须写死,见陷阱 3):
- scalar 字段(parallelism / role / model):**子赢**
- list 字段(triggers / inputs):**默认 replace**;`+ [extra]` 前缀显式 concat
- map 字段(agents):**merge by key**,key 冲突子赢

### 3.2 Evaluator-Optimizer sugar `evaluates / max_iterations / on_max_exceeded` ─→ **Rust 加 schema + reuse `fail_counts`**

- **性质**:hard cap 部分结构性,review verdict 部分语义性 → **跨原则**,Rust 描述边界,prompt 描述内容
- **决策**:Rust schema sugar over 已有 3-strike + reviewer agent.md 决定 verdict
- **理由**:`fail_counts: HashMap<role, u32>` + `bump_fail_count` + `escalation_count` 已经在 `crates/ccteam-core/src/orchestrator.rs`(实测 161/200/587/655 行)。sugar 只是把"agent A 输出 → agent B reviewer → B 不通过则 bump A 的 fail_count"这个模式语法糖化。**~80 LOC**(schema + orchestrator dispatch)

**Schema 形态**:
```yaml
agents:
  - name: implementor
    # ...
  - name: reviewer
    evaluates: implementor       # 新字段
    max_iterations: 3            # 复用 fail_counts cap
    on_max_exceeded: escalate    # 或 accept(罕见)
```

**职责分工**:
- Rust 实现:reviewer artifact 含 `verdict: pass | fail` 时,fail → bump `fail_counts[implementor]`,撞 cap → 写 `escalation` event
- agent.md 实现:reviewer prompt 写"如何判断 pass/fail";implementor prompt 不变

### 3.3 Parallelization vote `agent.fan_out: {count, merge}` ─→ **Rust schema + 优先 deterministic merger**

- **性质**:结构性(spawn N + 等 N artifact + merge 触发)
- **决策**:Rust schema;**merger 优先 deterministic 实现**,仅 `merge: custom` 才 spawn merger agent
- **理由**:fan-out 本质是拓扑(N 个 worker + 1 个 fan-in 等待点),merger 的 "vote / best / concat" 在 schema-validated artifact 上**可以 deterministic 实现**(majority / max-confidence / concat)。**~150 LOC**(新 `Trigger::AllArtifactsReceived` 或类似 + 3 个内置 merger + 测试)

**Schema 形态**:
```yaml
agents:
  - name: reviewer
    fan_out:
      count: 3                 # spawn 3 个同 prompt worker
      merge: vote              # vote | best | concat | custom
      merger_agent: ~          # 仅 merge: custom 时指定
    output_schema:             # 强约束 worker 输出 shape
      verdict: enum[approve, reject, abstain]
      confidence: float
      reasoning: string
```

**Worker artifact 格式必须 schema-validated JSON**(见陷阱 2),否则 deterministic merger 无法解析。worker agent.md 模板配套约束输出 shape。

**这条比你原稿砍掉了一个 LLM 调用**:`merge: vote` 不再 spawn merger agent,直接 Rust majority。减少一个 agent 类型 = 减少一份维护成本。`merge: custom` 留口子给真需要 LLM 合成的场景(如 ccg 式"合成双方同意 / 冲突 / 选谁")。

### 3.4 Dynamic Routing via `router agent.md` ─→ **Skill/Agent prompt(0 Rust)**

- **性质**:语义性(看 input 判断 → 调 `spawn_agent(role)`)
- **决策**:agent.md 模板 + 已有 `spawn_agent` MCP 工具,**workflow.yaml 不加 `router:` 字段**
- **理由**:
  - 第一阶(语义性 → prompt)
  - 第二阶(三层用户都需要 routing,只在 Rust 层加字段反而割裂)
- **代价**:不能 declarative 看 workflow.yaml 一眼懂路由 — 但这是可接受的(routing 决策本就依赖任务内容,本来就没法静态可读)
- **LOC**:0 Rust;新增 `docs/research/router-agent-template.md` + 一个 `~/.claude/agents/router.md` 模板

**模板形态**(伪):
```markdown
---
name: router
description: 看任务特征选 role,通过 spawn_agent 派出去
---

你的工作:读输入,判断该让谁处理,然后调用 `mcp__ccteam__workflow_spawn_agent(role=...)`。

判断规则(按优先级):
- 任务含"refactor / 跨文件" → architect
- 任务含"调试 / 编译错误" → debugger
- 任务含"UI / 设计" → designer
- 其它 → executor

不要自己执行任务,只调度。
```

---

## 四、三个隐藏陷阱

### 陷阱 1:Evaluator-Optimizer sugar 的"静默通过"

`evaluates: target / max_iterations: N` 后,reviewer agent.md 自己判断"通过/不通过"。

**故障模式**:reviewer 太宽松,第一轮就 approve → `fail_counts` 永远不 bump → 看起来一切正常。**但 evaluator-optimizer 的价值就在迭代,0 次迭代就通过 = 模式失效**。

**缓解**:
- progress.jsonl 新增 `review_iteration` event(每轮 reviewer 出结论都记,不管 pass/fail)
- SPA WorkflowView 显示 "this loop ran X iterations" — 总是 1 则用户能看出"reviewer 太软" smell
- reviewer agent.md 模板加 prompt 约束:"若 first-pass approve,在 verdict 里说明为啥不需迭代"

### 陷阱 2:Parallel vote 的"schema-prompt 隐性耦合"

`fan_out` 派 N 个 worker 写 artifact,merger(deterministic 或 LLM)读 N 个合成。

**故障模式**:worker artifact 格式未 schema 约束 → 3 个 worker 可能写完全不同结构(一个 markdown 一个 JSON) → deterministic merger 解析失败;LLM merger prompt 要超复杂才能消化。**schema 和 prompt 形成隐性强耦合**。

**缓解**:
- `fan_out` mandate worker artifact 写 **schema-validated JSON**(由 `output_schema` 字段约束)
- worker agent.md 模板里硬写"输出 `{verdict: ..., confidence: ..., reasoning: ...}`"
- deterministic merger 走 serde 解析,fail-loud(不能解析 → escalate)
- LLM merger(`merge: custom`) 仍走 prompt,但 prompt 模板里强调"输入是 schema-validated,直接 destructure"

### 陷阱 3:Composability extends 的"override 语义不明"

`extends: ../shared/ralph-shell.yaml` — 子文件 override 父文件。问题:**怎么 override**?

- `agents` 列表:replace 整个列表? merge by name?
- `triggers`:concat 还是 replace?
- `parallelism: 3` 父 vs `parallelism: 5` 子 → 子赢(直觉)
- 父有 `agent.foo`,子有 `agent.foo.role` 改但其它没变 → 部分 override 还是替换整个 `foo`?

**缓解**(语义在 `docs/interfaces.md` 写死 + 单元测试覆盖):
- scalar(parallelism / role / model / timeout):子赢
- list(triggers / inputs):**默认 replace**(更安全);显式 `+ [extra]` 前缀走 concat
- map(agents,key 是 name):**merge by key**,key 冲突时子赢(deep merge 内部)
- 抄 OMC `omc.jsonc` precedence chain + Kustomize strategic merge 的形态

---

## 五、优先级排序(对原稿调整)

| 我建议优先级 | 项 | LOC | 风险 | 早做的理由 |
|---|---|---|---|---|
| **P0**(立即,可独立于 V0.4.7 ship) | Dynamic routing via router agent.md 模板 | 0 Rust + ~50 行 doc | 极低 | 零 LOC = 零编译风险。可作为 V0.4.6 patch 或 V0.4.7 准备工作发布,**早一天验证"agent.md 模板路径能解决用户痛点"**,省得 V0.4.7 ship 完发现无人用 |
| **P1** | `workflow.yaml::extends` | ~100 LOC | 低 | 纯解析,不动 LLM 行为,可独立测试。先做不亏 |
| **P2** | Evaluator-Optimizer sugar | ~80 LOC | 中 | 复用 `fail_counts`,但需测试 sugar 不破坏既有 3-strike 语义;新 `review_iteration` event 要更新 SPA |
| **P3** | `fan_out` + merger | ~150 LOC | 中-高 | 引入新 trigger 类型(`AllArtifactsReceived`),与 ArtifactWatcher 集成需要小心;`output_schema` 字段是 ccteam 首次引入 worker 输出形态约束,影响面广 |

**关键调整**:
- **router agent.md 从 P4 提到 P0** — 零 LOC 不需等 V0.4.7
- **fan_out 从 P3 保留 P3 但加 deterministic merger 简化** — 砍掉默认 LLM merger 路径,只在 `merge: custom` 时 spawn

---

## 六、跨原则总结:V0.4.7 不是"加 4 个 feature",是执行已有红线

四项放回 §一原则:

| 项 | 结构 / 语义 | 落点 | 是否越界 |
|---|---|---|---|
| `extends` | 结构性(配置 merge) | Rust | ✅ 正确 |
| Evaluator sugar | hard cap 结构性 + verdict 语义性 | Rust schema + reviewer agent.md | ✅ 正确分层 |
| `fan_out` | 结构性(拓扑 + merge 时机) | Rust + deterministic merger 优先 | ✅ 正确(custom 留 LLM 口子) |
| Dynamic routing | 语义性(判断) | router agent.md | ✅ 正确 |

**没有一项越界**。这是 V0.4.7 包看起来很顺的根本原因 — 不是堆功能,是按红线把已有但未声明的模式补上 schema/prompt 表达。

---

## 七、对 dev-plan 的具体输入

下面是直接可贴进 `docs/v0-4-7/dev-plan.md`(假设 V0.4.7 启动时新建)的草稿:

```markdown
## V0.4.7 — 模式扩展

**主题**:Rust 编排层补齐 5 模式 catalog 缺口;按"结构性 → schema,语义性 → prompt"红线分层。

### F-finding 列表

| F# | 项 | 模式 | LOC | 落点 |
|---|---|---|---|---|
| F92 | router agent.md 模板 + 文档 | Routing 动态 | ~50 行 doc | Skill/Agent |
| F93 | workflow.yaml::extends + deep-merge | Composability | ~100 LOC Rust | Rust schema |
| F94 | evaluates / max_iterations sugar | Evaluator-Optimizer | ~80 LOC Rust | Rust schema |
| F95 | fan_out + deterministic merger | Parallel vote | ~150 LOC Rust | Rust schema |

### Ship 顺序
1. F92(独立 ship,可作 V0.4.6 patch)
2. F93(纯解析,先做)
3. F94(基于 F93)
4. F95(架构改动最大,放最后)

### 验收
- F92:用户能用 `Task(subagent_type=router)` 让 Claude 选 role
- F93:`extends: shared/ralph.yaml` 工作,override 语义在 interfaces.md 锁定
- F94:`evaluates: target` + max=3 自动触发 fix loop + escalate
- F95:`fan_out: {count:3, merge:vote}` 派 3 worker → 自动 majority verdict
```

---

## 八、要点速回

1. **判定规则**:结构性(拓扑 / 计数 / 时限 / hard cap)→ Rust schema;语义性(判断 / 内容)→ agent.md prompt
2. **第二阶推论**:agent.md 比 schema 更 portable across 用户层 — 拿不准时偏向 agent.md
3. **V0.4.7 四项各归其位**,没有越界(矩阵在 §三)
4. **优先级**:router(P0,零 LOC)→ extends(P1,纯解析)→ evaluator sugar(P2)→ fan_out(P3)
5. **三个陷阱**:reviewer 静默通过 / fan_out schema-prompt 隐性耦合 / extends override 语义不明
6. **本质**:V0.4.7 不是加功能,是把已有但未声明的模式按红线补上表达

---

## 九、引用来源

| 论点 | 出处 |
|---|---|
| 5 canonical 模式 + Claude Code 原语映射 | [`claude-code-orchestration-patterns.md`](claude-code-orchestration-patterns.md) §二 §三 |
| 三个用户层(主会话 / Skill / Rust) | 会话:三种 Claude Code 多 agent 编排模式辩论 |
| `fail_counts` + `bump_fail_count` 3-strike 实现 | `crates/ccteam-core/src/orchestrator.rs:161/200/587/655` |
| `escalation_count` 事件聚合 | `crates/ccteam-core/src/progress.rs:228-237` |
| OMC `ccg` vote 模式参考 | [`omc-orchestration-modes.md`](omc-orchestration-modes.md) §2.3 |
| OMC role-router resolved snapshot 模式 | [`omc-vs-ccteam-orchestration.md`](omc-vs-ccteam-orchestration.md) §4.2 + §四.4 启发 |
| OMC `omc.jsonc` precedence chain | `oh-my-claudecode/skills/team/SKILL.md` §"Role-Based Routing" |
| ccteam fix-loop hard cap 红线 | `CLAUDE.md §三` |
| ccteam workflow.yaml 当前 schema | `docs/v0-4-0/prd.md` + `docs/interfaces.md` |
