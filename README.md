# ccteam

> **Your AI dev team, cloud-resident on your own machine — driven from IM and a web console.** Built on top of Claude Code (and OpenAI Codex). No YAML to write, no flags to memorize.

ccteam is a meta-tool that runs on top of [Claude Code](https://code.claude.com/) (and OpenAI Codex). It puts a single resident gateway daemon on your own computer, sitting in front of the real `claude` / `codex` agents, so you can drive them from instant messaging (Telegram, Slack, …) and a local web console — as if your phone were a terminal into your machine.

The AI runs on **your computer**: it reads your files, runs your commands, touches your code. IM is just the entry point — close the laptop lid and the work keeps running. Sessions are durable and survive restarts.

## What it is

You talk to ccteam from three places, all backed by one daemon:

- **From IM** — DM a bot to work on a project, or `@ccteam <natural language>` in a group for control (`pause`, `cost`, `list`, `stop everything`, …). The full slash-command surface of the agent works straight from chat (see below), and when an agent asks you a question, you answer it right there.
- **Inside a Claude or Codex session** — the `mcp__ccteam__*` MCP tools are the programmatic surface: register a bot, send a file to your chat, take a screenshot, or ask a panel of agents to vote. The default `cto` manager can also spawn a work-role session, dispatch a task to it, and collect the result — so it delegates instead of doing everything itself.
- **From a web console** — one chat-style local console (create a project, open / switch sessions, watch live transcripts and a per-session terminal, browse and one-click-install plugins from the marketplace, see a live cost pill and status view, and configure IM credentials) plus a standard HTTP resource API (under `/api/v1`) so the same projects, roles, and sessions are reachable from a browser, your own app, or a third-party integration.

## Key concepts

**chat ⇄ project ⇄ session ⇄ role.** This is the whole mental model:

- A **chat** is one IM conversation or browser surface — your terminal. It can span multiple projects and hold multiple live sessions at once. Another chat is fully isolated.
- A **project** is a local directory you ran `ccteam init` on.
- A **session** is an independent, resident agent handle with its own context (`/compact` and `/clear` are per-session), exactly like a native Claude Code session. A project can hold many at once and they never cross-talk — even two sessions of the *same* role each keep their own conversation. Each gets a durable handle `s<N>` that survives a daemon restart. You spin up sessions with `/new`, list a chat's sessions (handle, project, vendor, role, model, context) with `/sessions`, switch between them with `@handle` / `/use s<N>`, and switch projects with `/cd`.
- A **role** is who a session *is*. A session usually launches as a specific role from the project's role library — just `.claude/agents/<role>.md`, plain Markdown persona files — or as **no role at all** (a bare `claude` that takes its brain from the project's `CLAUDE.md`). `ccteam init` seeds one default role, **`cto`**: a chat-first manager that understands ccteam, recommends the right work-role for the job, and hands off. You switch a session's role at any time with `/role <role>` (the session keeps its handle and is restarted in place).

