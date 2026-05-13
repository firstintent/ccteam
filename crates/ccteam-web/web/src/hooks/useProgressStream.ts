// V0.3.2 F54 — canonical SSE hook for the SPA.
//
// One hook handles the three subscription scopes the server exposes
// (see `crates/ccteam-web/src/routes/sse.rs`):
//
//   - `{ kind: 'all' }`                 → `/sse/all`
//   - `{ kind: 'project', slug }`       → `/sse/project/<slug>`
//   - `{ kind: 'session', slug, sid }`  → `/sse/project/<slug>/<sid>`
//
// F55 reuses this hook for project-detail / session-detail pages —
// changing the `scope` prop is enough to switch streams, so detail
// pages don't reimplement the EventSource dance.
//
// IMPORTANT gotchas the server forced on us:
//
// 1. The server emits NAMED events: `event: progress` for normal
//    payloads and `event: reconnect_hint` when the broadcast subscriber
//    lags. `EventSource.onmessage` only fires for unnamed events, so we
//    `addEventListener('progress', ...)` explicitly. Forgetting this is
//    the classic "stream connects, zero events arrive" bug.
//
// 2. EventSource auto-reconnects by default with a fixed delay (browser
//    UA controlled, usually 3s) and ignores app errors. The spec wants
//    a controlled exponential backoff with a retry cap. We `.close()`
//    on `onerror` so the browser's reconnect path is disabled, then
//    drive `new EventSource(url)` ourselves on a setTimeout. Mirrors
//    `useTerminal`'s retry shape: 1s → 30s cap, 7 attempts.
//
// 3. `reconnect_hint` is the server's "I'm dropping you, please
//    reconnect" signal. We treat it the same as an error — close,
//    backoff, reopen. The retry counter resets on a successful `open`.

import { useEffect, useRef, useState } from "react";

/** One event line off the SSE stream.
 *
 *  The server passes through the original `progress.jsonl` keys and
 *  splices in `slug` (always) and `sid` (flex sessions only). Other
 *  fields vary by event type — `detail` is best-effort and may be
 *  absent. Treat anything beyond `ts` and `slug` as optional.
 *
 *  See `crates/ccteam-web/src/routes/sse.rs::progress_event` for the
 *  authoritative shape. */
export type ProgressEvent = {
  ts: string;
  event: string;
  detail: string;
  slug?: string;
  sid?: string;
};

/** Discriminated union — match the server's three SSE routes 1:1. */
export type ProgressStreamScope =
  | { kind: "all" }
  | { kind: "project"; slug: string }
  | { kind: "session"; slug: string; sid: string };

export interface UseProgressStreamOpts {
  scope: ProgressStreamScope;
  /** Default `true`. Pass `false` to keep the hook mounted but tear the
   *  EventSource down (e.g. when a parent route is hidden). */
  enabled?: boolean;
}

export interface UseProgressStreamResult {
  /** Ring buffer of events, newest at the END. Capped at 500 — oldest
   *  drop when the buffer overflows. Replaced (not mutated) on each
   *  append so React's referential-equality check rerenders consumers. */
  events: ProgressEvent[];
  /** True between a successful `onopen` and the next close / error. */
  connected: boolean;
  /** Last error message surfaced to the UI. `null` while healthy or
   *  during the controlled-retry window. Set to a human-readable string
   *  ("max retries reached", "scope: invalid", …) when the hook gives
   *  up so the caller can render an explicit retry button. */
  lastError: string | null;
}

const RING_CAP = 500;
const MAX_RETRIES = 7;
const RETRY_BASE_MS = 1000;
const RETRY_CAP_MS = 30000;
const retryDelayMs = (attempt: number): number =>
  Math.min(RETRY_CAP_MS, RETRY_BASE_MS * 2 ** (attempt - 1));

function scopeUrl(scope: ProgressStreamScope): string {
  switch (scope.kind) {
    case "all":
      return "/sse/all";
    case "project":
      return `/sse/project/${encodeURIComponent(scope.slug)}`;
    case "session":
      return `/sse/project/${encodeURIComponent(scope.slug)}/${encodeURIComponent(scope.sid)}`;
  }
}

/** Derive a string key for the scope so the connect effect only re-runs
 *  when the actual scope identity changes (not on every parent rerender
 *  that builds a fresh object literal). */
