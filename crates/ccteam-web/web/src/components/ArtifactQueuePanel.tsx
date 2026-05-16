// V0.4.6 F90 — Artifact queue panel.
//
// Lists every `Trigger::Watch(<path>)` agent in the project's
// workflow.yaml + the live file count / oldest age for each. The
// data source is `GET /api/v1/projects/<slug>/artifact_queue`
// (computed by `ccteam_core::artifact_queue`).
//
// Refresh strategy:
//   - Initial fetch on mount.
//   - Re-fetch whenever a new `artifact_received` event arrives on the
//     project's SSE stream (the only event that changes the queue
//     count). Other event kinds are ignored so we don't thrash.
//
// Red line: this component never opens its own polling timer. It only
// piggy-backs on the SSE stream owned by `useProgressStream`.

import { useEffect, useState } from "react";
import {
  fetchArtifactQueue,
  ageLabel,
  type ArtifactQueueEntry,
} from "../lib/workflowPanels";
import { useProgressStream } from "../hooks/useProgressStream";

interface Props {
  slug: string;
  /** Bump from the parent (e.g. after a manual `trigger_gate`) to
   *  force a fresh fetch even when no SSE event fires. */
  reloadKey?: number;
}

export default function ArtifactQueuePanel({ slug, reloadKey = 0 }: Props) {
  const [entries, setEntries] = useState<ArtifactQueueEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const stream = useProgressStream({ scope: { kind: "project", slug } });
  const latest = stream.events[stream.events.length - 1];

  useEffect(() => {
    let cancelled = false;
    fetchArtifactQueue(slug)
      .then((rows) => {
        if (cancelled) return;
        setEntries(rows);
        setError(null);
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

  // Only re-fetch on artifact_received events; everything else is no-op.
  useEffect(() => {
    if (!latest) return;
    if (latest.event !== "artifact_received") return;
    let cancelled = false;
    fetchArtifactQueue(slug)
      .then((rows) => {
        if (!cancelled) setEntries(rows);
      })
      .catch(() => {
        // Silent — keep last good list. The next refresh will retry.
      });
    return () => {
      cancelled = true;
    };
  }, [slug, latest]);

  return (
    <section className="border border-surface-700/40 rounded-md bg-surface-850 flex flex-col">
      <header className="flex items-center justify-between px-3 py-2 border-b border-surface-700/30">
        <h3 className="text-xs uppercase tracking-wide text-text-secondary">
          Artifact Queue
        </h3>
        {entries.length > 0 && (
          <span className="text-[10px] font-mono text-text-dim">
            {entries.length} watch path{entries.length === 1 ? "" : "s"}
          </span>
        )}
      </header>
      <div className="flex flex-col font-mono text-xs">
        {loading && (
          <div className="px-3 py-2 text-text-dim italic">loading…</div>
        )}
        {!loading && error && (
          <div className="px-3 py-2 text-status-error" role="alert">
            {error}
          </div>
        )}
        {!loading && !error && entries.length === 0 && (
          <div className="px-3 py-2 text-text-dim italic">
            no watch triggers
          </div>
        )}
        <ul className="divide-y divide-surface-700/20">
          {entries.map((e) => (
            <li
              key={`${e.role}|${e.path}`}
              className="px-3 py-1.5 flex flex-col gap-0.5 min-w-0"
            >
              <div className="flex items-baseline justify-between gap-2 min-w-0">
                <span
                  className="text-text-primary truncate"
                  title={e.path}
                >
                  {e.path}
                </span>
                <span className="text-text-dim shrink-0">
                  {e.file_count} file{e.file_count === 1 ? "" : "s"}
                </span>
              </div>
              <div className="flex items-baseline justify-between gap-2 text-[10px] text-text-dim">
                <span title={`agent role: ${e.role}`}>{e.role}</span>
                <span>
                  {e.oldest_age_seconds != null
                    ? `oldest ${ageLabel(e.oldest_age_seconds)} ago`
                    : "—"}
                </span>
              </div>
              {e.newest_filename && (
                <div
                  className="text-[10px] text-text-muted truncate"
                  title={e.newest_filename}
                >
                  latest: {e.newest_filename}
                </div>
              )}
            </li>
          ))}
        </ul>
      </div>
    </section>
  );
}
