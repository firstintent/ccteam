// v0.8.9 Phase 4 — REST client for the daemon-wide status aggregate
// (`GET /api/v1/status`), the source for the top-bar cost pill + the Status
// global view.
//
// Backend SoT: `crates/ccteam-web/src/routes/status.rs::StatusResponse`. A
// best-effort glance: a missing daemon degrades to `daemon_healthy:false` +
// zeroed cost (never a 500). Auth + error mapping mirror `sessionsApi`:
//   401 → throw Error("UNAUTHENTICATED")  (global TokenEntryGate kicks in)
//   other non-2xx → throw Error("HTTP <status>")

import { httpError } from "./httpError";
import { backgroundHeaders } from "./backgroundRequest";

/** v0.8.18 柱1 — one live session's fleet-view row (`SessionCostRow`). The
 *  loop-ops console skeleton: per-session cost today, oracle/gate columns
 *  next version. `cost_usd` is priced deterministically per-turn by each
 *  turn's canonical model (see status.rs); it is `null` when the session
 *  has priced no turn (rendered "—", never a faked 0). `unpriced_turns`
 *  counts turns skipped for lacking a table-matched model. */
export interface SessionCostRow {
  sid: string;
  project: string;
  role: string;
  vendor: string;
  /** Host axis (`local` or satellite id) — multi-host cost attribution. */
  host?: string;
  status: string;
  cost_usd: number | null;
  unpriced_turns?: number;
}

/** `GET /api/v1/status` response (`StatusResponse`). `budget_cap_24h` is null
 *  when no project configures a cap; `cost_24h_by_vendor` is keyed by vendor
 *  (`"claude"`, `"codex"`, …) and may be empty. `sessions` is the per-session
 *  fleet list (empty on the standalone no-gateway path). */
export interface StatusSnapshot {
  daemon_healthy: boolean;
  sessions_live: number;
  sessions_idle: number;
  cost_24h_usd: number;
  cost_24h_by_vendor: Record<string, number>;
  budget_cap_24h: number | null;
  sessions?: SessionCostRow[];
}

export interface StatusRequestOptions {
  signal?: AbortSignal;
  background?: boolean;
}

/** `GET /api/v1/status`. */
export function getStatus(options: StatusRequestOptions = {}): Promise<StatusSnapshot> {
  return getJson<StatusSnapshot>("/api/v1/status", options);
}

async function getJson<T>(url: string, options: StatusRequestOptions): Promise<T> {
  let res: Response;
  const headers = options.background
    ? backgroundHeaders({ Accept: "application/json" })
    : { Accept: "application/json" };
  try {
    res = await fetch(url, {
      headers,
      credentials: "same-origin",
      ...(options.signal ? { signal: options.signal } : {}),
    });
  } catch (e) {
    throw new Error(`network: ${e instanceof Error ? e.message : "connection failed"}`);
  }
  if (res.status === 401) throw new Error("UNAUTHENTICATED");
  if (!res.ok) throw await httpError(res);
  return (await res.json()) as T;
}
