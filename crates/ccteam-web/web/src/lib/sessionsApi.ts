// v0.8.7 W4 (DD.1) — REST client for the gateway session resource API
// (`/api/v1/...`), the per-session web UI surface.
//
// This is the namespace of the gateway `s{n}` session ids (minted by the
// IM gateway), NOT the legacy workflow `claude-N`/`codex-N` ids that
// `listApi.ts` (`/sessions/active`) + the operator pages use. The new
// per-session ChatConsole drives sessions exclusively through these
// endpoints; the legacy SessionsListPage / SessionDetail stay on their own
// (unrepointed) progress.jsonl world.
//
// Auth: every call is a plain same-origin `fetch`; the global
// `fetchInterceptor` monkey-patch attaches `Authorization: Bearer <token>`
// automatically (the SSE hook authenticates via cookie instead — see
// `useSessionEvents`). We keep `credentials: "same-origin"` so the cookie
// rides along too. Error mapping mirrors `listApi`/`detailApi`:
//   401 → throw Error("UNAUTHENTICATED")  (global TokenEntryGate kicks in)
//   404 → throw Error("NOT_FOUND")
//   other non-2xx → throw server `{error}` / text body, else `HTTP <status>`

/** One live gateway session (the `SessionView` the backend serializes —
 *  `crates/ccteam-im/src/gateway.rs::SessionView`). `sid` is the gateway
 *  `s{n}` id; `permission_mode` is `"skip"` | `"hitl"` (W2). */
export interface SessionView {
  sid: string;
  project: string;
  role: string;
  vendor: string;
  permission_mode: string;
  current: boolean;
  status: string;
  last_activity_seconds?: number | null;
  /** Wire protocol: `"stream-json"` (the薄/default, no pane) | `"terminal"`
   *  (advanced, owns a tmux/rmux pane). Optional for backward-compat — a
   *  session that predates the field is treated as terminal-capable. */
  protocol?: string;
  /** Where the session runs: `"local"` (reserved; ignored in the UI). */
  host?: string;
  /** v0.8.22 P0-3 — RFC3339 spawn time, read from `meta.json`. Optional for
   *  backward-compat / a session whose meta couldn't be resolved. */
  created_at?: string;
  /** v0.8.22 P0-3 — RFC3339 last-turn-completion time, read from
   *  `meta.json`. Drives recency sort/relative-time display server-side; the
   *  live rail list already arrives sorted `last_active` desc. */
  last_active?: string;
  /** v0.8.22 P1 — user-facing session title (session-title system), read
   *  from `meta.json`. `null`/absent until the first user message is
   *  auto-titled (or a vendor/explicit title is set) — render `role`/`sid`
   *  as the fallback (unchanged from before this field existed). */
  title?: string | null;
  /** v0.8.22 P1 — turns.jsonl line count. */
  turn_count?: number;
  /** v0.8.22 P1 — accrued priced cost (USD); `null` when nothing priced yet. */
  cost_usd?: number | null;
  /** v0.8.23 review §1.3-D item 9 — true when a HITL approval is currently
   *  outstanding for this session (`PendingInteractions::pending_for_sid`).
   *  Drives the "等待批准" attention badge + the rail/history sort. Optional
   *  for backward-compat — absent/omitted reads as `false`. */
  waiting_approval?: boolean;
}

/** One history event from `GET /api/v1/sessions/{sid}` — a mirrored turn
 *  (`crates/ccteam-web/src/routes/sessions_api.rs::turn_to_event`). Used to
 *  seed a reopened per-session transcript before live SSE takes over. */
export interface SessionHistoryEvent {
  turn_id: string;
  ts: string;
  role: string;
  user: string;
  assistant: string;
}

export interface SessionHistory {
  sid: string;
  events: SessionHistoryEvent[];
}

/** Context-window usage for a session (`SessionStatus.context`). `pct` is a
 *  float 0–100. `null` on the parent when a brand-new session hasn't completed
 *  a turn yet (no context numbers to report). */
export interface SessionContext {
  used_tokens: number;
  window_tokens: number;
  pct: number;
}

