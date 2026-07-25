# ccteam User Manual

**ccteam turns the coding agents you already run (Claude Code, Codex, Grok, Kimi…) into one team — any session can spawn, dispatch, and collect work from any vendor on any machine, while you steer it all from Telegram, Lark, or a browser tab.**

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

**1 · Let an agent do it**

Paste into any agent you already have:

> Install https://github.com/firstintent/ccteam — follow `INSTALL.md` in the repo.

**2 · From source** — recommended; requires Rust + Node.js (for the web console bundle):

```bash
git clone https://github.com/firstintent/ccteam && cd ccteam
make install
```

**3 · One-click script** — prebuilt binary, no toolchain required:

```bash
curl -sSL https://raw.githubusercontent.com/firstintent/ccteam/main/install.sh | sh
```

Then verify:

```bash
ccteam --version
claude --version   # required; log in if prompted
codex --version    # optional; only needed for Codex sessions
grok --version     # optional; only needed for Grok Build sessions
opencode --version # optional; only needed for OpenCode sessions
kimi --version     # optional; only needed for Kimi Code sessions (`kimi login` first)
```

> If `ccteam` is not found, add `~/.local/bin` to PATH: `export PATH="$HOME/.local/bin:$PATH"`, then reopen your shell.

### 2. The Daemon

`make install` (and the one-click script, when it detects an upgrade) already started the daemon with **`ccteam daemon start`**: one resident process (web console + IM gateway + standard resource API + MCP socket), detached with `setsid` so it survives your shell closing and SSH disconnects. This is the single supervision mechanism on Linux, macOS, and WSL — **there is no systemd or launchd unit anymore** (retired in v0.9.7). Manage it the same way on every platform:

```bash
ccteam daemon status     # pid · ready · running-vs-binary version   (add --json for scripts)
ccteam daemon logs -f    # follow ~/.ccteam/daemon.log
ccteam daemon restart    # graceful SIGTERM stop + re-detach (also `make daemon-restart`)
ccteam daemon stop       # graceful stop; sessions resume by id on the next start
```

Honest tradeoff: with no OS supervisor there is **no crash-restart and no auto-start at boot** — after a reboot, run `ccteam daemon start` again (`ccteam status` / `ccteam doctor` show a down daemon at a glance; a `@reboot ccteam daemon start` cron line covers boot-start if you want it). Uninstall with `make uninstall` (source) or `install.sh --uninstall` (prebuilt) — both stop the daemon and remove the binary but keep `~/.ccteam`.

