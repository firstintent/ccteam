// V0.3.2 F55 — live progress.jsonl tail panel.
//
// Wraps F54's `useProgressStream` hook (or the F55 shim until F54
// lands) and renders a reverse-chronological 200-row tail. Highlights:
//   - phase_done    → green (run completed a phase, win)
//   - stall_detected → amber (idle-detector flagged a stall)
//
// Initial events from REST (`ProjectSummary.events` /
// `SessionDetail.events`) seed the panel via `initialEvents` so the
// 200-row tail isn't empty until the first SSE push arrives. New
// streamed events append, then the merged buffer is sliced + reversed
// for render.

import {
  useProgressStream,
  type ProgressEvent,
  type ProgressStreamScope,
} from "../hooks/useProgressStream";

type Props = {
  scope: ProgressStreamScope;
  /** Events already returned by REST (`ProjectSummary.events` or
   *  `SessionDetail.events`). Used to seed the tail so the panel
   *  isn't blank before the first SSE push. */
  initialEvents?: ProgressEvent[];
};

const HIGHLIGHT = {
  phase_done: "border-l-2 border-status-running pl-2 bg-status-running/5",
  stall_detected: "border-l-2 border-status-waiting pl-2 bg-status-waiting/5",
} as const;

function rowClass(event: string): string {
  if (event === "phase_done") return HIGHLIGHT.phase_done;
  if (event === "stall_detected") return HIGHLIGHT.stall_detected;
  return "pl-2";
}

export function EventsLive({ scope, initialEvents }: Props) {
  const { events, connected, lastError } = useProgressStream({ scope });

  // Merge: REST seed (oldest first) + streamed (chronological). Slice
  // last 200, then reverse so latest is on top.
  const merged: ProgressEvent[] = [
    ...(initialEvents ?? []),
    ...events,
  ];
  const view = merged.slice(-200).reverse();

  return (
    <section className="border border-surface-700/40 rounded-md bg-surface-850 flex flex-col min-h-0">
      <header className="flex items-center justify-between px-3 py-2 border-b border-surface-700/30 shrink-0">
        <h3 className="text-xs uppercase tracking-wide text-text-secondary">
          Live events
        </h3>
        <span
          className={
            "text-[10px] font-mono " +
            (connected ? "text-status-running" : "text-text-dim")
          }
          title={lastError ?? undefined}
        >
          {connected ? "live" : "off"}
        </span>
      </header>

      {!connected && lastError != null && (
        <div className="px-3 py-1.5 bg-status-error/10 border-b border-status-error/30 text-xs text-status-error font-mono shrink-0">
          Disconnected: {lastError}
        </div>
      )}

      <ol className="flex-1 min-h-0 overflow-auto font-mono text-xs divide-y divide-surface-700/20">
        {view.length === 0 && (
          <li className="px-3 py-2 text-text-dim italic">
            no events yet
          </li>
        )}
        {view.map((ev, i) => (
          <li
            key={`${ev.ts}-${i}`}
            className={"px-1 py-1 flex gap-2 whitespace-pre " + rowClass(ev.event)}
          >
            <span className="text-text-dim shrink-0">{ev.ts}</span>
            <span className="text-text-bright shrink-0">{ev.event}</span>
            <span className="text-text-secondary truncate">{ev.detail}</span>
          </li>
        ))}
      </ol>
    </section>
  );
}