/** Per-session statusline payload from `GET /api/v1/sessions/{sid}/status`
 *  (`crates/ccteam-im` `ThreadStatus` → `ThreadStatus::status_suffix()`).
 *  `status_line` is the server-rendered, ready-to-display line (prefer it
 *  verbatim when present); `model` / `context` are the structured fields for
 *  styling / fallback. ALL three are `null` for a brand-new session that has
 *  not completed a turn yet. */
export interface SessionStatus {
  sid: string;
  model: string | null;
  context: SessionContext | null;
  status_line: string | null;
}

/** One role summary from `GET /api/v1/projects/{slug}/roles`
 *  (`crates/ccteam-core` `RoleSummary` → `crates/ccteam-web/src/routes/roles.rs`).
 *  `description`/`model` default to `""` server-side when absent. Drives the
 *  new-session modal's role dropdown (real project roles, not static hints). */
export interface RoleSummary {
  role: string;
  description: string;
  model: string;
}

/** Full single-role payload from `GET /api/v1/projects/{slug}/roles/{role}`
 *  (`ccteam_core::RoleDetail` → `crates/ccteam-web/src/routes/roles.rs`).
 *  `frontmatter` is a free-form JSON object (empty object when the `.md` has
 *  no frontmatter fence; values may be non-string, so render them
 *  defensively); `body` is the markdown after the closing fence. Drives the
 *  read-only Roles page detail view. */
export interface RoleDetail {
  role: string;
  frontmatter: Record<string, unknown>;
  body: string;
}

/** Build the per-project sessions URL (gateway `s{n}` list). */
export function sessionsUrl(slug: string): string {
  return `/api/v1/projects/${encodeURIComponent(slug)}/sessions`;
}

/** Build the per-session base URL (`/api/v1/sessions/{sid}`). */
export function sessionUrl(sid: string): string {
  return `/api/v1/sessions/${encodeURIComponent(sid)}`;
}

async function getJson<T>(url: string): Promise<T> {
  let res: Response;
  try {
    res = await fetch(url, {
      headers: { Accept: "application/json" },
      credentials: "same-origin",
    });
  } catch (e) {
    throw new Error(
      `network: ${e instanceof Error ? e.message : "connection failed"}`,
    );
  }
  if (res.status === 401) throw new Error("UNAUTHENTICATED");
  if (res.status === 404) throw new Error("NOT_FOUND");
  if (!res.ok) throw new Error(await errorMessage(res));
  return (await res.json()) as T;
}

async function postJson<T>(url: string, body: unknown): Promise<T> {
  let res: Response;
  try {
    res = await fetch(url, {
      method: "POST",
      credentials: "same-origin",
      headers: { "Content-Type": "application/json", Accept: "application/json" },
      body: JSON.stringify(body),
    });
  } catch (e) {
    throw new Error(
      `network: ${e instanceof Error ? e.message : "connection failed"}`,
    );
  }
  if (res.status === 401) throw new Error("UNAUTHENTICATED");
  if (res.status === 404) throw new Error("NOT_FOUND");
  if (!res.ok) throw new Error(await errorMessage(res));
  return (await res.json()) as T;
}

async function patchJson<T>(url: string, body: unknown): Promise<T> {
  let res: Response;
  try {
    res = await fetch(url, {
      method: "PATCH",
      credentials: "same-origin",
      headers: { "Content-Type": "application/json", Accept: "application/json" },
      body: JSON.stringify(body),
    });
  } catch (e) {
    throw new Error(
      `network: ${e instanceof Error ? e.message : "connection failed"}`,
    );
  }
  if (res.status === 401) throw new Error("UNAUTHENTICATED");
  if (res.status === 404) throw new Error("NOT_FOUND");
  if (!res.ok) throw new Error(await errorMessage(res));
  return (await res.json()) as T;
}

async function errorMessage(res: Response): Promise<string> {
  const fallback = `HTTP ${res.status}`;
  const contentType = res.headers.get("content-type") ?? "";
  try {
    if (contentType.includes("application/json")) {
      const body = (await res.json()) as { error?: unknown; message?: unknown };
      const msg = body.error ?? body.message;
      if (typeof msg === "string" && msg.trim()) return msg;
      return fallback;
    }
    const text = (await res.text()).trim();
    return text ? `HTTP ${res.status}: ${text.slice(0, 200)}` : fallback;
  } catch {
    return fallback;
  }
}

