---
name: ccteam-team-author
description: Author a new ccteam team plugin via dialogue with the user. Use when the user asks to create / design / scaffold a team, or wants to package a custom phase pipeline as a Claude Code plugin. Primary consumer is the ccteam meta-agent session — it walks the user through phase list, tools, golden rules, retro schema, verdict schema, and plugin metadata, then invokes `ccteam team init` / `ccteam team publish` to materialize and share the result.
---

# ccteam-team-author

ccteam ships two teams out of the box (`dev`, `product-research`). When
the user wants a different workflow — code review only, market research
only, custom multi-phase research, anything — author a new team. The
factory produces a Claude Code plugin (with a ccteam `team.yaml` riding
as a top-level unknown field) so the same artifact installs anywhere
Claude Code runs.

## Capability index

| What you want | Bash command |
|---|---|
| Scaffold a team from interview answers | `ccteam team init <name> --description "…"` |
| Validate a staged team | `ccteam doctor --validate-team <name>` |
| Publish to local marketplace        | `ccteam team publish <name> --target local` |
| Publish to GitHub                   | `ccteam team publish <name> --target github --repo <owner>/<name>` |
| Inspect what was generated          | `ls ~/.config/ccteam/teams/<name>/` |

Staging path: `~/.config/ccteam/teams/<name>/`. Layout mirrors a Claude
Code plugin (`.claude-plugin/plugin.json` + `team.yaml` + `phases/` +
optional `agents/` `commands/` `hooks/hooks.json` `.mcp.json`).

## Typical workflow — interview the user, then scaffold

The factory expects answers to a small set of questions before
`ccteam team init` can produce a useful skeleton. Walk the user through
them one at a time — do **not** dump every question in a single turn.

### A) Plugin metadata

1. Plugin / team `name` — must be ascii lowercase + `-` + digits.
   Doubles as `team.yaml.name`.
2. One-line `description` (shown in `ccteam ls --teams`,
   `marketplace.json`).
3. `author.name` (and optionally `author.email`).

### B) Phase list

1. How many phases? (Typical range 3 – 8.)
2. For each phase, in order:
   - `name` (kebab-case)
   - one-line task description
   - `required_inputs` (file paths under `.ccteam/`, eg `.ccteam/spec.md`)
   - `required_outputs` (eg `.ccteam/<phase>.md`)
   - `parallelism`: `solo` (default) — `agent_team` is M3+, rare
   - `auto_loop`: should the assistant self-loop to retry until it emits
     a completion signal? (Default: yes — V0.2 M0.19.)
3. Optional: a `verdict` phase (writes `.ccteam/verdict.md` with
   PASS / CONCERN / REJECT / CLARIFY) — research-style teams use this.

### C) Tools each phase needs

Per phase, ask: which subagents / skills / MCP servers does this phase
invoke? (Common subagents: `code-reviewer`, `code-architect`,
`code-explorer`, `silent-failure-hunter`.) The factory writes
`tools_required:` for each phase; `ccteam doctor --validate-team` then
cross-checks reachability.

### D) Golden rules (team-wide)

V0.2 split into `protocol` (orchestrator-enforced) and `domain`
(prompt-only):

- `protocol` with `enforce: cmd_check` — runs at phase boundary
  (eg `cargo test --workspace`).
- `protocol` with `enforce: prompt_directive` — text injected into every
  phase's inject prompt. Default-include the `forbid_ask_user_question`
  directive (V0.2 M0.19) — no team should let the assistant block on
  AskUserQuestion.
- `domain` — pure prompt guidance ("prefer small PRs", "no SQL string
  interpolation").

### E) Retro schema

Fields the M4.1 retro phase will fill. Empty list = team has no retro.
Fields are stable per team (renaming invalidates indexed history).
Default for code teams: `tech_stack`, `pitfalls`, `successful_designs`,
`do_not_do_again`.

### F) Verdict schema (research / decision teams only)

Phase names that emit `verdict.md` with PASS / CONCERN / REJECT /
CLARIFY. Empty = team doesn't produce a verdict.

### G) ESCALATE grammar extensions (optional)

Team-specific ESCALATE prefixes the Stop hook should recognize. Each
prefix has a `route`: `revert_to_phase` (with `target_phase`) /
`need_user_input` / `abort`. Default = empty (the four built-in
prefixes — `REVERT_TO_PHASE` / `NEED_USER_INPUT` / `ABORT` /
`INSUFFICIENT_CLARIFICATION` — already cover most cases).

## Decision principles

- **One question at a time.** The user is collaborating with you in real
  time; batching 7 questions into one turn loses signal. Walk through
  A → G in order; let the user answer; move on.
- **Defaults exist.** Each frontmatter / `team.yaml` field has a default.
  When the user doesn't have a strong opinion, accept the default and
  move on.
- **Phase markdown bodies are user territory.** The factory writes a
  domain-task template (1–2 sections); the user is encouraged to flesh
  it out post-`init`. Do not write protocol literals (`PHASE_DONE: …` /
  `ESCALATE: …`) into the body — those are inject-prompt territory only
  (V0.2 M0.18).
- **Validate before publishing.** Always run
  `ccteam doctor --validate-team <name>` after `init` and before
  `publish`. Fix any `[FAIL]`; surface `[WARN]` to the user.

## Publish targets

- `--target local` — link the staging dir to
  `~/.claude/plugins/marketplaces/ccteam-local/plugins/<name>/`. The
  user runs `claude /plugin enable <name>@ccteam-local` and the team
  becomes available immediately. Share = give the staging path.
- `--target github` — `gh repo create` + push. Output is a GitHub URL
  the user can give to anyone; install side does
  `claude /plugin add <owner>/<repo>` per Claude Code marketplace
  conventions. Requires `gh auth login` first; the command fails loud
  if `gh` is missing or unauthenticated.

## What this skill cannot do

- It cannot write phase markdown bodies for the user — those are domain
  knowledge (the user's "what does this phase actually do"). The
  factory writes a 5-line task-description template for each phase; the
  user fills in the rest by editing the staging `phases/<name>.md`
  files.
- It cannot decide whether a team is needed — only the user knows their
  workflow. If the request maps cleanly to `dev` or `product-research`,
  use those instead.
- It does not auto-install the plugin in any session. Publish writes the
  artifact; the user enables it in each Claude Code session that wants
  it (`/plugin enable`).
