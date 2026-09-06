# The ccteam MCP server — complete tool reference

> 中文版: [mcp-cn.md](mcp-cn.md) · Plain-language delegation guide: [orchestration.md](orchestration.md) · Human surfaces manual: [usage.md](usage.md) · Policy hooks & workflows: [hook-dynamic-workflows.md](hook-dynamic-workflows.md)

ccteam exposes **one MCP server, named `ccteam`**, over streamable HTTP at `POST /mcp` on the daemon (`http://127.0.0.1:7331/mcp` by default). Your harness namespaces the tools with the server key — Claude shows `mcp__ccteam__agent`, other harnesses use their own prefix. The surface is deliberately a **menu, not a manual**: six tools, one-line parameter descriptions, compact JSON bodies, and thin defaults with knobs — because every byte of schema and every default response line is charged to an agent's context. Edge cases and failure semantics live in the server's error messages (only the caller who trips one pays) and on this page (a human reads it once).

## 1. Connecting: two credential families

`POST /mcp` always requires a bearer — there is no cookie or web-token path, and no admin tier:

| Bearer | Who | How you get it |
|---|---|---|
| `ccteam-sid:<sid>:<secret>` | A **ccteam-managed session** speaking for itself | Written into the session's curated MCP config at spawn — nothing to do |
| `ccteam-enroll:<id>:<secret>` | A **hand-started client** (a CLI session you launched yourself, an SDK, a script) | `ccteam config mcp` writes a machine-wide one into each vendor's global config; the web console mints project-scoped ones (project page → external agent) |

An enrollment credential only says *whose* config it is. The per-process identity is issued at `initialize`: the response carries an `Mcp-Session-Id` header that every later request must echo; `DELETE /mcp` (or a ~2 h idle sweep) ends that binding. An enrolled client becomes a real ledger session (`managed_by: external`), so its hires are its children, not roots.

**Naming a workspace.** A machine-wide credential names no project, so an enrolled client's first `agent` / `agent_read` / `agent_stop` call must carry `project:"<slug>"` — the first project named sticks for the binding's life, only projects the credential's owner can see are accepted, and ccteam never infers one from a working directory. A refusal lists the slugs you can reach.

## 2. What the server injects (and to whom)

The tool list and the server `instructions` are **composed per caller** at connect time, so a session never carries a hiring manual it cannot use:

| Caller | `tools/list` |
|---|---|
| A session that can still hire (below `delegation.max_depth`, default 2) | `status` · `grok_claude_codex_kimi` · `agent` · `agent_read` · `agent_stop` |
| A child at the depth cap, or hired with `tools:"read"` | `status` · `agent_read` |
| Hired with `tools:"none"` | *(empty)* |
| A hand-started client (before and after naming a project) | all six |
| …and additionally, any caller with a chat to answer into (a root session, or one currently bound to an IM/web chat) | + `chat_send_file` |

`CCTEAM_DISABLE_TOOLS` (comma list of groups: `admin`, `chat`, `session`) still applies on top. The face is fixed for the life of the process; a resume is a new process and recomputes it. **Hiding a tool is a listing decision, not a permission**: a hidden tool that is called anyway hits the same authorization gates it always did.

`initialize.instructions` stay under ~1 kB and are composed the same way: one line on what ccteam is; the "use `agent`, never shell out to `codex exec` / `claude -p`" policy only when the caller can hire; the chat-envelope note only when a chat can reach it; the attachment rule (a `<channel …>` tag or `[attachment …]` line carrying `image_path=` / `file_path=` means: read those files before answering) always; and one declarative identity fact — `You are s42 in project cct. Completion notifications from your hires arrive here.`, or, for a client-run session and an enrolled client that has not named a project, the opposite fact (`notifications cannot be pushed to you; agent_read{sid,wait} awaits a turn instead`), plus the depth-cap fact when it applies. Delivery is stated rather than left to be inferred: a managed session that read a missing `notify_deliverable` as "I must be hand-started" built a polling side-channel it did not need. ccteam states who you are and where you work, never how to behave.

## 3. The six tools

### `agent` — hire, or hand over the next task

`{task, sid?, vendor?, wait?, model?, effort?, role?, project?, title?, notify?, tools?, mode?, permission_mode?, idempotency_key?, parent_sid?}`

`task` is **required**, forwarded verbatim as a user turn (zero injection); there is no spawn-only form.

