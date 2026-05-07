---
name: test-run
required_inputs:
  - .ccteam/test-author-report.md
required_outputs:
  - .ccteam/test-report.md
soft_cost_warn_usd: 2.0
stall_warn_minutes: 10
parallelism: solo
sub_skills: []
---

# 任务:跑测试

执行项目的测试套件(语言惯例:`cargo test` / `pnpm test` / `pytest` 等)。捕获完整输出。

写 `.ccteam/test-report.md`:

- 总用例数 / 通过 / 失败 / 跳过
- 每个失败用例的简短摘要(下 phase 修复用)
- 测试运行时长(可选)

判定分流:

- **全绿** → 最后一行 `PHASE_DONE: test-run`
- **有失败** → 最后一行 `PHASE_DONE: test-run`(orchestrator 状态机转 fix-cycle)
- **测试套件本身崩溃** → `ESCALATE: test suite crashed: <原因>`
