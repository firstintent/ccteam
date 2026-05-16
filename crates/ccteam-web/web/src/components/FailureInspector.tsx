// V0.4.6 F90 — Failure Inspector modal.
//
// Renders the tail of `~/.claude/jobs/<job_id>/output.log` for a
// (typically errored) agent session. Read-only: no PTY, no
// send-keys — the operator scans the failure output, then dismisses.
//
// The component is presentation-only; the parent decides when to mount
// it (by setting `jobId` non-null). The fetch fires on every change of
// `jobId` so reopening the same modal re-tails the latest log content
// (the underlying log may have grown since last view).

import { useEffect, useState } from "react";
import { fetchJobLog, type JobLogResponse } from "../lib/workflowPanels";

interface Props {
  slug: string;
  /** Set to a job_id to open the modal; null/undefined closes it. */
  jobId: string | null;
  onClose: () => void;
  /** Default 200 lines; SPA may override (max 5000 server-side). */
  tail?: number;
}

export default function FailureInspector({
  slug,
  jobId,
  onClose,
  tail = 200,
}: Props) {
  const [resp, setResp] = useState<JobLogResponse | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!jobId) {
      setResp(null);
      setError(null);
      return;
    }
    let cancelled = false;
    setLoading(true);
    setError(null);
    setResp(null);
    fetchJobLog(slug, jobId, tail)
      .then((data) => {
        if (!cancelled) setResp(data);
      })
      .catch((e: unknown) => {
        if (!cancelled)
          setError(e instanceof Error ? e.message : String(e));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [slug, jobId, tail]);

  // Esc closes the modal.
  useEffect(() => {
    if (!jobId) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [jobId, onClose]);

  if (!jobId) return null;

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60"
      role="dialog"
      aria-modal="true"
      aria-label="Failure log"
      onClick={onClose}
    >
      <div
        className="w-[min(90vw,80rem)] h-[min(80vh,50rem)] rounded-md border border-surface-700/40 bg-surface-900 shadow-xl flex flex-col"
        onClick={(e) => e.stopPropagation()}
      >
        <header className="flex items-center justify-between px-4 py-2 border-b border-surface-700/30">
          <div className="flex items-baseline gap-3 min-w-0">
            <h2 className="text-sm font-semibold text-text-bright">
              Failure Log
            </h2>
            <span className="font-mono text-[11px] text-text-dim truncate">
              job={jobId}
            </span>
            {resp && (
              <span className="font-mono text-[10px] text-text-muted">
                showing last {Math.min(tail, resp.total_lines)} of{" "}
                {resp.total_lines} lines
              </span>
            )}
          </div>
          <button
            onClick={onClose}
            type="button"
            className="px-2 py-0.5 text-xs font-mono uppercase tracking-wide text-text-dim hover:text-text-bright cursor-pointer"
            aria-label="Close"
          >
            close
          </button>
        </header>
        <div className="flex-1 min-h-0 overflow-auto p-3 font-mono text-[11px] text-text-secondary whitespace-pre-wrap">
          {loading && <span className="italic text-text-dim">loading…</span>}
          {!loading && error && (
            <span className="text-status-error">Error: {error}</span>
          )}
          {!loading && !error && resp && resp.tail.length === 0 && (
            <span className="italic text-text-dim">
              no log available (output.log missing or empty)
            </span>
          )}
          {!loading && !error && resp && resp.tail.length > 0 && (
            <pre className="whitespace-pre-wrap break-words">{resp.tail}</pre>
          )}
        </div>
      </div>
    </div>
  );
}
