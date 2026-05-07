---
name: differentiation-analysis
required_inputs:
  - .ccteam/brief.md
  - .ccteam/market-survey.md
required_outputs:
  - .ccteam/differentiation.md
parallelism: solo
soft_cost_warn_usd: 1.5
stall_warn_minutes: 5
decision_mode: hybrid
max_clarify_rounds: 3
---

# Differentiation Analysis — 差异化分析

读 `@.ccteam/brief.md` 与 `@.ccteam/market-survey.md`,回答:**这个 idea 与现有方案相比,差异化在哪里?差到能让用户切换吗?**

## 必答的三件事

### 1. 与每个直接竞品的差异点

对 market-survey 列的每个直接竞品,写一行:

```
| 竞品 | 我们独特的地方 | 对用户的实际价值 |
```

**"独特"的判定门**:用户为了这点会主动切换吗?如果只是"我们的颜色更好看",那不算差异。

### 2. 差异化的可持续性

差异化能保持多久?

- **垄断式**:数据 / 网络效应 / 监管牌照 → 长期可持续
- **执行式**:更好的 UX、更快迭代 → 短期(6-12 月)可持续
- **没有**:任何竞品想做都能做,只是没做 → 不可持续

### 3. "用户为什么选我们"的一句话

20 字以内回答:用户为什么选这个方案,而不是 market-survey 列的任一现有方案?

如果答不出来 / 答案像"功能更全"这种空话 → 差异化 == 0,触发 LOW_DIFFERENTIATION。

## 退出

差异化清楚:

```
PHASE_DONE: differentiation-analysis
```

差异化不存在或仅是"颜色不同"级别的伪差异:

```
ESCALATE: LOW_DIFFERENTIATION — 现有方案已覆盖核心需求,无可持续差异
```

(此 prefix 由 team.yaml 注册;路由 revert_to_phase → kickoff,让用户重新审视 brief。差异化不存在通常意味着 idea 切入点不对,需要换角度而不是继续推进。)
