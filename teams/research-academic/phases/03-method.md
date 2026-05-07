---
name: method
required_inputs:
  - .ccteam/topic.md
  - .ccteam/hypotheses.md
required_outputs:
  - .ccteam/method.md
soft_cost_warn_usd: 3.0
stall_warn_minutes: 5
parallelism: solo
sub_skills: []
tools_required:
  subagents: [general-purpose]
  skills: []
  mcp: []
---

# 任务:方法学设计

> 为每条假设选采集方法、设计样本框、定义工具(访谈提纲 / 问卷 / 实验
> 设计)。**这是 research 团队独有的 phase**——dev 没有等价物,因为代码
> 不需要"采样设计"。

## 为什么要单独一个 phase(不并入 02-hypothesis 也不并入 04-primary)

- **不并入 02-hypothesis**:hypothesis 关注"信什么";method 关注"怎么
  验证"。两件事用同一个 critic 评会模糊——method-critic 看的是
  appropriateness(方法配不配得上假设),hypothesis-critic 看的是
  falsifiability(假设本身可不可证伪)。
- **不并入 04-primary**:primary 一旦开跑就有真实成本(用户时间 / 受访
  者档期 / 调研工具费用)。method 写错跑下去 = 浪费真实预算。method
  必须先独立通过 critic 才进 primary。

## 输入

- `@.ccteam/topic.md` —— 决策问题 + scope
- `@.ccteam/hypotheses.md` —— H1..Hn(每条带"采集方式预想")

## 产物 `.ccteam/method.md`

固定章节:

### §1 方法清单(为每条假设独立选)

每条假设单独一节:

```
## H<n> 的采集方法

**方法**:< 深度访谈 / 半结构化访谈 / 在线问卷 / 日志分析 / A-B 实验 /
behavioral observation / diary study / ... >

**为什么是这个方法**(方法-假设匹配理由):
- 该方法能观察到 H<n> 证伪条件中的 < 具体证据形态 >
- 该方法在 < 时间 / 预算 / 伦理 > 约束内可行

**为什么不是别的**(列至少一条 alternative + 拒绝理由):
- alternative: < 另一方法 >
- 拒绝:< 时间 / 预算 / 不能采到证伪数据 / 伦理 >
```

### §2 样本框(只对一手数据相关的方法填)

```
- 总体定义:< 谁是这条假设关心的人群 — 边界明确 >
- 抽样方法:< 配额 / 滚雪球 / 用户列表随机 / 流量随机 >
- 目标 N:< 数字 + 与证伪条件的关系:为什么 N 这么多就够 >
- 排除条件:< 谁不在采样范围、为什么 >
- 招募通道:< Telegram / 邮件 / 客服 follow-up / 第三方 panel / 内部用户列表 >
- 激励方案:< 无 / 礼品卡 / 现金,标注合规性考虑 >
```

### §3 工具(对应方法的具体载体)

- **访谈类**:访谈提纲(开放式问题清单、不带引导;避免 leading questions)
- **问卷类**:问卷题目(标注必答、跳题逻辑、避免 double-barrelled
  question)
- **实验类**:metric 定义、对照组分配、最小可检测效应、停止条件

每条工具**写在 method.md 内 + 跟一句话说明该工具如何对应到对应假设的证伪
条件**。

### §4 时间线 + 预算

| 假设 | 方法 | 预计耗时 | 预计 token / API / 人力开销 | 排期(有外部依赖时标注) |

## 推荐的 subagent 调用 — method-critic + bias-watcher

写完 method.md 后**必须**两步审:

### 步骤 A:method-critic(配不配得上假设)

```
请用 Task 工具,subagent_type="general-purpose",
description="method-appropriateness critic",
prompt="读 @.ccteam/hypotheses.md 与 @.ccteam/method.md。逐条假设审:
(1) 该方法采到的数据能否真的让 H<n> 被推翻?(给一个具体场景:'如果
受访者说 < X >,H<n> 就被反驳吗?如果不被反驳,说明方法采的不是证
伪数据。')
(2) 样本框能否覆盖 H<n> 关心的人群?是否有抽样偏差(比如只问留下来
的用户)?
(3) 时间线是否真实(招募 + 收集 + 分析,不是只算访谈本身)?
逐条标 PASS / CONCERN / BLOCK。任意 BLOCK 输出 'METHOD_CRITIC_BLOCK'
+ 具体修改建议;否则 'METHOD_CRITIC_PASS'。"
```

### 步骤 B:bias-watcher(避免主动制造确认偏差)

```
请用 Task 工具,subagent_type="general-purpose",
description="bias and ethics watcher",
prompt="读 @.ccteam/method.md 的 §3 工具节(访谈提纲 / 问卷 / 实验
设计)。审:
(1) 是否有 leading question(暗示答案的提问)?给具体例子。
(2) 双管齐发问题(同一题问两件事)?
(3) 招募激励方案是否会扭曲样本(只吸引来'想要奖励的人')?
(4) **伦理担忧**:暗访、未告知录音、儿童 / 弱势群体未授权介入?
若任一伦理类发现存在,输出 'ETHICAL_BLOCK';
若仅有偏差类发现,输出 'BIAS_CONCERN' + 修改建议;
否则 'BIAS_PASS'。"
```

## ESCALATE 路径

- method-critic BLOCK 两轮未通过 → `ESCALATE: METHOD_INSUFFICIENT —
  REVERT_TO_PHASE 02-hypothesis,因为 H<n> 在现有约束内无法证伪`
- bias-watcher 输出 ETHICAL_BLOCK → `ESCALATE: ETHICAL_CONCERN —
  < 具体伦理问题,例:'拟未告知用户录音访谈' >。 必须人审。`
- 时间线超出用户最初 brief 中的预算 → `ESCALATE: NEED_USER_INPUT —
  完整方法预算 N 周 / $ X,超原 brief。请 [A] 缩样本框 [B] 砍 H<i>
  [C] 接受新预算`

## 收尾

`PHASE_DONE: method` 或 `ESCALATE: <prefix> — <reason>`。
完成信号(M4.5+)= `METHOD_DESIGNED`。
