# Phase Prompt Architecture — 协议层与领域层分离

> V0.2 §5.3 子设计文档。配套 PRD `docs/versions/v0-2/prd.md`。
>
> **TL;DR**:phase markdown 三层架构 — frontmatter 是协议(declarative)、
> 正文是领域(用户全权)、orchestrator 注入的 inject-prompt 是运行时拼装层
> (从 frontmatter 字段差异化生成)。**协议关键词不再出现在用户正文**。

---

## 1. 设计哲学溯源 — Claude Code 自己怎么做

观察 Claude Code 所有 LLM-facing 文件的统一模式:

| 文件 | frontmatter 承载 | 正文承载 |
|---|---|---|
| `.claude/skills/<name>.md` | `name`, `description`, `tools` 等协议字段 | 100% 自由 prompt(LLM 唯一消费者) |
| `.claude/agents/<name>.md` | `name`, `description`, `tools`, `model` | 100% 自由 prompt |
| `.claude/output-styles/<name>.md` | `name`, `description` | 100% 自由 prompt |
| `~/.claude/CLAUDE.md` | (无) | 100% 自由 prompt(用户全权) |
| `.claude/settings.json` 中 hooks | declarative event + command | (无 prompt — Claude Code 系统执行行为) |

抽出三条铁律:

1. **协议永远在 frontmatter 或 declarative 配置**;正文从不承载协议关键词、grammar、magic string
2. **正文是 LLM 唯一消费者**;不要求出现特定字符串、不被 lint 校验内容
3. **行为差异由 declarative 字段表达**;系统(Claude Code core)根据字段执行差异化行为

ccteam 当前 phase markdown **违反了 1 + 2**:正文里写了 `PHASE_DONE: implement` 关键词、`完成后写 .ccteam/implement-report.md` 协议级文件声明、`ESCALATE: <reason>` grammar 例子。这些指令同时在 frontmatter 字段表达(`required_outputs`、`completion_signal`)和正文重复出现 — **唯一 source of truth 模糊,用户改正文可破协议**。

## 2. 现状:协议注入路径已经存在(但只走了一半)

`crates/ccteam-core/src/progress.rs:145-169` 已经做事实上的协议注入。orchestrator 派发 phase 时不发完整 prompt,而是发一条 short prompt(让 send-keys 一次容纳得下):

```
请按 @.ccteam/phases/{phase}.md 完成本阶段。完成后写 .ccteam/{phase}-report.md,
并在最后单独输出一行:PHASE_DONE: {phase}(或 ESCALATE: <一句话原因>)。
```

LLM 见此 short prompt,自己 `Read .ccteam/phases/<phase>.md` 拉到正文。

事实上 orchestrator 已经把核心协议(PHASE_DONE / ESCALATE / report 路径)放在了 inject prompt 里。phase markdown 正文里**重复**这些协议关键词只是历史遗留 — V0.2 §5.3 的工作是**清理重复**,把协议唯一 source 收敛到 inject prompt + frontmatter,正文只留领域。

## 3. 三层架构

```
┌────────────────────────────────────────────────────────────────┐
│  Layer 1 — frontmatter (协议层 / declarative)                  │
│    • required_inputs / required_outputs (IO 契约)              │
│    • auto_loop / completion_signal (循环行为)                   │
│    • decision_mode / max_clarify_rounds (用户决策行为)          │
│    • sub_skills / tools_required (工具表面)                     │
│    • escalate_grammar / outbox_protocol (异常 / 询问 grammar)   │
│    • golden_rules.protocol (协议级红线 — 见 §6)                 │
│    Source of truth — 用户改需谨慎,doctor 强校验                 │
└────────────────────────────────────────────────────────────────┘
                              │
                              ↓ orchestrator 加载时读
┌────────────────────────────────────────────────────────────────┐
│  Layer 3 — orchestrator inject prompt (运行时层)               │
│    根据 frontmatter 字段差异化拼装 short prompt,send-keys 注入  │
│    模板化、declarative composition,不一刀切                     │
│    用户看不到,但可通过 `ccteam phase show` 渲染调试             │
└────────────────────────────────────────────────────────────────┘
                              │
                              ↓ short prompt 里 `@` 引用
┌────────────────────────────────────────────────────────────────┐
│  Layer 2 — phase markdown 正文 (领域层 / 自由 prompt)           │
│    • 任务叙述                                                   │
│    • 业务约束清单(代码风格、commit 规范、设计偏好)             │
│    • 角色 framing                                               │
│    • golden_rules.domain(业务级偏好,见 §6)                     │
│    用户全权改,doctor 不校内容                                   │
└────────────────────────────────────────────────────────────────┘
```

