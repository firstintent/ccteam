---
name: hypothesis
required_inputs:
  - .ccteam/topic.md
  - .ccteam/desk.md
required_outputs:
  - .ccteam/hypotheses.md
soft_cost_warn_usd: 2.0
stall_warn_minutes: 5
parallelism: solo
sub_skills: []
tools_required:
  subagents: [general-purpose]
  skills: []
  mcp: []
---

# 任务:假设设定

> 把 desk 暴露的"信息缺口" 转成**可证伪的假设**——一手数据收集的目标。

> dev 等价物**没有**——dev 的 plan-eng 是"做什么"的设计;research 的
> hypothesis 是"信什么"的预设。两者都用 plan-eng 字面承担会失去 critical
> 分别。

## 输入

- `@.ccteam/topic.md` —— 决策问题 + 子问题
- `@.ccteam/desk.md` §3 候选答案 vs 信息缺口、§4 给 02-hypothesis 的输入

## 假设的硬性要求

每条假设必须**显式满足**以下 4 条,缺一不可:

1. **可证伪性**:存在一组在 04-primary 可观测到的数据,如果观察到该组
   数据,则该假设被推翻
2. **方向性**:假设说的是"X **导致** Y"或"X **不影响** Y",不能是
   "X **可能** Y"——可能性陈述不可证伪
3. **与决策耦合**:假设的真假直接影响 topic.md §1 的决策。无关假设
   即使有趣也不写
4. **独立性**:多条假设之间不能"恰好都真"或"恰好都假"——要存在某
   种数据能区分它们

## 推荐的 subagent 调用 — falsifiability critic

写完假设草稿**必须**走一遍这步:

```
请用 Task 工具,subagent_type="general-purpose",
description="hypothesis-falsifiability critic",
prompt="读 @.ccteam/hypotheses.md。逐条假设审问:
(1) 写出能推翻该假设的一组观测——至少给出一种具体证据形态(数字 /
访谈 quote / 日志模式);
(2) 这组观测能在 04-primary phase 用什么具体方法采到?
(3) 如果两条假设互相矛盾,哪条数据能优先区分它们?
逐条标 PASS / WEAK_FALSIFIABILITY / NOT_FALSIFIABLE。
若 ≥1 条 NOT_FALSIFIABLE,输出 'CRITIC_BLOCK';否则 'CRITIC_PASS'
+ 一句最值得改的具体建议。"
```

CRITIC_BLOCK 时:**不要**为了过关把假设写软("X 可能 Y")——那是退化
成 desk。要么改成方向性更明确的假设,要么承认"现有方法采不到证伪
数据"并:

> ESCALATE: METHOD_INSUFFICIENT — 假设 H<n> 不可证伪,因为 < 具体方法
> 障碍 >。需要 [A] 改假设方向 [B] 引入新方法采集通道 [C] 删除该假设。

## 产物 `.ccteam/hypotheses.md`

每条假设固定结构:

```
## H<n> — <假设的一句话表述,带方向>

**形式**:< 在 X 场景下,Y 由 Z 机制导致 > / < 在 X 场景下,Y 与 Z 无关 >

**驱动的决策**:< topic.md §1 决策问题中的哪一面 >

**证伪条件**(可观测形式):
- 数字:< 具体阈值 / 比例 / 分布形态 >
- 行为:< 具体观察到的用户行为模式 >
- 反证质性证据:< 受访者会说什么样的话 >

**采集方式预想**:< 03-method phase 应当用什么方法采到证伪数据 >

**与其它假设的区分点**:< 哪条数据让 H<n> 与 H<m> 不再同时为真 >
```

## 假设数量

- 至少 3 条,最多 5 条
- < 3 条:研究信息含量低 → `ESCALATE: NEED_USER_INPUT — 子问题只能
  抽出 < 3 条独立假设,要不要扩 topic / 接受研究规模较小?`
- > 5 条:精力分散 → 自动收窄,在 hypotheses.md 末尾列被舍弃的假设 +
  舍弃理由

## 与 dev fix-loop 语义对照(关键认知)

dev 的 fix-loop 是"测试失败 → 改代码 → 测试通过"——目标是让代码满足
预设规约。

research 的"假设被反驳"**不是 fix**——它本身就可能是研究的核心结论
("我们以为是 H1,数据证明 H1 是错的,真实原因是 H2 反向")。所以:

- 04-primary 收到反驳数据时,**不会**触发 ralph 自循环改假设
- 而是 05-synthesis 把"H<n> 被反驳"作为一条 insight 录入
- 真正需要"回头改假设"的场景是:**desk 阶段就被反驳的假设(说明
  hypothesis 写得太松,本不该立)**——这时走 `ESCALATE:
  HYPOTHESIS_REJECTED — REVERT_TO_PHASE 02-hypothesis`

这个区别让 02 的 critic 维度比 dev 的 review 严格——必须在写假设时就
预想证伪条件,否则 04-primary 会浪费一手数据预算。

## 收尾

完成后最后一行:`PHASE_DONE: hypothesis` 或 `ESCALATE: <prefix> — <reason>`。
完成信号(M4.5+)= `HYPOTHESES_SET`。
