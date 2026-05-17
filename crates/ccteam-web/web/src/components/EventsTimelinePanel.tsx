// V0.4.6 F90 — Events Timeline panel.
//
// Variant of EventsLive specialised for the WorkflowView side rail:
// color-coded by workflow event kind so the operator can scan a
// running project at a glance.
//
//   green   → agent_done (status=completed|stopped)
//   amber   → gate_triggered / budget_exceeded
//   red     → escalation / agent_done (status != completed|stopped)
//   neutral → everything else (workflow_start, agent_spawn, ...)
//
// Seeds from REST `initialEvents` (so the panel isn't blank before the
// first SSE push lands), then merges in streamed events. Capped at
// `tailRows` (default 100).

import {
  useProgressStream,
  type ProgressEvent,
} from "../hooks/useProgressStream";

type Props = {
  slug: string;
  initialEvents?: ProgressEvent[];
  tailRows?: number;
};

type Severity = "info" | "ok" | "warn" | "error";

/** Decide row color based on event kind + optional `status` field. */
export function classify(event: ProgressEvent): Severity {
  const kind = event.event;
  if (kind === "agent_done") {
    const status = (event as Record<string, unknown>).status;
    if (typeof status === "string") {
      return status === "completed" || status === "stopped" ? "ok" : "error";
    }
    return "ok";
  }
  if (kind === "escalation") return "error";
  if (kind === "gate_triggered" || kind === "budget_exceeded") return "warn";
  if (kind === "workflow_done") {
    const reason = (event as Record<string, unknown>).reason;
    return reason === "shutdown" || reason === "completed" ? "ok" : "warn";
  }
  return "info";
}

const severityClass: Record<Severity, string> = {
  info: "border-l-2 border-surface-700/40",
  ok: "border-l-2 border-status-running bg-status-running/5",
  warn: "border-l-2 border-status-waiting bg-status-waiting/5",
  error: "border-l-2 border-status-error bg-status-error/5",
};

/** Tool-use events (PreToolUse / PostToolUse) wire-encode the tool name
 *  in `tool` and (for Bash) the command in `cmd`. `detail` is empty for
 *  hook-driven events, so the row would render a blank right column
 *  without this fallback. Full string is returned — visual truncation
 *  is handled by the row's CSS (`truncate` Tailwind class), so wide
 *  layouts show more of the command and narrow layouts ellipsize. */
export function detailLabel(ev: ProgressEvent): string {
  if (ev.event === "PreToolUse" || ev.event === "PostToolUse") {
    const tool = ev.tool ?? "?";
    const arg = ev.cmd ?? ev.file_path;
    return arg ? `${tool}: ${arg}` : tool;
  }
  return ev.detail ?? "";
}

export function EventsTimelinePanel({
  slug,
  initialEvents,
  tailRows = 100,
}: Props) {
  const { events, connected, lastError } = useProgressStream({
    scope: { kind: "project", slug },
  });

  const merged: ProgressEvent[] = [
    ...(initialEvents ?? []),
    ...events,
  ];
  // Tail + reverse so newest is on top.
  const view = merged.slice(-tailRows).reverse();

  return (
    <section className="border border-surface-700/40 rounded-md bg-surface-850 flex flex-col min-h-0 h-full w-full">
      <header className="flex items-center justify-between px-3 py-2 border-b border-surface-700/30 shrink-0">
        <h3 className="text-xs uppercase tracking-wide text-text-secondary">
          Events Timeline
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
          <li className="px-3 py-2 text-text-dim italic">no events yet</li>
        )}
        {view.map((ev, i) => {
          const severity = classify(ev);
          return (
            <li
              key={`${ev.ts}-${i}`}
              className={
                "pl-2 pr-1 py-1 flex gap-2 items-baseline " +
                severityClass[severity]
              }
            >
              <span className="text-text-dim shrink-0">{ev.ts}</span>
              <span className="text-text-bright shrink-0">{ev.event}</span>
              <span
                className="text-text-secondary truncate flex-1 min-w-0"
                title={detailLabel(ev)}
              >
                {detailLabel(ev)}
              </span>
            </li>
          );
        })}
      </ol>
    </section>
  );
}

export default EventsTimelinePanel;
