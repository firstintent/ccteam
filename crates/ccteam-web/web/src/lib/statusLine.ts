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
  const parts: string[] = [];
  const pct = contextPct(status.context);
  const roundedPct = typeof pct === "number" && Number.isFinite(pct) ? Math.round(pct) : null;
  if (roundedPct !== null) parts.push(`ctx ${roundedPct}%${roundedPct >= 85 ? "⚠" : ""}`);
  if (typeof status.model === "string" && status.model.trim()) parts.push(status.model);
  parts.push(`turn ${status.turn}`);
  if (
    typeof status.cost_usd === "number" &&
    Number.isFinite(status.cost_usd) &&
    status.cost_usd >= 0
  ) {
    parts.push(status.cost_usd > 0 && status.cost_usd < 0.005 ? "$<0.01" : `$${status.cost_usd.toFixed(2)}`);
  } else if (
    typeof status.tokens_total === "number" &&
    Number.isFinite(status.tokens_total) &&
    status.tokens_total > 0
  ) {
    // Mirrors the Rust renderer: a zero ledger is unknown (omitted); ≥1M reads in M.
    parts.push(
      status.tokens_total >= 1_000_000
        ? `${(status.tokens_total / 1_000_000).toFixed(1)}M tok`
        : `${(status.tokens_total / 1000).toFixed(1)}k tok`,
    );
  }
  return { text: parts.join(" · "), warn: roundedPct !== null && roundedPct >= 85 };
}

/** TurnStatus serializes ContextUsage's numerator/denominator; derive pct
 * locally because the Rust `pct()` helper is intentionally not a stored field. */
export function contextPct(context?: TurnStatus["context"]): number | null {
  if (!context || context.used_tokens == null || context.window_tokens <= 0) return null;
  return (context.used_tokens / context.window_tokens) * 100;
}
