// v0.8.18 柱1 — REST client for the host-keyed agent report
// (`GET /api/v1/hosts` + `/{host}` + `POST .../register-mcp` +
// `DELETE /{host}`, the last added TEAM-5/TEAM-6 for satellite removal).
//
// Backend SoT: `crates/ccteam-web/src/routes/hosts.rs`. Auth + error mapping
// mirror `statusApi` / `sessionsApi`:
//   401 → throw Error("UNAUTHENTICATED")  (global TokenEntryGate kicks in)
//   other non-2xx → throw Error("HTTP <status>")

import { httpError } from "./httpError";

/** One agent vendor's health on a host (`AgentHealth`). */
export interface AgentHealth {
  vendor: string;
  harness_id: string;
  installed: boolean;
  version: string | null;
  bin: string;
  mcp_registered: boolean;
  /** How ccteam exposes its tools to this vendor. */
  tool_surface: "native_mcp_config" | "managed_session_bridge" | "unknown";
  /** Honest explanation for managed-session-only tool surfaces. */
  tool_surface_note?: string | null;
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
  /** `online` | `offline` (satellite heartbeat freshness; local always online). */
  status?: string;
  agent_count: number;
  agents_ready: number;
  /** Unix seconds of the last satellite heartbeat — the age anchor behind the
   *  roster's "已离线 N 天" hint. Absent for `local` (always online). */
  last_heartbeat_unix?: number;
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
  /** Projects registered on THIS host (local: the daemon registry; satellite:
   *  its own `~/.ccteam` registry as reported at heartbeat). A remote spawn
   *  is only possible into a slug listed here. */
  projects?: HostProjectView[];
}

export interface HostProjectView {
  slug: string;
  path: string;
  cataloged: boolean;
  catalog_slug: string | null;
}

/** `POST .../register-mcp` response. */
export interface RegisterMcpResult {
  registered: string[];
  paths: Record<string, string>;
}

/** `GET`/`POST /api/v1/hosts/join-token` response. `token` is
 *  null when none has been minted / all are spent (GET only). */
export interface JoinTokenInfo {
  token: string | null;
  label?: string | null;
  minted_at?: string;
  max_uses?: number | null;
  uses?: number;
  command?: string;
}

/** `GET /api/v1/hosts/join-token` — newest still-valid join token (or
 *  `{token: null}`). */
export function getJoinToken(): Promise<JoinTokenInfo> {
  return getJson<JoinTokenInfo>("/api/v1/hosts/join-token");
}

/** `POST /api/v1/hosts/join-token` — mint a fresh join token. */
export function mintJoinToken(label?: string): Promise<JoinTokenInfo> {
  return postJson<JoinTokenInfo>("/api/v1/hosts/join-token", { label: label ?? null });
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

/** `DELETE /api/v1/hosts/{host}` — deregister a satellite (`routes/hosts.rs`
 *  `handle_host_remove`, TEAM-5). `local` is refused 400 server-side; an
 *  ONLINE host is refused 409 unless `force: true` (an offline host never
 *  needs it — the backend's own online check makes passing it harmless
 *  either way). Returns `{host}` on success; non-2xx surfaces the server's
 *  `{error}` body text when present (so a 409/404/400 reads as the real
 *  reason, not just `HTTP 409`). */
export function deleteHost(host: string, opts?: { force?: boolean }): Promise<{ host: string }> {
  const q = opts?.force ? "?force=true" : "";
  return deleteJson<{ host: string }>(`/api/v1/hosts/${encodeURIComponent(host)}${q}`);
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
  if (!res.ok) throw await httpError(res);
  return (await res.json()) as T;
}

async function deleteJson<T>(url: string): Promise<T> {
  let res: Response;
  try {
    res = await fetch(url, {
      method: "DELETE",
      headers: { Accept: "application/json" },
      credentials: "same-origin",
    });
  } catch (e) {
    throw new Error(`network: ${e instanceof Error ? e.message : "connection failed"}`);
  }
  if (res.status === 401) throw new Error("UNAUTHENTICATED");
  if (!res.ok) throw await httpError(res);
  return (await res.json()) as T;
}

async function postJson<T>(url: string, body?: unknown): Promise<T> {
  let res: Response;
  try {
    res = await fetch(url, {
      method: "POST",
      headers:
        body === undefined
          ? { Accept: "application/json" }
          : { Accept: "application/json", "Content-Type": "application/json" },
      body: body === undefined ? undefined : JSON.stringify(body),
      credentials: "same-origin",
    });
  } catch (e) {
    throw new Error(`network: ${e instanceof Error ? e.message : "connection failed"}`);
  }
  if (res.status === 401) throw new Error("UNAUTHENTICATED");
  if (!res.ok) throw await httpError(res);
  return (await res.json()) as T;
}
