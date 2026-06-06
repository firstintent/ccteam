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
//   other non-2xx → throw Error("HTTP <status>")

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
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
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
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  return (await res.json()) as T;
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

/** `POST /api/v1/sessions/{sid}/turn` — submit a user turn. 202
 *  `{accepted:true}`; the reply + progress arrive over the SSE stream. */
export function submitTurn(sid: string, text: string): Promise<{ accepted: boolean }> {
  return postJson<{ accepted: boolean }>(`${sessionUrl(sid)}/turn`, { text });
}

/** `POST /api/v1/sessions/{sid}/stop` — deregister the session. */
export function stopSession(sid: string): Promise<{ stopped: boolean }> {
  return postJson<{ stopped: boolean }>(`${sessionUrl(sid)}/stop`, {});
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
}

/** `POST /api/v1/projects/{slug}/sessions` — create (or idempotently reuse)
 *  a `(project, role)` session. 201 `{sid}`. */
export function createSession(
  slug: string,
  opts: CreateSessionOpts,
): Promise<{ sid: string }> {
  const body: Record<string, unknown> = { role: opts.role };
  if (opts.vendor) body.vendor = opts.vendor;
  if (opts.permission_mode) body.permission_mode = opts.permission_mode;
  return postJson<{ sid: string }>(sessionsUrl(slug), body);
}
