// v0.8.18 档1 — fetch the caller's identity once, so the shell can keep user
// management and global IM credentials owner-only.

import { useEffect, useState } from "react";

import { getMe, type Me } from "../lib/meApi";

/**
 * Fetch `/api/v1/me` once. `isAdmin` is **fail-closed**: it stays `false`
 * until the identity loads (and on any error), so user management and global
 * IM credentials never flash to a tenant.
 */
export function useMe(): { me: Me | null; isAdmin: boolean } {
  const [me, setMe] = useState<Me | null>(null);
  useEffect(() => {
    let cancelled = false;
    getMe()
      .then((m) => {
        if (!cancelled) setMe(m);
      })
      .catch(() => {
        // UNAUTHENTICATED is handled by the global token gate; any other error
        // leaves `me` null → fail-closed (no admin surfaces).
      });
    return () => {
      cancelled = true;
    };
  }, []);
  return { me, isAdmin: me?.is_admin ?? false };
}
