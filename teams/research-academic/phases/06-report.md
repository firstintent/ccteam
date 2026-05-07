---
name: report
required_inputs:
  - .ccteam/synthesis.md
  - .ccteam/topic.md
required_outputs:
  - .ccteam/report.md
soft_cost_warn_usd: 3.0
stall_warn_minutes: 5
parallelism: solo
sub_skills: []
tools_required:
  subagents: [general-purpose]
  skills: []
  mcp: []
---

# 任务:成稿(对应 dev 的 ship)

> 把 synthesis 的 insight 改写成**决策友好**的报告——读者是 topic.md §1
> 决策问题中的"谁"。**不是综述,是决策书**。

## 输入

- `@.ccteam/synthesis.md` —— insight 主表 + 决策当前回答
- `@.ccteam/topic.md` —— 决策问题、scope、成功标准

## 决策友好的硬要求

读者在 5 分钟内读完应当能回答:

1. **当下能不能下决策?**(确定性高 / 中 / 低)
2. **下什么决策?**(明确的方向性建议,不是"考虑各种因素")
3. **若下了这个决策,主要风险是什么?**(insight 给出的 caveats)
4. **若信息不足,补什么能补到决策线?**(把 04-primary 的"待补数据"
   挂在这条上)

任何一项答不上来,报告未完成。

## 产物 `.ccteam/report.md`

固定章节(顺序固定,不要发明新顺序):

### §1 一句话结论(TL;DR — ≤ 50 字)
直接答 topic.md §1 决策问题。

### §2 决策建议(≤ 200 字)
- 确定性等级(高 / 中 / 低)
- 推荐方向 + 一句话理由
- 主要风险(≤ 3 条)

### §3 关键 insight(3-5 条)
从 synthesis.md insight 主表里**只**挑强度 STRONG 的;每条:
- 一句话表述
- 1-2 条最有力的支撑证据(quote 或数字)
- 对决策的具体影响

### §4 反例 / 限制
- 数据未覆盖的人群 / 场景 / 时段
- 反驳了某假设的证据(如有)
- 单源观察的列表(从 §3 待确认观察简列)

### §5 下一步研究建议(可选)
若决策"中"或"低"确定性,列 1-2 条最高 ROI 的下一步研究方向。

### §6 附录:研究方法摘要(≤ 100 字)
方法、样本 N、时间窗口、关键约束。供读者评估证据强度。

## 推荐的 subagent 调用 — actionability critic

```
请用 Task 工具,subagent_type="general-purpose",
description="report actionability critic",
prompt="读 @.ccteam/report.md。扮演一个 < topic.md §1 决策问题 >
中提到的'决策者'(如:产品经理 / VP)。逐节审:
(1) §1 TL;DR 真的能在 50 字以内让我下决策吗?
(2) §2 推荐方向是否具体?(反例:'考虑各种因素后再决定' = FAIL)
(3) §3 insight 是否真的对应到我能下的具体决策动作?
(4) §4 反例是否完整?有没有 cherry-pick 只支撑结论的数据?
(5) 这份报告改成 1 张幻灯片我能讲清楚吗?
逐节标 PASS / CONCERN / BLOCK。任意 BLOCK 输出
'REPORT_ACTIONABILITY_BLOCK';否则 'REPORT_ACTIONABILITY_PASS'。"
```

BLOCK 时改 report.md,**不要改 synthesis.md / 不要回头改数据**——这一阶段
只动表达层。

## ESCALATE 路径

- actionability critic BLOCK 两轮未通过 → `ESCALATE: NEED_USER_INPUT —
  报告 < 哪部分 > 不能让决策者决策,具体卡点 < ... >;请决定
  [A] 接受当前报告并跟进访谈解决 [B] 回 05-synthesis 收窄 insight
  [C] 回 04-primary 补 < 具体数据 >`
- 报告写完发现 synthesis 的"决策问题当前回答"实际**矛盾于** topic.md
  的成功标准(例:topic 要求"达成共识",但 synthesis 是"两组证据
  分立") → `ESCALATE: NEED_USER_INPUT — 研究无法满足 topic.md §5 成
  功标准。请 [A] 修改成功标准接受当前结果 [B] 回 04 / 05 补做 [C] 项目
  REJECT 并归档`

## ship 动作(对应 dev ship 的代码侧)

研究项目没有"git tag v0.1.0"等价物;但仍要做几件 ship 收尾:

1. **report.md 的最终 commit**——git add + commit,message:
   `research: <topic 短描> — <一句话决策建议>`
2. **生成可分享版本**(若 brief 提了"给 < 谁 > 看"):
   - markdown 默认就够;若指定 PDF / slide,在 06-report 内**只**生成
     markdown,转换交给后续工具(`pandoc` / 截图 / 等)——research phase
     不发明新输出格式
3. **隐私检查最后一关**:grep `report.md` 确认无受访者真名 / 邮箱 / 个人
   电话(去识别化在 04-primary 已做,这里是兜底)

## 收尾

`PHASE_DONE: report` / `ESCALATE: <prefix> — <reason>`。
完成信号(M4.5+)= `REPORT_READY`。

## 与 dev ship 的差异(读模板时的认知校准)

| 维度 | dev ship | research report |
|---|---|---|
| 主产物 | git tag + retro + 测试全绿确认 | report.md(决策友好)+ retro |
| 失败模式 | "测试通过但用户不要" | "数据漂亮但读完不知道下什么决策" |
| 兜底机制 | 痛点 11 三层防御 | 同上 + actionability critic |
| 完成信号 | (沿用 PHASE_DONE) | `REPORT_READY` |
