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
decision_mode: hybrid
max_clarify_rounds: 3
---

# 任务:技术规划

ccteam 是**高质量软件元开发系统**,不是"凑合能跑就行"的玩具——这个阶段
的产出会直接驱动后续 implement / test / fix。规划质量决定终产物质量。

## 跨项目经验(开始规划前先看)

`~/.claude/rules/ccteam-lessons-dev.md` 在 session 启动时已自动加载到上下文。
**先扫一遍**,尤其 `do_not_do_again` 段——别把上一个项目踩过的坑当成新发现重新来一遍。

需要深挖某个 topic 时:
- `/memory` 浏览本仓 auto-memory
- `Read ~/.claude/projects/<encoded>/memory/<topic>.md` 直接读 topic 文件

**可选**:如果工具列表里出现 `mcp__*claude-mem*search` 之类工具(用户装了
[claude-mem](https://github.com/thedotmack/claude-mem)),可以调它做跨项目语义检索;
没有就跳过,默认机制已够用。

## 当 spec 内容不足以严肃规划时(例:只有几个字、需求模糊、关键约束缺失)

**不要**自己脑补一份需求继续做下去——脑补出来的需求做出来的软件不是
用户要的软件,这是把 ccteam 降级成玩具的最快路径。

正确做法是把缺失的关键事实**单独列成澄清问题清单**反馈给用户,例如:

> spec.md 仅含 "mdeditor",无法做技术选型。需澄清:(1) 目标平台
> 是 CLI / TUI / Web / Desktop 哪一种?(2) 目标用户是开发者还是普通文字
> 工作者?(3) 核心场景是只读预览、本地编辑、还是协同编辑?(4) 需不需要
> Markdown 扩展语法(数学公式 / 流程图 / mermaid)?(5) 性能 / 体积 / 离
> 线 等关键约束?

用户读到澄清后会补全 spec.md,本阶段恢复继续做。

## 当 spec 足够严肃时,产出:

### 1. `plan-eng.md` 内容要点
- 技术栈选型(语言 / 框架 / 关键库)——**每条都要写选型理由**(性能、生
  态成熟度、与 spec 约束的匹配度);如果有显著替代方案,写为什么不选
- 核心数据结构与对外接口(函数/类型签名级,不是"大概有个 X 模块")
- 任务拆分(每条 ≤ 4 小时;可并行的注明)
- 已知技术风险与缓解方案(至少 2 条:不是"风险不大"这种敷衍)

### 2. `architecture.md` 内容要点
- 模块图(组件、依赖方向、边界)
- 关键流程(2–3 个核心场景的时序图或步骤描述,需对得上 spec 的核心场景)
- 失败模式分析(关键路径上每一步可能怎么坏,系统怎么恢复或暴露错误)

## 与用户对齐:review-with-user 循环

`plan-eng.md` 与 `architecture.md` 写完后,**先用 review-with-user 循环跟用户对齐**——
spec 中模糊的地方现在没澄清,implement 阶段会撞上同样的问题,代价更高。

按以下模板的步骤跑(`@` 引用直接 inline,Claude Code 原生机制;orchestrator 不参与解析):

@~/.ccteam/templates/review-with-user-loop.md

settled review 写到 `.ccteam/plan-eng-review.md`(供 implement 阶段读取)。
