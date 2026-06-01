# ccteam

> **Your AI dev team, cloud-resident on your own machine — driven from IM and a web console.** Built on top of Claude Code and Codex. No YAML to write, no flags to memorize.

ccteam is a meta-tool that runs on top of [Claude Code](https://code.claude.com/) (and OpenAI Codex). It puts a single resident gateway daemon on your own computer, sitting in front of the real `claude` / `codex` agents, so you can drive them from instant messaging (Telegram, Slack, …) and a local web console — as if your phone were a terminal into your machine.

The AI runs on **your computer**: it reads your files, runs your commands, touches your code. IM is just the entry point — close the laptop lid and the work keeps running. Sessions are durable and survive restarts.

## What it is

You talk to ccteam from three places, all backed by one daemon:

- **From IM** — DM a bot to work on a project, or `@ccteam <natural language>` in a group for control (`pause`, `cost`, `list`, `stop everything`, …).
- **Inside a Claude or Codex session** — `/ccteam <natural language>` is the universal entry; per-task slash commands let you skip the router when you know the path. The `mcp__ccteam__*` MCP tools are the programmatic surface.
- **From a web console** — a local dashboard to watch live sessions, transcripts, and 24h spend.

## Key concepts

**chat ⇄ project ⇄ session.** This is the whole mental model:

- A **chat** is one IM conversation — your terminal. It can span multiple projects and hold multiple live sessions at once. Another chat is fully isolated.
- A **project** is a local directory you ran `ccteam init` on.
- A **session** is `project × vendor × role` — a resident agent handle with its own context (`/compact` and `/clear` are per-session). You spin up sessions with `/new`, switch between them with `@bot` / `/use`, and switch projects with `/cd`.

**One gateway daemon, no tick loop.** `ccteam start` runs a single resident process that is purely an IM⇄session routing gateway — there is no orchestrator polling loop. In one runtime it co-hosts the IM gateway, a local MCP Unix socket (so the Claude/Codex plugins can call ccteam tools), and the web server. All three share one clean-shutdown signal.

**Two vendors, best-fit drive.** Each agent runtime is driven through its most natural channel, then normalized to a single neutral `CanonicalEvent` stream:

- **Claude** runs in a long-lived **tmux TUI** session — durable, full TUI, driven with `send-keys` plus transcript tailing and the official Claude Code hooks. ccteam never scrapes terminal output.
- **Codex** runs via the **app-server JSON-RPC** control plane — native and documented; `/compact`, `/review`, etc. map to Codex-native RPCs.

Both vendors can run concurrently inside the same chat.

**Resume-by-id, durable sessions.** Sessions are spawned on demand (first message creates one), resumed by id, and released when idle — state lives on disk, never as a shadow source of truth. They survive a daemon restart: the next `ccteam start` re-attaches your bots and replays any unsent IM replies, so upgrading or restarting ccteam doesn't lose context. If a Claude pane was killed, it is recreated with `claude --resume` for a lossless reload of the model's full context; if resume isn't possible, ccteam falls back to a fresh session and emits a visible reset event rather than silently forgetting.

## Quickstart

```bash
# 0. Install Claude Code first: https://code.claude.com/docs/install

# 1. Install the ccteam binary (one line, no Rust toolchain needed):
curl -sSL https://raw.githubusercontent.com/firstintent/ccteam/main/install.sh | sh
#    Installs to ~/.local/bin/ccteam. If that directory isn't on your PATH,
#    the script prints an export hint — follow it, then restart your shell.
#    Install system-wide:  CCTEAM_INSTALL_DIR=/usr/local/bin curl ... | sh
#    Build from source:    cargo install --git https://github.com/firstintent/ccteam ccteam-cli
```

```bash
# 2. Turn any directory into a ccteam project:
ccteam init

# 3. Start the gateway daemon (IM gateway + MCP socket + web console):
ccteam start
#    Stop it cleanly with Ctrl+C, or:  ccteam stop
```

```text
# 4. Register the plugin so the slash commands and MCP tools light up.
#    In a Claude session:
/plugin marketplace add https://github.com/firstintent/ccteam
/plugin install ccteam

#    Or in a Codex session (the ccteam binary must be on $PATH from step 1):
codex plugin marketplace add firstintent/ccteam
```

```bash
# 5. Drive it in natural language — from a Claude/Codex session or from IM:
/ccteam "scan this repo and tell me what it does"
/ccteam "fix the TypeScript errors in src/"
/ccteam "build a Telegram bot that summarizes my GitHub PRs at 7am"
```

**Supported platforms.** Linux x86_64 / aarch64 and macOS arm64 / x86_64 (prebuilt binaries). Linux binaries are musl-static, so they run on any glibc version (NAS and older distros included). Windows is supported via WSL2 using the linux-x64 binary — tmux, inotify, and POSIX signals are foundational to ccteam, so native Windows isn't supported. On macOS, if Gatekeeper blocks the binary on first run: `xattr -d com.apple.quarantine ~/.local/bin/ccteam`.

## Features

- **A meta AI team you drive from your phone** — assign work, get results, and intervene from IM, anywhere.
- **Multi-project, multi-session** — one chat fans out across many repos and many concurrent agent sessions, each with its own context.
- **Two agent vendors** — Claude (tmux TUI) and Codex (app-server) side by side; ask for a cross-vendor second opinion on hard calls.
- **Durable by design** — sessions survive daemon restarts and machine reboots; nothing silently forgets.
- **File-system source of truth** — all state is reconstructable from disk; ccteam reads hooks, transcripts, and RPC events, never scraped terminal text.
- **Cost-aware** — per-vendor 24h budgets with a hard ceiling; long-running sessions are never killed unless a budget cap is hit.
- **Cross-project memory** — lessons accumulate through the official Claude Code / Codex memory channels, so new projects don't start from zero.
- **Web console** — a local dashboard to watch sessions, transcripts, and spend in real time.
- **MCP surface** — the `ccteam` MCP server exposes workflow, chat, advise, admin, and screenshot tool groups for programmatic control.

## Docs

| What you want | Read this |
|---|---|
| Command guide — every CLI command, slash command, and IM control | [docs/usage.md](docs/usage.md) |
| Architecture — components, data protocols, design rationale | [docs/tech-design.md](docs/tech-design.md) |
| Requirements — the problems ccteam solves, and for whom | [docs/requirements.md](docs/requirements.md) |

## License & acknowledgements

MIT — see [LICENSE](LICENSE). Built on [Claude Code](https://code.claude.com/) (the agent runtime) and OpenAI Codex, with [openhuman/channels](https://github.com/openhuman/channels) providing IM connectivity across many platforms. The IM-bot pattern (tmux + send-keys + transcript polling) is inspired by `ccgram` and `oh-my-claudecode`.
