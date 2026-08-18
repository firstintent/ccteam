import type { StatusSnapshot } from "../lib/statusApi";
import { useStatusStore } from "../hooks/useStatusStore";
import { SkeletonRows } from "../components/ui";
import {
  budgetSeverity,
  formatCostBudget,
  formatUsd,
  vendorCostSplit,
} from "../lib/marketplaceFormat";

/** `embedded` — hide the page header (Ops panel already owns the title). */
export default function StatusView({ embedded = false }: { embedded?: boolean } = {}) {
  const { data: status, loading, error } = useStatusStore();

  return (
    <div data-testid="status-view" className="flex flex-col gap-3">
      {embedded ? null : (
        <header>
          <h1>Status · 运维总览</h1>
          <p>daemon 健康 · 会话 · 今日成本 / 预算。</p>
        </header>
      )}
      <div className="space-y-3">
        {loading && status === null ? (
          <div data-testid="status-loading"><SkeletonRows rows={3} /></div>
        ) : status === null ? (
          <div data-testid="status-error" role="alert" className="rounded-lg border border-status-error/40 bg-status-error/10 px-4 py-4 text-sm text-status-error">
            加载状态失败: {error ?? "加载失败"}
          </div>
        ) : (
          <StatusCards status={status} />
        )}
      </div>
    </div>
  );
}

export function StatusCards({ status }: { status: StatusSnapshot }) {
  const severity = budgetSeverity(status.cost_24h_usd, status.budget_cap_24h);
  const vendorSplit = vendorCostSplit(status.cost_24h_by_vendor);
  return (
    <>
      {/* Daemon health leads — full-width strip so operators see it first. */}
      <div
        className={`daemon-strip ${status.daemon_healthy ? "ok" : "down"}`}
        data-testid="status-daemon"
      >
        <span className={`dot ${status.daemon_healthy ? "on" : "off"}`} />
        <span className="daemon-strip-title">
          {status.daemon_healthy ? "daemon healthy" : "daemon down"}
        </span>
        <span className="daemon-strip-sub">
          {status.daemon_healthy ? "MCP sock OK" : "MCP sock unreachable"}
        </span>
      </div>
      <div className="stat-grid">
        <div className="stat" data-testid="status-session-stat">
          <span className="k">会话</span>
          <span className="v">
            {status.sessions_live} <span className="u">live ·</span> {status.sessions_idle}{" "}
            <span className="u">idle</span>
          </span>
          <span className="k">共 {status.sessions_live + status.sessions_idle}</span>
        </div>
        <div className="stat" data-testid="status-cost">
          <span className="k">今日成本</span>
          <span className="v" style={{ color: severity === "over" ? "var(--red-text)" : severity === "warn" ? "#B45309" : undefined }}>
            {formatCostBudget(status.cost_24h_usd, status.budget_cap_24h)}
          </span>
          <span className="k">{vendorSplit.length > 0 ? vendorSplit.join(" · ") : "本窗口暂无计费记录。"}</span>
        </div>
      </div>
      {status.budget_cap_24h !== null && severity !== "ok" ? (
        <div data-testid="status-budget-warn" role="status" className={`badge ${severity === "over" ? "warn" : ""}`} style={{ padding: "8px 12px", borderRadius: 10, fontSize: 12 }}>
          {severity === "over"
            ? `已达/超 24h 预算（${formatUsd(status.cost_24h_usd)} / ${formatUsd(status.budget_cap_24h)}）— 接近上限会自停（红线）。`
            : `接近 24h 预算（${formatUsd(status.cost_24h_usd)} / ${formatUsd(status.budget_cap_24h)}）。`}
        </div>
      ) : null}
    </>
  );
}