LLM 见到的最终 prompt = Layer 3(short)+ `@` 引用拉取的 Layer 2 全文。
Layer 1 frontmatter 不直接进 LLM 视野,只通过 Layer 3 转译进。

## 4. frontmatter 协议字段全集(基于 §5.1 扩展)

V0.2 § 5.3 不**新增大量字段**,主要是把已有字段语义梳理 + 补两到三个声明性字段。

### 4.1 现有(interfaces.md §5.1 已有)

| 字段 | 语义 | inject prompt 用法 |
|---|---|---|
| `required_inputs` | 必读上游产物 | inject 时加"读取 @<path>"段 |
| `required_outputs` | 必产出物 | inject 时加"完成时产出 .ccteam/<path>"段 |
| `auto_loop` | 是否自循环 | inject 时加"达到 `completion_signal` 退出"段 |
| `completion_signal` | 自循环退出信号 | 同上 |
| `decision_mode` | sync / async / hybrid | inject 时加对应询问协议指引 |
| `max_clarify_rounds` | 多轮 CLARIFY 上限 | inject 时加"超过 N 轮自动 ESCALATE" |
| `sub_skills` | phase 边界自动 sub-skill | 不进 LLM prompt(orchestrator 自行编排) |
| `tools_required` | 工具表面声明 | 不进 prompt(启动期校验用) |

### 4.2 V0.2 新增(为完整 declarative 表达协议)

| 字段 | 默认 | 用途 |
|---|---|---|
| `completion_signal` | `PHASE_DONE: <name>` | **改默认**:从隐式硬编码变 frontmatter 显式默认。auto_loop=false 时也使用 |
| `escalate_grammar_ref` | `standard` | 引用 `team.yaml.escalate_grammar_extensions`;inject prompt 据此拼"异常时输出 ESCALATE: <prefix> ..."指令 |
| `outbox_question_protocol` | `v1` | 询问用户走哪个版本 outbox 协议;inject prompt 据此拼"询问用户必须写 outbox,不要用 AskUserQuestion / 纯文本"段 |
| `inject_directives` | `[read_inputs, write_outputs, completion_signal, escalate_grammar, outbox_protocol]` | 高级 escape hatch — 用户可关闭某段注入。**默认全开**,绝大多数用户用不到 |

### 4.3 字段红线

为防止 frontmatter 膨胀:

- **declarative only** — 字段值是事实(eg `auto_loop: true`),不是命令式行为(eg `auto_loop_handler: "function() { ... }"`)
- **YAGNI** — 新字段必须有当前 consumer(orchestrator inject prompt template / doctor 校验);"将来可能用到"不加
- **可空回退** — 字段缺省时回退到 team.yaml 默认 → ccteam-core 内置默认;不强制每 phase 都写

## 5. orchestrator inject prompt 拼装规则

### 5.1 模板结构

```
[每 phase 必有]
请按 @.ccteam/phases/<phase>.md 完成本阶段。

[条件段 — 据 frontmatter 字段开关]

if required_inputs 非空:
  上游产物(必读):@<path1> @<path2> ...

if required_outputs 非空:
  完成时必产出:.ccteam/<path1>, .ccteam/<path2> ...

[完成 / 异常协议 — 必有]
完成后:输出一行 <completion_signal>(默认 "PHASE_DONE: <phase>")
异常时:按 <escalate_grammar_ref> grammar 输出 ESCALATE: <prefix> <reason>
       (前缀清单见 team golden_rules)

if outbox_question_protocol == "v1":
  询问用户:写 .ccteam/outbox/clarify-<ts>.md(不要用 AskUserQuestion / 纯文本提问)

if auto_loop == true:
  本 phase 启用 auto_loop。检测到 <completion_signal> 之前会反复 prompt 你继续。
  达到 <auto_loop_max_iterations> 次仍未出信号 → 自动 ESCALATE。

if decision_mode == "sync":
  用户在线 — 用 AskUserQuestion 直接询问(本 phase 例外允许)
elif decision_mode == "async":
  用户离线 — 询问走 outbox(同 outbox_question_protocol)
elif decision_mode == "hybrid":
  ...
```

