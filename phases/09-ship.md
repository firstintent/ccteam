---
name: ship
required_inputs:
  - .ccteam/test-report.md
  - .ccteam/implement-report.md
required_outputs:
  - .ccteam/retro.md
soft_cost_warn_usd: 2.0
stall_warn_minutes: 5
parallelism: solo
sub_skills: []
---

# 任务:收尾交付

按顺序:

1. 运行最终测试一次,确认全绿(否则 `ESCALATE`)
2. 创建 git commit(message 简洁说明本项目做了什么)
3. 写 `.ccteam/retro.md`,30 行内回顾:
   - 实际花费 vs plan-eng 估算
   - 关键技术决策(后续项目能复用的)
   - 踩过的坑(不要再做的事)
   - 给跨项目记忆的"建议复用 / 不要再做"摘要
4. 若项目有 README 需求则生成

最后一行:`PHASE_DONE: ship`(或 `ESCALATE: <一句话原因>`)。
