<div align="center">
  <img src="assets/logo.svg" width="132" alt="ccteam mascot — a juggler bot keeping codex, grok and kimi in the air" />
  <h1>ccteam</h1>
  <p><b>ccteam turns the coding agents you already run (Claude Code, Codex, Grok, Kimi…) into one team —<br/>any session can spawn, dispatch, and collect work from any vendor on any machine,<br/>while you steer it all from Telegram, Lark, or a browser tab.</b></p>
  <p>
    <a href="https://github.com/firstintent/ccteam/actions/workflows/check.yml"><img src="https://github.com/firstintent/ccteam/actions/workflows/check.yml/badge.svg" alt="CI" /></a>
    <img src="https://img.shields.io/badge/made%20with-Rust-b7410e" alt="Made with Rust" />
    <img src="https://img.shields.io/badge/platform-Linux%20%C2%B7%20macOS-4c8dae" alt="Linux · macOS" />
    <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue" alt="MIT" /></a>
  </p>
</div>

<p align="center">
  <img src="assets/orchestration.svg" width="1000" alt="you, from any device, drive a claude brain that spawns and dispatches codex, grok and kimi on their strengths — each on its own machine" />
</p>

Claude Code plans deepest, Codex grinds long jobs without wobbling, Grok answers fastest, Kimi does bulk work on a tiny bill. Alone, each is one terminal with one context and no colleagues. ccteam bridges them with what a team needs underneath — identity, routing, delivery guarantees, guardrails, a cost ledger — and leaves how the team organizes itself to prompts you version.

## Usage

**1 · Remote control from Telegram / Lark**

Paste a bot token once (Settings → IM) and the chat becomes a full console — completion notifications, HITL `[approve] [deny]` buttons and shipped files land in the same thread. Dispatch at midnight, close the laptop; the result is waiting at breakfast:

```text
/cd demo                        # pick a project; your next message talks to it
/new codex                      # more sessions: /new [vendor] [role]
@s2 run the test suite          # address any session directly
/status  /sessions  /stop s3    # health · fleet · cost · stop
```

**2 · Remote control from the web console**

The installer already runs the daemon — `ccteam status` reprints your link, `http://<lan-ip>:7331/?token=…`; open it from any device on your LAN. It's a chat shell, not a dashboard: quick-start templates aimed at each vendor's strength, a Chat tab per session (plus a byte-faithful terminal where applicable), the live delegation tree, a cost pill with daily budget caps, marketplace and settings. Everything the console does is also `/api/v1` (OpenAPI at `/api/docs`).

**3 · Orchestrate a team from inside a claude session**

Any registered session can hire the others — say it in plain language, and `session_spawn` / `dispatch` / `collect` run under the hood (with an honest `working`/`idle` signal, so nobody guesses from silence). Prompts that just work:

```text
Spawn a codex session, have it implement RFC-12 and run the tests; report back when green.

Plan this refactor, then delegate: codex implements, grok profiles the hot path in
parallel, kimi sweeps the rename across the repo. Collect everything into one summary.

Spawn a claude reviewer on s2's diff — I'm not merging until it signs off.
```

**4 · Many machines, one console**

