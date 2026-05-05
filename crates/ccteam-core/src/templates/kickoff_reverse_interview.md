# Helper: kickoff reverse-interview

> Embed this with `@~/.ccteam/templates/kickoff-reverse-interview.md`
> from a kickoff-style phase whose `required_inputs` is just `.ccteam/spec.md`
> and the spec is too thin to act on. Pattern: instead of guessing
> what the user meant, *interview the user* and synthesize a brief.
>
> Best practices §5.2 (reverse interview) is the philosophy; this
> template is the operational form. Pairs with
> `decision_mode: hybrid` (interfaces §5.6.1) and a higher
> `max_clarify_rounds` (5–7 rather than the default 3) — kickoff
> phases legitimately need more rounds than mid-pipeline ones.

## When to trigger this loop

Trigger when *any* of:

- spec.md body has fewer than ~30 meaningful tokens
- spec.md mentions a category but no platform / target user / scope
- you cannot produce a single concrete next-phase artifact without
  inventing a requirement

If spec.md is rich enough, skip this template entirely.

## How to interview

1. **List what's missing**, ordered by load-bearing weight.
   Categories worth checking:
   - **Target user**: developer / end-user / specific role / team
   - **Platform**: CLI / TUI / web / mobile / desktop / library
   - **Core scenario**: the one thing this absolutely must do well
   - **Constraints**: deadline / cost / privacy / offline / language
   - **Out-of-scope**: what the user explicitly *does not* want
2. **Ask one question per CLARIFY round**, highest-leverage first
   (route via `AskUserQuestion` or outbox per `decision_mode`).
3. **Re-evaluate after each answer** — sometimes one answer
   eliminates two follow-ups.
4. **At round cap or at "no more high-leverage questions"**, write
   the synthesized brief.

## Synthesized brief

Write to `.ccteam/brief.md` (or whatever your phase's `required_outputs`
specifies) with these sections:

```markdown
# 综合 brief

## 用户原始一句话
(spec.md 原文)

## 经反向面试澄清的关键事实
- 目标用户:
- 平台:
- 核心场景:
- 关键约束:
- 不做:

## 仍未确定但可推进的假设
列每条假设 + 触发回头条件(if X, revisit)

## 建议的下一阶段
(plan-eng / market-survey / 哪一个,以及最先要写哪份产物)
```

The next phase reads this brief instead of the raw spec — that's the
whole point. If you can't produce a brief that the next phase can act
on, ESCALATE `INSUFFICIENT_CLARIFICATION` (interfaces §5.6.2) so the
user picks: provide more, accept the assumptions, or abort.

## Anti-patterns

- Asking generic "what do you want?" — that's the question the user
  already failed to answer in spec.md. Drill into a *specific* gap.
- Synthesizing a brief by guessing missing facts then *labeling them
  "confirmed"*. If the user didn't confirm, list it under "假设" and
  state the revisit trigger.
- Treating the cap as a budget to spend — if 2 rounds cleared the
  ambiguity, write the brief and exit. Token budget left on the table
  is fine.