### 5.2 send-keys 容量约束

tmux send-keys 单次容量 ~4KB。模板拼装产物保持在 1KB 以内 — 模板字段是开关式拼接,不是叙述堆砌。LLM 看 short prompt + `@` 引用拉 phase markdown 详细领域内容。

### 5.3 实现位置

`crates/ccteam-core/src/progress.rs::build_phase_prompt_with_attachments`
扩展为接受 `&PhaseTemplate`(取代当前只传 phase 名 + attachments),
内部据 frontmatter 字段做模板填空。

## 6. team.yaml.golden_rules 拆分

当前 `team.yaml.golden_rules` + phase YAML `golden_rules` 都是 list of `{rule_id, cmd | pattern}` enforcement check。语义上承载两类:

- **协议级红线**(必须走 outbox / 禁用 AskUserQuestion / 必跑测试)— 系统侧关心
- **业务级偏好**(代码风格、不写 SQL 注入)— LLM 应当遵守

V0.2 拆成两段:

```yaml
golden_rules:
  protocol:                          # 协议红线 — orchestrator inject prompt 注入到 short prompt
    - rule_id: outbox_only
      enforce: prompt_directive       # 仅 prompt 层,不跑命令
      directive: "询问用户唯一合法出口是 outbox,禁用 AskUserQuestion / 纯文本"
    - rule_id: tests_green
      cmd: cargo test --workspace     # phase_done 后 enforcement 跑
      enforce: cmd_check
  domain:                            # 业务偏好 — 由 phase markdown 正文消化(用户改正文可改这段)
    - rule_id: no_sql_injection
      directive: "永远不写 SQL 字符串拼接;用参数化 query"
    - rule_id: prefer_small_pr
      directive: "PR 控制在 500 行以内,大改动拆 stack"
```

orchestrator 行为差异:

- `protocol.*.enforce: cmd_check` — 现有 §5.1 enforcement 路径不变
- `protocol.*.enforce: prompt_directive` — inject prompt 拼接进 short prompt
- `domain.*` — 不动 inject prompt;由 phase markdown 正文显式或隐式 reference;用户改正文等价于改 domain rules

## 7. phase markdown 改造前后(示例 `phases/03-implement.md`)

### 7.1 改造前(现状)

```markdown
---
name: implement
required_inputs: [.ccteam/plan-eng.md, .ccteam/architecture.md]
required_outputs: [.ccteam/implement-report.md, .ccteam/code-review.md]
sub_skills: [...]
tools_required: { subagents: [code-reviewer] }
---

# 任务:代码实现

读取上游产物:
- `@.ccteam/plan-eng.md` —— 任务拆分清单                    ← 协议级 (重复 frontmatter)
- `@.ccteam/architecture.md` —— 模块图                       ← 协议级 (重复)

按 plan-eng 任务清单逐项实现。约束:                          ← 领域
- 不引入未在 plan-eng 中声明的新依赖                          ← 领域
- ...

完成后写 `.ccteam/implement-report.md`,概括:                ← 协议级 (重复 required_outputs)
- 已实现的任务清单(对照 plan-eng 勾选)                      ← 领域
- ...

写完 implement-report 后写 `PHASE_DONE: implement` ——        ← 协议级关键词 (重复)
**ccteam orchestrator 会在 phase_done 边界自动触发 ...**     ← 协议级解释 (用户不该关心)
```

### 7.2 改造后

