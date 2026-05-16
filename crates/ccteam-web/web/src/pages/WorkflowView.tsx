// V0.4.0 F68 — Workflow view.
//
// Renders `ProjectSummary.workflow_summary` (a `WorkflowSummary` —
// see lib/detailApi.ts) as the operational SPA panel for V0.4.0
// artifact-driven workflow projects. Replaces the V0.3.x phase panel
// (retired in F60).
//
// V0.4.6 F90 expansion: each agent card now expands on click to show
// the live session list (job_id / age / cwd / cost) served by
// `GET /api/v1/projects/<slug>/sessions/active`. Errored cards also
// surface a "view log" affordance that pops `FailureInspector` so the
// operator can read the failed bg job's `output.log` tail. Plus four
// new sibling panels mounted by the parent page (`ArtifactQueuePanel`,
// `EventsTimelinePanel`, `CostSparkline`, `FailureInspector`); this
// file owns only the agent grid + session expansion. Parent
// `ProjectDetail` wires the panels.
//
// Layout:
//   - header   : workflow_name + roll-up cost / escalation count
//   - agents[] : one card per role — running / queued counts, total
//                cost, last session status, "force trigger" action when
//                the role is a Gate. Clicking the card expands an
//                active-session list (V0.4.6 F90).
//   - artifacts: one row per declared input/output dir → file count +
//                progress bar (retained from F68; F90 adds a sibling
//                ArtifactQueuePanel for fresh queue stats from disk)
//   - gates    : one chip per Gate role with Waiting / Fired state
//
// Live updates: subscribes to the project-level SSE topic and patches
// the matching agent card on incoming `agent_spawn` / `agent_done` /
// `gate_triggered` / `artifact_landed` events (no separate polling
// loop — F68 red line). Counts and cost reflect the SSE-delta path
// until the next REST refresh, then drift back to authoritative.
//
// Red line: this component **never** opens its own polling timer or
// session-attached watcher. Live data arrives via the existing
// `/sse/project/<slug>` topic (`useProgressStream`) and via the
// parent page's REST refetch on `reloadTick`.

import { useEffect, useMemo, useState } from "react";
import type {
  AgentSessionStatus,
  AgentStatus,
  WorkflowSummary,
} from "../lib/detailApi";
import {
  useProgressStream,
  type ProgressEvent,
} from "../hooks/useProgressStream";
import { toastBus } from "../lib/toastBus";
import {
  fetchActiveSessions,
  ageLabel,
  basename,
  type ActiveSessionInfo,
} from "../lib/workflowPanels";
import FailureInspector from "../components/FailureInspector";

interface Props {
  slug: string;
  summary: WorkflowSummary;
  /** Bump after a successful write (`/api/.../trigger_gate` etc.) so the
   *  parent page refetches the authoritative REST snapshot. */
  onReload?: () => void;
}

function statusLabel(s: AgentSessionStatus | null): string {
  if (!s) return "—";
  if (s.status === "running") return "running";
  if (s.status === "done") return `done $${s.cost_usd.toFixed(2)}`;
  return `errored $${s.cost_usd.toFixed(2)}`;
}

function statusBadgeClass(s: AgentSessionStatus | null): string {
  if (!s) return "text-text-dim border-surface-700/40 bg-surface-800";
  if (s.status === "running")
    return "text-status-running border-status-running/40 bg-status-running/10";
  if (s.status === "done")
    return "text-status-waiting border-status-waiting/40 bg-status-waiting/10";
  return "text-status-error border-status-error/40 bg-status-error/10";
}

