# ccteam

> **Turn coding-agent CLIs into a resident AI dev team — driven from your phone.**

ccteam is a meta-tool on top of [Claude Code](https://code.claude.com/) (plus Codex, Grok, and OpenCode). A resident daemon on your own machine keeps agent sessions alive around the clock; you drive them from Telegram, Lark/Feishu, or a web console — hand off work on the train, answer a mid-task question from the couch, approve a risky command from your phone. ccteam owns everything *around* the agent — routing, session lifetime, delegation, cost — and never touches the work itself: **no injected prompts, no scraped terminals, no forked runtime.**

[Quick start](#quick-start) • User manual: [English](docs/usage.md) · [中文](docs/usage-cn.md)

## Why

Coding agents got good. Using one still chains you to a desk:

- **Sessions die with your terminal.** Close the laptop and the agent — and its context — is gone. ccteam keeps sessions resident on a server, each with a stable id (`s1`, `s2`, …) that survives daemon restarts; stopped sessions cold-resume from disk with their conversation intact.
- **You can't drive a long-running agent from your phone.** ccteam's IM gateway and mobile-friendly web chat shell put the full agent conversation — slash commands, file exchange, tool approvals — in your pocket.
- **One agent isn't a team.** In ccteam, any session can spawn, dispatch to, and collect results from other sessions — across vendors, and across machines via satellite hosts.
- **Orchestration tools love lock-in.** ccteam injects nothing: personas are plain Markdown in your project's `.claude/agents/`, loaded by the vendor CLI itself. A plugin marketplace ([ccteam-hub](https://github.com/firstintent/ccteam-hub)) supplies roles, skills, and workflows; remove ccteam and your repo still works.
- **Cost anxiety.** Per-session spend tracking and daily per-vendor budget caps put a hard ceiling on the bill.

## What you get

- **Always on.** One supervised daemon (systemd on Linux, launchd on macOS) survives logout, crashes, and reboots. Sessions spawn on demand, resume by id, and are released when idle — context is preserved, not held hostage by a terminal window.
- **Chat from anywhere.** Telegram and Lark/Feishu bots plus a web console speak to the same sessions. Vendor slash commands (`/compact`, `/model`, …) pass straight through; picker prompts become tappable buttons; images and files flow both ways; long answers arrive chunked with live progress.
- **A team, not a chat.** Sessions are independent, addressable agents — each with its own project, vendor, model, role, and context. Run several in parallel, switch with `@handle`, watch the delegation tree and live activity in the web console.
- **Agent-to-agent delegation.** Eight `mcp__ccteam__*` tools let any session `session_spawn` a helper, `session_dispatch` work to it, and `session_collect` the result — with guardrails (delegation depth, child counts, budget) enforced by the daemon, not by prompts.
- **Multi-machine.** Register satellite hosts and spawn sessions on them; transcripts, cost, and approvals still flow through the main daemon.
- **Human-in-the-loop when you want it.** Start a session in `hitl` mode and non-allowlisted tool calls wait for your approve/deny in chat. Deny blocks that one call — never the whole turn.
- **Plugin marketplace.** Browse curated roles, skills, and workflows from ccteam-hub in the web console or via `ccteam role search/add`; every install is pinned to an upstream revision and sha256-verified.
- **An API, not a silo.** Everything the console does is a token-authenticated HTTP API (`/api/v1`), self-documented at `/api/docs`.

Claude Code is the first-class harness; Codex, Grok, and OpenCode sessions are supported best-effort through the same session model. ccteam launches the real vendor binaries from your `PATH` — a new vendor feature works the day it ships.

## Quick start

Prerequisite: [Claude Code](https://code.claude.com/docs/install) installed and logged in on the machine that will host the daemon.

```bash
# Install a prebuilt binary (no toolchain needed):
curl -sSL https://raw.githubusercontent.com/firstintent/ccteam/main/install.sh | sh

# Or build from source (Rust + Node.js) and install as a supervised service:
git clone https://github.com/firstintent/ccteam && cd ccteam && make install

ccteam config     # one-time setup: register ccteam's MCP tools, connect an IM bot
ccteam start      # resident daemon (skip if `make install` already set up the service)
ccteam status     # prints the web console link: http://<lan-ip>:7331/?token=ccteam:<token>
```

Open the web console to create a project, install marketplace plugins, and connect Telegram / Lark in **Settings → IM** — or do it all from chat:

```text
/newproject demo ~/code/demo   # register a repo as a project
/cd demo                       # make it current; your first message spawns a session
/new claude                    # more sessions: /new [vendor] [role] [hitl]
@s2 run the test suite         # address any session directly
/status   /sessions   /stop s3 # health · fleet · cost · stop
```

New sessions default to *roleless*: the bare vendor CLI reading your project's own `CLAUDE.md` / `AGENTS.md`. Bind a persona by installing a role (`ccteam role add <role>` or the marketplace page) and passing it at `/new`, or swap it live with `/role <role>`.

Day-2 ops: `ccteam status` · `ccteam doctor` · `make daemon-logs` · `make daemon-restart`.

> **Security note:** the web console binds to `0.0.0.0:7331` with token auth and no TLS. Keep it on a trusted LAN; don't expose it to the public internet. For local-only use: `ccteam start --web-bind 127.0.0.1:7331`.

## Multi-machine

Any registered project can run its sessions on another machine. On the main daemon, mint a join token; on the satellite, join and serve:

```bash
# main daemon (admin)
ccteam host mint-token --daemon http://192.168.1.10:7331

# satellite machine
ccteam host join  --daemon http://192.168.1.10:7331 --token <join-token>
ccteam host serve --advertise-url http://192.168.1.20:7332   # only if no daemon runs here
```

A machine that already runs its own `ccteam start` daemon doubles as a satellite automatically: within seconds of joining, the daemon embeds the exec bridge and heartbeat in-process — `host serve` is only for machines that don't run a daemon.

The satellite is a thin process host: the main daemon streams the session protocol over a WebSocket bridge, while the satellite resolves its own vendor binaries and project paths. Transcripts, cost accounting, and HITL approvals stay on the main daemon. Remote execution currently supports Claude sessions; hosts and readiness are visible on the console's **Hosts** page.

## Architecture in one paragraph

One resident daemon is the whole control plane: IM gateway, web console, `/api/v1`, and an MCP endpoint — a **router, not an orchestrator** (no tick loop, no scheduler; orchestration intelligence lives in your personas and prompts, in user space). Sessions are first-class entities with durable ids: spawned on demand as real vendor CLI processes, resumed by id after restarts, released when idle. State lives in plain files inside your repos — chat transcripts in `<project>/.ccteam/chat/<sid>/`, business events in `progress.jsonl` — and delegation between sessions travels over the same message-routing path a human uses, exposed to agents as MCP tools.

## Principles

- **No prompt injection.** Roles are plain `.md` files the vendor CLI loads itself via its native agent mechanism; roleless sessions are the bare CLI reading your `CLAUDE.md`. ccteam never writes a system prompt into a session.
- **No terminal scraping.** Session state comes from transcripts and structured vendor events, never from parsing screen output.
- **Your repo stays yours.** ccteam never edits your product code, `.git/`, `.env`, or an existing `CLAUDE.md` / `AGENTS.md`. Its entire per-project footprint is `.ccteam/`, `.claude/agents/`, and its own hook section in `.claude/settings.local.json`.
- **Pure CLI, not a vendor plugin.** ccteam registers its MCP tools with Claude and Codex configs and otherwise stays out of the vendor's runtime. It doesn't bundle or fork any vendor binary.
- **Your data stays on your machines.** All state is local files under `~/.ccteam` and your project directories — no cloud service in the loop.
- **Budgets guard, never kill.** Daily per-vendor cost caps are the only automatic brake — on overrun a vendor is auto-disabled until the window resets. Outside that, ccteam never kills a long-running session on its own.

## License

MIT — see [LICENSE](LICENSE). Built on **Claude Code**, with support for **Codex**, **Grok**, and **OpenCode**.
