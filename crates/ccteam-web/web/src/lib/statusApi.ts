// v0.8.9 Phase 4 — REST client for the daemon-wide status aggregate
// (`GET /api/v1/status`), the source for the top-bar cost pill + the Status
// global view.
//
// Backend SoT: `crates/ccteam-web/src/routes/status.rs::StatusResponse`. A
// best-effort glance: a missing daemon degrades to `daemon_healthy:false` +
// zeroed cost (never a 500). Auth + error mapping mirror `sessionsApi`:
//   401 → throw Error("UNAUTHENTICATED")  (global TokenEntryGate kicks in)
//   other non-2xx → throw Error("HTTP <status>")

/** v0.8.18 柱1 — one live session's fleet-view row (`SessionCostRow`). The
 *  loop-ops console skeleton: per-session cost today, oracle/gate columns
 *  next version. `cost_usd` is best-effort (see status.rs). */
export interface SessionCostRow {
  sid: string;
  project: string;
  role: string;
  vendor: string;
  status: string;
  cost_usd: number;
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

/** `GET /api/v1/status`. */
export function getStatus(): Promise<StatusSnapshot> {
  return getJson<StatusSnapshot>("/api/v1/status");
}

async function getJson<T>(url: string): Promise<T> {
  let res: Response;
  try {
    res = await fetch(url, {
      headers: { Accept: "application/json" },
      credentials: "same-origin",
    });
  } catch (e) {
    throw new Error(`network: ${e instanceof Error ? e.message : "connection failed"}`);
  }
  if (res.status === 401) throw new Error("UNAUTHENTICATED");
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  return (await res.json()) as T;
}
