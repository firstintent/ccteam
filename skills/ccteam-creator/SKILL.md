---
name: ccteam-creator
description: Create a new ccteam project (step 1/2/3/4 dialogue then dispatch via `ccteam new`) or design content (workflow.yaml, `.claude/agents/<role>.md`, project-local skills) inside an existing project. Use when the user says "新项目" / "建一个 X" / "做个 X" / "调研 X" / "做个 workflow" / "建一个 agent / skill" / "把 X 自动化" / "迁移 X 到 ccteam" / "给这个项目加 ccteam 流水线" / "design a multi-agent loop". V0.5.0 F100 merge: absorbed `ccteam-project-creator` (new-project dialogue) and `ccteam-team-author` (team factory; the factory CLI itself was deleted — the conversational design surface lives here).
---

# ccteam-creator — design + dispatch ccteam content

You design both **new ccteam projects** (via `ccteam new`) and the
**content inside an existing project** — `workflow.yaml`,
`.claude/agents/<role>.md`, and optional project-local skills. The
top of this skill body covers the new-project flow (step 1/2/3/4
dialogue); the bottom covers in-project content authoring. Pick the
section that matches the user's intent.

You enforce three contracts. **Each contract lives in an authoritative
file — read it directly, don't paraphrase it here:**

| Contract | Authoritative spec — read with `@<path>` or `Read` |
|---|---|
| ccteam `workflow.yaml` schema | `@crates/ccteam-core/src/workflow.rs` (parser SoT) + `@docs/v0-4-0/prd.md` (§6) + `@docs/interfaces.md` |
| Claude Code **agent** frontmatter | `@~/.claude/plugins/marketplaces/claude-plugins-official/plugins/plugin-dev/agents/agent-creator.md` |
| Claude Code **skill** frontmatter | `@~/.claude/plugins/marketplaces/anthropic-agent-skills/skills/skill-creator/SKILL.md` |

If any path is missing on the host, fall back to existing in-repo
examples (any deployed project's `.claude/agents/<role>.md` for agent
shape; any of the V0.5.0 shipped skills under repo `skills/` for skill
shape).

## V0.5.0 skill family (you are here)

| Intent | Skill |
|---|---|
| **Create / design ccteam content (this skill)** | **`ccteam-creator`** |
| Manage existing ccteam projects (status / pause / resume / inject decision) | `ccteam-control` |
| Spin up an Anthropic Agent Team in the current Claude session | `ccteam-team` |

If the user wants to *start* a quick team in their current session
without persistent ccteam state, point them at `/ccteam:team` (the
`ccteam-team` skill). If the user wants to *create a long-running
ccteam project* with `workflow.yaml` + `ccteam start` orchestration,
this skill is the right place.

---

# Part 1 — Creating a new ccteam project (step 1/2/3/4)

Use this part when the user wants a **new ccteam project** dispatched
via `ccteam new <slug>` to a fresh ccteam-managed working directory at
`<projects_root>/<team>-<slug>/`.

You are a **dispatch dialogue guide**, not a worker. After this section
finishes, you call `ccteam new <slug> --team <team>` to dispatch the
project. The refined brief from step 1 is captured in the session's
CLAUDE.md template — pass slug + team only, no free-text body. **You
do not write code, do not scaffold, do not run `git init` / `cargo new`
— the dispatched session does all of that.**

## Boundary check before you start

- This part runs **only inside the ccteam meta-agent session** or any
  Claude session that has `AskUserQuestion` available. Project sessions
  with a PreToolUse `AskUserQuestion` deny can't run it.
- If the user is asking a **fact / definition / status** question, do
  not invoke this skill. Drop back to plain Q&A. (See the meta-agent
  role prompt: "调研 X" is a project request, but "X 是什么意思?" is a
  fact.)
- If you already have a deliberate `--slug` from the user (e.g. they
  said "用 hermestrade-home 这个名字"), skip step 2 and use it verbatim.

## Step 1 — Clarify the brief

Read the user's original brief. **Information density check** — does
the brief carry ≥ 2 sentences with a clear technical form / goal /
constraint?

- **Yes** → skip step 1, jump to step 2.
- **No, single-token brief** (e.g. "做个 todo") → ask **one** clarifying
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

**Only ask one question.** Don't fire a second clarifier — the dispatched
session's first turn will handle deeper requirements gathering. Your job
is to surface enough signal to pick the right team + name the project.

## Step 2 — Recommend a slug + confirm

Compose a recommended slug from the brief + step 1 answer:

