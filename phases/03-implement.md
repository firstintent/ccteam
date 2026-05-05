---
name: implement
required_inputs:
  - .ccteam/plan-eng.md
  - .ccteam/architecture.md
required_outputs:
  - .ccteam/implement-report.md
  - .ccteam/code-review.md
soft_cost_warn_usd: 10.0
stall_warn_minutes: 5
parallelism: solo
sub_skills:
  - skill: claude-plugins-official:pr-review-toolkit/agents/code-reviewer.md
    trigger: phase_done
    output_to: .ccteam/code-review.md
tools_required:
  subagents:
    - code-reviewer
---

# 任务:代码实现

读取上游产物:

- `@.ccteam/plan-eng.md` —— 任务拆分清单
- `@.ccteam/architecture.md` —— 模块图与关键流程

按 plan-eng 任务清单逐项实现。约束:

- 不引入未在 plan-eng 中声明的新依赖
- 每个文件保持单一职责;避免巨型函数
- 关键边界条件写清楚断言或防御代码

完成后写 `.ccteam/implement-report.md`,概括:

- 已实现的任务清单(对照 plan-eng 勾选)
- 偏离原计划的地方(及原因)
- 已知遗留(留给 test / fix 阶段处理的)

## 自检:plugin 级 code-reviewer

写完 implement-report 后写 `PHASE_DONE: implement` —— **ccteam orchestrator
会在 phase_done 边界自动触发 plugin 级 reviewer 自检**(M2.1 sub-skill
自动调度,interfaces §7),把 `code-reviewer` 的输出写到
`.ccteam/code-review.md`,ship phase 自动 @ 引用。phase markdown 不需要
再手动 `Task(subagent_type="code-reviewer")`——orchestrator 已经接管。

如果你担心代码质量,在写 PHASE_DONE 前自查一下;orchestrator 的自动 review
是补充层而不是替代你自己的判断(见 requirements §11)。

最后一行单独输出 `PHASE_DONE: implement`(或 `ESCALATE: <一句话原因>`)。
