---
name: test-author
required_inputs:
  - .ccteam/implement-report.md
required_outputs:
  - .ccteam/test-author-report.md
soft_cost_warn_usd: 5.0
stall_warn_minutes: 5
parallelism: solo
sub_skills: []
---

# 任务:测试编写

读取 `@.ccteam/implement-report.md` 与代码,为每个核心模块写测试。覆盖:

- 正常路径(happy path)
- 边界条件(空输入、上 / 下界、并发)
- 失败路径(预期错误必须可重现)

不允许 mock 关键外部依赖(数据库 / 文件系统 / 外部 API 用 fixture,避免"测试绿但生产挂"的假绿)。

完成后写 `.ccteam/test-author-report.md`:

- 新增测试文件 + 用例数量
- 选择 fixture vs mock 的判断依据
- 仍未覆盖的边界(明确遗留给 review 关注)

最后一行单独输出 `PHASE_DONE: test-author`(或 `ESCALATE: <一句话原因>`)。
