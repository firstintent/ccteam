# ccteam User Manual

**ccteam is a self-hosted, always-on background agent team: drive Claude Code / Codex / Grok Build / OpenCode / Kimi Code on your own machine from the web console, Telegram, or Lark/Feishu.**

Install once, start one resident process, then do daily work from three surfaces, listed in recommended order:

| Surface | Best for | Section |
|---|---|---|
| **Web console** | Create projects, start sessions, install plugins, configure IM, inspect status - the easiest default path | [1. Web console](#1-web-console-recommended) |
| **Telegram / Lark** | Mobile control, file exchange, tool approvals | [2. Telegram / Lark](#2-telegram--lark) |
| **CLI** | Scripts, ops, advanced headless use | [3. CLI](#3-cli-advanced) |

---

## Core Concepts

- **chat** = one conversation surface: one web console tab, Telegram/Feishu DM, or group. Each chat has its own current project, current session, and session list. Chats are isolated from each other.
- **project** = a local code directory registered with a short slug.
- **session** = one independent agent conversation with its own context, like a native Claude Code session. A project can have many sessions running side by side. Each session has a durable handle `s<N>` that survives restarts and is never reused.
- **role** = an optional persona bound at session start, loaded from `.claude/agents/<role>.md`. The default is **roleless**: the bare vendor reads the project's own `CLAUDE.md`/`AGENTS.md`. Personas are installed from the marketplace or written by you; ccteam seeds none.

> **ccteam only manages its own footprint.** It never edits your product code, `.git/`, `.env`, `CLAUDE.md`, or `AGENTS.md`. Project instructions stay owned by the project and are read natively by Claude and Codex.

---

## Before You Start: Install and Run the Service

These are the only terminal steps required. Afterward, the web console is the recommended surface.

### 1. Install

ccteam calls the Claude Code, Codex, Grok Build, OpenCode, and Kimi Code CLIs already installed and logged in on your machine. It does not bundle them.

```bash
# Recommended: build from source and install as a service.
# Requires Rust and Node.js (for the web console bundle).
git clone https://github.com/firstintent/ccteam && cd ccteam
make install

# Alternative: prebuilt binary, no toolchain required (also offers systemd setup).
curl -sSL https://raw.githubusercontent.com/firstintent/ccteam/main/install.sh | sh

ccteam --version
claude --version   # required; log in if prompted
codex --version    # optional; only needed for Codex sessions
grok --version     # optional; only needed for Grok Build sessions
opencode --version # optional; only needed for OpenCode sessions
kimi --version     # optional; only needed for Kimi Code sessions (`kimi login` first)
```

> If `ccteam` is not found, add `~/.local/bin` to PATH: `export PATH="$HOME/.local/bin:$PATH"`, then reopen your shell.

### 2. The Service

`make install` already started the service: one resident process (web console + IM gateway + standard resource API + MCP socket) supervised by systemd `--user` on Linux or a launchd agent on macOS — it starts at boot/login, restarts on crash, and survives logout. Manage it with `make daemon-status` / `daemon-logs` / `daemon-restart` / `daemon-stop` on either platform (macOS logs go to `~/.ccteam/daemon.log`). Uninstall with `make uninstall` (source) or `install.sh --uninstall` (prebuilt) — both remove the service and binary but keep `~/.ccteam`. Without any supervisor, run `ccteam start` in the foreground.

`make install`, and `ccteam status` at any time, print the web console URL, for example:

```text
web url:   http://<your-lan-ip>:7331/?token=ccteam:<token>
```

Open that link to enter the console.

---

## 1. Web Console (Recommended)

Open the link printed by `ccteam start`. The console is a chat-style UI with a **collapsible sidebar** (search with ⌘K, New session, Workflow, session list) and **no full-width top bar**. Cost and the avatar menu live in the sidebar footer. **Workflow** covers Skills / Roles / MCP / Evolution (read-only) / Compare. **Settings** links Hosts, Marketplace, Status, and IM credentials (admin). Theme defaults to **light** (dark remains available).

> **Access and security:** by default the web server binds to `0.0.0.0:7331` and uses token auth. The token is stored at `~/.ccteam/secrets/web-token`. The web console has **no TLS** and transmits plaintext; use it only on a trusted LAN, and do not expose it to the public internet. For a stricter local-only mode: `ccteam start --web-bind 127.0.0.1:7331` (tokenless local bind).

### Register MCP (One-Time)

Open the **Hosts** page and click **Register ccteam MCP**. This writes ccteam's own tools (session spawning/dispatch, file sending, screenshots, and related controls) into the configuration of **all five vendors** — Claude (`~/.claude.json`), Codex (`~/.codex/config.toml`), Grok (`~/.grok/config.toml`), OpenCode (`~/.config/opencode/opencode.json`), Kimi (`~/.kimi-code/mcp.json`) — so a plain session of ANY vendor can orchestrate the team (`grok mcp doctor` verifies the Grok side). The Hosts page also reports which vendors are installed, their versions, and readiness.

### Create a Project

In the new-session dialog, choose **+ New project...**, enter a slug and directory path, and ccteam registers that directory as a project. If the same slug already exists, ccteam appends a number such as `demo2` or `demo3`.

### Start, Switch, and Drive Sessions

- **New session:** choose a vendor (Claude / Codex / Grok / OpenCode / Kimi) and protocol (stream-json / terminal for Claude admin-only / ACP for Grok, OpenCode and Kimi), optional effort, and HITL at spawn time. The execution host is the project's — sessions run wherever their project is bound, and every session row wears a vendor chip. Roles come from the project's `.claude/agents/` (admin picker); tenants create roleless sessions. The session gets a handle like `s1`.
- **Each session** has **Chat | Terminal** tabs. Chat renders assistant output as Markdown, including headings, lists, tables, and code blocks with copy buttons. Press **Enter** to send, **Shift+Enter** for a newline, and stop an in-flight turn from the UI.
- **Dedicated session page:** `/app/chat/s/<sid>` is a clean view for one session. It has that session's history and session-filtered live events, without mixing other sessions.
- **Terminal tab:** a byte-faithful mirror of the session screen, including ANSI, cursor, and alignment. Currently available for Claude sessions. Codex, Grok, OpenCode, and Kimi are chat-only (Grok / OpenCode / Kimi run over ACP, with no terminal mirror).
- **History and resume:** click **More history (N)** under the session list to expand stopped-but-not-destroyed sessions. Click any row to cold-resume it from disk `meta.json`. Stopped sessions, sessions from before a daemon restart, and `/use <sid>` from mobile all resume the same way. **Import historical session** can find native Claude sessions started outside ccteam (matched by working directory) and adopt them into ccteam while keeping the transcript.

> Some advanced options (terminal/rmux protocol selection, role selection in web, history resume, and external session import) are currently admin-only. Regular users get the standard Claude / Codex / Grok / OpenCode / Kimi chat flow by default; advanced controls will open up as they stabilize.

### Marketplace: Install Roles, Skills, and Workflows

The **Marketplace** page browses curated plugins from [ccteam-hub](https://github.com/firstintent/ccteam-hub). Official ccteam plugins are shown first, followed by tracked open-source sources such as [agency-agents](https://github.com/wshobson/agents) and [mattpocock/skills](https://github.com/mattpocock/skills). Open an item to preview its body, then install it into the current project. Installs verify sha256 and show installed status. After installing a role, switch to it from any surface with `/role <role>`.

### Configure Telegram / Lark

Open **Settings** and enter IM credentials:

- **Telegram:** paste the bot token, save it, then send the bot a message. The page polls and captures your chat id.
- **Lark/Feishu:** enter App ID, App Secret, region (Feishu China / Lark international), and allowed users.

Secrets are masked (`...last4`) and never returned in plaintext. **Restart the daemon after changing global IM credentials** because they are loaded at startup. The page will show `restart required`. Per-user IM bots are hot-reloaded; see [Multi-User](#multi-user).

Detailed bot setup is in [2. Telegram / Lark](#setup).

### Multi-User

One daemon can serve multiple users on one machine. This is **soft isolation** under one OS account: a UX boundary, not a security boundary.

- Admins can create users in **Settings -> User Management**. Each user receives a one-time personal login link and sees only their own projects and sessions.
- Each user can configure their own IM bot in **Settings -> My IM bot**. Save validates the token and applies immediately without daemon restart. That bot drives only that user's sessions. **Each bot token must be unique.**

### Status and Cost

- **Status** shows daemon health, live/idle session counts, per-session cost, and today's total cost / budget. The top-bar cost pill uses the same data.
- Cost is tracked separately by vendor. Claude / Codex / Grok use embedded tables when the model is known; **OpenCode uses only vendor-reported USD** (or "—" when missing/zero — never another vendor's price table); **Kimi always shows "—"** (its ACP wire carries no usage/cost).

### Standard Resource API

The console is built on a token-authenticated HTTP API you can use directly:

- Interactive docs: `http://<host>:7331/api/docs` (Scalar). Machine-readable spec: `/api/v1/openapi.json`.
- Resources include `/api/v1/projects`, `.../projects/{slug}/sessions`, `/sessions/{sid}/{turn,events,stop}`, `/marketplace`, `/status`, `/hosts`, and `/capabilities`.
- Auth uses the same web token. Session endpoints require the daemon to be online.

---

## 2. Telegram / Lark

After connecting IM, you can drive sessions, send files, and approve tools from your phone. The easiest setup is [Web console Settings](#configure-telegram--lark). You can also use the `ccteam config` menu or write the credentials file manually.

### Setup

**Telegram:** talk to `@BotFather`, run `/newbot`, and copy the token. Configure it one of three ways:

1. **Web** (recommended): paste the token in Settings and let the console capture chat id.
2. **CLI menu:** run `ccteam config`, choose the IM bot token option, validate the token, and capture chat id.
3. **Credentials file** at `~/.ccteam/secrets/im-credentials.json` (directory `0700`, file `0600`):

```json
{
  "telegram": {
    "bot_token": "123456:replace_me",
    "allowed_chat_ids": ["123456789"]
  }
}
```

`allowed_chat_ids` is the safety boundary. Only listed chats can reach the daemon. **Do not leave it empty in production.** To find a chat id, send the bot a message, then run `curl -s "https://api.telegram.org/bot<token>/getUpdates"` and look for `message.chat.id`.

**Feishu / Lark** can coexist with Telegram and uses native WebSocket long connection, with no public callback URL. In the developer console (Feishu: `open.feishu.cn`, Lark: `open.larksuite.com`), create an app, enable the bot, choose **WebSocket** event subscription, subscribe to `im.message.receive_v1`, grant `im:message` and `im:message:send_as_bot`, then copy App ID (`cli_...`) and App Secret. Configure through Web Settings / `ccteam config`, or add a `lark` block:

```json
{
  "lark": {
    "app_id": "cli_replace_me",
    "app_secret": "replace_me",
    "allowed_user_ids": ["ou_replace_me"],
    "use_feishu": true
  }
}
```

- `use_feishu`: `true` for Feishu (China), `false` for Lark international.
- `allowed_user_ids` is an open_id allowlist (`ou_...`). **Empty means reject everyone** (fail closed). To get your open_id, start with an empty list, message the bot, find `ignoring ou_xxxx (not in allowed_users)` in logs, and add that `ou_xxxx`.

> Manual credentials file changes require daemon restart. The same applies to global credentials configured in Web Settings. Lark/Feishu and Telegram are peers: text, rich text, images, and files are supported.

### Gateway Commands

Send these commands in chat. The gateway handles them directly. Use `/help` anytime; Telegram also shows command candidates when you type `/`.

```text
# Projects
/cd <project>              Switch to a project. First message starts a roleless session.
/projects                  List known projects.
/newproject <slug> <path>  Create and register a project, then switch to it.

# Sessions
/new [vendor] [role] [hitl]  Create a session and return handle s<N>.
                             vendor = claude (default) | codex | grok | opencode | kimi
/compare <question>          Fan the same question across available vendors
                             omit role = bare Claude reading CLAUDE.md; provide role to bind it
                             grok/opencode/kimi = roleless ACP session (role arg ignored)
                             add hitl = approve tools in IM; default skip runs directly
/use <id>                  Switch to session s<N>; stopped sessions cold-resume from disk.
/role <role>               Change the current session role in place; handle stays the same.
/interrupt [id]            Interrupt an in-flight turn; keep the session. Omit id for current.
/stop <id>                 Destroy a session.
/screen [id]               Screenshot the current screen. Omit id for current.

# Inspect / onboard
/sessions [all]            List sessions for current project; all = across projects.
/status                    Team health: idle / working / stuck plus model and context.
/help                      List gateway commands.
```

### Addressing

```text
@<role>          Switch to that role's session and make it current. Alone = switch only.
@<role> <text>   Switch to it and send a message.
```

`@` always addresses a session. Deterministic control is the slash surface above (`/status`, `/sessions`, `/stop`, …); free-form ops questions ("which project burned the most today?") are ordinary chat to a session — any session answers them with the ccteam MCP tools.

### Direct Chat and File Exchange

- **Messages without a prefix** go to the current session.
- **Non-gateway slash commands** (`/compact`, `/clear`, `/model`, etc.) pass through to the current agent. Picker commands such as `/model` become option buttons.
- **Images or files plus a note** are read by the agent automatically (screenshots and logs work well). Agents can send files and screenshots back to chat.
- **During an in-flight turn,** ccteam keeps a live progress message such as `working... · bash x3`. The final answer arrives separately and long answers are chunked. If the agent asks a question, it appears as option buttons; tap one and the agent continues.

### Human-in-the-Loop (HITL)

Sessions default to direct execution (`skip`). Start an approval-gated session with `/new <vendor> <role> hitl`. Before non-allowlisted tools run, ccteam sends the requested action plus approve / deny buttons. Approve runs the tool; deny blocks only that tool call and does not kill the turn. Codex sessions have their own sandbox and ignore this mode. Grok and Kimi sessions currently run in `skip` (auto-approve) only; IM approval for them is planned but not yet wired.

### Let Any Session Dispatch Work

Every session can spawn colleagues, dispatch tasks, and collect results through the `mcp__ccteam__session_*` tools — no manual switching, no special role. Just ask in natural language:

```text
start a codex session, implement the RFC under docs/rfc-12.md, and report back when tests pass
```

For everyday use, wrap the phrasing in a skill (e.g. a user-level `cct-codex` that spawns, supervises, and reports back with a short summary) so one sentence does the whole loop reliably. The plain-language guide — what to say, best practices, plus a tool appendix for skill authors — is [orchestration.md](orchestration.md).

---

## 3. CLI (Advanced)

Use Web / IM for daily work. The CLI is for scripts, ops, and headless environments. Commands are split into flat lifecycle commands (`init / config / start / stop / status / doctor`) and grouped commands (`project / session / role`).

### Install-Time and Service Commands

```bash
ccteam init                    # Initialize the current directory as a project (slug = dir name).
ccteam init --in /path/to/repo # Initialize elsewhere.
ccteam init --slug demo        # Override inferred slug.
ccteam init --owner user:u123  # Multi-user: assign project ownership.
ccteam config                  # One-time setup: MCP, IM bot, preferences.
ccteam config mcp              # Register/refresh ccteam MCP for all four vendors; useful without TTY.
ccteam start                   # Start resident service; add & for background.
ccteam start --web-bind 127.0.0.1:7331   # Local-only bind, no token.
ccteam start --no-web | --no-imd         # Gateway only / web only.
ccteam stop                    # Gracefully stop daemon.
ccteam status                  # Daemon heartbeat, projects, sessions, web link.
ccteam doctor                  # Install/dependency checks; --verify-mcp checks MCP surface.
```

`ccteam init` only writes ccteam-owned files: project `.ccteam/` and the ccteam hook section in `.claude/settings.local.json` (it seeds no roles — `.claude/agents/` stays yours). It does **not** touch your `.claude/settings.json`. Re-running is safe. Preferences live in `~/.ccteam/preferences.toml`; currently `fallback.on_claude_quota = off|codex` controls whether Claude quota exhaustion falls back to Codex.

### `project` (Project Lifecycle)

```bash
ccteam project ls                  # List known projects.
ccteam project show demo           # Full project status and recent events.
ccteam project new demo --team dev # Create under <projects_root>/dev-demo/ and init.
ccteam project stop demo           # Stop all project sessions; resumable by id.
ccteam project rm demo             # Deregister project and clear ccteam state.
ccteam project rm demo --dry-run   # Preview what would stop/delete.
ccteam project rm demo --purge     # Deregister and remove ccteam-owned project traces.
```

`rm --purge` removes only ccteam-owned traces: project `.ccteam/` and ccteam hook entries in `settings.local.json`. It **always keeps** your work roles, `CLAUDE.md` / `AGENTS.md`, `.env`, product code, and `.claude/settings.json`.

### `session` (Sessions and Bot Registration)

```bash
ccteam session ls                          # List gateway sessions; marks orphans.
ccteam session attach demo reviewer        # Attach to a session.
ccteam session pause demo / resume demo    # Pause/resume project dispatch; never kills long sessions.
ccteam session persona demo reviewer -     # Replace a role .md with stdin.
ccteam session add-tool demo reviewer "Bash(git*)"   # Add one tool rule to a role.
ccteam session register / bots / unregister ...      # Manage bot registration for scripts/no daemon.
```

> Change a live session's role from IM with `/role <role>` because it needs daemon in-memory state. CLI `session role` only prints this guidance.

### `role` (Install Roles from the Marketplace)

```bash
ccteam role search backend         # Search marketplace; official plugins first; --format json available.
ccteam role add backend-architect  # Fetch role .md, verify sha256, write to .claude/agents/.
ccteam role add data-scientist --project demo   # Install into a named project.
ccteam role list                   # List roles installed in current project.
```

ccteam reads ccteam-hub over HTTPS with a local cache at `~/.ccteam/cache/hub/`, fetches upstream files pinned to fixed commits, verifies sha256, and writes only when missing unless `--force` is used. Multi-file skills install under `.claude/skills/<id>/`. The web marketplace uses the same catalog.

### Operations

```bash
ccteam status                      # Daemon + projects/sessions + web token/url lines.
ccteam session ls                  # Gateway session status; degrades when daemon is offline.
ccteam doctor --verify-mcp         # MCP surface check: 8 tools / 0 stubs; drift exits 1.
ccteam doctor --check-cost-orphan  # Cost ledger reconciliation.
```

Restart daemon only; sessions reconnect by id afterward:

```bash
systemctl --user restart ccteam    # or: make daemon-restart (rebuilds first)
```

State file quick reference. `~/.ccteam` is grouped by responsibility: `secrets/` for credentials, `state/` for daemon-written state, `cache/` for disposable cache, and `run/` for sockets.

```bash
journalctl --user -u ccteam -n 120               # Daemon log (systemd journal; or make daemon-logs).
cat ~/.ccteam/config.yaml                        # Project registry: slug -> path.
cat ~/.ccteam/state/gateway/routing.json         # Chat routing: current project/session + live set.
cat ~/.ccteam/state/sessions/next-sid            # Monotonic sid counter; never reused.
cat <project>/.ccteam/chat/<sid>/meta.json       # Session SoT: vendor/role/owner/uuid...
tail ~/.ccteam/state/im/outbound.jsonl           # Outbound ledger; replayed after restart.
cat <project>/.ccteam/progress.jsonl             # Project business events; state authority.
```

Environment variables:

```bash
CCTEAM_HOME=~/.ccteam2          # Isolate a full state/config/session tree; pairs with ccteam --home.
CCTEAM_PROJECTS_ROOT=...        # Default project root; default ~/projects.
CCTEAM_CLAUDE_BIN=... CCTEAM_CODEX_BIN=... CCTEAM_GROK_BIN=... CCTEAM_OPENCODE_BIN=...
# Override vendor CLI paths (tests / non-PATH installs).
```

### Multi-Machine (Satellites)

Every node runs the same `ccteam start`. A node becomes a **satellite** of another daemon by joining it once — after that it **dials out** to the daemon (reverse connection): only the daemon needs a reachable address/port (`:7331`); satellites expose nothing and work from behind NAT/firewalls. Put an HTTPS reverse proxy in front of the daemon and all satellite traffic is `wss`.

```bash
# On the daemon (or web console → 主机 page): mint a join token.
ccteam host mint-token --daemon http://daemon-host:7331 --web-token <admin-hex>

# On the satellite (any machine running ccteam start):
ccteam host join --daemon http://daemon-host:7331 --token <join-token>
# The running `ccteam start` picks the join up within 30s and connects out.

ccteam host ls                     # This machine's satellite credentials (if joined).
```

The satellite reports its agents and registered projects every ~25s over its control channel; the hosts page shows online/offline live. **Projects are bound to a host** — to run sessions on a satellite, give it a project there and spawn into that project (no per-session host choice):

- **Create remotely:** web console → new project → pick the satellite in the host picker → absolute path on that machine. The daemon asks the satellite to bootstrap and register it in place.
- **Import an existing checkout:** `ccteam init` in the repo on that machine, then hosts page → 接入/Import next to the reported project. Same-slug collisions get a distinct catalog slug (`demo` → `demo2`) — slug equality across machines is not project identity.

Remote execution currently supports Claude stream-json sessions; the connection self-heals with backoff, and a dropped exec link resumes context via vendor `--resume` on the next spawn. Fleet capacity: at most 50 live sessions daemon-wide (configurable `sessions.max_live`); admitting one more gracefully stops the least-recently-active idle session, which stays resumable.

---

## Troubleshooting

Start with these three commands; they usually locate the issue:

```bash
ccteam doctor
ccteam status
journalctl --user -u ccteam -n 120
```

1. **`ccteam: command not found`** - `~/.local/bin` is not in PATH. Run `export PATH="$HOME/.local/bin:$PATH"`.
2. **Telegram does not reply / log says `drop msg from non-allowed chat`** - chat id is not allowlisted, or credentials changed without restart. Fix `allowed_chat_ids` in `~/.ccteam/secrets/im-credentials.json` or Web Settings, then restart daemon.
3. **IM says send failed / session has no output yet** - restart daemon and send the same `@handle` again. For long contexts, first try `@bot /compact`; if it keeps failing, start a fresh session with `/new`.
4. **`/cd` or `/new` says project not found** - initialize or reload the project: `cd <repo> && ccteam init`, restart daemon, check `/projects`, then `/cd <slug>`.
5. **Web does not open / asks for token** - use the full `web url` printed at the end of `ccteam status`. Or bind locally with `--web-bind 127.0.0.1:7331` to skip token.

> Claude sessions from IM default to `skip` (direct execution, no approval gate). Expose the bot only to trusted chats, and never commit bot tokens. For per-tool approval, start with `/new <vendor> <role> hitl`.
