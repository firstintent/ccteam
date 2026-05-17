// V0.5.1 F103a — cross-project list APIs for the `/sessions` top-level
// tab.
//
// Mirrors the new Rust handler in `crates/ccteam-web/src/routes/api_v1.rs`:
//
//   GET /api/v1/sessions/active → Vec<ActiveSessionInfo & { slug }>
//
// Shape is the same `ActiveSessionInfo` already exported by
// `workflowPanels.ts`, with a leading `slug` field so the SPA can deep
// link into `/p/<slug>/s/<session_id>` without a second lookup.

import type { ActiveSessionInfo } from "./workflowPanels";

export interface ActiveSessionWithSlug extends ActiveSessionInfo {
  /** Owning project slug — joined at the API layer so we don't
   *  coordinate per-project fetches in the SPA. */
  slug: string;
}

async function fetchJson<T>(url: string): Promise<T> {
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

/** GET `/api/v1/sessions/active`. Empty array when no project has a
 *  live agent_spawn. Throws `UNAUTHENTICATED` on 401 so the global
 *  TokenEntryGate kicks in.
 */
export function fetchAllActiveSessions(): Promise<ActiveSessionWithSlug[]> {
  return fetchJson<ActiveSessionWithSlug[]>("/api/v1/sessions/active");
}
