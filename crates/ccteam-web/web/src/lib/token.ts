// Persists the auth token across iOS PWA launches.
//
// iOS manifests use `start_url` when launching from the home screen, which
// strips any `?token=...` that was on the URL when the user tapped "Add to
// Home Screen". Cookies may also be lost across the Safari→standalone
// context switch. localStorage survives both, so we stash the token there
// and send it via `Authorization: Bearer` on every request.
//
// Trade-off: localStorage is readable by any JS running on this origin,
// which widens XSS blast radius versus HttpOnly cookies. Accepted because
// the dashboard is a small self-hosted app with a minimal dependency surface
// and the PWA flow otherwise doesn't work at all on iOS. If we ever add a
// rich plugin system or user-generated content to the dashboard, revisit.

const STORAGE_KEY = "aoe_auth_token";

/** Pure parser: pull `?token=...` out of an arbitrary URL string. Returns
 *  null when absent. Doesn't touch localStorage or history. Exposed for
 *  TokenEntryPage (V0.3.2 F58) so it can accept either a raw token or a
 *  pasted dashboard URL without re-implementing this logic. */
export function extractTokenFromQuery(href?: string): string | null {
  const input =
    href ?? (typeof window !== "undefined" && window.location ? window.location.href : "");
  if (!input) return null;
  try {
    const url = new URL(input);
    const token = url.searchParams.get("token");
    return token && token.length > 0 ? token : null;
  } catch {
    return null;
  }
}

function captureFromUrl(): void {
  // Runs at module load — must not throw on a partial environment (the
  // node-env vitest suite stubs a minimal `window` without `location`).
  if (typeof window === "undefined" || !window.location) return;
  const token = extractTokenFromQuery(window.location.href);
  if (!token) return;

  try {
    window.localStorage.setItem(STORAGE_KEY, token);
  } catch {
    // Private mode / storage disabled: fall back to the token staying in the
    // URL and cookie for this session only. Nothing else to do.
    return;
  }

  const url = new URL(window.location.href);
  url.searchParams.delete("token");
  const clean = url.pathname + (url.search ? url.search : "") + url.hash;
  window.history.replaceState(null, "", clean || "/");
}

captureFromUrl();

export function getToken(): string | null {
  try {
    return window.localStorage.getItem(STORAGE_KEY);
  } catch {
    return null;
  }
}

// Called when the server sends X-Aoe-Token on a response, indicating the
// auth token has been rotated. Keeps the PWA in sync without a page reload.
export function saveToken(token: string): void {
  const trimmed = token.trim();
  if (!trimmed) return;
  try {
    window.localStorage.setItem(STORAGE_KEY, trimmed);
  } catch {
    // Private mode or quota exceeded: nothing to do. The request that
    // prompted this save still succeeded on its cookie/header, so the
    // user isn't locked out until the next session.
  }
}

export function clearToken(): void {
  try {
    window.localStorage.removeItem(STORAGE_KEY);
  } catch {
    // nothing to do
  }
}
