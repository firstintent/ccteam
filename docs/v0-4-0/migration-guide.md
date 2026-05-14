# V0.3.x → V0.4.0 迁移指南

> 本文档面向有 V0.3.x phase 驱动项目（`team.yaml::kind: workflow` + `phases:`
> 列表）的用户。V0.4.0 是 ccteam 的**架构级重构**，phase 模板系统全部
> 删除——本指南帮你把存量项目迁移到新的 `workflow.yaml` + agent role 模型。
>
> 如果你是 V0.4.0 新用户（从未跑过 phase 驱动 workflow），可跳过本文，
> 直接看 [`user-manual.md`](user-manual.md) §2 快速上手。

---

## 1. 为什么要迁移

V0.3.x 的 `team.yaml::kind: workflow` 模式下，每个 phase 的 prompt 和
执行顺序是 ccteam 控制的。V0.4.0 把这层完全删掉，原因（详 [`prd.md`](prd.md) §1）：

- **phase prompt 模板和 Claude Code 内置任务规划能力竞争**——
  inject_directives / golden_rules 实际是在和 `~/.claude/CLAUDE.md` +
  subagent 机制打架，用户真实 prompt 被稀释
- **workflow 不可定制**——每加一种业务 workflow 要改 ccteam 核心代码，
  而不是写一个 YAML
- **可观测性倒置**——orchestrator 管 prompt，但对"哪个 agent 读了哪个文件"
  一无所知；artifact-driven 模型把这层信息变成 first-class state

迁移后你将获得：

- workflow 行为完全由你的 `.claude/agents/*.md` 控制（Claude Code 原生格式）
- 多 agent 并行执行（V0.3.x phase DAG 是顺序的）
- 自然支持 `claude --bg` + Agent View 监控
- progress.jsonl 新增 artifact event / gate event 类型，可观测性更强

---

## 2. 不再支持的内容

### 2.1 EOL 配置

`team.yaml` 的以下字段**不再被 orchestrator 支持**：

```yaml
# V0.3.x team.yaml（V0.4.0 拒绝加载）
kind: workflow                   # 删除（或改 kind: flex）
phases:                          # 全部删除
  - name: plan-eng
    prompt: |
      ...
    inject_directives: [...]
    golden_rules: [...]
    decision_mode: strict
    escalate_grammar: [...]
  - name: implement
    ...
```

V0.4.0 orchestrator 读到 `phases:` 字段会 hard error（明确的 fail-fast，
避免静默忽略导致 phase 不跑的困惑）。

### 2.2 EOL CLI 子命令

下列子命令在 V0.4.0 移除：

| V0.3.x 命令 | V0.4.0 替代 |
|---|---|
| `ccteam phase advance` | 无（phase 概念删除）|
| `ccteam phase rerun` | `ccteam ctl spawn-agent --role <role>` |
| `ccteam phase skip` | 无（artifact-driven 无顺序语义） |
| `ccteam doctor --check-phase-dag` | `ccteam doctor --check-workflow` |

### 2.3 progress.jsonl event 类型变化

V0.3.x 的下列 event 类型**保留**，但语义可能不同：

| Event 类型 | V0.3.x 含义 | V0.4.0 含义 |
|---|---|---|
| `phase_start` / `phase_end` | phase 状态机 | **不再产生**；新写 progress.jsonl 不出现 |
| `session_start` / `session_end` | tmux session 生命周期 | 改名 `agent_spawn` / `agent_done` |
| `cost_update` | cost 累计 | 保留，语义不变 |
| `escalation` | fix-loop 3 次顶 | 保留 |

V0.4.0 新增 event 类型：

- `workflow_start` / `workflow_done`：workflow 整体生命周期
- `agent_spawn` / `agent_done`：agent session 生命周期
- `artifact_created` / `artifact_consumed`：artifact 文件事件
- `gate_locked` / `gate_unlocked`：Gate 状态变化
- `parallelism_changed`：动态调节 parallelism 记录

---

## 3. 自动迁移工具

```bash
ccteam doctor --migrate-phase-to-workflow
```

该命令的行为：

### 3.1 检测阶段

扫描 `~/projects/<team>-<slug>/team.yaml`，找出所有满足条件的项目：

- `kind: workflow`
- 含 `phases:` 字段且非空

