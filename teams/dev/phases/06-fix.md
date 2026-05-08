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
---

# 任务:修 bug(fix-cycle)

读 `test-report.md` 中的失败用例,逐一修复。

约束:

- 不要为了让测试过而删测试或加 `if false`
- 修改实现而非测试,除非测试本身写错(那种情况必须在 fix-report 里说明并保留旧测试为参考)
- fix-cycle 上限由 orchestrator 强制 3 轮;第 3 轮仍未全绿会自动 escalate

`fix-report.md` 内容要点(每轮都写):

- 本轮处理的失败用例
- 改了哪些文件、为什么这么改
- 仍未修复的(下轮重试或 escalate)

判定分流:

- **本轮全绿**:正常完成,orchestrator 推进下一相位
- **仍有失败但 < 3 轮**:写本轮 fix-report 后等待 Stop hook 重喂 prompt 进下一轮 fix-cycle
- **第 3 轮仍失败**:orchestrator 自动 escalate(由 auto_loop 上限触发);若用户手动需要走异常出口,reason 写"fix-cycle 已 N 轮未通过"
