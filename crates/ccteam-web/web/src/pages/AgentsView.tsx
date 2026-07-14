// v0.9.1 团队/Team view — redesigned around the operator's real questions
// (who is working, who is stuck, who spent what, who delegated to whom):
//
// - 拓扑 topology (DEFAULT): a compact, collapsible delegation tree grouped
//   by project, designed to stay readable with 100+ sessions.
// - 成员 roster: a delegation tree TABLE — one row per session,
//   children indented under their parent (process-tree shape). The common
//   no-delegation case reads as a clean list, not scattered graph boxes.
// - 时间轴 timeline: the 30-min dispatch strip (rows + arrows), unchanged
//   semantics from v0.9.0 W4, now with full-height room.
//
// A KPI strip (live / working / active dispatches / total cost) sits above
// the tabs, computed client-side from the same graph+SSE data. Admin-only
// beta gate lives in the SIDEBAR nav button; the real ACL is the backend's
// per-tenant graph/SSE filter (fail-closed regardless of this UI gate).
//
// Live state comes from the global SSE (`useAgentsEvents`) folded through
// `lib/agentsReducer.ts`; roster order from `lib/agentsTree.ts` (pure).

import { useEffect, useMemo, useRef, useState } from "react";
import { fetchAgentsGraph, type AgentEdge, type AgentNode, type AgentsGraphResponse } from "../lib/agentsApi";
import {
  applyDelegationEvent,
  delegationToast,
  sidsActiveWithin,
  type TimestampedAgentsEvent,
} from "../lib/agentsReducer";
import {
  flattenDelegationTree,
  groupDelegationTrees,
  type RosterRow,
} from "../lib/agentsTree";
import { useAgentsEvents } from "../hooks/useAgentsEvents";
import { getHistory, type SessionHistoryEvent } from "../lib/sessionsApi";
import { emptyFold, foldActivity, renderFold, type ActivityFold } from "./chatTranscript";
import { vendorChipClass, vendorDotClass } from "../lib/vendors";
import { makeT, type Lang } from "../lib/i18n";
import { relativeTime } from "./railHelpers";
import { toastBus } from "../lib/toastBus";

const PULSE_WINDOW_MS = 60_000;
const TIMELINE_WINDOW_MS = 30 * 60 * 1000;
const GRAPH_REFRESH_MS = 15_000;
const EVENT_LOG_CAP = 500;

export type AgentsTab = "roster" | "timeline" | "topology";

/** Roster status dot: pulsing = actively working (amber), live = idle-live
 *  (green), persisted-only = off (grey). */
function rosterDotClass(node: AgentNode, pulsing: Set<string>): string {
  if (node.status !== "live") return "dot off";
  return pulsing.has(node.sid) ? "dot busy" : "dot on";
}

/** Project-grouped, collapsible delegation tree. Component state contains
 *  only collapsed sids, so the initial render stays deterministic and SSR-safe. */
