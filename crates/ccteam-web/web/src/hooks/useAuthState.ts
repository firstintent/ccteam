// V0.3.2 F58 — auth-state hook backing TokenEntryGate.
//
// Probes `GET /api/v1/auth/token` once on mount to learn whether the
// server requires auth (response `{wire_token: string|null}`). Also
// subscribes to `TOKEN_EXPIRED_EVENT` from the global fetchInterceptor
// — when a 401 lands on any /api/* call, the dispatcher fires that
// event and we flip a flag the gate consumes to swap the UI to
// TokenEntryPage.
//
// Tri-state for `wireToken`:
//   - undefined → bootstrap probe in flight
//   - null      → backend says no auth needed (loopback default)
//   - string    → "ccteam:<hex>" — auth required
//
// `authRequired` is just `wireToken !== null` (treats `undefined` as
// not-yet-known but also not-required so the SPA renders during the
// bootstrap; the first 401 will flip `saw401` and bring the gate up).

import { useCallback, useEffect, useState } from "react";
import { fetchAuthToken } from "../lib/api";
import {
  TOKEN_EXPIRED_EVENT,
  resetTokenExpired,
} from "../lib/fetchInterceptor";
import { clearToken } from "../lib/token";

export interface AuthState {
  /** `undefined` while bootstrap probe is in flight, `null` when auth
   *  is disabled, `"ccteam:<hex>"` when required. */
  wireToken: string | null | undefined;
  /** Convenience: true once we know the server requires a token. */
  authRequired: boolean;
  /** True after the global fetch interceptor reports a 401 on any
   *  /api/* call. The gate uses this to swap to TokenEntryPage. */
  saw401: boolean;
  /** Log the user out: nuke localStorage token + the cookie, reset
   *  the 401 dedup flags, and reload so the next bootstrap re-probes
   *  `/api/v1/auth/token` cleanly. */
  clearAuth: () => void;
}

export function useAuthState(): AuthState {
  const [wireToken, setWireToken] = useState<string | null | undefined>(undefined);
  const [saw401, setSaw401] = useState(false);

  useEffect(() => {
    let cancelled = false;
    fetchAuthToken().then((res) => {
      if (cancelled) return;
      // On network failure (`res === null`) we leave `wireToken` as
      // undefined; the SPA renders, and a subsequent /api/* 401 (which
      // will happen if auth is required) trips `saw401` and the gate
      // shows TokenEntryPage anyway.
      if (res) setWireToken(res.wire_token);
    });
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    function onTokenExpired() {
      setSaw401(true);
    }
    window.addEventListener(TOKEN_EXPIRED_EVENT, onTokenExpired);
    return () => {
      window.removeEventListener(TOKEN_EXPIRED_EVENT, onTokenExpired);
    };
  }, []);

  const clearAuth = useCallback(() => {
    clearToken();
    // Best-effort cookie nuke. The HttpOnly `ccteam_token` cookie set
    // by the server's URL shim is technically JS-invisible, but the
    // browser still treats a same-name max-age=0 cookie set from the
    // page as a delete for the visible cookie jar; if the HttpOnly
    // flavour was set it'll be cleared by the post-reload bootstrap
    // failing auth + showing TokenEntryPage anyway.
    if (typeof document !== "undefined") {
      document.cookie = "ccteam_token=; path=/; max-age=0";
    }
    resetTokenExpired();
    setSaw401(false);
    if (typeof window !== "undefined") {
      window.location.reload();
    }
  }, []);

  return {
    wireToken,
    authRequired: wireToken !== null && wireToken !== undefined,
    saw401,
    clearAuth,
  };
}
