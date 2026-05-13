// V0.3.2 F55 — Session detail page.
//
// Consumes `/api/v1/projects/<slug>/sessions/<sid>` (docs/interfaces.md
// §16.3) and subscribes:
//   - `/sse/project/<slug>/<sid>`  via EventsLive
//   - `/sse/harness/<slug>/<sid>`  via HarnessPanel
//
// Layout (per F55 spec):
//   - top:     status row (slug / sid / harness / tmux_session /
//              started_at / cost_label + status badge)
//   - middle:  HarnessPanel
//   - below:   EventsLive (scope=session)
//   - side:    OutboxList
//   - reserved: <div id="terminal-mount" /> — F57 mounts TerminalView
//   - debug:   collapsible Pending decisions list
//
// Only flex projects return a SessionDetail; non-flex slugs 404 at
// the API and surface as "Not found" here.

import { useEffect, useState } from "react";
import { Link, useParams } from "react-router-dom";
import {
  fetchSession,
  type SessionDetail as SessionDetailJson,
  type OutboxRow,
} from "../lib/detailApi";
import { EventsLive } from "../components/EventsLive";
import { HarnessPanel } from "../components/HarnessPanel";

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

  useEffect(() => {
    if (!slug || !sid) return;
    let cancelled = false;
    setLoading(true);
    setError(null);
    setDetail(null);
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
  }, [slug, sid]);

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

  return (
    <div className="flex flex-col h-full min-h-0 overflow-auto p-4 gap-4">
      {/* TOP — identity row */}
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
      </header>

      {/* MIDDLE — harness panel + terminal placeholder */}
      <div className="grid grid-cols-1 lg:grid-cols-3 gap-4">
        <HarnessPanel
          slug={detail.slug}
          sid={detail.sid}
          snapshot={detail.harness_snapshot}
        />
        {/* TODO(F57): mount <TerminalView /> here. F57 owns
            useTerminal.ts + TerminalView.tsx and wires the WS PTY
            relay (F56 backend already shipped). */}
        <div
          id="terminal-mount"
          className="lg:col-span-2 border border-surface-700/40 rounded-md bg-surface-950 min-h-[16rem] flex items-center justify-center"
        >
          <span className="text-xs font-mono uppercase tracking-wide text-text-dim">
            terminal mount — F57
          </span>
        </div>
      </div>

      {/* BELOW — events + outbox */}
      <div className="grid grid-cols-1 lg:grid-cols-3 gap-4 min-h-0 flex-1">
        <div className="lg:col-span-2 min-h-[24rem] flex">
          <EventsLive
            scope={{ kind: "session", slug: detail.slug, sid: detail.sid }}
            initialEvents={detail.events}
          />
        </div>
        <div className="min-h-[24rem] flex">
          <OutboxList rows={detail.outbox} />
        </div>
      </div>

      {/* Collapsible decisions debug list */}
      <details className="border border-surface-700/40 rounded-md bg-surface-850">
        <summary className="px-3 py-2 cursor-pointer text-xs uppercase tracking-wide text-text-secondary">
          Pending decisions ({detail.decision_candidates.length})
        </summary>
        <ul className="px-3 py-2 font-mono text-xs space-y-1">
          {detail.decision_candidates.length === 0 && (
            <li className="text-text-dim italic">none</li>
          )}
          {detail.decision_candidates.map((p) => (
            <li key={p} className="text-text-secondary truncate" title={p}>
              {p}
            </li>
          ))}
        </ul>
      </details>
    </div>
  );
}
