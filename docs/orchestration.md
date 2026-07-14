# Agent orchestration — the deep-user guide

> 中文版: [orchestration-cn.md](orchestration-cn.md)

**For people who already live in Claude Code / Codex.** This is the Task tool, except the subagent is a full vendor session — any vendor, any machine — that survives you closing the laptop, and every hop is recorded.

ccteam exposes eight MCP tools under the `ccteam` server. In Claude Code they appear as `mcp__ccteam__session_spawn`, `mcp__ccteam__session_dispatch`, `mcp__ccteam__session_collect`, `mcp__ccteam__session_list`, `mcp__ccteam__session_stop`, plus `mcp__ccteam__status`, `mcp__ccteam__chat_send_file`, `mcp__ccteam__screenshot`. The five `session_*` tools are the orchestration surface; everything below is about using them well.

---

## 1. The mental model

```text
chat ⇄ project ⇄ session          project = (slug, host, path)
                 └─ s1, s2, …     sessions run wherever their project lives
```

- **session** — one vendor process with its own context, addressed by a durable id (`s1`, `s2`, …) that survives daemon restarts and is never reused. A session belongs to exactly one **project**.
- **project** — a registered working tree **bound to exactly one host** (`local` or a satellite). It is the unit of delegation guardrails, cost ceilings, access control — and now placement. The same repo checked out on two machines is **two projects** (slug equality across machines was abolished as an identity: it misled — same slug can be different projects, different slugs the same repo). Catalog a satellite checkout either by creating it from the web (pick the host) or by importing one already registered there.
- **host** — a property of the **project**, not of the spawn. Sessions inherit it: spawn into a project on `gpu02` and the child runs on `gpu02`. Transcript, cost, and the delegation ledger stay on your daemon regardless of where the process runs. There is no `host` parameter on `session_spawn` — passing one is a hard error.
- **The daemon routes; it never schedules.** No tick loop, no orchestrator. Whatever topology exists is the one your sessions (or you) built with these five tools.
- **Delegation is recorded, not injected.** A dispatched task is forwarded verbatim as a user turn. A completion notification is an ordinary turn delivered to the parent — the same shape as a human relaying a message. Nothing is ever written into another session's system prompt.

## 2. Which sessions can call the tools — and as whom

Two identities, both end at the same daemon gate:

| Caller | Identity | Scope |
|---|---|---|
| **ccteam-spawned session** | per-session `(sid, secret)` principal, minted at spawn, injected via the session's curated MCP config | its **own project** only; delegation guardrails apply (depth, fan-out, cycles, budget) |
| **Plain main session** — the `claude` / `codex` you launched yourself | same-user admin fallback: the stdio forwarder reads the admin web token (`~/.ccteam/secrets/web-token`, 0600) and the daemon verifies it | fleet-wide; `session_spawn` targets the project resolved from your **working directory** (or an explicit `project` arg); spawns are roots of new delegation trees |

Practical consequences for the main-session case:

- Run your session **inside a registered project directory** and `session_spawn{vendor:"codex", task:"…"}` just works — no setup beyond `ccteam config mcp` (once) and a running daemon.
- Your main session is not itself a ccteam session, so **completion notifications have nowhere to land**. Use `wait_seconds` for short tasks or poll `session_collect`; the spawned child is fully tracked either way.
- Outside any registered project, pass `project:"<slug>"` explicitly.

**Never shell out to `codex exec` / `claude -p` to "call another agent".** A raw CLI run has no sid, writes no `turns.jsonl`, accrues untracked cost, sends no notification, and is invisible to `session_list` and the team view. If it matters enough to delegate, it matters enough to be on the ledger.

## 3. Verify the surface (60 seconds)

```bash
ccteam doctor --verify-mcp       # 8 tools, 0 stubs — drift exits 1
claude mcp list                  # server `ccteam` — ✔ Connected
claude -p "list your tools containing 'ccteam'"   # names visible to a real session
```

If `claude mcp list` says Connected but a *particular* session has no `mcp__ccteam__*` tools:

- The session predates `ccteam config mcp` — MCP servers are read at session start; restart it.
- The session was spawned **by ccteam** with a curated `--strict-mcp-config` — it gets exactly one MCP entry (ccteam itself, with its principal). If that entry's HTTP endpoint can't be reached, the session has zero tools; check `ccteam status` and the daemon log.
- SDK-driven harnesses may not load user-scope `~/.claude.json` servers at all — orchestrate from a normal CLI session, or wire the server into the SDK config explicitly.

## 4. The five tools

Parameter lists below are exact; everything optional unless marked.

### `session_spawn` — hire a colleague (and hand over the first task)

```json
{"vendor":"codex", "title":"impl-rfc12",
 "task":"Implement RFC-12, run the test suite, report pass/fail with a diff summary."}
```

- `vendor`: `claude` (default) | `codex` | `grok` | `opencode`. `model`, `effort`: vendor-specific overrides. `protocol`: `stream-json` (default) or `acp` — grok/opencode force `acp`; `terminal` is never available to agents.
- Execution host is **inherited from the project** — there is no `host` parameter (passing one errors). To run remotely, spawn into a project bound to that satellite (create it from the web host picker, or import a checkout already registered there). Remote execution currently supports Claude stream-json sessions.
- `role`: a `.claude/agents/<role>.md` persona, loaded by the vendor's native mechanism. Omit for roleless — the bare vendor reads the project's own `CLAUDE.md`/`AGENTS.md`, which is the right default more often than not.
- `task` + `wait_seconds` + `notify`: spawn-and-dispatch in one call. Async by default: the child works, you get a completion-notification turn when it finishes (ccteam-session callers only). `wait_seconds:120` blocks inline and returns `result_text`. `notify:false` = ledger-only.
- `title`: ≤80 chars, ledger/team-view label only — never enters any prompt.
- `permission_mode`: `skip` (default) or `hitl` — tool calls pop approve/deny to the bound IM chat.
- `idempotency_key`: a retry with the same key replays the original spawn (same sid) instead of double-spawning — set it when your MCP client might time out and retry.
- Returns `{sid, vendor_session_id, host, …}`. Always a **new** sid.

### `session_dispatch` — send work to an existing session

```json
{"sid":"s7", "task":"Now rebase onto dev and re-run the failing suite only.", "wait_seconds":0}
```

Verbatim user turn, no injection. Async + notification by default; `wait_seconds` (≤600) blocks and returns `{status:"completed", result_text, cost_usd}` or `{status:"pending"}` on timeout — the child keeps running, never cancelled. Self/ancestor dispatch is rejected (cycle). Same `idempotency_key` semantics as spawn.

### `session_collect` — read output without joining the session

```json
{"sid":"s7", "tail":true, "n":3}
```

Tails the child's ccteam-owned transcript. Key fields: `activity` — `working` (mid-turn: poll, don't parse silence) / `idle` (turn done: read) / `stale` / `stuck`; `cost_usd`; `vendor_session_id` (native resume key). Incremental polling: pass `since:<turn_id you last saw>`. Final answer of a long run: `tail:true`. Default page is oldest-first, `n:20`.

Returns are **bounded**: `max_chars` (default 10 000, clamp 500–50 000) caps total returned content; longer turns keep a 70 % head / 30 % tail excerpt with an explicit marker and the full text stays in the ledger (page it with `since`/`n`). Waited spawn/dispatch `result_text` is capped at 10 000; the async completion-notification embed at 4 000 with a `session_collect` pointer. Your context is the scarce resource — the caps are the floor, your prompts are the ceiling (see §5).

### `session_list` — the delegation tree

Returns every live session (`sid`, `project`, `vendor`, `activity`, `waiting_approval`, `host`, `cost_usd`, `title`, `parent_sid`) plus a `tree` — roots and children. This is your fleet dashboard in tool form; the web team view renders the same graph live.

### `session_stop` — explicit, never proactive

Stops one sid. State stays on disk; the sid can cold-resume later. ccteam has exactly two automatic brakes, neither of which kills work mid-turn by preference: the daily per-vendor budget cap refuses *new* work, and the live-session capacity (`sessions.max_live`, default 50) gracefully stops the least-recently-active idle session to admit a new one — evicted sids stay resumable and the eviction is on the ledger (`session_evicted`). Creation never fails for capacity.

## 5. Patterns that earn their keep

