// v0.8.9 Phase 4 — top-bar cost pill (prototype `.cost`). Renders
// `今日 $X.XX / $Y` (cost_24h_usd / budget_cap_24h) from `GET /api/v1/status`;
// warn-colors (amber → red) as spend nears / passes the 24h budget cap.
// Clickable → navigates to `/status` (the detail view). Polls on a cheap
// interval + refreshes on window focus so the number stays fresh without an
// SSE channel.
//
// Theme tokens only (surface-*/brand-*/text-*/status-*). Carries the
// `data-testid="cost-pill"` the shell relied on so layout is stable.

import { useNavigate } from "react-router-dom";
import type { StatusSnapshot } from "../lib/statusApi";
import { budgetSeverity, formatUsd } from "../lib/marketplaceFormat";
import { useStatusStore } from "../hooks/useStatusStore";

export default function CostPill() {
  const navigate = useNavigate();
  const { data: snap } = useStatusStore();

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
      title="今日花费 / 24h 预算（点开看 per-harness）"
      className="cost-pill"
      style={{ ...tone, cursor: "pointer" }}
    >
      今日 {snap ? formatUsd(snap.cost_24h_usd) : "$—"}
      {snap ? (cap !== null ? ` / ${formatUsd(cap)}` : "") : " / $—"}
    </button>
  );
}
