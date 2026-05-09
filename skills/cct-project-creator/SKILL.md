<!-- ccteam-managed:skill begin -->
---
name: cct-project-creator
description: |
  When the user wants to create a new ccteam project — walk a four-phase
  dialogue: clarify the brief, recommend a slug, pick a team, and dispatch
  via `cct new`. Use when the user says "新项目" / "建一个 X" / "做个 X" /
  "做一个 X" / "调研 X" / "评估 X 值不值" / "看看 X 能不能做". Primary
  consumer is the ccteam meta-agent session; the skill assumes
  `AskUserQuestion` is available (which is true inside the meta-agent
  context — the V0.2 PreToolUse intercept runs in project sessions only,
  not the meta-agent).
allowed-tools: [Bash, AskUserQuestion]
---

# cct-project-creator

You are a **project-creation dialogue guide**, not a worker. After this
skill finishes, you call `cct new --slug <slug> --team <team>
"<refined brief>"` to dispatch the project to a fresh ccteam session.
**You do not write code, do not scaffold, do not run `git init` /
`cargo new` — the dispatched session does all of that.**

## Boundary check before you start

- This skill runs **only inside the ccteam meta-agent session**. Project
  sessions cannot reach it (their PreToolUse hook denies
  `AskUserQuestion`, V0.2 §2.4).
- If the user is asking a **fact / definition / status** question, do not
  invoke this skill. Drop back to plain Q&A. (See `meta_agent_role.md`
  §1: "调研 X" is a project request, but "X 是什么意思?" is a fact.)
- If you already have a deliberate `--slug` from the user (eg they said
  "用 hermestrade-home 这个名字"), skip Phase B and use it verbatim.

## Phase A — Clarify the brief

Read the user's original brief. **Information density check** — does the
brief carry ≥ 2 sentences with a clear technical form / goal / constraint?

- **Yes** → skip Phase A, jump to Phase B.
- **No, single-token brief** (eg "做个 todo") → ask **one** clarifying
  question with `AskUserQuestion` and typical options + an "Other" slot:

  ```
  AskUserQuestion({
    question: "项目什么形态?",
    options: [
      { label: "Web 应用", description: "浏览器跑,带前端" },
      { label: "CLI 工具", description: "命令行,纯文本" },
      { label: "移动端", description: "iOS / Android" },
      { label: "其他", description: "我下面会描述" },
    ]
  })
  ```

**Only ask one question.** Do not fire a second clarifier — the
dispatched team's first phase (`plan-eng` / `kickoff`) will handle deeper
requirements gathering. Your job is to surface enough signal to pick the
right team + name the project.

## Phase B — Recommend a slug + confirm

Compose a recommended slug from the brief + Phase A answer:

**Rules**:
- **Prefer brand / proper nouns** the user mentioned ("HermesTrade DEX"
  → `hermestrade-dex`).
- **No verb-leading**: use `todo-cli`, not `build-todo-cli`.
- **2-4 tokens, kebab-case, `[a-z0-9-]+`, ≤ 60 chars.**
- **Do not include the team prefix** — `cct new` adds it automatically
  (B2 prefix semantics, PRD §3.2.1).

Confirm with `AskUserQuestion`:

```
AskUserQuestion({
  question: "项目 slug 用什么?",
  options: [
    { label: "<recommended-slug>",
      description: "基于 brief 的 <核心词>;推荐用这个" },
    { label: "我来定",
      description: "选这个我下面问你想用什么 slug" },
    { label: "再来一个",
      description: "换一个角度重算" },
  ]
})
```

- User picks the recommended slug → Phase C.
- User picks "我来定" → ask plain NL: "你想用什么 slug?(eg
  `hermestrade-home`)"; validate `[a-z0-9-]+` and length ≤ 60; if it
  fails, surface the error and re-ask.
- User picks "再来一个" → re-derive a different slug from a different
  angle (eg drop the action verb, take the user-mentioned brand,
  combine domain + form factor) and re-ask once. After a second decline,
  fall through to "我来定".

## Phase C — Pick a team

Apply the `meta_agent_role.md` §2 decision tree:

| User says | Recommend |
|---|---|
| "做个 X / 帮我写 X / 来个 X" + brief is actionable | `dev` |
| "我想做 Y 但不确定 / 值不值 / 该不该" | `product-research` |
| "调研 Z / 这个想法有人做过吗 / 这个值得做吗" | `product-research` |

If the recommendation is unambiguous, propose it directly with one
`AskUserQuestion` for confirmation. If it's borderline:

```
AskUserQuestion({
  question: "派给哪支团队?",
  options: [
    { label: "dev",
      description: "立即开发(plan-eng → implement → … → ship)" },
    { label: "product-research",
      description: "先调研判断 idea 值不值得做(verdict + next-steps)" },
  ]
})
```

Default toward `product-research` when in doubt — research is cheap, dev
is expensive. But do not auto-research every brief; obvious build asks
should go straight to dev.

## Phase D — Dispatch + notify

Run the CLI:

```bash
cct new --slug <slug> --team <team> "<refined brief>"
```

Use the brief from Phase A (incorporating the user's clarification) as
the request body. The slug is whatever Phase B settled on; `--slug`
makes `cct new` skip the Tier 3 `claude -p` smart-suggestion path.

After dispatch, write an outbox `event_kind: reply` (per
`meta_agent_role.md` §8) telling the user:

- The project slug (`<team>-<slug>`) and the team it landed on.
- The first milestone they should expect:
  - **dev** → `plan-eng` runs first, ~30 min, may surface clarify
    questions you'll see in the decisions queue.
  - **product-research** → `kickoff` runs first and may immediately
    reverse-interview before doing any research.
- Follow-up commands: `cct show <slug>` for state, `cct attach <slug>`
  for live tmux.

Do **not** announce the dispatch before `cct new` returns successfully —
if the CLI errors out (eg unknown team), surface the error to the user
and re-run Phase C with the corrected team.

## Hard limits — what this skill never does

- ❌ Never edits or writes user code (that's the dispatched session's
  job).
- ❌ Never runs `git clone`, `cargo new`, `npm init` itself.
- ❌ Never dispatches more than one project per invocation.
- ❌ Never asks the user > 1 clarifying question per phase.
- ❌ Never bypasses `cct new` — even if the user says "just do it" the
  ccteam pipeline is the only path that gets progress / cost / context
  / Seed Gate / Critic guarantees.

If the user explicitly says "先别建项目,直接帮我写一段代码" (mode 1 ad-hoc),
do **not** invoke this skill. Drop back to plain conversation; the
meta-agent role prompt §3 covers that exception.
<!-- ccteam-managed:skill end -->