function scopeKey(scope: ProgressStreamScope): string {
  switch (scope.kind) {
    case "all":
      return "all";
    case "project":
      return `project:${scope.slug}`;
    case "session":
      return `session:${scope.slug}/${scope.sid}`;
  }
}

export function useProgressStream(
  opts: UseProgressStreamOpts,
): UseProgressStreamResult {
  const { scope } = opts;
  const enabled = opts.enabled ?? true;
  const key = scopeKey(scope);

  const [events, setEvents] = useState<ProgressEvent[]>([]);
  const [connected, setConnected] = useState(false);
  const [lastError, setLastError] = useState<string | null>(null);

  const esRef = useRef<EventSource | null>(null);
  const retryTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const retryCountRef = useRef(0);

  useEffect(() => {
    if (!enabled) {
      // Caller turned us off — drain any in-flight reconnect timer and
      // surface a fresh "disconnected" state so consumers can render
      // an empty pane instead of stale events.
      if (retryTimerRef.current) {
        clearTimeout(retryTimerRef.current);
        retryTimerRef.current = null;
      }
      esRef.current?.close();
      esRef.current = null;
      retryCountRef.current = 0;
      setConnected(false);
      setLastError(null);
      return;
    }

    const url = scopeUrl(scope);
    let cancelled = false;

    const appendEvent = (raw: string) => {
      let parsed: ProgressEvent;
      try {
        parsed = JSON.parse(raw) as ProgressEvent;
      } catch {
        // Garbage payloads have happened in dev when the watcher races
        // a half-written progress line. Drop silently — the next event
        // will land cleanly and the ring buffer never goes bad.
        return;
      }
      setEvents((prev) => {
        if (prev.length >= RING_CAP) {
          // Drop from the front to keep the cap. Slice + push beats
          // shift() on a 500-elem array (which mutates) since we want
          // a new reference for React's diff anyway.
          return [...prev.slice(prev.length - RING_CAP + 1), parsed];
        }
        return [...prev, parsed];
      });
    };

    const connect = () => {
      if (cancelled) return;
      const es = new EventSource(url);
      esRef.current = es;

      es.addEventListener("open", () => {
        if (cancelled) return;
        retryCountRef.current = 0;
        setConnected(true);
        setLastError(null);
      });

      // Server emits `event: progress` — addEventListener with the
      // explicit name; `onmessage` would only catch unnamed events
      // (which the server never sends).
      es.addEventListener("progress", (ev) => {
        if (cancelled) return;
        appendEvent((ev as MessageEvent).data);
      });

      // `reconnect_hint` is the server's "I'm dropping you, reconnect"
      // signal (broadcast subscriber lagged). Treat as a soft error:
      // close + backoff + reopen.
      es.addEventListener("reconnect_hint", () => {
        if (cancelled) return;
        es.close();
        scheduleReconnect("server requested reconnect");
      });

      es.addEventListener("error", () => {
        if (cancelled) return;
        // EventSource fires `error` both on transient network failures
        // and on hard 4xx/5xx. The browser's auto-reconnect would kick
        // in on transients with no backoff; close first to suppress it.
        es.close();
        scheduleReconnect("connection lost");
      });
    };

    const scheduleReconnect = (reason: string) => {
      setConnected(false);
      if (retryCountRef.current >= MAX_RETRIES) {
        setLastError(`SSE max retries reached (${reason})`);
        return;
      }
      retryCountRef.current += 1;
      const delay = retryDelayMs(retryCountRef.current);
      // Don't set lastError here — within the backoff window the hook
      // is still "trying", and surfacing an error every retry would
      // flicker the dashboard's banner. Only the terminal failure
      // above writes lastError.
      retryTimerRef.current = setTimeout(() => {
        retryTimerRef.current = null;
        connect();
      }, delay);
    };

    connect();

    return () => {
      cancelled = true;
      if (retryTimerRef.current) {
        clearTimeout(retryTimerRef.current);
        retryTimerRef.current = null;
      }
      esRef.current?.close();
      esRef.current = null;
      retryCountRef.current = 0;
    };
    // `key` captures the scope identity; rebuild the stream when it
    // changes. `enabled` toggles the whole effect. We don't depend on
    // `scope` directly because callers commonly inline the object,
    // which would tear down + rebuild on every parent rerender.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key, enabled]);

  return { events, connected, lastError };
}
