# ccteam

> **Remote Claude Code & Codex, done right — autonomous, online 24/7, answers the moment you ping.** Your AI dev team, resident on your own machine, driven from IM and a web console.

ccteam is a **self-hosted control plane for coding agents.** One resident daemon sits in front of the stock [Claude Code](https://code.claude.com/) and OpenAI Codex agents and owns everything *around* the work — routing, session identity, lifecycle, and budget — while never touching the work itself: no injected prompts, no scraped terminals, no forked runtime. You drive the real Claude Code / Codex, every native capability intact.

## Architecture

```text
   IM (Telegram · Slack · Lark)        web console        MCP tools (in a session)
              │                            │                        │
              └─────────────────────────────┼────────────────────────┘
                                            ▼
                  ┌───────────────────────────────────────┐
                  │              ccteam daemon             │   one resident process —
                  │   IM gateway · web · /api/v1 · MCP     │   routes, owns session
                  │         (resident, no tick loop)       │   identity · lifecycle ·
                  └───────────────────┬───────────────────┘   budget; never the work
                     spawn on demand · resume by id · release when idle
           ┌─────────────────┬─────────┴────────┬─────────────────┐
           ▼                 ▼                  ▼
      ┌─────────┐       ┌─────────┐        ┌─────────┐
      │ s1 cto  │       │ s2 dev  │        │ s3  …   │    independent sessions, each its
      │ claude  │       │ claude  │        │ codex   │    own role + context (handle s<N>)
      └────┬────┘       └────┬────┘        └────┬────┘
           └───────  the real Claude Code / Codex  ───────┘   --agent <role>, no injection
                              │
           your machine · your files · state on disk (resumes after any restart)
```

## Why

- **Remote-first** — your phone is a terminal into your machine; answer an agent's mid-task questions right in chat.
- **Always-on** — runs on *your* computer and keeps working after you close the lid.
- **A team, not a chat** — many independent sessions run side by side, not one serialized conversation.
- **Vendor-native, zero lock-in** — a new Claude Code / Codex feature works the day it ships; the vendor is a per-session attribute, not a platform bet.

## Three ways to drive it

- **From IM** — DM a bot to work a project, or `@ccteam <natural language>` in a group for control (`pause`, `cost`, `list`, `stop everything`). The agent's full slash-command surface works straight from chat.
- **From a web console** — one chat-style local console (in 中文 or English): create projects, open and switch sessions, watch live transcripts and a byte-faithful per-session terminal, one-click-install plugins from the marketplace, check each host's agent install / MCP-registration status, and see a live cost pill plus per-session spend — plus a standard, self-documenting HTTP API at `/api/v1` (`GET /api/docs`).
- **Inside a session** — the `mcp__ccteam__*` MCP tools are the programmatic surface (register a bot, send a file to chat, screenshot, run a vote). The default `cto` manager can spawn a work-role session, dispatch a task to it, and collect the result.

## The model: chat ⇄ project ⇄ session ⇄ role

- A **chat** is one IM conversation or browser surface — your terminal. It spans projects and holds many live sessions at once; another chat — another person, even sharing the same machine and daemon — sees only its own sessions (soft per-chat isolation under one OS account). Several people can share one daemon: the owner mints a **per-user web login link**, and each user can run their **own** IM bot (its own Telegram / Lark token, set self-serve in the web console) that drives only their sessions.
- A **project** is a local directory you ran `ccteam init` on.
- A **session** is an independent, resident agent handle (`s<N>`) with its own context (`/compact` and `/clear` are per-session) — exactly like a native Claude Code session. `/new` spawns, `/sessions` lists (handle, project, vendor, role, model, live context), `@handle` / `/use` switches, `/cd` changes project. Two sessions of the same role never cross-talk.
- A **role** is who a session *is* — a plain Markdown persona at `.claude/agents/<role>.md`, or no role at all (a bare `claude` driven by your project's `CLAUDE.md`). `ccteam init` seeds a `cto` manager that recommends the right work-role; swap any session's role live with `/role`.

Build your team by dropping `.md` files into `.claude/agents/` — or one-click-install roles, skills, and workflows from the **plugin marketplace** (the web console, or `ccteam role search` / `add`): a curated catalog — official ccteam plugins first, then tracked open-source libraries like [agency-agents](https://github.com/wshobson/agents) and [mattpocock/skills](https://github.com/mattpocock/skills) — that pins each upstream at a revision (pointers, not copies) and content-integrity-checks every install.

## How it works

- **One gateway daemon, no tick loop** — `ccteam start` runs a single resident process: IM gateway + web server + resource API + local MCP socket. It routes; it does not orchestrate.
- **No prompt injection** — a session launches as `claude --agent <role>`, so the vendor loads and obeys its own role file; ccteam never injects a system prompt. Project memory stays vendor-native (`CLAUDE.md` / `AGENTS.md`), neither generated nor rewritten.
- **Harness × provider** — each agentic CLI is a *harness* (claude-code first-class, Codex best-effort, more by design); the model behind it is the *provider*. Claude is driven over the default **stream-json** channel (a long-lived process over an NDJSON pipe — lightweight, no terminal) or a **terminal** channel (a tmux TUI) when you want the byte-faithful mirror / attach / screenshot. ccteam never scrapes terminal output. `GET /capabilities` reports what's actually installed on your `PATH`.
- **Full slash coverage from chat** — Claude's open commands pass through; picker commands (model, review target) become inline buttons in IM (chips in web); an agent's own `AskUserQuestion` surfaces the same way, answerable from your phone.
- **Optional human-in-the-loop** — `/new claude <role> hitl` makes every non-allowlisted tool call pause for your inline approve / deny via Claude's native permission hook. Deny blocks just that tool, never the turn; the rest run hands-off.
- **Cost-aware & durable** — per-vendor 24h budget caps with a hard ceiling (a long session is never killed unless a cap is hit, or you stop it); all state is reconstructable from disk (hooks, transcripts, RPC events — never scraped text). A killed Claude pane reloads losslessly with `claude --resume`, or falls back to a fresh session with a visible reset event rather than silently forgetting.

## Quickstart

```bash
# 0. Install Claude Code first: https://code.claude.com/docs/install

# 1. Install ccteam (prebuilt binary, no Rust toolchain → ~/.local/bin/ccteam):
curl -sSL https://raw.githubusercontent.com/firstintent/ccteam/main/install.sh | sh

# 2. Initialize a project, then one-time setup (MCP for Claude + Codex, IM token, prefs):
cd ~/code/myproject && ccteam init
ccteam config

# 3. Start the daemon — IM gateway + web console + resource API + MCP, one process:
ccteam start                              # foreground; detached: nohup ccteam start >~/ccteam.log 2>&1 &
```

Then drive it from **either** surface — the web console or IM:

```text
# Web console — open in your browser:
http://localhost:7331
#   Token auth: paste the token shown when the daemon starts
#   (also stored at ~/.ccteam/secrets/web-token).
```

```text
# IM (Telegram):
/pair <code>            # link your chat (code from `ccteam config`)
/cd myproject           # switch project → a `cto` session spins up; start chatting
/role backend-dev       # switch to a work-role
/new   /use   @handle   # open / switch / address sessions
@ccteam status          # group control: status / cost / stop
```

Manage from the CLI (the daemon stays the source of truth): `ccteam project ls|new|stop|rm`, `ccteam session ls`, `ccteam status`, `ccteam doctor`. Build from source instead: `cargo install --git https://github.com/firstintent/ccteam ccteam-cli`.

**Web console** binds to `0.0.0.0:7331` with token auth and no TLS — keep it on a trusted LAN; don't expose it to the public internet.

**Platforms.** Linux x86_64 / aarch64 and macOS arm64 / x86_64 (prebuilt). Linux binaries are musl-static (run on any glibc — NAS and older distros included). Windows is supported via WSL2 with the linux-x64 binary; tmux, inotify, and POSIX signals are foundational, so native Windows isn't supported. macOS Gatekeeper on first run: `xattr -d com.apple.quarantine ~/.local/bin/ccteam`.

## Docs

| What you want | Read this |
|---|---|
| Command guide — every CLI command, slash command, and IM control | [docs/usage.md](docs/usage.md) |
| Architecture — components, data protocols, design rationale | [docs/tech-design.md](docs/tech-design.md) |
| Requirements — the problems ccteam solves, and for whom | [docs/requirements.md](docs/requirements.md) |

## License

MIT — see [LICENSE](LICENSE). Built on [Claude Code](https://code.claude.com/) and OpenAI Codex.
