// Per-session statusline formatter — a pure (dependency-free, node-testable)
// helper that turns a `SessionStatus` into the single display string the
// SessionView bar renders. No React / DOM here so it can be unit-tested
// directly (mirrors `marketplaceFormat.ts`).
//
// The server already renders the line (`SessionStatus.status_line`, =
// `ThreadStatus::status_suffix()` on the backend, e.g.
// `"claude-opus-4-8[1m] · ctx 188k / 1M (19%)"`) — prefer it verbatim. The
// structured `model` / `context` fields are a fallback when an older daemon
// (or a partial payload) gives us numbers but no pre-rendered line.

import type { SessionContext, SessionStatus, TurnStatus } from "./sessionsApi";

/** Humanize a token count the way the backend does: `1_000_000 → "1M"`,
 *  `200_000 → "200k"`, `188_000 → "188k"`. Whole millions render as `<n>M`;
 *  everything ≥1000 renders as `<n>k` (rounded to a whole k); below 1000 the
 *  raw count. Negative / non-finite clamp to `"0"`. */
export function humanizeTokens(n: number): string {
  if (!Number.isFinite(n) || n <= 0) return "0";
  if (n >= 1_000_000 && n % 1_000_000 === 0) return `${n / 1_000_000}M`;
  if (n >= 1000) return `${Math.round(n / 1000)}k`;
  return `${Math.round(n)}`;
}

/** Build the `ctx <used> / <window> (<pct>%)` fragment from structured
 *  context numbers, mirroring the backend's `ContextUsage::render()`.
 *  A null `used_tokens` means nobody reports occupancy — render the dash
 *  form (`ctx — / 500k (usage unknown)`) rather than a fabricated `0 (0%)`. */
export function formatContext(context: SessionContext): string {
  const window = humanizeTokens(context.window_tokens);
  if (typeof context.used_tokens !== "number") {
    return context.window_tokens > 0
      ? `ctx — / ${window} (usage unknown)`
      : "ctx —";
  }
  const used = humanizeTokens(context.used_tokens);
  if (context.window_tokens <= 0) return `ctx ${used} (window unknown)`;
  const pct = Number.isFinite(context.pct as number)
    ? Math.round(context.pct as number)
    : 0;
  return `ctx ${used} / ${window} (${pct}%)`;
}

/** The display line for a session's statusline bar, or `null` to render
 *  nothing. Prefers the server-rendered `status_line` verbatim when it is a
 *  non-empty string; else falls back to `<model> · ctx …` built from the
 *  structured `model` / `context` fields (joining with " · " only the parts we
 *  have); else (a brand-new session: all null) returns `null`. */
export function formatStatusLine(status: SessionStatus): string | null {
  if (typeof status.status_line === "string" && status.status_line.trim()) {
    return status.status_line;
  }
  const parts: string[] = [];
  if (typeof status.model === "string" && status.model.trim()) {
    parts.push(status.model);
  }
  if (status.context) {
    parts.push(formatContext(status.context));
  }
  return parts.length > 0 ? parts.join(" · ") : null;
}

export function formatTurnStatus(status?: TurnStatus): { text: string; warn: boolean } | null {
  if (!status) return null;
  // The metrics half of the bubble footer (`turn N · ctx N%`); SessionView
  // prefixes `vendor · sid · model` — every fact once, the ledger stays on the
  // session pages.
  const parts: string[] = [`turn ${status.turn}`];
  const pct = contextPct(status.context);
  const roundedPct = typeof pct === "number" && Number.isFinite(pct) ? Math.round(pct) : null;
  if (roundedPct !== null) parts.push(`ctx ${roundedPct}%${roundedPct >= 85 ? "⚠" : ""}`);
  return { text: parts.join(" · "), warn: roundedPct !== null && roundedPct >= 85 };
}

/** TurnStatus serializes ContextUsage's numerator/denominator; derive pct
 * locally because the Rust `pct()` helper is intentionally not a stored field. */
export function contextPct(context?: TurnStatus["context"]): number | null {
  if (!context || context.used_tokens == null || context.window_tokens <= 0) return null;
  return (context.used_tokens / context.window_tokens) * 100;
}

/** `m:ss` (or `h:mm:ss` past an hour) for the busy heartbeat — how long the
 *  current turn has been running (GitHub #186 B). Negative or
 *  non-finite input renders as `0:00`. */
export function formatElapsed(ms: number): string {
  const total = Number.isFinite(ms) ? Math.max(0, Math.floor(ms / 1000)) : 0;
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const mm = h > 0 ? String(m).padStart(2, "0") : String(m);
  return `${h > 0 ? `${h}:` : ""}${mm}:${String(s).padStart(2, "0")}`;
}