**Route by strength, pay for depth once.** Let the deepest model decompose and verdict; ship the long grind to Codex and the quick probe to Grok:

```json
{"vendor":"codex", "title":"impl",  "task":"Implement RFC-12 per the constraints below… run tests, report."}
{"vendor":"grok",  "title":"probe", "task":"Profile the hot path in src/ingest; top 3 offenders.", "wait_seconds":120}
```

Async for the grind (the notification lands like a colleague reporting back), inline wait only for sub-minute answers you need before your next sentence.

**Gate merges with a rival model.** Codex implements; before merging, spawn a Claude reviewer *on the same project* and collect the verdict:

```json
{"vendor":"claude", "title":"review-rfc12",
 "task":"Review the diff on branch rfc12 for correctness and API-contract breaks. Verdict: MERGE or list blockers."}
```

Then `session_collect{sid, tail:true}` — the verdict without paging the whole transcript. Cross-vendor review catches what same-model review rubber-stamps.

**Run where the environment is.** GPU tests live on the Linux box: join it once as a satellite, `ccteam init` the repo there, import that checkout from the web hosts page (or create it there via the new-project host picker) — it becomes a project bound to `linux-box` — then spawn into *that project*. Satellites dial out to the daemon — a laptop behind NAT is a perfectly good satellite; only the daemon needs a reachable port. The daemon's gate only accepts projects the satellite actually reports — an unregistered checkout fails fast with a readable error, never a silent local fallback.

**Protect the parent's context.** Fan-out multiplies whatever your children say: ten workers × a 20 KB essay each is 200 KB into one parent. The engine caps returns (§4), but the real fix is the brief: end every task with a reply contract — "answer in ≤25 lines: STATUS / FILES / DECISIONS / GATES; no code or diff dumps" — and review diffs with `git diff` locally instead of through the child's mouth. Prefer `notify:false` + cursored `session_collect` for long jobs; big artifacts belong in files, not in chat turns.

**Poll like you mean it.** `working` means mid-turn — do something else, poll again with `since`. `idle` means the turn is done — read. Don't infer completion from silence, and don't re-collect the whole transcript when a cursor gives you the delta.

**Cap the blast radius, then trust it.** Delegation depth (default 2), per-parent fan-out (10), per-project delegated-session ceiling (50), cycle rejection, and daily per-vendor budgets are enforced by the daemon with a stated reason — a runaway fan-out is refused at spawn time, not discovered on the invoice. Set them once in config; design your prompts assuming refusal is possible. Separately, fleet-wide *capacity* (50 live sessions) is managed by graceful LRU eviction, not refusal.

**One task per dispatch.** The completion notification fires per turn. Bundling three asks into one dispatch means one notification for the lot and a transcript you'll have to disentangle; three dispatches give you three checkpoints and three cost lines.

## 6. Trust model, honestly

Per-session secrets and the admin-token fallback are **defense in depth under a single OS user, not a hard boundary** — any same-uid process can ultimately read another's env or the token file. What the gate buys you: agents can't *accidentally* act cross-project or as each other; every action is attributed to an authenticated caller; remote hosts never see secrets that aren't theirs. Hard isolation (per-agent OS users / sandboxes) is deliberately out of scope for now. The HTTP `/mcp` endpoint always requires a bearer (admin or per-session) — the same-user fallback exists only on the local socket.

## 7. When something's off

| Symptom | Likely cause → fix |
|---|---|
| Tools listed but `session_*` answers "not in a ccteam session … no admin web token" | daemon never started on this machine → `ccteam start`, retry |
| `session_spawn: missing project` | main session outside any registered repo → `cd` into one, or pass `project` |
| project's satellite is offline / checkout not reported | bring the satellite back online (`ccteam start` there), or `ccteam init` the repo on it and re-import; wait one heartbeat (~25 s), retry |
| `removed in v0.9.2: host is bound to the project…` | drop the `host` argument — spawn into a project bound to the target host instead |
| Spawn/dispatch might have double-fired after a client timeout | it didn't, if you set `idempotency_key`; start setting it |
| Child seems silent | `session_collect` and look at `activity` — `working` is not silent, it's busy |

Manual for the human surfaces (web console, Telegram/Lark, CLI): [usage.md](usage.md) · [中文](usage-cn.md).
