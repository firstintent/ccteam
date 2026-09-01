# Policy hooks & flows

> 中文版: [hook-dynamic-workflows-cn.md](hook-dynamic-workflows-cn.md) · Tool reference: [mcp.md](mcp.md) · Delegation guide: [orchestration.md](orchestration.md)

Three ways to put deterministic code around your agent team — and they compose, because every hire in every mode still passes the policy hook:

| mode | what it is | reach for it when |
|---|---|---|
| **1. Policy hook** | a script that gates every delegation | constraints: quotas, vendor allowlists, project rules beyond the built-in guardrails |
| **2. ccteam Flow** | a deterministic JS script the ccteam runner executes, driving real cross-harness hires | orchestration that must be repeatable, resumable, headless, or huge |
| **3. Claude-native bridge** | Claude Code's own dynamic workflows hiring ccteam agents over MCP | you live in Claude Code and want its workflow UI with cross-harness leaves |

The hook constrains choices an agent makes; a Flow makes the choices itself; the bridge borrows Claude's runtime to do the same.

## 1. The pre-agent policy hook

Every `agent` call (hiring a new session or dispatching to an existing one) can be gated by a script of yours, resolved fresh on every call:

1. `<project>/.ccteam/hooks/pre-agent` — the project's own policy
2. `~/.ccteam/hooks/pre-agent` — the machine-wide fallback
3. neither exists → the call proceeds (an unconfigured daemon behaves exactly as before)

A project that states a policy states **all** of it: the project file *replaces* the global one, never merges with it — the same rule `routing.md` follows. There is nothing to register and nothing to restart: edit the file, and the next `agent` call runs the new logic. `ccteam init` never seeds one; you create it when you want one.

### The contract

The hook is any executable (shell, python, a compiled binary). It runs with cwd = the project root (the global rung: your ccteam home), `CCTEAM_HOOK=pre-agent` in its env, a **3-second budget**, and its own process group — helpers it spawns die with it. It receives one line of JSON on stdin:

```json
{"kind":"hire",
 "caller":{"sid":"s42","vendor":"claude","depth":0,"project":"myapp","context_pct":41},
 "request":{"vendor":"claude","model":"opus","wait":0,"task_head":"first 500 chars…","task_chars":1234},
 "usage":{"claude":{"observed":"2026-09-01T04:10:00Z","windows":[{"w":"5h","pct":83,"resets":"…"}]}},
 "counts":{"children":3,"delegated":12,"cost_24h_usd":4.2}}
```

`usage` is the same per-harness quota map `status{detail:"usage"}` reports — handed in, so a policy needs **no token and no callback** into the daemon (the hook runs lock-free, so calling the REST API anyway is safe too). Unknown facts are omitted, never zeroed.

The verdict is the exit code:

| exit | meaning |
|---|---|
| `0` | allow — the call proceeds |
| `2` | **deny** — your stderr (up to 2000 bytes, verbatim, whitespace and all) is relayed to the calling agent inside the refusal (prefixed `delegation denied by policy: `; an all-whitespace reason gets a stock sentence naming the script) |
| anything else / timeout / not executable | **script fault** — the call is still refused (a guardrail that silently opens when its script breaks is not a guardrail), but as a distinct `policy_script_error` naming the script and the failure, so "your policy said no" and "your policy is broken" never read alike |

Every denial lands in the project's progress ledger (`delegation_policy_denied`) and the daemon's denial counters — a policy that stops half your delegations is visible, never silent.

### Example: quota-aware routing

The reason this exists: let each project decide its own thresholds in its own script, instead of ccteam growing a config DSL.

```sh
#!/bin/sh
# .ccteam/hooks/pre-agent — steer hires off claude when its 5h window runs hot.
payload=$(cat)
vendor=$(printf '%s' "$payload" | jq -r '.request.vendor // "claude"')
pct=$(printf '%s' "$payload" | jq -r '.usage.claude.windows[]? | select(.w=="5h") | .pct // 0' | head -1)
if [ "$vendor" = "claude" ] && [ "${pct:-0}" -ge 80 ]; then
  echo "claude 5h window at ${pct}% — hire codex or kimi for this task instead" >&2
  exit 2
fi
exit 0
```

The denied agent gets that sentence as the tool error and decides again — the constraint is deterministic, the re-decision stays with the model.

### Honest scope