/** `GET /api/v1/projects/{slug}/sessions` — the gateway `s{n}` session list
 *  for one project (the per-session switcher source). Empty array when the
 *  project has no live session. */
export function listSessions(slug: string): Promise<SessionView[]> {
  return getJson<SessionView[]>(sessionsUrl(slug));
}

/** `GET /api/v1/sessions/{sid}` — mirrored history to seed a reopened page. */
export function getHistory(sid: string): Promise<SessionHistory> {
  return getJson<SessionHistory>(sessionUrl(sid));
}

/** `GET /api/v1/sessions/{sid}/status` — the per-session statusline (model +
 *  context-window usage). A brand-new session reports `model`/`context`/
 *  `status_line` all `null`. 404 (unknown sid) maps to NOT_FOUND, 503 (no live
 *  gateway, standalone web) lifts the server `{error}` body; the SessionView
 *  caller catches any failure and simply hides the bar. */
export function getSessionStatus(sid: string): Promise<SessionStatus> {
  return getJson<SessionStatus>(`${sessionUrl(sid)}/status`);
}

/** `GET /api/v1/projects/{slug}/roles` — the project's real roles
 *  (`.claude/agents/<role>.md`), used to populate the new-session role
 *  dropdown. Empty array for a project with no agents/. 404 (unknown
 *  project) maps to NOT_FOUND, 401 to UNAUTHENTICATED. */
export function listProjectRoles(slug: string): Promise<RoleSummary[]> {
  return getJson<RoleSummary[]>(
    `/api/v1/projects/${encodeURIComponent(slug)}/roles`,
  );
}

/** `GET /api/v1/projects/{slug}/roles/{role}` — one role's frontmatter + body
 *  (`.claude/agents/<role>.md`), for the read-only Roles page detail view.
 *  404 (unknown project or role) maps to NOT_FOUND, 401 to UNAUTHENTICATED,
 *  a bad role name to `HTTP 400`. */
export function getRoleDetail(slug: string, role: string): Promise<RoleDetail> {
  return getJson<RoleDetail>(
    `/api/v1/projects/${encodeURIComponent(slug)}/roles/${encodeURIComponent(role)}`,
  );
}

/** `POST /api/v1/sessions/{sid}/turn` — submit a user turn. 202
 *  `{accepted:true}`; the reply + progress arrive over the SSE stream. */
export function submitTurn(sid: string, text: string): Promise<{ accepted: boolean }> {
  return postJson<{ accepted: boolean }>(`${sessionUrl(sid)}/turn`, { text });
}

/** `POST /api/v1/sessions/{sid}/stop` — deregister the session. */
export function stopSession(sid: string): Promise<{ stopped: boolean }> {
  return postJson<{ stopped: boolean }>(`${sessionUrl(sid)}/stop`, {});
}

/** `PATCH /api/v1/sessions/{sid}` — rename a session's user-facing title
 *  (v0.8.22 P1 session-title system). The server rule-truncates the title
 *  (whitespace-collapsed, capped ~40 chars — never an LLM call) and records
 *  it as the STICKY user-source (never later overwritten by the
 *  first-message auto-title or a vendor `ai-title`). Returns the cleaned
 *  title actually stored. */
export function renameSession(
  sid: string,
  title: string,
): Promise<{ sid: string; title: string }> {
  return patchJson<{ sid: string; title: string }>(sessionUrl(sid), { title });
}

/** `POST /api/v1/sessions/{sid}/interrupt` — interrupt the session's
 *  CURRENTLY-RUNNING turn WITHOUT destroying it. The non-destructive twin of
 *  `stopSession`: the session stays live (context preserved), so the user can
 *  then `/model` switch or send a follow-up. Reaches the adapter out-of-band,
 *  so it stops the turn even while it's mid-stream. 200 `{interrupted:true}`. */
export function interruptSession(sid: string): Promise<{ interrupted: boolean }> {
  return postJson<{ interrupted: boolean }>(`${sessionUrl(sid)}/interrupt`, {});
}

