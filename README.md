# ccteam

> **A multi-agent orchestrator on top of Claude Code** — describe what you want, ccteam picks the command. No YAML. No CLI flags to memorize.

## What do you want to do?

Pick the row that matches your goal. The right column is the command — copy-paste into any Claude session.

```
You want to do                                  → Run this
──────────────────────────────────────────────────────────────────────
Get a feel for a new codebase (60 s, zero deps)  /ccteam-scan --quick
Audit a codebase navigability / large monorepo   /ccteam-scan
Build / fix / refactor (watching it work)        /ccteam-team "<task>"
Cross-vendor second opinion on a hard call       /ccteam-advise "<question>"
A private IM assistant (24/7, always on)         /ccteam-creator "build me a <X> assistant"
A multi-bot IM round-table                       /ccteam-creator "a few bots in a group"
Run a long task overnight (hands-off)            /ccteam-creator "<task>, run while I sleep"
List / pause / resume / check spending           /ccteam-control list | pause | cost
Wire up an IM token (Telegram / Slack / Discord) /ccteam-im-setup
Verify your install / MCP surface / Codex critic ccteam doctor [--verify-mcp | --check-codex-auto-critic | --check-cost-orphan]
Not sure? Just describe it in natural language   /ccteam "<what you want>"
```

> Each command is a Claude Code slash command. Type it in a `claude` session — `/ccteam <NL>` is the universal entry; the others let you skip the router when you already know the path.

## Get started

### Install (Claude Code or Codex)

Inside any Claude Code session:

```
/plugin marketplace add https://github.com/firstintent/ccteam
/plugin install ccteam
```

Inside any Codex session:

```
codex plugin marketplace add firstintent/ccteam
```

The plugin auto-downloads the Rust engine into its own sandbox on the first MCP invocation — no system-wide binary required, no Rust toolchain, no separate install step. A Node.js bridge (`index.js`) ships in the repo, detects the host (Claude vs Codex), pulls the matching prebuilt tarball from GitHub Releases, and execs `ccteam mcp-serve` under the covers. It also symlinks the binary to `~/.local/bin/ccteam` so the CLI is available from any terminal.

### Use it

```
# Universal entry — describe what you want in any language:
/ccteam "scan this repo and tell me what it does"
/ccteam "fix the TypeScript errors in src/"
/ccteam "build a Telegram bot that summarizes my GitHub PRs at 7am"

# (Optional) Bootstrap a per-project workflow scaffold from the CLI:
ccteam init <project>
```

Supported platforms for the prebuilt binary: Linux x86_64, macOS arm64 (Apple Silicon), macOS x86_64 (Intel). Windows users: install via WSL2 and use the linux-x64 binary — native Windows isn't supported because tmux + inotify + POSIX signals are foundational to ccteam. On macOS, if Gatekeeper blocks the binary on first run: `xattr -d com.apple.quarantine ~/.local/bin/ccteam`.

The plugin install registers the seven `/ccteam*` slash commands and `mcp__ccteam__*` MCP tools — they light up immediately. The 5-minute walkthrough for "private IM assistant" (the flagship use case) lives in [docs/quickstart.md](docs/quickstart.md).

### Advanced: system-wide CLI without the plugin

If you want `ccteam` on `$PATH` for daemon use (`ccteam start`) without going through a Claude or Codex session — for example on a headless server — install the binary directly:

```bash
curl -sSL https://raw.githubusercontent.com/firstintent/ccteam/main/install.sh | sh
```

Pin a tag: `CCTEAM_VERSION=<tag> curl ... | sh`. System-wide: `CCTEAM_INSTALL_DIR=/usr/local/bin curl ... | sh`. Or build from source: `cargo install --git https://github.com/firstintent/ccteam ccteam-cli` (Rust 1.85+).

## Three ways to talk to ccteam

- **Inside a Claude session** — `/ccteam <NL>` is the universal entry; the per-task slash commands above are shortcuts. MCP tools (`mcp__ccteam__chat_*`, `mcp__ccteam__advise_*`, `mcp__ccteam__admin_*`) are the programmatic path for anything a sub-skill does.
- **From IM** — DM your bot directly, or `@ccteam <NL admin>` inside a group for control (`pause`, `cost`, `list`, `stop everything`, …).
- **Web dashboard** (read-only) — `http://localhost:7331` to watch workflows, transcripts, and 24h spend.

The AI runs on **your computer** — it can read your files, run your commands, touch your code. Your phone / IM is just the entry point; close the laptop lid and the workflow keeps running.

## Run as a daemon

`ccteam start` runs the orchestrator in the foreground (or as a detached process). To stop it cleanly:

```bash
kill -TERM $(cat ~/.ccteam/ccteam.pid)   # or just Ctrl+C in the foreground terminal
```

It drains within 5 seconds, releases the web port, and unlinks its pidfile automatically. Long-running tmux chat sessions survive a daemon restart and are re-attached on the next `ccteam start` — upgrading ccteam does not lose your bot's context. If a chat pane was killed (OOM, manual `tmux kill-session`), the next `ccteam start` re-spawns it with `claude --resume <name>` so the model reloads its full API-level context (tool-use history, cache, reasoning) losslessly via the official Anthropic CLI path. If resume isn't possible (no on-disk session, schema drift), ccteam falls back to a fresh session and emits a visible `chat_session_reset` event — you'll see the bot acknowledge the reset rather than silently forget.

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
