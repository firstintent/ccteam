// Artifact status counts panel.
//
// Surfaces per-dir status-grouped counts (`open`/`fixing`/`closed`/...)
// for every project-defined artifact dir under `<project>/.ccteam/`.
// Source: `GET /api/v1/projects/<slug>/artifact_status`
// (`ccteam_core::artifact_status`). Project-agnostic — works for any
// project whose `*.json` artifacts carry a top-level string `.status`.
//
// Refresh: re-fetched on every `PostToolUse` event over SSE (the only
// channel through which agent-driven status mutations land on disk).
// A 10s floor between fetches keeps a chatty Bash session from
// thrashing the endpoint.

import { useEffect, useRef, useState } from "react";
import {
  fetchArtifactStatus,
  type ArtifactStatusGroup,
} from "../lib/workflowPanels";
import { useProgressStream } from "../hooks/useProgressStream";

interface Props {
  slug: string;
  reloadKey?: number;
}

const REFETCH_FLOOR_MS = 10_000;

const STATUS_TONE: Record<string, string> = {
  open: "text-status-error",
  fixing: "text-status-waiting",
  needs_human: "text-status-error",
  "needs-human": "text-status-error",
  pending: "text-text-bright",
  failed: "text-status-error",
  closed: "text-text-dim",
  merged: "text-status-running",
  tested: "text-status-running",
  skipped: "text-text-dim",
  fixed: "text-status-running",
};

function pillClass(status: string): string {
  return STATUS_TONE[status] ?? "text-text-secondary";
}

export default function ArtifactStatusPanel({ slug, reloadKey = 0 }: Props) {
  const [groups, setGroups] = useState<ArtifactStatusGroup[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const lastFetchRef = useRef<number>(0);

  const stream = useProgressStream({ scope: { kind: "project", slug } });
  const latest = stream.events[stream.events.length - 1];

  useEffect(() => {
    let cancelled = false;
    fetchArtifactStatus(slug)
      .then((rows) => {
        if (cancelled) return;
        setGroups(rows);
        setError(null);
        lastFetchRef.current = Date.now();
      })
      .catch((e: unknown) => {
        if (cancelled) return;
        setError(e instanceof Error ? e.message : String(e));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [slug, reloadKey]);

  // Only PostToolUse can change disk state; throttle to one fetch per
  // REFETCH_FLOOR_MS so a Bash burst (10s of cmds) doesn't queue 10
  // round-trips.
  useEffect(() => {
    if (!latest) return;
    if (latest.event !== "PostToolUse") return;
    const now = Date.now();
    if (now - lastFetchRef.current < REFETCH_FLOOR_MS) return;
    lastFetchRef.current = now;
    let cancelled = false;
    fetchArtifactStatus(slug)
      .then((rows) => {
        if (!cancelled) setGroups(rows);
      })
      .catch(() => {
        // Silent — keep last good snapshot.
      });
    return () => {
      cancelled = true;
    };
  }, [slug, latest]);

  return (
    <section className="border border-surface-700/40 rounded-md bg-surface-850 flex flex-col">
      <header className="flex items-center justify-between px-3 py-2 border-b border-surface-700/30 shrink-0">
        <h3 className="text-xs uppercase tracking-wide text-text-secondary">
          Artifact Status
        </h3>
      </header>
      <div className="px-3 py-2 text-xs font-mono">
        {loading && <div className="text-text-dim italic">loading…</div>}
        {error != null && (
          <div className="text-status-error">error: {error}</div>
        )}
        {!loading && !error && groups.length === 0 && (
          <div className="text-text-dim italic">
            no statused artifacts in .ccteam/
          </div>
        )}
        {!loading && !error && groups.length > 0 && (
          <ul className="space-y-2">
            {groups.map((g) => (
              <li key={g.dir}>
                <div className="flex items-center justify-between">
                  <span className="text-text-bright">{g.dir}</span>
                  <span className="text-text-dim">total {g.total}</span>
                </div>
                <div className="mt-1 flex flex-wrap gap-x-3 gap-y-1">
                  {Object.entries(g.counts).map(([status, count]) => (
                    <span key={status} className={pillClass(status)}>
                      {status} <span className="font-bold">{count}</span>
                    </span>
                  ))}
                </div>
              </li>
            ))}
          </ul>
        )}
      </div>
    </section>
  );
}
