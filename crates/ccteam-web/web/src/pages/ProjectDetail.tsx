// V0.3.2 F55 + F59 — Project detail page.
//
// Consumes `/api/v1/projects/<slug>` (docs/interfaces.md §16.2) and
// subscribes `/sse/project/<slug>` via EventsLive for live event
// updates.
//
// Layout:
//   - top:    team / kind / badge_label + PauseResumeButtons (F58)
//   - middle: WorkflowView (F68 — agent cards + artifact counts +
//             gate chips), driven by ProjectSummary.workflow_summary
//   - right:  EventsLive (scope=project) + BtwForm (F58) + OutboxList
//   - bottom: flex-only session tab strip (is_flex && sessions.length)
//
// V0.4.0 F68: the phase chip + InjectDecisionForm section are gone
// (phase machinery retired in F60). The workflow view replaces the
// "pending decisions" pane; meta-agent decisions now flow through the
// F65 MCP tools (`ccteam__trigger_gate` etc.).

import { useEffect, useState } from "react";
import { Link, useParams } from "react-router-dom";
import {
  fetchProject,
  type ProjectSummary,
  type OutboxRow,
  type SessionCard,
} from "../lib/detailApi";
import { BtwForm } from "../components/BtwForm";
import { PauseResumeButtons } from "../components/PauseResumeButtons";
import WorkflowView from "./WorkflowView";
import ArtifactStatusPanel from "../components/ArtifactStatusPanel";
import EventsTimelinePanel from "../components/EventsTimelinePanel";
import CostSparkline from "../components/CostSparkline";

/** Read `state.user_pause_pending` from the opaque `ProjectSummary.state`
 *  JSON blob without leaking the unknown shape through to consumers.
 *  Defaults to `false` so we render the "Pause" button enabled when
 *  the server hasn't populated the field. */
function readPaused(state: unknown): boolean {
  if (state && typeof state === "object" && "user_pause_pending" in state) {
    const v = (state as Record<string, unknown>).user_pause_pending;
    return v === true;
  }
  return false;
}

function StatusBadge({ cls, label }: { cls: string; label: string }) {
  // `badge_class` strings like "badge-ok" / "badge-warn" / "badge-err"
  // come straight from server templates. We don't have CSS classes
  // for those in the SPA, so we map the suffix to color tokens we DO
  // have, falling back to neutral surface.
  const suffix = cls.replace(/^badge-/, "");
  const color =
    suffix === "ok"
      ? "text-status-running border-status-running/40 bg-status-running/10"
      : suffix === "warn"
        ? "text-status-waiting border-status-waiting/40 bg-status-waiting/10"
        : suffix === "err" || suffix === "error"
          ? "text-status-error border-status-error/40 bg-status-error/10"
          : "text-text-secondary border-surface-700/40 bg-surface-800";
  return (
    <span
      className={
        "inline-flex items-center px-2 py-0.5 rounded text-[11px] font-mono uppercase tracking-wide border " +
        color
      }
    >
      {label}
    </span>
  );
}

function OutboxList({ rows }: { rows: OutboxRow[] }) {
  return (
    <section className="border border-surface-700/40 rounded-md bg-surface-850 flex flex-col min-h-0">
      <header className="px-3 py-2 border-b border-surface-700/30 shrink-0">
        <h3 className="text-xs uppercase tracking-wide text-text-secondary">
          Outbox
        </h3>
      </header>
      <ol className="flex-1 min-h-0 overflow-auto font-mono text-xs divide-y divide-surface-700/20">
        {rows.length === 0 && (
          <li className="px-3 py-2 text-text-dim italic">empty</li>
        )}
        {rows.map((r) => (
          // F58 may add "open in editor" — for now filename is plain text.
          <li key={r.filename} className="px-3 py-1.5">
            <div className="flex justify-between gap-3">
              <span className="text-text-primary truncate" title={r.filename}>
                {r.filename}
              </span>
              <span className="text-text-dim shrink-0">{r.kind}</span>
            </div>
            {r.preview && (
              <div className="text-text-muted text-[11px] truncate">
                {r.preview}
              </div>
            )}
            <div className="text-text-dim text-[10px]">{r.created_at}</div>
          </li>
        ))}
      </ol>
    </section>
  );
}

function SessionTab({ slug, card }: { slug: string; card: SessionCard }) {
  return (
    <Link
      to={`/p/${encodeURIComponent(slug)}/s/${encodeURIComponent(card.sid)}`}
      className="flex items-center gap-2 px-3 py-1.5 border border-surface-700/40 rounded-md bg-surface-800 hover:bg-surface-700/50 transition-colors text-xs"
    >
      <span className="font-mono text-text-primary">{card.sid}</span>
      <StatusBadge cls={card.status_class} label={card.status_label} />
      {card.harness && (
        <span className="text-text-dim text-[10px] uppercase">
          {card.harness}
        </span>
      )}
    </Link>
  );
}

