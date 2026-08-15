<div align="center">
  <img src="assets/logo.svg" width="132" alt="ccteam mascot — a juggler bot keeping codex, grok and kimi in the air" />
  <h1>ccteam</h1>
  <p><b>ccteam turns the coding agents you already run (Claude Code, Codex, Grok, Kimi, Deepseek Harness) into one team —<br/>any session can spawn, dispatch, and collect work from any vendor on any machine,<br/>while you steer it all from Telegram, Lark, or a browser tab.</b></p>
  <p>
    <a href="https://github.com/firstintent/ccteam/actions/workflows/check.yml"><img src="https://github.com/firstintent/ccteam/actions/workflows/check.yml/badge.svg" alt="CI" /></a>
    <img src="https://img.shields.io/badge/made%20with-Rust-b7410e" alt="Made with Rust" />
    <img src="https://img.shields.io/badge/platform-macOS%20%C2%B7%20Linux%20%C2%B7%20WSL-4c8dae" alt="macOS · Linux · WSL" />
    <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue" alt="MIT" /></a>
  </p>
</div>

<p align="center">
  <img src="assets/orchestration.svg" width="1000" alt="you, from any device, drive a claude brain that spawns and dispatches codex, grok and kimi on their strengths — each on its own machine" />
</p>

Each coding CLI is brilliant alone but works in isolation — one terminal, one context, no colleagues:

- **Claude Code** — plans the deepest
- **Codex** — grinds long jobs without wobbling
- **Grok** — answers fastest
- **Kimi** — bulk work on a tiny bill
- **DSH** — plugin-native DeepSeek Harness sessions with cold resume
- **Pi** — one CLI over many providers (`anthropic/…`, `openai/…`), on your own machine

ccteam is the connective tissue they lack — identity, routing, delivery guarantees, guardrails, a cost ledger — and leaves *how* the team organizes itself to prompts you version.

<p align="center">
  <img src="docs/images/web-team-topology.png" width="1000" alt="Team page — live delegation topology: 50 live sessions across claude, codex, grok and kimi; who delegated whom, each session's model and reasoning effort, and the running cost ledger" />
  <br/>
  <sub><b>An afternoon on the Team page</b> — 50 live sessions across four vendors, every delegation a traceable parent→child edge, every dollar on the ledger.</sub>
</p>

## Usage

**1 · Remote control from Telegram / Lark**

Paste a bot token once (Settings → Access) and the chat becomes a full console — completion notifications, HITL `[approve] [deny]` buttons, and shipped files all land in the same thread. Dispatch at midnight, close the laptop, find the result at breakfast:

```text
/cd demo                        # pick a project; your next message talks to it
/new codex effort=high          # more sessions: /new [vendor] [role] [model=…] [effort=…]
@s2 run the test suite          # address any session directly
/status  /sessions  /stop s3    # health · fleet · cost · stop
/inbox +30m remind me …         # schedule a one-shot user turn; /inbox lists · cancel dN
```

<p align="center">
  <img src="docs/images/telegram-console.png" width="640" alt="Telegram as a full console — /projects to switch project, /use to pick a session, /status showing the session's model, context, usage and its working/idle children" />
  <br/>
  <sub><b>Telegram is the whole console</b> — switch projects, address any session, and one <code>/status</code> card shows the brain plus every delegate it hired.</sub>
</p>

**2 · Remote control from the web console**

The installer runs the daemon; `ccteam status` reprints your link (`http://<lan-ip>:7331/?token=…`) — open it from any device on your LAN. It's a chat shell, not a dashboard:

<p align="center">
  <img src="docs/images/web-launcher.png" width="1000" alt="The web launcher — pick a project, host, role, vendor and model in one pill, type, and the session is born on your first message; six formation playbooks below prefill a vendor lineup" />
  <br/>
  <sub><b>No “create session” form</b> — pick project · host · vendor · model in one pill and just type; the formation playbooks below prefill a whole lineup.</sub>
</p>

