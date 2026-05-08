# Team Factory — User Guide (V0.2 M0.22)

> Goal: ship a custom ccteam team as a Claude Code plugin, sharable via
> a local marketplace symlink or a GitHub repo. This guide walks you
> through the dialogue + commands. ~10 minutes.

## When you want this

- The user wants a workflow `dev` and `product-research` don't cover —
  custom phases, custom golden rules, custom retro fields.
- You want to share a team with another machine / another user — the
  plugin format makes the artifact installable anywhere Claude Code
  runs.

## What you produce

```text
~/.config/ccteam/teams/<name>/         # staging tree (private)
  .claude-plugin/plugin.json           # Claude Code plugin manifest
  team.yaml                            # ccteam team config
  phases/01-<phase>.md                 # phase markdown templates
  README.md
```

After `publish --target local`, this tree gets symlinked into
`~/.claude/plugins/marketplaces/ccteam-local/plugins/<name>/`. After
`publish --target github`, it gets pushed to a new repo as the
sharable artifact.

## Step 1 — interview the user (skill-driven)

Inside any Claude Code session that has the `ccteam-team-author` skill
installed (auto-installed by `ccteam doctor --install-skill --force`
once shipped, or manually by the meta-agent):

```bash
# In a meta-agent session:
"我想做个 marketing 团队,phase 跑 5 步"
```

The skill walks you through:

1. plugin metadata (`name`, `description`, `author`)
2. phase list (count + per-phase name / inputs / outputs / auto_loop)
3. tools per phase (subagents / skills / MCP)
4. golden rules (`protocol` cmd-check / prompt-directive, `domain`)
5. retro schema (M4.1 retro fields)
6. verdict schema (research-style teams only)
7. ESCALATE grammar extensions (optional)

One question at a time — the skill answers one user question per turn.

## Step 2 — scaffold the staging tree

The skill drives `ccteam team init` for you. The CLI direct path:

```bash
ccteam team init my-team \
  --description "Custom marketing-research team" \
  --author-name "Alice"
```

Result: a starter staging tree at `~/.config/ccteam/teams/my-team/`
with a single `intake` phase. Edit the phase markdown bodies to
fill in the actual domain task. **Do not** add `PHASE_DONE: …` /
`ESCALATE: …` to the body text — those are inject-prompt-only (V0.2
M0.18); the body is pure domain task description.

## Step 3 — validate

```bash
ccteam doctor --validate-team my-team
```

Reads:
- `[OK] plugin.json` — manifest schema OK
- `[OK] team.yaml` — TeamSpec validate OK
- per-phase `[OK]` / `[WARN]` lines (phase frontmatter + IO contract)
- `[FAIL]` for anything broken

Fix the `[FAIL]` lines before publishing.

## Step 4 — publish

### Local marketplace

```bash
ccteam team publish my-team --target local
# Then in any Claude Code session:
claude /plugin enable my-team@ccteam-local
```

The team is now available wherever Claude Code reads
`~/.claude/plugins/marketplaces/`.

### GitHub repo

```bash
gh auth login                                 # one-time
ccteam team publish my-team \
  --target github \
  --repo alice/ccteam-marketing
# Output:
#   pushed → https://github.com/alice/ccteam-marketing
#   share with: claude /plugin add alice/ccteam-marketing
```

The factory shells out to `gh repo create` + `git push`; it does not
embed credentials. If `gh auth status` fails, the publish fails loud —
re-run after `gh auth login`.

## How it integrates with Claude Code

The artifact is a **standard Claude Code plugin**. Claude Code's plugin
loader reads:

- `.claude-plugin/plugin.json` — strict schema (`name`, `description`,
  `author`); ccteam additions (`team.yaml` at the plugin root) are
  silently ignored by the loader (zod default `strip`).
- `agents/` / `commands/` / `skills/` / `hooks/hooks.json` / `.mcp.json`
  — auto-discovered when present. The factory does not write these by
  default; the team author can drop them in by hand once the staging
  tree exists.

ccteam reads `team.yaml` directly via its own `team_resolver` — the
file is invisible to Claude Code's plugin pipeline.

## Limits in V0.2

- The factory writes one starter phase. Multi-phase teams come from the
  team-author skill dialogue (drives the same `init_team_staging`
  primitive multiple times) or by hand-editing the staging tree.
- Plugin `userConfig` (manifest field for "user fills these on enable")
  is supported by Claude Code but the factory does not yet emit it.
  Add by hand to `.claude-plugin/plugin.json` post-init.
- Plugin `dependencies` (team-plugin-A depends on `code-reviewer@…`)
  is supported by Claude Code; the factory does not yet emit it.
  Add by hand post-init.
- `--target github` requires `gh` CLI installed + authenticated.
  V0.3 may add a pre-flight check earlier in the pipeline.

## Troubleshooting

| symptom | fix |
|---|---|
| `staging dir … not found` on publish | run `ccteam team init <name>` first |
| `validation failed — fix the [FAIL] lines` | re-run `ccteam doctor --validate-team <name>` to see what broke |
| `gh CLI not found` | install <https://cli.github.com>, then `gh auth login` |
| `plugin.json` schema rejected by Claude Code | check `name` is ascii-lowercase / `-` / `_`, `description` non-empty, `author.name` non-empty |
| team.yaml.name doesn't match plugin.json.name | the factory keeps them in lock-step — if you hand-edit, keep both consistent |

## Reference

- `crates/ccteam-core/src/team_factory.rs` — staging + publish + validate
  primitives (unit-tested).
- `crates/ccteam-cli/src/team_factory_cli.rs` — CLI handlers for
  `ccteam team {init,publish}`.
- `docs/tech-design.md` §6.12 — design decisions.
- `docs/interfaces.md` §5.1 / §5.5 — phase frontmatter + team.yaml
  schema (the factory writes these).
- `docs/v0-2-claude-code-alignment-review.md` §2 — why we use the
  Claude Code plugin format instead of inventing a ccteam-specific
  packaging.