```markdown
---
name: implement
required_inputs: [.ccteam/plan-eng.md, .ccteam/architecture.md]
required_outputs: [.ccteam/implement-report.md, .ccteam/code-review.md]
completion_signal: "PHASE_DONE: implement"
escalate_grammar_ref: standard
outbox_question_protocol: v1
sub_skills: [...]
tools_required: { subagents: [code-reviewer] }
---

# 任务:代码实现

按 plan-eng 任务清单逐项实现。

约束:
- 不引入未在 plan-eng 中声明的新依赖
- 每个文件保持单一职责;避免巨型函数
- 关键边界条件写清楚断言或防御代码

implement-report 内容:
- 已实现的任务清单(对照 plan-eng 勾选)
- 偏离原计划的地方(及原因)
- 已知遗留(留给 test / fix 阶段处理的)
```

少了什么:

- 上游产物路径声明 → frontmatter `required_inputs` + orchestrator inject prompt 注入"读 @<paths>"段
- "完成后写 .ccteam/implement-report.md" → frontmatter `required_outputs` + inject prompt 注入"产出 .ccteam/<paths>"段
- "PHASE_DONE: implement" 关键词 → frontmatter `completion_signal` + inject prompt 注入完成协议
- "ccteam orchestrator 会在 phase_done 边界自动触发 reviewer" 解释 → 协议级解释,用户不该关心,删除

剩下的全是领域:任务叙述、业务约束、报告内容要点。**用户怎么改都改不到协议**。

### 7.3 inject prompt 渲染示例

orchestrator 派发 implement phase 时,根据 frontmatter 拼出:

```
请按 @.ccteam/phases/implement.md 完成本阶段。

上游产物(必读):@.ccteam/plan-eng.md @.ccteam/architecture.md
完成时必产出:.ccteam/implement-report.md, .ccteam/code-review.md

完成后:输出一行 PHASE_DONE: implement
异常时:按 standard grammar 输出 ESCALATE: <prefix> <reason>
询问用户:写 .ccteam/outbox/clarify-<ts>.md(不要用 AskUserQuestion / 纯文本)

[domain rules:]
- 询问用户唯一合法出口是 outbox,禁用 AskUserQuestion / 纯文本
```

LLM 见 short prompt → 自己 Read phase markdown → 看到纯领域指令,知道要做什么 + 怎么做。

## 8. 边界 / 红线

继承自 `tech-design.md §3` + `ccteam-as-domain-agnostic-orchestrator.md §3`,新增三条:

1. **协议关键词唯一 source 在 inject prompt template + frontmatter,不在 phase markdown 正文** — 任何 PR 把 `PHASE_DONE` / `ESCALATE` / `required_*` 文件路径字面量写回正文都要 review 拒绝
2. **frontmatter 字段必须 declarative + 当前有 consumer** — 反对"将来可能用到"的字段
3. **正文不被 lint 内容,只被 lint 存在(非空)** — 不让 doctor 校"正文必须含 X" — 否则等于把协议关键词偷偷塞回正文

## 9. doctor 校验策略

V0.2 §5.3 doctor 增量:

| 校验项 | 类型 | fail-loud |
|---|---|---|
| frontmatter schema 严格(字段类型 / 必填项) | 已有 | yes |
| `required_outputs` 是合法相对路径 | 已有 | yes |
| `completion_signal` 非空 | 新加(V0.2) | yes |
| `escalate_grammar_ref` 在 team `escalate_grammar_extensions` 列表内或 `standard` | 新加 | yes |
| phase IO 契约:phase A `required_outputs` ⊇ phase B `required_inputs`(如果 B 在 A 之后) | 新加 | yes |
| 正文非空 | 新加 | yes |
| **正文不含协议关键词**(`PHASE_DONE` / `ESCALATE:` / `required_outputs` 文件路径字面量) | 新加,**warn 不 fail** | 让用户知道正文重复了协议,但不阻止运行 |

最后一条是反模式 detector,不是强校验。

## 10. 跟 V0.2 其他章节的关系

| V0.2 章节 | 关系 |
|---|---|
| §2 自循环 | 自循环行为由 `auto_loop` frontmatter 字段表达;inject prompt 据此差异化拼装 |
| §3 watchdog | 不影响 — watchdog 是 runtime translation 层,不动 phase prompt |
| §4 工厂 | 工厂产出新 team 的 phase markdown 自然走本设计:frontmatter 字段填好 + 正文纯领域。工厂复杂度大幅降低 — 不用产协议关键词 |
| §5.1/§5.2 团队布局 | 正交。布局解决文件**在哪**,本文档解决文件**写什么** |
| §5.4 重命名 | 正交 |

