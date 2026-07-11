// v0.8.24 A3 — Workflow APIs: compare fan-out + evolution read panel.

export interface CompareSlot {
  vendor: string;
  sid: string;
  answer: string;
  cost_usd: number | null;
  status: string;
  error?: string | null;
}

export interface CompareResult {
  compare_group: string;
  prompt: string;
  slots: CompareSlot[];
  cost_subtotal_usd: number | null;
  timeout_secs: number;
}

export interface EvolutionBucket {
  kind: string;
  id: string;
  sha: string;
  turn_count: number;
  avg_cost_usd?: number | null;
  total_cost_usd?: number | null;
}

export interface EvolutionSummary {
  slug: string;
  turn_records: number;
  verdict_records: number;
  /** v0.8.24 — turn records written in the last 7 days (trend stat). */
  turn_records_7d: number;
  roles: EvolutionBucket[];
  skills: EvolutionBucket[];
  empty: boolean;
}

// ── compare history (v0.8.24) ────────────────────────────────────────────────

export interface CompareHistoryMember {
  sid: string;
  vendor: string;
  cost_usd?: number | null;
  title?: string | null;
}

export interface CompareHistoryGroup {
  group: string;
  created_at: string;
  prompt: string;
  members: CompareHistoryMember[];
  cost_subtotal_usd?: number | null;
}

export interface CompareHistoryResponse {
  groups: CompareHistoryGroup[];
}

// ── project MCP servers (v0.8.24 F1.12) ──────────────────────────────────────

export interface McpServerView {
  name: string;
  /** `stdio` | `http` | `sse` | `unknown`. */
  kind: string;
  command?: string | null;
  args?: string[] | null;
  url?: string | null;
  /** Env var NAMES only — values never echo. */
  env_keys?: string[] | null;
  is_ccteam: boolean;
}

export interface McpServersResponse {
  servers: McpServerView[];
  ccteam_registered: boolean;
}

/** POST body: exactly one of `url` (http/sse) or `command` (stdio). */
export interface RegisterMcpServerForm {
  name: string;
  url?: string;
  command?: string;
  args?: string[];
}

export function compareUrl(slug: string): string {
  return `/api/v1/projects/${encodeURIComponent(slug)}/compare`;
}

export function evolutionUrl(slug: string): string {
  return `/api/v1/projects/${encodeURIComponent(slug)}/evolution`;
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

async function postJson<T>(url: string, body: unknown): Promise<T> {
  const res = await fetch(url, {
    method: "POST",
    credentials: "same-origin",
    headers: { "Content-Type": "application/json", Accept: "application/json" },
    body: JSON.stringify(body),
  });
  if (res.status === 401) throw new Error("UNAUTHENTICATED");
  if (!res.ok) {
    let msg = `HTTP ${res.status}`;
    try {
      const j = (await res.json()) as { error?: string };
      if (j.error) msg = j.error;
    } catch {
      /* ignore */
    }
    throw new Error(msg);
  }
  return (await res.json()) as T;
}

export function runCompare(
  slug: string,
  prompt: string,
  vendors?: string[],
): Promise<CompareResult> {
  const body: Record<string, unknown> = { prompt };
  if (vendors && vendors.length > 0) body.vendors = vendors;
  return postJson<CompareResult>(compareUrl(slug), body);
}

export function getEvolution(slug: string): Promise<EvolutionSummary> {
  return getJson<EvolutionSummary>(evolutionUrl(slug));
}

/** `GET .../compare/history` — past compare groups (newest first). */
export function getCompareHistory(slug: string): Promise<CompareHistoryResponse> {
  return getJson<CompareHistoryResponse>(`${compareUrl(slug)}/history`);
}

export function mcpServersUrl(slug: string): string {
  return `/api/v1/projects/${encodeURIComponent(slug)}/mcp-servers`;
}

/** `GET .../mcp-servers` — the project `.mcp.json` (masked view). */
export function getMcpServers(slug: string): Promise<McpServersResponse> {
  return getJson<McpServersResponse>(mcpServersUrl(slug));
}

/** `POST .../mcp-servers` — idempotently merge one third-party server into
 *  the project `.mcp.json` (admin-only; config write, never executes). */
export function registerMcpServer(
  slug: string,
  form: RegisterMcpServerForm,
): Promise<{ ok: boolean; name: string; path: string }> {
  return postJson(mcpServersUrl(slug), form);
}
