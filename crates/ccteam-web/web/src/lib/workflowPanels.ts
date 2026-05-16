// V0.4.6 F90 — typed fetch wrappers for the WorkflowView panel
// endpoints. Mirrors the four new Rust handlers in
// `crates/ccteam-web/src/routes/api_v1.rs`:
//
//   GET /api/v1/projects/<slug>/artifact_queue          → ArtifactQueueEntry[]
//   GET /api/v1/projects/<slug>/cost_history?window=... → CostHistoryResponse
//   GET /api/v1/projects/<slug>/sessions/active         → ActiveSessionInfo[]
//   GET /api/v1/projects/<slug>/jobs/<job_id>/log?tail= → JobLogResponse
//
// Types are 1:1 with the Rust `Serialize` impls. Errors propagate as
// plain `Error` instances; the SPA components surface the message in
// their own error states (no toast bus / global handler — the panels
// are read-only and a transient failure shouldn't disrupt the rest of
// the UI).

/** One watch-path entry. Matches Rust `ArtifactQueueEntry` 1:1. */
export interface ArtifactQueueEntry {
  /** workflow.yaml-relative watch path */
  path: string;
  /** Owning agent role */
  role: string;
  /** Number of files currently in the watch directory */
  file_count: number;
  /** Age of the oldest file in seconds, or null when dir is empty */
  oldest_age_seconds: number | null;
  /** Basename of the freshest file, or null when dir is empty */
  newest_filename: string | null;
}

/** One hour-bucket of cost. `hour` is RFC3339 UTC top-of-hour. */
export interface CostHistoryBucket {
  hour: string;
  cost_usd: number;
}

/** Response shape from `cost_history`. */
export interface CostHistoryResponse {
  window: "24h" | "7d" | string;
  buckets: CostHistoryBucket[];
}

/** One open agent_spawn with live state.json data. */
export interface ActiveSessionInfo {
  role: string;
  session_id: string;
  job_id: string | null;
  cwd: string | null;
  started_at: string;
  cost_usd: number;
}

/** Response shape from `jobs/<job_id>/log`. */
export interface JobLogResponse {
  job_id: string;
  total_lines: number;
  tail: string;
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

export function fetchArtifactQueue(slug: string): Promise<ArtifactQueueEntry[]> {
  return fetchJson<ArtifactQueueEntry[]>(
    `/api/v1/projects/${encodeURIComponent(slug)}/artifact_queue`,
  );
}

export function fetchCostHistory(
  slug: string,
  window: "24h" | "7d" = "24h",
): Promise<CostHistoryResponse> {
  return fetchJson<CostHistoryResponse>(
    `/api/v1/projects/${encodeURIComponent(slug)}/cost_history?window=${window}`,
  );
}

export function fetchActiveSessions(slug: string): Promise<ActiveSessionInfo[]> {
  return fetchJson<ActiveSessionInfo[]>(
    `/api/v1/projects/${encodeURIComponent(slug)}/sessions/active`,
  );
}

export function fetchJobLog(
  slug: string,
  jobId: string,
  tail = 200,
): Promise<JobLogResponse> {
  return fetchJson<JobLogResponse>(
    `/api/v1/projects/${encodeURIComponent(slug)}/jobs/${encodeURIComponent(
      jobId,
    )}/log?tail=${tail}`,
  );
}

// ---------- pure helpers used by panel rendering / tests ----------

/** Format a `seconds` duration into a compact human label. */
export function ageLabel(seconds: number | null | undefined): string {
  if (seconds == null || !isFinite(seconds) || seconds < 0) return "—";
  if (seconds < 60) return `${Math.round(seconds)}s`;
  const m = Math.floor(seconds / 60);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h`;
  return `${Math.floor(h / 24)}d`;
}

/** Last path component (so SPA cards display "team-d" not "/tmp/team-d"). */
export function basename(p: string | null | undefined): string {
  if (!p) return "—";
  const trimmed = p.replace(/\/+$/, "");
  if (!trimmed) return "—";
  const idx = trimmed.lastIndexOf("/");
  return idx === -1 ? trimmed : trimmed.slice(idx + 1);
}

/** Build a polyline `points=` string for SVG sparkline rendering.
 *  Domain = bucket index; range = cost normalized to [0, 1] of the
 *  global max. Returns `""` for an empty / all-zero series so the
 *  consumer can fall back to a flat baseline. */
export function sparklinePoints(
  buckets: CostHistoryBucket[],
  width: number,
  height: number,
): string {
  if (buckets.length === 0) return "";
  const max = Math.max(...buckets.map((b) => b.cost_usd), 0);
  if (max <= 0) {
    // All zeros — draw a flat baseline at the bottom.
    const baseline = height - 1;
    return buckets
      .map((_, i) => {
        const x = (i / Math.max(1, buckets.length - 1)) * width;
        return `${x.toFixed(2)},${baseline.toFixed(2)}`;
      })
      .join(" ");
  }
  return buckets
    .map((b, i) => {
      const x = (i / Math.max(1, buckets.length - 1)) * width;
      const y = height - (b.cost_usd / max) * height;
      return `${x.toFixed(2)},${y.toFixed(2)}`;
    })
    .join(" ");
}
