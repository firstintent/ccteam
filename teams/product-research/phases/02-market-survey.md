---
name: market-survey
required_inputs:
  - .ccteam/brief.md
required_outputs:
  - .ccteam/market-survey.md
parallelism: solo
soft_cost_warn_usd: 2.0
stall_warn_minutes: 10
decision_mode: hybrid
max_clarify_rounds: 3
---

# Market Survey — 市场调研

读 `brief.md`,系统调研这个 idea 当前的市场状态。

## 调研维度(每个都要在产物里覆盖)

1. **现有竞品 / 替代品**
   - 至少 3 个直接竞品(同类工具)+ 3 个间接替代(用户怎么"将就")
   - 每个标注:免费 / 付费 / 开源 / 商业,定价档位,核心定位
2. **市场饱和度**
   - 大致用户量级(粗估:百人 / 千人 / 万人 / 百万级)
   - 增长态势(新进者多吗?巨头是否在做?)
3. **用户痛点真实性**
   - 现有方案的差评 / 抱怨样本(stack overflow / reddit / app store reviews / github issues)
   - 抱怨强度:无关紧要 / 偶尔提 / 频繁吐槽 / 痛到付费
4. **进入壁垒**
   - 需要数据集吗?需要法规许可吗?需要规模效应吗?

## 数据来源要求

- **至少 3 个独立信息源**(不是同一个文章被多处转载)
- 信息源记录在产物的"参考"节(URL + 一句话内容摘要)
- **不要**编造数据;不确定时标 "未确认" 或主动 outbox 问用户

## 关键决策门(关键!)

读完调研结果后,自检:

- 如果发现 ≥3 个免费且广泛使用的同类工具完全覆盖了核心场景 → 走 `MARKET_DUPLICATE` 异常出口(team.yaml 注册;route abort,项目终态)
- 如果搜不到竞品但用户痛点明确 → 这是 contrarian opportunity 信号,在产物里高亮

## 异常分流(domain-specific)

- 数据收集严重不足且 3 轮 clarify 无法补全 → `INSUFFICIENT_VALIDATION` prefix(team.yaml 注册;route need_user_input),reason 描述缺哪些维度、为什么 3 轮没解决
- 市场已被免费工具完全覆盖 → `MARKET_DUPLICATE` prefix,reason 列 N 个免费替代品 + 概述覆盖度
