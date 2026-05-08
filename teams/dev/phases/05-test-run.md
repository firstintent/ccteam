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

`test-report.md` 内容要点:

- 总用例数 / 通过 / 失败 / 跳过
- 每个失败用例的简短摘要(下 phase 修复用)
- 测试运行时长(可选)

判定分流:

- **全绿** → 正常完成,phase 收尾
- **有失败** → 仍正常完成本阶段,orchestrator 状态机会自动转 fix-cycle
- **测试套件本身崩溃**(无法判定通过/失败)→ 走异常出口,reason 描述崩溃原因