- **No `sid` = a hire.** `vendor` picks the harness — `claude` (default) / `codex` / `grok` / `opencode` / `kimi` / `pi` / `dsh` — and the response always carries a **new** sid. `model` / `effort` pass to the vendor verbatim (omit for its default; a value the vendor refuses fails the hire, it is never silently ignored). `role` names a `.claude/agents/<role>.md` persona (omit for roleless — the bare vendor reads the project's own `CLAUDE.md`/`AGENTS.md`). `mode` is DSH-only (`standard` | `ptc` | `minimal` | `creator`). `permission_mode:"hitl"` pops approve/deny prompts to your bound chat; the default `skip` does not. `tools` sets the child's own face (§2). `title` (≤80 chars) labels the ledger and team view only — it never enters any prompt. `parent_sid` preserves the delegation edge when ccteam does not manage *you*.
- **With `sid` = a follow-up** on that session; a `released` session resumes by sid first. Hire-only parameters are rejected in this form rather than silently ignored.
- `wait` — seconds to block inline, 0–240 (default 0 = async). It waits for **this request's** answer: a sibling task on the same child finishing first is that request's completion, not yours, and it is pushed to you as usual. A timeout answers `answered:false` with the delivery facts below, and **never cancels the child**.
- `idempotency_key` — a retry with the same key replays the original call instead of doubling it (scoped per project for a hire, per child for a follow-up; in-memory, ~1 h). A replayed response adds `idempotent_replay:true`.
- There is **no `host`** (the machine follows the project binding) and **no `protocol`** (the wire channel is derived from the vendor); both are hard errors, as is the retired `wait_seconds` (renamed `wait`).

**Every call is a request with an identity.** ccteam mints a `request_id` and makes it durable *before* the vendor is written to, carrying its own parent, `notify` mode, `title` and lifecycle (`accepted` → `queued` | `submitted` → `executing` → `answered` | `failed`). A request is resolved **only** by the execution turn it is bound to — never by recency, timestamp or queue position — so a second dispatch to a busy child cannot inherit the first one's answer, rename it, or redirect its notification.

Responses (compact JSON): async → `{sid, request_id, turn_id, status, delivery, queue_position?}`.

- `status` is what the adapter actually **did**: `started` (the child was idle and this opened a turn), `injected` (it joined the turn already running), or `queued` (it is a distinct follow-up turn waiting its turn — also the shape when the task is queued behind a pre-restart process, which adds a `hint`).
- `queue_position` is 1-based and present only when the adapter can see its own FIFO. `turn_id` names the execution turn this task **will** run in, even while it is queued.
- `delivery` keeps four different claims apart: `accepted` (ccteam holds it durably), `queued` (it is being retained ahead of the harness), `written` (the bytes reached the harness), `executing` (a turn carrying it was observed to open). A stdin flush is not proof the model read anything, so `executing` is `"unknown"` until a turn opens; anything ccteam cannot observe says `"unknown"` rather than a confident `false`.
- `notify_deliverable:false` when no completion notification can reach you — then use `agent_read{sid,wait}`.

Inline (`wait`) → `{sid, request_id, turn_id, turn, status:"completed"|"failed", context_pct?, cost_usd?, result_text, error_kind?, error?}`, where `turn_id` is the transcript row that answered **this** request; `result_text` keeps a 2000-char head/tail excerpt — the same `final` tier a pushed notification can carry — whose marker names the exact `agent_read{sid,turn:<turn_id>,max_chars}` call for the whole text. A lapsed `wait` answers `{sid, request_id, turn_id, status, state, delivery, answered:false}` — `state` is the request's live lifecycle state, so "still third in the queue" is distinguishable from "running". A `wait` that returns the answer also cancels **that request's** completion notification: you hold it, so it is never pushed to you a second time. An unknown `sid` error distinguishes a session that never existed here from one its user explicitly stopped.

### `agent_read` — the roster, or one transcript

`{sid?, n?, tail?, since?, turn?, max_chars?, wait?, project?, activity?, tree?}` — read-only; the `sid` decides what you get.

- **No `sid` = the roster** of sessions you can reach, most recently active first — for a managed session that is its own project (naming another is refused, exactly as it is for every other sid-addressed call); a web/tenant caller sees the projects it owns. `n` rows (default 10, max 500), filters `project` and `activity` (`working` | `idle` | `stale` | `stuck` | `all`), and `tree:true` adds delegation topology **over the returned rows only**. A row is `{sid, vendor, model?, role?, title?, activity, residency?, context_pct?, parent_sid?, is_self?, waiting_approval?, host?, cost_usd?, tokens_total?}` with empty fields omitted; `is_self` marks your own row; `truncated:true` + `total` appear only when the cap bit. `residency` appears only when ccteam holds no process: `released` resumes on your next `agent{sid}` — reuse it instead of hiring a twin — and `stopped` was ended by its user.
- **With `sid` = that session's transcript**, **newest first** by default (`tail` defaults to true unless `since` is given, which pages forward from a `turn_id` cursor — **oldest unread first**, so `since` + `n:1` is the oldest unread turn, never a shortcut to the newest). `n` defaults to **1** turn here — "what did it answer" is the newest one, and the roster's ten is for one-line rows — with `max_chars` 1000 across the page (100–50000). Longer content keeps a 70 % head / 30 % tail excerpt whose marker is the exact one-call read of that whole turn; a turn is returned intact when that marker would cost more than the text it withholds, and a page whose rows would each fall under ~200 characters drops its oldest rows (counted in `remaining`) rather than shredding every one. The full text is always in the ledger. Body: `{activity, context_pct?, cursor?, remaining?, latest?, cost_usd?, tokens_total?, residency?, truncated?, requests?, resolved_requests?, turns:[{turn_id, content, outcome?, error_kind?, error?}]}` — `cursor` is the last turn on the page; `remaining` counts the matching turns the page did not show (the still-unread ones for a `since` read); `latest` names the newest turn whenever the page does not end on it; `truncated:true` means a returned turn's text was cut to `max_chars`. Empty `turns` = no answer yet; `activity:"working"` = mid-turn.
- **`requests`** — what this session still owes and to whom: outstanding rows first (acceptance order), then a bounded tail of resolved ones, up to ten. A row is `{request_id, parent_sid, state, notify, title?, queue_position?, turn_id?, answered_turn?, created_at, delivery}` with the same four delivery facts the dispatch response carries. This is how a dispatcher sees its own queue instead of inferring one.
- **`turn:<turn_id>`** — that exact turn, and nothing else, whatever the session has finished since. It is what every truncated excerpt's read recipe names: `n:1` meant "the newest turn", which is only what an excerpt was at the instant it was written. A turn the transcript does not hold is an error, never a page of something else.
- **`n:0` = status only**: the same body with no turn text — `activity`, `context_pct`, `latest`, and with `since` the unread count as `remaining`. This is the cheap "is it done? did it say anything new?" read. You rarely need it: for your own hires the completion notification is authoritative and arrives on its own; poll only a session that says `notify_deliverable:false`, and prefer `wait` over a loop.
- `wait` (with `sid`) — seconds to hold the read open while the target's turn is in flight, 0–240 (default 0). At the boundary you get the ordinary body containing the final turn, plus `resolved_requests`: which of **your** tasks on that session this read answered, named, so an answer is never mistaken for another task's. On timeout the ordinary body with `activity:"working"`; with nothing in flight it returns at once. A lapsed wait never touches the turn. `agent_read{sid,wait:240,since:<cursor>}` in a loop is the way to sit out a child that runs longer than an inline `agent{wait}` can cover — never tail its `turns.jsonl` yourself. When the wait returns at a boundary and you are the parent that dispatched the task, the completion notification is suppressed: you already have the answer.
- The retired `limit` parameter is a hard error (renamed `n`).

### `agent_stop` — explicitly end a session

`{sid}` → `{sid, stopped:true}`. An explicit command, never a proactive kill: the transcript survives and `agent_read{sid}` still reads it. An agent may only stop its own descendants — and a hand-started client that reconnects is a *new* ledger node, so its earlier hires are no longer its descendants: stop those from the web console or `POST /api/v1/sessions/{sid}/stop`, which the refusal itself says. (ccteam itself has exactly two automatic brakes: the daily per-vendor budget cap refuses *new* work, and live-session capacity releases the least-recently-active idle session — creation never fails for capacity.)

### `status` — who can be hired, what it costs; tiered

`{detail?: "brief" | "models" | "vendors" | "routing" | "usage" | "full"}` — read-only, default `brief` (~100–200 B): `{project, host, cost_24h_usd, hire:[…]}` where `hire` lists the vendors actually installed on the host your project is bound to, plus `host_online:false` / `stale:true` when a satellite is offline or its snapshot old, and `budget_disabled:[…]` when a vendor hit its cap. `detail` buys more, only when asked:

- `models` — observed model ids + reasoning-effort ladders per vendor (runtime last-seen, with a seen-at) and the hub `models.json` catalog, kept as two separately labeled sources. Advisory, never a hiring allowlist.
- `vendors` — per-vendor installed / version / auth (`unknown` — honest: being on PATH never masquerades as logged in, and it never blocks a hire) / budget posture, an observed timestamp, and the pi/dsh bridge notes.
- `routing` — your routing notes verbatim (`source`, `sha256`, `updated_at`, `truncated`, `text`; project `<project>/.ccteam/routing.md` replaces the global `~/.ccteam/routing.md`, never merged), or `{missing:[…]}` naming both paths.
- `usage` — how much room is left, on both axes. See below.
- `full` — all of the above plus daemon health and every visible project's 24 h cost. Operator data lives only here.

#### `detail:"usage"` — your context headroom + each harness account's remaining quota

Two numbers a session needs before it hires anything, in one call (`full` carries them too):

```json
{"you": {"sid": "s42", "context_pct": 63},
 "usage": {
   "claude": {"observed": "2026-08-31T09:12:03+00:00", "source": "status card", "subscription": "max",
              "windows": [{"w": "5h", "pct": 8, "resets": "2026-08-31T14:00:00Z"},
                          {"w": "7d", "pct": 23, "resets": "2026-09-03T00:00:00Z", "severity": "warning"},
                          {"w": "7d", "model": "Fable", "pct": 16, "resets": "2026-09-03T00:00:00Z"},
                          {"w": "credits", "pct": 3}]},
   "codex":  {"observed": "…", "source": "session release", "windows": [{"w": "7d", "pct": 12, "resets": "…"}]}}}
```

Read `you.context_pct` first — it decides *continue here vs. start fresh* — then the harness windows decide *whom to hire*: a harness at 8 % of its 5-hour window has room for a long job; one at 92 % of its weekly does not. `pct` is percent **consumed** (higher = less left). A `7d` row carrying `model` is that model's own weekly bucket, which is the one that constrains a spawn naming it; a `7d` row without one is the shared pool.

Honest by construction: **a harness appears only when ccteam has actually observed its account** — no live session of it and no unexpired observation means no row at all, never a zeroed one you could misread as headroom. Each window disappears at the harness's *own* declared reset rather than at some staleness cutoff, so nothing stale is ever shown as current; `observed` says when ccteam heard it. Reading costs no probe: live same-harness sessions are asked for state they already hold (never a turn), and otherwise the recorded observation is read back.

The same map, for scripts: **`GET /api/v1/usage`** → `{"usage": {…}}`, identical shape, optional `?vendor=claude`. It sits inside the ordinary web-token gate (any logged-in identity), so:

```bash
curl -sS -H "Authorization: Bearer ccteam:$(cat ~/.ccteam/secrets/web-token)" \
     http://127.0.0.1:7331/api/v1/usage | jq '.usage.claude.windows'
```

The token file holds bare hex; you add the `ccteam:` prefix. On a loopback bind the web gate may be disabled entirely, in which case the header is simply ignored. The port comes from `~/.ccteam/run/daemon-endpoint.json` (`web_bind`). Not to be confused with `GET /api/v1/vendors/quota`, which is an admin-only *network* probe of vendor billing APIs; this route costs no network and no credentials.

### `grok_claude_codex_kimi` — the bare-name discovery alias

No parameters; returns the same brief `status` body. It exists for hosts that surface tool *names* only — nothing else on the surface says "grok" or "codex", so this name front-loads the vendor keywords.

### `chat_send_file` — send a file back to your own chat

`{path, caption?, kind?}` — sends a file from the daemon's filesystem to the chat bound to *you* (a chat user cannot open a local path). `kind` (`photo` | `document`) defaults from the extension. Zero addressing parameters by design; listed only for chat-capable callers (§2).

## 4. Completion notifications

Every `agent` task is watched (unless you opt out) and reports **once, at the boundary of the turn its own request is bound to** — a chatty child's mid-turn narration stays in the ledger, and a sibling task finishing first is that task's report, not yours. The notification is one header line — `s12 done · codex · turn 7 · ctx 19% · «verify the fix» req-18d2… · 2 still queued` (`⚠` from 85 %; `s12 FAILED (<kind>) …` on failure) — followed by an excerpt of the answer. The header names **which** request answered and **its own** title (a later dispatch never renames an earlier one), and `N still queued` is what that child still owes you. `turn N` counts turns that **finished**, not messages that were accepted: three tasks handed to a child that has completed one report `turn 1`.

| `notify` | Excerpt | Use |
|---|---|---|
| `brief` (default) | 500 chars, head/tail, + the exact `agent_read{sid,turn:<turn_id>,max_chars}` call for the whole answer | the default: verdict + coordinates; the parent is the scarcest context in a team, and the full text is one precise call away |
| `final` | 2000 chars, same shape | a parent that wants the whole answer pushed |
| `off` | none (ledger only) | fire-and-forget |

An omitted `notify` **inherits** whatever you last asked for on that child among your still-outstanding requests; an explicit one overrides, and with no precedent the default is `brief`. (Reverting to the default on a follow-up is how a deliberate `final` silently became a 443-character `brief` mid-conversation.) Booleans still parse (`true`→final, `false`→off); the retired `all` is a readable error (it behaved exactly like `final`). A task whose answer you took inline — `agent{wait}`, or an `agent_read{sid,wait}` that returned at the boundary — sends no notification at all: the decision is made when the wait is declared, not undone afterwards, so the two paths can never both deliver. That suppression is per **request**: a parent blocked on B is still pushed A's completion, because it is not holding it. Delivery needs a managed parent: ccteam appends the notification to the parent's conversation as an ordinary user turn, **once**. A notification is a distinct follow-up turn, never a steer: a parent that is mid-turn receives it right after that turn ends (claude shows a mid-turn stdin line to the model twice — as a queued-command preview and again as the next prompt — so the boundary is where it is read exactly once; the parked line is mirrored to disk and survives a daemon restart). Between processes it is queued and delivered on resume. A hand-started parent has no return transport — its dispatch replies say `notify_deliverable:false`, its `initialize` instructions say so up front, and it uses `agent{wait}` or `agent_read{sid,wait}` instead. Dispatching to a session you did not hire is a handoff: it runs and is recorded, but subscribes you to nothing unless you pass `notify` explicitly.

## 5. Protocol details

- **Versions**: the server negotiates `2025-06-18`, `2025-03-26`, `2024-11-05`. A client asking for anything else gets the server's latest (per spec — never an error). An `MCP-Protocol-Version` request header naming a version the server does not speak — including a present-but-empty or non-UTF-8 value — is refused with HTTP 400; an absent header just negotiates at `initialize`.
- **Transport**: one JSON-RPC message per `POST`; notifications answer 202 with an empty body; `GET /mcp` is 405 (no server-initiated stream); `DELETE /mcp` closes an enrolled binding. Parse errors are JSON-RPC `-32700` with HTTP 200.
- **Annotations**: `status`, its alias and `agent_read` declare `readOnlyHint`; `agent_stop` declares `destructiveHint`; `agent` and `chat_send_file` declare `destructiveHint:false`.
- **Serialization**: every body is compact JSON (no pretty-printing); empty/default fields are omitted rather than spelled out.
- **Observability**: the daemon logs one INFO line per tool call *and* per discovery request (`initialize` / `tools/list`) with the caller tier — so "how many calls did this session make" is a log query, not a guess.

## 6. Guardrails and trust, honestly

Delegation is guarded by the daemon with a stated reason: depth (`delegation.max_depth`, default 2), fan-out (10 per parent), 50 delegated per project, cycle rejection (self/ancestor), and per-vendor 24 h budgets. A managed session's `(sid, secret)` principal scopes it to its own project and attributes every action — but it is **defense in depth under a single OS user, not a hard boundary**: same-uid processes can ultimately read each other's environment. What it buys is that agents cannot *accidentally* act cross-project or as each other. Hard isolation (per-agent OS users / sandboxes) is deliberately out of scope for now.

## 7. Wiring and verification

- `ccteam config mcp` registers the server with Claude, Codex, Grok, OpenCode and Kimi (their global configs; ccteam's only write there is its own entry).
- **DSH** has no ccteam-writable config: its surface is the `@ccteam/ccteam-ui` plugin inside the DSH web runtime, which registers the same six tools once at load (a static full face — per-caller faces apply to harnesses that talk to `POST /mcp` directly) and can deliver completion notifications back into the DSH conversation.
- **Pi** gets the tools only in ccteam-managed sessions, via an embedded bridge (`ccteam_`-prefixed names inside Pi; the read-only ones are auto-allowed); a `pi` you start by hand is untouched.
- Verify any time: `ccteam doctor --verify-mcp` → **6 tools, 0 stubs**; `claude mcp list` shows `ccteam ✔`; a session that lists fewer tools than a peer is not broken — that is its face (§2).