- six formation playbooks (commander & crews, driver & advisor, cross review, bake-off, research triangulation, cost pyramid) that prefill the launcher with a vendor lineup
- a Chat tab per session (plus a byte-faithful terminal where applicable), including a clock on the composer to queue delayed user turns above the input
- a Team page: the live delegation topology — vendor, the model and reasoning effort each session is actually running, cost, every row a real link so a parent and its delegate open side by side — plus a division-of-labor charter (the per-project `routing.md` agents read via `status`) edited in place
- a DSH page that opens DeepSeek Harness Web inside ccteam: the daemon authenticates the request, starts or attaches the right local DSH web instance, and gives each logged-in user a separate DSH home
- a cost pill with daily budget caps
- a per-project ⋯ menu in the sidebar: start a session there, copy its path, or take the project out of ccteam (deregister + stop its live sessions — your directory and code are never touched)
- marketplace and settings

Everything the console does is also `/api/v1` (OpenAPI at `/api/docs`).

**3 · Orchestrate a team from inside a claude session**

Any registered session can hire the others — say it in plain language and `session_spawn` / `dispatch` / `collect` run under the hood (with an honest `working` / `idle` signal, so nobody guesses from silence):

```text
Spawn a codex session, have it implement RFC-12 and run the tests; report back when green.

Plan this refactor, then delegate: codex implements, grok profiles the hot path in
parallel, kimi sweeps the rename across the repo. Collect everything into one summary.

Spawn a claude reviewer on s2's diff — I'm not merging until it signs off.
```

**4 · Many machines, one console**

Register a satellite with a join token (Settings → Access) — it dials out to your daemon, so a laptop behind NAT works fine. Projects are bound to a host and run where they live: spawn into the GPU-box project and its tests run on the GPU box, while transcripts, cost, and the team view stay in one console. Switching machines is just switching projects.

> Satellite execution currently runs Claude sessions; the other vendors run on the daemon's machine.

---

Under all four modes are the same **eight MCP tools**, available to every session, to your plain hand-started CLIs once registered, and to **any external agent** that presents an enrollment credential over `POST /mcp` — one credential per vendor config or per copy-button, and the daemon issues each *process* its own identity when it connects, so two agents sharing a config are still two callers with their own ledger rows and their own children:

```text
session_spawn · session_dispatch · session_collect · session_list · session_stop
status (+ its discovery alias grok_claude_codex_kimi) · chat_send_file
```

The daemon routes and records — at-least-once notifications across restarts, idempotency keys, a child's turn written to disk before its parent is told, guardrails that refuse runaway fan-out with a reason. When a web-driven session finishes autonomous work while nobody is watching the console, the final answer is mirrored to your IM; the IM `/status` card shows your session's working children at a glance. It never schedules; *when* to delegate lives in prompts you version.

- Plain-language walkthrough → [orchestration guide](docs/orchestration.md)
- Every command → manual ([English](docs/usage.md) · [中文](docs/usage-cn.md))

## Install

Runs on **macOS**, **Linux**, and **Windows (via WSL)**.

