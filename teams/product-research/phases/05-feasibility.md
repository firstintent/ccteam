---
name: feasibility
required_inputs:
  - .ccteam/brief.md
  - .ccteam/market-survey.md
  - .ccteam/differentiation.md
  - .ccteam/value-prop.md
required_outputs:
  - .ccteam/feasibility.md
parallelism: solo
soft_cost_warn_usd: 1.5
stall_warn_minutes: 10
decision_mode: async
max_clarify_rounds: 3
---

# Feasibility — 可行性评估

读所有上游产物,评估 **构建这个 idea 在技术 / 资源 / 时间维度的可行性**。

## 评估维度

### 1. 技术可行性

- 核心技术栈(粗:语言 / 主要框架 / 关键依赖)
- 高风险点:有没有"能不能做出来"层面的不确定?
  - 例:依赖 LLM 长上下文 + 低延迟,而当前 API 不支持
  - 例:需要离线运行但模型大小超手机存储
- 替代路径:如果主路径技术不成立,有 plan B 吗?

### 2. 资源可行性

- 团队人月估算(开发 / 运维 / 内容)
- 数据 / 内容获取成本(如果产品依赖外部数据)
- 持续运行成本(LLM 调用费 / 服务器 / 第三方 API 月度账单)

### 3. 时间可行性

- 从立项到 v0.1 可发布(给真实用户用)的预估时长
- 关键里程碑(M1 / M2 / M3)各自标志性产物

## decision_mode: async — 用户离线时的处理

本 phase 用 `async` 模式 (interfaces §5.6.1):

- 如果碰到关键决策点(例:技术选型 A vs B,影响 1 周以上工作量),写 outbox `reply-<ts>-<seq>.md`(YAML frontmatter `event_kind: clarify`),**不阻塞**继续做能做的事
- 如果剩余工作全部依赖该决策 → ESCALATE `PHASE_DONE_PENDING`,把 outbox 文件名列入 reason

outbox 路径:`<project>/.ccteam/outbox/reply-<ts>-<seq>.md`(schema 见 interfaces §3.4.3 — 文件名前缀是 `reply-`,event_kind 在 frontmatter 区分)

## 退出

正常完成:

```
PHASE_DONE: feasibility
```

部分完成 + 待用户决策(M3.6 PHASE_DONE_PENDING):

```
ESCALATE: PHASE_DONE_PENDING — reply-<ts>-001.md (技术选型决策待 user)
```

(orchestrator 看到该 ESCALATE 后切 PhaseState::DonePending,下 phase verdict 启动时检查依赖,有则 block。)

无法在合理范围内拿到关键技术信息:

```
ESCALATE: INSUFFICIENT_VALIDATION — 列具体哪些技术不确定性 + 已尝试的途径
```
