---
name: ccteam-creator
description: "NL dialogue to start a new ccteam project / workflow / IM bot. Pulls user intent → infers execution mode (in-proc / bg / chat-dm / chat-group) → picks a persona from the prefab library → writes workflow.yaml + .claude/agents/<role>.md + registers the bot with no yaml editing on the user side. Use when the user says '新项目' / '做个 TG 助理 bot' / '做个 X workflow' / '帮我搭一个 ccteam team' / 'create a workflow' / 'spin up a chat bot'."
---

# /ccteam-creator — NL bootstrap for ccteam projects

V0.6.0 Wave 2 F114 — supersedes the V0.5 step-1/2/3/4 dispatch
dialogue with an LLM-driven NL flow that infers everything the user
shouldn't have to specify (execution mode, preset, persona),
surfaces a PROJECT PLAN, and only executes after `go`.

## V0.6.0 skill family (you are here)

| User intent | Skill |
|---|---|
| **Top-level NL dispatcher** | `ccteam` (calls back into this skill) |
| Spin up a short-lived team in the current session | `ccteam-team` |
| **Start a new project / workflow / IM bot (this skill)** | **`ccteam-creator`** |
| Manage existing ccteam projects | `ccteam-control` |
| One-shot IM token onboarding | `ccteam-im-setup` |
| Codex + Claude parallel advisor | `ccteam-advise` (Wave 3) |

If the user wants to *start* a short-lived team in their current
Claude session without persistent ccteam state, point them at
`/ccteam-team` instead. This skill is for **long-running ccteam
projects** with a persistent `workflow.yaml` + daemon orchestration.

## Contracts you enforce

| Contract | Authoritative spec — read with `@<path>` |
|---|---|
| `workflow.yaml` schema | `@crates/ccteam-core/src/workflow.rs` |
| Claude Code agent frontmatter | `@~/.claude/plugins/marketplaces/claude-plugins-official/plugins/plugin-dev/agents/agent-creator.md` |
| `mode_inferrer` decision table | `@crates/ccteam-core/src/mode_inferrer.rs` |
| Persona prefab library | `@skills/ccteam-creator/personas/manifest.toml` |
| Workflow templates | `@crates/ccteam-core/src/templates/workflow_templates/` |

If any path is missing, fall back to the embedded examples below.

---

# Phase 1 — Intent classification

Parse the user's NL request and extract three orthogonal axes. Use
LLM reasoning — no regex, no keyword tables.

| Axis | Vocabulary | Example tokens |
|---|---|---|
| `task_type` | `coding` / `writing` / `research` / `support` / `scheduling` / `monitoring` / `chat-assistant` / `multi-bot-team` / `qa-loop` / `other` | "fix all TS errors" → `coding`; "做个 TG 助理 bot" → `chat-assistant` |
| `presence` | `full-attended` / `partial` / `hands-off` / `im-dm` / `im-group` | "我全程盯着" → `full-attended`; "关上电脑跑" → `hands-off`; "私聊 bot" → `im-dm` |
| `timeline` | `one-shot` / `hours` / `long-running` | "今天搞定" → `one-shot`; "夜里跑" → `hours`; "24/7" → `long-running` |

**Examples:**

```
"我想做个 TG 助理 bot"
  → task=chat-assistant, presence=im-dm, timeline=long-running

"fix all TS errors"
  → task=coding, presence=full-attended, timeline=one-shot

"夜里跑一个 qa-loop"
  → task=qa-loop, presence=hands-off, timeline=long-running

"和我们群里搭个 3 bot 的圆桌"
  → task=multi-bot-team, presence=im-group, timeline=long-running
```

If any axis is unrecoverable from the user's message, **ask one
clarifying question** before continuing. Do **not** assume.

---

# Phase 2 — Mode inference

Feed the three axes into `ccteam_core::infer_mode(intent) ->
InferenceResult`. The function applies the V0.6.0 PRD F114 decision
table:

| presence | timeline | CreatorMode | Preset |
|---|---|---|---|
| full-attended | one-shot / hours | InProc | inproc-solo / inproc-team |
| partial | one-shot | InProc | inproc-solo |
| partial | hours / long-running | Bg | bg-overnight |
| hands-off | (any) | Bg | bg-overnight |
| im-dm | (any) | ChatDm | chat-pocket |
| im-group | (any) | ChatGroup | chat-squad |

Three return shapes:

- `InferenceResult::Confident(mode)` — proceed to Phase 3.
- `InferenceResult::Ambiguous(candidates)` — surface ranked options
  to the user, let them pick one.
- `InferenceResult::NeedsClarification(question)` — bounce the
  question to the user, then re-enter Phase 1 with the new info.

---

# Phase 3 — Persona match

Read `@skills/ccteam-creator/personas/manifest.toml`. Match the
user's intent against the `description` + `tags` fields. Default
selection rules:

1. If `task_type == "chat-assistant"` + mode is ChatDm → prefer
   `tech-helper` / `writing-assistant` / `tutor` / `customer-support`.
2. If `task_type == "qa-loop"` or mode is Bg with coding-tagged
   intent → prefer `code-critic`.
3. If `task_type == "multi-bot-team"` (IM Squad) → pick 2–3 personas
   that complement (e.g. `tech-helper` + `code-critic` + `project-lead`).
4. If the user wrote in Chinese → default to `zh/role.md`; otherwise
   `en/role.md`. (User may override: "用英文 persona".)

If no persona is a clean fit, ask the user to pick from the top 3
candidates. **Do not invent a new persona** — V0.7 will add a
`/ccteam-creator-persona-new` flow; for V0.6 the library is fixed.

---

# Phase 4 — Output PROJECT PLAN (plan-first)

