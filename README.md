# ccteam

> **A multi-agent orchestrator on top of Claude Code** — describe what you want, ccteam picks the command. No YAML. No CLI flags to memorize.

![demo](docs/versions/v0-6-0/demos/30s-tg-bot-team.gif)

## What do you want to do?

Pick the row that matches your goal. The right column is the command — copy-paste into any Claude session.

```
You want to do                                 → Run this
──────────────────────────────────────────────────────────────────────
Get a feel for a new codebase / repo audit      /ccteam-scan
Build / fix / refactor (watching it work)       /ccteam-team "<task>"
Review a PR / get a second opinion              /ccteam-advise "<PR or path>"
A private IM assistant (24/7, always on)        /ccteam-creator "build me a <X> assistant"
A multi-bot IM round-table                      /ccteam-creator "a few bots in a group"
Run a long task overnight (hands-off)           /ccteam-creator "<task>, run while I sleep"
List / pause / resume / check spending          /ccteam-control list | pause | cost
Wire up an IM token (Telegram / Slack / Discord)  /ccteam-im-setup
Not sure? Just describe it in natural language  /ccteam "<what you want>"
```

> Each command is a Claude Code slash command. Type it in a `claude` session — `/ccteam <NL>` is the universal entry; the others let you skip the router when you already know the path.

## Get started

```bash
# 0. Install Claude Code first: https://code.claude.com/docs/install
claude
/plugin install ccteam

# 1. Try the universal entry — describe what you want in any language:
/ccteam "scan this repo and tell me what it does"
/ccteam "fix the TypeScript errors in src/"
/ccteam "build a Telegram bot that summarizes my GitHub PRs at 7am"
```

The 5-minute walkthrough for "private IM assistant" (the flagship use case) lives in [docs/quickstart.md](docs/quickstart.md).

## Three ways to talk to ccteam

- **Inside a Claude session** — `/ccteam <NL>` is the universal entry; the per-task slash commands above are shortcuts.
- **From IM** — DM your bot directly, or `@ccteam <NL admin>` inside a group for control (`pause`, `cost`, `list`, `stop everything`, …).
- **Web dashboard** (read-only) — `http://localhost:7331` to watch workflows, transcripts, and 24h spend.

The AI runs on **your computer** — it can read your files, run your commands, touch your code. Your phone / IM is just the entry point; close the laptop lid and the workflow keeps running.

## Docs

| What you want | Read this |
|---|---|
| Decision tree — task to command (with examples) | [docs/task-to-command.md](docs/task-to-command.md) |
| 5-minute walkthrough for the flagship IM-bot use case | [docs/quickstart.md](docs/quickstart.md) |
| Full user manual (every scenario, every flag) | [docs/user-manual.md](docs/user-manual.md) |
| Copy-paste a ready-made use case | [docs/recipes.md](docs/recipes.md) |
| Something broke | [docs/troubleshooting.md](docs/troubleshooting.md) |
| Advanced customization / Codex integration | [docs/advanced/](docs/advanced/) |

## License & acknowledgements

See [LICENSE](LICENSE). Built on [Claude Code](https://code.claude.com/) (the runtime) and [openhuman/channels](https://github.com/openhuman/channels) (14+ IM platforms in Rust); orchestration taxonomy from Anthropic's *Building Effective Agents*; IM-bot pattern (tmux + send-keys + transcript polling) inspired by `ccgram` and `oh-my-claudecode`.
