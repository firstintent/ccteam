// 团队 → 编排 tab — ccteam Flow run envelopes + the pure derivations the tab
// renders from. Fetch style mirrors `workflowApi.ts`/`agentsApi.ts` (private
// `getJson` + `httpError`).
//
// CONTRACT NOTE (2026-09-01): `GET /api/v1/projects/{slug}/flow-runs` is being
// built in a parallel backend track; this module codes against the agreed
// draft shape below. If the landed Rust route differs, reconcile HERE — this
// is the single SPA-side home of the contract.
//
// Per-agent detail deliberately needs NO new endpoint: every `agent()` hire
// inside a flow is an ordinary delegation with `parent_sid` = the triggering
// session, so a run's leaves are derived from the delegation graph the team
// view already fetches ([flowRunLeaves]).

import { httpError } from "./httpError";
import type { AgentNode } from "./agentsApi";

/** One `ccteam flow run` execution (RUN-level envelope; mirrors the draft
 *  Rust `FlowRunView`). */
export interface FlowRun {
  run_id: string;
  /** Script `meta.name`. */
  name: string;
  /** Script `meta.description`. */
  description: string;
  /** The triggering session, when a session (not a bare CLI) drove the run. */
  parent_sid?: string | null;
  /** `running` | `ok` | `error` | `brake` — `brake` is a guardrail refusing
   *  new admissions (max-agents / max-cost / budget / wall clock), NOT a
   *  worker failure. Unknown values render verbatim (honest fallback). */
  status: string;
  /** Number of `agent()` hires the run dispatched. */
  agents: number;
  cost_usd?: number | null;
  started_at: string;
  finished_at?: string | null;
}

export interface FlowRunsResponse {
  runs: FlowRun[];
}

/** One project's runs — the team view is cross-project, so the tab fetches
 *  every visible project and keeps the slug for the ambiguity badge. */
export interface ProjectFlowRuns {
  slug: string;
  runs: FlowRun[];
}

export function flowRunsUrl(slug: string): string {
  return `/api/v1/projects/${encodeURIComponent(slug)}/flow-runs`;
}

async function getJson<T>(url: string, signal?: AbortSignal): Promise<T> {
  const res = await fetch(url, {
    headers: { Accept: "application/json" },
    credentials: "same-origin",
    signal,
  });
  if (res.status === 401) throw new Error("UNAUTHENTICATED");
  if (!res.ok) throw await httpError(res);
  return (await res.json()) as T;
}

export function fetchFlowRuns(slug: string, signal?: AbortSignal): Promise<FlowRunsResponse> {
  return getJson<FlowRunsResponse>(flowRunsUrl(slug), signal);
}

/** Fetch every visible project's runs as ONE logical poll attempt.
 *  Per-project fail-SOFT: one slug 403ing (or the endpoint not deployed yet)
 *  must not blank the others — that slug just contributes zero runs this
 *  cycle. The empty state stays honest either way: "no runs visible". */
export function fetchProjectsFlowRuns(
  slugs: readonly string[],
  signal?: AbortSignal,
): Promise<ProjectFlowRuns[]> {
  return Promise.all(
    slugs.map((slug) =>
      fetchFlowRuns(slug, signal).then(
        (res) => ({ slug, runs: res.runs ?? [] }),
        () => ({ slug, runs: [] }),
      ),
    ),
  );
}

// ── pure derivations (node-env tested, no React) ────────────────────────────

/** Status → badge class. `brake` shares warn with `error` (both mean "the run
 *  did not complete its plan") but keeps its own LABEL so a tripped guardrail
 *  never reads as a crash; `running` takes the brand badge (in-progress, not
 *  a verdict). Unknown statuses degrade to the neutral badge. */
export function runStatusBadgeClass(status: string): string {
  if (status === "ok") return "badge ok";
  if (status === "error" || status === "brake") return "badge warn";
  if (status === "running") return "badge brand";
  return "badge";
}

/** Slack past `finished_at` for leaf membership: a worker's final bookkeeping
 *  turn (completion notification, ledger flush) can stamp `last_active`
 *  slightly after the run report closes. */
const LEAF_WINDOW_GRACE_MS = 60_000;

/** A run's leaves = descendants of its triggering session in the delegation
 *  graph, bounded by the run's time window (the trigger session may do other
 *  things outside the run). Window check is a HEURISTIC on `last_active` —
 *  the graph carries no spawn timestamp — so unparseable dates degrade to
 *  "show" (better an extra row than a hidden hire); an out-of-window child's
 *  whole subtree is skipped (its hires predate the run too). Returns [] for
 *  CLI-driven runs with no trigger session. Sids sort natural-ascending, the
 *  same order `agentsTree.ts` gives siblings. */
export function flowRunLeaves(
  nodes: readonly AgentNode[],
  run: Pick<FlowRun, "parent_sid" | "started_at" | "finished_at">,
): AgentNode[] {
  const rootSid = run.parent_sid;
  if (!rootSid) return [];
  const children = new Map<string, AgentNode[]>();
  for (const node of nodes) {
    if (!node.parent_sid || node.parent_sid === node.sid) continue;
    const siblings = children.get(node.parent_sid) ?? [];
    siblings.push(node);
    children.set(node.parent_sid, siblings);
  }
  const startMs = Date.parse(run.started_at);
  const endMs = run.finished_at
    ? Date.parse(run.finished_at) + LEAF_WINDOW_GRACE_MS
    : Number.POSITIVE_INFINITY;
  const leaves: AgentNode[] = [];
  const seen = new Set<string>([rootSid]);
  const queue = [rootSid];
  while (queue.length > 0) {
    const sid = queue.shift()!;
    for (const child of children.get(sid) ?? []) {
      if (seen.has(child.sid)) continue;
      seen.add(child.sid);
      const activeMs = Date.parse(child.last_active);
      const inWindow =
        Number.isNaN(startMs) || Number.isNaN(activeMs)
          ? true
          : activeMs >= startMs && activeMs <= endMs;
      if (!inWindow) continue;
      leaves.push(child);
      queue.push(child.sid);
    }
  }
  return leaves.sort((a, b) => a.sid.localeCompare(b.sid, "en", { numeric: true }));
}

/** Compact language-neutral duration token (`42s` / `4m12s` / `1h12m` /
 *  `2d3h`), same spirit as `relativeTimeEn`'s compact units — durations, like
 *  model/effort tokens, never translate. Running runs measure against
 *  `nowMs`; unparseable input renders `"—"`, never a fake zero. */
export function runDurationLabel(
  startedAt: string,
  finishedAt: string | null | undefined,
  nowMs: number,
): string {
  const start = Date.parse(startedAt);
  if (Number.isNaN(start)) return "—";
  const end = finishedAt ? Date.parse(finishedAt) : nowMs;
  if (Number.isNaN(end)) return "—";
  const secs = Math.max(0, Math.floor((end - start) / 1000));
  if (secs < 60) return `${secs}s`;
  const mins = Math.floor(secs / 60);
  if (mins < 60) {
    const s = secs % 60;
    return s ? `${mins}m${s}s` : `${mins}m`;
  }
  const hours = Math.floor(mins / 60);
  if (hours < 24) {
    const m = mins % 60;
    return m ? `${hours}h${m}m` : `${hours}h`;
  }
  const days = Math.floor(hours / 24);
  const h = hours % 24;
  return h ? `${days}d${h}h` : `${days}d`;
}
