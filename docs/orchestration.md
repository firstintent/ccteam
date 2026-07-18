# Use your AI team — the plain-language guide

> 中文版: [orchestration-cn.md](orchestration-cn.md)

**You don't memorize tool names.** You say "use cct-codex to refactor this page" and a skill hands the work to a Codex session, supervises it to completion, and brings back a short report — status, files changed, tests pass/fail — for you to review. The work keeps running after you close your laptop, and every hop is on the ledger.

This is Claude Code's Task tool, except the "subagent" is a full vendor session — Codex, Grok, another Claude — possibly on another machine, and everything it does is recorded and inspectable.

---

## 1. Three ways in

| Where you are | How you use the team |
|---|---|
| **Phone / IM** (Telegram, Lark) | just message your session; `/compare why is this code slow` asks three models at once. Install the `team-brain` persona (marketplace) and one session becomes your chief of staff |
| **Web console** | open sessions in the browser, watch the team tree, review diffs, track cost |
| **Inside your coding agent** — Claude, Codex, Grok, OpenCode or Kimi (this guide's focus) | delegate with one sentence, via **skills like cct-codex / cct-grok** that drive the tools for you |

The full manual for the human surfaces is [usage.md](usage.md). This guide is about the third row — **commanding a whole team from inside your everyday AI session.**

## 2. The mental model (30 seconds)

Think of a small team where you are the lead:

- **You** = the lead. You say what you want, review results, decide what ships.
- **Codex** = the colleague who grinds through long work. Multi-file implementation, migrations, test-fixing, mechanical slogs.
- **Grok** = quick answers / second opinions. "Where's the bottleneck", "which of these three is right" — minute-scale answers (needs the grok CLI on that machine).
- **Claude** = the deepest reasoner. Decomposition, verdicts, the review gate before a merge.

Each colleague is a **session** with a durable id (`s47`). A session runs on whatever machine its **project** is bound to (local or a satellite). Close your laptop and it keeps working; what it spent and what it changed is all on your daemon's ledger.

**One iron rule:** when you want to "call another agent", **never** shell out to `codex exec` / `claude -p` yourself. That run has no session id, no cost accounting, no completion signal, and is invisible in the team view. If it's worth delegating, it's worth being on the ledger — let the skill go through the proper channel.

## 3. The phrases you say

The skills hide the tool calls. You say the left column; the right column happens:

| You say | What happens |
|---|---|
| "**use cct-codex** to implement / refactor / fix X" | a codex session grinds in the background; when done you get a **short report** (STATUS / files changed / test results) and review the diff yourself with `git diff` |
| "**use cct-grok** for a quick look at X", "ask grok …" | a grok session spins up, waits a minute or two inline, pastes the answer back |
| "have **claude review** whether this diff can merge" | a cross-model review gate: a different model reads the diff and returns MERGE / BLOCK |
| "**what sessions** are running?" | the team tree: who reports to whom, busy or idle, cost so far |
| "**stop s47**" | explicitly closes that session (state stays on disk, resumable later) |

**cct-codex** is for long work (background + poll); **cct-grok** is for quick Q&A (wait inline). Once installed (§6), you just say these things in your everyday Claude session.

## 4. Making delegation pay (best practices, in plain language)

These turn "it works" into "it's good". The skills encode them, but you should know them too:

1. **Brief clearly, and demand a short report with no code dumps.** The single biggest lever. One line — "reply in ≤25 lines: STATUS / files changed / test results / open questions, no diffs" — makes the reply ten times denser; otherwise a screenful of logs floods **your own** context.
2. **Long work runs in the background; quick answers wait inline.** Implementation goes to codex async (it reports back like a colleague); only minute-scale answers you need for your next sentence are worth an inline grok wait.
3. **Review the diff yourself; don't have it read aloud.** The colleague reports *which files and why*; you read the code with `git diff`.
4. **Gate merges with a different model.** Codex implements; before merging, have a Claude or Grok session review the same diff — cross-vendor review catches what same-model review rubber-stamps.
5. **Run where the environment is.** GPU tests live on the Linux box? Join it as a satellite, register the repo there, and delegate into *that project* — the work runs on that machine automatically.
6. **Set the limits once, then trust them.** Delegation depth, fan-out, and daily budgets are guardrails the daemon enforces with a stated reason. Configure once, then delegate without worrying.
7. **One task per dispatch.** Three asks in one message = one muddled report you must untangle; three dispatches = three clean checkpoints.

## 5. A real example (how a real feature shipped)

The lead says: "merge the settings 'Hosts' and 'Status' pages into one adaptive page."

1. A **codex session `s47`** starts on it in the background (async).
2. Minutes later it reports: changed `SettingsView / App / CSS / i18n` + tests, **Vitest 379 green, build passes**, and notes it "also fixed 3 pre-existing lint errors".
3. The lead (an orchestrating Claude) **runs `git diff` itself**: merge is clean, the 3 lint fixes were already red in the repo and the changes are safe.
4. It starts a **claude session `s49`** as a cross-model reviewer, waits a minute inline, gets the verdict: **MERGE, no blockers**.
5. Done. `s49` is stopped; `s47` stays around for follow-ups.

**The lead said two sentences in total.** Two sessions from different vendors did the work and reviewed each other, every hop on the ledger and in the team view.

## 6. Install once

`cct-codex` and `cct-grok` are first-party recipes on the **marketplace** — install them into a project from the web marketplace page (they land in `.claude/skills/`, so ccteam-managed sessions get them too), or keep personal copies user-level in `~/.claude/skills/` so every session of yours can use them. Prerequisites:

- `ccteam start` is running on this machine; `ccteam config mcp` has registered ccteam with **all four vendors** — Claude, Codex, Grok, OpenCode (one-time; any vendor's plain session can orchestrate).
- You're inside a **registered ccteam project** directory (the skill resolves the project from the working directory).
- You're in a **plain vendor CLI session** — it reads the global config and gets the ccteam tools. (Some SDK-driven sessions don't load user-scope MCP config; see §7.)

Verify in 60 seconds:

```bash
ccteam doctor --verify-mcp       # 8 tools, 0 stubs — drift exits 1
claude mcp list                  # server `ccteam` — ✔ Connected
grok mcp doctor                  # the Grok axis: handshake OK, 8 tools discovered
```

## 7. When something's off

| Symptom | What it is → what to do |
|---|---|
| "tool not available / no such tool" | this session didn't load ccteam. Use a plain vendor CLI session; check `ccteam status`. SDK sessions can fall back to `POST http://localhost:7331/mcp` with `Authorization: Bearer ccteam:<hex>` (hex = `~/.ccteam/secrets/web-token`) — same tools, admin identity (spawns are roots). |
| "it's been silent forever" | it's **working**, not stuck. Go do something else and come back for the report. |
| "project not found" | you're not in a registered project directory. `cd` into one, or have the skill pass the project name. |
| "grok doesn't work" | that machine doesn't have the grok CLI. `ccteam status` / capabilities shows which vendors this machine actually has. |
| "did the delegation double-fire?" | the skills set an idempotency key; a timeout-retry never creates a duplicate. |

---

## Appendix: tool reference (for skill authors / manual orchestration)

You normally never touch these — the skills do. But if you're **writing a skill** or orchestrating by hand, ccteam exposes eight tools under the `ccteam` MCP server, visible in Claude as `mcp__ccteam__<name>`:

- **`session_spawn`** — hire a colleague (and hand over the first task in the same call). `{vendor, title, task?, wait_seconds?, notify?, idempotency_key?, role?, model?, effort?, protocol?, permission_mode?, project?}`. `vendor` = `claude` (default) / `codex` / `grok` / `opencode` / `kimi`; grok/opencode/kimi force `protocol:"acp"`. `role` names a `.claude/agents/<role>.md` persona — omit for roleless (the bare vendor reads the project's own `CLAUDE.md`/`AGENTS.md`, the right default more often than not). `title` ≤80 chars, ledger/team-view label only — never enters any prompt. `permission_mode:"hitl"` pops approve/deny to the bound IM chat. **There is no `host` parameter** — the execution machine is inherited from the project's binding; passing one is a hard error. `wait_seconds>0` waits inline for the first answer; default is async. Always returns a **new** `sid`; the response's `caller` names the authenticated spawner — `ambient:<sid>` (a ccteam session called it; it becomes the child's `parent_sid`) or `admin` (the owner front door / main-session fallback — always a **root** spawn, `parent_sid: null`). If you expected a parent edge and see `caller: "admin"`, your call rode an admin-authenticated MCP server instead of your session's own bearer.
- **`session_dispatch`** — send another task to an existing session (`{sid, task, wait_seconds?, notify?, idempotency_key?}`). Forwarded verbatim as a user turn, zero injection. Async by default: when the child's **vendor turn completes and it goes idle** you get ONE notification that says so explicitly (a chatty child's mid-turn narration never notifies — it stays in the ledger). `notify` selects the mode: `"final"` (default) / `"all"` (every assistant message, debug firehose) / `"off"` (ledger-only; booleans still parse). The notification marks the child **idle/waiting** — if the task isn't actually done, that's your cue to dispatch the next step (the "silently stalled child" failure mode is gone: idle always signals). `wait_seconds` (≤600) blocks until the turn actually finishes and returns the FINAL `result_text` (interim narration never ends the wait), or `status:"pending"` on timeout — the child keeps running, never cancelled. Dispatching to yourself or an ancestor is rejected (cycle guard).
- **`session_collect`** — read a session's output without joining it (`{sid, tail?, n?, since?, max_chars?}`). Watch `activity`: `working` = mid-turn (poll again, pass `since:<last turn_id>` for the delta) / `idle` = turn done (read). Returns are bounded (`max_chars` default 10 000): long turns keep a 70 % head / 30 % tail excerpt with an explicit marker; the full text is always in the ledger. Also carries the accrued ledger: `cost_usd` (priced vendors) and `tokens_total` (raw token count — present for every vendor that reports usage, so codex/grok/opencode/kimi sessions are not blank).
- **`session_list`** — the delegation tree (who reports to whom, busy/idle, cost/tokens, `parent_sid`), most recently active first. Accepts `{project?, activity?, limit?}` filters (default cap 30 rows with an explicit `truncated`/`total`; null/empty fields are omitted) so a big fleet never floods your context. The web team view renders the same graph live.
- **`session_stop`** — explicitly stop one `sid` (state stays on disk, cold-resumable). ccteam has exactly two automatic brakes: the daily per-vendor budget cap refuses *new* work, and live-session capacity gracefully evicts the least-recently-active idle session — **creation never fails for capacity**.
- Plus **`status`** (daemon health + sessions + today's cost), **`chat_send_file`**, **`screenshot`**.

**Identity & trust (honestly):** a ccteam-spawned session carries a per-session `(sid, secret)` principal and can only act within its own project, with delegation guardrails (depth 2, fan-out 10 per parent, 50 delegated per project, cycle rejection, budgets) enforced by the daemon with a stated reason. Your own main session rides the same-user admin fallback and can manage the whole fleet; it is not itself a ccteam session, so completion notifications have nowhere to land — use `wait_seconds` for short tasks or poll `session_collect`, and pass `project:"<slug>"` when outside a registered repo. The per-session secret is **defense in depth under a single OS user, not a hard boundary** — same-uid processes can ultimately read each other's env. What it buys: agents can't *accidentally* act cross-project or as each other, and every action is attributed to an authenticated caller. Hard isolation (per-agent OS users / sandboxes) is deliberately out of scope for now.
