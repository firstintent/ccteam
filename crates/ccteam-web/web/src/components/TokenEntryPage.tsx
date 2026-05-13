// V0.3.2 F58 — token entry page.
//
// F53 inherited this from AoE where the submit handler stashed the
// token in localStorage and verified via a session-exempt endpoint.
// ccteam's auth model is different: the server's `auth_layer` exposes
// a URL-shim path — `GET /?token=ccteam:<hex>` extracts the token,
// validates it constant-time, sets a HttpOnly `ccteam_token` cookie,
// then 302-redirects to the same path minus the query param (see
// crates/ccteam-web/src/auth.rs).
//
// So instead of POSTing to a verify endpoint, we just navigate the
// browser to `/?token=ccteam:<hex>` and let the server set the cookie.
// On success the browser ends up back at `/` (or `/app/` under the
// SPA basename) with the cookie installed — the SPA bootstrap retries
// `/api/v1/auth/token` and the gate re-evaluates.
//
// The UI is kept from F53. The submit handler is the only thing
// rewritten. We also accept either a raw `ccteam:<hex>` token, a bare
// hex string, or a full dashboard URL containing `?token=...`.

import { useState, useRef, useEffect } from "react";
import { extractTokenFromQuery } from "../lib/token";

interface Props {
  /** Optional hook so the gate can clear local state right before the
   *  full-page nav fires. The handler will still always call
   *  `window.location.href = ...` — `onSubmit` is informational. */
  onSubmit?: () => void;
}

/** Normalise user input to a wire-format token (`ccteam:<hex>`).
 *  Accepts:
 *    - `ccteam:<hex>`             — taken as-is
 *    - `<hex>`                    — prefixed with `ccteam:`
 *    - a dashboard URL with ?token=ccteam:<hex>
 *
 *  Returns null on empty input. The server does the real validation;
 *  we just shape what we POST. */
function shapeToken(input: string): string | null {
  const raw = input.trim();
  if (!raw) return null;
  // URL form? Pull the token out.
  const fromUrl = extractTokenFromQuery(raw);
  const candidate = fromUrl ?? raw;
  if (candidate.startsWith("ccteam:")) return candidate;
  return `ccteam:${candidate}`;
}

export function TokenEntryPage({ onSubmit }: Props) {
  const [value, setValue] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    if (loading) return;
    const token = shapeToken(value);
    if (!token) {
      setError("Paste a token or dashboard URL.");
      inputRef.current?.focus();
      return;
    }

    setLoading(true);
    setError(null);
    onSubmit?.();

    // Server's auth_layer will: validate the token, set the
    // ccteam_token HttpOnly cookie, 302 → `/` (no token param). The
    // browser then reloads the SPA and `useAuthState` re-probes.
    //
    // We use the bare `/` path (not the SPA basename `/app/`) because
    // the URL shim is mounted on the whole router — once the cookie is
    // installed the user lands on the legacy HTML index, which itself
    // links to `/app/` if/when F59 swaps the default. For V0.3.2 the
    // SPA bootstrap is still reached via `/app/`, so callers can re-
    // navigate manually if needed.
    window.location.href = `/?token=${encodeURIComponent(token)}`;
  };

  return (
    <div className="h-dvh flex items-center justify-center bg-surface-900 p-4 safe-area-inset">
      <div className="w-full max-w-sm animate-slide-up">
        <form
          onSubmit={handleSubmit}
          className="bg-surface-800 border border-surface-700/40 rounded-xl p-8"
        >
          <div className="flex items-center justify-center gap-2 mb-6">
            <img
              src="/icon-192.png"
              alt=""
              width="28"
              height="28"
              className="rounded-sm"
            />
            <span className="font-mono text-lg text-text-primary tracking-tight">
              ccteam
            </span>
          </div>

          <p className="text-xs text-text-muted mb-6 text-center leading-relaxed">
            Your session is missing or unauthorised. Paste the token (or full
            dashboard URL) printed by{" "}
            <code className="text-brand-500 font-mono">ccteam web</code> to
            reconnect.
          </p>

          <div className="mb-4">
            <label
              htmlFor="ccteam-token"
              className="block text-xs text-text-muted mb-2 font-medium"
            >
              Token or URL
            </label>
            <input
              ref={inputRef}
              id="ccteam-token"
              type="text"
              value={value}
              onChange={(e) => setValue(e.target.value)}
              disabled={loading}
              autoComplete="off"
              spellCheck={false}
              className="w-full px-3 py-2.5 bg-surface-900 border border-surface-700/60 rounded-lg text-text-primary text-sm font-mono placeholder:text-text-dim focus:outline-none focus:ring-2 focus:ring-brand-600 focus:border-transparent disabled:opacity-50 transition-colors"
              placeholder="ccteam:<hex> or paste full URL"
            />
          </div>

          {error && (
            <p className="text-status-error text-xs mb-4">{error}</p>
          )}

          <button
            type="submit"
            disabled={loading || !value.trim()}
            className="w-full py-2.5 bg-brand-600 hover:bg-brand-700 text-white text-sm font-medium rounded-lg transition-colors disabled:opacity-50 disabled:cursor-not-allowed cursor-pointer flex items-center justify-center gap-2"
          >
            {loading ? (
              <>
                <svg
                  className="animate-spin h-4 w-4"
                  viewBox="0 0 24 24"
                  fill="none"
                >
                  <circle
                    className="opacity-25"
                    cx="12"
                    cy="12"
                    r="10"
                    stroke="currentColor"
                    strokeWidth="4"
                  />
                  <path
                    className="opacity-75"
                    fill="currentColor"
                    d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"
                  />
                </svg>
                Connecting...
              </>
            ) : (
              "Connect"
            )}
          </button>
        </form>
      </div>
    </div>
  );
}
