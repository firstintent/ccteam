// v0.8.9 Phase 4 — REST client for the ccteam-hub plugin marketplace
// (`/api/v1/marketplace…`), the web marketplace browser surface.
//
// Backend SoT: `crates/ccteam-web/src/routes/marketplace.rs` (the network face
// of `ccteam_im::hub`). Four routes:
//   GET  /api/v1/marketplace?refresh                       → HubIndex (global catalog)
//   GET  /api/v1/marketplace/{id}/body                     → {id, body}  (markdown preview)
//   GET  /api/v1/projects/{slug}/marketplace?refresh       → DecoratedIndex (per-project installed_status)
//   POST /api/v1/projects/{slug}/marketplace/install       → 201 {id,type,path,overwrote}
//
// Auth: plain same-origin `fetch`; the global `fetchInterceptor` attaches
// `Authorization: Bearer <token>` automatically (see `sessionsApi`). Error
// mapping mirrors `sessionsApi`/`dashboardApi`:
//   401 → throw Error("UNAUTHENTICATED")  (global TokenEntryGate kicks in)
//   404 → throw Error("NOT_FOUND")
//   other non-2xx → lift the JSON `{error}` / `{ok:false,error}` envelope (so a
//                   409 already-installed surfaces "already exists at <path>"
//                   like dashboardApi.createProject), else `HTTP <status>`.

/** A plugin's install state in a target project (backend
 *  `ccteam_im::hub::InstalledStatus`, serde snake_case). */
export type InstalledStatus = "not_installed" | "installed" | "update_available";

/** One installable plugin entry in the hub index
 *  (`ccteam_im::hub::HubPlugin`). `type` is `agent | skill | workflow`. */
export interface HubPlugin {
  id: string;
  type: "agent" | "skill" | "workflow";
  name: string;
  description: string;
  path: string;
  content_sha: string;
  source: string;
  upstream: string;
  license: string;
  tags: string[];
}

/** A plugin decorated with its per-project installed status — the shape of
 *  each entry under `GET /api/v1/projects/{slug}/marketplace`. */
export interface DecoratedPlugin extends HubPlugin {
  installed_status: InstalledStatus;
}

/** The whole `index.json` catalog (`ccteam_im::hub::HubIndex`). */
export interface HubIndex {
  version: number;
  name: string;
  description: string;
  generated_at: string;
  plugins: HubPlugin[];
}

/** The per-project decorated catalog (each plugin carries `installed_status`). */
export interface DecoratedIndex extends Omit<HubIndex, "plugins"> {
  plugins: DecoratedPlugin[];
}

/** Plugin body preview payload (`GET /marketplace/{id}/body`). */
export interface PluginBody {
  id: string;
  body: string;
}

/** Install outcome (`201` from the install POST). */
export interface InstallResult {
  id: string;
  type: string;
  path: string;
  overwrote: boolean;
}

// ---- URL builders (exported for unit tests) -------------------------------

/** `GET /api/v1/marketplace` (optionally `?refresh=true`). */
export function marketplaceUrl(refresh = false): string {
  return refresh ? "/api/v1/marketplace?refresh=true" : "/api/v1/marketplace";
}

/** `GET /api/v1/marketplace/{id}/body`. */
export function marketplaceBodyUrl(id: string): string {
  return `/api/v1/marketplace/${encodeURIComponent(id)}/body`;
}

/** `GET /api/v1/projects/{slug}/marketplace` (optionally `?refresh=true`). */
export function projectMarketplaceUrl(slug: string, refresh = false): string {
  const base = `/api/v1/projects/${encodeURIComponent(slug)}/marketplace`;
  return refresh ? `${base}?refresh=true` : base;
}

/** `POST /api/v1/projects/{slug}/marketplace/install`. */
export function projectInstallUrl(slug: string): string {
  return `/api/v1/projects/${encodeURIComponent(slug)}/marketplace/install`;
}

// ---- shared fetch helpers --------------------------------------------------

/** Lift a JSON error envelope (`{error}` or `{ok:false,error}`) to a string,
 *  falling back to `HTTP <status>` when the body is missing / unparseable.
 *  This is what makes a 409 surface "already installed at <path>" instead of a
 *  bare status (mirrors dashboardApi.createProject). */
async function liftError(res: Response): Promise<string> {
  let detail = `HTTP ${res.status}`;
  try {
    const data = (await res.json()) as { error?: unknown };
    if (data && typeof data.error === "string" && data.error.length > 0) {
      detail = data.error;
    }
  } catch {
    // keep the status-code fallback
  }
  return detail;
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
  if (res.status === 404) throw new Error("NOT_FOUND");
  if (!res.ok) throw new Error(await liftError(res));
  return (await res.json()) as T;
}

// ---- API ------------------------------------------------------------------

/** `GET /api/v1/marketplace` — the global hub catalog. `refresh` bypasses the
 *  `~/.ccteam/hub-cache/` and re-fetches the index. A hub fetch / parse
 *  failure is a 502 (lifted to its `{error}` message). */
export function getMarketplace(refresh = false): Promise<HubIndex> {
  return getJson<HubIndex>(marketplaceUrl(refresh));
}

/** `GET /api/v1/projects/{slug}/marketplace` — the catalog decorated with each
 *  plugin's per-project `installed_status`. 404 (unknown project) →
 *  NOT_FOUND. */
export function getProjectMarketplace(
  slug: string,
  refresh = false,
): Promise<DecoratedIndex> {
  return getJson<DecoratedIndex>(projectMarketplaceUrl(slug, refresh));
}

/** `GET /api/v1/marketplace/{id}/body` — the plugin markdown body, for the
 *  install-time review drawer. Unknown id → NOT_FOUND, integrity/transport
 *  failure → the lifted 502 message. */
export function getPluginBody(id: string): Promise<PluginBody> {
  return getJson<PluginBody>(marketplaceBodyUrl(id));
}

/** `POST /api/v1/projects/{slug}/marketplace/install` — install `id` into the
 *  project. 201 `{id,type,path,overwrote}`. Errors carry the lifted envelope:
 *  400 (bad/unsupported type), 404 (unknown project/plugin → NOT_FOUND), 409
 *  (already installed — message names the target path), 500 (local write), 502
 *  (upstream hub). Pass `force` to overwrite an existing file. */
export function installPlugin(
  slug: string,
  id: string,
  force = false,
): Promise<InstallResult> {
  const body: Record<string, unknown> = { id };
  if (force) body.force = true;
  return postInstall(projectInstallUrl(slug), body);
}

async function postInstall(url: string, body: unknown): Promise<InstallResult> {
  let res: Response;
  try {
    res = await fetch(url, {
      method: "POST",
      credentials: "same-origin",
      headers: { "Content-Type": "application/json", Accept: "application/json" },
      body: JSON.stringify(body),
    });
  } catch (e) {
    throw new Error(`network: ${e instanceof Error ? e.message : "connection failed"}`);
  }
  if (res.status === 401) throw new Error("UNAUTHENTICATED");
  if (res.status === 404) throw new Error("NOT_FOUND");
  if (!res.ok) throw new Error(await liftError(res));
  return (await res.json()) as InstallResult;
}
