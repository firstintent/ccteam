---
name: implement
required_inputs:
  - .ccteam/plan-eng.md
  - .ccteam/architecture.md
required_outputs:
  - .ccteam/implement-report.md
soft_cost_warn_usd: 10.0
stall_warn_minutes: 5
parallelism: solo
sub_skills: []
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

最后一行单独输出 `PHASE_DONE: implement`(或 `ESCALATE: <一句话原因>`)。
