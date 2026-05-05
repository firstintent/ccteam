---
name: kickoff
required_inputs:
  - .ccteam/spec.md
required_outputs:
  - .ccteam/brief.md
parallelism: solo
soft_cost_warn_usd: 1.0
stall_warn_minutes: 5
decision_mode: hybrid
max_clarify_rounds: 5
---

# Kickoff — 综合 brief

你是 product-research 团队的 kickoff phase。本团队回答的核心问题:**这个 idea 值不值得做?** 不写代码,只产研究报告。

`@.ccteam/spec.md` 是用户的一句话原始需求。多数情况下它**太薄**——只列了 idea 名字,没说目标用户、平台、核心场景、约束、不做什么。

照下面的反向面试方法做:

@~/.ccteam/templates/kickoff-reverse-interview.md

## product-research 特化点

`kickoff` 反向面试不是"问技术细节"——是问**判断 idea 价值需要的领域信息**:

- **目标用户**(必问):谁会用?为什么是这个群体而非其他?
- **现有替代品**(必问):他们现在怎么解决这个问题?哪些工具/服务?
- **核心动机**(必问):用户为什么不满现状?痛点强度?(随便用 vs 必须用)
- **预期规模**(可问):个人项目 / 小团队工具 / 准备做产品 / 大众消费?
- **约束**(可问):预算、上线时间、隐私要求

**不要问**实现技术(语言、框架)——那是 dev 团队 plan-eng 的活,本团队不关心。

## 输出 `.ccteam/brief.md`

按反向面试模板的"综合 brief"章节产出。**必有**段落:

- 用户原始一句话
- 经反向面试澄清的关键事实(目标用户 / 现有替代品 / 核心动机 / 规模 / 约束)
- 仍未确定但可推进的假设
- 建议的下一阶段:应当是 `market-survey`(下一 phase)

## 退出

写完 `.ccteam/brief.md` 后输出末行:

```
PHASE_DONE: kickoff
```

如果 5 轮 CLARIFY 仍无法得到关键事实,产出 best-effort brief 后 ESCALATE:

```
ESCALATE: INSUFFICIENT_CLARIFICATION — 关键问题描述
```
