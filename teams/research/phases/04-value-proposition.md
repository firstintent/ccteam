---
name: value-proposition
required_inputs:
  - .ccteam/brief.md
  - .ccteam/market-survey.md
  - .ccteam/differentiation.md
required_outputs:
  - .ccteam/value-prop.md
parallelism: solo
soft_cost_warn_usd: 1.5
stall_warn_minutes: 5
decision_mode: hybrid
max_clarify_rounds: 3
---

# Value Proposition — 价值主张

读 brief / market-survey / differentiation,综合写一份**价值主张**说明:用户为什么会用,以及为什么会持续用。

## 必有章节

### 1. 一句话价值主张

格式:**对 \<目标用户\>,这个工具帮你 \<解决某具体问题\>,与 \<现有替代\> 的区别在 \<差异点\>**。

- 主语必须是用户,不是产品(不要写"这是一个 X 应用")
- 解决的问题必须从 brief 的"用户原始一句话"链回到 market-survey 的"用户痛点真实性"
- 差异点必须是 differentiation.md 列出的可持续差异

### 2. 用户旅程(Day 1 / Week 1 / Month 1)

- **Day 1**:用户首次接触,什么场景,做什么动作?
- **Week 1**:用户用了 5-7 次后,什么习惯形成?
- **Month 1**:用户还在用吗?为什么?(retention 来源)

### 3. 用户群体的层次

不是所有目标用户价值都一样:

- **核心用户**:每天 / 每周必用,愿意付费 / 强烈推荐
- **辅助用户**:偶尔用,锦上添花
- **看客**:听说过但用不上

写每层用户的占比估算 + 总用户量级。

### 4. 价值的脆弱点

什么变化会让这个价值消失?(技术变化 / 用户偏好转移 / 竞品行动 / 监管)

## 决策

`value-prop.md` 末尾必含一个判断行:

```
价值强度: weak | moderate | strong
```

- **weak**:看起来 nice-to-have,核心用户也只是 occasional 用,容易被替代
- **moderate**:有清楚的核心用户群,Month 1 retention 估计能到 30-50%
- **strong**:核心用户痛点强,Month 1 retention 估计 50%+,差异化可持续

本 phase 不预期触发团队特化异常出口 — 价值弱归弱,不一定是项目 abort 信号,
留给 verdict phase 综合判断。
