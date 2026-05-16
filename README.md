# ccteam

> **Autonomous multi-agent orchestrator built on Claude Code.** Declare a `workflow.yaml` — ccteam schedules `claude --bg` sessions per role, watches artifact directories for triggers, and asks for your input only when agents deadlock.

## Quick start

```bash
# Install (per machine)
ccteam doctor --install-all           # skill + MCP + meta-agent

# New project
cd ~/projects/my-app && ccteam init   # writes .ccteam/workflow.yaml + .claude/agents/explorer.md
ccteam start                          # daemon + web UI on http://localhost:7331
```

Edit `.ccteam/workflow.yaml` to declare your agent topology:

```yaml
name: my-app-loop
budget:
  max_cost_usd_per_24h: 5.00          # auto-disable if exceeded
agents:
  planner:
    trigger: manual                   # explicit spawn only
    executor: claude
  builder:
    trigger: watch:.ccteam/plans/     # spawn on new file in dir
    parallelism: 2                    # up to 2 concurrent sessions
  reviewer:
    trigger: gate                     # wait for trigger_gate MCP call
```

Each role has a system prompt at `.claude/agents/<role>.md` (Anthropic agent spec — name / description / tools / model / color frontmatter + body).

## Architecture

Three layers ([`docs/tech-design.md §2.1`](docs/tech-design.md)):

```
L0  user           — chat with meta-agent OR edit workflow.yaml directly
L1  meta-agent     — singleton claude session + 17 mcp__ccteam__* tools
L2  ccteam daemon  — Rust orchestrator, ArtifactWatcher, progress.jsonl SoT
L3  project agents — `claude --bg --agent <role>` per workflow.yaml role
                     (Codex executor uses tmux)
```

**No prompt injection** — agent behavior lives in `.claude/agents/<role>.md`. **No tmux long-sessions for Claude** — each spawn is a fresh `claude --bg` job, context resets cleanly. **All state in `progress.jsonl`** — 7 canonical workflow events, single source of truth.

5 canonical orchestration patterns ([`docs/orchestration-patterns.md`](docs/orchestration-patterns.md)) — Chaining / Routing / Parallelization / Orchestrator-Worker / Evaluator-Optimizer — all map to workflow.yaml + agent.md combinations.

## What it gives you

| Pain | ccteam answer |
|---|---|
| AI helper still asks me to project-manage | meta-agent dispatches; you supervise |
| Multi-agent topology is brittle | workflow.yaml + ArtifactWatcher; hot-reload on edit |
| Bug fixes loop forever | hard 3-strike escalation, then notify user |
| Run-aways blow my budget | per-project `max_cost_usd_per_24h` auto-disable |
| Daemon ungraceful shutdown loses state | F86 cancel token + 30s timeout fallback |
| Stale `~/.claude/jobs/<id>/` accumulates | F85 GC at daemon startup + `doctor --gc-claude-jobs` |
| Want to delete a project | `ccteam remove <slug>` with active-session red-line refusal |

## Web UI

`ccteam start` launches a SPA on `http://<host>:7331` (token auth on non-loopback). Four panels per workflow:

- **WorkflowView** — agent cards with running / queued / cost + SSE live updates
- **Artifact Queue** — pending files per watch dir + oldest age
- **Events Timeline** — `progress.jsonl` tail with event-type colors
- **Failure Inspector** — click errored agent → live tail of `~/.claude/jobs/<id>/output.log`
- **Cost Sparkline** — 24h / 7d trend

## Commands

13 user-facing (see `ccteam --help`):

```
init   start  stop  new  ls  status  show  remove
doctor web    team  session
```

8 internal (meta-agent / MCP / hooks):
```
ccteam internal hook | mcp-serve | spawn | send | peek | attach | progress | resume
```

Plus `ccteam-control` skill (loaded via `ccteam doctor --install-skill`) gives natural-language wrappers in any Claude Code session.

## Status

- **V0.4.6 shipped** (2026-05-16) — 11 findings F81-F91 (project lifecycle / workflow hot-reload / budget cap / graceful shutdown / cost SoT / Web panels / CLI slimming)
- **755 passing / 1 known port-bind flake** in `cargo test --workspace`
- **V1.0.0 goals** ([`docs/requirements.md §14-15`](docs/requirements.md)):
  1. Run autonomously ≥7 days on real 20-100k LOC projects (token maxxing)
  2. Extend to non-coding domains (research / content ops / investment analysis)

Pre-v1.0 = development stage — breaking changes welcome, no migration debt ([`CLAUDE.md §五.3`](CLAUDE.md)).

## Documentation

Full index: [`docs/README.md`](docs/README.md).

| Doc | Use case |
|---|---|
| [`docs/v0-4-6/user-manual.md`](docs/v0-4-6/user-manual.md) | end-user command reference |
| [`docs/orchestration-patterns.md`](docs/orchestration-patterns.md) | designing workflow.yaml topologies (5 patterns + split philosophy) |
| [`docs/tech-design.md`](docs/tech-design.md) | architecture SoT |
| [`docs/interfaces.md`](docs/interfaces.md) | protocol reference (YAML / JSON / CLI / hooks) |
| [`docs/requirements.md`](docs/requirements.md) | 15 pain points (13 user + 2 V1.0.0 ultimate) |
| [`CLAUDE.md`](CLAUDE.md) | implementation rules (consumed by Claude Code) |

## Contributing

ccteam is developed using Claude Code under the worktree-per-task pattern.

```bash
git worktree add -b feat/your-thing /tmp/ccteam-feature origin/main
# read CLAUDE.md + docs/README.md before changing code
cargo test --workspace --locked   # must stay green
cargo clippy --workspace --all-targets   # no new warnings
```

Every PR maps to: a pain point in `requirements.md` + a section in `tech-design.md` + a F-finding in `dev-coupling-audit.md` (if applicable).

Docs / agent prompts in Chinese (working language); commit messages + PR descriptions in English.

## License

See [`LICENSE`](LICENSE).

## Acknowledgments

- [Claude Code](https://code.claude.com/) — the runtime
- [Anthropic *Building Effective Agents*](https://www.anthropic.com/engineering/building-effective-agents) — the 5-pattern taxonomy