export default function ProjectDetail() {
  const { slug } = useParams<{ slug: string }>();
  const [summary, setSummary] = useState<ProjectSummary | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  // Bumping `reloadTick` triggers a re-fetch. The F58 form components
  // call this via `onSuccess` so the visible state catches up with the
  // server after a successful write (pause flag, decision list, etc.).
  const [reloadTick, setReloadTick] = useState(0);
  const triggerReload = () => setReloadTick((n) => n + 1);

  useEffect(() => {
    if (!slug) return;
    let cancelled = false;
    queueMicrotask(() => {
      if (cancelled) return;
      setLoading(true);
      setError(null);
      setSummary(null);
    });
    fetchProject(slug)
      .then((data) => {
        if (!cancelled) setSummary(data);
      })
      .catch((e) => {
        if (!cancelled)
          setError(e instanceof Error ? e.message : String(e));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [slug, reloadTick]);

  if (!slug) {
    return (
      <div className="p-6 text-text-dim text-sm">missing :slug route param</div>
    );
  }
  if (loading) {
    return (
      <div className="p-6 text-text-dim text-xs font-mono uppercase">
        loading project {slug}…
      </div>
    );
  }
  if (error) {
    return (
      <div className="p-6 text-status-error text-sm font-mono">
        Failed to load project: {error}
      </div>
    );
  }
  if (!summary) {
    return <div className="p-6 text-text-dim text-sm">no data</div>;
  }

  const paused = readPaused(summary.state);

  return (
    <div className="flex flex-col h-full min-h-0 overflow-auto p-4 gap-4">
      {/* TOP — slim identity row. team/kind shown only on hover so we
          spend the prime real estate on the panels below. */}
      <header className="flex flex-wrap items-baseline gap-3">
        <h1
          className="text-lg font-semibold text-text-bright"
          title={`team=${summary.team} · kind=${summary.kind}`}
        >
          {summary.slug}
        </h1>
        <StatusBadge cls={summary.badge_class} label={summary.badge_label} />
        <span className="text-xs text-text-dim font-mono ml-auto">
          cost ${summary.cost_label}
        </span>
        <PauseResumeButtons
          slug={summary.slug}
          paused={paused}
          onSuccess={triggerReload}
        />
      </header>

      {/* Flex-only session tabs */}
      {summary.is_flex && summary.sessions.length > 0 && (
        <nav className="flex flex-wrap gap-2">
          {summary.sessions.map((s) => (
            <SessionTab key={s.sid} slug={summary.slug} card={s} />
          ))}
        </nav>
      )}

      {/* High-signal panels at the top: cost trend (2-col, wide) +
          artifact status counts (1-col). ArtifactQueuePanel was dropped
          — marker dirs are almost always empty, and the per-trigger
          watch path is already surfaced on the WorkflowView agent cards
          below. */}
      {summary.workflow_summary && (
        <div className="grid grid-cols-1 lg:grid-cols-3 gap-4">
          <div className="lg:col-span-2">
            <CostSparkline slug={summary.slug} reloadKey={reloadTick} />
          </div>
          <ArtifactStatusPanel slug={summary.slug} reloadKey={reloadTick} />
        </div>
      )}

      {/* Workflow view — agent cards + per-trigger watch counts +
          gate chips. `workflow_summary` is null for legacy projects
          without workflow.yaml; the component renders a friendly
          "no workflow.yaml" hint in that case. */}
      {summary.workflow_summary && (
        <WorkflowView
          slug={summary.slug}
          summary={summary.workflow_summary}
          onReload={triggerReload}
        />
      )}

      {/* Main two-column layout: events timeline + (BTW form + outbox).
          The timeline panel is bounded to 24rem so a long event tail
          scrolls internally instead of stretching the whole page. */}
      <div className="grid grid-cols-1 lg:grid-cols-3 gap-4 min-h-0 flex-1">
        <div className="lg:col-span-2 h-[24rem] flex">
          <EventsTimelinePanel
            slug={summary.slug}
            initialEvents={summary.events}
          />
        </div>
        <div className="min-h-[24rem] flex flex-col gap-4">
          <section className="border border-surface-700/40 rounded-md bg-surface-850 p-3">
            <h3 className="text-xs uppercase tracking-wide text-text-secondary mb-2">
              BTW (inject note)
            </h3>
            <BtwForm slug={summary.slug} onSuccess={triggerReload} />
          </section>
          <OutboxList rows={summary.outbox} />
        </div>
      </div>
    </div>
  );
}
