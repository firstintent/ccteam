// V0.3.2 F58 — token entry page (redesigned v0.9.x).
//
// Auth model: the server's `auth_layer` exposes a URL shim —
// `GET /?token=ccteam:<hex>` extracts the token, validates it constant-time,
// sets a persistent HttpOnly `ccteam_token` cookie (Max-Age = 7 days, see
// crates/ccteam-web/src/auth.rs), then 302-redirects to the same path minus
// the query param. So the submit handler simply navigates the browser to
// `/?token=ccteam:<hex>` and lets the server set the cookie; the SPA
// bootstrap then re-probes `/api/v1/auth/token` and the gate re-evaluates.
//
// The user pastes just the bare hex token (no prefix, no URL). `shapeToken`
// still tolerates a full `ccteam:<hex>` string or a pasted dashboard URL so a
// copy-paste of either keeps working, but the UI asks only for the hex.

import { useState, useRef, useEffect } from "react";
import { extractTokenFromQuery } from "../lib/token";
import { CcLogo } from "./Logo";

interface Props {
  /** Optional hook so the gate can clear local state right before the
   *  full-page nav fires. The handler will still always call
   *  `window.location.href = ...` — `onSubmit` is informational. */
  onSubmit?: () => void;
}

/** The CLIs ccteam orchestrates — shown as chips under the tagline. */
const VENDORS = ["Claude Code", "Codex", "Grok", "Kimi", "OpenCode"];

/** Brand gradient (matches the CcLogo mark). */
const BRAND_GRADIENT = "linear-gradient(135deg, #6D6AF6 0%, #22D3EE 100%)";

/** Normalise user input to a wire-format token (`ccteam:<hex>`).
 *  Accepts, most-to-least expected:
 *    - `<hex>`                    — the bare token the UI asks for
 *    - `ccteam:<hex>`             — taken as-is (tolerated)
 *    - a dashboard URL with ?token=ccteam:<hex> (tolerated)
 *
 *  Returns null on empty input. The server does the real validation; we just
 *  shape what we send. */
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
      setError("请粘贴访问令牌。");
      inputRef.current?.focus();
      return;
    }

    setLoading(true);
    setError(null);
    onSubmit?.();

    // Server's auth_layer validates the token, sets the persistent
    // ccteam_token cookie, and 302 → `/` (no token param). The browser then
    // reloads the SPA and `useAuthState` re-probes.
    window.location.href = `/?token=${encodeURIComponent(token)}`;
  };

  const canSubmit = value.trim().length > 0 && !loading;

  return (
    <div className="h-dvh flex items-center justify-center bg-surface-950 p-4 safe-area-inset relative overflow-hidden">
      {/* Ambient brand glow behind the card. */}
      <div
        aria-hidden
        className="pointer-events-none absolute -top-40 left-1/2 -translate-x-1/2 h-[28rem] w-[28rem] rounded-full opacity-20 blur-3xl"
        style={{ background: BRAND_GRADIENT }}
      />

      <div className="w-full max-w-sm animate-slide-up relative">
        <form
          onSubmit={handleSubmit}
          className="bg-surface-800/95 backdrop-blur border border-surface-700/50 rounded-2xl p-8 shadow-2xl"
        >
          {/* Brand mark + wordmark */}
          <div className="flex flex-col items-center mb-6">
            <CcLogo
              className="h-14 w-14 mb-3"
              title="ccteam"
            />
            <span className="font-semibold text-2xl text-text-primary tracking-tight">
              ccteam
            </span>
          </div>

          {/* Tagline */}
          <h1 className="text-center text-[15px] font-medium text-text-primary mb-1.5">
            管理你的多 Agent 团队
          </h1>
          <p className="text-center text-xs text-text-muted mb-4 leading-relaxed">
            一个控制台，统一调度下列编码 Agent
          </p>

          {/* Vendor chips */}
          <div className="flex flex-wrap items-center justify-center gap-1.5 mb-7">
            {VENDORS.map((v) => (
              <span
                key={v}
                className="text-[11px] px-2 py-0.5 rounded-full bg-surface-900 border border-surface-700/50 text-text-secondary font-mono whitespace-nowrap"
              >
                {v}
              </span>
            ))}
          </div>

          {/* Token input */}
          <div className="mb-4">
            <label
              htmlFor="ccteam-token"
              className="block text-xs text-text-muted mb-2 font-medium"
            >
              访问令牌
            </label>
            <input
              ref={inputRef}
              id="ccteam-token"
              type="text"
              value={value}
              onChange={(e) => {
                setValue(e.target.value);
                if (error) setError(null);
              }}
              disabled={loading}
              autoComplete="off"
              autoCapitalize="off"
              autoCorrect="off"
              spellCheck={false}
              className="w-full px-3.5 py-3 bg-surface-900 border border-surface-700/60 rounded-xl text-text-primary text-sm font-mono placeholder:text-text-dim focus:outline-none focus:ring-2 focus:ring-brand-600 focus:border-transparent disabled:opacity-50 transition-colors"
              placeholder="粘贴令牌（hex）"
            />
          </div>

          {error && (
            <p className="text-status-error text-xs mb-4">{error}</p>
          )}

          <button
            type="submit"
            disabled={!canSubmit}
            className="w-full py-3 rounded-xl text-white text-sm font-semibold transition-all disabled:cursor-not-allowed cursor-pointer flex items-center justify-center gap-2"
            style={{
              background: BRAND_GRADIENT,
              opacity: canSubmit ? 1 : 0.45,
              boxShadow: canSubmit
                ? "0 10px 30px -10px rgba(109,106,246,0.7)"
                : "none",
            }}
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
                连接中…
              </>
            ) : (
              "登录"
            )}
          </button>

          <p className="mt-5 text-center text-[11px] text-text-dim leading-relaxed">
            令牌由终端运行{" "}
            <code className="font-mono text-text-muted">ccteam web</code>{" "}
            时打印 · 登录状态保留 7 天
          </p>
        </form>
      </div>
    </div>
  );
}
