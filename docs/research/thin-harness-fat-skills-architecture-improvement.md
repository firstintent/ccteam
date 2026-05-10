# Thin Harness + Fat Skills 对 ccteam 的架构改进建议

面向读者：架构师、产品技术负责人、ccteam 核心贡献者  
参考输入：用户提供的 Garry Tan / Light Cone 访谈笔记，核心方法论为 Thin Harness + Fat Skills、Tokenmaxxing、CEO Plan、Plan-Eng-Review、Browse/QA、独立 review  
说明：本文把参考内容作为方法论输入，不对其中项目数据、star 数、效率倍数等外部事实做背书。本文关注它对 ccteam 使用场景和架构演进的启发。

---

## 1. 一句话结论

ccteam 当前已经具备 Thin Harness 的雏形：Rust orchestrator 很薄，Claude Code 长会话做执行，team/phase markdown 做工作流，MCP/CLI/Web 做适配层。下一步最有价值的方向不是把 Rust harness 做厚，而是把 ccteam 升级成 **Fat Skills 优先的 builder operating system**：

- Rust 层只维护状态机、调度、恢复、权限、观测和协议。
- 复杂经验沉淀到 skills、phase markdown、team plugin、review/QA recipe。
- meta-agent 变成用户的 builder cockpit：先做 CEO 级判断，再做工程规划，再执行，再由独立 reviewer/QA gate 收敛。
- Tokenmaxxing 变成受控上下文策略：多喂 context，但必须结构化、可追溯、可重置。

---

## 2. 参考方法论拆解

### 2.1 Thin Harness

Thin Harness 的核心不是“没有架构”，而是把 harness 的职责限制在确定性边界：

| Harness 应该做 | Harness 不应该做 |
|---|---|
| 调度任务、注入 prompt、维护状态 | 把产品/工程判断硬编码进程序 |
| 管理 session、成本、超时、恢复 | 直接实现复杂工作流细节 |
| 校验输入输出契约 | 解释所有领域语义 |
| 暴露 CLI/MCP/Web 控制面 | 在每个 channel 里嵌一个 LLM |

这与 ccteam 当前红线一致：orchestrator 不内嵌 LLM，progress/state 文件协议是事实源，channel adapter 是 dumb router。

### 2.2 Fat Skills

Fat Skills 的核心是把复杂、模糊、经验性的工作写成 Markdown recipe：

- 怎么做 CEO 级产品判断。
- 怎么做 plan-eng-review。
- 怎么画数据流、状态机、用户路径。
- 怎么做 code review。
- 怎么跑 Browse/Playwright QA。
- 怎么把历史经验转成下一次项目的约束。

这类逻辑如果硬塞进 Rust，会变成脆弱的规则引擎；写在 Markdown skill/phase 里更适合 LLM 理解和发挥。

### 2.3 Tokenmaxxing

Tokenmaxxing 不是无脑浪费 token，而是把 token 当作杠杆：

- 编码前尽可能补齐上下文。
- 让 agent 先展开问题空间，再收敛方案。
- 用图、表、契约、测试和 review gate 防止大量上下文带来的漂移。

对 ccteam 来说，Tokenmaxxing 需要被工程化成“上下文包”和“证据清单”，而不是把所有文件一次性塞进主 session。

---

## 3. ccteam 当前适配度

### 3.1 已经做对的部分

| 参考理念 | ccteam 现状 | 评价 |
|---|---|---|
| Thin Harness | `ccteam-core` 负责状态机、tmux、hooks、team/phase 解析 | 方向正确 |
| Fat Skills | phase markdown、`sub_skills`、team factory、Claude Code plugin 兼容 | 已有基础，但还偏 phase-centric |
| 人类做 director | meta-agent + `ccteam-control` skill + MCP 9 tools | 已具备入口，但还偏调度工具 |
| 先规划再实现 | `plan-eng` 在 `implement` 前，要求架构图和风险分析 | 已有，但 CEO/10x/product taste 不够突出 |
| 自动 review | `implement` phase 可触发 code-reviewer sub-skill | 有基础，但 reviewer 编排还不够系统 |
| 异步用户决策 | outbox / decisions queue / `PHASE_DONE_PENDING` | 很适合“人不在线”场景 |
| 长 session | tmux + Claude Code 长会话 + context reset | 符合 token/cache 复用思路 |

### 3.2 主要差距

1. **Fat Skills 还不是第一等架构对象**  
   当前系统的第一等对象是 team、phase、sub_skill。skill 更像工具依赖，而不是可发现、可组合、可评估的工作流资产。

