---
name: retro
required_inputs:
  - .ccteam/report.md
  - .ccteam/synthesis.md
  - .ccteam/method.md
  - .ccteam/topic.md
required_outputs:
  - .ccteam/retro.md
soft_cost_warn_usd: 1.0
stall_warn_minutes: 5
parallelism: solo
sub_skills: []
tools_required:
  subagents: [general-purpose]
  skills: []
  mcp: []
---

# 任务:retro(方法学反思)

> dev 等价物是 retro phase;字段不同——research 的 retro 输入到跨项目
> 记忆(M3+)的 schema 由 `team-research.yaml.retro_schema` 定义,不是
> dev 的"tech stack / 踩过的坑 / 成功设计"。

> 详见 [strategic doc §2.7](../docs/ccteam-as-domain-agnostic-orchestrator.md#27-与跨项目记忆m3的对接方式)
> 与 [dev-coupling-audit.md F20](../docs/dev-coupling-audit.md#f20--跨项目记忆-schema-假设-dev-字段)。

## 输入

- `@.ccteam/topic.md` —— 决策问题与原 scope
- `@.ccteam/method.md` —— 方法选择
- `@.ccteam/synthesis.md` —— insight 与 triangulation 强度
- `@.ccteam/report.md` —— 最终决策建议

## 产物 `.ccteam/retro.md`

字段顺序固定(对应 team-research.yaml.retro_schema,M4.5.5 上线后由
orchestrator 校验):

### §1 research_question(text)
重写一遍 topic.md §1 决策问题——但换成"事后看,真问题是 X"或确认
"原问题问对了"。

### §2 methods_used(text)
逐条假设列方法。每条加一句"事后看,这个方法 < 配 / 不配 >",理由
1 行内。

### §3 source_quality(rubric, 0..1)
单一 0..1 数字 + 一句话理由。
- 1.0 = ≥3 完全独立来源,所有 insight triangulation STRONG
- 0.7 = 多源但部分重叠;部分 insight WEAK
- 0.4 = 主要靠单源 + 主观推断
- 0.2 = 数据量不足,结论靠猜

### §4 hypothesis_outcomes(text)
按 H1..Hn 各一行:支撑 / 反驳 / 不定;每行附"事后看,这条假设 < 早该
预想到 / 没想到 / 写错了 >"。

### §5 would_redo_method(bool)
true / false + 1-2 行理由。如果 false,**必须**给出"下次会换什么方法"。

### §6 insights_per_dollar(number)
最终 STRONG insight 数量 / 项目累计 cost_used_usd。**这是跨项目记忆做
比对的核心 metric**——下个 research 项目召回时,用类似 topic 的历史
项目 insights_per_dollar 校准预期。

### §7 给跨项目记忆的"建议复用 / 不要再做"摘要(≤ 5 行)

固定模板:
```
- 复用:<方法 / 来源 / 工具 / phase 顺序中的某条>
- 复用:...
- 不要再做:<具体反模式,不是"做得不够好">
- 不要再做:...
```

**不写空话**:"应该早点做调研" 这种空话不进——必须是下次相似项目能直接
照做 / 直接避开的具体动作。

### §8 隐私 / 伦理回看(必填)

逐条勾:
- [ ] primary/ 内已无真名 / 邮箱 / 电话
- [ ] 受访者激励已结清
- [ ] 受访者承诺的 follow-up(若有)已发送
- [ ] 任何 ETHICAL_CONCERN escalate 的 resolution 已落档
- [ ] 进入跨项目记忆的内容已二次确认无可识别用户痕迹

任一未勾 → 不能 PHASE_DONE,先解决再回来。

## 推荐的 subagent 调用 — retro completeness check

```
请用 Task 工具,subagent_type="general-purpose",
description="retro completeness self-check",
prompt="读 @.ccteam/retro.md。检查:
(1) §1-§8 字段都填了吗?(空白 / 占位符 = FAIL)
(2) §3 source_quality 数字与 §2 方法描述是否自洽?(打 0.9 但方法
描述里有 'N=3 偏少' = 矛盾)
(3) §5 false 时是否给出'下次会换什么'?
(4) §7 是否有'空话',形如'下次更注意 X'? (具体动作 = PASS,
心灵鸡汤 = FAIL)
(5) §8 是否所有勾全打?
任一失败 → 'RETRO_INCOMPLETE'+ 具体哪条;否则 'RETRO_PASS'。"
```

## ESCALATE 路径

- retro 自检 INCOMPLETE 两轮 → `ESCALATE: NEED_USER_INPUT — retro
  自检反复未过,具体 < ... >;请用户决定接受现状 vs 让 claude 重写`
- §8 隐私勾未全 → **不 escalate,先做完再 retro**;真做不完才
  `ESCALATE: ETHICAL_CONCERN — < 具体未完成项 >`

## 收尾

`PHASE_DONE: retro` / `ESCALATE: <prefix> — <reason>`。
完成信号(M4.5+)= `RETRO_RECORDED`。

## retro 完成后(orchestrator 自动)

retro phase 完成后,orchestrator 把 retro.md 按 retro_schema 索引到
`~/.ccteam/memory/research/<slug>-retro.md`(M3+ namespace 隔离,见
strategic doc §2.7);anti-pattern 单独索引到 `~/.ccteam/memory/
anti-patterns/`(跨 team 共享)。

下次跑 research 项目,Seed 阶段(M2+)会按新 topic embedding 召回 top-3
历史 retro,作为新 topic.md 的"已知信息基线"输入(§00-topic.md)。