function AgentCard({
  agent,
  gateState,
  onTriggerGate,
  expanded,
  onToggleExpand,
  sessions,
  onInspectJob,
}: {
  agent: AgentStatus;
  gateState?: string;
  onTriggerGate?: (role: string) => void;
  /** V0.4.6 F90 — clicked-open state for the session list expansion. */
  expanded?: boolean;
  /** V0.4.6 F90 — callback bound to the card click; toggles expansion. */
  onToggleExpand?: (role: string) => void;
  /** V0.4.6 F90 — live session list for this role (slice already
   *  filtered by parent). Empty array → "no running sessions" hint. */
  sessions?: ActiveSessionInfo[];
  /** V0.4.6 F90 — open the FailureInspector for a job_id. */
  onInspectJob?: (jobId: string) => void;
}) {
  const isGate = gateState !== undefined;
  // V0.4.5 F80 — surface a live "agent active" indicator alongside the
  // role label when running_count > 0. The count badge alone was easy
  // to miss; the pulsing dot makes "this agent is doing something right
  // now" obvious at a glance. Animation lives in index.css
  // `.agent-active-dot` (slow pulse, prefers-reduced-motion honored).
  const isActive = agent.running_count > 0;
  const lastStatus = agent.last_session_status;
  const isErrored = lastStatus?.status === "errored";
  // The card itself is the click target so the operator can tap
  // anywhere in the header / counts area to expand. We stop event
  // propagation on the inner button so "force trigger" doesn't also
  // toggle the expansion.
  return (
    <div
      className={
        "border border-surface-700/40 rounded-md bg-surface-850 p-3 flex flex-col gap-1.5 min-w-0 " +
        (onToggleExpand ? "cursor-pointer hover:bg-surface-800/60" : "")
      }
      onClick={() => onToggleExpand?.(agent.role)}
      role={onToggleExpand ? "button" : undefined}
      tabIndex={onToggleExpand ? 0 : undefined}
      aria-expanded={onToggleExpand ? expanded : undefined}
      onKeyDown={
        onToggleExpand
          ? (e) => {
              if (e.key === "Enter" || e.key === " ") {
                e.preventDefault();
                onToggleExpand(agent.role);
              }
            }
          : undefined
      }
    >
      <div className="flex items-center gap-2 min-w-0">
        {isActive && (
          <span
            className="agent-active-dot shrink-0"
            aria-label={`agent ${agent.role} active`}
            role="status"
            title={`${agent.running_count} session${agent.running_count > 1 ? "s" : ""} running`}
          />
        )}
        <span
          className="font-mono text-sm text-text-primary truncate flex-1"
          title={agent.role}
        >
          {agent.role}
        </span>
        {isGate && (
          <span
            className={
              "shrink-0 px-1.5 py-0.5 rounded text-[10px] font-mono uppercase tracking-wider border " +
              (gateState === "fired"
                ? "text-status-running border-status-running/40 bg-status-running/10"
                : "text-status-waiting border-status-waiting/40 bg-status-waiting/10")
            }
          >
            gate {gateState}
          </span>
        )}
      </div>
      <div className="flex flex-wrap gap-3 text-[11px] font-mono text-text-dim">
        <span>running={agent.running_count}</span>
        <span>queued={agent.queued_count}</span>
        <span>cost=${agent.total_cost_usd.toFixed(2)}</span>
      </div>
      <div className="flex items-center justify-between gap-2">
        <span
          className={
            "inline-flex items-center px-2 py-0.5 rounded text-[10px] font-mono uppercase tracking-wide border " +
            statusBadgeClass(lastStatus)
          }
        >
          {statusLabel(lastStatus)}
        </span>
        {isGate && gateState !== "fired" && onTriggerGate && (
          <button
            onClick={(e) => {
              e.stopPropagation();
              onTriggerGate(agent.role);
            }}
            type="button"
            className="px-2 py-0.5 rounded text-[10px] font-mono uppercase tracking-wide border border-brand-600/40 bg-brand-600/10 text-brand-500 hover:bg-brand-600/20 transition-colors cursor-pointer"
          >
            force trigger
          </button>
        )}
      </div>
      {/* V0.4.6 F90 — expanded session list */}
      {expanded && (
        <div
          className="mt-1 border-t border-surface-700/30 pt-2 flex flex-col gap-1 font-mono text-[10px]"
          onClick={(e) => e.stopPropagation()}
        >
          {sessions && sessions.length > 0 ? (
            sessions.slice(0, 5).map((s) => (
              <div
                key={s.session_id}
                className="flex items-center justify-between gap-2 min-w-0"
              >
                <div className="flex flex-col min-w-0">
                  <span className="text-text-primary truncate" title={s.session_id}>
                    {s.job_id ? s.job_id.slice(0, 8) : s.session_id}
                  </span>
                  <span className="text-text-dim truncate" title={s.cwd ?? ""}>
                    cwd: {basename(s.cwd)}
                  </span>
                </div>
                <div className="flex flex-col items-end shrink-0">
                  <span className="text-text-dim">
                    {ageLabel(secondsSince(s.started_at))} ago
                  </span>
                  <span className="text-text-secondary">
                    ${s.cost_usd.toFixed(2)}
                  </span>
                </div>
              </div>
            ))
          ) : (
            <div className="text-text-dim italic">no running sessions</div>
          )}
          {/* "view log" button: surfaced when the last session is errored
              and the parent supplied an inspector callback. We rely on
              the most-recent ActiveSessionInfo for the job_id; if the
              card has no live sessions but is marked errored, we fall
              back to an info hint. */}
          {isErrored && onInspectJob && sessions && sessions.length > 0 && (
            <button
              type="button"
              onClick={(e) => {
                e.stopPropagation();
                const job = sessions[0]?.job_id;
                if (job) onInspectJob(job);
              }}
              className="self-start mt-1 px-2 py-0.5 rounded text-[10px] font-mono uppercase tracking-wide border border-status-error/40 bg-status-error/10 text-status-error hover:bg-status-error/20 transition-colors cursor-pointer"
            >
              view log
            </button>
          )}
        </div>
      )}
    </div>
  );
}