2. **dev pipeline 缺少 CEO / taste / 10x gate**  
   `plan-eng` 已关注技术严肃性，但对“这个功能是否值得做、有没有 10x 更好方案、是否被传统软件惯性限制”表达不足。

3. **Tokenmaxxing 缺少结构化策略**  
   当前有 memory bridge、context reset、required_inputs，但还没有“上下文收集、压缩、引用、预算、证据追踪”的统一模型。

4. **review/QA 没有形成多 gate 矩阵**  
   code-reviewer 是单点，测试阶段存在，但 Browse/Playwright、UX、security、performance、docs 等维度还没有统一的 critic matrix。

5. **meta-agent 仍偏项目调度员，不够像 builder cockpit**  
   现在 meta-agent 能派单、查询、处理决策；下一步应主动帮助用户做 CEO plan、实验拆分、PR stack 管理和取舍判断。

6. **效率指标还没有闭环**  
   ccteam 记录 cost、phase history、progress，但还没有把“PR 数、测试通过率、review block、返工率、cycle time、代码 churn”等 builder throughput 指标体系化。

---

## 4. 使用场景改进建议

### 4.1 “久不写代码的 founder 重启 coding”

目标用户类似参考中的 builder：有产品判断和 taste，但不想重新陷入 boilerplate。

建议工作流：

```text
用户一句话想法
  → meta-agent 运行 CEO skill
  → 输出 .ccteam/ceo-plan.md / .ccteam/10x-options.md
  → 用户只确认方向和取舍
  → plan-eng-review 生成工程方案 + ASCII 图 + PR stack
  → dev team 执行
  → reviewer / QA gate 收敛
  → ship / retro 写回经验
```

架构需求：

- 增加 `ceo-plan` 或 `product-shape` 作为 dev 前置可选 phase。
- meta-agent 支持 “先 CEO 判断，不直接 new dev project” 的显式路径。
- 产物中必须包含“为什么值得做 / 不值得做”“10x 方案”“MVP 边界”“反惯性选项”。

### 4.2 “技术负责人一天推进多个 PR”

目标用户需要并行推进多个小改动，但仍保持质量。

建议工作流：

```text
meta-agent 接收多个 feature brief
  → 拆成 PR stack
  → 每个项目/子任务独立 dev session
  → watchdog 汇总阻塞点
  → reviewer matrix 对每个 PR 打 gate
  → 用户只处理方向性 block
```

架构需求：

- 把 `max_concurrent_projects` 配置化，并支持 team/project priority。
- 增加 stack 级 dashboard：哪些 PR blocked、哪些待 review、哪些可 ship。
- 每个项目输出 `.ccteam/throughput.json` 或全局 `build_metrics.jsonl`。

### 4.3 “已有应用持续迭代”

这是 ccteam 当前 dev team 的强适配场景，但需要更强上下文加载。

建议工作流：

```text
新需求
  → context pack 收集现有架构、关键文件、测试、历史 lessons
  → plan-eng 先画现有系统影响图
  → implement
  → test + browser QA + regression checklist
  → reviewer gate
```

架构需求：

- 新增 context pack builder：根据需求自动生成 `.ccteam/context-manifest.md`。
- phase prompt 要求先画“变更影响图”，再写实现计划。
- 对 Web 项目自动启用 Playwright/Browse QA skill。

### 4.4 “产品研究到开发的连续闭环”

当前 research 和 dev 是不同 team，但可以更紧密。

建议工作流：

```text
research team 输出 PASS / CONCERN / REJECT
  → PASS 自动生成 dev spec 草案
  → CONCERN 进入 CEO skill 做 scope shrink
  → REJECT 只沉淀 lessons，不启动 dev
```

架构需求：

- 定义 research → dev 的 handoff artifact：`.ccteam/dev-brief.md`。
- meta-agent 支持“研究结论转开发项目”的一键路径。
- verdict schema 与 dev spec schema 建立字段映射。

### 4.5 “自定义团队与 skill pack 发布”

team factory 当前能生成 team plugin。下一步应该支持生成“团队 + skills + commands + agents”的完整 pack。

建议工作流：

```text
用户描述工作流
  → ccteam-team-author 访谈
  → scaffold team.yaml + phases
  → scaffold skills/<skill>/SKILL.md
  → scaffold commands (/ceo, /qa, /review)
  → doctor validate
  → publish local/GitHub plugin
```

架构需求：

