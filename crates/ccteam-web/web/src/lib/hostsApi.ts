// v0.8.18 柱1 — REST client for the host-keyed agent report
// (`GET /api/v1/hosts` + `/{host}` + `POST .../register-mcp`).
//
// Backend SoT: `crates/ccteam-web/src/routes/hosts.rs`. Auth + error mapping
// mirror `statusApi` / `sessionsApi`:
//   401 → throw Error("UNAUTHENTICATED")  (global TokenEntryGate kicks in)
//   other non-2xx → throw Error("HTTP <status>")

/** One agent vendor's health on a host (`AgentHealth`). */
export interface AgentHealth {
  vendor: string;
  harness_id: string;
  installed: boolean;
  version: string | null;
  bin: string;
  mcp_registered: boolean;
  /** Whether config-file MCP registration applies to this vendor at all
   *  (`false` for grok/ACP — MCP rides the session protocol). Gates the
   *  register CTA. */
  mcp_registrable: boolean;
  /** `ready` | `needs_config` | `not_installed`. */
  status: string;
  /** Copy-paste remediation when not ready; null when ready. */
  hint: string | null;
}

/** Collection row for `GET /api/v1/hosts` (`HostSummary`). */
export interface HostSummary {
  host: string;
  hostname: string;
  is_local: boolean;
  agent_count: number;
  agents_ready: number;
}

/** `GET /api/v1/hosts` response (`HostsResponse`). */
export interface HostsResponse {
  hosts: HostSummary[];
}

/** `GET /api/v1/hosts/{host}` detail (`HostDetail`). */
export interface HostDetail {
  host: string;
  hostname: string;
  is_local: boolean;
  os: string;
  arch: string;
  ccteam_version: string;
  agents: AgentHealth[];
}

/** `POST .../register-mcp` response. */
export interface RegisterMcpResult {
  registered: string[];
  paths: Record<string, string>;
}

/** `GET /api/v1/hosts` — list every host (today just `local`). */
export function getHosts(): Promise<HostsResponse> {
  return getJson<HostsResponse>("/api/v1/hosts");
}

/** `GET /api/v1/hosts/{host}` — one host's full agent report. `refresh` forces
 *  a re-probe (bypasses the daemon-lifetime cache). */
export function getHostDetail(host: string, refresh = false): Promise<HostDetail> {
  const q = refresh ? "?refresh=true" : "";
  return getJson<HostDetail>(`/api/v1/hosts/${encodeURIComponent(host)}${q}`);
}

/** `POST /api/v1/hosts/{host}/register-mcp` — register ccteam's own MCP server
 *  into the vendor config(s). Idempotent. `vendor` omitted ⇒ register all. */
export function registerMcp(host: string, vendor?: string): Promise<RegisterMcpResult> {
  const q = vendor ? `?vendor=${encodeURIComponent(vendor)}` : "";
  return postJson<RegisterMcpResult>(`/api/v1/hosts/${encodeURIComponent(host)}/register-mcp${q}`);
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

async function postJson<T>(url: string): Promise<T> {
  let res: Response;
  try {
    res = await fetch(url, {
      method: "POST",
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