export function AgentsTree({
  nodes,
  edges,
  selected,
  pulsing,
  lang: langProp,
  onSelect,
}: {
  nodes: AgentNode[];
  edges: AgentEdge[];
  selected: string | null;
  pulsing: Set<string>;
  lang?: Lang;
  onSelect: (sid: string) => void;
}) {
  const lang = langProp ?? "zh";
  const t = makeT(langProp ?? "zh");
  const [collapsed, setCollapsed] = useState<Set<string>>(() => new Set());
  const projects = useMemo(() => groupDelegationTrees(nodes, collapsed), [nodes, collapsed]);
  const delegating = useMemo(
    () => new Set(edges.filter((edge) => edge.active).map((edge) => edge.child)),
    [edges],
  );

  const toggleCollapsed = (sid: string) => {
    setCollapsed((previous) => {
      const next = new Set(previous);
      if (next.has(sid)) next.delete(sid);
      else next.add(sid);
      return next;
    });
  };

  return (
    <div className="agents-tree" data-testid="agents-tree" role="tree" aria-label={t("team")}>
      {projects.map((project) => (
        <section className="agents-tree-project" data-testid={`agents-tree-project-${project.slug}`} key={project.slug}>
          <header className="agents-tree-project-head">
            <span className="mono">{project.slug}</span>
            <span>{project.liveCount}/{project.totalCount} {t("teamKpiLive")}</span>
          </header>
          {project.rows.map(({ node: n, indent, hasChildren }) => {
            const isCollapsed = collapsed.has(n.sid);
            const isDelegating = delegating.has(n.sid);
            return (
              <div
                key={n.sid}
                role="treeitem"
                aria-level={indent + 1}
                aria-expanded={hasChildren ? !isCollapsed : undefined}
                tabIndex={0}
                className={[
                  "agents-tree-row",
                  n.sid === selected ? "selected" : "",
                  isDelegating ? "delegating" : "",
                ].filter(Boolean).join(" ")}
                data-testid={`agents-tree-row-${n.sid}`}
                data-delegating={isDelegating ? "true" : "false"}
                onClick={() => onSelect(n.sid)}
                onKeyDown={(event) => {
                  if (event.key === "Enter") onSelect(n.sid);
                }}
              >
                <span
                  className={indent > 0 ? "agents-tree-indent has-parent" : "agents-tree-indent"}
                  style={{ width: indent * 16 }}
                  aria-hidden="true"
                />
                {hasChildren ? (
                  <button
                    type="button"
                    className="agents-tree-toggle"
                    aria-label={isCollapsed ? t("expand") : t("collapse")}
                    onClick={(event) => {
                      event.stopPropagation();
                      toggleCollapsed(n.sid);
                    }}
                  >
                    {isCollapsed ? "›" : "⌄"}
                  </button>
                ) : (
                  <span className="agents-tree-toggle-spacer" aria-hidden="true" />
                )}
                <span className={vendorChipClass(n.vendor)}>{n.vendor}</span>
                <span className="agents-tree-sid mono">{n.sid}</span>
                <span className="agents-tree-title">{n.title || n.role || "—"}</span>
                <span className={rosterDotClass(n, pulsing)} aria-hidden="true" />
                <span className="agents-tree-active">{relativeTime(lang, n.last_active)}</span>
                <span className="agents-tree-cost mono">{n.cost_usd != null ? `$${n.cost_usd.toFixed(4)}` : "—"}</span>
                <span className="agents-tree-turns mono" title={t("teamColTurns")}>{n.turn_count}t</span>
              </div>
            );
          })}
        </section>
      ))}
    </div>
  );
}

/** Pure presentational roster table — one row per session in delegation-tree
 *  DFS order. Exported for SSR tests (fixture rows, no data loading). */
export function AgentsRoster({
  rows,
  selected,
  pulsing,
  lang: langProp,
  onSelect,
  onOpenChat,
}: {
  rows: RosterRow[];
  selected: string | null;
  pulsing: Set<string>;
  lang?: Lang;
  onSelect: (sid: string) => void;
  onOpenChat?: (sid: string) => void;
}) {
  const lang = langProp ?? "zh";
  const t = makeT(lang);
  return (
    <div className="agents-roster" data-testid="agents-roster" role="table" aria-label={t("team")}>
      <div className="agents-roster-head" role="row">
        <span>{t("teamColSession")}</span>
        <span>{t("vendor")}</span>
        <span>{t("host")}</span>
        <span>{t("cost")}</span>
        <span>{t("teamColTurns")}</span>
        <span>{t("teamColLastActive")}</span>
        <span />
      </div>
      {rows.map(({ node: n, indent }) => (
        <div
          key={n.sid}
          role="row"
          tabIndex={0}
          className={`agents-roster-row ${n.sid === selected ? "selected" : ""}`}
          data-testid={`agents-roster-row-${n.sid}`}
          onClick={() => onSelect(n.sid)}
          onKeyDown={(e) => {
            if (e.key === "Enter") onSelect(n.sid);
          }}
        >
          <span className="agents-roster-name" style={{ paddingLeft: indent * 22 }}>
            {indent > 0 ? <span className="agents-roster-elbow">└</span> : null}
            <span className={rosterDotClass(n, pulsing)} />
            <span className="t">{n.title || n.role || n.sid}</span>
            <span className="agents-roster-sid mono">{n.sid}</span>
          </span>
          <span>
            <span className={vendorDotClass(n.vendor)} /> {n.vendor}
          </span>
          <span className="mono">{n.host}</span>
          <span className="mono">{n.cost_usd != null ? `$${n.cost_usd.toFixed(4)}` : "—"}</span>
          <span className="mono">{n.turn_count}</span>
          <span>{relativeTime(lang, n.last_active)}</span>
          <span>
            <button
              type="button"
              className="btn ghost mini"
              onClick={(e) => {
                e.stopPropagation();
                onOpenChat?.(n.sid);
              }}
            >
              {t("teamOpenChat")}
            </button>
          </span>
        </div>
      ))}
    </div>
  );
}

