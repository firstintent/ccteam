# Use your AI team — the plain-language guide

> 中文版: [orchestration-cn.md](orchestration-cn.md)

**You don't memorize tool names — you just say it.** Tell your session "hand this refactor to codex and report back", and it hires a Codex session, supervises it to completion, and brings back a short report — status, files changed, tests pass/fail — for you to review. Once a session is connected, there is no extra install step: the ccteam MCP server ships its own instructions, so it already knows how to run the team. The work keeps running after you close your laptop, and every hop is on the ledger.

This is Claude Code's Task tool, except the "subagent" is a full vendor session — Codex, Grok, DSH, another Claude — possibly on another machine, and everything it does is recorded and inspectable.

---

## 1. Three ways in

| Where you are | How you use the team |
|---|---|
| **Phone / IM** (Telegram, Lark) | just message your session; say "ask codex and grok too" and it fans the question out to several vendors, then weighs the answers itself. Install the `team-brain` persona (marketplace) and one session becomes your chief of staff |
| **Web console** | open sessions in the browser, watch the team tree, review diffs, track cost |
| **Inside your coding agent** — Claude, Codex, Grok, OpenCode, Kimi, DSH or Pi (this guide's focus) | delegate with one sentence, in plain language — every MCP-connected session already knows the team tools |

The full manual for the human surfaces is [usage.md](usage.md). This guide is about the third row — **commanding a whole team from inside your everyday AI session.**

## 2. The mental model (30 seconds)

Think of a small team where you are the lead:

- **You** = the lead. You say what you want, review results, decide what ships.
- **Codex** = the colleague who grinds through long work. Multi-file implementation, migrations, test-fixing, mechanical slogs.
- **Grok** = quick answers / second opinions. "Where's the bottleneck", "which of these three is right" — minute-scale answers (needs the grok CLI on that machine).
- **Claude** = the deepest reasoner. Decomposition, verdicts, the review gate before a merge.

Each colleague is a **session** with a durable id (`s47`). A session runs on whatever machine its **project** is bound to (local or a satellite). Close your laptop and it keeps working; what it spent and what it changed is all on your daemon's ledger.

**One iron rule:** when you want to "call another agent", **never** shell out to `codex exec` / `claude -p` yourself. That run has no session id, no cost accounting, no completion signal, and is invisible in the team view. If it's worth delegating, it's worth being on the ledger — say it, and the session goes through the proper channel (the `agent` tool).

## 3. The phrases you say

Your session hides the tool calls. You say the left column; the right column happens:

| You say | What happens |
|---|---|
| "**hand codex** the RFC-12 implementation — work in the background, report back with a diff summary and test results" | a codex session grinds in the background; ONE notification when the task completes and the child goes idle, then you review the diff yourself with `git diff` |
| "**ask grok** for a quick second opinion on this stack trace — wait for the answer" | a grok session spins up, waits a minute or two inline, pastes the answer back |
| "put this design question to **codex and grok independently**, then give me consensus / disagreements / your verdict" | the fan-out compare: two sessions answer blind, your session weighs the evidence and rules |
| "before merge, have a **different vendor review** this diff — MERGE / BLOCK with reasons" | a cross-vendor review gate: the builder never rubber-stamps its own work |
| "**which vendors** are available here, and what do my routing notes say?" | one `status` call: which vendors this project's bound host can hire and what the team spent today — `status{detail:"routing"}` adds the selected project override / global fallback carried verbatim |
| "**what sessions** are running? what did that fan-out cost?" | one `agent_read` call: the roster — busy or idle, per-member model and cost; `tree:true` draws who reports to whom |
| "**stop s47**" | `agent_stop` explicitly closes that session (the transcript stays on disk and `agent_read` still reads it) |

Rule of thumb: **long work → background + completion notification** (close the laptop); **quick questions → wait inline**. Wire your session once (§8), then these phrases work as-is in your everyday session.

## 4. Making delegation pay (best practices, in plain language)

These turn "it works" into "it's good". They're one sentence each — fold them into how you phrase the ask:

1. **Brief clearly, and demand a short report with no code dumps.** The single biggest lever. One line — "reply in ≤25 lines: STATUS / files changed / test results / open questions, no diffs" — makes the reply ten times denser; otherwise a screenful of logs floods **your own** context.
2. **Long work runs in the background; quick answers wait inline.** Implementation goes to codex async (it reports back like a colleague); only minute-scale answers you need for your next sentence are worth an inline grok wait.
3. **Review the diff yourself; don't have it read aloud.** The colleague reports *which files and why*; you read the code with `git diff`.
4. **Gate merges with a different model.** Codex implements; before merging, have a Claude or Grok session review the same diff — cross-vendor review catches what same-model review rubber-stamps.
5. **Run where the environment is.** GPU tests live on the Linux box? Join it as a satellite, register the repo there, and delegate into *that project* — the work runs on that machine automatically.
6. **Set the limits once, then trust them.** Delegation depth, fan-out, and daily budgets are guardrails the daemon enforces with a stated reason. Configure once, then delegate without worrying.
7. **One task per dispatch.** Three asks in one message = one muddled report you must untangle; three dispatches = three clean checkpoints.
8. **Ask for the verdict on the FIRST line** (`VERDICT: ship` / `VERDICT: blocked — …`). The completion notification excerpts from the top of the answer, so a report that buries its conclusion under a preamble makes you open the transcript to learn what you already asked for.

## 5. A real example (how a real feature shipped)

The lead says: "merge the settings 'Hosts' and 'Status' pages into one adaptive page."

1. A **codex session `s47`** starts on it in the background (async).
2. Minutes later it reports: changed `SettingsView / App / CSS / i18n` + tests, **Vitest 379 green, build passes**, and notes it "also fixed 3 pre-existing lint errors".
3. The lead (an orchestrating Claude) **runs `git diff` itself**: merge is clean, the 3 lint fixes were already red in the repo and the changes are safe.
4. It starts a **claude session `s49`** as a cross-model reviewer, waits a minute inline, gets the verdict: **MERGE, no blockers**.
5. Done. `s49` is stopped; `s47` stays around for follow-ups.

**The lead said two sentences in total.** Two sessions from different vendors did the work and reviewed each other, every hop on the ledger and in the team view.

## 6. Model routing (who does what, without guessing)

Picking the right colleague for a task rests on three layers, kept deliberately separate:

- **Facts, probed.** A default `status` call answers the hiring question in a couple of hundred bytes: which vendors are installed on the host your project is bound to, what the team spent in the last 24 hours, and whether that host is offline or its snapshot stale. `status{detail:"vendors"}` opens the panel behind it — installed/version per vendor, an honest auth signal (being on PATH never masquerades as logged in, and an unknown state never blocks a hire), budget posture, and when it was observed. Remote hosts report over their satellite channel; an offline host shows its last snapshot marked `stale`, never the local machine's abilities in disguise.
- **Catalog, advisory.** Model ids, display names, and alias tiers from two sources kept separate and labeled: **runtime last-seen** (catalogs the adapters already capture, with an observed-at) and the hub **`models.json`** (community-maintained). `status{detail:"models"}` carries each vendor's **reasoning-effort ladder** alongside them — the levels that vendor itself declared, else ccteam's CLI-verified pinned set. The ladders genuinely differ (claude `low…max`, codex `low…xhigh`, grok `low|medium|high`, kimi `low|high|max`, opencode publishes no shared ladder at all, and pi's is per *model* — it declares which levels the model you picked actually supports), so read one rather than reusing another vendor's spelling. The catalog is a reference, never a spawn allowlist: `model`/`effort` pass through verbatim at spawn, a model absent from the catalog spawns all the same, and a stale catalog can at worst recommend something outdated — it blocks nothing. What it will *not* do is swallow your pick: name a model or an effort the vendor refuses and the spawn comes back as an error, never as a session quietly running at the default.
- **Opinions, your text.** Global routing lives in `~/.ccteam/routing.md` (the shared home initializer creates a neutral starter when missing and never overwrites it); an optional project override lives in `<project>/.ccteam/routing.md`. When the project file exists it replaces the global file completely—the two are not merged. Both are plain markdown with no schema. `status{detail:"routing"}` transports the selected file verbatim (source/sha/truncation noted) to whichever session asks—identical text for a planner on any vendor, on any host—and ccteam never parses or executes it.

For a remote project, routing remains main-daemon control-plane configuration: `<project>` means the daemon-side project data home recorded in the catalog. ccteam does not silently synchronize or read routing files from a satellite worktree.

**The workflow is one call, then hire.** Call `status` — add `detail:"models"` when you need ids and effort ladders, `detail:"routing"` for your own notes — then `agent` with explicit `vendor` / `model` / `effort` and the task in the same call. If you do aim at a vendor that isn't there, the hire fails fast with the list of what that host *does* have — failure is discovery too.

A `routing.md` looks like this — write only the exceptions:

```markdown
# Routing notes

Default: omit `model` — vendor defaults track their latest releases.

| Task type | Vendor / model / effort | Why |
|---|---|---|
| Long refactors, migrations | codex / sol-max / high | grinds without wobbling |
| Quick second opinion | grok / (vendor default) / low | minute-scale answers |
| Final review before merge | claude / opus / high | catches what the builder rubber-stamps |
```

**Comparing vendors is an in-session move,** not a separate product feature. To put a question to the team:

1. **Fan out** — one `agent` call per vendor with the same self-contained question, 2+ of them (async, one task each, `title` labels the matchup).
2. **Let each answer independently** — separate sessions, no cross-contamination.
3. **Collect at the turn boundary** — the completion notification fires as each child goes idle; `agent_read{sid}` picks up anything you're still missing (an absent or failed member is noted, never killed).
   Every completion notification is one header line — `s12 done · turn 7 · ctx 19%` (`⚠` from 85%; `s12 FAILED (<kind>) …` on failure) — followed by the answer excerpt (2000 characters by default, 500 under `notify:"brief"`, the full text always one `agent_read{sid,tail:true}` away), nothing else; and `agent_read` rows, transcripts and inline `agent` results carry `context_pct` as a number, so you can decide **reuse vs. fresh session** without another call: keep dispatching to a child while its context is comfortably low; once it nears the warning band, spawn a new one for the next task and let the old one idle. Mid-turn narration never wakes you at all — it stays in the child's ledger — so a chatty child costs you nothing extra. If a child outlasts an inline `agent{wait}`, `agent_read{sid,wait:240}` holds the read open until its turn ends instead (repeat with `since:<cursor>` to page forward); reading a child's `turns.jsonl` yourself is never necessary.
4. **Synthesize the verdict yourself** — consensus, disagreements, and your call. Optionally dispatch the collected answers back to one child for rebuttal, or spawn a third session as tie-breaker.

**The bill stays visible.** `agent_read` rows carry the accrued `cost_usd` / `tokens_total` per member, so a fan-out's cost is a sum you can read, not a surprise.

## 7. Formations (openings for a multi-vendor team)

Six openings ship as cards on the web console (Home, and Team → Charter) — each prefills the launcher with a vendor lineup; the plan itself is always yours, said in plain language:

- **Commander & crews** (总控-工班) — a strong-reasoning controller plans, decomposes and accepts; codex builds, grok scouts the ecosystem; completion notifications flow back to the controller. The expensive model pays only for decomposition and acceptance — volume work rides cheaper specialists.
- **Daily driver & advisor** (主力-顾问) — grok or codex drives the routine work; when it hits a wall, spawn an advisor session on the same repo, take the plan, let the driver execute, stop the advisor. The expensive model bills only for the hard minutes.
- **Cross review** (交叉互审) — one vendor writes, a different vendor reviews the diff cold, disagreements return to the controller. Different models make uncorrelated mistakes; the overlap catches what self-review rubber-stamps.
- **Bake-off** (并行竞标) — the same hard problem to 2–3 vendors in parallel; compare, keep the best, merge the good ideas. Worth it when the solution space is wide.
- **Research triangulation** (调研三角) — grok mines X and real-time chatter, claude does the deep web read, codex verifies against the source; one controller merges. No single harness has all three windows.
- **Cost pyramid** (金字塔用工) — kimi/opencode grind the mechanical bulk (renames, formatting, test triage); failures escalate to an expensive model. The ledger shows the savings per member.

Three more that need no card:

- **Overseer** (监工模式) — spawn risky-ops sessions with `permission_mode:"hitl"` (approvals pop to your IM) while bulk workers run skip. Risk gets a gate, volume keeps its speed.
- **Standing watch** (定时值守) — schedule messages on a session (composer clock / scheduled API): grok sweeps the ecosystem every morning, claude files a weekly repo-health note. The daemon only fires the schedule; the thinking happens in the session.
- **Many machines** (跨机编队) — bind heavy projects to a beefy satellite host; the topology wears host badges while transcripts and cost stay in one console.

## 8. Wire up once

For config-writable vendors, there is nothing to install for orchestration itself: `ccteam config mcp` (one-time) registers the ccteam server with Claude, Codex, Grok, OpenCode, and Kimi, and the server's own instructions teach any connected session the delegation flow. DSH is the vendor that works both ways: ccteam can hire DSH directly (`/new dsh` or `agent{vendor:"dsh", task:"…"}`) — the hire runs inside that identity's own DSH web runtime and shows up live in the DSH page's sidebar, plugin preloaded, and a DSH session you start from DSH's own web UI can become a delegation parent after `dsh plugin --profile web add @ccteam/ccteam-ui` plus the daemon URL and enrollment credential from Settings → Access. Its first tool call asks for a project slug if the session has not been bound yet. Pi is different: ccteam writes none of its config and instead loads its own bridge into the Pi sessions it spawns, so a managed Pi session can delegate while a `pi` you started by hand stays untouched. Want a standing orchestrator persona on top (routing habits and review gates baked in)? Install `team-brain` from the **marketplace** — a persona choice, not a prerequisite. What you do need:

- `ccteam start` is running on this machine.
- You have a **registered ccteam project** and know its slug; config-writable CLI sessions can also resolve it from their working directory.
- You're in a **plain vendor CLI session** for the config-writable vendors, or in DSH's web UI with `@ccteam/ccteam-ui` connected. (Some SDK-driven sessions don't load user-scope MCP config; see §9.)

Verify in 60 seconds:

```bash
ccteam doctor --verify-mcp       # 6 tools, 0 stubs — drift exits 1
claude mcp list                  # server `ccteam` — ✔ Connected
grok mcp doctor                  # the Grok axis: handshake OK, 6 tools discovered
```

## 9. When something's off

| Symptom | What it is → what to do |
|---|---|
| "tool not available / no such tool" | this session didn't load ccteam. Use a plain vendor CLI session; for DSH, install `@ccteam/ccteam-ui` and paste the Access credential in DSH Settings. SDK sessions can call `POST http://localhost:7331/mcp` directly with an enrollment credential (`Authorization: Bearer ccteam-enroll:<id>:<secret>`, minted under Settings → Access, plus the `Mcp-Session-Id` returned at `initialize`) — same tools, and the caller gets its own ledger row, so its spawns are its children rather than roots. |
| "it's been silent forever" | it's **working**, not stuck. Go do something else and come back for the report. |
| "project not found" | you're not in a registered project directory. `cd` into one, or say the project name so the session passes `project:"<slug>"`. |
| "grok doesn't work" | that machine doesn't have the grok CLI. `ccteam status` / capabilities shows which vendors this machine actually has. |
| "did the delegation double-fire?" | `agent` takes an `idempotency_key` — a retry with the same key replays the original call instead of doubling it (scoped per project for a hire, per child for a follow-up). Ask for one on flaky links, or check `agent_read` before retrying. |

---

## Appendix: tool reference

Moved to its own page: **[mcp.md](mcp.md)** — the complete reference for all six tools (`agent` · `agent_read` · `agent_stop` · `status` + its `grok_claude_codex_kimi` alias · `chat_send_file`): parameters, response shapes, the per-caller tool face, notification tiers, credential families, protocol details, and the delegation guardrails. You normally never need it — your session drives the tools from your plain-language ask — but skill authors and manual orchestrators read it once.
