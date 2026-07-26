import { useEffect, useState } from "react";
import { getStatus, type StatusSnapshot } from "../lib/statusApi";
import { SkeletonRows } from "../components/ui";
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

const STATUS_POLL_MS = 15000;

export default function StatusView() {
  const [state, setState] = useState<LoadState>({ kind: "loading" });

  useEffect(() => {
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | null = null;
    const tick = (initial: boolean) => {
      getStatus()
        .then((status) => {
          if (!cancelled) setState({ kind: "ready", status });
        })
        .catch((e) => {
          if (cancelled || (e instanceof Error && e.message === "UNAUTHENTICATED")) return;
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
    <div data-testid="status-view" className="flex flex-col gap-5">
      <header>
        <h1>Status · 运维总览</h1>
        <p>daemon 健康 · 会话 · 今日成本 / 预算。</p>
      </header>
      <div className="space-y-3">
        {state.kind === "loading" ? (
          <div data-testid="status-loading"><SkeletonRows rows={3} /></div>
        ) : state.kind === "error" ? (
          <div data-testid="status-error" role="alert" className="rounded-lg border border-status-error/40 bg-status-error/10 px-4 py-4 text-sm text-status-error">
            加载状态失败: {state.message}
          </div>
        ) : (
          <StatusCards status={state.status} />
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
      <div className="stat-grid">
        <div className="stat" data-testid="status-daemon">
          <span className="k">daemon</span>
          <span className="v" style={{ color: status.daemon_healthy ? "var(--green-text)" : "var(--red-text)" }}>
            {status.daemon_healthy ? "daemon healthy" : "daemon down"}
          </span>
          <span className="k">{status.daemon_healthy ? "MCP sock OK" : "MCP sock unreachable"}</span>
        </div>
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