每个匹配项目，命令打印：

```
[match] dev-myproject:
  - phases: [plan-eng, implement, review, retro]
  - team.yaml path: /home/user/projects/dev-myproject/team.yaml
```

### 3.2 生成阶段

对每个匹配项目，生成：

1. **`workflow.yaml`**（项目根）：把 phase DAG 翻译成 artifact-trigger
   连线。常见映射：

   ```yaml
   # 生成的 workflow.yaml
   name: <slug>-migrated
   description: "Auto-migrated from V0.3.x phase DAG"

   agents:
     plan-eng:
       executor: claude
       trigger: manual                       # 第一个 phase → manual
       output: .ccteam/plan/

     implement:
       executor: claude
       trigger: watch:.ccteam/plan/          # 顺序连线 → watch
       parallelism: 1                        # 保守起步，不并发
       input: .ccteam/plan/
       output: .ccteam/code/

     review:
       executor: claude
       trigger: watch:.ccteam/code/
       input: .ccteam/code/
       output: .ccteam/review/

     retro:
       executor: claude
       trigger: gate                         # 最后一个 phase → gate（需人解锁）
       input: .ccteam/review/
   ```

2. **`.claude/agents/<phase-name>.md`**（每个 phase 一个文件）：把原
   `phases[].prompt` 内容搬到 markdown 正文，frontmatter 用默认值：

   ```markdown
   ---
   name: plan-eng
   description: "Auto-migrated from V0.3.x phase 'plan-eng'"
   tools: Read, Write, Edit, Grep, Glob, Bash
   model: opus
   ---

   <原 phases[0].prompt 内容>

   ## ccteam 注入的环境变量

   - `$CCTEAM_PROJECT_SLUG`: 项目 slug
   - `$CCTEAM_INPUT`: 输入目录（前一个 phase 的 output）
   - `$CCTEAM_OUTPUT`: 输出目录（写完后下游 phase 自动触发）

   请把你的产出写到 `$CCTEAM_OUTPUT/` 目录，文件名含时间戳 +
   short-id 以避免冲突。
   ```

3. **备份原 `team.yaml`** 到 `team.yaml.v0-3-bak`，新 `team.yaml`
   去掉 `phases:` 字段、`kind:` 改为 `flex`（V0.4.0 中 `kind: flex`
   = workflow-driven 模式的承载，详 [`prd.md`](prd.md) §9.2）。

### 3.3 报告阶段

命令结束时打印总结：

```
Migration summary:
  - 3 projects migrated
  - workflow.yaml created: 3
  - .claude/agents/*.md created: 12
  - team.yaml.v0-3-bak created: 3

Next steps:
  1. Review each .claude/agents/<role>.md and tighten prompts
  2. Review workflow.yaml — adjust parallelism, trigger types
  3. Test with: ccteam run <slug> + ccteam ctl spawn-agent --role <first>
  4. Delete team.yaml.v0-3-bak when satisfied
```

---

## 4. 手动 review 清单

自动迁移**只是结构性转换**，不修改任何 prompt 内容。你需要逐个 review：

### 4.1 Prompt 内容

打开每个 `.claude/agents/<role>.md`，检查：

- [ ] **input/output 路径**：原 phase prompt 可能写死了 `inbox/` /
  `outbox/` 之类的硬编码路径——改成 `$CCTEAM_INPUT` / `$CCTEAM_OUTPUT`
- [ ] **phase 间引用**：原 prompt 可能说"上一个 phase 写了 plan.md"——
  V0.4.0 是 artifact 目录扫描，可能有多个文件；改成 "扫描
  `$CCTEAM_INPUT/` 中所有未处理文件"
- [ ] **ccteam 内置指令引用**：原 prompt 可能引用 `inject_directives` 内容
  （如"按红线 1-5 执行"）——这些指令在 V0.4.0 不再注入，需要把内容
  inline 进 agent prompt 或 `~/.claude/CLAUDE.md`
- [ ] **escalation 语法**：原 phase 可能用 `escalate_grammar` 触发
  escalation——V0.4.0 改用 `ccteam__signal` MCP 工具或 progress.jsonl
  `escalation` event 自描述

### 4.2 Trigger 类型选择

自动迁移把所有"中间 phase" 一律设为 `trigger: watch:<前置 output>`。
但实际场景中：