**Rules**:
- **Prefer brand / proper nouns** the user mentioned ("HermesTrade DEX"
  → `hermestrade-dex`).
- **No verb-leading**: use `todo-cli`, not `build-todo-cli`.
- **2–4 tokens, kebab-case, `[a-z0-9-]+`, ≤ 60 chars.**
- **Do not include the team prefix** — `ccteam new` adds it automatically.

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

- User picks the recommended slug → step 3.
- User picks "我来定" → ask plain NL: "你想用什么 slug?(eg
  `hermestrade-home`)"; validate `[a-z0-9-]+` and length ≤ 60; if it
  fails, surface the error and re-ask.
- User picks "再来一个" → re-derive a different slug from a different
  angle (e.g. drop the action verb, take the user-mentioned brand,
  combine domain + form factor) and re-ask once. After a second decline,
  fall through to "我来定".

## Step 3 — Pick a team

| User says | Recommend |
|---|---|
| "做个 X / 帮我写 X / 来个 X" + brief is actionable | `dev` |
| "我想做 Y 但不确定 / 值不值 / 该不该" | `research` |
| "调研 Z / 这个想法有人做过吗 / 这个值得做吗" | `research` |

If the recommendation is unambiguous, propose it directly with one
`AskUserQuestion` for confirmation. If it's borderline:

```
AskUserQuestion({
  question: "派给哪支团队?",
  options: [
    { label: "dev",     description: "立即开发(workflow → agents → ship)" },
    { label: "research", description: "先调研判断 idea 值不值得做(verdict + next-steps)" },
  ]
})
```

Default toward `research` when in doubt — research is cheap, dev is
expensive. But don't auto-research every brief; obvious build asks
should go straight to `dev`.

## Step 4 — Dispatch + notify

Run the CLI:

```bash
ccteam new <slug> --team <team>
```

The slug is whatever step 2 settled on. The refined brief from step 1 is
the conversation context the dispatched session inherits via its
CLAUDE.md.

After dispatch, write an outbox `event_kind: reply` (per the meta-agent
role prompt) telling the user:

- The project slug (`<team>-<slug>`) and the team it landed on.
- Follow-up commands: `ccteam show <slug>` for state, `ccteam internal
  attach <slug>` for live tmux.

Do **not** announce the dispatch before `ccteam new` returns successfully —
if the CLI errors out (e.g. unknown team), surface the error to the user
and re-run step 3 with the corrected team.

## Hard limits — Part 1 never does

- Never edits or writes user code (that's the dispatched session's job).
- Never runs `git clone`, `cargo new`, `npm init` itself.
- Never dispatches more than one project per invocation.
- Never asks the user > 1 clarifying question per step.
- Never bypasses `ccteam new` — even if the user says "just do it" the
  ccteam pipeline is the only path that gets progress / cost / context
  guarantees.

If the user explicitly says "先别建项目,直接帮我写一段代码" (one-shot
ad-hoc), do **not** invoke Part 1. Drop back to plain conversation; the
meta-agent role prompt covers that exception.

---

# Part 2 — Designing content for an existing project

Use this part when the user already has a ccteam project (or wants to
add ccteam to an existing repo) and needs help authoring `workflow.yaml`
+ `.claude/agents/<role>.md` + optional project-local skills.

## Capability index — Part 2

