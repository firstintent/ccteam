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
sub_skills: []
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

写完 implement-report 后,**必须**调起 plugin 级 reviewer 自检——这是 ccteam
"测试之外再加一层 review"的最小落地(见 requirements §11)。

请用 Task 工具(注意是 Agent 调度工具,不是 TaskCreate 任务管理工具),传:

- `subagent_type="code-reviewer"`
- `description="self-review HEAD diff"`
- `prompt="审查本轮 implement 阶段对仓库的全部改动(`git diff` 与新建文件)。
   按 critical / major / minor 三档列问题。最终把完整 review 内容写入
   `.ccteam/code-review.md`(覆盖任何旧内容),让 ship 阶段可读。
   只看代码质量,不重复跑测试。"`

review 触发后会发出 `SubagentStop` hook → orchestrator 据此确认本轮工具触发面
打通。**不要**在 review 没跑就写 PHASE_DONE。

最后一行单独输出 `PHASE_DONE: implement`(或 `ESCALATE: <一句话原因>`)。
