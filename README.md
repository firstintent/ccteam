# ccteam

> **Autonomous project orchestrator built on Claude Code.** Hand it a one-line idea — it spins up a long-running tmux session, runs a multi-phase pipeline (plan → implement → test → fix → ship), and gets out of your way until the project ships or genuinely needs you.

## What ccteam does

ccteam is a meta-tool that **schedules Claude Code sessions across many projects in parallel**, accumulates lessons across them, and asks for your input only at decision points that actually need a human.

You talk to a **meta-agent** — a permanent Claude Code session that understands your intent and dispatches projects on your behalf:

```
You:    "Make a Rust CLI for managing bookmarks, SQLite-backed."
Meta:   "I'll dispatch the dev team. Slug = dev-bookmark-cli-rust-sqlite.
         Tracking via `ccteam show dev-bookmark-cli-rust-sqlite`."
[~30 minutes later]
Meta:   "dev-bookmark-cli-rust-sqlite shipped. 22 tests green, $4.80
         cost, retro lessons appended to ~/.claude/rules/ccteam-lessons-dev.md."
```

For uncertain ideas, ccteam ships a separate research team that produces a verdict before you commit:

```
You:    "I have an idea — AI recipe generator from fridge photos. Worth doing?"
Meta:   "I'll route to product-research. It returns PASS/CONCERN/REJECT/CLARIFY."
[product-research runs 5–6 phases, $1–3 cost]
Meta:   "verdict: REJECT. Three saturated competitors plus per-photo cost is
         prohibitive. Detailed rationale in ~/projects/product-research-…/.ccteam/
         rationale.md. Lessons appended to product-research lessons file."
```

You can also **author your own teams** — describe a workflow in natural language and ccteam scaffolds a Claude Code plugin you can run, share, or publish:

```
You:    "I want a team for iterating an existing app: new requirement →
         feasibility → architecture → implementation → tests → release."
Meta:   "Drafting a custom team. Plugin manifest + 6 phase markdowns
         under ~/.config/ccteam/teams/iter-app/. Run
         `ccteam team publish iter-app --target local` when ready."
```

## Why ccteam

If you've used Claude Code for serious work, you've hit some of these:

- **AI still requires you as PM**: clarifying the same context, restating preferences, watching for drift
- **The session stops mid-task waiting for input** you weren't there to give
- **Bug-fix infinite loops**: re-running tests forever instead of escalating
- **Many ideas, none ship**: focus only on one project at a time
- **Some ideas aren't worth doing** but you only learn after burning a week
- **Each new project starts from zero**: yesterday's lessons are forgotten

ccteam attacks all of these with engineering discipline:

| Pain | ccteam mechanism |
|---|---|
| AI requires you to PM | Meta-agent dispatcher; idle-aware injection; decisions queue |
| Sessions stop waiting for input | Phases self-loop until they reach a structured exit (`phase_done` / `escalate` / `outbox-question`); `AskUserQuestion` is intercepted and rerouted to a structured decisions outbox |
| Bug-fix infinite loops | Hard 3-strike fix-loop ceiling, then escalate with "what tried, what failed" |
| Many ideas pile up | Long-running tmux sessions per project, queue with `max_concurrent_projects` |
| Not every idea is worth doing | Separate `product-research` team produces a verdict before dev work |
| Each project from zero | Cross-project memory via `~/.claude/rules/` + per-repo auto-memory |
| Stuck and you don't notice | Watchdog scans all sessions, surfaces stalls / cost overruns / dead daemon |

## Install

**Requirements**: Linux or WSL2, `tmux ≥ 3.0`, Claude Code CLI `≥ 2.1.59`, `cargo` (stable Rust), `git`.

```bash
git clone git@github.com:firstintent/ccteam.git ~/workplace/agents/ccteam
cd ~/workplace/agents/ccteam
make install
ccteam --version
```

One-time setup (idempotent — re-runs are no-ops):

```bash
make setup HANDLE=<your-handle>   # init + 4 doctor installs + tool-surface health check
```

`<your-handle>` is whatever you want to call yourself — snake_case, e.g. `cto` or `alice`.

<details>
<summary>What <code>make setup</code> runs (if you want to do it by hand)</summary>

```bash
ccteam init                                       # ~/.ccteam/ skeleton
ccteam doctor --install-skill                     # ccteam-control skill (any claude session reaches ccteam)
ccteam doctor --install-mcp                       # 9-tool MCP server in ~/.claude.json
ccteam doctor --install-memory-bridge             # ~/.claude/rules/ccteam-lessons-<team>.md placeholders
ccteam doctor --install-meta-agent <your-handle>  # bootstrap your meta-agent project
ccteam doctor --tool-surface                      # health check (must be green)
```

