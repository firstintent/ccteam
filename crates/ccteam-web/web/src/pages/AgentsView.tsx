// 团队/Team view — topology-first (v0.9.11 TEAM-1). The roster + timeline
// tabs are gone; the live delegation topology is the single canvas (a later
// card adds a "charter" tab beside it):
//
// - 拓扑 topology: a compact, collapsible delegation tree grouped by project,
//   designed to stay readable with 100+ sessions. Every row (and the detail
//   panel) links to the real chat route `/chat/s/<sid>` — right/middle-click
//   open-in-new-tab works because these are real hyperlinks.
// - KPI strip (live / working / active dispatches / total cost) + per-vendor
//   chips (live count + Σcost per vendor; clicking a chip filters the
//   topology to that vendor, clicking it again clears the filter).
// - dispatch ticker: the last 5 delegation frames off the global SSE,
//   newest first — clicking one selects the child session.
// - host badge on tree rows ONLY when the graph spans more than one host.
//
// The view is available to every identity; the real ACL is the backend's
// per-tenant graph/SSE filter. Live state comes from the global SSE
// (`useAgentsEvents`) folded through `lib/agentsReducer.ts`; tree grouping
// from `lib/agentsTree.ts` (pure).

import { useEffect, useMemo, useRef, useState } from "react";
import { Link } from "react-router-dom";
import { fetchAgentsGraph, type AgentEdge, type AgentNode, type AgentsGraphResponse } from "../lib/agentsApi";
import {
  applyDelegationEvent,
  delegationToast,
  sidsActiveWithin,
  type TimestampedAgentsEvent,
} from "../lib/agentsReducer";
import { groupDelegationTrees } from "../lib/agentsTree";
import { useAgentsEvents } from "../hooks/useAgentsEvents";
import { VendorChip } from "../components/VendorChip";
import { getHistory, type SessionHistoryEvent } from "../lib/sessionsApi";
import { emptyFold, foldActivity, renderFold, type ActivityFold } from "./chatTranscript";
import { vendorDotClass } from "../lib/vendors";
import { makeT, type Lang } from "../lib/i18n";
import { relativeTime } from "./railHelpers";
import { toastBus } from "../lib/toastBus";

const PULSE_WINDOW_MS = 60_000;
const GRAPH_REFRESH_MS = 15_000;
const EVENT_LOG_CAP = 500;
const TICKER_SIZE = 5;

const chatPath = (sid: string) => `/chat/s/${encodeURIComponent(sid)}`;

/** Tree status dot: pulsing = actively working (amber), live = idle-live
 *  (green), persisted-only = off (grey). */
function treeDotClass(node: AgentNode, pulsing: Set<string>): string {
  if (node.status !== "live") return "dot off";
  return pulsing.has(node.sid) ? "dot busy" : "dot on";
}

/** Project-grouped, collapsible delegation tree. Component state contains
 *  only collapsed sids, so the initial render stays deterministic and SSR-safe.
 *  `hosts` = every host in the graph; the per-row host badge renders only
 *  when there is more than one. */
export function AgentsTree({
  nodes,
  edges,
  hosts = [],
  selected,
  pulsing,
  lang: langProp,
  onSelect,
}: {
  nodes: AgentNode[];
  edges: AgentEdge[];
  hosts?: string[];
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
  const showHost = hosts.length > 1;

  const toggleCollapsed = (sid: string) => {
    setCollapsed((previous) => {
      const next = new Set(previous);
      if (next.has(sid)) next.delete(sid);
      else next.add(sid);
      return next;
    });
  };

  return (
    <div
      className={showHost ? "agents-tree with-host" : "agents-tree"}
      data-testid="agents-tree"
      role="tree"
      aria-label={t("team")}
    >
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
                <VendorChip vendor={n.vendor} />
                <span className="agents-tree-sid mono">{n.sid}</span>
                <span className="agents-tree-title">{n.title || n.role || "—"}</span>
                {showHost ? <span className="agents-tree-host mono">{n.host}</span> : null}
                <span className={treeDotClass(n, pulsing)} aria-hidden="true" />
                <span className="agents-tree-active">{relativeTime(lang, n.last_active)}</span>
                <span className="agents-tree-cost mono">{n.cost_usd != null ? `$${n.cost_usd.toFixed(4)}` : "—"}</span>
                <span className="agents-tree-turns mono" title={t("teamColTurns")}>{n.turn_count}t</span>
                <Link
                  className="btn ghost mini agents-tree-open"
                  to={chatPath(n.sid)}
                  onClick={(event) => event.stopPropagation()}
                >
                  {t("teamOpenLink")}
                </Link>
              </div>
            );
          })}
        </section>
      ))}
    </div>
  );
}

