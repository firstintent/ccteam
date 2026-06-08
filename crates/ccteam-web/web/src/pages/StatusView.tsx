// v0.8.9 Phase 4 — lightweight Status / 运维总览 global view (prototype
// `#v-status`). Replaces the retired operator dashboard with a single glance:
// daemon health + sessions (live · idle) + today's cost vs the 24h budget.
//
// Data: `GET /api/v1/status` (statusApi). Best-effort backend — a down daemon
// degrades to `daemon_healthy:false` + zeroed cost, never a 500, so the view
// always renders. The live session rail the shell already built is passed in
// to enrich the sessions card (per-session lines), independent of the
// aggregate counts.
//
// Four states (v0.8.8 baseline): loading / error / empty (no sessions/cost) /
// success. Theme tokens only (surface-*/brand-*/text-*/status-* + vendor-*).

import { useEffect, useState } from "react";
import { getStatus, type StatusSnapshot } from "../lib/statusApi";
import type { SessionView } from "../lib/sessionsApi";
import {
  budgetSeverity,
  formatCostBudget,
  formatUsd,
  vendorCostSplit,
} from "../lib/marketplaceFormat";

type LoadState =
  | { kind: "loading" }
  | { kind: "error"; message: string }
  | { kind: "ready"; status: StatusSnapshot };

/** Poll the status snapshot on a sane cadence so the Status view stays fresh
 *  without an SSE channel (the aggregate is cheap + best-effort). */
const STATUS_POLL_MS = 15000;

export default function StatusView({ rail }: { rail: SessionView[] }) {
  const [state, setState] = useState<LoadState>({ kind: "loading" });

  useEffect(() => {
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | null = null;

    const tick = (initial: boolean) => {
      getStatus()
        .then((status) => {
          if (cancelled) return;
          setState({ kind: "ready", status });
        })
        .catch((e) => {
          if (cancelled) return;
          if (e instanceof Error && e.message === "UNAUTHENTICATED") return;
          // Only surface the error if we have nothing to show yet; a transient
          // poll failure shouldn't blank an already-rendered card.
          if (initial) {
            setState({ kind: "error", message: e instanceof Error ? e.message : "加载失败" });
          }
        })
        .finally(() => {
          if (!cancelled) timer = setTimeout(() => tick(false), STATUS_POLL_MS);
        });
    };
    tick(true);

    return () => {
      cancelled = true;
      if (timer) clearTimeout(timer);
    };
  }, []);

  return (
    <div data-testid="status-view" className="p-6 max-w-[880px] mx-auto">
      <h2 className="text-base font-semibold text-text-primary">Status · 运维总览</h2>
      <p className="mt-1 text-sm text-text-secondary">
        轻量运维视图（daemon 健康 + 各 session 状态 + 今日成本），取代旧 operator dashboard。
      </p>

      <div className="mt-4 space-y-3">
        {state.kind === "loading" ? (
          <div
            data-testid="status-loading"
            className="rounded-lg border border-dashed border-surface-700/60 bg-surface-900/40 px-4 py-8 text-center text-xs text-text-dim"
          >
            加载运维状态中…
          </div>
        ) : state.kind === "error" ? (
          <div
            data-testid="status-error"
            role="alert"
            className="rounded-lg border border-status-error/40 bg-status-error/10 px-4 py-4 text-sm text-status-error"
          >
            加载状态失败: {state.message}
          </div>
        ) : (
          <StatusCards status={state.status} rail={rail} />
        )}
      </div>
    </div>
  );
}