</details>

> **Upgrading from a pre-2026-05 ccteam?** Run `ccteam doctor --migrate-recommended-agents` once after the upgrade. Spawned project sessions now resolve plugin agents through Claude Code's plugin pipeline, so the legacy `~/.claude/agents/<name>.md` symlinks ccteam used to create are obsolete; this command removes them. User-authored agents are preserved.

## Quick start

```bash
# Terminal A — start the orchestrator (foreground; keep this open)
make start                              # or: ccteam start --foreground

# Terminal B — talk to the meta-agent
make attach HANDLE=<your-handle>        # or: tmux attach -t ccteam-meta-<your-handle>
# Then type in natural language:
#   "Make a markdown editor, web-based, single HTML file, no build step."
# Or for fuzzy ideas:
#   "I'm thinking of building X — should I bother?"
# Or to author a custom team:
#   "Help me design a workflow for iterating an existing app."
```

Or from any other claude session anywhere on your machine:

```bash
cd ~/anywhere
claude
# Then: "ccteam, what projects are running?"
# Or:   "Dispatch a dev team to make a todo CLI in Python."
```

The meta-agent uses the `ccteam-control` skill plus the `ccteam-mcp` 9-tool MCP server you installed, so any Claude Code session understands ccteam.

## Web dashboard (V0.3)

A read+write web UI for browsing all projects, watching events stream live, and dispatching actions to running sessions:

```bash
# Local-only (no auth required)
ccteam web --bind 127.0.0.1:7331
# → open http://127.0.0.1:7331

# LAN-accessible (auto-generates token at ~/.ccteam/web-token, mode 0600)
ccteam web --bind 0.0.0.0:7331
# → token printed to stderr; pass as ?token=ccteam:<token> on first visit
#   (browser then stores HttpOnly cookie; subsequent navigation seamless)
```

Surface:

- `/` — project list (slug / team / phase / last event / status badge / cost)
- `/project/<slug>` — detail page (state + last 200 events + outbox + tmux pane screenshot)
- Live updates via Server-Sent Events
- Write actions: send `/btw <text>`, inject decisions, pause/resume per project

V0.3.1 adds flex teams for manual multi-session work. A flex project has no phase DAG:
ccteam keeps observability, harness snapshots, progress streams, and web controls while you drive
Claude Code sessions directly.

```bash
ccteam team init scratch --kind flex --author-name "$USER"
ccteam team publish scratch --target local
ccteam new migration --team scratch          # creates ~/projects/scratch-migration/
ccteam session add scratch-migration --harness=claude
ccteam session ls scratch-migration
```

The dashboard shows `kind=flex`; `/project/<slug>` lists session cards, and
`/session/<slug>/<sid>` shows that session's events, harness snapshot, pane snapshot, screenshot
fallback, and sid-scoped `/btw` control.

Security: non-loopback bind requires `Authorization: Bearer ccteam:<token>` (or the cookie set by the URL shim) on every request — this header doubles as the CSRF token for write actions, since browsers won't auto-attach `Authorization` on cross-origin form submissions. `--no-auth` opt-out shows a 5-second stderr countdown so accidents are recoverable. See [`docs/v0-3/prd.md §9`](docs/v0-3/prd.md) for the full threat model.

## Built-in teams

| Team | What it ships | When to use |
|---|---|---|
| `dev` | A working software project — plan → implement → test → fix → ship | You've decided what to build |
| `product-research` | A verdict (PASS / CONCERN / REJECT / CLARIFY) with rationale — market signals, technical feasibility, differentiation | You're not sure if it's worth building |
| Your own | Custom phase pipeline authored via the team factory; ships as a Claude Code plugin you can share | Your workflow doesn't fit dev / product-research |

To author a custom team, ask the meta-agent:

```
"Design a team for me: <describe your workflow>"
```

The meta-agent walks you through phase definition, tools required, golden-rule constraints, and retro-schema fields, then scaffolds the plugin under `~/.config/ccteam/teams/<name>/`. Publish with:

```bash
ccteam team publish <name> --target local              # link into ccteam-local marketplace
ccteam team publish <name> --target github --repo ...  # push to a GitHub repo as a shareable plugin
```

Anyone who installs your team-plugin via Claude Code's plugin commands gets a fully-functional ccteam team.

## How it works

ccteam is a **three-tier architecture**:

```
┌─────────────────────────────────────────────────────────────┐
│  User interaction layer                                     │
│   ┌──────────────────┐    ┌──────────────────────────┐      │
│   │ ccteam-meta-X    │    │ daily-driver claude      │      │
│   │  (permanent NL   │    │  (any session, w/ skill  │      │
│   │   dispatcher)    │    │   + MCP)                 │      │
│   └──────────────────┘    └──────────────────────────┘      │
└────────────────────────┬────────────────────────────────────┘
                         │
┌────────────────────────▼────────────────────────────────────┐
│  Orchestration layer (Rust daemon)                          │
│   - tmux long sessions per project                          │
│   - file-system control plane (~/.ccteam/, progress.jsonl)  │
│   - hooks-based state machine                               │
│   - phase DAG per team, three-layer team resolution         │
│     (project > user > repo)                                 │
└────────────────────────┬────────────────────────────────────┘
                         │
┌────────────────────────▼────────────────────────────────────┐
│  Per-project Claude Code session                            │
│   - sub-skill auto-trigger at phase boundaries              │
│   - golden-rule enforcement                                 │
│   - structured exits: phase_done / escalate / outbox        │
│   - Stop-hook-driven self-loop                              │
└─────────────────────────────────────────────────────────────┘
```

Key invariants:

- **`progress.jsonl` is the single source of state truth** — orchestrator never parses tmux output
- **Long-running sessions, never killed** — soft warns at 5/15/30 min idle; only hard kill is at $200 cumulative cost
- **Three-strike fix-loop** — same fix attempted ≤ 3 times, then escalates with diagnostics
- **Phases self-loop** — when a phase doesn't produce a structured exit, the Stop hook reminds the model to continue (or escalates after a recursion guard fires); `AskUserQuestion` is intercepted at `PreToolUse` and rerouted to a decisions outbox so the user can answer asynchronously
- **Cross-project memory via Claude Code's official mechanisms** — no self-built RAG; uses `~/.claude/rules/ccteam-lessons-<team>.md` (auto-loaded at session start) + per-repo auto-memory + optional [`claude-mem`](https://github.com/thedotmack/claude-mem) MCP for deep search
- **Watchdog as translation layer** — surfaces stall / cost / daemon-down / auto-loop iteration alerts to the meta-agent; never mutates orchestrator state

For the full design and protocol reference, see [`docs/tech-design.md`](docs/tech-design.md) and [`docs/interfaces.md`](docs/interfaces.md).

## Documentation

Full documentation index: [`docs/README.md`](docs/README.md).

| Doc | Audience | Purpose |
|---|---|---|
| [`docs/v0-1/user-quickstart.md`](docs/v0-1/user-quickstart.md) | end users | hands-on walkthrough — install through dispatching a dev project end-to-end |
| [`docs/v0-2/team-factory-userguide.md`](docs/v0-2/team-factory-userguide.md) | end users | authoring and publishing your own teams |
| [`docs/requirements.md`](docs/requirements.md) | contributors | user pain points (acceptance baseline) |
| [`docs/tech-design.md`](docs/tech-design.md) | contributors | architecture, three-tier model, invariants |
| [`docs/interfaces.md`](docs/interfaces.md) | contributors | protocol reference (state.json / phase YAML / events / CLI / MCP) |
| [`docs/dev-coupling-audit.md`](docs/dev-coupling-audit.md) | contributors | F-finding tracker (cross-version) |
| [`CLAUDE.md`](CLAUDE.md) | AI sessions | implementation rules + red lines (consumed by Claude Code in this repo) |

## Contributing

ccteam is itself developed using Claude Code under the worktree-per-task pattern.

1. Fork + clone
2. `git worktree add -b feat/your-thing /tmp/ccteam-feature origin/main`
3. Read [`CLAUDE.md`](CLAUDE.md) and [`docs/README.md`](docs/README.md) — implementation rules apply equally to humans and AI
4. Every PR maps to a pain point in `requirements.md` + a section in `tech-design.md` + a task in the current version's `dev-plan.md`
5. `cargo test --workspace` must stay green; `cargo clippy --workspace --all-targets` no new warnings
6. Open a PR; the maintainer reviews

Documentation is in Chinese (the project's working language); commit messages and PR descriptions are in English.

## License

See [`LICENSE`](LICENSE).

## Acknowledgments

- [Claude Code](https://code.claude.com/) — the runtime that makes this all possible
- [`claude-plugins-official`](https://github.com/anthropics/claude-plugins) — many phase sub-skills are referenced from this marketplace
- [OpenAI Symphony](https://github.com/openai/symphony) — long-term architectural reference for orchestration model
