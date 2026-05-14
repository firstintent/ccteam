// V0.3.2 F54 — Dashboard page (project list SPA).
//
// On mount: fetch `/api/v1/projects`, render WorkspaceSidebar + a card
// grid. Subscribe `/sse/all` and patch the matching row's
// `last_event_label` to a relative timestamp ("3s ago") whenever an
// event with a `slug` field arrives.
//
// Manual verification once `npm run build` is back online (deferred to
// the user per F54's hard constraints):
//
//   - GET /app/ should render the dashboard
//   - clicking a project navigates to /app/p/<slug>
//   - SSE event with slug=<x> must update that row's last_event_label
//
// Auth surface is intentionally minimal: a 401 from `/api/v1/projects`
// throws `UNAUTHENTICATED` which we render as a banner. F58 will
// replace that banner with a redirect into TokenEntryPage; this page
// avoids touching `lib/token.ts` or `lib/api.ts` so F58's rewrite
// stays the single owner of token flow.

import { useEffect, useMemo, useState } from "react";
import { useNavigate } from "react-router-dom";
import {
  fetchDashboard,
  type DashboardRow,
} from "../lib/dashboardApi";
import {
  useProgressStream,
  type ProgressEvent,
} from "../hooks/useProgressStream";
import { WorkspaceSidebar } from "../components/WorkspaceSidebar";

/** Format an SSE event timestamp as a short relative-time string.
 *
 *  Uses `Intl.RelativeTimeFormat` so we don't pull in a date lib. The
 *  server emits RFC3339 UTC strings; bad input falls back to the raw
 *  string so the cell never goes blank. */
function formatRelative(tsIso: string, now: number = Date.now()): string {
  const t = Date.parse(tsIso);
  if (Number.isNaN(t)) return tsIso;
  const deltaSec = Math.round((t - now) / 1000);
  const abs = Math.abs(deltaSec);
  const rtf = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" });
  if (abs < 60) return rtf.format(deltaSec, "second");
  if (abs < 3600) return rtf.format(Math.round(deltaSec / 60), "minute");
  if (abs < 86400) return rtf.format(Math.round(deltaSec / 3600), "hour");
  return rtf.format(Math.round(deltaSec / 86400), "day");
}

/** Events the dashboard surfaces as "fresh" in the last_event_label.
 *  Anything else (PostToolUse spam, UserPromptSubmit echoes) would
 *  thrash the row label every keystroke without telling the operator
 *  anything new. */
const FRESHNESS_EVENTS = new Set([
  "phase_start",
  "phase_done",
  "phase_end",
  "idle_state_change",
  "idle_prompt",
  "fix_loop_escalation",
  "cost_alert",
  "session_started",
  "session_ended",
]);

interface ProjectCardProps {
  row: DashboardRow;
  onOpen: (slug: string) => void;
}

function ProjectCard({ row, onOpen }: ProjectCardProps) {
  // F54 doesn't surface harness on `/api/v1/projects` yet (see
  // dashboardApi.ts). Default to "claude" so the pill always renders;
  // F55+ will plumb the real value through once session-detail lands.
  const harness = row.harness ?? "claude";
  return (
    <button
      onClick={() => onOpen(row.slug)}
      className="text-left bg-surface-800/60 hover:bg-surface-800 border border-surface-700/40 rounded-lg p-4 transition-colors cursor-pointer flex flex-col gap-2 min-w-0"
    >
      <div className="flex items-center gap-2 min-w-0">
        <span
          className="font-mono text-sm text-text-primary truncate flex-1"
          title={row.slug}
        >
          {row.slug}
        </span>
        <span
          className={`shrink-0 px-1.5 py-0.5 rounded text-[10px] font-mono uppercase tracking-wider ${row.badge_class}`}
        >
          {row.badge_label}
        </span>
      </div>
      <div className="flex items-center gap-2 text-[11px] font-mono text-text-dim">
        <span className="truncate" title={`${row.team} / ${row.kind}`}>
          {row.team} / {row.kind}
        </span>
        <span className="px-1.5 py-0.5 rounded bg-surface-700/40 uppercase tracking-wider">
          {harness}
        </span>
      </div>
      <div className="text-xs text-text-secondary truncate" title={row.current_phase}>
        {row.current_phase || "—"}
      </div>
      <div className="flex items-center justify-between text-[11px] font-mono text-text-dim mt-auto pt-1">
        <span className="truncate">{row.last_event_label || "no events"}</span>
        <span className="shrink-0 pl-2">{row.cost_label}</span>
      </div>
    </button>
  );
}