export default function AgentsView({
  lang: langProp,
  onOpenChat,
  initialTab = "topology",
}: {
  lang?: Lang;
  isAdmin?: boolean;
  onOpenChat?: (sid: string) => void;
  /** Initial tab (tests / deep links); the user switches freely afterwards. */
  initialTab?: AgentsTab;
} = {}) {
  const lang = langProp ?? "zh";
  const t = makeT(lang);

  const [tab, setTab] = useState<AgentsTab>(initialTab);
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

  const roster = useMemo(() => flattenDelegationTree(graph.nodes), [graph.nodes]);

  const selectedNode = graph.nodes.find((node) => node.sid === selected) ?? null;

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
    () => [...graph.nodes].sort((a, b) => a.depth - b.depth || a.sid.localeCompare(b.sid)),
    [graph.nodes],
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

  // ---- KPI strip (client-side, from the same graph + SSE data) ----
  const liveCount = graph.nodes.filter((n) => n.status === "live").length;
  const workingCount = graph.nodes.filter((n) => pulsing.has(n.sid)).length;
  const activeDispatches = edges.filter((e) => e.active).length;
  const totalCost = graph.nodes.reduce((sum, n) => sum + (n.cost_usd ?? 0), 0);

  const empty = !loading && graph.nodes.length === 0;
  const showPanel = tab !== "timeline";

  return (
    <section className="view active agents-view" data-testid="agents-view">
      <header className="agents-head">
        <div>
          <h1>{t("team")}</h1>
          <p>{t("teamDesc")}</p>
        </div>
        <div className="agents-kpis" data-testid="agents-kpis">
          <span>
            <b>{liveCount}</b> {t("teamKpiLive")}
          </span>
          <span>
            <b>{workingCount}</b> {t("teamKpiWorking")}
          </span>
          <span>
            <b>{activeDispatches}</b> {t("teamKpiDispatches")}
          </span>
          <span>
            <b>${totalCost.toFixed(2)}</b> {t("teamKpiCost")}
          </span>
        </div>
      </header>

      <div className="seg agents-tabs" data-testid="agents-tabs">
        {(
          [
            ["topology", t("teamTabTopology")],
            ["roster", t("teamTabRoster")],
            ["timeline", t("teamTabTimeline")],
          ] as [AgentsTab, string][]
        ).map(([id, label]) => (
          <button
            key={id}
            type="button"
            data-testid={`agents-tab-${id}`}
            className={tab === id ? "active" : ""}
            onClick={() => setTab(id)}
          >
            {label}
          </button>
        ))}
      </div>

      <div className="agents-body">
        <div className="agents-canvas" data-testid="agents-canvas">
          {tab !== "timeline" && loading ? (
            <p style={{ color: "var(--text-faint)", fontSize: 13, padding: 16 }}>{t("loading")}</p>
          ) : tab !== "timeline" && empty ? (
            <p style={{ color: "var(--text-faint)", fontSize: 13, padding: 16 }} data-testid="agents-empty">
              {t("teamEmpty")}
            </p>
          ) : tab === "roster" ? (
            <AgentsRoster
              rows={roster}
              selected={selected}
              pulsing={pulsing}
              lang={lang}
              onSelect={setSelected}
              onOpenChat={onOpenChat}
            />
          ) : tab === "topology" ? (
            <AgentsTree
              nodes={graph.nodes}
              edges={edges}
              selected={selected}
              pulsing={pulsing}
              lang={lang}
              onSelect={setSelected}
            />
          ) : (
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
              <svg
                className="agents-timeline-arrows"
                viewBox={`0 0 100 ${Math.max(timelineNodes.length, 1) * 28}`}
                preserveAspectRatio="none"
              >
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
          )}
        </div>

        {showPanel && selectedNode ? (
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
        ) : showPanel && graph.nodes.length > 0 ? (
          <aside className="agents-panel agents-panel-empty" data-testid="agents-panel-empty">
            <p style={{ color: "var(--text-faint)", fontSize: 13 }}>{t("teamSelectHint")}</p>
          </aside>
        ) : null}
      </div>
    </section>
  );
}
