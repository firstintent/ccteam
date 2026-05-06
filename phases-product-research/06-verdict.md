---
name: verdict
required_inputs:
  - .ccteam/brief.md
  - .ccteam/market-survey.md
  - .ccteam/differentiation.md
  - .ccteam/value-prop.md
  - .ccteam/feasibility.md
required_outputs:
  - .ccteam/verdict.md
  - .ccteam/rationale.md
  - .ccteam/next-steps.md
parallelism: solo
soft_cost_warn_usd: 1.0
stall_warn_minutes: 5
decision_mode: async
max_clarify_rounds: 5
---

# Verdict — 最终裁决

综合所有上游产物,做出 **PASS / CONCERN / REJECT / CLARIFY** 判断,产出三份产物。

## 三份产物

### 1. `.ccteam/verdict.md`

按 interfaces §5.3 通用 verdict schema:

```markdown
---
verdict: PASS | CONCERN | REJECT | CLARIFY
confidence: 0.0-1.0
---

## 市场分析
(一段总结自 market-survey)

## 差异化
(一段总结自 differentiation)

## 价值主张
(一段总结自 value-prop)

## 技术可行性
(一段总结自 feasibility)

## 决策
(一句话:做 / 不做 / 还要更多信息)
```

### 2. `.ccteam/rationale.md`

详细论证:

- **支持决策的关键证据**(每条引上游产物)
- **削弱决策的反向证据**(诚实列出,不掩盖)
- **不确定性来源**(我们没找到 / 没确认的事)

### 3. `.ccteam/next-steps.md`

按 verdict 的不同走向给具体下一步:

- **PASS**:推荐派 dev 团队 spec(`ccteam new --team=dev "<refined brief>"`),列必带的关键事实
- **CONCERN**:建议先做 1-2 周轻量验证(调用第三方 API 跑通核心 flow / 找 5 个目标用户访谈),再决定要不要派 dev
- **REJECT**:建议不做。如果用户仍要做,**不要直接派 dev**——先按下文「REJECT 分支 retro」要求把这次否决落进跨项目 lessons 库,再让用户决定
- **CLARIFY**:列出还需要的信息

## REJECT 分支 retro

**只在 `verdict = REJECT` 时执行**(PASS / CONCERN 的 retro 由下游 dev 项目的 ship phase 写;
CLARIFY 不是终态)。两处落地(基于 `teams/product-research.yaml.retro_schema`):

1. **本仓库 auto-memory** → 调 `/memory` 写本项目特定 lessons,topic 文件结构你自定。

2. **跨项目 lessons 库** → 用 `Edit` 修改 `~/.claude/rules/ccteam-lessons-product-research.md`,
   **只改 `<!-- ccteam-managed:lessons begin/end -->` 之间内容**(不动标记,也不动
   marked 外的用户段)。在 marked section 内 append 一段以本项目 slug + 日期为
   H2 标题的新条目;字段顺序与 description 取自
   `teams/product-research.yaml.retro_schema`(每字段一个 H3):

   - `market_signals` — Top market signals collected (demand, saturation, pricing)
   - `differentiation_findings` — Unique angles found / ruled out
   - `feasibility_assessment` — Tech / business feasibility verdict
   - `verdict_rationale` — Why this verdict (PASS / CONCERN / REJECT / CLARIFY)

   格式:

   ```
   ## <项目 slug> (YYYY-MM-DD) — REJECT

   ### market_signals
   <一段总结>

   ### differentiation_findings
   <一段总结>

   ### feasibility_assessment
   <一段总结>

   ### verdict_rationale
   <一段总结,引 rationale.md>
   ```

   若 `~/.claude/rules/ccteam-lessons-product-research.md` 不存在,说明用户没跑过
   `ccteam doctor --install-memory-bridge`;在 `.ccteam/rationale.md` 末尾追加一行
   "memory bridge missing — run ccteam doctor --install-memory-bridge",跨项目
   lessons 这次跳过(不 ESCALATE,verdict 已写完)。

## verdict 决定退出方式

| verdict | PHASE_DONE 形式 | 含义 |
|---|---|---|
| `PASS` | `PHASE_DONE: verdict` | 项目研究完成,推荐进入 dev |
| `CONCERN` | `PHASE_DONE: verdict` | 项目研究完成,但有保留 |
| `REJECT` | `ESCALATE: ABORT — <reason>` | 项目研究完成,结论是不做(此为 product-research happy path 的真实终态;不是失败) |
| `CLARIFY` | 写 outbox event_kind=clarify;循环回本 phase | 信息不足,等用户答复 |

`max_clarify_rounds: 5` 撞顶时:

```
ESCALATE: INSUFFICIENT_CLARIFICATION — 关键问题
```

(产物已 best-effort 写出,等用户决定接受 / 继续 / abort。)

## decision_mode: async

本 phase 用 async,因为 verdict 决策**重要**——用户离线也要给到完整决策,不能凑合。所有 CLARIFY 走 outbox,允许用户慢思考。
