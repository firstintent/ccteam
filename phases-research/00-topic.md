---
name: topic
required_inputs:
  - .ccteam/brief.md
required_outputs:
  - .ccteam/topic.md
soft_cost_warn_usd: 1.5
stall_warn_minutes: 5
parallelism: solo
sub_skills: []
tools_required:
  subagents: [general-purpose]
  skills: []
  mcp: []
---

# 任务:topic 收敛

> **research 团队第一 phase**——把用户提的 brief 收敛成"**决策问题**"。
> dev 团队对应位置是 plan-ceo;但 research 输出的不是产品规划,而是
> "这个研究将驱动什么具体决策"。

## 研究的纪律 — 不要被 brief 牵着走

ccteam 是高质量元开发系统,research 同样不是"凑个报告就行"。一个
**没人会用结论做决策**的研究,无论数据多漂亮都是浪费——topic phase
的核心动作是**把模糊好奇心钉成可决策的问题**。

## 输入

- `@.ccteam/brief.md` —— 用户提的原始 brief

## 当 brief 不足以严肃定题时

**不要**自己脑补一份 topic 直接往下做——脑补的 topic 跑出来的研究,用
户读了一句话就发现"不是我想问的"。

正确做法是 escalate,带具体澄清问题清单。例:

> ESCALATE: NEED_USER_INPUT — brief "为什么 PWA 用户留存抬头" 不足以定题,
> 需澄清:(1) 留存抬头是 D1/D7/D30 哪一档?(2) 这个研究输出后用户会做
> 什么决策——重构缓存 / 加灰度 / 改通知?(3) 已有的 web vitals /
> Sentry / 客服反馈是否要纳入 desk 调研?(4) 受访用户能不能联系到——
> 有 N=多少的样本可用?(5) 这次研究的预算 / 时间窗口?

## 当 brief 足够严肃时,产出 `.ccteam/topic.md`

字段顺序固定:

### 1. 决策问题(必填)
- **形如**:"这个研究的结论会驱动 < 谁 > 在 < 何时 > 做出 < 什么决策 >?"
- 示例:"产品团队在 2026 Q3 OKR 锁定前决定:是否把'离线缓存重构'
  纳入下个季度路线图。"
- **不能写成**:"我们想了解用户为什么 X"——这是好奇心,不是决策问题。

### 2. 研究问题(可证伪化的子问题,3–5 条)
- 每条形如:"在 <场景> 下,<现象> 是由 <某机制> 还是 <另一机制> 导致?"
- **现在不写假设**(假设是 02-hypothesis 的事);这里只把决策问题拆成
  可分头研究的子问题。

### 3. 范围声明(scope-watcher 后续 phase 会比对)
- **包括**:本研究覆盖的现象 / 用户群 / 时段 / 平台
- **排除**:**显式声明本研究不回答什么**(避免后续 phase scope drift)
  - 反例:不写"不研究商业模式"——后续 desk 阶段就可能跑到商业分析去
- 示例:"覆盖 PWA 离线模式启用过的 D7-D30 用户;排除从未启用 PWA 的
  用户、企业版账号、后端 API 性能问题"。

### 4. 已知信息基线(前置已知,避免重复挖)
- 简列已知的事实(从 brief / 仓库已有数据 / 已有报告)
- 如果几乎为空 → 标注 "01-desk phase 重负载"

### 5. 成功标准
- 写"这份研究在什么条件下算成功"——不是"产出报告"(那是输出),而
  是"决策问题被回答到 < 何种确定性 > 程度"。例:"H1/H2/H3 三条候选
  解释中至少一条经多源数据支撑达成共识"。

## 验证(自检)

写完 topic.md 后**必须**用以下 prompt 启 subagent 自审一次:

```
请用 Task 工具,subagent_type="general-purpose",
description="topic-critic on this research's decision question",
prompt="读 @.ccteam/topic.md,严格扮演一名 research 方法学 critic,
检查:(1) 决策问题是否包含具体的 < 谁 / 何时 / 何决策 > 三要素?
(2) 研究子问题是否可以通过观察 < 任何具体证据 > 来回答(可证伪
方向性)?(3) 排除声明是否真的排除了——还是写成了'不限制'这种
空话?(4) 成功标准是否可独立判定 vs 需要后续主观裁量?
对每条不通过给一句具体修改建议。如果三个以上不通过,输出
'TOPIC_CRITIC_BLOCK',否则 'TOPIC_CRITIC_PASS'。"
```

如果输出 BLOCK → 改 topic.md → 重审一次 → 仍 BLOCK 则:

> ESCALATE: NEED_USER_INPUT — topic 自审两轮未通过,具体卡点:< 摘 critic 输出 >

## 收尾

完成后最后一行单独输出:

- 顺利:`PHASE_DONE: topic`
- 任意 escalate 路径:`ESCALATE: <prefix> — <reason>`(prefix 见
  `phases-research/README.md` ESCALATE grammar 表)

## 与 dev plan-ceo 的差异(读模板时的认知校准)

| 维度 | dev plan-ceo | research topic |
|---|---|---|
| 主产物 | 产品规划(做什么、什么形态) | 决策问题(谁会用结论做什么决策) |
| 失败模式 | "做了用户不要的功能" | "出了没人用的研究" |
| escalate 触发 | spec 模糊 | brief 不指向具体决策 |
| 完成信号(M4.5+) | (沿用 PHASE_DONE) | `TOPIC_SCOPED` |
