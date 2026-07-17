// v0.8.9 Phase 4 — lightweight Status / 运维总览 global view (prototype
// `#v-status`). Replaces the retired operator dashboard with a single glance:
// daemon health + sessions (live · idle) + today's cost vs the 24h budget.
//
// Data: `GET /api/v1/status` (statusApi). Best-effort backend — a down daemon
// degrades to `daemon_healthy:false` + zeroed cost, never a 500, so the view
// always renders. The live session rail the shell already built is passed in
// to enrich the sessions card (a sortable @tanstack/react-table fleet table),
// independent of the aggregate counts.
//
// Four states (v0.8.8 baseline): loading / error / empty (no sessions/cost) /
// success. Theme tokens only (surface-*/brand-*/text-*/status-* + vendor-*).

import { useEffect, useMemo, useState } from "react";
import {
  flexRender,
  getCoreRowModel,
  getSortedRowModel,
  useReactTable,
  type ColumnDef,
  type SortingState,
} from "@tanstack/react-table";
import { Inbox } from "lucide-react";
import { getStatus, type StatusSnapshot } from "../lib/statusApi";
import type { SessionView } from "../lib/sessionsApi";
import {
  Badge,
  EmptyState,
  SkeletonRows,
  SortableHeader,
  Table,
  TableBody,
  TableCell,
  TableHeader,
  TableRow,
  type SortDirection,
} from "../components/ui";
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
    <div data-testid="status-view" className="flex flex-col gap-5">
      <header>
        <h1>Status · 运维总览</h1>
        <p>daemon 健康 · 会话 · 今日成本 / 预算。</p>
      </header>

      <div className="space-y-3">
        {state.kind === "loading" ? (
          <div data-testid="status-loading">
            <SkeletonRows rows={3} />
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

/** One row of the fleet table — a live rail session joined to its per-sid cost. */
interface FleetRow {
  sid: string;
  project: string;
  vendor: string;
  role: string;
  /** Deterministic per-turn cost, or null when nothing priceable → "—". */
  cost: number | null;
  /** Seconds since last activity, or null when the session never reported. */
  activitySeconds: number | null;
  status: string;
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
  // v0.8.18 柱1 — per-session cost from /status's fleet list, joined to the
  // live rail by sid. The loop-ops console skeleton: this column is the seam
  // the loop version grows oracle/gate columns onto.
  const costBySid = useMemo(
    () => new Map((status.sessions ?? []).map((s) => [s.sid, s.cost_usd])),
    [status.sessions],
  );

  const rows: FleetRow[] = useMemo(
    () =>
      rail.map((s) => {
        // Preserve null (unknown cost → "—"); a session absent from the fleet
        // list is likewise unknown, not $0.
        const c = costBySid.get(s.sid);
        return {
          sid: s.sid,
          project: s.project,
          vendor: s.vendor,
          role: s.role || "(无 role)",
          cost: c === undefined ? null : c,
          activitySeconds:
            typeof s.last_activity_seconds === "number" ? s.last_activity_seconds : null,
          status: s.status,
        };
      }),
    [rail, costBySid],
  );

  return (
    <>
      {/* prototype stat-grid: daemon · sessions · today's cost */}
      <div className="stat-grid">
        <div className="stat" data-testid="status-daemon">
          <span className="k">daemon</span>
          <span
            className="v"
            style={{ color: status.daemon_healthy ? "var(--green-text)" : "var(--red-text)" }}
          >
            {status.daemon_healthy ? "daemon healthy" : "daemon down"}
          </span>
          <span className="k">
            {status.daemon_healthy ? "MCP sock OK" : "MCP sock unreachable"}
          </span>
        </div>
        <div className="stat">
          <span className="k">会话</span>
          <span className="v">
            {status.sessions_live} <span className="u">live ·</span> {status.sessions_idle}{" "}
            <span className="u">idle</span>
          </span>
          <span className="k">共 {rows.length}</span>
        </div>
        <div className="stat" data-testid="status-cost">
          <span className="k">今日成本</span>
          <span
            className="v"
            style={{
              color:
                severity === "over"
                  ? "var(--red-text)"
                  : severity === "warn"
                    ? "#B45309"
                    : undefined,
            }}
          >
            {formatCostBudget(status.cost_24h_usd, status.budget_cap_24h)}
          </span>
          <span className="k">
            {vendorSplit.length > 0 ? vendorSplit.join(" · ") : "本窗口暂无计费记录。"}
          </span>
        </div>
      </div>

      {status.budget_cap_24h !== null && severity !== "ok" ? (
        <div
          data-testid="status-budget-warn"
          role="status"
          className={`badge ${severity === "over" ? "warn" : ""}`}
          style={{ padding: "8px 12px", borderRadius: 10, fontSize: 12 }}
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

      {/* per-session fleet table (sortable — kept, 红线 §1.6-6) */}
      <div
        data-testid="status-sessions"
        style={{
          border: "1px solid var(--border)",
          borderRadius: "var(--radius-card)",
          background: "var(--bg-card)",
          padding: "12px 16px",
        }}
      >
        <div style={{ fontSize: 13.5, fontWeight: 500, marginBottom: 6 }}>
          会话（{status.sessions_live} live · {status.sessions_idle} idle）
        </div>
        {rows.length === 0 ? (
          <EmptyState
            icon={Inbox}
            title="没有活动会话"
            description="从聊天面板或新建会话开始，会话会出现在这里。"
            className="border-0 bg-transparent py-6"
          />
        ) : (
          <FleetTable rows={rows} />
        )}
      </div>
    </>
  );
}

/** The sortable fleet table — one row per live rail session. Sorting is a
 *  client enhancement; the initial (sid-ascending) server render already emits
 *  every row + value so the SSR smoke tests see the data without a DOM. */
function FleetTable({ rows }: { rows: FleetRow[] }) {
  // Default sort by sid asc so the SSR order is stable + deterministic.
  const [sorting, setSorting] = useState<SortingState>([{ id: "sid", desc: false }]);

  const columns = useMemo<ColumnDef<FleetRow>[]>(
    () => [
      {
        accessorKey: "project",
        header: ({ column }) => (
          <SortableHeader sorted={column.getIsSorted() as SortDirection} onSort={column.getToggleSortingHandler()}>
            项目
          </SortableHeader>
        ),
        cell: ({ row }) => (
          <span className="font-mono text-text-secondary">{row.original.project}</span>
        ),
      },
      {
        id: "sid",
        accessorKey: "sid",
        header: ({ column }) => (
          <SortableHeader sorted={column.getIsSorted() as SortDirection} onSort={column.getToggleSortingHandler()}>
            agent · sid
          </SortableHeader>
        ),
        cell: ({ row }) => (
          <span className="flex items-center gap-2">
            <span className={vendorTextClass(row.original.vendor)}>
              {[row.original.vendor, row.original.role].filter(Boolean).join(" · ")}
            </span>
            <span className="font-mono text-text-dim">{row.original.sid}</span>
          </span>
        ),
      },
      {
        accessorKey: "cost",
        header: ({ column }) => (
          <SortableHeader
            sorted={column.getIsSorted() as SortDirection}
            onSort={column.getToggleSortingHandler()}
            align="right"
          >
            成本
          </SortableHeader>
        ),
        // null cost (no priceable turn) sorts last when ascending.
        sortUndefined: "last",
        cell: ({ row }) => (
          <span
            data-testid={`session-cost-${row.original.sid}`}
            className="block text-right font-mono tabular-nums text-text-secondary"
            title={
              row.original.cost === null
                ? "暂无可定价成本（模型未在价目表 / 该会话尚无用量）"
                : "本会话按每回合真实模型累计的成本"
            }
          >
            {row.original.cost === null ? "—" : formatUsd(row.original.cost)}
          </span>
        ),
      },
      {
        accessorKey: "activitySeconds",
        header: ({ column }) => (
          <SortableHeader
            sorted={column.getIsSorted() as SortDirection}
            onSort={column.getToggleSortingHandler()}
            align="right"
          >
            最近活动
          </SortableHeader>
        ),
        // null (never reported) sorts last when ascending.
        sortUndefined: "last",
        cell: ({ row }) => (
          <span className="block text-right tabular-nums text-text-dim">
            {row.original.activitySeconds === null
              ? "—"
              : `${formatActivityAge(row.original.activitySeconds)}前`}
          </span>
        ),
      },
      {
        accessorKey: "status",
        header: ({ column }) => (
          <SortableHeader
            sorted={column.getIsSorted() as SortDirection}
            onSort={column.getToggleSortingHandler()}
            align="right"
          >
            状态
          </SortableHeader>
        ),
        cell: ({ row }) => {
          const meta = sessionActivityMeta(row.original.status);
          return (
            <span className="flex justify-end">
              <Badge className={meta.className}>{meta.label}</Badge>
            </span>
          );
        },
      },
    ],
    [],
  );

  // eslint-disable-next-line react-hooks/incompatible-library -- TanStack Table returns non-memoizable functions; the React Compiler skips this component, which is fine (the fleet table is small + re-renders cheaply).
  const table = useReactTable({
    data: rows,
    columns,
    state: { sorting },
    onSortingChange: setSorting,
    getCoreRowModel: getCoreRowModel(),
    getSortedRowModel: getSortedRowModel(),
  });

  return (
    <Table data-testid="fleet-table">
      <TableHeader>
        {table.getHeaderGroups().map((hg) => (
          <tr key={hg.id}>
            {hg.headers.map((h) =>
              flexRender(h.column.columnDef.header, h.getContext()),
            )}
          </tr>
        ))}
      </TableHeader>
      <TableBody>
        {table.getRowModel().rows.map((r) => (
          <TableRow key={r.id}>
            {r.getVisibleCells().map((c) => (
              <TableCell key={c.id}>{flexRender(c.column.columnDef.cell, c.getContext())}</TableCell>
            ))}
          </TableRow>
        ))}
      </TableBody>
    </Table>
  );
}

/** 5-way vendor text color (never collapse a vendor into another's color). */
function vendorTextClass(vendor: string): string {
  switch (vendor) {
    case "claude":
      return "text-vendor-claude";
    case "grok":
      return "text-vendor-grok";
    case "opencode":
      return "text-vendor-opencode";
    case "kimi":
      return "text-vendor-kimi";
    default:
      return "text-vendor-codex";
  }
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
    case "stale":
      return {
        label: "疑似慢",
        className: "bg-status-waiting/15 text-status-waiting",
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
