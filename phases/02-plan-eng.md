---
name: plan-eng
required_inputs:
  - .ccteam/spec.md
  - .ccteam/plan-ceo.md
required_outputs:
  - .ccteam/plan-eng.md
  - .ccteam/architecture.md
soft_cost_warn_usd: 3.0
stall_warn_minutes: 5
parallelism: solo
sub_skills: []
---

# 任务:技术规划

读取上游产物:

- `@.ccteam/spec.md` —— 用户需求(经 Seed 澄清后版本)
- `@.ccteam/plan-ceo.md` —— 产品规划

产出物:

## 1. `.ccteam/plan-eng.md`
- 技术栈选型(语言 / 框架 / 库)与选型理由
- 核心数据结构与接口
- 任务拆分(每条 ≤ 4 小时;可并行的注明)

## 2. `.ccteam/architecture.md`
- 模块图(组件间的关系)
- 关键流程(2–3 个核心场景的时序图或步骤描述)
- 已知风险与应对

完成后请在最后一行单独输出 `PHASE_DONE: plan-eng`(或 `ESCALATE: <一句话原因>`)。