/** Best-effort `Date.now() - parsedTs` in seconds. Returns `null` when
 *  `ts` is unparseable. */
function secondsSince(ts: string): number | null {
  const dt = Date.parse(ts);
  if (Number.isNaN(dt)) return null;
  return Math.max(0, Math.floor((Date.now() - dt) / 1000));
}

function ArtifactRow({
  path,
  count,
  maxCount,
}: {
  path: string;
  count: number;
  maxCount: number;
}) {
  const pct = maxCount > 0 ? Math.round((count / maxCount) * 100) : 0;
  return (
    <div className="flex flex-col gap-1 min-w-0">
      <div className="flex justify-between gap-3 text-[11px] font-mono">
        <span className="text-text-secondary truncate" title={path}>
          {path}
        </span>
        <span className="text-text-dim shrink-0">{count}</span>
      </div>
      <div className="h-1 bg-surface-800 rounded overflow-hidden">
        <div
          className="h-full bg-brand-600/60"
          style={{ width: `${pct}%` }}
          aria-label={`${count} artifacts`}
        />
      </div>
    </div>
  );
}

/** Apply a streamed progress event to the local workflow summary
 *  shadow. Pure (returns a new `WorkflowSummary` if anything changed,
 *  or the same reference if not) so React's diff stays cheap. */
export function applyEventToSummary(
  current: WorkflowSummary,
  event: ProgressEvent,
): WorkflowSummary {
  const kind = event.event;
  const role = (event as Record<string, unknown>).role;
  if (typeof role !== "string" || role.length === 0) return current;

  switch (kind) {
    case "agent_spawn": {
      const next = current.agents.map((a) =>
        a.role === role
          ? {
              ...a,
              running_count: a.running_count + 1,
              last_session_status: { status: "running" } as AgentSessionStatus,
            }
          : a,
      );
      // Orphan role (not declared in spec) — synth a card so the UI sees it.
      if (!next.some((a) => a.role === role)) {
        next.push({
          role,
          running_count: 1,
          queued_count: 0,
          total_cost_usd: 0,
          last_session_status: { status: "running" },
        });
      }
      return { ...current, agents: next };
    }
    case "agent_done": {
      const rawStatus = (event as Record<string, unknown>).status;
      const rawCost = (event as Record<string, unknown>).cost_usd;
      const status = typeof rawStatus === "string" ? rawStatus : "completed";
      const cost = typeof rawCost === "number" ? rawCost : 0;
      const terminal: AgentSessionStatus =
        status === "completed" || status === "stopped"
          ? { status: "done", cost_usd: cost }
          : { status: "errored", cost_usd: cost };
      const next = current.agents.map((a) =>
        a.role === role
          ? {
              ...a,
              running_count: Math.max(0, a.running_count - 1),
              total_cost_usd: a.total_cost_usd + cost,
              last_session_status: terminal,
            }
          : a,
      );
      return {
        ...current,
        agents: next,
        total_cost_usd: current.total_cost_usd + cost,
      };
    }
    case "gate_triggered": {
      // role is already validated above
      return {
        ...current,
        gate_states: { ...current.gate_states, [role]: "fired" },
      };
    }
    case "escalation": {
      return {
        ...current,
        escalation_count: current.escalation_count + 1,
      };
    }
    default:
      return current;
  }
}

