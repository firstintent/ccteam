---
name: fix
required_inputs:
  - .ccteam/test-report.md
required_outputs:
  - .ccteam/fix-report.md
soft_cost_warn_usd: 8.0
stall_warn_minutes: 5
parallelism: solo
sub_skills: []
auto_loop: true
auto_loop_max_iterations: 3
completion_signal: TESTS_GREEN
---

# 任务:修 bug(fix-cycle)

读取 `@.ccteam/test-report.md` 中的失败用例,逐一修复。

约束:

- 不要为了让测试过而删测试或加 `if false`
- 修改实现而非测试,除非测试本身写错(那种情况必须在 fix-report 里说明并保留旧测试为参考)
- fix-cycle 上限 3 轮(orchestrator 强制);第 3 轮仍未全绿 → 输出 `ESCALATE: fix-cycle 已 3 轮未通过`

完成本轮(无论是否全绿)后写 `.ccteam/fix-report.md`:

- 本轮处理的失败用例
- 改了哪些文件、为什么这么改
- 仍未修复的(下轮重试或 escalate)

最后一行:

- **本轮全绿** → `PHASE_DONE: fix`(orchestrator 转回 test-run 复跑确认)
- **仍有失败但 < 3 轮** → `PHASE_DONE: fix`(orchestrator 自动重入 fix-cycle)
- **第 3 轮仍失败** → `ESCALATE: <一句话原因>`
