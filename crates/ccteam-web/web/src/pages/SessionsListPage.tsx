// V0.5.1 F103a — global `/sessions` top-level list.
//
// Renders one card per live `agent_spawn` across every project. Pulls
// from `/api/v1/sessions/active` (the aggregate handler added in F103a).
// Clicking a card routes to `/p/<slug>/s/<session_id>` which, post-F103c,
// resolves for both flex and workflow projects.
//
// Auth: 401 throws `UNAUTHENTICATED` and the global TokenEntryGate
// (App.tsx) handles the swap. Other failures render an inline banner so
// transient network blips don't blank the tab.

import { useEffect, useState } from "react";
import { Link } from "react-router-dom";
import {
  fetchAllActiveSessions,
  type ActiveSessionWithSlug,
} from "../lib/listApi";
import { ageLabel, basename } from "../lib/workflowPanels";

/** Best-effort `Date.now() - parsedTs` in seconds. Returns `null` when
 *  `ts` is unparseable (mirrors WorkflowView). */
function secondsSince(ts: string): number | null {
  const dt = Date.parse(ts);
  if (Number.isNaN(dt)) return null;
  return Math.max(0, Math.floor((Date.now() - dt) / 1000));
}

export default function SessionsListPage() {
  const [sessions, setSessions] = useState<ActiveSessionWithSlug[] | null>(
    null,
  );
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    // No need to reset state here — useState already initialised
    // both to `null`, and `react-hooks/set-state-in-effect` flags the
    // redundant calls. Async state updates land via .then / .catch
    // below, which the rule allows since they happen outside the
    // synchronous effect body.
    fetchAllActiveSessions()
      .then((rows) => {
        if (!cancelled) setSessions(rows);
      })
      .catch((err) => {
        if (cancelled) return;
        const msg = err instanceof Error ? err.message : String(err);
        if (msg !== "UNAUTHENTICATED") setError(msg);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  if (error) {
    return (
      <div
        data-testid="sessions-error"
        className="p-4 text-xs text-status-error font-mono"
        role="alert"
      >
        failed to load sessions: {error}
      </div>
    );
  }
  if (sessions === null) {
    return (
      <div
        data-testid="sessions-loading"
        className="p-4 text-xs text-text-dim font-mono"
      >
        loading sessions…
      </div>
    );
  }
  if (sessions.length === 0) {
    return (
      <div
        data-testid="sessions-empty"
        className="p-6 text-xs text-text-dim font-mono flex flex-col gap-2"
      >
        <span>No running sessions.</span>
        <span>
          Spawn via <code>ccteam start &lt;slug&gt;</code> or drop a trigger
          artifact.
        </span>
      </div>
    );
  }
  return (
    <div data-testid="sessions-list" className="p-4">
      <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-3">
        {sessions.map((s) => (
          <SessionCard key={`${s.slug}:${s.session_id}`} session={s} />
        ))}
      </div>
    </div>
  );
}

function SessionCard({ session }: { session: ActiveSessionWithSlug }) {
  const age = secondsSince(session.started_at);
  return (
    <Link
      to={`/p/${encodeURIComponent(session.slug)}/s/${encodeURIComponent(session.session_id)}`}
      data-testid={`session-card-${session.slug}-${session.session_id}`}
      className="block bg-surface-800/60 hover:bg-surface-800 border border-surface-700/40 rounded-lg p-3 transition-colors flex flex-col gap-1 min-w-0"
    >
      <div className="flex items-center gap-2 min-w-0">
        <span
          className="agent-active-dot shrink-0"
          aria-label={`session ${session.session_id} running`}
          role="status"
          title="running"
        />
        <span
          className="font-mono text-sm text-text-primary truncate flex-1"
          title={`${session.slug}/${session.session_id}`}
        >
          {session.slug}
        </span>
        <span className="shrink-0 px-1.5 py-0.5 rounded text-[10px] font-mono uppercase tracking-wider bg-accent-600/10 text-accent-600">
          {session.role}
        </span>
      </div>
      <div className="flex flex-wrap gap-3 text-[11px] font-mono text-text-dim">
        <span title="model">{session.model ?? "—"}</span>
        <span title="live cost">${session.cost_usd.toFixed(2)}</span>
        <span title="started">{ageLabel(age)} ago</span>
      </div>
      <span
        className="text-[10px] text-text-dim font-mono truncate"
        title={session.cwd ?? ""}
      >
        cwd: {basename(session.cwd)}
      </span>
    </Link>
  );
}