- **真正需要并行的 phase**（如 fixer）→ 改 `parallelism: 5+`
- **每次只跑一次的 phase**（如 plan-eng）→ 改 `trigger: manual`
- **依赖人决策的 phase**（如 ship）→ 改 `trigger: gate`
- **定时巡检的 phase**（V0.3.x 没有这个概念）→ 改 `trigger: schedule`

review 时问自己：这个 phase 真的"一来文件就跑"吗？还是更适合
"meta-agent 决定什么时候跑"？

### 4.3 Parallelism 调整

默认 `parallelism: 1`（保守）。如果原 phase 的工作天然可分割（每
issue 一个 fix、每文件一个 review），可以提到 5-10。建议：

- 第一次跑 smoke：保持 1，确认逻辑稳定
- 第二次跑：提到 3 看 cost / 速度
- 稳定后：按 budget 决定上限

### 4.4 Agent role 文件 frontmatter

自动生成的 frontmatter 是默认值：

```yaml
---
name: <role>
description: Auto-migrated from V0.3.x phase '<role>'
tools: Read, Write, Edit, Grep, Glob, Bash
model: opus
---
```

按需调整：

- `description`：改成更具体的一句话（Claude Code 选 agent 时看这个）
- `tools`：只列真正需要的（reviewer 可能不需要 Edit；shipper 需要 gh CLI 的话加 `Bash` 但说明用途）
- `model`：简单 fix 用 sonnet 省钱；复杂 review 用 opus 提质量

---

## 5. 顺序 vs 事件驱动语义差异（核心理解点）

V0.3.x phase DAG 是**顺序**的：phase A 完全跑完 → phase B 开始。
V0.4.0 artifact-trigger 是**事件驱动**的：A 写一个文件 → 一个 B session
立即启动；A 写另一个 → 又一个 B session 启动（如果 parallelism 允许）。

这带来几个语义差异：

### 5.1 "完成"的定义不同

V0.3.x：phase A "完成" = orchestrator 收到 phase_end event。
V0.4.0：agent A "完成" = artifact 文件写完。**单个 agent 可以产出多个
artifact**——每个 artifact 都触发下游。

如果你的原 phase A 是 "agent 干完一堆事，最后写一个汇总 report"，
V0.4.0 下要决定：

- 仍然只写一个 report → 下游 watch 看到 report 出现就跑（语义不变）
- 改成每完成一个子任务就写一个 artifact → 下游可以并行（推荐）

### 5.2 顺序保证消失

V0.3.x：phase A → B → C，保证 B 看到 A 完整产物。
V0.4.0：A 写 file1，B1 启动；A 又写 file2，B2 启动。**B1 和 B2 之间没有
顺序保证**，且都不等 A 完成。

如果你的 workflow 依赖严格顺序（如 build → test → deploy 必须串行），
有两种处理方式：

1. **保持串行**：把每个 phase 的 parallelism 设为 1，下游 trigger 看
   "summary" 文件而非 "intermediate" 文件——agent 内部完成所有子任务
   后再写 summary
2. **用 Gate 控制**：在需要串行边界处插一个 `trigger: gate` agent，
   meta-agent 决定何时解锁

### 5.3 Context 管理

V0.3.x：phase 边界 orchestrator 决定是否 reset CC session（`/exit` +
新 session）。
V0.4.0：每个 agent session 是独立的 `claude --bg --agent <role>`，
**天然独立 context**。不需要 orchestrator 干预 context reset——每次
spawn 都是新 session。

这意味着：

- 原 phase 间 "share context" 的隐式假设失效 → 必须通过 artifact 文件
  显式传递信息
- 不再需要 `ccteam doctor --check-cache-cost` 之类的 phase 边界优化
- agent prompt 不要假设"上一轮"的状态——总是从 artifact 读

---

## 6. 边角 case + 已知限制

### 6.1 V0.3.x 用了 `inject_directives` / `golden_rules` 的项目

原 phase 配置可能有：

```yaml
phases:
  - name: plan-eng
    prompt: "..."
    inject_directives:
      - "always use cargo test --workspace"
      - "escalate on third failure"
    golden_rules:
      - "no global state"
```

V0.4.0 这些字段不再注入。自动迁移**不会**自动把这些规则 inline 到
agent prompt——你必须手动决定：

