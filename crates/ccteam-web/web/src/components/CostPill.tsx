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

  return <CostPillButton snap={snap} onOpenStatus={() => navigate("/status")} />;
}

export function CostPillButton({
  snap,
  onOpenStatus,
}: {
  snap: StatusSnapshot | null;
  onOpenStatus: () => void;
}) {
  const severity = snap ? budgetSeverity(snap.cost_24h_usd, snap.budget_cap_24h) : "ok";
  const cap = snap?.budget_cap_24h ?? null;

  // Prototype `.cost-pill` (green pill); warn/over override the text tone.
  const tone =
    severity === "over"
      ? { color: "var(--red-text)", background: "var(--red-soft)" }
      : severity === "warn"
        ? { color: "#B45309", background: "#FDF3E1" }
        : undefined;

  return (
    <button
      type="button"
      data-testid="cost-pill"
      onClick={onOpenStatus}
      title="今日花费 / 24h 预算（点开看 per-vendor）"
      className="cost-pill"
      style={{ ...tone, cursor: "pointer" }}
    >
      今日 {snap ? formatUsd(snap.cost_24h_usd) : "$—"}
      {snap ? (cap !== null ? ` / ${formatUsd(cap)}` : "") : " / $—"}
    </button>
  );
}
