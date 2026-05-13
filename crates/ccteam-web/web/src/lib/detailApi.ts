// V0.3.2 F55 — typed fetch wrappers for the F52 JSON API v1
// (`/api/v1/projects/{slug}` + `/api/v1/projects/{slug}/sessions/{sid}`).
//
// Types mirror docs/interfaces.md §16.2 (`ProjectSummary`) + §16.3
// (`SessionDetail`). Kept in this file (not lib/types.ts) on purpose:
//   - lib/types.ts is owned by F58 (write-action types).
//   - lib/api.ts is owned by F58 and is the AoE-derived REST helpers,
//     not the ccteam JSON API v1.
// Anything F55 needs lives here.

export type ProgressEventRow = {
  ts: string;
  event: string;
  detail: string;
  // Backend often injects extras (tool, phase, slug, sid, etc.) —
  // we keep the shape open so EventsLive can surface them later.
  [key: string]: unknown;
};

export type OutboxRow = {
  filename: string;
  kind: string;
  created_at: string;
  preview: string;
};

export type SessionCard = {
  sid: string;
  harness: string;
  harness_class: string;
  status_class: string;
  status_label: string;
  started_at?: string | null;
  cost_label?: string | null;
  current_phase?: string | null;
  [key: string]: unknown;
};

export type HarnessSnapshotView = {
  model?: string | null;
  context_used_pct?: string | null;
  cost_usd_total?: string | null;
  rate_limit_pct?: string | null;
  captured_at?: string | null;
};

export type ProjectSummary = {
  slug: string;
  team: string;
  kind: "workflow" | "multi_workflow" | "flex" | string;
  is_flex: boolean;
  current_phase: string;
  badge_class: string;
  badge_label: string;
  cost_label: string;
  created_at: string;
  sessions: SessionCard[];
  state: unknown; // serde_json::Value — SPA decides how to render.
  events: ProgressEventRow[];
  outbox: OutboxRow[];
  decision_candidates: string[];
};

export type SessionDetail = {
  slug: string;
  sid: string;
  team: string;
  kind: string;
  harness: string;
  harness_class: string;
  tmux_session: string;
  started_at: string;
  status_class: string;
  status_label: string;
  cost_label: string;
  events: ProgressEventRow[];
  outbox: OutboxRow[];
  decision_candidates: string[];
  harness_snapshot: HarnessSnapshotView | null;
};

/** Custom error so callers can branch on auth vs. not-found vs. other.
 *  Per F55 spec:
 *    401  → throw Error('UNAUTHENTICATED')
 *    404  → throw Error('NOT_FOUND')
 *    other non-2xx → throw with status. */
async function fetchJsonOrThrow<T>(url: string): Promise<T> {
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
  if (!res.ok) {
    throw new Error(`HTTP ${res.status}`);
  }
  return (await res.json()) as T;
}

export function fetchProject(slug: string): Promise<ProjectSummary> {
  return fetchJsonOrThrow<ProjectSummary>(
    `/api/v1/projects/${encodeURIComponent(slug)}`,
  );
}

export function fetchSession(
  slug: string,
  sid: string,
): Promise<SessionDetail> {
  return fetchJsonOrThrow<SessionDetail>(
    `/api/v1/projects/${encodeURIComponent(slug)}/sessions/${encodeURIComponent(sid)}`,
  );
}
