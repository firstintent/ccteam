// v0.8.9 Phase 4 — top-bar cost pill (prototype `.cost`). Renders
// `今日 $X.XX / $Y` (cost_24h_usd / budget_cap_24h) from `GET /api/v1/status`;
// warn-colors (amber → red) as spend nears / passes the 24h budget cap.
// Clickable → navigates to `/status` (the detail view). Polls on a cheap
// interval + refreshes on window focus so the number stays fresh without an
// SSE channel.
//
// Theme tokens only (surface-*/brand-*/text-*/status-*). Carries the
// `data-testid="cost-pill"` the shell relied on so layout is stable.

import { useEffect, useState } from "react";
import { useNavigate } from "react-router-dom";
import { getStatus, type StatusSnapshot } from "../lib/statusApi";
import { budgetSeverity, formatUsd } from "../lib/marketplaceFormat";

/** Poll cadence for the pill — the aggregate is cheap + best-effort. */
const COST_POLL_MS = 20000;

export default function CostPill() {
  const navigate = useNavigate();
  const [snap, setSnap] = useState<StatusSnapshot | null>(null);

  useEffect(() => {
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | null = null;

    const fetchOnce = () => {
      getStatus()
        .then((s) => {
          if (!cancelled) setSnap(s);
        })
        .catch(() => {
          // Silent: a transient status failure shouldn't toast or blank the
          // pill. The 401 path is handled by the global gate; we keep the last
          // good value (or the dim em-dash placeholder).
        });
    };

    const schedule = () => {
      timer = setTimeout(() => {
        fetchOnce();
        schedule();
      }, COST_POLL_MS);
    };

    fetchOnce();
    schedule();
    const onFocus = () => fetchOnce();
    window.addEventListener("focus", onFocus);

    return () => {
      cancelled = true;
      if (timer) clearTimeout(timer);
      window.removeEventListener("focus", onFocus);
    };
  }, []);

  const severity = snap ? budgetSeverity(snap.cost_24h_usd, snap.budget_cap_24h) : "ok";
  const cap = snap?.budget_cap_24h ?? null;

  // Border/text severity color. `ok` = neutral dim; warn = amber; over = red.
  const tone =
    severity === "over"
      ? "border-status-error/60 text-status-error"
      : severity === "warn"
        ? "border-brand-500/60 text-brand-400"
        : "border-surface-700/60 text-text-dim";

  return (
    <button
      type="button"
      data-testid="cost-pill"
      onClick={() => navigate("/status")}
      title="今日花费 / 24h 预算（点开看 per-vendor）"
      className={`text-[11px] font-mono px-2.5 py-1 rounded-full bg-surface-800 border ${tone} hover:bg-surface-700 transition-colors cursor-pointer`}
    >
      今日{" "}
      <span className={severity === "ok" ? "text-text-secondary" : undefined}>
        {snap ? formatUsd(snap.cost_24h_usd) : "$—"}
      </span>
      {snap ? (cap !== null ? ` / ${formatUsd(cap)}` : "") : " / $—"}
    </button>
  );
}
