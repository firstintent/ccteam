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

ccteam 是**高质量软件元开发系统**,不是"凑合能跑就行"的玩具——这个阶段
的产出会直接驱动后续 implement / test / fix。规划质量决定终产物质量。

## 输入

- `@.ccteam/spec.md` —— 用户原始需求

(M0 没有产品规划阶段;plan-ceo / plan-pm 等上游产物在后续里程碑加入。
只用 spec.md 做技术规划即可,不要因为缺其它文件 escalate。)

## 当 spec 内容不足以严肃规划时(例:只有几个字、需求模糊、关键约束缺失)

**不要**自己脑补一份需求继续做下去——脑补出来的需求做出来的软件不是
用户要的软件,这是把 ccteam 降级成玩具的最快路径。

正确做法是 **escalate 到用户,并在 escalate 原因里给出具体的澄清问题
清单**,例如:

> ESCALATE: spec.md 仅含 "mdeditor",无法做技术选型。需澄清:(1) 目标平台
> 是 CLI / TUI / Web / Desktop 哪一种?(2) 目标用户是开发者还是普通文字
> 工作者?(3) 核心场景是只读预览、本地编辑、还是协同编辑?(4) 需不需要
> Markdown 扩展语法(数学公式 / 流程图 / mermaid)?(5) 性能 / 体积 / 离
> 线 等关键约束?

用户会读到这条 escalate,补全 spec.md,再恢复本阶段。

## 当 spec 足够严肃时,产出:

### 1. `.ccteam/plan-eng.md`
- 技术栈选型(语言 / 框架 / 关键库)——**每条都要写选型理由**(性能、生
  态成熟度、与 spec 约束的匹配度);如果有显著替代方案,写为什么不选
- 核心数据结构与对外接口(函数/类型签名级,不是"大概有个 X 模块")
- 任务拆分(每条 ≤ 4 小时;可并行的注明)
- 已知技术风险与缓解方案(至少 2 条:不是"风险不大"这种敷衍)

### 2. `.ccteam/architecture.md`
- 模块图(组件、依赖方向、边界)
- 关键流程(2–3 个核心场景的时序图或步骤描述,需对得上 spec 的核心场景)
- 失败模式分析(关键路径上每一步可能怎么坏,系统怎么恢复或暴露错误)

## 收尾

完成后请在最后一行单独输出 `PHASE_DONE: plan-eng`(或 `ESCALATE: <一句话原因>`)。
ESCALATE 的"一句话原因"要具体,让用户读了就知道怎么补。
