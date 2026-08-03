// The live model + effort catalog (`GET /api/v1/models`), fetched ONCE per
// page load and shared by every composer on screen.
//
// The cache is module-level, not per-component: Home and the conversation
// view each mount a composer, and the catalog is a slow-moving daemon-side
// observation — one request answers all of them, and re-rendering (every
// keystroke goes through the composer) must never touch the network.
//
// Failure is not an error state here: a daemon older than this SPA 404s the
// route, and the honest degradation is "we know only what `lib/vendors.ts`
// knows statically", which an empty catalog expresses exactly.

import { useEffect, useState } from "react";

import { fetchModels, indexCatalog, type VendorCatalog } from "../lib/modelsApi";

const EMPTY: VendorCatalog = {};

let inflight: Promise<VendorCatalog> | null = null;

function loadCatalog(): Promise<VendorCatalog> {
  inflight ??= fetchModels()
    .then(indexCatalog)
    .catch(() => EMPTY);
  return inflight;
}

/**
 * The vendor catalog, or `{}` until it resolves (and forever, if the route is
 * unavailable). Picker helpers fall back to the static registry per vendor,
 * while explicit values remain pass-through for adapter-side validation — so
 * an empty catalog is a complete working menu, never a spawn whitelist.
 */
export function useVendorCatalog(): VendorCatalog {
  const [catalog, setCatalog] = useState<VendorCatalog>(EMPTY);
  useEffect(() => {
    let cancelled = false;
    loadCatalog().then((loaded) => {
      if (!cancelled) setCatalog(loaded);
    });
    return () => {
      cancelled = true;
    };
  }, []);
  return catalog;
}