- Everything under one OS user is soft isolation: an agent working in the project can edit the project's own hook. That is project self-governance, not a security boundary.
- A project bound to a satellite host keeps its `.ccteam/` on that machine, where the daemon cannot read it — the **global** hook governs remote projects' delegations.

## 2. ccteam Flow — deterministic orchestration

> Formerly "dynamic workflows"; renamed **Flow** so it never collides with Claude Code's native feature of that name — the CLI was already `ccteam flow`.

A **flow** is a JavaScript file whose script drives many hires deterministically — the plan lives in code, models only do the leaves. Every `agent()` inside it is an ordinary ccteam delegation: any harness, a real sid on the ledger, the same depth and budget guardrails, and your pre-agent policy hook.

### Quick start

```js
// flow.js
export const meta = { name: 'audit-routes', description: 'Audit route handlers for missing auth' }

const files = await agent('List every route file under src/routes/, one path per line, nothing else.')
const audits = await parallel(
  files.trim().split('\n').map(f => () => agent(`Audit ${f} for missing auth checks.`, { label: f }))
)
return audits.filter(Boolean)
```

```bash
ccteam flow run flow.js            # inside an initialized project (--project <slug> otherwise)
```

Progress streams on stderr, one line per event; stdout is the final **RunReport** JSON — the script's returned value, per-agent records (sid, cost, cached), totals, and cache diagnostics. Exit 0 on a clean run.

Flags: `--args <json>` (the script reads it as `args`) · `--parallel <n>` · `--max-agents <n>` · `--max-cost <usd>` · `--budget <usd>` · `--run-dir <dir>` · `--resume <run-dir>` · `--watchdog <secs>`.

### Where flow scripts live

Anywhere `ccteam flow run` can reach — the path is explicit. Convention: keep shared flows in **`.agents/flows/`** (the same family as `.agents/skills`), committed so the whole team runs the same orchestration. **Not `.ccteam/`** — ccteam gitignores that directory, so a script there silently falls out of version control (which is exactly why per-checkout *hooks* DO live there). Runnable samples: [`examples/flows/`](../examples/flows/).

### Triggering a flow

- **From a shell**: `ccteam flow run <script>` — synchronous; `--resume` continues a run.
- **From the main session**: agents have shells — any session (whatever its harness) launches the same command, in the background if it likes, and reads the RunReport JSON when it lands. That IS the main-session trigger today; dedicated MCP `flow_*` tools are deliberately deferred until real usage proves the CLI insufficient — every byte of tool schema taxes every session's context.
- **From Claude Code natively**: bridge mode (§3) — Claude's own workflow runtime, ccteam leaves.

### Evaluate a run, then improve the script

A finished run leaves everything an evaluation needs in its run directory: the persisted script and args, `journal.jsonl` (per-call content key, sid, cost, cached), `results/`, plus the RunReport you captured from stdout (nulls, the brake, the cache diagnostic). Deterministic metrics fall straight out of those files — null rate, spend per leaf, cache reuse; judgment comes from an agent you point at the directory. [`examples/flows/flow-review.flow.js`](../examples/flows/flow-review.flow.js) is that loop expressed as a flow: one leaf grades the run (task clarity, vendor fit, wasted spend), another proposes concrete script edits. Then edit and `--resume` — you re-pay only from the first changed call.

### The script surface

| global | contract |
|---|---|
| `agent(task, opts?)` | the worker's final text; the validated object when `opts.schema` matched; **`null` on any worker-side failure** (vendor error, guardrail or policy refusal — the reason is in the run report) |
| `parallel([...thunks])` | barrier; a failed slot is `null`, the call itself never rejects |
| `pipeline(items, ...stages)` | no barrier between stages — item A can be in stage 3 while B is in stage 1; a stage throw nulls that item and skips its remaining stages |
| `phase(t)` / `log(m)` | progress structure and narration |
| `args` | the `--args` value, verbatim |
| `budget` | `{total, spent(), remaining()}` in USD — children costs, summed live |
| `usage()` | the same per-harness quota map as `status{detail:"usage"}` — quota-aware vendor choice in three lines |

`agent` opts: `vendor` (claude default) · `model` · `effort` · `role` · `sid` (follow up on an existing session — reuse a worker's context across steps) · `keep` (don't stop the worker after its result is consumed) · `label` (also the ledger title) · `phase` · `permission_mode` · `schema` + `retry:{max,prompt}`. An unknown option is a hard error, not a silent ignore.