/** Per-vendor rollup for the KPI chips: live session count + Σcost.
 *  Module-private (react-refresh: component files export only components);
 *  covered through {@link VendorKpiChips}' rendered output. */
function vendorRollup(nodes: AgentNode[]): { vendor: string; live: number; cost: number }[] {
  const byVendor = new Map<string, { vendor: string; live: number; cost: number }>();
  for (const n of nodes) {
    const agg = byVendor.get(n.vendor) ?? { vendor: n.vendor, live: 0, cost: 0 };
    if (n.status === "live") agg.live += 1;
    agg.cost += n.cost_usd ?? 0;
    byVendor.set(n.vendor, agg);
  }
  return [...byVendor.values()].sort((a, b) => a.vendor.localeCompare(b.vendor));
}

/** Per-vendor KPI chips row — hook-free presentational (exported for
 *  node-env tests). Clicking a chip toggles the topology's vendor filter. */
export function VendorKpiChips({
  nodes,
  active,
  onToggle,
}: {
  nodes: AgentNode[];
  active: string | null;
  onToggle: (vendor: string) => void;
}) {
  const rollup = vendorRollup(nodes);
  if (rollup.length === 0) return null;
  return (
    <div className="agents-vendor-chips" data-testid="agents-vendor-chips">
      {rollup.map(({ vendor, live, cost }) => (
        <button
          key={vendor}
          type="button"
          className={vendor === active ? "agents-vendor-chip active" : "agents-vendor-chip"}
          data-testid={`agents-vendor-chip-${vendor}`}
          aria-pressed={vendor === active}
          onClick={() => onToggle(vendor)}
        >
          <VendorChip vendor={vendor} />
          {/* Single-expression text nodes keep the SSR html free of comment
              separators, so tests can assert the literal strings. */}
          <span className="agents-vendor-live">{`●${live}`}</span>
          <span className="mono">{`$${cost.toFixed(2)}`}</span>
        </button>
      ))}
    </div>
  );
}

/** Recent-dispatch ticker: the last {@link TICKER_SIZE} delegation frames,
 *  newest first — `parent → child · relation · relative time`. Hook-free
 *  presentational (exported for node-env tests); hidden when empty. */
export function AgentsTicker({
  events,
  lang: langProp,
  onSelect,
}: {
  events: TimestampedAgentsEvent[];
  lang?: Lang;
  onSelect: (sid: string) => void;
}) {
  const lang = langProp ?? "zh";
  const t = makeT(lang);
  const recent = events
    .filter((ev) => ev.kind === "delegation" && ev.parent_sid && ev.child_sid)
    .slice(-TICKER_SIZE)
    .reverse();
  if (recent.length === 0) return null;
  return (
    <div className="agents-ticker" data-testid="agents-ticker">
      <span className="agents-ticker-label">{t("teamTicker")}</span>
      {recent.map((ev, i) => (
        <button
          key={`${ev.parent_sid}-${ev.child_sid}-${ev.receivedAt}-${i}`}
          type="button"
          className="agents-ticker-item"
          data-testid="agents-ticker-item"
          onClick={() => onSelect(ev.child_sid!)}
        >
          <span className="mono">{`${ev.parent_sid} → ${ev.child_sid}`}</span>
          <span aria-hidden="true">·</span>
          <span className="agents-ticker-relation">{ev.relation}</span>
          <span aria-hidden="true">·</span>
          <span className="agents-ticker-when">
            {relativeTime(lang, new Date(ev.receivedAt).toISOString())}
          </span>
        </button>
      ))}
    </div>
  );
}

/** Detail side panel for the selected session — hook-free presentational
 *  (exported for node-env tests). The 打开会话 action is a real `<Link>`. */
