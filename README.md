# ccteam

> **Remote control for Claude Code, Codex, Grok & OpenCode — agents that run 24/7 on your own machine, driven from your phone.**

ccteam makes you asynchronous. A resident daemon keeps the stock [Claude Code](https://code.claude.com/), OpenAI Codex, Grok Build, and OpenCode CLIs alive around the clock; you drop in from Telegram, Lark/Feishu, or a web console — hand off work, answer the agent's mid-task question, approve the risky command — whenever it suits you, from wherever you are. ccteam owns everything *around* the agent (routing, session lifetime, cost) and never touches the work itself: **no injected prompts, no scraped terminals, no forked runtime.**

[Quickstart](#quickstart) • User manual: [English](docs/usage.md) · [中文](docs/usage-cn.md)

## Why

Coding agents got good. Using one still means being *synchronous* with it:

- The session lives in a terminal — close the laptop and it dies; the idea you get on the train waits until you're back at a desk.
- When the agent needs an answer or a permission, it blocks until you notice. So you sit there, babysitting a screen.
- One terminal is one serialized conversation — while real work wants a backend session, a frontend session, and a reviewer running side by side.

ccteam breaks the synchronous link: **the machine works; you decide — on your schedule, not the agent's.**

## Remote agent management — the base

- **Always on, nothing lost.** The daemon runs as a supervised service (systemd on Linux, launchd on macOS): it survives logout, crashes, and reboots, and sessions resume where they left off. Any past session — one you stopped, one from before a restart, even a native `claude` session you started outside ccteam — can be re-activated with its transcript intact. Per-vendor 24-hour budget caps put a hard ceiling on spend.
- **Everything from your phone.** The agent's full slash-command surface works from chat; picker commands (`/model`, review targets) become inline buttons; a mid-task question arrives as tappable options; files flow both directions. Start a session in `hitl` mode and every non-allowlisted tool call waits for your approve / deny — deny blocks that one call, never the whole turn.
- **A team, not a chat.** Sessions are independent, addressable agents (`s1`, `s2`, …), each with its own role, vendor, model, and context — run several in parallel across projects and switch with `@handle`. Watch any of them live in the console: transcript view or a byte-faithful terminal, with per-session spend.
- **Vendor-native, zero lock-in.** ccteam launches the real `claude` / `codex` / `grok` / `opencode` from your `PATH`, so a new vendor feature works the day it ships. Claude speaks its long-lived `stream-json` control plane, Codex its app-server, and Grok / OpenCode the standard Agent Client Protocol (ACP) over stdio — each normalized to the same session model, so chat, resume, and cost look identical from your phone. OpenCode cost is vendor-reported USD only (or "—" when missing). No prompt injection (the vendor loads its own persona file), no terminal scraping (state comes from transcripts and structured events), and your `CLAUDE.md` / `AGENTS.md` are never rewritten.
- **Compare & multi-host.** Run the same prompt across available vendors side by side (`/compare` and the Workflow → Compare page). Register satellite machines with `ccteam host join` and (for stdio protocols) spawn sessions on a remote host when online.

## Your workflow — built on top

The base is deliberately unopinionated. On top of it, everyone shapes their own workflow:

- **Roles** — a role is a plain Markdown persona in `.claude/agents/`; `ccteam init` seeds a `cto` manager, and `/role` swaps a session's role live.
- **Marketplace** — one-click-install roles, skills, workflows, and Claude Code plugins from a curated catalog that pins every upstream at a revision and integrity-checks every install.
- **Delegation** — from inside a session, the `mcp__ccteam__*` tools let the `cto` spawn a work-role session, dispatch a task to it, and collect the result.
- **Integration** — everything the console does is a self-documenting HTTP API (`/api/v1`, docs at `/api/docs`). One daemon serves multiple users: per-user web logins, per-user IM bots, isolated projects.

## Quickstart

**Let an agent install it for you.** In any Claude Code / Codex / Grok session on the target machine, paste one line:

> Install https://github.com/firstintent/ccteam for me.

The agent clones the repo and follows [`INSTALL.md`](INSTALL.md): `make install` brings the daemon up as a supervised service and prints a web console link where you register MCP and connect Telegram / Lark. Prefer to do it by hand? Same steps:

```bash
# 0. Install Claude Code first: https://code.claude.com/docs/install

# 1. Build from source and install as a service (needs Rust + Node.js):
git clone https://github.com/firstintent/ccteam && cd ccteam
make install    # release build → ~/.local/bin/ccteam → service (systemd / launchd), started

#    Prebuilt binary instead (no toolchain needed):
#    curl -sSL https://raw.githubusercontent.com/firstintent/ccteam/main/install.sh | sh
```

`make install` ends by printing a ready-to-click web console link. Finish setup there — register MCP (one-time), create a project, connect Telegram / Lark in **Settings → IM** — then start chatting:

```text
/cd myproject           # switch project → a `cto` session spins up; start chatting
/new   /use   @handle   # open / switch / address sessions
/status   /sessions     # deterministic control: state · cost · fleet · stop
```

Day-2 ops: `make daemon-logs` · `make daemon-restart` (rebuild + reload) · `ccteam status` · `ccteam doctor` · `make uninstall` (removes service + binary, keeps state).

> **Note:** the web console binds to `0.0.0.0:7331` with token auth and no TLS — keep it on a trusted LAN; don't expose it to the public internet.

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
      │ claude  │       │ codex   │        │ grok    │    own role + context (handle s<N>)
      └────┬────┘       └────┬────┘        └────┬────┘
           └────  the real Claude Code / Codex / Grok  ────┘   vendor persona, no injection
                              │
           your machine · your files · state on disk (resumes after any restart)
```

## License

MIT — see [LICENSE](LICENSE). Built on **Claude Code**, **OpenAI Codex**, and **Grok Build**.
