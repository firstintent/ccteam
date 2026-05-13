// V0.3.2 F58 — write-action wrappers for the SPA.
//
// F53 left this file as the AoE-original (sessions, profiles, settings,
// docker, login, etc). None of that survives V0.3.2 cleanly. ccteam's
// SPA splits reads (F54 dashboardApi.ts + F55 detailApi.ts) from writes
// (this file). What lives here:
//
//   - postBtw(slug, text, {sid?})         — POST /api/<slug>[/<sid>]/btw
//   - postInjectDecision(slug, path, body) — POST /api/<slug>/inject_decision
//   - postPause(slug, {sid?})              — POST /api/<slug>[/<sid>]/pause
//   - postResume(slug, {sid?})             — POST /api/<slug>[/<sid>]/resume
//
// Each wrapper POSTs `application/json`, expects the F52 JSON shape
// (`{"ok": true}` on success, `{"ok": false, "error": "..."}` on
// failure), and throws `Error(json.error)` when the server reports
// failure or `Error("UNAUTHENTICATED")` on 401. The 401 itself is
// caught by the global `fetchInterceptor` wrapper which dispatches
// `TOKEN_EXPIRED_EVENT` — we don't re-emit it here.

/** Reasonable client-side caps matching the server's F52 validation
 *  (actions.rs `BTW_MAX` / `DECISION_BODY_MAX`). Surfaced here so the
 *  form components can show counters / disable submit before the
 *  round-trip. */
export const BTW_MAX = 4000;
export const DECISION_BODY_MAX = 8000;

interface ActionResponse {
  ok: boolean;
  error?: string;
}

/** Shared POST helper. Parses JSON, throws on 401, throws on
 *  `{ok: false}`. Returns void on success — write-action endpoints
 *  carry no useful payload beyond `{ok: true}`. */
async function postJson(url: string, body: unknown): Promise<void> {
  const res = await fetch(url, {
    method: "POST",
    credentials: "same-origin",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });

  if (res.status === 401) {
    // fetchInterceptor already dispatched TOKEN_EXPIRED_EVENT for the
    // gate; we just need to abort the calling form handler.
    throw new Error("UNAUTHENTICATED");
  }

  let parsed: ActionResponse | null = null;
  try {
    parsed = (await res.json()) as ActionResponse;
  } catch {
    parsed = null;
  }

  if (!res.ok) {
    const msg = parsed?.error ?? `Server error (${res.status})`;
    throw new Error(msg);
  }
  if (parsed && parsed.ok === false) {
    throw new Error(parsed.error ?? "Action failed");
  }
}

export async function postBtw(
  slug: string,
  text: string,
  opts?: { sid?: string },
): Promise<void> {
  const url = opts?.sid
    ? `/api/${encodeURIComponent(slug)}/${encodeURIComponent(opts.sid)}/btw`
    : `/api/${encodeURIComponent(slug)}/btw`;
  await postJson(url, { text });
}

export async function postInjectDecision(
  slug: string,
  path: string,
  body: string,
): Promise<void> {
  const url = `/api/${encodeURIComponent(slug)}/inject_decision`;
  await postJson(url, { path, body });
}

export async function postPause(
  slug: string,
  opts?: { sid?: string },
): Promise<void> {
  const url = opts?.sid
    ? `/api/${encodeURIComponent(slug)}/${encodeURIComponent(opts.sid)}/pause`
    : `/api/${encodeURIComponent(slug)}/pause`;
  await postJson(url, {});
}

export async function postResume(
  slug: string,
  opts?: { sid?: string },
): Promise<void> {
  const url = opts?.sid
    ? `/api/${encodeURIComponent(slug)}/${encodeURIComponent(opts.sid)}/resume`
    : `/api/${encodeURIComponent(slug)}/resume`;
  await postJson(url, {});
}

/** Read the server's auth-required state once. Used by `useAuthState`
 *  on mount; exposed here so non-hook contexts (tests, smoke checks)
 *  can also probe. Returns `null` for `wire_token` when auth is
 *  disabled (loopback default), or `"ccteam:<hex>"` when required. */
export interface AuthTokenResponse {
  wire_token: string | null;
}

export async function fetchAuthToken(): Promise<AuthTokenResponse | null> {
  try {
    const res = await fetch("/api/v1/auth/token", {
      credentials: "same-origin",
    });
    if (!res.ok) return null;
    return (await res.json()) as AuthTokenResponse;
  } catch {
    return null;
  }
}