export default function WorkflowView({ slug, summary, onReload }: Props) {
  // Local shadow of summary so SSE deltas show up without waiting for
  // the next REST refetch. Whenever the parent re-supplies (e.g. after
  // reloadTick), we replace the shadow with the authoritative copy.
  const [local, setLocal] = useState<WorkflowSummary>(summary);
  useEffect(() => {
    setLocal(summary);
  }, [summary]);

  const stream = useProgressStream({
    scope: { kind: "project", slug },
  });

  const latest: ProgressEvent | undefined =
    stream.events[stream.events.length - 1];
  useEffect(() => {
    if (!latest) return;
    setLocal((prev) => applyEventToSummary(prev, latest));
  }, [latest]);

  // V0.4.6 F90 — active session list (one fetch per workflow agent
  // open/close cycle). Re-fetch on agent_spawn / agent_done events so
  // the expansion stays roughly fresh without polling.
  const [active, setActive] = useState<ActiveSessionInfo[]>([]);
  const [expandedRole, setExpandedRole] = useState<string | null>(null);
  const [inspectJobId, setInspectJobId] = useState<string | null>(null);
  useEffect(() => {
    let cancelled = false;
    fetchActiveSessions(slug)
      .then((rows) => {
        if (!cancelled) setActive(rows);
      })
      .catch(() => {
        // Silent — expansion shows "no running sessions" until the next
        // refresh attempt succeeds. We don't want a transient network
        // hiccup to gray out the whole workflow view.
      });
    return () => {
      cancelled = true;
    };
  }, [slug]);
  useEffect(() => {
    if (!latest) return;
    if (latest.event !== "agent_spawn" && latest.event !== "agent_done")
      return;
    let cancelled = false;
    fetchActiveSessions(slug)
      .then((rows) => {
        if (!cancelled) setActive(rows);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [slug, latest]);
  const sessionsByRole = useMemo(() => {
    const out = new Map<string, ActiveSessionInfo[]>();
    for (const s of active) {
      const arr = out.get(s.role) ?? [];
      arr.push(s);
      out.set(s.role, arr);
    }
    return out;
  }, [active]);

  const maxCount = useMemo(() => {
    const vals = Object.values(local.artifact_counts);
    if (vals.length === 0) return 0;
    return Math.max(...vals, 1);
  }, [local.artifact_counts]);

  const sortedArtifacts = useMemo(() => {
    return Object.entries(local.artifact_counts).sort(([a], [b]) =>
      a.localeCompare(b),
    );
  }, [local.artifact_counts]);

  const handleTriggerGate = (role: string) => {
    // F68: the spawn / trigger endpoints are wired in V0.4.1; for now
    // we surface a toast pointing the operator at the MCP tool so the
    // SPA stays honest about its capability set.
    toastBus.handler?.info(
      `Force-trigger for "${role}" is not yet wired in the SPA. ` +
        `Use the ccteam meta-agent MCP tool ccteam__trigger_gate (slug=${slug}, role=${role}).`,
    );
    onReload?.();
  };

  const handleToggleExpand = (role: string) => {
    setExpandedRole((curr) => (curr === role ? null : role));
  };

  if (!local.workflow_name && local.agents.length === 0) {
    return (
      <section className="border border-surface-700/40 rounded-md bg-surface-850 p-4 text-xs font-mono text-text-dim">
        <div className="mb-1 uppercase tracking-wide text-text-secondary">
          Workflow
        </div>
        No workflow.yaml for this project. Legacy V0.3.x slug — workflow
        view is empty until the project is migrated to V0.4.0.
      </section>
    );
  }

  return (
    <section className="border border-surface-700/40 rounded-md bg-surface-850 flex flex-col gap-3 p-3">
      <header className="flex flex-wrap items-baseline gap-3">
        <h2 className="text-sm font-semibold text-text-bright">
          Workflow: <span className="font-mono">{local.workflow_name || "—"}</span>
        </h2>
        <span className="text-[11px] font-mono text-text-dim">
          cost ${local.total_cost_usd.toFixed(2)}
        </span>
        <span className="text-[11px] font-mono text-text-dim">
          escalations {local.escalation_count}
        </span>
        <span
          className={
            "text-[10px] font-mono ml-auto " +
            (stream.connected ? "text-status-running" : "text-text-dim")
          }
          title={stream.lastError ?? undefined}
        >
          {stream.connected ? "live" : "off"}
        </span>
      </header>

      {/* Agents */}
      {local.agents.length === 0 ? (
        <div className="text-xs text-text-dim font-mono px-1 py-2">
          No agents yet — workflow.yaml has no roles, or none have spawned.
        </div>
      ) : (
        <div className="grid grid-cols-1 md:grid-cols-2 xl:grid-cols-3 gap-2">
          {local.agents.map((a) => (
            <AgentCard
              key={a.role}
              agent={a}
              gateState={local.gate_states[a.role]}
              onTriggerGate={handleTriggerGate}
              expanded={expandedRole === a.role}
              onToggleExpand={handleToggleExpand}
              sessions={sessionsByRole.get(a.role) ?? []}
              onInspectJob={(jobId) => setInspectJobId(jobId)}
            />
          ))}
        </div>
      )}

      {/* Artifacts (legacy F68 input/output dir counts — sibling
          ArtifactQueuePanel adds richer watch-path info; this row stays
          for parity with output dirs that aren't watch triggers). */}
      {sortedArtifacts.length > 0 && (
        <div className="flex flex-col gap-1.5 pt-2 border-t border-surface-700/30">
          <h3 className="text-xs uppercase tracking-wide text-text-secondary">
            Artifacts
          </h3>
          <div className="flex flex-col gap-2">
            {sortedArtifacts.map(([path, count]) => (
              <ArtifactRow
                key={path}
                path={path}
                count={count}
                maxCount={maxCount}
              />
            ))}
          </div>
        </div>
      )}

      {/* V0.4.6 F90 — failure inspector modal. Closed by default;
          opens when an errored agent card's "view log" button fires. */}
      <FailureInspector
        slug={slug}
        jobId={inspectJobId}
        onClose={() => setInspectJobId(null)}
      />
    </section>
  );
}