- team factory 支持 optional `skills/`、`commands/`、`agents/` 目录。
- `doctor --validate-team` 校验 plugin 内 skill/command/agent 引用是否一致。
- 引入 `skill_intent.yaml` 或等价 manifest，让新 skill 自描述适用场景、输入、输出和推荐挂载 phase。

---

## 5. 目标架构：Skill-First ccteam

### 5.1 架构图

```text
┌─────────────────────────────────────────────────────────────┐
│ Human Builder                                                │
│ vision / taste / tradeoff / final approval                   │
└───────────────────────────────┬─────────────────────────────┘
                                │
┌───────────────────────────────▼─────────────────────────────┐
│ Meta-Agent Builder Cockpit                                   │
│ CEO plan / project dispatch / decision queue / metrics        │
└───────────────────────────────┬─────────────────────────────┘
                                │
┌───────────────────────────────▼─────────────────────────────┐
│ Fat Skills Plane                                             │
│ skills / commands / agents / phase recipes / reviewer matrix  │
│ 产物：ceo-plan、architecture、context-manifest、QA report      │
└───────────────────────────────┬─────────────────────────────┘
                                │
┌───────────────────────────────▼─────────────────────────────┐
│ Thin Harness Plane                                           │
│ Rust orchestrator / state.json / progress.jsonl / tmux/hooks  │
│ 只做调度、恢复、校验、观测、注入，不做领域判断                 │
└─────────────────────────────────────────────────────────────┘
```

### 5.2 设计原则

1. **Harness 越薄越好**  
   Rust 只消费 declarative metadata，不把 CEO、review、QA 的具体判断写成代码分支。

2. **Skills 越厚越好，但产物必须结构化**  
   Markdown 可以长、可以有启发式、可以有例子；但输出必须落到 `.ccteam/*.md|json`，供后续 phase、review 和 UI 读取。

3. **上下文可以多，但要有清单**  
   Tokenmaxxing 必须产出 context manifest，记录读了什么、为什么读、哪些结论来自哪些证据。

4. **review/QA 是 gate，不是建议**  
   reviewer 输出要能阻塞 ship，至少形成 `PASS / CONCERN / BLOCK`。

5. **meta-agent 是 director，不是 worker**  
   meta-agent 不直接写项目代码，而是选择 workflow、派发 team、收敛用户决策和解释 tradeoff。

---

## 6. 具体架构改进项

### 6.1 把 Fat Skills 升为第一等对象

当前：

```yaml
sub_skills:
  - skill: claude-plugins-official:pr-review-toolkit/agents/code-reviewer.md
    trigger: phase_done
    output_to: .ccteam/code-review.md
```

建议演进：

```yaml
skills:
  - name: ceo
    kind: claude_skill
    source: plugin:builder-workflows/skills/ceo
    inputs:
      - .ccteam/spec.md
    outputs:
      - .ccteam/ceo-plan.md
      - .ccteam/10x-options.md

  - name: plan-eng-review
    kind: claude_skill
    source: plugin:builder-workflows/skills/plan-eng-review
    inputs:
      - .ccteam/spec.md
      - .ccteam/ceo-plan.md
    outputs:
      - .ccteam/plan-eng.md
      - .ccteam/architecture.md
      - .ccteam/architecture-diagrams.md
```

实现建议：

- 保持 `sub_skills` 兼容，新增更通用的 `skills` 或 `workflow_skills` 可作为 V0.3+ proposal。
- skill 执行仍由 Claude Code/插件机制完成，orchestrator 只负责触发和接力输出。
- doctor 校验 skill 的输入输出路径、插件可用性、是否声明 tools。

### 6.2 新增 CEO Plan / 10x Gate

建议为 dev team 增加可选前置 phase，或新建 `dev-ceo` team 以避免破坏现有 dev 流程。

产物：

| 文件 | 内容 |
|---|---|
| `.ccteam/ceo-plan.md` | 用户、痛点、目标、非目标、成功指标 |
| `.ccteam/10x-options.md` | 至少 3 个更激进方案及取舍 |
| `.ccteam/scope-cut.md` | MVP 边界、明确不做的事 |
| `.ccteam/product-risks.md` | 产品风险、体验风险、技术风险 |

phase 要求：

- 不直接进入实现。
- 必须指出“传统软件惯性方案”和“agent-native 方案”的差异。
- 对低价值需求给出 shrink / reject / research 建议。

### 6.3 强化 Plan-Eng-Review

当前 `plan-eng` 已要求模块图和关键流程。建议把“先画图再实现”升级为硬产物契约：