**Do not execute anything yet.** Surface this exact shape to the
user:

```
PROJECT PLAN
============
Slug: <kebab-case-slug>
Type: <Preset label> (<short description>)
Persona: <persona label, zh or en variant>
Bot: <@handle>                          # chat presets only
IM platform: <telegram|slack|discord>   # chat presets only
Codex critic: <auto-enabled | not applicable | unavailable>
Cost estimate / day: ~$<n>
Files I'll write:
  - <project>/.ccteam/workflow.yaml
  - <project>/.claude/agents/<role>.md
  - <project>/.mcp.json
  - ~/.ccteam/im/credentials.json    # only if IM onboarding needed

Reply 'go' to execute, or describe what to change
(persona / IM platform / bot name / language).
```

Cost estimate cheat sheet (rough Sonnet 4.5 baseline; actual usage
varies):

- `inproc-solo` — $0.5–$2 per session
- `inproc-team` — $2–$8 per session
- `bg-overnight` — $3–$10 per night (capped by `budget`)
- `chat-pocket` — $0.5 per day light use, $3+ heavy
- `chat-squad` — $2–$15 per day depending on hop_limit

**STOP and wait** for the user's reply. Treat any reply that is not
literally `go` / `yes` / `approve` (or the obvious zh equivalents
`好` / `好的` / `开干` / `行` / `可以`) as feedback — loop back to
the relevant earlier phase to revise.

---

# Phase 5 — On `go`, execute

For each step, surface a one-line progress note ("⏳ rendering
workflow.yaml…", "✓ wrote .claude/agents/tech-helper.md") so the
user knows what's happening.

## 5.1  IM onboarding (chat presets only)

If `~/.ccteam/im/credentials.json` does not have the required
platform yet, invoke `/ccteam-im-setup`. Pause this skill until the
sub-skill returns; then resume.

## 5.2  Pick a slug

Use `ccteam_core::pick_unused_slug(<team>)` if you have a team
context; otherwise propose `<persona-id>-<short-noun>` and confirm.

## 5.3  Render workflow.yaml

```
ctx = WorkflowTemplateCtx::new()
    .with("workflow_slug", slug)
    .with("persona_label", persona.label)
    .with("user_brief", original_user_message)
    # preset-specific keys (see workflow_templates/mod.rs):
    .with("primary_role", persona.id)
    .with("bot_handle", "@" + bot_name)          # chat-only
    .with("im_platform", "telegram")              # chat-only
    .with("owner_chat_id", creds.owner_chat_id)   # chat-only
yaml = render_workflow_template(preset, &ctx)
write_to(project_dir + "/.ccteam/workflow.yaml", yaml)
```

## 5.4  Install persona

```
src  = repo + "/skills/ccteam-creator/personas/<id>/<lang>/role.md"
dest = project_dir + "/.claude/agents/<persona-id>.md"
copy src → dest
```

## 5.5  Bot handle minting (chat presets)

```
existing = list_existing_bot_handles_across_projects()
handle   = "@" + pick_unused_bot_name(existing).to_lowercase()
```

The pool is the scientist-nickname list in
`@crates/ccteam-core/src/agent_naming.rs`.

## 5.6  Register the bot

For chat presets, call `ccteam_imd::register_bot(slug, persona_id,
vendor, im_platform)` → `BotRegistration`. Embed the returned
`bot_handle` into the rendered `workflow.yaml` (the
`chat.bot_name` field).

## 5.7  Project-level `.mcp.json`

```
render_project_mcp_json(current_ccteam_bin()) → project_dir/.mcp.json
```

Merges into existing `.mcp.json` if present (does not clobber other
servers).

## 5.8  Ensure daemon is running

Run `ccteam internal daemon ensure-running` so the workflow is
picked up. If it was already running, the next reload tick (≤ 5s)
will roster the new workflow.

## 5.9  User reply

```
好了 ✓

  Bot:    <@handle>
  Slug:   <slug>
  Mode:   <preset>
  Persona: <label>

在 <IM platform> 私聊 <@handle> 就能开聊。要看状态:
  /ccteam-control show <slug>
```

(English equivalent if the user wrote in English.)

---

# Phase 6 — Mid-flight revision / fallbacks

If the user interrupts with "等等,改成 X" / "wait, make it Y",
bounce back to the relevant earlier phase:

- Persona change → re-enter Phase 3 with the new constraint
- IM platform change → re-enter Phase 4 (rebuild PROJECT PLAN)
- Mode change → re-enter Phase 2 with the override

If Phase 1 cannot produce a single intent category, ask:
"你是想 (a) 一次性写点代码 / (b) 长跑后台干活 / (c) 做个 IM bot /
(d) 其他?选一个或重新描述。"

If `infer_mode` returns `Ambiguous`, show the top 2 candidates +
one-line tradeoff comparison; let user pick.

If persona match is < 50% confidence, list top 3 + "其他 / 自己写
一个 V0.7+ 才支持,V0.6 请从内置库选" and bounce.

Never silently re-execute Phase 5 steps that already ran — if
`workflow.yaml` is already on disk, ask before overwriting.

---

# What this skill does NOT do

- **User-defined personas** — V0.7+ will add a creator flow; for
  V0.6 the prefab library is fixed at 7 personas.
- **Persona marketplace pull** — V0.8+.
- **OAuth-based IM onboarding** — `/ccteam-im-setup` is token-only
  for V0.6.
- **Voice / multi-modal input** — V0.7+ (text only).
- **Cross-vendor agent vendoring** — Codex critic auto-enable is
  declarative (a hint in PROJECT PLAN); actual Codex vendoring lands
  in F112 (Wave 3).
