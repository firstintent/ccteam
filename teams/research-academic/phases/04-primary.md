---
name: primary
required_inputs:
  - .ccteam/method.md
  - .ccteam/hypotheses.md
required_outputs:
  - .ccteam/primary/index.md
soft_cost_warn_usd: 12.0
stall_warn_minutes: 60
parallelism: solo
sub_skills: []
tools_required:
  subagents: [general-purpose]
  skills: [ccteam-control]
  mcp: [Telegram]
---

# 任务:一手数据收集

> **本 phase 必然有外部等待**——访谈排期、问卷回收、实验观察周期都
> 跨真实日历时间。phase 模板必须把"等用户回 inbox 数据"作为合法
> NEED_USER_INPUT 状态,而**不是 stall 异常**。

> dev 等价物 = implement;但 implement 是封闭式编码,primary 是开放式
> 与外部世界交互。两者的 ralph-loop 触发条件天差地别(详见 §"为什么
> 标 auto_loop")。

## 输入

- `@.ccteam/method.md` —— 每条假设的方法 / 样本框 / 工具
- `@.ccteam/hypotheses.md` —— H1..Hn 与证伪条件

## 收集纪律(读了再做)

1. **不要"边收集边改假设"**——发现数据反驳假设,**记录**到 primary,
   05-synthesis 阶段再判定;**不在本 phase 改 hypotheses.md**
2. **不要"挑数据"**——某条访谈"不符合预期"必须仍记录,且明确标记
   "与 H<n> 不一致"
3. **每条数据落档时必须带元数据**:来源 / 时间 / 受访者 ID / 方法 /
   原文(quote)/ 该证据支撑或反驳哪条假设
4. **去识别化**:受访者真实身份不进 primary 文件;用代号 R1 / R2 / ...
5. **隐私**:用户提供的信息只做约定研究范围,**禁止**进入 cross-project
   memory(M3) ——这条由 retro phase 把关,但本 phase 写入时就不该泄

## 主流程(orchestrator 视角的状态机)

primary phase 的 happy path 不是"claude 一口气跑完",而是**循环**:

```
[准备发出收集请求] → [等用户 / 受访者回数据]
   ↓                            ↓
   ↓                       数据回到 inbox
   ↓                            ↓
   ←  追加到 primary/   ←  解析 + 落档
                                ↓
                       数据足够? — 否 → 回到顶
                                ↓ 是
                          PHASE_DONE: primary
```

### 等数据时怎么 PHASE_DONE / ESCALATE

每轮 claude 不是把整个 phase 跑完,而是把"本轮收到的数据"落档,然
后给 orchestrator 一个明确信号:

- **数据已够**(覆盖证伪条件,N 达到 method.md §2 目标):
  > PHASE_DONE: primary
- **本轮处理完,继续等**:
  > ESCALATE: NEED_USER_INPUT — 已收 < N >/< 目标 N > 受访者数据;
  > 还在等 < 谁 / 哪批 >;预计 < 时间 >;orchestrator 请暂停推进,
  > 收到下一批数据(写到 ~/.ccteam/control/answer-<slug>-<n>.md)再续。
- **某来源拿不到**:
  > ESCALATE: SOURCE_UNAVAILABLE — < 哪批样本拒绝 / 联系不上 >。请
  > 决定 [A] 扩样本框补 < N > [B] 接受降级 N [C] 改方法

## 为什么标 `auto_loop: true`(M4.5+ 实施)

dev 的 fix phase auto_loop 是"测试失败 → 改代码 → 重跑"——同一段
prompt 重喂直到 TESTS_GREEN。

primary phase 的 auto_loop 是不同语义:**每次新数据到位,自动续跑同一
段 prompt**(读最新 inbox 数据 → 落档 → 评估覆盖度)。完成信号是
`PRIMARY_GATHERED`(目标 N 达到 + 覆盖证伪条件 + 至少 ≥3 独立来源)。

ralph-loop 范式机制相同(Stop hook 拦截 + 重喂);触发条件不同
(dev 看测试 / research 看 N 达标)。这正是 §F1 审计建议把
`completion_signal` 抽到 phase 模板的论证。

## 落档结构 `<project>/.ccteam/primary/`

```
.ccteam/primary/
├── index.md                       # 总账(本 phase 必产出物)
├── R1.md                          # 受访者 R1 完整原始数据
├── R2.md
├── ...
├── survey-2026-05-08.csv          # 问卷原始数据
└── log-analysis-2026-05-09.md     # 日志分析快照
```

`index.md` 字段:

```
| 编号 | 来源类型 | 收集日期 | 支撑/反驳的假设 | 一句话总结 | 文件 |
|---|---|---|---|---|---|
| R1 | 访谈 | 2026-05-07 | 支撑 H1, 反驳 H3 | 用户从未发现离线 toggle | R1.md |
| ... |
```

## 推荐的 subagent 调用 — 数据落档质量自检

每收到一批数据后:

```
请用 Task 工具,subagent_type="general-purpose",
description="primary data quality check",
prompt="读 @.ccteam/primary/<本批新文件>。检查:
(1) 元数据完整(来源、时间、ID、方法、quote)?
(2) 去识别化做了吗?(出现真名 / 邮箱 / 电话?)
(3) 与 hypotheses.md 的关联是否标对了?
(4) 有没有把 leading question 自答(claude 自己'解读'了用户没说的话)?
若 (3) 错或 (2) 漏 → 输出 'PRIMARY_QUALITY_BLOCK';否则 'PRIMARY_QUALITY_PASS'。"
```

QUALITY_BLOCK 时**不要**进 PHASE_DONE;先在 primary/ 内修正。

## ESCALATE 路径(完整列表)

| 触发 | ESCALATE |
|---|---|
| 等用户回数据 | NEED_USER_INPUT — 已收 N/M;等 < ... > |
| 来源全失败 | SOURCE_UNAVAILABLE — 替代方案 ABC |
| 收到的数据反驳了 method 设想本身(不是反驳假设) | METHOD_INSUFFICIENT — REVERT_TO_PHASE 03-method,因为 < 具体观察 > |
| 数据触碰伦理边界(用户提到了未告知话题) | ETHICAL_CONCERN — < 具体 > |
| 数据让 topic scope 显得不对(发现真问题在别处) | SCOPE_DRIFT — < 具体 >;请决定扩 scope vs 切下个项目 |
| 数据已够 | (走 PHASE_DONE,不是 escalate) |

## 与 stall 检测的边界

primary phase 的 `stall_warn_minutes: 60` —— 比 dev phase 默认 5 分钟
长一个数量级,因为这里**真的会等**几小时甚至几天。orchestrator 的
stall 5/15/30 分钟告警在 primary phase 应当**明确容忍**:

- 5 分钟无 hook 事件:正常,在等 inbox
- 15 分钟无事件:仍正常
- 30 分钟无事件:正常(M1+ telegram 软提醒,不升级)
- 24 小时无事件:升级为"长期等待",照例 NEED_USER_INPUT(已通过 PHASE
  内的 escalate 表达,不需要 stall 兜底)

实现注:M4.5 之前 stall 检测对 phase 不区分,这里的"60 分钟"只在 phase
模板字段成熟后生效。**不要为此放慢 dev 团队的 5 分钟告警**。

## 收尾

完成时最后一行:

- 数据足够 → `PHASE_DONE: primary`
- 等下一批数据 → `ESCALATE: NEED_USER_INPUT — < 具体 >`
- 失败路径 → `ESCALATE: <SOURCE_UNAVAILABLE | METHOD_INSUFFICIENT |
  ETHICAL_CONCERN | SCOPE_DRIFT> — < reason >`

完成信号(M4.5+)= `PRIMARY_GATHERED`。