**Upgrading from a pre-v0.9.7 install (systemd/launchd):** just re-run the installer or `ccteam daemon start` once — its one-time takeover disables and removes the old ccteam service unit and brings the daemon back up self-managed. A unit you wrote by hand is left untouched (ccteam reports that instance as "not managed" and won't stop it — use your own supervisor). See [Updating](#updating) for the ongoing path.

Without detaching at all (dev / containers / your own supervisor's `ExecStart`), `ccteam start` still runs in the foreground.

`make install`, and `ccteam status` at any time, print the web console URL, for example:

```text
web url:   http://<your-lan-ip>:7331/?token=ccteam:<token>
```

Open that link to enter the console.

---

## 1. Web Console (Recommended)

Open the link printed by `ccteam start`. The console is a chat-style UI with a **collapsible sidebar** (search with ⌘K, New session, Workflow, session list) and **no full-width top bar**. Cost and the avatar menu live in the sidebar footer. **Workflow** covers Skills / Roles / MCP / Evolution (read-only). **Settings** links Hosts, Marketplace, Status, and IM credentials (admin). Theme defaults to **light** (dark remains available).

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
- **Attach files and skills:** the composer's **＋** menu uploads files or photos (drag-and-drop and clipboard paste work too — attachments show as removable chips while they upload), and attaches skills from two sections: the **project's own skills** (`.agents/skills/`, with legacy `.claude/skills/` entities still read) and — admins only — the user-level **global skill library** (`~/.ccteam/skills`, nested ids included; attaching is a per-message pointer and never copies anything into the project). Files and skills ride the message for **every vendor**: files land on disk and the turn carries their path for the agent to read (the same mechanism as sending a photo over Telegram); an attached skill adds a read-and-follow pointer to its `SKILL.md`, so it works even for vendors with no native skill loader. Files go to the project's `.ccteam/uploads/` (local-host projects; remote/satellite projects are politely rejected for now).
- **Schedule a message:** tap the **clock** on the composer to enter schedule mode. Enter **how many minutes and/or hours** from now (or tap chips `+15m` / `+30m` / `+1h` / `+2h`), or pick a **local-clock** datetime — the UI converts everything to a relative delay so browser timezone and daemon timezone never disagree. A preview shows the expected local send time. Type the text and send — the message joins a **queue above the input**, sorted by send time; cancel with **×**. At fire time the text is a normal user turn into that session. Schedule mode does not carry file/skill attachments. Caps: 20 pending per session, farthest **7 days** ahead. Failed deliveries stay in the queue (marked failed) for 24 hours so you can dismiss them.

> Some advanced options (terminal/rmux protocol selection, role selection in web, history resume, and external session import) are currently admin-only. Regular users get the standard Claude / Codex / Grok / OpenCode / Kimi chat flow by default; advanced controls will open up as they stabilize.

### Marketplace: Install Roles, Skills, and Workflows

The **Marketplace** page browses curated plugins from [ccteam-hub](https://github.com/firstintent/ccteam-hub). Official ccteam plugins are shown first, followed by tracked open-source sources such as [agency-agents](https://github.com/wshobson/agents) and [mattpocock/skills](https://github.com/mattpocock/skills). Open an item to preview its body, then install it. Agents (roles) install into the current project's `.claude/agents/`; **skills install into the user-level global library** `~/.ccteam/skills` — never into the project — and are attached per message from the composer. Installs verify sha256 and show status (skill status is computed against the library). After installing a role, switch to it from any surface with `/role <role>`.

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
- Resources include `/api/v1/projects`, `.../projects/{slug}/sessions`, `/sessions/{sid}/{turn,events,stop,scheduled}`, `/marketplace`, `/status`, `/hosts`, and `/capabilities`.
- Auth uses the same web token. Session endpoints require the daemon to be online.

### External agents over MCP (`POST /mcp`)

Any agent ccteam does not manage (your own script, a CLI running elsewhere) can call the daemon's MCP endpoint directly with a **ccteam web token** and get the same eight tools a managed session has:

```
POST http://<host>:7331/mcp
Authorization: Bearer ccteam:<hex>
```

- The **admin token** (the one `ccteam status` prints) is the owner's front door, fleet-wide. A **per-user token** (issued under Settings → Users) resolves to that user: every tool is scoped to the user's own projects — `session_spawn` requires an explicit `project` and only accepts the user's own, `dispatch/collect/stop` authorize by the target sid's project, and `session_list`/`status` aggregate only visible projects.
- Unknown and forbidden projects/sessions return the same error (anti-enumeration). Bearer-only: cookies and query tokens are never accepted.

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

# Delayed send (one-shot user turns)
/inbox                     List scheduled messages you can see (own + web pool), by send time.
/inbox <time> <text>       Schedule text into the **current** session (/use first if needed).
/inbox cancel <dN>         Cancel (or dismiss a failed) item by short id from the list.
```

Time forms for `/inbox <time> …` (daemon local timezone; past times are rejected, bare `HH:MM` does **not** roll to tomorrow):

```text
/inbox +30m remind me to open the PR
/inbox +2h run the nightly checklist
/inbox 22:30 write the daily summary
/inbox 明天 09:00 morning standup notes
/inbox 2026-07-26 09:00 release checklist
```

List lines look like `d3 · s12 · 2026-07-26 09:00 · preview…` (failed rows carry a reason). Successful fires are silent in IM — the text just appears as a normal user message in that session. Failures notify you and stay listed for 24 hours. Same limits as the web queue (20 per session, 7-day horizon). Empty text is rejected; a body that starts with `/` is still sent as ordinary agent text at fire time (not re-parsed as a gateway command).

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

There is no skill to install — the ccteam MCP server ships its own instructions, so any connected session already knows the whole loop (spawn, supervise, report back). For a standing orchestrator persona, install `team-brain` from the marketplace. The plain-language guide — what to say, best practices, plus a tool appendix for persona/skill authors — is [orchestration.md](orchestration.md).

### Model Routing

A session deciding whom to spawn never has to guess. One `status` call (the MCP tool) answers with a **vendor panel** for the host your current project is bound to: which vendors are installed and their versions, an honest auth signal (`ready` / `not_ready` / `unknown` — sitting on PATH never masquerades as logged in), budget state, and whether the snapshot is fresh or stale. Alongside it comes an **advisory model catalog** — runtime last-seen data and the hub `models.json`, each labeled with its source and never consulted as a spawn allowlist — plus your **routing notes**, transported verbatim.

Your division of labor is plain markdown you own; ccteam carries it to any session that asks, on any host, and never parses, merges, or executes it:

- `~/.ccteam/routing.md` — the global fallback. The shared home initializer creates a neutral starter when it is missing and never overwrites your content.
- `<project>/.ccteam/routing.md` — an optional project override. When present it replaces the global file completely; the two are not merged.

Write it as a dumb table of task type → vendor/model/effort → reason. Default posture: **omit `model` at spawn** and ride the vendor default (free upgrades as vendors ship new models); the routing table only lists exceptions and upgrades. The full recipe — capability check, fan-out compare, synthesis, cost — is the [Model routing chapter](orchestration.md) of the orchestration guide.

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
ccteam config mcp              # Register/refresh ccteam MCP for all five vendors; useful without TTY.
ccteam daemon start            # Start the daemon in the background (setsid; idempotent).
ccteam daemon stop [--force]   # Graceful stop; --force escalates to SIGKILL (daemon only).
ccteam daemon restart          # Graceful stop + re-detach under one lock.
ccteam daemon status [--json]  # pid · ready · running-vs-binary version.
ccteam daemon logs [-f] [-n N] # Tail/follow ~/.ccteam/daemon.log.
ccteam start                   # Run in the FOREGROUND (dev / containers / your own supervisor).
ccteam start --web-bind 127.0.0.1:7331   # Local-only bind, no token.
ccteam start --no-web | --no-imd         # Gateway only / web only.
ccteam stop                    # Alias for `ccteam daemon stop`.
ccteam update [--now] [--no-restart] [--json]   # Update in place, then restart onto the new binary.
ccteam status                  # Daemon heartbeat, projects, sessions, web link, version/update hint.
ccteam doctor                  # Install/dependency checks; --verify-mcp checks MCP surface.
```

`ccteam init` only writes ccteam-owned files: project `.ccteam/` and the ccteam hook section in `.claude/settings.local.json` (it seeds no roles — `.claude/agents/` stays yours). It does **not** touch your `.claude/settings.json`. Re-running is safe. Preferences live in `~/.ccteam/preferences.toml`; currently `fallback.on_claude_quota = off|codex` controls whether Claude quota exhaustion falls back to Codex.

### `project` (Project Lifecycle)

```bash
ccteam project ls                  # List known projects.
ccteam project show demo           # Full project status and recent events.
ccteam project new demo            # Create under <projects_root>/demo/ and init (collision appends demo2, demo3, …).
ccteam project stop demo           # Stop all project sessions; resumable by id.
ccteam project rm demo             # Deregister project and clear ccteam state.
ccteam project rm demo --dry-run   # Preview what would stop/delete.
ccteam project rm demo --purge     # Deregister and remove ccteam-owned project traces.
```

`rm --purge` removes only ccteam-owned traces: project `.ccteam/` and ccteam hook entries in `settings.local.json`. It **always keeps** your work roles, `CLAUDE.md` / `AGENTS.md`, `.env`, product code, and `.claude/settings.json`.

### `session` (Sessions)

```bash
ccteam session ls                # List gateway sessions; marks orphans.
ccteam session attach demo [sid] # Attach to a terminal-protocol session's pane.
```

> `attach` only applies to `terminal`-protocol sessions (they have a tmux pane). Default `stream-json` sessions have no pane — drive them from the web chat console or IM. Change a live session's role from IM with `/role <role>`.

### `role` (Install Roles from the Marketplace)

```bash
ccteam role search backend         # Search marketplace; official plugins first; --format json available.
ccteam role add backend-architect  # Fetch role .md, verify sha256, write to .claude/agents/.
ccteam role add data-scientist --project demo   # Install into a named project.
ccteam role list                   # List roles installed in current project.
```

ccteam reads ccteam-hub over HTTPS with a local cache at `~/.ccteam/cache/hub/`, fetches upstream files pinned to fixed commits, verifies sha256, and writes only when missing unless `--force` is used. Skills install into the user-level global library — multi-file skills land whole under `~/.ccteam/skills/<id>/` after the entire batch verifies. The web marketplace uses the same catalog.

Skills have their own command group (`ccteam role add` refuses skill ids and points here):

```bash
ccteam skill search research        # Search marketplace skills.
ccteam skill add deep-research      # Install into the global library ~/.ccteam/skills.
ccteam skill ls                     # List the library; ids may nest (baoyu-skills/baoyu-comic).
ccteam skill rm <id>                # Remove one skill; --force removes a whole tree.
ccteam skill update --all           # Re-sync hub-pinned skills that drifted.
ccteam skill source add <git-url>   # Register a multi-skill repo into the library.
ccteam skill source update --all    # Sync registered sources (ls / rm work too).
ccteam skill ensure-project         # Project-own skills: .agents/skills + .claude/skills symlink.
ccteam skill migrate-project        # Move legacy .claude/skills entities into .agents/skills.
```

The global library and project skills never mix: nothing links or copies from the library into a project — sessions reference library skills per message (composer attach), while project-own skills live in the project as normal git-visible files.

### Operations

```bash
ccteam status                      # Daemon + projects/sessions + web token/url lines.
ccteam session ls                  # Gateway session status; degrades when daemon is offline.
ccteam doctor --verify-mcp         # MCP surface check: 8 tools / 0 stubs; drift exits 1.
ccteam doctor --check-cost-orphan  # Cost ledger reconciliation.
```

Restart daemon only; sessions reconnect by id afterward:

```bash
ccteam daemon restart              # or: make daemon-restart (rebuilds release first)
```

### Updating

```bash
ccteam update                      # Update in place, then restart the daemon onto the new binary.
ccteam update --no-restart         # Swap the binary only; apply later with `ccteam daemon restart`.
ccteam update --now                # Skip the drain wait and restart immediately.
```

`ccteam update` detects how ccteam was installed and re-runs that install path — for the one-click / prebuilt install it replays `install.sh` (same download + SHA-256 verify + atomic swap; no second downloader). A from-source checkout is never recompiled for you: it prints `git pull && make install`. After the binary is swapped, if a managed daemon is running, `update` waits for in-flight turns to go idle (up to 5 minutes; `--now` skips the wait), gracefully restarts the daemon onto the new binary, and verifies the running version matches.

What a daemon restart does to live sessions is the existing resume-by-id contract, stated plainly: `terminal`/tmux sessions keep running (separate process tree); a default `stream-json`/ACP session's child process ends with the daemon, its in-flight turn reports `stream_closed_in_flight`, and the session resumes its context by id on the next message. `ccteam status` and `ccteam doctor` show the install channel, the running-vs-binary version, and whether a newer release is available (a lazy check, at most once every ~20h; toggle with `check_for_update` in `preferences.toml`). **Satellites update themselves** — run `ccteam update` on each; the console's Hosts view and `ccteam status` flag any host whose version lags the daemon.

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