/** `POST /api/v1/sessions/{sid}/resolve` — resolve a pending HITL choice by
 *  `token` + the chosen option `id` (`selection`). v0.8.7 review-fix (R-H1):
 *  this routes through the SAME gateway pending machinery an IM click uses
 *  (NOT a turn), so `[Approve]` makes the blocked tool actually run and
 *  `[Deny]` denies immediately. 200 `{resolved:true}`; 404 (mapped to
 *  NOT_FOUND) for an unknown/expired token or an invalid selection. */
export function resolveApproval(
  sid: string,
  token: string,
  selection: string,
): Promise<{ resolved: boolean }> {
  return postJson<{ resolved: boolean }>(`${sessionUrl(sid)}/resolve`, {
    token,
    selection,
  });
}

/** Options for {@link createSession}. `permission_mode` defaults to skip
 *  server-side when omitted; pass `"hitl"` to opt the new session into W2
 *  IM-approval prompts for non-allowlist tool calls. */
export interface CreateSessionOpts {
  role: string;
  vendor?: string;
  permission_mode?: "skip" | "hitl";
  /** Wire protocol for the new session. Omitted → server defaults to
   *  `"stream-json"` (the薄/default path); pass `"terminal"` for the advanced
   *  pane-backed session (terminal mirror / attach / screenshot). */
  protocol?: "stream-json" | "terminal" | "acp";
}

export interface CreateSessionResult {
  sid: string;
  model_warning?: string;
}

/** `POST /api/v1/projects/{slug}/sessions` — mint a fresh session sid.
 *  201 `{sid}` with optional `{model_warning}` for honest model support. */
export function createSession(
  slug: string,
  opts: CreateSessionOpts,
): Promise<CreateSessionResult> {
  const body: Record<string, unknown> = { role: opts.role };
  if (opts.vendor) body.vendor = opts.vendor;
  if (opts.permission_mode) body.permission_mode = opts.permission_mode;
  if (opts.protocol) body.protocol = opts.protocol;
  return postJson<CreateSessionResult>(sessionsUrl(slug), body);
}

// ── v0.8.21 history / resume / external-import ───────────────────────────────

/** A stopped ccteam session from `GET .../sessions/history`. */
export interface HistorySessionView {
  sid: string;
  slug: string;
  vendor: string;
  protocol: string;
  role: string;
  permission_mode: string;
  owner: string;
  vendor_uuid: string;
  created_at: string;
  last_active: string;
  origin: "ccteam" | "adopted";
  /** True when the vendor transcript still exists → precise `--resume`. */
  transcript_present: boolean;
  /** v0.8.22 P1 — user-facing session title, when set. */
  title?: string | null;
  /** v0.8.22 P1 — turns.jsonl line count. */
  turn_count?: number;
  /** v0.8.22 P1 — accrued priced cost (USD); `null` when nothing priced yet. */
  cost_usd?: number | null;
}

/** An external vendor session from `GET .../external-sessions`. */
export interface ExternalSessionView {
  vendor: string;
  vendor_uuid: string;
  title: string;
  last_active: string;
  cwd: string;
  adoptable: boolean;
}

/** Fetch stopped ccteam sessions for a project (lazy — call on expand). */
export function listHistorySessions(slug: string): Promise<HistorySessionView[]> {
  return getJson<HistorySessionView[]>(
    `${sessionsUrl(slug)}/history`,
  );
}

/** Re-activate a stopped session. Returns `{sid}`. */
export function resumeSession(slug: string, sid: string): Promise<{ sid: string }> {
  return postJson<{ sid: string }>(
    `${sessionsUrl(slug)}/${encodeURIComponent(sid)}/resume`,
    {},
  );
}

/** Discover external Claude sessions for a project. */
export function listExternalSessions(slug: string): Promise<ExternalSessionView[]> {
  return getJson<ExternalSessionView[]>(
    `/api/v1/projects/${encodeURIComponent(slug)}/external-sessions`,
  );
}

/** Import (adopt) an external Claude session. Returns `{sid}` of the new ccteam session. */
export function importExternalSession(
  slug: string,
  vendorUuid: string,
): Promise<{ sid: string }> {
  return postJson<{ sid: string }>(
    `${sessionsUrl(slug)}/import`,
    { vendor: "claude", vendor_uuid: vendorUuid },
  );
}
