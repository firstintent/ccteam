---
name: desk
required_inputs:
  - .ccteam/topic.md
required_outputs:
  - .ccteam/desk.md
soft_cost_warn_usd: 5.0
stall_warn_minutes: 10
parallelism: solo
sub_skills: []
tools_required:
  subagents: [general-purpose, Explore, code-explorer]
  skills: []
  mcp: []
---

# 任务:案头调研(desk research)

> 把已经存在的资料过一遍,**找出"什么已知 / 什么未知"**——避免一手数据
> phase 重新发明轮子。

> dev 等价物是 `code-explorer` 类的 codebase 探索;research 这里的探索
> 对象**不是 codebase**,而是**已有材料 + 已有数据**:产品历史 / 客服记录 /
> 已发布报告 / 竞品 / 学术论文。

## 输入

- `@.ccteam/topic.md` —— 决策问题 + 研究子问题 + 范围 + 已知基线

## 不要做的事

- ❌ **不要试图通过 desk 回答研究问题本身**——desk 的任务是"找信息缺口
  ",不是"得结论"。研究问题要靠 02-hypothesis + 04-primary 回答。
- ❌ **不要把已知基线再重新写一遍**——topic.md 已经列过的不复述。
- ❌ **不要把未来要做的一手调研提前承诺**——那是 03-method 的事。

## 应该做的事

### 1. 信息源覆盖
扫以下 4 类来源,标注每类的覆盖度(`覆盖`/`部分`/`未触达`):

| 来源类 | 应当扫的 | desk 工具 |
|---|---|---|
| 项目自有 | 历史 PR / commit / issue / spec | Explore subagent + git log |
| 已有数据 | 仓库 / web 上能拿到的运营 / 监控 / log 数据 | grep + Read,必要时 Bash 跑分析 |
| 客服 / 反馈 | 用户报告、support 工单、社区贴 | grep / WebFetch |
| 外部研究 | 竞品报告、学术论文、行业 benchmark | WebSearch / WebFetch |

**覆盖度记账**:每类标"用了多少时间 / 找到几条相关 / 还遗漏什么类型来
源未触达"——这些是 03-method 决策的输入。

### 2. 推荐的 subagent 调用(approach B —— 见 docs/claude-code-tool-surface.md §1.1.3)

`code-explorer` 这个 plugin agent 在 research 团队改用作"已有资料探索":

```
请用 Task 工具,subagent_type="general-purpose",
description="desk research source scan",
prompt="读 @.ccteam/topic.md 的研究子问题,然后:
(1) 列出 ~/projects/<slug>/ 仓库内可能相关的文档与 issue;
(2) 用 WebSearch 找该领域近 2 年的 ≥3 份独立外部报告(避免
单一作者 / 单一机构);
(3) 输出 markdown 表格,每行 < 来源 / 类别 / 与哪条研究子问题相关 /
1 句话摘要 / URL 或路径 >。
不要给结论,只给 inventory。"
```

### 3. 信息缺口分析(本 phase 的核心输出)
desk 的产物**不是综述**,是**信息缺口清单**——告诉 02-hypothesis:

- 哪些研究子问题 desk 已能给出一个候选答案?(候选,不是结论)
- 哪些子问题 desk 完全没信息?
- 哪些子问题 desk 信息互相矛盾?(这往往是最高价值的假设来源)

## 产物 `.ccteam/desk.md`

固定章节:

### §1 来源 inventory
表格:`来源 | 类别 | 相关子问题 | 1 句话摘要 | URL/路径`(≥10 行;低于 10
应在 §2 解释为什么覆盖不够)

### §2 覆盖度记账
4 类来源各自的覆盖 / 部分 / 未触达 + 1 句话理由。

### §3 候选答案 vs 信息缺口
```
- 子问题 SQ1:< topic.md 里的原文 >
  - desk 候选答案:< 有 / 无 / 矛盾 >;< 一句话依据 >
  - 信息缺口:< 还需要什么数据才能定 >
```

### §4 给 02-hypothesis 的输入
2-3 条最值得做假设的方向(矛盾源 / 完全空白 / 候选答案太弱),每条 1 行。

## ESCALATE 路径

- desk 发现某子问题已有**确定结论**且决策已可下 → `ESCALATE:
  NEED_USER_INPUT — desk 阶段已能回答 SQ2,无需走完后续 phase。
  请决定 [A] 跳到 06-report 直接成稿 [B] 收窄 topic 到剩余子问题
  [C] 仍按原 topic 走完(我们想要交叉验证)`
- desk 发现 topic.md 的范围声明与已知信息**不一致**(比如声明排除"企
  业版用户"但已有数据混着) → `ESCALATE: SCOPE_DRIFT — 范围声明矛盾
  ,要不要回 00-topic 重定?`
- 外部资料严重缺失,主要靠主观经验 → `ESCALATE: NEED_USER_INPUT —
  外部资料覆盖不足(未触达 ≥2 类来源),要继续做内部资料驱动的轻 desk
  还是补 desk?`

## 收尾

完成后最后一行:`PHASE_DONE: desk` 或 `ESCALATE: <prefix> — <reason>`。
完成信号(M4.5+)= `DESK_COMPLETE`。
