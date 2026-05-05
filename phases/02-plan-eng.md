---
name: plan-eng
required_inputs:
  - .ccteam/spec.md
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

- `@.ccteam/spec.md` —— 用户需求

(M0 没有产品规划阶段;plan-ceo / plan-pm 等上游产物在后续里程碑加入。
只用 spec.md 做技术选型即可,不要因为缺其它文件 escalate。)

如果 `spec.md` 内容明显不足以做技术规划(例:只有几个字、需求模糊),
请先在 spec 上**自行扩写一份合理的 v0 解读并写回 `.ccteam/spec.md`**,
再继续做技术规划——不要 escalate 到用户。

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
