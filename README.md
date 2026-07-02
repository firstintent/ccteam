# ccteam

> **Remote Claude Code & Codex, done right — autonomous, online 24/7, answers the moment you ping.**

ccteam is a self-hosted gateway that turns the stock [Claude Code](https://code.claude.com/) and OpenAI Codex CLIs into a resident AI dev team. One daemon on your machine keeps every agent session alive and reachable; you drive them from Telegram, Lark/Feishu, or a local web console — spawn a session, hand it work, answer its mid-task questions, approve the risky command, read the result — all from your phone. ccteam owns everything *around* the work (routing, session identity, lifecycle, cost) and never touches the work itself: **no injected prompts, no scraped terminals, no forked runtime.**

[Quickstart](#quickstart) • User manual: [English](docs/usage.md) · [中文](docs/usage-cn.md)

## Why

Coding agents got good. Running them didn't.

- **You're chained to a desk.** Claude Code and Codex are desktop CLIs. The idea you get on the train — and the question your agent asks five minutes after you walk away — both wait until you're back at a keyboard.
- **You babysit instead of delegating.** Close the laptop and the session dies. Leave it running and you're checking the terminal every five minutes: stuck? done? off the rails?
- **One serialized conversation.** Real work wants a backend session, a frontend session, and a reviewer running side by side — with you switching between them like a tech lead, not retyping context into a single chat.

The ceiling on how much you can delegate stopped being the model a while ago — it's your attention, and your presence at a keyboard. ccteam removes that bottleneck without touching the agent: **your machine does the work; your phone does the deciding.**

## What you get

- **Your agents, in your pocket.** DM your bot on Telegram or Lark: every gateway command and the agent's full slash-command surface work straight from chat. Picker commands (`/model`, review targets) become inline buttons, an agent's mid-task questions arrive as tappable options, and files flow in both directions.
- **A team, not a chat.** Sessions are independent, addressable agents (`s1`, `s2`, …), each with its own role, vendor, model, and context. Run five in parallel across three projects; switch with `@handle`, see them all with `/sessions`.
- **Nothing is lost.** Sessions survive closed laptops, network drops, and daemon restarts. Any past session — one you stopped, one from before a restart, even a native `claude` session you started outside ccteam — can be re-activated with its transcript intact.
- **Hands-off by default, human-in-the-loop when it matters.** Sessions run unattended; start one in `hitl` mode and every non-allowlisted tool call pauses for your approve / deny from chat. Deny blocks that one call, never the whole turn.
- **Roles and a plugin marketplace.** A role is a plain Markdown persona in `.claude/agents/` — write your own, or one-click-install roles, skills, workflows, and Claude Code plugins from a curated catalog ([agency-agents](https://github.com/wshobson/agents), [mattpocock/skills](https://github.com/mattpocock/skills), official plugins, …) that pins every upstream at a revision and integrity-checks every install.
- **Cost you can see.** A live cost pill in the console, per-session spend, and per-vendor 24-hour budget caps with a hard ceiling.
- **A real API underneath.** Everything the console does is a standard, self-documenting HTTP API (`/api/v1`, interactive docs at `/api/docs`) — script it, integrate it, or build your own front end.
- **Multi-user.** Share one daemon: the owner mints per-user web login links, each user can connect their own IM bot, and projects and sessions stay isolated per user.
- **Vendor-native, zero lock-in.** ccteam launches the real `claude` / `codex` from your `PATH`. A new vendor feature works the day it ships; the vendor is a per-session choice, not a platform bet.

## Quickstart

```bash
# 0. Install Claude Code first: https://code.claude.com/docs/install

# 1. Install ccteam — recommended: build from source with cargo (needs a Rust
#    toolchain + Node.js for the web-console bundle → ~/.cargo/bin/ccteam):
cargo install --git https://github.com/firstintent/ccteam ccteam-cli

#    Fallback — prebuilt binary, no toolchain needed (→ ~/.local/bin/ccteam):
#    curl -sSL https://raw.githubusercontent.com/firstintent/ccteam/main/install.sh | sh

# 2. Initialize a project, then one-time setup (MCP for Claude + Codex, IM token, prefs):
cd ~/code/myproject && ccteam init
ccteam config

# 3. Start the daemon in the background — IM gateway + web console + API + MCP, one process:
nohup ccteam start >~/ccteam.log 2>&1 &   # foreground instead: just `ccteam start`
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
/cd myproject           # switch project → a `cto` session spins up; start chatting
/role backend-dev       # switch to a work-role
/new   /use   @handle   # open / switch / address sessions
@ccteam status          # group control: status / cost / stop
```

Manage from the CLI whenever you prefer (the daemon stays the source of truth): `ccteam project ls|new|stop|rm`, `ccteam session ls`, `ccteam status`, `ccteam doctor`.

Every CLI command, slash command, and IM control is in the user manual: [English](docs/usage.md) · [中文](docs/usage-cn.md).

> **Note:** the web console binds to `0.0.0.0:7331` with token auth and no TLS — keep it on a trusted LAN; don't expose it to the public internet.

## Three front ends, one team

- **IM (Telegram · Lark/Feishu)** — DM a bot to work a project, or `@ccteam <natural language>` in a group for control (`pause`, `cost`, `list`, `stop everything`). Made for the phone: quick check-ins, approvals, and course corrections from anywhere.
- **Web console** — one chat-style console (English or 中文, light/dark): create projects, open and switch sessions, watch live transcripts or a byte-faithful per-session terminal, install plugins from the marketplace, check each host's agent install / MCP status, and track spend at a glance.
- **Inside a session** — the `mcp__ccteam__*` MCP tools are the programmatic surface: register a bot, send a file to chat, take a screenshot, run a multi-agent vote. The default `cto` manager uses them to spawn a work-role session, dispatch a task to it, and collect the result.

## The model: chat ⇄ project ⇄ session ⇄ role

- A **chat** is one conversation surface — an IM DM or group, or a browser tab. It's your terminal: it spans projects and holds many live sessions at once. Another chat — another person, even on the same daemon — sees only its own sessions.
- A **project** is a local directory you ran `ccteam init` in.
- A **session** is one independent agent handle (`s<N>`) with its own context, exactly like a native Claude Code session — `/compact`, `/clear`, and `/model` are per-session. `/new` spawns, `/sessions` lists (project, vendor, role, model, live context), `@handle` / `/use` switches, `/cd` changes project. Sessions are durable: `/use <id>` — or the console's history list — re-activates **any** past session, and can even adopt a native Claude session started outside ccteam.
- A **role** is who a session *is* — a plain Markdown persona at `.claude/agents/<role>.md`, or no role at all (a bare `claude` guided by your project's `CLAUDE.md`). `ccteam init` seeds a `cto` manager that recommends the right work-role; swap any session's role live with `/role`.

Build your team by dropping `.md` files into `.claude/agents/` — or install them from the marketplace (web console, or `ccteam role search` / `add`).

## Under the hood

- **One gateway daemon.** `ccteam start` runs a single resident process: IM gateway + web server + HTTP API + local MCP socket. It routes and manages lifecycle; it never orchestrates or edits the agent's work.
- **No prompt injection, ever.** A session launches as `claude --agent <role>`, so the vendor loads and obeys its own persona file. Project memory (`CLAUDE.md` / `AGENTS.md`) stays yours — never generated, never rewritten.
- **No terminal scraping.** State comes from transcripts and structured vendor events, never parsed screen text. The web terminal is a read-only, byte-faithful mirror.
- **Sessions on demand.** Spawned when addressed, resumed by id, released when idle — context is preserved on disk, not held hostage in a process. Claude runs over a lightweight structured pipe by default; pick the terminal (tmux) channel per session when you want to attach, watch, or screenshot.
- **Never killed silently.** A long-running session is stopped only by you or by a budget cap — and a dead process resumes losslessly, or restarts fresh with a visible reset event rather than a silent memory wipe.

## Architecture

```text
   IM (Telegram · Lark/Feishu)          web console        MCP tools (in a session)
              │                             │                        │
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

## License

MIT — see [LICENSE](LICENSE). Built on [Claude Code](https://code.claude.com/) and OpenAI Codex.
