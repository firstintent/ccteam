// V0.3.2 F55 — typed fetch wrappers for the F52 JSON API v1
// (`/api/v1/projects/{slug}` + `/api/v1/projects/{slug}/sessions/{sid}`).
//
// Types mirror docs/interfaces.md §16.2 (`ProjectSummary`) + §16.3
// (`SessionDetail`). Kept in this file (not lib/types.ts) on purpose:
//   - lib/types.ts is owned by F58 (write-action types).
//   - lib/api.ts is owned by F58 and is the AoE-derived REST helpers,
//     not the ccteam JSON API v1.
// Anything F55 needs lives here.
//
// V0.4.0 F68 update: `current_phase` and `decision_candidates` removed
// from the TS shape (matches F67's Rust drop). Workflow view consumes
// the new `workflow_summary` field — see `WorkflowSummary` /
// `AgentStatus` / `AgentSessionStatus` below.

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
  [key: string]: unknown;
};

export type HarnessSnapshotView = {
  model?: string | null;
  context_used_pct?: string | null;
  cost_usd_total?: string | null;
  rate_limit_pct?: string | null;
  captured_at?: string | null;
};

/** V0.4.0 F67 / F68 — workflow agent session status.
 *
 *  Serde tag = "status" + rename_all = "snake_case" on the Rust side
 *  (`crates/ccteam-core/src/progress.rs::AgentSessionStatus`). The
 *  discriminator is "status" with values "running" | "done" | "errored";
 *  Done / Errored carry `cost_usd: number`. */
export type AgentSessionStatus =
  | { status: "running" }
  | { status: "done"; cost_usd: number }
  | { status: "errored"; cost_usd: number };

/** V0.4.0 F67 / F68 — per-agent aggregate for the workflow view.
 *
 *  Matches `crates/ccteam-core/src/queries.rs::AgentStatus` 1:1.
 *  `queued_count` stays 0 in V0.4.0 (F66's pending queue is in-memory). */
export interface AgentStatus {
  role: string;
  running_count: number;
  queued_count: number;
  total_cost_usd: number;
  last_session_status: AgentSessionStatus | null;
}

/** V0.4.0 F67 / F68 — project workflow snapshot consumed by the SPA's
 *  workflow view. Matches `crates/ccteam-core/src/queries.rs::WorkflowSummary`
 *  1:1. `workflow_name` is `""` when the project has no workflow.yaml
 *  (legacy V0.3.x slug). */
export interface WorkflowSummary {
  workflow_name: string;
  agents: AgentStatus[];
  /** Map: `<input or output dir relative path>` → file count. */
  artifact_counts: Record<string, number>;
  total_cost_usd: number;
  escalation_count: number;
  /** Map: `role` → `"waiting"` / `"fired"`. */
  gate_states: Record<string, string>;
}

export type ProjectSummary = {
  slug: string;
  team: string;
  kind: "workflow" | "multi_workflow" | "flex" | string;
  is_flex: boolean;
  badge_class: string;
  badge_label: string;
  cost_label: string;
  created_at: string;
  sessions: SessionCard[];
  state: unknown; // serde_json::Value — SPA decides how to render.
  events: ProgressEventRow[];
  outbox: OutboxRow[];
  /** V0.4.0 F68 — workflow snapshot. `null` for legacy projects without
   *  a workflow.yaml; default-shaped (empty agents/artifacts) otherwise. */
  workflow_summary: WorkflowSummary | null;
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
