// v0.9.0 W4 (F4) — team visualization data: `GET /api/v1/agents/graph`
// (a point-in-time snapshot of every session across every host, as nodes +
// parent→child delegation edges). Mirrors the `getJson` pattern every other
// `lib/*Api.ts` module keeps its own private copy of (see `workflowApi.ts`).

import { httpError } from "./httpError";

/** One session node in the team graph (mirrors the Rust `AgentNode`). */
export interface AgentNode {
  sid: string;
  slug: string;
  role: string;
  vendor: string;
  model?: string | null;
  /** Reasoning-effort token off the same live statusline join as `model`
   *  (`low`/`medium`/`high`/`xhigh`/`max`); absent on idle nodes and on
   *  vendors with no effort axis. */
  effort?: string | null;
  host: string;
  /** `"live"` (gateway-tracked) or `"idle"` (persisted, not tracked). */
  status: string;
  parent_sid?: string | null;
  depth: number;
  cost_usd?: number | null;
  title?: string | null;
  last_active: string;
  turn_count: number;
}

/** One parent→child delegation edge (mirrors the Rust `AgentEdge`). */
export interface AgentEdge {
  parent: string;
  child: string;
  title?: string | null;
  /** Best-effort seed — the live SSE `dispatched`/`completed` frames correct
   *  this in real time (see `lib/agentsReducer.ts`). */
  active: boolean;
}

export interface AgentsGraphResponse {
  nodes: AgentNode[];
  edges: AgentEdge[];
  /** Every host any node runs on, `"local"` first, then sorted. */
  hosts: string[];
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

/** Build the graph snapshot URL. Exported for unit tests + so callers share
 *  one template. */
export function agentsGraphUrl(slug?: string): string {
  return slug ? `/api/v1/agents/graph?slug=${encodeURIComponent(slug)}` : "/api/v1/agents/graph";
}

/** Fetch one graph snapshot. `signal` lets the caller ABORT an in-flight
 *  request (unmount, or a poll superseded by a manual refresh) — without it a
 *  slow link leaves the socket held after the view is gone, and the browser's
 *  6-connections-per-origin cap starves every other request on the page. */
export function fetchAgentsGraph(
  slug?: string,
  signal?: AbortSignal,
): Promise<AgentsGraphResponse> {
  return getJson<AgentsGraphResponse>(agentsGraphUrl(slug), signal);
}
