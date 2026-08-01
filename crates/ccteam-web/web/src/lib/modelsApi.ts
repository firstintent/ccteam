// The live per-vendor model + reasoning-effort catalog: `GET /api/v1/models`.
//
// Why a server catalog at all — the composer used to hard-code one global
// effort ladder (low/medium/high/max) and offer it to every vendor, which was
// a lie twice over: kimi has no `medium`, grok has no `max`, opencode declares
// no effort axis at all, and only claude had a real model list. The daemon
// observes what each installed CLI actually declares (ACP `availableModels`,
// the vendor handshake, `--help`) and reports it here, so the menu can offer
// exactly the vendor's own tokens and nothing else.
//
// Mirrors the `getJson` pattern every other `lib/*Api.ts` module keeps its own
// private copy of (see `agentsApi.ts`).

import { httpError } from "./httpError";

/** One model a vendor declares. `efforts` is the per-MODEL override when the
 *  vendor scopes reasoning effort to a model (kimi does); absent ⇒ the
 *  vendor-level {@link VendorModels.efforts} applies. */
export interface VendorModelEntry {
  id: string;
  display_name?: string | null;
  efforts?: string[] | null;
}

/** One vendor's observed catalog. `models` is empty when the daemon has never
 *  seen this vendor run (never observed ⇒ nothing honest to list); `efforts`
 *  then falls back to a pinned set server-side. */
export interface VendorModels {
  vendor: string;
  /** RFC3339 timestamp of the observation, or null when it came from a pin. */
  observed_at?: string | null;
  /** Provenance, e.g. `"ACP session availableModels"` — for the UI's title. */
  source?: string | null;
  models: VendorModelEntry[];
  efforts: string[];
}

export interface ModelsResponse {
  vendors: VendorModels[];
}

/** The catalog as the composer consumes it: vendor id → its raw tokens.
 *  Deliberately NOT the wire shape — `lib/vendors.ts` (pure, dependency-free)
 *  takes this structural view so it never imports a fetch module. A vendor
 *  missing from the map (or with an empty list) means "nothing observed" and
 *  the static registry answers instead. */
export type VendorCatalog = Record<string, { models: string[]; efforts: string[] }>;

async function getJson<T>(url: string): Promise<T> {
  const res = await fetch(url, {
    headers: { Accept: "application/json" },
    credentials: "same-origin",
  });
  if (res.status === 401) throw new Error("UNAUTHENTICATED");
  if (!res.ok) throw await httpError(res);
  return (await res.json()) as T;
}

/** Exported so callers share one template (and the test asserts one string). */
export const MODELS_URL = "/api/v1/models";

export function fetchModels(): Promise<ModelsResponse> {
  return getJson<ModelsResponse>(MODELS_URL);
}

/** Dedupe while preserving the vendor's own ordering (its first entry is its
 *  own preferred one — we never re-sort a vendor's list). */
function cleanTokens(raw: unknown): string[] {
  if (!Array.isArray(raw)) return [];
  const out: string[] = [];
  for (const item of raw) {
    if (typeof item !== "string") continue;
    const token = item.trim();
    if (token && !out.includes(token)) out.push(token);
  }
  return out;
}

/** Fold the response into the {@link VendorCatalog} the menus consume.
 *
 *  Total and defensive by design: this runs against a daemon that may be
 *  older than the SPA (the route 404s → the caller hands us nothing) or newer
 *  (extra fields). Anything unusable is dropped rather than thrown, because a
 *  malformed catalog must degrade to the static fallback, never to a blank
 *  composer menu. */
export function indexCatalog(res: ModelsResponse | null | undefined): VendorCatalog {
  const catalog: VendorCatalog = {};
  for (const entry of res?.vendors ?? []) {
    const vendor = typeof entry?.vendor === "string" ? entry.vendor.trim() : "";
    if (!vendor) continue;
    const models = cleanTokens((entry.models ?? []).map((m) => m?.id));
    catalog[vendor] = { models, efforts: cleanTokens(entry.efforts) };
  }
  return catalog;
}
