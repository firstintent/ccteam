# ccteam

> **Autonomous project orchestrator built on Claude Code.** Hand it a one-line idea — it spins up a long-running tmux session, runs a multi-phase pipeline (plan → implement → test → fix → ship), and gets out of your way until it ships or genuinely needs you.

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

## Why ccteam

If you've used Claude Code for serious work, you've hit some of these:

- **AI still requires you as PM**: clarifying the same context, restating preferences, watching for drift
- **Bug-fix infinite loops**: re-running tests forever instead of escalating
- **Many ideas, none ship**: focus only on one project at a time
- **Some ideas aren't worth doing** but you only learn after burning a week
- **Each new project starts from zero**: yesterday's lessons are forgotten

ccteam attacks all of these with engineering discipline:

| Pain | ccteam mechanism |
|---|---|
| AI requires you to PM | Meta-agent dispatcher; idle-aware injection; decisions queue |
| Bug-fix infinite loops | Hard 3-strike fix-loop ceiling, then escalate with "what tried, what failed" |
| Many ideas pile up | Long-running tmux sessions per project, queue with `max_concurrent_projects` |
| Not every idea is worth doing | Separate `product-research` team produces a verdict before dev work |
| Each project from zero | Cross-project memory via official `~/.claude/rules/` + per-repo auto-memory |

## Quick install

**Requirements**: Linux or WSL2, `tmux ≥ 3.0`, Claude Code CLI `≥ 2.1.59`, `cargo` (stable Rust), `git`.

```bash
git clone git@github.com:firstintent/ccteam.git ~/workplace/agents/ccteam
cd ~/workplace/agents/ccteam
make install
ccteam --version          # confirm install
```

One-time setup (idempotent — re-runs are no-ops):

```bash
ccteam init                                       # ~/.ccteam/ skeleton
ccteam doctor --install-recommended-agents        # link plugin agents to ~/.claude/agents/
ccteam doctor --tool-surface                      # health check (must be green)
ccteam doctor --install-skill                     # ccteam-control skill (any claude session reaches ccteam)
ccteam doctor --install-mcp                       # 9-tool MCP server in ~/.claude.json
ccteam doctor --install-memory-bridge             # ~/.claude/rules/ccteam-lessons-{dev,product-research}.md
ccteam doctor --install-meta-agent <your-handle>  # bootstrap your meta-agent project
```

Replace `<your-handle>` with whatever you want to call yourself (snake_case, e.g. `rob` / `alice`).

## Quick start

```bash
# Terminal A — start the orchestrator (foreground; keep this open)
ccteam start --foreground

# Terminal B — talk to the meta-agent
tmux attach -t ccteam-meta-<your-handle>
# Then type in natural language:
#   "Make a markdown editor, web-based, single HTML file, no build step."
# Or for fuzzy ideas:
#   "I'm thinking of building X — should I bother?"
```

Or from any other claude session anywhere on your machine:

```bash
cd ~/anywhere
claude
# Then: "ccteam, what projects are running?"
# Or:   "Dispatch a dev team to make a todo CLI in Python."
```

The meta-agent uses the `ccteam-control` skill plus the `ccteam-mcp` 9-tool MCP server you installed, so any Claude Code session understands ccteam.

## How it works

ccteam is a **three-tier architecture**:

```
┌─────────────────────────────────────────────────────────────┐
│  Channel layer (M2+ optional — Telegram / Feishu adapters) │
└────────────────────────┬────────────────────────────────────┘
                         │
┌────────────────────────▼────────────────────────────────────┐
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
│   - phase DAG per team (dev / product-research / …)         │
└─────────────────────────────────────────────────────────────┘
```

Key invariants:

- **`progress.jsonl` is the single source of state truth** — orchestrator never parses tmux output
- **Long-running sessions, never killed** — soft warns at 5/15/30 min idle; only hard kill is at $200 cumulative cost
- **Three-strike fix-loop** — same fix attempted ≤ 3 times, then escalates with diagnostics
- **Cross-project memory via Claude Code's official mechanisms** — no self-built RAG; uses `~/.claude/rules/ccteam-lessons-<team>.md` (auto-loaded) + per-repo auto-memory + optional [`claude-mem`](https://github.com/thedotmack/claude-mem) MCP for deep search

For the full design, see [`docs/tech-design.md`](docs/tech-design.md).

## What's shipped

| Milestone | Status |
|---|---|
| M0 — single-project CLI MVP | ✅ |
| M0.5 — tool surface (plugin agent ln -sf, validation, doctor flags) | ✅ |
| M1 — meta-agent + decisions queue + inbox/outbox protocol | ✅ |
| M2 — sub-skill auto-trigger / phase YAML / `ccteam-mcp` 9 tools / golden_rules | ✅ |
| M3 — team abstraction + product-research team | ✅ |
| M4.1–M4.4 — cross-project memory (rules + auto-memory + claude-mem optional) | ✅ |
| M4.5+ — audit matrix / voting / `multi_session` parallelism / TUI | 🚧 planned |
| M5 — Critic agent + anti-leniency | 🚧 planned |
| M6 — Symphony-scale (multi-module DAG, weeks-long projects) | 🔮 open exploration |

## User guide

The complete walkthrough — install, every doctor flag, dispatch a dev project end-to-end, dispatch a product-research project, intervene when stuck, decisions queue — is at:

→ [`docs/user-quickstart-v0.1.md`](docs/user-quickstart-v0.1.md)

The user guide is **versioned**: when M5 ships, `v0.2` will appear without mutating v0.1.

## Documentation map

| Doc | Audience | Purpose |
|---|---|---|
| [`docs/user-quickstart-v0.1.md`](docs/user-quickstart-v0.1.md) | end users | hands-on walkthrough |
| [`docs/requirements.md`](docs/requirements.md) | contributors | 13 user pain points (acceptance baseline) |
| [`docs/tech-design.md`](docs/tech-design.md) | contributors | architecture, three-tier model, invariants |
| [`docs/interfaces.md`](docs/interfaces.md) | contributors | protocol reference (state.json / phase YAML / events / CLI / MCP) |
| [`docs/development-plan.md`](docs/development-plan.md) | contributors | milestone roadmap + dependency graph |
| [`docs/dev-coupling-audit.md`](docs/dev-coupling-audit.md) | contributors | dev-team coupling tracking (F1–F23) |
| [`docs/ccteam-as-domain-agnostic-orchestrator.md`](docs/ccteam-as-domain-agnostic-orchestrator.md) | contributors | strategic case for team abstraction |
| [`CLAUDE.md`](CLAUDE.md) | AI sessions | implementation rules + red lines (consumed by Claude Code in this repo) |

## Contributing

ccteam is itself developed using Claude Code under the worktree-per-task pattern. To contribute:

1. Fork + clone
2. `git worktree add -b feat/your-thing /tmp/ccteam-feature origin/main`
3. Read [`CLAUDE.md`](CLAUDE.md) — it's the AI implementation guide and applies to humans equally
4. Every PR maps to a pain point + a `tech-design.md` section + a `development-plan.md` task ID
5. `cargo test --workspace` must stay green; `cargo clippy --workspace --all-targets -- -D warnings` no new warnings
6. Open a PR; the maintainer reviews

Documentation is in Chinese (the project's working language); commit messages and PR descriptions are in English.

## License

See [`LICENSE`](LICENSE).

## Acknowledgments

- [Claude Code](https://code.claude.com/) — the runtime that makes this all possible
- [`claude-plugins-official`](https://github.com/anthropics/claude-plugins) — many phase sub-skills are referenced from this marketplace
- [`agent-of-empires`](https://github.com/loperanger7/gstack-auto) — short-term reference implementation for tmux + axum WebSocket bridge patterns
- [OpenAI Symphony](https://github.com/openai/symphony) — long-term architectural reference for orchestration model
