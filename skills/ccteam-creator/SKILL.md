---
name: ccteam-creator
description: "NL dialogue to start a new ccteam project / workflow / IM bot. Pulls user intent → infers execution mode (in-proc / bg / chat-dm / chat-group) → picks a persona from the prefab library → writes workflow.yaml + .claude/agents/<role>.md + registers the bot with no yaml editing on the user side. Use when the user says '新项目' / '做个 TG 助理 bot' / '做个 X workflow' / '帮我搭一个 ccteam team' / 'create a workflow' / 'spin up a chat bot'."
---

# /ccteam-creator — NL bootstrap for ccteam projects

LLM-driven NL flow that infers everything the user shouldn't have to
specify (execution mode, preset, persona), surfaces a PROJECT PLAN,
and only executes after `go`.

## Skill family (you are here)

| User intent | Skill |
|---|---|
| **Top-level NL dispatcher** | `ccteam` (calls back into this skill) |
| Spin up a short-lived team in the current session | `ccteam-team` |
| **Start a new project / workflow / IM bot (this skill)** | **`ccteam-creator`** |
| Manage existing ccteam projects | `ccteam-control` |
| One-shot IM token onboarding | `ccteam-im-setup` |
| Codex + Claude parallel advisor | `ccteam-advise` |

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
InferenceResult`. The function applies this decision table:

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
candidates. **Do not invent a new persona** — the prefab library is
fixed; user-defined personas are not in scope for this skill.

## Phase 3.5 — Codex auto-critic detection

When the matched persona's role hints at adversarial / second-opinion
work, ccteam-creator silently consults the Codex CLI and — if it's
installed and authenticated — vendors that one role on Codex. The
user never sees `vendor: codex` in YAML during the dialogue; it just
shows up as a one-line "Codex critic: auto-enabled" note in Phase 4.

**Trigger roles** (LLM-matched against persona id + tags):

- `code-critic`
- `reviewer` / `code-reviewer` / `pr-reviewer`
- `architect` / `architecture-reviewer`
- any persona tagged `critic` or `second-opinion`

**Detection probe** (run once per `ccteam-creator` dialogue) —
consult the deterministic gate subprocess **instead of** running
`codex --version && codex login status` inline:

```bash
ccteam doctor --check-codex-auto-critic
# stdout last line: {"available": true|false, "exit_code": 0|2|3, ...}
# exit code: 0 = inject `executor: codex`; 2 = silent fallback;
#            3 = silent fallback (probe malformed — don't inject)
```

The gate (`crates/ccteam-cli/src/commands.rs::
run_check_codex_auto_critic`) wraps a `<bin> --version` probe AND a
one-shot `<bin> exec --json --skip-git-repo-check` canary so the
skill doesn't have to second-guess the result of two inline probes
under different failure modes (broken install, auth missing, output
schema drift). Honors `$CCTEAM_CODEX_BIN` for hermetic tests.

- **exit 0** (`available: true`, `probe: "ok"`) → set
  `auto_critic_vendor = "codex"` for the matched role; the
  workflow.yaml render in Phase 5.3 injects
  `agents.<role>.executor: codex`. PROJECT PLAN line:
  "Codex critic: auto-enabled".
- **exit 2** (`available: false`) → silent fallback to
  `vendor: claude` for that role; PROJECT PLAN line:
  "Codex critic: unavailable (codex CLI not installed / not
  authenticated)". User can opt in later by re-running the gate
  after `codex login`.
- **exit 3** (`available: true`, `probe: "malformed"`) → still
  silent fallback (don't inject `executor: codex` until the operator's
  codex install is fixed); PROJECT PLAN line:
  "Codex critic: unavailable (codex exec output malformed — see
  `ccteam doctor --check-codex-auto-critic`)".
- Persona is not a critic-flavoured role → PROJECT PLAN line:
  "Codex critic: not applicable" (or omit entirely for brevity).

The persona `.md` body copied in Phase 5.4 is unchanged — vendor
selection is purely a `workflow.yaml::agents.<role>.executor: codex`
injection. The user does not edit YAML at any point.

## Phase 3.6 — Project probe (sensible scope defaults)

Before rendering the PROJECT PLAN, probe the target project root so
the rendered `workflow.yaml` ships with a `scope:` field already
pointing at the right subtree. Without this, every freshly-created
workflow has an empty `scope:` and the user has to hand-edit before
the first spawn picks the right cwd — defeating the per-role scope
blast-radius guarantee that keeps large-codebase agents focused.

```bash
ccteam probe-project --path <project_dir> --json
```

Returns:

```json
{
  "kind": "monorepo" | "single-repo" | "docs-only" | "scripts-only" | "empty",
  "languages": ["rust", "typescript", ...],
  "has_tests": true | false,
  "probable_scope": ["crates/foo/src", "crates/bar/src", ...]
}
```

Use the result as input to Phase 5.3's template ctx:

- For `bg-overnight`, set `scope_yaml` to `"    scope: <probable_scope[0]>\n"`
  so each `agents.<role>` block embeds the scope (or call
  `ccteam_core::apply_probe_defaults_to_workflow_ctx(&mut ctx, preset,
  &probe)` if you're rendering in-process).
- For `inproc-solo` / `inproc-team`, surface `probable_scope[0]` in the
  PROJECT PLAN "Files I'll write" section as a comment hint — the
  agent-team preset renders `agents: {}` so per-role scope is set by
  the lead at Task-spawn time, not by the bootstrap.
- For `chat-pocket` / `chat-squad`, chat bots run at project root by
  design — no scope override needed.

Detection is pure file-existence sweep (no source parsing, no LLM
call) so the probe is fast (~10 ms even on the ccteam repo) and
deterministic.

**Edge cases:**

- `kind == "empty"` → the user likely pointed at a fresh dir; PROJECT
  PLAN shows `scope: (none — fresh repo)`.
- `kind == "monorepo"` with > 3 first-party crates → probe caps at
  top-3 by descending LOC, ties broken alphabetically. The skill
  surfaces "(top 3 of N detected)" in PROJECT PLAN so the user knows
  to widen if needed.
- `kind == "docs-only"` + `bg-overnight` is unusual but valid — the
  probe returns `scope: ["docs"]` and the user can override.

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

# Overlay sensible scope defaults from the project probe (Phase 3.6)
# so the rendered yaml's `scope:` is non-empty out of the box. Skill
# callers without in-process access can `ccteam probe-project --json`
# and set `scope_yaml` manually.
apply_probe_defaults_to_workflow_ctx(&mut ctx, preset, &probe)

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

You do **not** need to mint a handle yourself. When you call
`mcp__ccteam__chat_register_bot` in Phase 5.6 without a `chat_handle`
argument, the MCP handler auto-mints the first unused scientist
nickname from `agent_naming::SCIENTIST_NAMES`
(`@crates/ccteam-core/src/agent_naming.rs`) and reports the chosen
handle in its reply.

If you want to pin a specific handle (user override, persona-coupled
naming, etc.) pass `chat_handle: "<name>"` explicitly — alphanumeric
plus `_` / `-` only, no leading `@`. The dispatcher then skips the
auto-mint and persists the supplied value.

## 5.6  Register the bot

For chat presets, invoke the **`mcp__ccteam__chat_register_bot`** MCP
tool. The skill is LLM-driven and cannot call Rust functions directly
— the registry is reached only through this MCP wire. The handler
writes `~/.ccteam/imd/registry/<workflow_slug>/<role>.json`; the
daemon's registry watcher picks it up and spawns the tmux session.

`im_chat_id` comes from `~/.ccteam/im/credentials.json` written in
Phase 5.1: take `telegram.allowed_chat_ids[0]` (cast to string —
Telegram chat ids are i64, but the MCP wire expects a string).

Always pass `project_dir` (absolute path of the directory holding
`.ccteam/workflow.yaml`, i.e. the `project_dir` you used in Phase 5.3 /
5.4 above) so the daemon can find the project no matter where the
user keeps it. Projects outside the default `~/projects/<slug>/`
layout (NAS shares, dir basename ≠ workflow slug) only resolve when
this field is set. Omitting it falls back to the MCP server's current
working directory.

```json
{
  "name": "mcp__ccteam__chat_register_bot",
  "arguments": {
    "workflow_slug": "<slug from 5.2>",
    "role": "<persona_id from Phase 3>",
    "vendor": "claude",
    "im_platform": "telegram",
    "im_chat_id": "<allowed_chat_ids[0]>",
    "persona_id": "<persona_id from Phase 3>",
    "project_dir": "<absolute path of the dir containing .ccteam/workflow.yaml>"
  }
}
```

When `chat_handle` is omitted (as above) the dispatcher returns the
auto-minted handle (and echoes the resolved `project_dir`) in the
response:

```json
{
  "ok": true,
  "path": "/home/.../helper.json",
  "workflow_slug": "<slug>",
  "role": "<role>",
  "chat_handle": "Euclid",
  "project_dir": "/abs/path/to/project"
}
```

Read `chat_handle` from the reply so Phase 5.9's user message can
quote `@Euclid` (or whichever name was assigned).

**Vendor must be lowercase** (`"claude"` or `"codex"`). The daemon's
`BotRegistration` deserialize trips on PascalCase `"Claude"`; the
dispatcher lowercases defensively, but stick to lowercase in the
call to be explicit.

**Error handling:**

- Response `{"ok": true, "path": "...", "chat_handle": "..."}` →
  continue to Phase 5.7; surface the minted handle to the user later.
- Response `{"ok": false, "error": "already_registered", "path": "..."}`
  → **idempotent OK**, this is a re-run of the same creator dialogue;
  log the line `Bot already registered at <path>; reusing` and
  continue. **Do not** call `chat_unregister_bot` to retry — that
  would race the daemon's tmux session.
- Any other error (validation, IO) → **STOP**. Surface to user:
  `"registry write failed: <error from response>"`. Do not proceed
  to Phase 5.7 / 5.8 — leaving a half-installed bot with workflow.yaml
  but no registration is worse than failing the dialogue.

The daemon's router resolves `@<chat_handle>` → `(slug, role)` from
the registry directly; no `chat.bot_name` plumbing through
workflow.yaml is required. Two bots in different slugs sharing the
same effective handle collide deterministically — the second claimant
(in `(slug, role)` sort order) receives a `__<slug>` suffix (double
underscore so the suffixed handle stays inside the IM mention
charset and users can actually type `@curie__beta`).

## 5.7  Project-level `.mcp.json`

```
render_project_mcp_json(current_ccteam_bin()) → project_dir/.mcp.json
```

Merges into existing `.mcp.json` if present (does not clobber other
servers).

## 5.8  User reply

```
好了 ✓

  Bot:    <@handle>
  Slug:   <slug>
  Mode:   <preset>
  Persona: <label>
  Bot 注册到 ~/.ccteam/imd/registry/<slug>/<role>.json ✓

在 <IM platform> 私聊 <@handle> 就能开聊。要看状态:
  /ccteam-control show <slug>

要卸载这个 bot:
  invoke mcp__ccteam__chat_unregister_bot{workflow_slug: <slug>, role: <role>}
  (or `ccteam remove <slug> --purge` to wipe everything for the slug)
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
一个目前不支持,请从内置库选" and bounce.

Never silently re-execute Phase 5 steps that already ran — if
`workflow.yaml` is already on disk, ask before overwriting.

---

# What this skill does NOT do

- **User-defined personas** — the prefab library is fixed at 7
  personas; user-defined persona creator flow is not in scope.
- **Persona marketplace pull** — out of scope.
- **OAuth-based IM onboarding** — `/ccteam-im-setup` is token-only.
- **Voice / multi-modal input** — text only.
- **Cross-vendor agent vendoring** — Codex critic auto-enable is
  declarative (a hint in PROJECT PLAN); actual Codex vendoring is
  handled by the daemon vendor adapters.
