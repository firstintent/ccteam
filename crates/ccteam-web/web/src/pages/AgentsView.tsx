// v0.9.0 W4 (F4) — 团队/Team view: a live cross-harness, cross-host graph of
// every session (nodes) + delegation edges (parent→child), a detail side
// panel for the selected node, and a 30-minute timeline strip. Admin-only
// beta gate lives in the SIDEBAR nav button (ChatConsole/Sidebar) — this
// page itself renders unconditionally once routed to (`/agents`), same as
// every other admin-gated tab in this shell (e.g. WorkflowView's MCP
// register form); the real ACL is the backend's per-tenant graph/SSE filter
// (`crate::routes::agents`), which is fail-closed regardless of this UI gate.
//
// Hand-rolled SVG (no graphing library — none is a dependency of this repo):
// one swim-lane per host (`lib/agentsLayout.ts`), nodes = vendor-colored
// cards with a status ring, edges = cubic beziers (dashed + animated while a
// dispatch is in flight). Live state comes from the global SSE
// (`useAgentsEvents`) folded through `lib/agentsReducer.ts`.

import { useEffect, useMemo, useRef, useState } from "react";
import { fetchAgentsGraph, type AgentEdge, type AgentsGraphResponse } from "../lib/agentsApi";
import { computeAgentsLayout, edgePath, type LayoutNode } from "../lib/agentsLayout";
import {
  applyDelegationEvent,
  delegationToast,
  sidsActiveWithin,
  type TimestampedAgentsEvent,
} from "../lib/agentsReducer";
import { useAgentsEvents } from "../hooks/useAgentsEvents";
import { getHistory, type SessionHistoryEvent } from "../lib/sessionsApi";
import { emptyFold, foldActivity, renderFold, type ActivityFold } from "./chatTranscript";
import { vendorDotClass } from "../lib/vendors";
import { makeT, type Lang } from "../lib/i18n";
import { relativeTime } from "./railHelpers";
import { toastBus } from "../lib/toastBus";

const PULSE_WINDOW_MS = 15_000;
const TIMELINE_WINDOW_MS = 30 * 60 * 1000;
const GRAPH_REFRESH_MS = 15_000;
const EVENT_LOG_CAP = 500;
const NODE_W = 168;
const NODE_H = 58;

function statusRingClass(sid: string, status: string, pulsing: Set<string>): string {
  if (status !== "live") return "agents-ring stopped";
  return pulsing.has(sid) ? "agents-ring pulse" : "agents-ring idle";
}

/** Pure presentational graph SVG — given an already-computed layout, renders
 *  the lane rules, edges, and node cards. Extracted from `AgentsView` so it's
 *  directly SSR-testable with fixture data (the parent component's data
 *  loading is `useEffect`-driven and doesn't run under `renderToString`). */
export function AgentsGraphSvg({
  layout,
  selected,
  pulsing,
  lang: langProp,
  onSelect,
}: {
  layout: ReturnType<typeof computeAgentsLayout>;
  selected: string | null;
  pulsing: Set<string>;
  lang?: Lang;
  onSelect: (sid: string) => void;
}) {
  const t = makeT(langProp ?? "zh");
  return (
    <svg
      className="agents-graph"
      viewBox={`0 0 ${layout.width} ${layout.height}`}
      width="100%"
      height={Math.max(layout.height, 160)}
      role="img"
      aria-label={t("team")}
    >
      {layout.hosts.map((h, i) => (
        <g key={h} data-testid={`agents-lane-${h}`}>
          <text x={12} y={i * 320 + 24} className="agents-lane-label">
            {h}
          </text>
          <line x1={0} y1={i * 320 + 34} x2={layout.width} y2={i * 320 + 34} className="agents-lane-rule" />
        </g>
      ))}
      {layout.edges.map((e) => (
        <path
          key={`${e.parent}-${e.child}`}
          d={edgePath(e)}
          className={`agents-edge ${e.active ? "active" : ""}`}
          data-testid={`agents-edge-${e.parent}-${e.child}`}
          data-active={e.active ? "true" : "false"}
          fill="none"
        />
      ))}
      {layout.nodes.map((n) => (
        <g
          key={n.sid}
          transform={`translate(${n.x - NODE_W / 2}, ${n.y - NODE_H / 2})`}
          className={`agents-node ${n.sid === selected ? "selected" : ""}`}
          data-testid={`agents-node-${n.sid}`}
          role="button"
          tabIndex={0}
          onClick={() => onSelect(n.sid)}
          onKeyDown={(e) => {
            if (e.key === "Enter") onSelect(n.sid);
          }}
        >
          <title>
            {n.title || n.role || n.sid} · {n.vendor} · {n.host}
          </title>
          <rect width={NODE_W} height={NODE_H} rx={12} className="agents-card" />
          <circle cx={14} cy={14} r={6} className={vendorDotClass(n.vendor)} />
          <circle cx={NODE_W - 12} cy={12} r={5} className={statusRingClass(n.sid, n.status, pulsing)} />
          <text x={26} y={19} className="agents-node-main">
            {(n.title || n.role || n.sid).slice(0, 18)}
          </text>
          <text x={12} y={36} className="agents-node-sub">
            {n.vendor} · {n.host}
          </text>
          <text x={12} y={50} className="agents-node-sub">
            {n.cost_usd != null ? `$${n.cost_usd.toFixed(4)}` : "—"} · {n.turn_count}t
          </text>
        </g>
      ))}
    </svg>
  );
}

