// V0.3.2 F55 + F59 — Session detail page.
//
// Consumes `/api/v1/projects/<slug>/sessions/<sid>` (docs/interfaces.md
// §16.3) and subscribes:
//   - `/sse/project/<slug>/<sid>`  via EventsLive
//   - `/sse/harness/<slug>/<sid>`  via HarnessPanel
//
// Layout:
//   - top:     status row (slug / sid / harness / tmux_session /
//              started_at / cost_label + status badge + PauseResumeButtons)
//   - middle:  HarnessPanel + terminal mount  (flex only)
//   - below:   EventsLive (scope=session) + BtwForm (sid-scoped) + Outbox
//
// Pre-V0.5.1: only flex projects returned a SessionDetail; non-flex
// slugs 404'd at the API. V0.5.1 F103c added workflow / multi_workflow
// support — the JSON's `kind` field discriminates ("flex" |
// "workflow" | "multi_workflow"). For non-flex kinds we hide the
// HarnessPanel + TerminalView (no harness mirror file, no tmux mount)
// and surface a small notice; the EventsLive + BtwForm + Outbox panels
// stay because workflow sessions still emit progress events and accept
// btw injections.
//
// `paused` defaults `false`: the F52 SessionDetail JSON doesn't surface
// the pause flag yet (the orchestrator's user_pause_pending is
// project-scoped per V0.3.1 F50). Operators looking for the live flag
// view should consult ProjectDetail's pause/resume buttons.
//
// V0.4.0 F68: the "Pending decisions" collapsible (phase decision graph)
// was dropped — phase machinery retired in F60. Workflow-axis decisions
// (gate triggers, role spawns) live on the ProjectDetail page via
// WorkflowView, since they're project-scoped not session-scoped.

import { useEffect, useState } from "react";
import { Link, useParams } from "react-router-dom";
import {
  fetchSession,
  type SessionDetail as SessionDetailJson,
  type OutboxRow,
} from "../lib/detailApi";
import { EventsLive } from "../components/EventsLive";
import { HarnessPanel } from "../components/HarnessPanel";
import { BtwForm } from "../components/BtwForm";
import { PauseResumeButtons } from "../components/PauseResumeButtons";
import { TerminalView } from "../components/TerminalView";

function StatusBadge({ cls, label }: { cls: string; label: string }) {
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

export default function SessionDetail() {
  const { slug, sid } = useParams<{ slug: string; sid: string }>();
  const [detail, setDetail] = useState<SessionDetailJson | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  // F58 forms call `triggerReload` to refresh the JSON snapshot after
  // a successful write (so the next render reflects server state).
  const [reloadTick, setReloadTick] = useState(0);
  const triggerReload = () => setReloadTick((n) => n + 1);

  useEffect(() => {
    if (!slug || !sid) return;
    let cancelled = false;
    queueMicrotask(() => {
      if (cancelled) return;
      setLoading(true);
      setError(null);
      setDetail(null);
    });
    fetchSession(slug, sid)
      .then((data) => {
        if (!cancelled) setDetail(data);
      })
      .catch((e) => {
        if (!cancelled) setError(e instanceof Error ? e.message : String(e));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [slug, sid, reloadTick]);

  if (!slug || !sid) {
    return (
      <div className="p-6 text-text-dim text-sm">
        missing :slug or :sid route param
      </div>
    );
  }
  if (loading) {
    return (
      <div className="p-6 text-text-dim text-xs font-mono uppercase">
        loading session {slug}/{sid}…
      </div>
    );
  }
  if (error) {
    return (
      <div className="p-6 text-status-error text-sm font-mono">
        Failed to load session: {error}
      </div>
    );
  }
  if (!detail) {
    return <div className="p-6 text-text-dim text-sm">no data</div>;
  }

  // V0.5.1 F103c — `kind === "flex"` means full SessionDetail (with
  // HarnessPanel + terminal mount); workflow / multi_workflow projects
  // come through the new core branch and have no harness mirror / no
  // tmux mount. The boolean keeps the JSX below readable.
  const isFlex = detail.kind === "flex";
  return (
    <div className="flex flex-col h-full min-h-0 overflow-auto p-4 gap-4">
      {/* TOP — identity row + pause/resume */}
      <header className="flex flex-wrap items-baseline gap-3">
        <Link
          to={`/p/${encodeURIComponent(detail.slug)}`}
          className="text-xs text-text-dim hover:text-text-secondary font-mono"
        >
          ← {detail.slug}
        </Link>
        <h1 className="text-lg font-semibold text-text-bright">{detail.sid}</h1>
        <span className="text-xs text-text-secondary font-mono">
          harness={detail.harness}
        </span>
        <span className="text-xs text-text-secondary font-mono">
          tmux={detail.tmux_session}
        </span>
        <span className="text-xs text-text-secondary font-mono">
          started={detail.started_at}
        </span>
        <StatusBadge cls={detail.status_class} label={detail.status_label} />
        <span className="text-xs text-text-dim font-mono ml-auto">
          cost ${detail.cost_label}
        </span>
        <PauseResumeButtons
          slug={detail.slug}
          sid={detail.sid}
          paused={false}
          onSuccess={triggerReload}
        />
      </header>

      {/* V0.5.1 F103c — workflow / multi_workflow sessions don't have a
          harness mirror file or a tmux mount; surface a small notice
          and skip the HarnessPanel + TerminalView rows. */}
      {!isFlex && (
        <div
          data-testid="workflow-session-notice"
          className="text-[11px] text-text-dim font-mono px-2 py-1 border border-surface-700/40 rounded-md bg-surface-850"
        >
          Workflow session — harness/terminal mount disabled (flex-only feature).
        </div>
      )}

      {/* MIDDLE — harness panel + terminal placeholder (flex only). */}
      {isFlex && (
        <div className="grid grid-cols-1 lg:grid-cols-3 gap-4">
          <HarnessPanel
            slug={detail.slug}
            sid={detail.sid}
            snapshot={detail.harness_snapshot}
          />
          <TerminalView
            slug={detail.slug}
            sid={detail.sid}
            className="lg:col-span-2 border border-surface-700/40 rounded-md bg-surface-950 min-h-[16rem]"
          />
        </div>
      )}

      {/* BELOW — events + (BTW + outbox). The session-scoped BtwForm
          routes to `/api/<slug>/<sid>/btw` (per V0.3.1 F50 session
          inbox), keeping cross-session traffic isolated. */}
      <div className="grid grid-cols-1 lg:grid-cols-3 gap-4 min-h-0 flex-1">
        <div className="lg:col-span-2 min-h-[24rem] flex">
          <EventsLive
            scope={{ kind: "session", slug: detail.slug, sid: detail.sid }}
            initialEvents={detail.events}
          />
        </div>
        <div className="min-h-[24rem] flex flex-col gap-4">
          <section className="border border-surface-700/40 rounded-md bg-surface-850 p-3">
            <h3 className="text-xs uppercase tracking-wide text-text-secondary mb-2">
              BTW (session inbox)
            </h3>
            <BtwForm
              slug={detail.slug}
              sid={detail.sid}
              onSuccess={triggerReload}
            />
          </section>
          <OutboxList rows={detail.outbox} />
        </div>
      </div>
    </div>
  );
}