export default function Dashboard() {
  const navigate = useNavigate();
  const [rows, setRows] = useState<DashboardRow[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [authError, setAuthError] = useState(false);

  useEffect(() => {
    let cancelled = false;
    fetchDashboard()
      .then((data) => {
        if (cancelled) return;
        setRows(data);
        setError(null);
        setAuthError(false);
      })
      .catch((err: unknown) => {
        if (cancelled) return;
        const msg = err instanceof Error ? err.message : String(err);
        if (msg === "UNAUTHENTICATED") {
          setAuthError(true);
        } else {
          setError(msg);
        }
        setRows([]);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const stream = useProgressStream({
    scope: { kind: "all" },
    // Don't open the stream until we have a baseline row set; merging
    // events into `null` would either drop them or force a guard in
    // the merge effect. Easier to wait the ~tens of ms.
    enabled: rows !== null && !authError,
  });

  // Merge incoming SSE events into the row table. We keep a small
  // index of slug → row so the patch is O(events) instead of
  // O(events * rows). Only the most recent event we've processed
  // matters; if multiple arrive between renders, all hit the same
  // setRows call below.
  const latestEvent: ProgressEvent | undefined =
    stream.events[stream.events.length - 1];
  useEffect(() => {
    if (!latestEvent || !latestEvent.slug) return;
    if (latestEvent.event && !FRESHNESS_EVENTS.has(latestEvent.event)) {
      // Spammy events (PostToolUse, etc.) wouldn't change the operator's
      // view. Skip the rerender.
      return;
    }
    const targetSlug = latestEvent.slug;
    const targetTs = latestEvent.ts;
    let cancelled = false;
    queueMicrotask(() => {
      if (cancelled) return;
      setRows((prev) => {
        if (!prev) return prev;
        let hit = false;
        const next = prev.map((r) => {
          if (r.slug !== targetSlug) return r;
          hit = true;
          return { ...r, last_event_label: formatRelative(targetTs) };
        });
        // Avoid creating a new array (and rerender) if no row matched —
        // common case: a project the dashboard hasn't fetched yet because
        // it was created after the SSE stream opened. F54 doesn't refresh
        // on insert; the user reloads to see new projects.
        return hit ? next : prev;
      });
    });
    return () => {
      cancelled = true;
    };
  }, [latestEvent]);

  const cards = useMemo(() => rows ?? [], [rows]);

  const handleOpen = (slug: string) => {
    navigate(`/p/${encodeURIComponent(slug)}`);
  };

  if (authError) {
    return (
      <div className="h-full flex items-center justify-center bg-surface-900">
        <div className="max-w-md text-center px-4">
          <div className="text-status-error font-mono text-sm mb-2">
            Token expired
          </div>
          <div className="text-text-secondary text-xs">
            Refresh the page to re-enter your access token.
          </div>
        </div>
      </div>
    );
  }

  if (rows === null) {
    return (
      <div className="h-full flex items-center justify-center text-text-dim text-xs font-mono">
        Loading dashboard…
      </div>
    );
  }

  return (
    <div className="h-full flex min-h-0 min-w-0">
      <WorkspaceSidebar projects={cards} activeSlug={null} />
      <div className="flex-1 min-w-0 overflow-y-auto">
        {error && (
          <div className="m-4 px-3 py-2 rounded border border-status-error/40 bg-status-error/10 text-status-error text-xs font-mono">
            {error}
          </div>
        )}
        {cards.length === 0 ? (
          <div className="h-full flex items-center justify-center text-text-dim text-xs font-mono">
            No projects. Create one with <code className="text-text-secondary ml-1">ccteam new</code>.
          </div>
        ) : (
          <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4 p-4">
            {cards.map((row) => (
              <ProjectCard key={row.slug} row={row} onOpen={handleOpen} />
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