- 用户级规则 → 写到 `~/.claude/CLAUDE.md`（所有项目共享）
- 项目级规则 → 写到 `<project>/CLAUDE.md`（Claude Code 自动读取）
- 单 agent 级规则 → inline 到 `.claude/agents/<role>.md` 的正文

### 6.2 V0.3.x 用了 `decision_mode: strict` 的项目

原 phase 通过 `decision_mode` 强制 agent 输出严格 schema。V0.4.0
没有这个机制——需要在 agent prompt 里显式写出 schema 要求，并在
artifact 文件名 / 内容上加 lint 步骤（reviewer agent 检查）。

### 6.3 跨项目记忆（M4）

V0.3.x M4 跨项目记忆通过 `~/.claude/rules/ccteam-lessons-<team>.md` 实现
（详 tech-design §3.7）。V0.4.0 **不变**——meta-agent 通过 Claude Code
原生 `/memory` 命令读写，ccteam-core 零检索代码。这条迁移路径上不需
要做任何事。

### 6.4 meta-agent 自身

V0.3.x 的 meta-agent session 仍然由 ccteam 起（`<handle>-meta` 后缀路径）。
V0.4.0 这个机制**不变**。但 meta-agent 的 prompt 需要更新——
新增 7 个 MCP 工具的说明：

```bash
ccteam doctor --update-meta-agent
```

这会更新所有 meta-agent 项目的 `~/.claude/CLAUDE.md` 中 ccteam 工具列表段
（marker section `<!-- ccteam-managed:mcp-tools begin/end -->` 内）。

### 6.5 flex 模式项目

V0.3.x 中 `team.yaml::kind: flex` 模式（多 session 自由编排）在 V0.4.0
被 `workflow.yaml` 完全取代——`kind: flex` 的语义重写为：
"加载 workflow.yaml 而非走 phase DAG"。已有 flex 项目影响：

- `state.json::sessions{}` 字段保留（用于 session id 分配）
- `~/.ccteam/progress/<slug>/<sid>.jsonl` per-session 路径保留
- 旧 flex CLI（如 `ccteam ctl flex-new`）替换为 `ccteam ctl spawn-agent`

自动迁移工具会把 flex 项目识别为"已是 workflow"状态，跳过转换，
但仍需要你手动写 `workflow.yaml`（自动迁移不会生成无 phase 的项目的
workflow.yaml）。

---

## 7. 迁移后验证

```bash
# 1. 验证 workflow.yaml schema
ccteam ctl validate-workflow --slug <slug>
# 期望：no errors

# 2. 验证所有 agent role 文件存在
ccteam doctor --check-workflow --slug <slug>
# 期望：all agent role files present

# 3. dry-run（不实际 spawn，只验证 trigger 注册）
ccteam run --dry-run --slug <slug>
# 期望：watcher 注册成功，无 error

# 4. smoke 测试 — manual trigger
ccteam ctl spawn-agent --slug <slug> --role <first-role>
ccteam ctl observe --slug <slug>
# 期望：可以看到 session 启动

# 5. 端到端测试 — 一轮跑通
# 用最低 parallelism 跑一遍，确认 artifact 流转 + Gate 解锁正常
```

确认全部 OK 后，删除 `team.yaml.v0-3-bak` 备份。

---

## 8. 求助

迁移过程中遇到问题：

1. 先查 [`user-manual.md`](user-manual.md) §10 故障排查 FAQ
2. 看 `~/.ccteam/logs/<slug>.log` orchestrator 日志
3. 看 [`prd.md`](prd.md) §9 设计层面对迁移路径的论证
4. 如果是 ccteam 本身 bug → 在仓库提 issue，附 `team.yaml`（脱敏后）+
   `ccteam doctor --version` 输出 + 错误日志

---

## 9. 进一步阅读

- [`user-manual.md`](user-manual.md) — V0.4.0 完整用户手册
- [`prd.md`](prd.md) §9 — 迁移路径的设计论证
- [`prd.md`](prd.md) §5 — 删除清单（被删的代码 ~3500 LOC 细目）
- [`README.md`](README.md) — V0.4.0 文档索引
- [`../../examples/workflows/`](../../examples/workflows/) — 可参考的示例 workflow
