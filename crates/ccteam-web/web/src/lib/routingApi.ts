// v0.9.11 TEAM-2 — REST client for the division-of-labor charter
// (`GET`/`PUT /api/v1/projects/{slug}/routing`).
//
// Backend SoT: `crates/ccteam-web/src/routes/routing.rs`. The charter is
// user-authored advisory markdown agents PULL via the MCP `status` tool —
// the web only reads/writes the PROJECT file; the global `~/.ccteam/
// routing.md` is a read-only fallback here. Auth + error mapping mirror the
// private-getJson pattern of `agentsApi.ts` (401 → UNAUTHENTICATED).

/** `GET .../routing` response (mirrors the Rust `RoutingDoc`). */
export interface RoutingDoc {
  /** False only when `source === "none"`. */
  exists: boolean;
  /** Which file is being served (the two are alternatives, never merged). */
  source: "project" | "global" | "none";
  /** The PROJECT charter path — always the target a save writes. */
  path: string;
  /** The global file actually served when `source === "global"`. */
  fallback_path: string | null;
  /** Raw markdown, verbatim (empty when `source === "none"`). */
  content: string;
  /** Lower-hex sha256 of `content`; null when `source === "none"`. */
  sha256: string | null;
  /** RFC3339 file mtime; null when `source === "none"`. */
  updated_at: string | null;
}

/** `PUT .../routing` response (mirrors the Rust `RoutingPutResult`). */
export interface RoutingSaveResult {
  sha256: string;
  updated_at: string;
}

/** Charter endpoint URL for one project. Exported for unit tests + so both
 *  verbs share one template. */
export function routingUrl(slug: string): string {
  return `/api/v1/projects/${encodeURIComponent(slug)}/routing`;
}

export function getRouting(slug: string): Promise<RoutingDoc> {
  return getJson<RoutingDoc>(routingUrl(slug));
}

/** Save the PROJECT charter (create-or-replace; never the global file). */
export function putRouting(slug: string, content: string): Promise<RoutingSaveResult> {
  return putJson<RoutingSaveResult>(routingUrl(slug), { content });
}

async function getJson<T>(url: string): Promise<T> {
  const res = await fetch(url, {
    headers: { Accept: "application/json" },
    credentials: "same-origin",
  });
  if (res.status === 401) throw new Error("UNAUTHENTICATED");
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  return (await res.json()) as T;
}

async function putJson<T>(url: string, body: unknown): Promise<T> {
  const res = await fetch(url, {
    method: "PUT",
    headers: { Accept: "application/json", "Content-Type": "application/json" },
    body: JSON.stringify(body),
    credentials: "same-origin",
  });
  if (res.status === 401) throw new Error("UNAUTHENTICATED");
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  return (await res.json()) as T;
}