export default function AgentsView({
  lang: langProp,
  onOpenChat,
}: {
  lang?: Lang;
  isAdmin?: boolean;
  onOpenChat?: (sid: string) => void;
} = {}) {
  const lang = langProp ?? "zh";
  const t = makeT(lang);

  const [graph, setGraph] = useState<AgentsGraphResponse>({ nodes: [], edges: [], hosts: [] });
  const [edges, setEdges] = useState<AgentEdge[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [historyBySid, setHistoryBySid] = useState<Record<string, SessionHistoryEvent[]>>({});
  const [now, setNow] = useState(() => Date.now());
  const [timestamped, setTimestamped] = useState<TimestampedAgentsEvent[]>([]);

  const { events } = useAgentsEvents();
  const seenCountRef = useRef(0);

  // ---- snapshot: initial load + periodic refresh (catches anything the
  // event-log reducer alone can't derive, e.g. a brand-new root session) ----
  useEffect(() => {
    let cancelled = false;
    const load = () => {
      fetchAgentsGraph()
        .then((g) => {
          if (cancelled) return;
          setGraph(g);
          setEdges(g.edges);
        })
        .catch(() => {
          /* best-effort: the page just keeps showing the last good snapshot */
        })
        .finally(() => {
          if (!cancelled) setLoading(false);
        });
    };
    load();
    const id = window.setInterval(load, GRAPH_REFRESH_MS);
    return () => {
      cancelled = true;
      window.clearInterval(id);
    };
  }, []);

  // ---- fold fresh SSE frames: edge active state + denial toasts + the
  // timestamped log the pulse/activity/timeline all read from ----
  useEffect(() => {
    if (events.length <= seenCountRef.current) return;
    const fresh = events.slice(seenCountRef.current);
    seenCountRef.current = events.length;
    const at = Date.now();
    setTimestamped((prev) => {
      const merged = [...prev, ...fresh.map((e) => ({ ...e, receivedAt: at }))];
      return merged.length > EVENT_LOG_CAP ? merged.slice(merged.length - EVENT_LOG_CAP) : merged;
    });
    setEdges((prev) => fresh.reduce(applyDelegationEvent, prev));
    for (const ev of fresh) {
      const msg = delegationToast(ev);
      if (msg) toastBus.handler?.error(msg);
    }
  }, [events]);

  // Tick `now` so the pulse window + timeline auto-advance without needing a
  // fresh SSE frame.
  useEffect(() => {
    const id = window.setInterval(() => setNow(Date.now()), 3000);
    return () => window.clearInterval(id);
  }, []);

  const pulsing = useMemo(
    () => sidsActiveWithin(timestamped, PULSE_WINDOW_MS, now),
    [timestamped, now],
  );

  const layout = useMemo(
    () => computeAgentsLayout(graph.nodes, edges, graph.hosts),
    [graph.nodes, edges, graph.hosts],
  );

  const selectedNode: LayoutNode | null =
    layout.nodes.find((n) => n.sid === selected) ?? null;

  const activityFold: ActivityFold = useMemo(() => {
    let fold = emptyFold();
    if (!selected) return fold;
    for (const ev of timestamped) {
      if (ev.sid !== selected) continue;
      if (ev.kind === "activity" && ev.activity) fold = foldActivity(fold, ev.activity);
      else if (ev.kind === "answer") fold = emptyFold(); // a new answer ends the turn
    }
    return fold;
  }, [timestamped, selected]);

  useEffect(() => {
    if (!selected || historyBySid[selected]) return;
    getHistory(selected)
      .then((h) => setHistoryBySid((prev) => ({ ...prev, [selected]: h.events.slice(-3) })))
      .catch(() => setHistoryBySid((prev) => ({ ...prev, [selected]: [] })));
  }, [selected, historyBySid]);

  const timelineNodes = useMemo(
    () => [...layout.nodes].sort((a, b) => a.depth - b.depth || a.sid.localeCompare(b.sid)),
    [layout.nodes],
  );
  const windowStart = now - TIMELINE_WINDOW_MS;
  const timelineDispatches = useMemo(
    () =>
      timestamped.filter(
        (e) =>
          e.kind === "delegation" &&
          e.relation === "dispatched" &&
          e.receivedAt >= windowStart &&
          e.parent_sid &&
          e.child_sid,
      ),
    [timestamped, windowStart],
  );

  const timelineX = (ms: number): number => {
    const clamped = Math.min(Math.max(ms, windowStart), now);
    return ((clamped - windowStart) / TIMELINE_WINDOW_MS) * 100;
  };
  const rowIndex = new Map(timelineNodes.map((n, i) => [n.sid, i]));

  return (
    <section className="view active agents-view" data-testid="agents-view">
      <header className="agents-head">
        <div>
          <h1>{t("team")}</h1>
          <p>{t("teamDesc")}</p>
        </div>
      </header>

      <div className="agents-body">
        <div className="agents-canvas" data-testid="agents-canvas">
          {loading ? (
            <p style={{ color: "var(--text-faint)", fontSize: 13, padding: 16 }}>{t("loading")}</p>
          ) : layout.nodes.length === 0 ? (
            <p style={{ color: "var(--text-faint)", fontSize: 13, padding: 16 }} data-testid="agents-empty">
              {t("teamEmpty")}
            </p>
          ) : (
            <AgentsGraphSvg layout={layout} selected={selected} pulsing={pulsing} lang={lang} onSelect={setSelected} />
          )}
        </div>

        {selectedNode ? (
          <aside className="agents-panel" data-testid="agents-panel">
            <h2>{selectedNode.title || selectedNode.role || selectedNode.sid}</h2>
            <div className="agents-panel-meta">
              <span>
                {t("vendor")}: <span className={vendorDotClass(selectedNode.vendor)} /> {selectedNode.vendor}
              </span>
              <span>
                {t("model")}: {selectedNode.model || "—"}
              </span>
              <span>
                {t("host")}: {selectedNode.host}
              </span>
              <span>
                {t("parentSession")}: {selectedNode.parent_sid || t("noParent")}
              </span>
              <span>
                {t("depth")}: {selectedNode.depth}
              </span>
              <span>
                {t("cost")}: {selectedNode.cost_usd != null ? `$${selectedNode.cost_usd.toFixed(4)}` : "—"}
              </span>
              <span>{relativeTime(lang, selectedNode.last_active)}</span>
            </div>

            <h3>{t("teamActivity")}</h3>
            <p className="agents-activity-line" data-testid="agents-activity-line">
              {pulsing.has(selectedNode.sid) ? renderFold(activityFold) : t("teamIdle")}
            </p>

            <h3>{t("teamRecentTurns")}</h3>
            <div className="agents-turns" data-testid="agents-turns">
              {(historyBySid[selectedNode.sid] ?? []).length === 0 ? (
                <p style={{ fontSize: 12.5, color: "var(--text-faint)" }}>—</p>
              ) : (
                (historyBySid[selectedNode.sid] ?? []).map((ev) => (
                  <div key={ev.turn_id} className="agents-turn-row">
                    <span className="mono" style={{ fontSize: 11, color: "var(--text-faint)" }}>
                      {ev.ts.slice(0, 16).replace("T", " ")}
                    </span>
                    <span style={{ fontSize: 12.5, color: "var(--text-muted)" }}>
                      {(ev.assistant || ev.user || "").slice(0, 120)}
                    </span>
                  </div>
                ))
              )}
            </div>

            <button
              type="button"
              className="btn primary mini"
              data-testid="agents-open-chat"
              onClick={() => onOpenChat?.(selectedNode.sid)}
            >
              {t("teamOpenChat")}
            </button>
          </aside>
        ) : layout.nodes.length > 0 ? (
          <aside className="agents-panel agents-panel-empty" data-testid="agents-panel-empty">
            <p style={{ color: "var(--text-faint)", fontSize: 13 }}>{t("teamSelectHint")}</p>
          </aside>
        ) : null}
      </div>

      <div className="agents-timeline" data-testid="agents-timeline">
        <h3>{t("teamTimeline")}</h3>
        <div className="agents-timeline-rows">
          {timelineNodes.map((n) => (
            <div key={n.sid} className="agents-timeline-row" data-testid={`agents-timeline-row-${n.sid}`}>
              <span className="agents-timeline-label">{n.title || n.role || n.sid}</span>
              <div className="agents-timeline-track">
                {n.status === "live" ? (
                  <div
                    className={`agents-timeline-bar ${pulsing.has(n.sid) ? "active" : ""}`}
                    style={{ left: `${timelineX(Date.parse(n.last_active) || now)}%`, width: "2%" }}
                  />
                ) : null}
              </div>
            </div>
          ))}
        </div>
        <svg className="agents-timeline-arrows" viewBox={`0 0 100 ${Math.max(timelineNodes.length, 1) * 28}`} preserveAspectRatio="none">
          {timelineDispatches.map((ev, i) => {
            const fromRow = rowIndex.get(ev.parent_sid!);
            const toRow = rowIndex.get(ev.child_sid!);
            if (fromRow === undefined || toRow === undefined) return null;
            const x = timelineX(ev.receivedAt);
            return (
              <line
                key={`${ev.parent_sid}-${ev.child_sid}-${i}`}
                x1={x}
                y1={fromRow * 28 + 14}
                x2={x}
                y2={toRow * 28 + 14}
                className="agents-timeline-arrow"
                data-testid="agents-timeline-arrow"
              />
            );
          })}
        </svg>
      </div>
    </section>
  );
}
