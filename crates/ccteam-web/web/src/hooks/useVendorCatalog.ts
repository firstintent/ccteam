// The live model + effort catalog (`GET /api/v1/models`), shared across every
// composer on screen via the ref-counted store engine.
//
// Why a refreshable store instead of a one-shot page cache: live-only vendors
// (`agy`, `codex`, `grok`, `kimi`, `opencode`, `pi`, `dsh`) do not appear in
// `~/.ccteam/model-catalog.json` until their first session handshakes. Refreshing
// on window focus / tab visibility, after creating a session, and when opening
// the model picker lets newly observed models show up without a manual reload,
// while deduplicating concurrent requests and never touching the network on
// keystrokes.
//
// Failure is not an error state here: a daemon older than this SPA 404s the
// route, and the honest degradation is "we know only what `lib/vendors.ts`
// knows statically", which an empty catalog expresses exactly.

import { useSyncExternalStore } from "react";

import { fetchModels, indexCatalog, type VendorCatalog } from "../lib/modelsApi";
import {
  createSharedResourceStore,
  type SharedResourceEnvironment,
  type SharedResourceSnapshot,
  type SharedResourceStore,
} from "./createSharedResourceStore";

const EMPTY: VendorCatalog = {};
const SERVER_SNAPSHOT: SharedResourceSnapshot<VendorCatalog> = {
  data: EMPTY,
  loading: false,
  error: null,
};

function isNotFoundError(error: unknown): boolean {
  return error instanceof Error && /^HTTP 404(?::|$)/.test(error.message);
}

export function createVendorCatalogStore(options: {
  fetcher?: (signal: AbortSignal) => Promise<VendorCatalog>;
  env?: SharedResourceEnvironment;
} = {}): SharedResourceStore<VendorCatalog> {
  const defaultFetcher = (signal: AbortSignal): Promise<VendorCatalog> =>
    fetchModels({ signal })
      .then(indexCatalog)
      .catch((error) => {
        // Older daemons without `/api/v1/models` return 404: degrade quietly
        // without spinning retry timers (focus / menu refresh still recovers
        // after an upgrade). Transient failures reject so the shared store
        // keeps the last good catalog and retries with backoff.
        if (isNotFoundError(error)) return EMPTY;
        throw error;
      });
  return createSharedResourceStore({
    fetcher: options.fetcher ?? defaultFetcher,
    // Slow-moving daemon observation: fetch on first subscriber, focus /
    // visibility, explicit refresh (menu open / vendor switch / post-spawn),
    // and retry after transient failure. No healthy-state polling interval.
    env: options.env,
  });
}

export const vendorCatalogStore = createVendorCatalogStore();

/**
 * The vendor catalog, or `{}` until it resolves (or when the route 404s on an
 * older daemon). Picker helpers fall back to the static registry per vendor,
 * while explicit values remain pass-through for adapter-side validation — so
 * an empty catalog is a complete working menu, never a spawn whitelist.
 */
export function useVendorCatalog(): VendorCatalog {
  const snapshot = useSyncExternalStore(
    vendorCatalogStore.subscribe,
    vendorCatalogStore.getSnapshot,
    () => SERVER_SNAPSHOT,
  );
  return snapshot.data ?? EMPTY;
}