> [!IMPORTANT]
> **Bring your own coding CLI — install and authenticate at least one before you start.** ccteam is the bridge, not the agent: it spawns the vendor CLIs already on the machine a project is bound to, so a vendor that is missing (or installed but not authenticated) cannot host a session.
>
> - **Claude Code** — install [Claude Code](https://docs.claude.com/en/docs/claude-code), then `claude auth login`
> - **Codex** — install [Codex CLI](https://github.com/openai/codex), then `codex login`
> - **Grok Build** — install [Grok CLI](https://docs.x.ai/build/overview), then `grok login`
> - **OpenCode** — install [OpenCode](https://opencode.ai), then `opencode auth login`
> - **Kimi Code** — install [Kimi Code](https://moonshotai.github.io/kimi-code/), then `kimi login`
> - **DSH** — install [DeepSeek Harness](https://www.npmjs.com/package/@deepseek-ai/dsh) with `npm i -g @deepseek-ai/dsh`. Managed DSH sessions can use `DEEPSEEK_API_KEY`; DSH Web can also be configured from its native Settings → Models page.
> - **Pi** — install [Pi](https://pi.dev/), then set your provider key and check it with `pi auth check --provider <provider>`
>
> Any one of them is enough to start. Afterwards `ccteam status` and **Settings → Hosts** report, per machine, which vendors are installed, their versions, and whether each is actually authenticated — sitting on `PATH` never counts as logged in.

**1 · One-click script**

```bash
curl -sSL https://raw.githubusercontent.com/firstintent/ccteam/main/install.sh | sh
```

One static binary into `~/.local/bin`, no sudo. Every install mode — the script, `make install`, and `ccteam update` — resolves the destination through the same ladder (`CCTEAM_INSTALL_DIR` → wherever `ccteam` already lives → `~/.local/bin`), so an upgrade replaces the copy you are actually running instead of leaving a second one to shadow it.

**2 · Let an agent do it** — paste into any agent you already have:

> Install https://github.com/firstintent/ccteam — follow `INSTALL.md` in the repo.

**3 · From source** (Rust + Node):

```bash
git clone https://github.com/firstintent/ccteam && cd ccteam && make install
```

**Start it** — `ccteam daemon start` runs ccteam in the background and keeps it running after you close the terminal (`make install` already did this for you). Manage it any time:

```bash
ccteam daemon start          # start in the background; prints your web console link
ccteam daemon status         # is it running, and on which version?
ccteam daemon restart        # restart it
ccteam daemon stop           # stop it (your sessions come back next time you start)
ccteam daemon logs -f        # watch the logs live
```

After you reboot your computer, run `ccteam daemon start` again to bring ccteam back.

**Configure in the browser** — open the printed link (also shown by `ccteam status`), create a project, and just type; the session is born on your first message. Then:

- **Settings → Access** — everything that connects to ccteam, on one page: the copy-paste MCP config for external agents (a credential scoped to one project, rendered as the real config each vendor expects, or as plain text for plugin-backed flows such as DSH, listed and revocable afterwards — the secret is shown once, never again), satellite join tokens for new machines, your own Telegram/Lark bot (a numbered two-step card per platform — save the credential, then bind who the bot answers, with sender capture starting on its own), and per-user login links
- **Settings → Hosts** — each machine's vendor panel (installed / version / readiness) and one-click registration of the ccteam MCP tools into the vendor CLIs with writable config (Claude Code, Codex, Grok, OpenCode, Kimi), so even hand-started sessions can hire the team. DSH uses its own plugin plus a pasted Access credential; Pi gets the team tools through a ccteam-owned bridge loaded into the sessions ccteam spawns, so a `pi` you start by hand in a shell is left completely untouched
- **Workflow → Marketplace** — install skills (into your user-level library `~/.ccteam/skills`; the skills tab comes first) and personas (into the project), checksum-verified; attach library skills to any message from the composer
- **DSH** — open native DSH Web as a first-class console page. The owner sees the real `~/.dsh` space (ccteam attaches to a DSH Web already on `127.0.0.1:3080` when present); each regular user gets an isolated `$CCTEAM_HOME/runtime/dsh/web/<user>/` space with the ccteam client plugin preloaded. User-installed DSH plugins are preserved.

<p align="center">
  <img src="docs/images/web-workflow-hub.png" width="1000" alt="Workflow hub — skills, roles, marketplace, MCP servers, and the per-project experience ledger: turn records with role and skill fingerprints" />
  <br/>
  <sub><b>The workflow hub</b> — skills, personas, marketplace and MCP servers in one place, next to the project's experience ledger (turn records + role/skill fingerprints).</sub>
</p>

> The console binds to `0.0.0.0:7331` with token auth, no TLS — keep it on a trusted LAN. DSH Web uses a companion listener on the web port + 1 by default; override it with `--dsh-web-bind <addr:port>` or disable it with `--dsh-web-bind off`. If you put HTTPS in front of ccteam, proxy the companion listener too (usually a second HTTPS port or subdomain). Proxying only `:7331` makes the DSH iframe mixed-content fail, and DSH Web cannot be safely mounted under a path prefix.

> DSH Web honesty: native DSH turns run inside DSH, not as ccteam sessions, so they do not appear in the ccteam cost ledger; work delegated through the ccteam DSH plugin is ledgered normally. Tenant DSH Web is same-OS-user isolation: DSH agents can run shell commands, and self-installed DSH plugins are arbitrary npm code with the same trust level as that user account.

## Chaining sessions

Delegation is explicit — an agent (or you) says who does what, and the bridge handles identity, routing, delivery, and the ledger:

```text
session_spawn{vendor:"codex", title:"impl",  task:"implement RFC-12, run tests, report"}
session_spawn{vendor:"grok",  title:"probe", task:"profile the hot path", wait_seconds:120}
session_spawn{vendor:"kimi",  title:"chore", task:"apply the rename across every module"}
```

Async by default: the completion notification lands in the parent's chat like a colleague reporting back. `wait_seconds` is for sub-minute answers you need inline.

**Common workflows:**

- **Plan → build → gate** — claude decomposes and sets constraints; codex implements; a rival model reviews the diff before you merge.
- **Grind + probe** — codex holds the long job while grok answers the quick question before codex finishes a step.
- **Bulk on a budget** — fan the repetitive 80% out to kimi; keep the judgment calls on claude.

Who gets what starts from facts, not guesses: one `status` call is the roster — vendors installed, authenticated, and in-budget on the project's host, each one's models and reasoning-effort levels as it last declared them, and your routing notes (`<project>/.ccteam/routing.md` over the global fallback).

Every spawn surface takes `model` and `effort` for every vendor and forwards both verbatim — the vendor owns the verdict on its own values, so a level it refuses comes back as a real error instead of a session quietly running at the default. Omit them and the vendor's own defaults hold. The ladders differ (claude `low…max`, codex `low…xhigh`, grok `low|medium|high`, kimi `low|high|max`, and pi's is per *model* — it declares which levels the chosen model actually supports), so ask rather than guess: `status` for agents, `GET /api/v1/models` for programs, and the web composer's menus render from the same source.

## Project context

ccteam adds a team to your repo without taking it over:

- **Roleless by default** — the brain reads *your* `CLAUDE.md` / `AGENTS.md` through the vendor's own mechanism; ccteam never rewrites project knowledge.
- **Small footprint** — exactly `.ccteam/` (state), `.claude/agents/` (personas you install), and ccteam's own section of `.claude/settings.local.json` — never your `settings.json`.
- **Durable sessions** — ids (`s1`, `s2`, …) survive daemon restarts and cold-resume from disk; state is plain files in your repo.

## Extras

- **Marketplace** — personas install from [ccteam-hub](https://github.com/firstintent/ccteam-hub) into your project's `.claude/agents/`; skills install into the user-level global library `~/.ccteam/skills` (nested ids, whole-repo sources via `ccteam skill source add`), then attach to sessions per message — the library never links or copies into a project, while project-own skills live in `.agents/skills/` as normal git-visible files (`ccteam skill ensure-project`). Everything is fetched from pinned upstreams, sha256-verified, copied verbatim, never executed. Vendor-native Claude Code plugins are delegated to Claude Code itself (ccteam only flips the two settings keys).
- **HITL approvals** — spawn a session in approval mode and its permission requests reach your IM as `[approve] [deny]` buttons, through the vendor's native gate; deny blocks the tool call without killing the turn.

## Why

Seven excellent coding CLIs shipped in two years, and each assumes it's alone. The result: you, alt-tabbing between vendors, re-pasting context, playing message bus. The fix isn't a framework on top — the vendors' harnesses are already great. It's the connective tissue they lack: identity, routing, delivery, cost, observability, across vendors and machines. That's ccteam — `cc` for the Claude Code it grew out of, `team` for what your agents become.

It stays deliberately underneath:

- **No prompt injection** — personas load through the vendor's native mechanism; task text is forwarded verbatim.
- **No terminal scraping** — state comes from transcripts and structured events.
- **Measurements, never placeholders** — a context reading you see was really reported by that vendor and survives restarts; one it has not reported yet reads as unknown, not `0%`.
- **Local first** — `~/.ccteam` and your repos; no cloud in the loop.
- **Budgets guard, never kill** — daily per-vendor caps are the only automatic brake.

## Update

```bash
ccteam update                # update in place; restarts the daemon onto the new binary
```

`ccteam status` shows your version and flags a newer release. (Details: [usage](docs/usage.md#updating).)

## Uninstall

```bash
curl -sSL https://raw.githubusercontent.com/firstintent/ccteam/main/install.sh | sh -s -- --uninstall
rm -rf ~/.ccteam        # state, secrets, hub cache — keep it if you may return
```

Per project, delete `.ccteam/` and ccteam's section of `.claude/settings.local.json`.

## Support

- Questions, bugs, ideas → [issues](https://github.com/firstintent/ccteam/issues); PRs welcome.
- Telegram: [@cryptorobsu](https://t.me/cryptorobsu)
- If the team saved you an alt-tab, a star keeps the juggler juggling.

## License

MIT — see [LICENSE](LICENSE). Built on **Claude Code**, driving **Codex**, **Grok**, **OpenCode**, **Kimi**, **DSH** and **Pi**.
