// V0.3.2 F54 — dashboard-local fetch wrapper.
//
// We intentionally do NOT extend `lib/api.ts` here: F58 will rewrite the
// V0.3.2 surface of `api.ts` (token plumbing, write-action helpers, JSON
// error envelope), and dragging a half-built wrapper through that PR
// makes review noisy. The dashboard owns these two helpers; F58 may
// fold them into the unified client later.
//
// Shape mirrors `crates/ccteam-web/src/views.rs::DashboardRow` exactly
// (the askama template and the JSON serializer share the struct). If
// the server adds a field, add it here as optional first — drop the
// optional once the rollout window passes.
//
// V0.4.0 F68 update: `current_phase` removed (phase machinery retired
// in F60). Workflow-aware columns will be added once a workflow-summary
// roll-up endpoint exists; until then the dashboard shows team / kind +
// badge / cost only.

/** One row in the dashboard project list — matches the Rust `DashboardRow`
 *  struct's `Serialize` shape. See `docs/interfaces.md` §16.1. */
export interface DashboardRow {
  slug: string;
  team: string;
  kind: string;
  last_event_label: string;
  badge_class: string;
  badge_label: string;
  cost_label: string;
  /** V0.3.2 F54 — not currently exposed by `/api/v1/projects`; reserved
   *  for the harness pill (claude / codex). F55's session-detail page
   *  carries the authoritative value. Dashboard treats absent ⇒ claude. */
  harness?: string;
}

/** GET `/api/v1/projects`. Returns the parsed array on 2xx.
 *
 *  Throws `new Error('UNAUTHENTICATED')` on 401 so the caller can branch
 *  into the token-expired UI without parsing a status code; throws a
 *  generic `Error` with the response status for any other non-2xx so
 *  the dashboard can surface a useful message. The network-failure
 *  branch (TypeError from fetch) propagates verbatim. */
export async function fetchDashboard(): Promise<DashboardRow[]> {
  const resp = await fetch("/api/v1/projects", { credentials: "same-origin" });
  if (resp.status === 401) {
    throw new Error("UNAUTHENTICATED");
  }
  if (!resp.ok) {
    throw new Error(`/api/v1/projects: ${resp.status}`);
  }
  return (await resp.json()) as DashboardRow[];
}