export function AgentsPanel({
  node,
  pulsing,
  activityFold,
  history,
  lang: langProp,
}: {
  node: AgentNode;
  pulsing: Set<string>;
  activityFold: ActivityFold;
  history: SessionHistoryEvent[];
  lang?: Lang;
}) {
  const lang = langProp ?? "zh";
  const t = makeT(lang);
  return (
    <aside className="agents-panel" data-testid="agents-panel">
      <h2>{node.title || node.role || node.sid}</h2>
      <div className="agents-panel-meta">
        <span>
          {t("vendor")}: <span className={vendorDotClass(node.vendor)} /> {node.vendor}
        </span>
        <span>
          {t("model")}: {node.model || "—"}
        </span>
        <span>
          {t("host")}: {node.host}
        </span>
        <span>
          {t("parentSession")}: {node.parent_sid || t("noParent")}
        </span>
        <span>
          {t("depth")}: {node.depth}
        </span>
        <span>
          {t("cost")}: {node.cost_usd != null ? `$${node.cost_usd.toFixed(4)}` : "—"}
        </span>
        <span>{relativeTime(lang, node.last_active)}</span>
      </div>

      <h3>{t("teamActivity")}</h3>
      <p className="agents-activity-line" data-testid="agents-activity-line">
        {pulsing.has(node.sid) ? renderFold(activityFold) : t("teamIdle")}
      </p>

      <h3>{t("teamRecentTurns")}</h3>
      <div className="agents-turns" data-testid="agents-turns">
        {history.length === 0 ? (
          <p style={{ fontSize: 12.5, color: "var(--text-faint)" }}>—</p>
        ) : (
          history.map((ev) => (
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

      <Link className="btn primary mini" data-testid="agents-open-chat" to={chatPath(node.sid)}>
        {t("teamOpenChat")}
      </Link>
    </aside>
  );
}

export default function AgentsView({ lang: langProp }: { lang?: Lang } = {}) {
  const lang = langProp ?? "zh";
  const t = makeT(lang);

  const [graph, setGraph] = useState<AgentsGraphResponse>({ nodes: [], edges: [], hosts: [] });
  const [edges, setEdges] = useState<AgentEdge[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [vendorFilter, setVendorFilter] = useState<string | null>(null);
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
  // timestamped log the pulse/activity/ticker all read from ----
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

  // Tick `now` so the pulse window + ticker auto-advance without needing a
  // fresh SSE frame.
  useEffect(() => {
    const id = window.setInterval(() => setNow(Date.now()), 3000);
    return () => window.clearInterval(id);
  }, []);

  const pulsing = useMemo(
    () => sidsActiveWithin(timestamped, PULSE_WINDOW_MS, now),
    [timestamped, now],
  );

  // Vendor filter: children whose parent is filtered out render as roots
  // (`groupDelegationTrees` treats an invisible parent as no parent).
  const visibleNodes = useMemo(
    () => (vendorFilter ? graph.nodes.filter((n) => n.vendor === vendorFilter) : graph.nodes),
    [graph.nodes, vendorFilter],
  );

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

  // ---- KPI strip (client-side, from the same graph + SSE data) ----
  const liveCount = graph.nodes.filter((n) => n.status === "live").length;
  const workingCount = graph.nodes.filter((n) => pulsing.has(n.sid)).length;
  const activeDispatches = edges.filter((e) => e.active).length;
  const totalCost = graph.nodes.reduce((sum, n) => sum + (n.cost_usd ?? 0), 0);

  const empty = !loading && graph.nodes.length === 0;

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

      <VendorKpiChips
        nodes={graph.nodes}
        active={vendorFilter}
        onToggle={(vendor) => setVendorFilter((current) => (current === vendor ? null : vendor))}
      />
      <AgentsTicker events={timestamped} lang={lang} onSelect={setSelected} />

      <div className="agents-body">
        <div className="agents-canvas" data-testid="agents-canvas">
          {loading ? (
            <p style={{ color: "var(--text-faint)", fontSize: 13, padding: 16 }}>{t("loading")}</p>
          ) : empty ? (
            <p style={{ color: "var(--text-faint)", fontSize: 13, padding: 16 }} data-testid="agents-empty">
              {t("teamEmpty")}
            </p>
          ) : (
            <AgentsTree
              nodes={visibleNodes}
              edges={edges}
              hosts={graph.hosts}
              selected={selected}
              pulsing={pulsing}
              lang={lang}
              onSelect={setSelected}
            />
          )}
        </div>

        {selectedNode ? (
          <AgentsPanel
            node={selectedNode}
            pulsing={pulsing}
            activityFold={activityFold}
            history={historyBySid[selectedNode.sid] ?? []}
            lang={lang}
          />
        ) : graph.nodes.length > 0 ? (
          <aside className="agents-panel agents-panel-empty" data-testid="agents-panel-empty">
            <p style={{ color: "var(--text-faint)", fontSize: 13 }}>{t("teamSelectHint")}</p>
          </aside>
        ) : null}
      </div>
    </section>
  );
}