Register a satellite with a join token (Settings → Hosts); it dials out to your daemon, so a laptop behind NAT is a perfectly good satellite. Projects are bound to a host and sessions run where their project lives — spawn into the GPU-box project and the tests run on the GPU box, while transcripts, cost and the team view stay in the one console. Switching machines is just switching projects. (Satellite execution currently runs Claude sessions; the other vendors run on the daemon's machine.)

Underneath, every one of these is the same eight MCP tools — `session_spawn` / `session_dispatch` / `session_collect` / `session_list` / `session_stop`, plus `status`, `chat_send_file`, `screenshot` — available to every session, and to your plain hand-started CLIs once registered. The daemon routes and records (at-least-once notifications across restarts, idempotency keys, a child's turn on disk before its parent is told, guardrails that refuse runaway fan-out with a reason); it never schedules — *when* to delegate lives in prompts you version. The [orchestration guide](docs/orchestration.md) is the plain-language walkthrough; the manual has every command ([English](docs/usage.md) · [中文](docs/usage-cn.md)).

## Install

**1 · Let an agent do it**

Paste into any agent you already have:

> Install https://github.com/firstintent/ccteam — follow `INSTALL.md` in the repo.

**2 · From source**

```bash
git clone https://github.com/firstintent/ccteam && cd ccteam && make install   # Rust + Node
```

**3 · One-click script**

```bash
curl -sSL https://raw.githubusercontent.com/firstintent/ccteam/main/install.sh | sh
```

One static binary into `~/.local/bin`, no sudo; the daemon starts and prints your web console link (`http://<lan-ip>:7331/?token=…`). `ccteam config` registers the MCP tools into the vendor CLIs you have — Claude Code, Codex, Grok, OpenCode, Kimi — so even your everyday, hand-started sessions can hire the team.

**Configure in the browser**

Create a project and just type — the session is born on your first message. Settings → IM pastes a Telegram/Lark bot token (chat id captured automatically); Settings → Hosts registers MCP into your CLIs and mints join tokens for new machines; the marketplace installs personas and skills, checksum-verified.

> The console binds to `0.0.0.0:7331` with token auth, no TLS — keep it on a trusted LAN, or use `ccteam start --web-bind 127.0.0.1:7331`.

## Chaining sessions

Delegation is explicit — an agent (or you) says who does what, and the bridge does identity, routing, delivery, and the ledger:

```text
session_spawn{vendor:"codex", title:"impl",  task:"implement RFC-12, run tests, report"}
session_spawn{vendor:"grok",  title:"probe", task:"profile the hot path", wait_seconds:120}
session_spawn{vendor:"kimi",  title:"chore", task:"apply the rename across every module"}
```

Async by default: the completion notification lands in the parent's chat like a colleague reporting back. `wait_seconds` is for sub-minute answers you need inline.

**Common workflows:**

- **Plan → build → gate** — claude decomposes and sets constraints; codex implements; claude reviews the diff before you merge. A rival model gates the merge.
- **Grind + probe** — codex holds the long job while grok answers the quick question before codex finishes a step.
- **Bulk on a budget** — fan the repetitive 80% out to kimi; keep the judgment calls on claude.

Picking who gets what starts from facts, not guesses: one `status` call is the roster — which vendors are installed, authenticated and in-budget on the project's host, an advisory model catalog, and your selected routing notes (`<project>/.ccteam/routing.md` over the global fallback) carried verbatim. Omit `model` and each spawn simply rides the vendor's default.

## Project context

ccteam adds a team to your repo without taking it over. Sessions are roleless by default: the brain reads *your* `CLAUDE.md` / `AGENTS.md` through the vendor's own mechanism — project knowledge stays vendor-native, and ccteam never rewrites it. The footprint is exactly `.ccteam/` (state), `.claude/agents/` (personas you choose to install), and ccteam's own section of `.claude/settings.local.json` — never your `settings.json`. Sessions have durable ids (`s1`, `s2`, …) that survive daemon restarts and cold-resume from disk; state is plain files in your repo.

## Extras

**Marketplace**

Personas and skills install from [ccteam-hub](https://github.com/firstintent/ccteam-hub) into your project's `.claude/` — fetched from pinned upstreams, sha256-verified, copied verbatim, never executed. Vendor-native Claude Code plugins are delegated to Claude Code itself: ccteam only flips the two settings keys.

**HITL approvals**

Spawn a session in approval mode and its permission requests come to your IM as `[approve] [deny]` buttons — through the vendor's native gate, deny blocks the tool call without killing the turn.

## Why

Five excellent coding CLIs shipped in two years, and each one assumes it's alone: one terminal, one context, no colleagues. The result is you, alt-tabbing between vendors, re-pasting context, playing message bus. The fix isn't a framework on top — the vendors' own harnesses are already great and improving weekly. The fix is the connective tissue they all lack: identity, routing, delivery guarantees, cost, observability, across vendors and machines. That's ccteam — `cc` for the Claude Code it grew out of, `team` for what your agents become.

It stays deliberately underneath:

- **No prompt injection** — personas load through the vendor's native mechanism; task text is forwarded verbatim.
- **No terminal scraping** — state comes from transcripts and structured events.
- **Local first** — `~/.ccteam` and your repos; no cloud in the loop.
- **Budgets guard, never kill** — daily per-vendor caps are the only automatic brake.

## Uninstall

```bash
curl -sSL https://raw.githubusercontent.com/firstintent/ccteam/main/install.sh | sh -s -- --uninstall
rm -rf ~/.ccteam        # state, secrets, hub cache — keep it if you may return
```

Per project, delete `.ccteam/` and ccteam's section of `.claude/settings.local.json`.

## Support

- Questions, bugs, ideas → [issues](https://github.com/firstintent/ccteam/issues); PRs welcome.
- If the team saved you an alt-tab, a star keeps the juggler juggling.

## License

MIT — see [LICENSE](LICENSE). Built on **Claude Code**, driving **Codex**, **Grok**, **OpenCode** and **Kimi**.
