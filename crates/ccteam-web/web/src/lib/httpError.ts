// Shared non-2xx → Error mapping for every REST client in `lib/`.
//
// Why this exists: each client used to throw a bare `HTTP ${res.status}` and
// drop the response body on the floor. Every ccteam handler answers with
// `{"error": "..."}` saying exactly what went wrong ("project not found: x",
// "unknown tenant: y", "no Telegram bot configured; save the bot token
// first") — and none of it ever reached the user, who saw only "404" and had
// no way to tell an ACL denial from a typo'd slug. A status code alone is not
// a report; the server already wrote the report, so deliver it.

/** Best-effort read of a ccteam error body. Never throws: a non-JSON or empty
 *  body (or one already consumed) just yields no detail. */
export async function errorDetail(res: Response): Promise<string | null> {
  try {
    const text = await res.text();
    if (!text) return null;
    try {
      const body: unknown = JSON.parse(text);
      if (body && typeof body === "object" && "error" in body) {
        const err = (body as { error: unknown }).error;
        if (typeof err === "string" && err.trim()) return err.trim();
      }
    } catch {
      // Not JSON (axum extractor rejections are plain text) — use it verbatim.
    }
    return text.slice(0, 300).trim() || null;
  } catch {
    return null;
  }
}

/** `HTTP <status>: <server's reason>`, falling back to `HTTP <status>` when
 *  the response carried none. Callers `throw await httpError(res)`. */
export async function httpError(res: Response): Promise<Error> {
  const detail = await errorDetail(res);
  return new Error(detail ? `HTTP ${res.status}: ${detail}` : `HTTP ${res.status}`);
}