export function StatusCards({
  status,
  rail,
}: {
  status: StatusSnapshot;
  rail: SessionView[];
}) {
  const severity = budgetSeverity(status.cost_24h_usd, status.budget_cap_24h);
  const vendorSplit = vendorCostSplit(status.cost_24h_by_vendor);

  return (
    <>
      {/* daemon health */}
      <div
        data-testid="status-daemon"
        className="rounded-lg bg-surface-900 border border-surface-700/60 px-4 py-3 flex items-center gap-2.5"
      >
        <span
          className={`h-2.5 w-2.5 rounded-full ${
            status.daemon_healthy ? "bg-status-running" : "bg-status-error"
          }`}
          aria-hidden
        />
        <span className="text-sm font-medium text-text-primary">
          {status.daemon_healthy ? "daemon healthy" : "daemon down"}
        </span>
        <span
          className={`ml-auto text-[11px] font-medium px-2 py-0.5 rounded-full ${
            status.daemon_healthy
              ? "bg-status-running/15 text-status-running"
              : "bg-status-error/15 text-status-error"
          }`}
        >
          {status.daemon_healthy ? "MCP sock OK" : "MCP sock unreachable"}
        </span>
      </div>

      {/* sessions */}
      <div
        data-testid="status-sessions"
        className="rounded-lg bg-surface-900 border border-surface-700/60 px-4 py-3"
      >
        <div className="text-sm font-medium text-text-primary mb-1.5">
          会话（{status.sessions_live} live · {status.sessions_idle} idle）
        </div>
        {rail.length === 0 ? (
          <div className="text-xs text-text-dim">没有活动会话。</div>
        ) : (
          <div className="space-y-1">
            {rail.map((s) => {
              const activity = sessionActivityMeta(s.status);
              return (
                <div key={s.sid} className="text-xs text-text-secondary flex items-center gap-2">
                  <span className="font-mono text-text-dim">{s.project}</span>
                  <span className="text-text-dim">/</span>
                  <span
                    className={s.vendor === "claude" ? "text-vendor-claude" : "text-vendor-codex"}
                >
                  {[s.vendor, s.role || "(无 role)"].filter(Boolean).join(" · ")}
                </span>
                <span className="font-mono text-text-dim">{s.sid}</span>
                  {typeof s.last_activity_seconds === "number" ? (
                    <span className="text-[10px] text-text-dim">
                      最近活动 {formatActivityAge(s.last_activity_seconds)}前
                    </span>
                  ) : null}
                  <span
                    className={`ml-auto text-[10px] font-medium px-1.5 py-0.5 rounded-full ${activity.className}`}
                  >
                    {activity.label}
                  </span>
                </div>
              );
            })}
          </div>
        )}
      </div>

      {/* today's cost */}
      <div
        data-testid="status-cost"
        className="rounded-lg bg-surface-900 border border-surface-700/60 px-4 py-3"
      >
        <div className="flex items-center gap-2">
          <span className="text-sm font-medium text-text-primary">今日成本</span>
          <b
            className={`ml-auto font-mono text-sm ${
              severity === "over"
                ? "text-status-error"
                : severity === "warn"
                  ? "text-brand-400"
                  : "text-text-primary"
            }`}
          >
            {formatCostBudget(status.cost_24h_usd, status.budget_cap_24h)}
          </b>
        </div>
        {vendorSplit.length > 0 ? (
          <div className="mt-1.5 text-xs text-text-secondary">{vendorSplit.join(" · ")}</div>
        ) : (
          <div className="mt-1.5 text-xs text-text-dim">本窗口暂无计费记录。</div>
        )}
        {status.budget_cap_24h !== null && severity !== "ok" ? (
          <div
            data-testid="status-budget-warn"
            role="status"
            className={`mt-2 text-[11px] rounded-md px-2.5 py-1.5 ${
              severity === "over"
                ? "bg-status-error/10 text-status-error border border-status-error/30"
                : "bg-brand-500/10 text-brand-400 border border-brand-500/30"
            }`}
          >
            {severity === "over"
              ? `已达/超 24h 预算（${formatUsd(status.cost_24h_usd)} / ${formatUsd(
                  status.budget_cap_24h,
                )}）— 接近上限会自停（红线）。`
              : `接近 24h 预算（${formatUsd(status.cost_24h_usd)} / ${formatUsd(
                  status.budget_cap_24h,
                )}）。`}
          </div>
        ) : null}
      </div>
    </>
  );
}

function formatActivityAge(seconds: number): string {
  if (seconds < 60) return `${Math.max(0, Math.floor(seconds))} 秒`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes} 分钟`;
  const hours = Math.floor(minutes / 60);
  if (hours < 48) return `${hours} 小时`;
  return `${Math.floor(hours / 24)} 天`;
}

function sessionActivityMeta(status: string): { label: string; className: string } {
  switch (status) {
    case "stuck":
      return {
        label: "疑似卡",
        className: "bg-status-error/15 text-status-error",
      };
    case "working":
      return {
        label: "working",
        className: "bg-status-running/15 text-status-running",
      };
    case "idle":
      return {
        label: "idle",
        className: "bg-brand-500/15 text-brand-400",
      };
    case "live":
      return {
        label: "live",
        className: "bg-status-running/15 text-status-running",
      };
    default:
      return {
        label: status || "idle",
        className: "bg-brand-500/15 text-brand-400",
      };
  }
}
