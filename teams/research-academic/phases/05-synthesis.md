---
name: synthesis
required_inputs:
  - .ccteam/primary/index.md
  - .ccteam/hypotheses.md
required_outputs:
  - .ccteam/synthesis.md
soft_cost_warn_usd: 6.0
stall_warn_minutes: 10
parallelism: solo
sub_skills: []
tools_required:
  subagents: [general-purpose]
  skills: []
  mcp: []
---

# 任务:综合 + insight 提取

> 把 primary 收集的多源数据**triangulate**——每条 insight 必须有 ≥2 独立
> 来源支撑;否则它是"轶事",不是 insight。

> dev 等价物 = test-author + test-run 合并;但 research 的"测试通过"
> 替换为"insight 经多源交叉验证"。

## 输入

- `@.ccteam/primary/index.md` —— 总账
- `@.ccteam/primary/*.md` —— 各来源原始数据(本阶段会读多个)
- `@.ccteam/hypotheses.md` —— H1..Hn 与证伪条件

## synthesis 的三个动作

### 动作 1 — 数据 → insight 候选

逐条假设遍历 primary 数据,**对每条假设**输出:

```
## H<n>: <原假设一句话>

### 支撑证据
- < 来源 R1 >: "<引用原话>" (访谈, 2026-05-07)
- < 日志分析 >: < 关键数字 / 模式 >
- ...

### 反驳证据
- < 来源 R3 >: "<引用原话>"
- ...

### 中立 / 模糊证据
- < 来源 R2 >: < 一句话 >

### 假设状态
[支撑 / 反驳 / 不定]——给出**判断依据**(数量 vs 质量,例:"3 个独立
来源支撑;1 个反驳但来自 power user 极端样本")
```

### 动作 2 — insight 提取(triangulation)

`insight = 跨假设的发现 + 至少 2 个独立来源支撑`。

每条 insight 写成:

```
## I<n>: <insight 一句话表述>

**强度**:< 2 / 3 / ≥4 独立来源支撑 >

**支撑链**:
- < 来源 1 + 关键证据 >
- < 来源 2 + 关键证据 >
- ...

**与原假设的关系**:< 直接验证 H<x> / 反驳 H<y> / 与所有原假设无关
但 emerged from data >

**对决策问题的影响**:< 引用 topic.md §1,这条 insight 让决策更倾向
< 哪个方向 >,具体度多少 >
```

**禁止**:写 insight 但只能找到 1 个来源支撑。这种是 anecdote,放
"§3 待确认观察"section,不进 insight 主表。

### 动作 3 — triangulation critic 自审

```
请用 Task 工具,subagent_type="general-purpose",
description="triangulation critic on synthesis insights",
prompt="读 @.ccteam/synthesis.md 的 insight 节。逐条 I<n> 审:
(1) 支撑链里 ≥2 个来源是否真的独立?(同一受访者多次说话不算多源;
受访者朋友介绍的另一受访者不算独立。)
(2) 支撑证据是否真支撑?还是 cherry-picked quote 表面像支撑?
(3) 'emerged from data' 类 insight 是否在原假设外但仍能回答 topic 决策
问题?如果不能,这条是 noise,不该放报告。
逐条标 STRONG / WEAK / FAILED。任意 FAILED 输出 'TRIANGULATION_BLOCK';
全 STRONG / WEAK 输出 'TRIANGULATION_PASS'。"
```

## 产物 `.ccteam/synthesis.md`

固定章节:

### §1 假设状态总表
| 假设 | 状态(支撑/反驳/不定)| 主要证据来源数 | 最强反对证据(若有)|

### §2 Insight 主表
按 §动作 2 格式逐条;按强度降序排列。

### §3 待确认观察(单源 / 模糊)
单源观察 + 一句话说明为什么没升级为 insight,以及"再补什么数据能让
它成立"。

### §4 决策问题的当前回答(回到 topic.md §1)

把 topic 的决策问题在这里**直接答一遍**:依据现有 insight,该决策
应当倾向 < A / B / C >;**给出确定性等级**(高 / 中 / 低)。这一节是
06-report 的核心输入。

## ESCALATE 路径

- triangulation critic BLOCK 两轮 → `ESCALATE: NEED_USER_INPUT —
  Insight I<n> triangulation 不足,具体卡点 < ... >;要 [A] 接受弱 insight
  [B] 回 04-primary 补来源 [C] 删该 insight`
- 多源数据**互相矛盾**且无法判定方向 → `ESCALATE: DATA_AMBIGUOUS —
  H1 与 H3 证据互相打架(具体描述);请选 [A] 收窄 topic 到 H1 方向
  [B] 收窄到 H3 方向 [C] 拆两个项目 [D] 接受'两种力同时作用'作为
  insight`
- 综合后发现真问题在 topic 范围之外 → `ESCALATE: SCOPE_DRIFT —
  数据指向 < 范围外问题 >;要扩 scope 还是切下个项目?`

## 与 dev fix-loop 的根本对照(读模板时的认知校准)

| 维度 | dev fix-cycle(06-fix.md) | research synthesis |
|---|---|---|
| 失败信号 | 测试不绿 | insight 单源 / 矛盾 |
| 修复动作 | 改代码 | **不能"改 primary 数据来满足 hypotheses"**——要么 escalate 回 primary 补数据,要么把"假设被反驳"作为 insight 接受,要么 DATA_AMBIGUOUS 让用户决定方向 |
| auto_loop 上限 | 3 轮(M0.12) | 2 轮(triangulation 第三轮还不通过 = 数据本身的问题,不是分析问题) |
| 完成信号 | TESTS_GREEN | INSIGHTS_TRIANGULATED |

`auto_loop` 上限差异写在 phase YAML(M4.5+):dev 是 3,research synthesis
是 2。

## 收尾

`PHASE_DONE: synthesis` / `ESCALATE: <prefix> — <reason>`。
完成信号(M4.5+)= `INSIGHTS_TRIANGULATED`。