| 产物 | 要求 |
|---|---|
| `.ccteam/architecture.md` | 模块边界、依赖方向、失败模式 |
| `.ccteam/architecture-diagrams.md` | ASCII 数据流图、状态机图、用户路径图 |
| `.ccteam/interface-contracts.md` | 关键接口、输入输出、错误语义 |
| `.ccteam/pr-stack.md` | 可独立 review 的任务拆分 |

`implement` phase 必须读取这些产物。缺失时不应脑补实现，应 `PHASE_DONE_PENDING` 或 `ESCALATE NEED_USER_INPUT`。

### 6.4 引入 Context Pack / Tokenmaxxing 策略

建议新增 `.ccteam/context-manifest.md`，由 plan 前置步骤或 context skill 生成。

示例：

```markdown
# Context Manifest

## User Intent
- Source: .ccteam/spec.md
- Summary: ...

## Repo Files Read
| Path | Why it matters | Key facts |
|---|---|---|
| src/orchestrator.rs | phase dispatch logic | decide_tick is pure; process_project owns side effects |

## Docs Read
| Path | Key constraints |
|---|---|
| docs/interfaces.md | state/progress/team schema |

## Historical Lessons
| Source | Lesson |
|---|---|
| ~/.claude/rules/ccteam-lessons-dev.md | ... |

## Open Questions
- ...
```

新增配置建议：

```yaml
context_policy:
  strategy: tokenmax_structured
  max_context_ratio_before_reset: 0.75
  required_context_manifest: true
  packs:
    - repo_architecture
    - tests
    - docs
    - prior_lessons
```

Rust 层只需要知道：

- 哪些 context pack 必须产出。
- context manifest 是否存在。
- token 比例超过阈值时何时 reset。

不要让 Rust 理解“哪些文件语义重要”，这个判断应留给 skill/agent。

### 6.5 Review / QA Gate 矩阵

建议把 review 从单个 code-reviewer 扩展为矩阵：

| Gate | 推荐触发点 | 输出 | 阻塞条件 |
|---|---|---|---|
| Code Review | implement phase_done | `.ccteam/code-review.md` | BLOCK |
| Architecture Review | plan-eng phase_done | `.ccteam/architecture-review.md` | BLOCK |
| Test Review | test-run phase_done | `.ccteam/test-review.md` | BLOCK |
| Browser QA | Web/app 项目 test 后 | `.ccteam/browser-qa.md` + screenshots | BLOCK |
| Security Review | 涉及 auth/network/secret | `.ccteam/security-review.md` | BLOCK |
| UX Review | 用户界面项目 | `.ccteam/ux-review.md` | BLOCK |

对应 team.yaml 可复用现有 `critic_dimensions`：

```yaml
critic_dimensions:
  - name: functionality
    weight: 0.30
    weak_threshold: 0.65
    anti_leniency_strictness: normal
  - name: architecture
    weight: 0.20
    weak_threshold: 0.70
    anti_leniency_strictness: strict
  - name: tests
    weight: 0.20
    weak_threshold: 0.70
    anti_leniency_strictness: normal
  - name: ux
    weight: 0.10
    weak_threshold: 0.60
    anti_leniency_strictness: normal
```

架构红线：

- reviewer 维度来自配置，不写 Rust enum。
- reviewer 输出格式结构化，但具体判断在 reviewer skill。
- ship phase 只消费 gate 结果，不重新解释所有 review 内容。

### 6.6 Meta-Agent 升级为 Builder Cockpit

建议 meta-agent 新增四类能力：

| 能力 | 行为 |
|---|---|
| CEO Mode | 对想法做 10x、scope、risk、research/dev 路由 |
| Stack Mode | 把大需求拆成多个 project/PR stack |
| Decision Mode | 汇总所有 outbox/escalation，帮用户批量决策 |
| Metrics Mode | 汇报吞吐、成本、阻塞、质量 gate 趋势 |

实现上优先通过 skills/commands 扩展 meta-agent，而不是在 orchestrator 中新增智能逻辑。

建议 Claude Code plugin command：

```text
/ceo <idea>
/plan-eng-review <slug>
/qa <slug>
/review <slug>
/stack <brief>
/metrics
```

这些 command 可以只调用现有 MCP/CLI 和读取 `.ccteam` 文件，保持 channel thin。

### 6.7 Team Factory 生成完整 Skill Pack

当前 team factory 主要生成 team.yaml 和 phases。建议扩展为：