**Brakes vs failures.** A worker failing resolves that call to `null`. A **brake** — `max_agents`, `max-cost`, wall clock, budget — refuses *new* admissions: a direct `await agent()` throws an error naming the brake, `parallel`/`pipeline` slots mask to `null`, `RunReport.brake` names it either way, and in-flight workers always finish — a brake never cancels running work.

**Determinism.** Script space has no filesystem, network, or process access, and `Date.now()`, `Math.random()`, argless `new Date()` throw — pass timestamps and randomness in via `--args`. That discipline is what makes resume exact.

### Resume

Every call is journaled (`<run-dir>/journal.jsonl`, content-keyed; large results in `results/`). `--resume <run-dir>` — or pointing `--run-dir` at a directory that already holds a journal — re-executes the script: unchanged calls replay from cache without touching the daemon, the first changed call invalidates the rest (the report says where and why), and a call that was mid-flight **re-attaches to its still-running session by sid**. Workers are daemon-managed sessions: they survive the CLI process, so a crash costs you nothing that was already dispatched.

### Scheduling, and the honest edges

- Admission flows through per-run `--parallel` (default 32) and per-vendor slot pools; a vendor quota/limit error backs the whole pool off exponentially instead of hammering the harness.
- A worker is stopped once its result is consumed (`keep:true` to hold it); run end stops the rest. Transcripts stay on the ledger — `agent_read` reads them like any session's.
- **Structured output is extraction, not enforcement**: ccteam injects nothing into a worker, so `schema` means deterministic JSON extraction + validation + a bounded same-session retry; a worker that never complies yields `null`. (Claude Code can force a schema tool onto its own subagents; a cross-harness runner cannot.)
- **The run lives in the CLI process today**: closing it stops *driving* — workers finish their current turns, and `--resume` picks the run back up. Daemon-managed background runs are a next phase.
- Parallel file edits share the project working tree — give agents disjoint files, or have them create their own worktrees; per-hire isolation is not provided yet.
- **Dogfood-testing a flow against a daemon that also serves real chats shares its gateway** — every hire, every progress event, every lock goes through the one process. Point `--home <dir>` (or `CCTEAM_HOME`) at an isolated ccteam home for exploratory or load-heavy runs, exactly like the checker-script discipline elsewhere in this project: a shared daemon is for the traffic it already carries, not a free load-test target.

## 3. Bridge mode — Claude-native workflows driving ccteam

If Claude Code is your main-session entry, its **native dynamic workflows** can orchestrate ccteam agents today: each `agent()` in a native workflow is a Claude subagent, and that subagent loads the ccteam MCP tools (ToolSearch) and hires a real session. You get Claude Code's `/workflows` progress view, pause and resume — while the leaves run on codex/kimi/grok, on the ledger, through your policy hook.

```js
// .claude/workflows/ccteam-team-review.js — run as /ccteam-team-review
export const meta = { name: 'ccteam-team-review', description: 'Cross-harness review via ccteam' }

const files = await agent('Run `git diff --name-only dev...HEAD`; one path per line, nothing else.')
const reviews = await pipeline(
  files.trim().split('\n').filter(Boolean),
  (f) => agent(
    'Load the ccteam tools with ToolSearch (select:mcp__ccteam__agent,mcp__ccteam__agent_read). ' +
    `Hire codex: mcp__ccteam__agent{task:"Review ${f} for correctness bugs. VERDICT first line.", vendor:"codex", wait:240}; ` +
    'poll mcp__ccteam__agent_read{sid, wait:240} if pending. Return ONLY the worker\'s final text.',
    { label: f },
  ),
)
return await agent(`Merge into one ranked list:\n${JSON.stringify(reviews.filter(Boolean))}`)
```

Full version: [`examples/claude-native/`](../examples/claude-native/). The honest trade against a ccteam Flow:

| | ccteam Flow | Claude-native bridge |
|---|---|---|
| glue cost | none — the runner calls the MCP face directly | one Claude subagent per leaf, forwarding over MCP |
| survives | the CLI process — `--resume` re-attaches; workers are daemon sessions | the Claude Code session; exiting restarts the run from scratch |
| planner | any harness, headless, cron | a Claude session only |
| progress | stderr lines + RunReport JSON | the `/workflows` tree, pause/resume keys |

Use the bridge when you are sitting in Claude Code and the workflow UI earns its keep; use a Flow when the run must outlive you, run headless, or drive hundreds of leaves.