**A role library, your way — plus a plugin marketplace.** Because a role is just a Markdown file in `.claude/agents/`, you build your team by dropping in `.md` files — write your own, or install ready-made roles, skills, and workflows from the **ccteam plugin marketplace**: a curated catalog (which vendors in open-source libraries like [agency-agents](https://github.com/wshobson/agents), MIT) that you browse and one-click-install into a project from the web console, or from the CLI with `ccteam role search` / `ccteam role add`. Every install is content-integrity checked. The default `cto` is a manager that suggests which role fits the task; you make the call with `/role`.

**One gateway daemon, no tick loop.** `ccteam start` runs a single resident process that is purely an IM/web⇄session routing gateway — there is no orchestrator polling loop. In one runtime it co-hosts the IM gateway, the web server and its resource API, and a local MCP Unix socket (so the Claude/Codex plugins can call ccteam tools). All tasks share one clean-shutdown signal.

**No prompt injection — the vendor reads its own role.** A session is launched as `claude --agent <role>`, so Claude itself loads and obeys `.claude/agents/<role>.md`; ccteam never injects a system prompt into the pane. Project knowledge stays vendor-native too — Claude reads your `CLAUDE.md`, Codex reads your `AGENTS.md`, and ccteam neither generates nor rewrites those files.

**Harness × provider, best-fit drive.** ccteam abstracts each agentic CLI as a *harness*; the model behind it is the *provider* sub-facet. Today the primary harness is **claude-code**, driven through a long-lived **tmux TUI** session — durable, full TUI, driven with `send-keys` plus transcript tailing and the official Claude Code hooks. ccteam never scrapes terminal output. **Codex** is supported on a best-effort basis, and more harnesses (gemini-cli, grok-cli, …) are designed to plug in as adapters. Whatever is actually installed on your `PATH` is reported live by the API's `GET /capabilities`.

**Full slash-command coverage, from chat.** A slash command you type in IM (or the web console) does the right thing for whichever agent owns the session — no command silently degrades into literal text. Claude's open command set (skills, `/compact`, `/clear`, custom commands, …) passes straight through to the TUI. Popup / picker commands — pick a model, choose a review target, and the like — are answered with **inline buttons** in IM (or **chips** in web chat) instead of getting stuck in a hidden TUI modal. And when an agent itself raises a question mid-task (an `AskUserQuestion`), it surfaces as the same kind of inline choice, so you can answer from your phone and the agent keeps going.

**Approve dangerous actions, per session (optional).** By default a session runs hands-off. Spin one up with human-in-the-loop instead (`/new claude <role> hitl`) and any non-allowlisted tool call pauses for your **approve / deny** — surfaced as inline buttons in chat or the web console, via Claude's native permission hook (ccteam never injects a prompt to do this). Deny blocks just that one tool, never the whole turn. Auto-allowed tools never prompt.

**Independent, resume-by-id sessions.** Each session is its own durable handle (`s<N>`): spawned on demand (first message creates one), resumed by id, and released when idle — state lives on disk, never as a shadow source of truth. Many run side by side in one project without bleeding into each other, even when they share a role. They survive a daemon restart: the next `ccteam start` re-attaches your bots by handle and replays any unsent IM replies, so upgrading or restarting ccteam doesn't lose context. If a Claude pane was killed, it is recreated with `claude --resume` for a lossless reload of the model's full context; if resume isn't possible, ccteam falls back to a fresh session and emits a visible reset event rather than silently forgetting.

**A standard resource API.** The web console and any other client speak the same versioned HTTP API (`/api/v1`, web-token auth):

- **projects** — `GET`/`POST /projects`, `GET`/`DELETE /projects/{slug}`
- **roles** — `GET /projects/{slug}/roles`, `GET`/`PUT /projects/{slug}/roles/{role}`
- **sessions** — `GET`/`POST /projects/{slug}/sessions`, `GET /sessions/{sid}`, `POST /sessions/{sid}/turn`, `GET /sessions/{sid}/events` (SSE), `POST /sessions/{sid}/stop`
- **capabilities** — `GET /capabilities` (which harnesses, and which are available on `PATH`)

It is self-documenting: `GET /api/docs` serves an interactive (Scalar) UI and `GET /api/v1/openapi.json` the OpenAPI 3.1 spec, both generated from the same route registrations. This is the integration surface: an app or third party can register a project, list its roles, open a session, stream its events, and send turns — the same primitives IM and the web console use.

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

# 3. One-time setup (install MCP, set your IM token, preferences):
ccteam config
#    Interactive menu, or non-interactively:  ccteam config <key> <value>

# 4. Start the gateway daemon (IM gateway + web console + resource API + MCP socket):
ccteam start
#    Stop it cleanly with Ctrl+C, or:  ccteam stop
```

```text
# 5. Register the plugin so the slash commands and MCP tools light up.
#    In a Claude session:
/plugin marketplace add https://github.com/firstintent/ccteam
/plugin install ccteam

#    Or in a Codex session (the ccteam binary must be on $PATH from step 1):
codex plugin marketplace add firstintent/ccteam
```

```text
# 6. Drive it from IM (Telegram):
/pair <code>            # link your chat to the daemon (code from `ccteam config`)
/cd myproject           # switch to a project (or `/cd` to list)
                        # → a `cto` session spins up; just start chatting
/role backend-dev       # become a work-role to do the job
/new   /use   @handle   # open / switch / address multiple sessions
@ccteam status          # group control: status / pause / cost / stop
```

**Managing it from the CLI** (the daemon stays the source of truth):

```bash
ccteam project ls               # registered projects
ccteam project new <slug>       # register a directory
ccteam project stop <slug>      # stop a project's sessions (resumable)
ccteam project rm <slug>        # deregister + stop  (--purge to remove ccteam's files)
ccteam session ls               # live sessions
ccteam status                   # daemon + sessions at a glance
ccteam doctor                   # diagnostics / self-check (--verify-mcp)
```

**Supported platforms.** Linux x86_64 / aarch64 and macOS arm64 / x86_64 (prebuilt binaries). Linux binaries are musl-static, so they run on any glibc version (NAS and older distros included). Windows is supported via WSL2 using the linux-x64 binary — tmux, inotify, and POSIX signals are foundational to ccteam, so native Windows isn't supported. On macOS, if Gatekeeper blocks the binary on first run: `xattr -d com.apple.quarantine ~/.local/bin/ccteam`.

## Features

- **A meta AI team you drive from your phone** — assign work, get results, and intervene from IM, anywhere.
- **Independent sessions, each with a role** — every session is its own durable handle running a role you can swap on the fly with `/role` (or no role — a bare `claude` driven by the project's `CLAUDE.md`); a default `cto` manager helps pick the right one (and can spawn / dispatch / collect work-role sessions to delegate). Roles are plain `.claude/agents/*.md` files — write your own or import from open libraries with `ccteam role search` / `add`.
- **Live, legible IM turns** — a turn shows folded step-by-step progress (`📖 read ×5 · 🔧 bash ×3`) in one editable status message while it works, then the answer arrives as its own message. Long replies are split into ordered chunks (code fences kept intact) instead of being truncated.
- **Pictures both ways** — send a screenshot or file to the bot and the agent reads it; the agent can send images/files back to your chat.
- **Multi-project, multi-session** — one chat fans out across many repos and many concurrent agent sessions, each fully independent with its own context (two sessions of the same role don't merge). `/sessions` lists them with each session's role, vendor, model, and live context usage (e.g. `188k / 1M (19%)`), and a command menu is registered with your IM client so the gateway commands are discoverable.
- **Full slash coverage from chat** — every agent slash command works from IM: Claude's open set passes through, model/review-style pickers become inline buttons, and an agent's own questions are answerable inline.
- **No prompt injection** — the vendor self-loads its role and project memory; ccteam drives, it doesn't ventriloquize.
- **Optional human-in-the-loop** — run a session in `hitl` mode and every risky tool call waits for your inline approve/deny; the rest run hands-off.
- **Harness × provider** — a pluggable adapter model: claude-code today, Codex best-effort, more agentic CLIs by design. `GET /capabilities` reports what's actually available on your machine.
- **A standard resource API** — versioned, web-token-authed `/api/v1` for projects, roles, and sessions (with SSE event streams), self-documenting via an interactive `/api/docs` (Scalar) and an OpenAPI spec, so apps and third parties integrate against the same primitives as IM and the web console.
- **Durable by design** — sessions survive daemon restarts and machine reboots; nothing silently forgets.
- **File-system source of truth** — all state is reconstructable from disk; ccteam reads hooks, transcripts, and RPC events, never scraped terminal text.
- **Cost-aware** — per-vendor 24h budgets with a hard ceiling; long-running sessions are never killed unless a budget cap is hit (or you explicitly stop them).
- **Cross-project memory** — lessons accumulate through the official Claude Code / Codex memory channels, so new projects don't start from zero.
- **Plugin marketplace** — browse and one-click-install roles, skills, and workflows from a curated catalog (which vendors in open-source libraries like agency-agents) into any project, with a body preview before you install and a content-integrity check on every install. Reachable from the web console or the CLI (`ccteam role search` / `add`).
- **Web console** — one chat-style local console to create projects, open and switch sessions (with a role picker that includes a no-role option), watch transcripts and a byte-faithful per-session terminal live, browse and install marketplace plugins, see a live cost pill and a status view, and configure IM credentials (Telegram + Lark/Feishu). Each running session also has a dedicated view (`/chat/s/:sid`). It binds to `0.0.0.0:7331` with token auth by default and has no TLS, so keep it on a trusted LAN — don't expose it to the public internet.
- **MCP surface** — the `ccteam` MCP server exposes chat, advise, admin, session (cto dispatch), and screenshot tool groups for programmatic control.

## Docs

| What you want | Read this |
|---|---|
| Command guide — every CLI command, slash command, and IM control | [docs/usage.md](docs/usage.md) |
| Architecture — components, data protocols, design rationale | [docs/tech-design.md](docs/tech-design.md) |
| Requirements — the problems ccteam solves, and for whom | [docs/requirements.md](docs/requirements.md) |

## License & acknowledgements

MIT — see [LICENSE](LICENSE). Built on [Claude Code](https://code.claude.com/) (the agent runtime) and OpenAI Codex, with [openhuman/channels](https://github.com/openhuman/channels) providing IM connectivity across many platforms. The IM-bot pattern (tmux + send-keys + transcript polling) is inspired by `ccgram` and `oh-my-claudecode`.