| What you want | Where |
|---|---|
| Pick agents + triggers | [Topology](#topology) below |
| Author `workflow.yaml` | [Workflow.yaml — ccteam-specific](#workflowyaml--ccteam-specific) below |
| Author `.claude/agents/<role>.md` | `@…/plugin-dev/agents/agent-creator.md` |
| Author `.claude/skills/<name>/SKILL.md` | `@…/anthropic-agent-skills/skills/skill-creator/SKILL.md` |
| Wire artifact dirs + gitignore | [Wire up](#wire-up) below |
| Verify everything loads | [Verify](#verify) below |

## Capture intent

Information density check on the user's brief.

- **Brief is dense** (≥ 2 sentences specifying loop semantics) → proceed
  directly to topology.
- **Single-token brief** ("做个 dex-ui 的自动化") → ask **one**
  `AskUserQuestion` with 3–4 options + Other slot. Typical archetypes:

  ```
  - 事件驱动闭环 (planner → explorer → fixer → master)
  - 单 agent 巡检 (schedule 触发,一个 agent 做完)
  - review 链 (writer → reviewer → shipper)
  - data 管线 (collector → processor → emitter)
  ```

Capture before moving on:

1. **Project root** (cwd or `--in <path>`).
2. **Agent list** — roles + 1-line responsibility each.
3. **Trigger story** — when does each agent wake?
4. **State storage** — `.ccteam/<artifact>/` local files (default per
   CLAUDE.md §三) vs external (GH / Linear / Slack — opt-in only).

Don't proceed without a single-sentence flow description.

## Topology

For each role pick exactly one trigger and a parallelism cap.

Triggers (full grammar in `@crates/ccteam-core/src/workflow.rs::parse_trigger`):
- `manual` — user invokes via `ccteam spawn <slug> <role>`
- `schedule` — interval-based (V0.4.1+ scheduler; V0.4.0 = manual placeholder)
- `gate` — waits for `trigger_gate` MCP call
- `watch:<rel/path/>` — spawn on file change under `<path>` relative to project root

Decision flow per agent:

1. Runs *every time* an upstream artifact appears? → `watch:`
2. Runs *periodically* regardless of state? → `schedule`
3. Waits for explicit user permission (e.g. ship gate)? → `gate`
4. Otherwise → `manual`.

`parallelism > 1` is **only** legal with `watch:` (validate() rejects
otherwise). Leave it unset for the other three.

### Self-excitation pitfall (`watch:` triggers)

A `watch:<dir>/` trigger fires on **every** filesystem event under
`<dir>/` — including `Modify` events emitted by `notify` whenever a
file inside is rewritten (e.g. `jq '.field=X' f.json > f.tmp && mv
f.tmp f.json`). If the agent's own body mutates files inside its
*own* watched dir, you get an infinite loop: agent runs → updates an
artifact's status → watcher fires → spawns the agent again.

**Real incident** (dex-ui 2026-05-16): explorer's `.ccteam/backlog/`
watch self-excited via status updates + new scenarios. Burst climbed
from 1/min to 8/min over 4 h, 45 successful spawns + 80 stale-spawn
errors, $1.10 burned. fixer + master had the same issue on `issues/`
and `prs/`.

**Pattern to apply** (when an agent must mutate the artifact source):

| Role of dir | Watched? | Who writes |
|---|---|---|
| tracking artifacts (mutable state) | NO | the agent itself |
| trigger markers (write-once tiny JSON) | YES | upstream agent / human |

Split the QA loop's `backlog/` (tracking, unwatched) from a
new `explore-requests/` (markers, watched). Upstream writes a marker
file like `planner-<ts>.json` containing `{requested_by, at,
reason}`. The downstream agent reads + immediately archives the
marker to `<dir>.archived/` (outside the watched tree) before doing
any real work — so a re-fire on the same marker cannot happen.

Rule of thumb: **an agent must never write a file into its own
watched dir**, even transiently. If it has to leave a status
breadcrumb, put it in a sibling unwatched dir.

### Common topologies

| Topology | Agents + triggers |
|---|---|
| **QA loop** | planner:manual; explorer:watch:.ccteam/explore-requests/; fixer:watch:.ccteam/fix-requests/ (parallelism:2); master:watch:.ccteam/merge-requests/ |
| **Single watchdog** | watchdog:schedule (interval:"2h") |
| **Review chain** | writer:manual; reviewer:watch:.ccteam/drafts/; shipper:gate |
| **Data pipeline** | collector:schedule; processor:watch:input/; emitter:watch:processed/ |

The QA loop row above pairs each `watch:<dir>` with a separate
tracking dir (`backlog/` / `issues/` / `prs/`) that the corresponding
agent mutates freely.

## Workflow.yaml — ccteam-specific

`workflow.yaml` is ccteam's contract (not Claude Code's). Schema is
defined by `WorkflowSpec` / `AgentSpec` in
`@crates/ccteam-core/src/workflow.rs` — when in doubt, read that file.
Minimal shape:

```yaml
name: <kebab-case>                  # required; appears in progress.jsonl
description: |                      # optional; reader-facing only
  One paragraph.

agents:                             # required, ≥ 1 entry
  <role>:                           # role = filename of .claude/agents/<role>.md
    executor: claude                # default; or `codex` (only if the
                                    # agent body is written for Codex CLI)
    trigger: <see Topology>         # required
    parallelism: 1                  # optional; > 1 only when trigger=watch:
    input: <rel/path>               # optional; passed as $CCTEAM_INPUT
    output: <rel/path>              # optional; passed as $CCTEAM_OUTPUT
    interval: "1h"                  # only for trigger=schedule (V0.4.1+)
    timeout: "30m"                  # optional; per-session wall clock
```

### Path to write
`<project>/.ccteam/workflow.yaml` (V0.4.6 F83+ canonical). New workflows
go in `.ccteam/`; the orchestrator's discovery order prefers `.ccteam/`.

### Validation
Daemon parses on rescan (10s tick). On load you should see in the daemon
log:

```
starting project event loop slug="<slug>"
watch registered slug="<slug>" role="<role>" watch="<path>"
```

Validation errors surface as `WARN`/`ERROR` lines around the same time;
project gets skipped if invalid.

## Agent + skill files — defer to official specs

The frontmatter rules for `.claude/agents/<role>.md` and
`.claude/skills/<name>/SKILL.md` are **not ccteam's contracts** — they
belong to Claude Code. Don't reinvent them here.

When writing or editing an agent, read:

```
@~/.claude/plugins/marketplaces/claude-plugins-official/plugins/plugin-dev/agents/agent-creator.md
```

For skill files, read:

```
@~/.claude/plugins/marketplaces/anthropic-agent-skills/skills/skill-creator/SKILL.md
```

V0.4.0+ agents do NOT need to emit completion sigils — the V0.2 phase
machinery was deleted and agents are now triggered + observed via
`workflow.yaml` + the `ArtifactWatcher` reading `.ccteam/<dir>/*`
artifact files. End-of-session is just the normal Claude `--bg` job
exit; the orchestrator writes the `agent_done` event into
`progress.jsonl` from `~/.claude/jobs/<id>/state.json`.

In-repo examples for the agent body style you can copy structurally
(not verbatim — keep them ccteam-flavored): any deployed project's
`.claude/agents/<role>.md` from a recent install, plus any starter under
`@agents/` if the repo has one.

## Wire up

After writing `workflow.yaml` + agents (+ optional skills):

1. **Create artifact dirs** for every `watch:<path>/` so inotify has
   something to watch even before the loop runs:

   ```bash
   mkdir -p .ccteam/{backlog,issues,prs,acceptance}/   # adjust to your topology
   ```

2. **`.gitignore`** — orchestration state is local, not source:

   ```bash
   grep -q '^\.ccteam/' .gitignore 2>/dev/null \
     || echo -e '\n# ccteam orchestration state (local-only)\n.ccteam/' >> .gitignore
   ```

3. **Project config** — if the workflow needs settings (GH owner/repo,
   staging URL, etc.), write `.ccteam/config.json` and have agents read
   via `jq -r '.<field>' .ccteam/config.json`. Convention, not enforced.

4. **Secrets** — `.env` at project root (gitignored); agents source
   with `set -a; . .env; set +a`.

## Verify

Three checks before declaring done:

1. **Daemon picks up the workflow** — within 10s of writing
   `workflow.yaml`, daemon log shows `starting project event loop` and
   `watch registered` lines for each role. If only "no workflow.yaml;
   skipping" → check file path.

2. **Agents load in Claude Code** — open a fresh `claude` session in the
   project, press `←` or run `/agents`. Each agent appears with its
   description. Missing → frontmatter problem (most common: missing
   `---` delimiters or `name` ≠ filename).

3. **Skills load** (if any) — `/skills` lists active skills. Same drill.

If all three pass: loop starts on the first artifact write (watch
triggers) or `ccteam spawn <slug> <role>` (manual).

## Anti-patterns — Part 2

- **`workflow.yaml` under `.claude/`** — that's Claude Code's scope.
  ccteam's workflow lives at project root (or `.ccteam/`).
- **Agent without frontmatter** — invisible to Claude Code's picker
  even though the file exists.
- **`parallelism: 2` on non-watch trigger** — `WorkflowSpec::validate`
  rejects; daemon skips the project.
- **Watch dir doesn't exist at load time** — inotify install fails
  silently; create dirs in "Wire up" before relying on triggers.
- **Auto-commit `.ccteam/`** — orchestration state, not code.
- **Reach for external systems by default** — `.ccteam/<artifact>/` is
  the canonical state store (CLAUDE.md §三 red lines). External APIs only
  when the existing human surface requires it (e.g. GH PRs for code
  review — keep those).

---

## Where to look in the repo

- `@docs/v0-4-0/prd.md` §6 — workflow.yaml + artifact-trigger architecture
- `@crates/ccteam-core/src/workflow.rs` — schema source of truth
- `@docs/interfaces.md` — protocol-level field meanings
- `@CLAUDE.md` §三 — architectural red lines (file system as control plane, etc.)
- `@skills/ccteam-control/SKILL.md` — sibling skill (manage existing projects)
- `@skills/ccteam-team/SKILL.md` — sibling skill (`/ccteam:team` in current session)