## 11. 实施步骤(V0.2 时间盒)

按依赖顺序:

1. **frontmatter 字段补齐**(`crates/ccteam-core/src/phases.rs`):加 `completion_signal` / `escalate_grammar_ref` / `outbox_question_protocol` / `inject_directives` 字段(`Option<String>`,空回退默认)。serde alias 兼容旧 yaml
2. **inject prompt 模板化**(`crates/ccteam-core/src/progress.rs`):`build_phase_prompt_with_attachments` 升级接受 `&PhaseTemplate`,据字段拼装;旧调用点同步迁移
3. **team.yaml.golden_rules 拆分**(`crates/ccteam-core/src/team.rs`):新增 `protocol` / `domain` 子段,旧扁平 list serde alias 当 `protocol` 加载
4. **phase markdown 正文清理**:dev / product-research 共 12 个 phase 改造,删协议级片段。改动单测覆盖每个 phase rendering 后的 short prompt 包含必要协议指令
5. **doctor 校验增量**:`crates/ccteam-cli/src/doctor.rs`(或同等)加 §9 校验项
6. **`ccteam phase show <team> <phase>` 命令**:渲染最终 short prompt + `@` 引用 phase markdown,给用户调试看
7. **interfaces.md §5.1 / §5.5 更新** + `tech-design.md §3` 加新红线
8. **e2e + 测试覆盖**:确保 369 baseline 不退步;新加 ~15 个测试覆盖 inject prompt 拼装 / golden_rules 拆分 / doctor 校验

## 12. 测试 / 验证策略

**Property tests**:

- 任意 frontmatter 组合 → inject prompt 永远 ≤ 1 KB
- inject prompt 永远包含 `completion_signal`(若 frontmatter 字段非空)
- inject prompt 永远包含 `@.ccteam/phases/<name>.md` 引用

**Negative tests**:

- 故意把 phase markdown 正文里加一段废话 → phase 行为不变(LLM 看到废话但协议指令仍由 inject 提供)
- 故意删 frontmatter `completion_signal` 字段 → doctor fail-loud
- 故意改 phase markdown 正文里残留的旧协议关键词 → doctor warn(不 fail)

**Smoke**:

- dev team 走完一遍 plan-eng → implement → test-author → test-run → fix → ship,各 phase 产出 PHASE_DONE 正常,phase_history 完整
- product-research 走完一遍

## 13. 待讨论 / V0.3 留白

1. **inject prompt 模板国际化** — 当前默认中文。需要按用户语言切换吗?(YAGNI;用户改 ccteam-core 模板字符串简单粗暴)
2. **inject prompt 完全外部化** — 把模板字符串挪到 `~/.ccteam/inject-prompt-templates/<name>.tmpl`,允许用户改注入指令本身。开放性强但破红线 — 用户能改协议层。**V0.2 拒绝**
3. **frontmatter 字段集合委员会** — 字段会随新需求增长。需要 governance 流程,任何新字段必须配套 inject template / doctor 校验 / interfaces.md 更新
4. **phase markdown 正文里 `@` 引用上游产物** — 当前正文里的 `@.ccteam/plan-eng.md` 算协议(被 frontmatter required_inputs 表达)还是领域(LLM 提示)?V0.2 倾向**两者都允许**:frontmatter required_inputs 是真理,inject prompt 注入"读 @<paths>";正文里 `@` 引用是 LLM 友好提示,允许保留但删了也无妨

## 14. 不在范围

- 方案 A 双文件分离 — 跟 Claude Code 单文件惯例冲突
- 方案 B Marker 双段 — 引入非 Claude-Code 的 marker convention
- 方案 D 进一步把 inject prompt 完全外部化(允许用户改协议) — 破红线
- LLM-aware phase markdown 校验("用 LLM 看正文改坏没") — 破"控制平面无 LLM"红线
- 自动跑 phase 做 dry-run 测试(`ccteam phase test`)— V0.3

---

## Changelog

- 2026-05-07:初稿。基于 PRD V0.2 §5.3 D 方案展开。