```text
~/.config/ccteam/teams/<name>/
  .claude-plugin/plugin.json
  team.yaml
  phases/
  skills/
    ceo/SKILL.md
    plan-eng-review/SKILL.md
    qa/SKILL.md
  commands/
    ceo.md
    qa.md
  agents/
    architecture-reviewer.md
  README.md
```

doctor 增强：

- 校验 phase `tools_required.skills` 是否存在。
- 校验 command 引用的 skill 是否存在。
- 校验 skill 输出是否匹配 phase `required_outputs`。
- 校验 plugin dependencies 是否覆盖外部 agents。

### 6.8 增加 Builder Throughput Metrics

参考里强调 LOC/PR 作为效率信号。ccteam 不应只追 LOC，但可以建立更完整的效率指标。

建议新增全局事件：

```json
{
  "ts": "2026-05-10T00:00:00Z",
  "slug": "dev-example",
  "phase": "ship",
  "metrics": {
    "loc_added": 420,
    "loc_deleted": 80,
    "tests_added": 12,
    "tests_passed": true,
    "review_blocks": 1,
    "fix_cycles": 2,
    "cycle_time_minutes": 47,
    "cost_usd": 3.42
  }
}
```

指标用途：

- meta-agent 汇报“今天推进了哪些项目”。
- watchdog 发现高返工/高成本/低测试覆盖项目。
- retro 自动总结哪些 skill 真正提高质量。

---

## 7. 建议路线图

### M1：文档与 prompt 层快速收益

- 新增 `dev-ceo` team 或 dev 可选 `00-ceo-plan` phase。
- 强化 `plan-eng`：把 ASCII 图、状态机、用户路径图写成 required output。
- 新增 `.ccteam/context-manifest.md` 产物约定。
- 更新 `ccteam-project-creator` skill：默认先问“要不要 CEO plan / research gate”。

### M2：Skill Registry 与 Doctor 校验

- 定义 skill manifest 或 `skill_intent.yaml`。
- `doctor --validate-team` 校验 skills/commands/agents 引用。
- team factory 支持生成 plugin 内 skills 和 commands。
- sub_skill 输出接力规则扩展到更通用的 workflow skill。

### M3：Tokenmaxxing 工程化

- 实现 context pack builder。
- plan phase 自动产出 context manifest。
- context reset 逻辑读取 context policy。
- Web/detail page 显示 context manifest 和证据链。

### M4：Review/QA Gate 矩阵

- 用 `critic_dimensions` 驱动 reviewer matrix。
- 增加 Browser QA / Security / UX 可选 gate。
- ship phase 统一读取 gate summary。
- BLOCK 自动回到 fix 或 escalate。

### M5：Builder Cockpit

- meta-agent 增加 `/ceo`、`/stack`、`/metrics` 工作流。
- Web UI 增加 stack board、decision queue、metrics dashboard。
- watchdog 把异常翻译成“CEO 可决策”的摘要，而不是底层日志。

---

## 8. 需要避免的错误方向

1. **不要把 CEO/QA/review 逻辑写进 Rust match 分支**  
   这些逻辑属于 Fat Skills。Rust 只校验产物是否存在、gate 是否 PASS。

2. **不要把 Tokenmaxxing 做成无限读文件**  
   大上下文必须有 manifest、摘要和证据链，否则会放大幻觉和 prompt injection 风险。

3. **不要让 Web/Channel 变成第二个 orchestrator**  
   Web、Telegram、Slack 只能走 MCP/actions/inbox/outbox，不直接改 progress 或解析 tmux。

4. **不要让 meta-agent 亲自写项目代码**  
   meta-agent 的职责是 director/cockpit。项目代码仍由 project session 和 team phase 执行。

5. **不要把 LOC 当唯一成功指标**  
   LOC 可以记录，但必须和测试、review block、返工率、cycle time、成本一起看。

---

## 9. 最终建议

ccteam 已经站在 Thin Harness 的正确方向上。下一阶段不要追求更复杂的 daemon，而要把复杂度转移到 Fat Skills：

- 用 CEO skill 提升需求质量。
- 用 Plan-Eng-Review skill 提升设计质量。
- 用 Context Pack 实现受控 Tokenmaxxing。
- 用 reviewer/QA gate 提升交付质量。
- 用 metrics 和 cockpit 提升多项目吞吐。

这样 ccteam 的定位会从“Claude Code 项目编排器”升级为“AI 时代 builder 的操作系统”：人负责 vision、taste 和关键取舍，agent 负责执行、测试、修复和迭代，Rust harness 负责让整个系统长期、可恢复、可观察地跑下去。

