// V0.4.6 F90 — Cost trend mini sparkline (SVG).
//
// Two stacked sparklines for the workflow view's cost section: 24h
// hourly buckets + 7d hourly buckets. Read-only SVG; no D3, no chart
// library — `sparklinePoints` (in lib/workflowPanels.ts) produces the
// polyline `points` string and we render two `<svg>` elements.
//
// Refresh strategy: fetch once on mount + re-fetch when the project's
// SSE stream emits an `agent_done` event (the only kind that changes
// cost totals). Mirrors `ArtifactQueuePanel`'s "watch the bus, not a
// timer" pattern.

import { useEffect, useState } from "react";
import {
  fetchCostHistory,
  sparklinePoints,
  type CostHistoryResponse,
} from "../lib/workflowPanels";
import { useProgressStream } from "../hooks/useProgressStream";

interface Props {
  slug: string;
  reloadKey?: number;
}

const SVG_W = 160;
const SVG_H = 32;

function totalOf(resp: CostHistoryResponse | null): number {
  if (!resp) return 0;
  return resp.buckets.reduce((acc, b) => acc + b.cost_usd, 0);
}

function Sparkline({
  label,
  resp,
}: {
  label: string;
  resp: CostHistoryResponse | null;
}) {
  const total = totalOf(resp);
  return (
    <div className="flex items-center justify-between gap-2 px-3 py-1.5 border border-surface-700/30 rounded-md bg-surface-850">
      <div className="flex flex-col gap-0.5">
        <span className="text-[10px] uppercase tracking-wide text-text-dim">
          {label}
        </span>
        <span className="font-mono text-xs text-text-primary">
          ${total.toFixed(2)}
        </span>
      </div>
      <svg
        width={SVG_W}
        height={SVG_H}
        viewBox={`0 0 ${SVG_W} ${SVG_H}`}
        aria-label={`cost sparkline ${label}`}
        className="text-brand-500"
      >
        {resp && (
          <polyline
            fill="none"
            stroke="currentColor"
            strokeWidth="1.5"
            points={sparklinePoints(resp.buckets, SVG_W, SVG_H)}
          />
        )}
      </svg>
    </div>
  );
}

export default function CostSparkline({ slug, reloadKey = 0 }: Props) {
  const [r24, setR24] = useState<CostHistoryResponse | null>(null);
  const [r7d, setR7d] = useState<CostHistoryResponse | null>(null);
  const [error, setError] = useState<string | null>(null);

  const stream = useProgressStream({ scope: { kind: "project", slug } });
  const latest = stream.events[stream.events.length - 1];

  // Fetch both windows together so the panel renders coherently. A
  // partial failure surfaces the error inline but doesn't blank both
  // sparklines.
  useEffect(() => {
    let cancelled = false;
    Promise.all([fetchCostHistory(slug, "24h"), fetchCostHistory(slug, "7d")])
      .then(([a, b]) => {
        if (cancelled) return;
        setR24(a);
        setR7d(b);
        setError(null);
      })
      .catch((e: unknown) => {
        if (cancelled) return;
        setError(e instanceof Error ? e.message : String(e));
      });
    return () => {
      cancelled = true;
    };
  }, [slug, reloadKey]);

  // Re-fetch on agent_done events (only events that change cost).
  useEffect(() => {
    if (!latest) return;
    if (latest.event !== "agent_done") return;
    let cancelled = false;
    Promise.all([fetchCostHistory(slug, "24h"), fetchCostHistory(slug, "7d")])
      .then(([a, b]) => {
        if (cancelled) return;
        setR24(a);
        setR7d(b);
      })
      .catch(() => {
        // Silent — next agent_done will retry.
      });
    return () => {
      cancelled = true;
    };
  }, [slug, latest]);

  return (
    <section className="flex flex-col gap-2">
      <h3 className="text-xs uppercase tracking-wide text-text-secondary">
        Cost Trend
      </h3>
      {error && (
        <div className="text-xs font-mono text-status-error" role="alert">
          {error}
        </div>
      )}
      <div className="grid grid-cols-1 sm:grid-cols-2 gap-2">
        <Sparkline label="24h" resp={r24} />
        <Sparkline label="7d" resp={r7d} />
      </div>
    </section>
  );
}
