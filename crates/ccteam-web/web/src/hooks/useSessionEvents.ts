// v0.8.7 W4 (DD.1) — per-session SSE hook for the gateway resource API.
//
// Generalizes `useProgressStream`'s EventSource dance to the W2 endpoint
//   GET /api/v1/sessions/{sid}/events
// which streams `event: progress` frames whose payload is the W2 shape
//   { id, sid, kind: "answer" | "progress", content, done?, options? }
// (see `crates/ccteam-web/src/routes/sessions_api.rs::session_event_payload`).
// `options` is the non-empty label list of an approval ChoicePrompt — the
// hook surfaces it so ChatConsole can render "session sX wants to run …
// [Approve][Deny]" per-session.
//
// Mechanics reused verbatim from useProgressStream (the server forced them
// on us): named `progress` events (`onmessage` never fires), controlled
// backoff 1s→30s cap / 7 retries, `reconnect_hint` → close+reopen, an sid
// key so the effect only re-subscribes when the sid identity changes.
//
// Auth: EventSource cannot set an Authorization header, so this stream
// authenticates via the `ccteam_token` cookie (same as the PTY WS), NOT the
// localStorage Bearer the fetch interceptor injects on REST calls.

import { useEffect, useRef, useState } from "react";

/** One selectable option on an approval ChoicePrompt (the SSE frame's
 *  `options[]`). `label` is the button text; `id` is the stable decision
 *  value (e.g. `"allow"` / `"deny"`) the web resolve path sends back as
 *  `selection`. v0.8.7 review-fix (R-H1). */
export interface SessionEventOption {
  label: string;
  id: string;
}

/** One event line off the per-session SSE stream (the W2 payload shape).
 *  `kind` is "answer" (assistant reply / approval prompt) or "progress"
 *  (status edit, `done` on the finalizing one). `options` present + non-
 *  empty marks an approval prompt; `token` is then the pending-resolution
 *  token the web resolve path POSTs back (R-H1). */
export interface SessionEvent {
  id?: string;
  sid?: string;
  kind: "answer" | "progress";
  content: string;
  done?: boolean;
  options?: SessionEventOption[];
  token?: string;
}

export interface UseSessionEventsResult {
  /** Ring buffer of events, newest at the END. Capped — oldest drop. */
  events: SessionEvent[];
  /** True between a successful open and the next close / error. */
  connected: boolean;
  /** Human-readable error after the hook gives up retrying; null while
   *  healthy or inside the backoff window. */
  lastError: string | null;
  /** True when the server reported it has no live gateway (the one-shot
   *  `gateway_unavailable` SSE frame on the no-daemon path). */
  gatewayUnavailable: boolean;
}

export const SESSION_RING_CAP = 500;
const MAX_RETRIES = 7;
const RETRY_BASE_MS = 1000;
const RETRY_CAP_MS = 30000;
const retryDelayMs = (attempt: number): number =>
  Math.min(RETRY_CAP_MS, RETRY_BASE_MS * 2 ** (attempt - 1));

/** Build the per-session SSE URL. Exported for unit tests + so callers
 *  share one template. */
export function sessionEventsUrl(sid: string): string {
  return `/api/v1/sessions/${encodeURIComponent(sid)}/events`;
}

/** Parse one raw SSE `progress` payload into a {@link SessionEvent}, or
 *  `null` for garbage / a non-object (dropped silently — the next frame
 *  lands clean). Pure + DOM-free so it is unit-testable in node env. */
export function parseSessionEvent(raw: string): SessionEvent | null {
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return null;
  }
  if (typeof parsed !== "object" || parsed === null) return null;
  const obj = parsed as Record<string, unknown>;
  const kind = obj.kind === "progress" ? "progress" : "answer";
  const event: SessionEvent = {
    kind,
    content: typeof obj.content === "string" ? obj.content : "",
  };
  if (typeof obj.id === "string") event.id = obj.id;
  if (typeof obj.sid === "string") event.sid = obj.sid;
  if (obj.done === true) event.done = true;
  if (Array.isArray(obj.options)) {
    // v0.8.7 review-fix (R-H1) — options are `{label, id}` objects; the id is
    // the decision value the resolve path sends back as `selection`. Drop any
    // malformed entry (no string label) so a half-formed frame can't render a
    // broken chip.
    const opts: SessionEventOption[] = [];
    for (const o of obj.options) {
      if (typeof o === "object" && o !== null) {
        const rec = o as Record<string, unknown>;
        if (typeof rec.label === "string") {
          opts.push({
            label: rec.label,
            id: typeof rec.id === "string" ? rec.id : "",
          });
        }
      }
    }
    if (opts.length > 0) event.options = opts;
  }
  if (typeof obj.token === "string") event.token = obj.token;
  return event;
}

/** Append `event` to a capped ring buffer, returning a NEW array (so
 *  React's referential-equality check rerenders). Pure — unit-testable. */
export function appendSessionEvent(
  prev: SessionEvent[],
  event: SessionEvent,
): SessionEvent[] {
  if (prev.length >= SESSION_RING_CAP) {
    return [...prev.slice(prev.length - SESSION_RING_CAP + 1), event];
  }
  return [...prev, event];
}

export function useSessionEvents(
  sid: string | null,
  enabled: boolean = true,
): UseSessionEventsResult {
  const [events, setEvents] = useState<SessionEvent[]>([]);
  const [connected, setConnected] = useState(false);
  const [lastError, setLastError] = useState<string | null>(null);
  const [gatewayUnavailable, setGatewayUnavailable] = useState(false);

  // Reset the event buffer SYNCHRONOUSLY when the sid changes (React's
  // "adjust state during render" pattern). The subscribe effect below also
  // `setEvents([])`, but that lags one render: on the first render after a
  // sid flip, `events` would still expose the PREVIOUS sid's stream. A
  // consumer that folds `events` into a per-sid transcript (ChatConsole)
  // then grafts the old session's messages onto the freshly-opened one. By
  // clearing here, a brand-new sid is observed with an empty buffer from its
  // very first render — streams never mix across a switch.
  const [streamedSid, setStreamedSid] = useState<string | null>(sid);
  if (sid !== streamedSid) {
    setStreamedSid(sid);
    setEvents([]);
  }

  const esRef = useRef<EventSource | null>(null);
  const retryTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const retryCountRef = useRef(0);

  useEffect(() => {
    // No sid (or disabled) — tear the stream down and reset state so the
    // pane renders empty rather than stale events from the previous sid.
    if (!sid || !enabled) {
      if (retryTimerRef.current) {
        clearTimeout(retryTimerRef.current);
        retryTimerRef.current = null;
      }
      esRef.current?.close();
      esRef.current = null;
      retryCountRef.current = 0;
      let cancelled = false;
      queueMicrotask(() => {
        if (cancelled) return;
        setConnected(false);
        setLastError(null);
        setGatewayUnavailable(false);
      });
      return () => {
        cancelled = true;
      };
    }

    // Switching sid: drop the previous sid's events so streams never mix.
    setEvents([]);
    setGatewayUnavailable(false);
    const url = sessionEventsUrl(sid);
    let cancelled = false;

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

      es.addEventListener("progress", (ev) => {
        if (cancelled) return;
        const parsed = parseSessionEvent((ev as MessageEvent).data);
        if (parsed) setEvents((prev) => appendSessionEvent(prev, parsed));
      });

      // No live gateway: server emits one `gateway_unavailable` frame and
      // keeps the stream open (no 503 retry-loop). Surface it instead of
      // hammering reconnects.
      es.addEventListener("gateway_unavailable", () => {
        if (cancelled) return;
        setGatewayUnavailable(true);
      });

      es.addEventListener("reconnect_hint", () => {
        if (cancelled) return;
        es.close();
        scheduleReconnect("server requested reconnect");
      });

      es.addEventListener("error", () => {
        if (cancelled) return;
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
    // `sid` keys the subscription; `enabled` toggles the whole effect.
  }, [sid, enabled]);

  return { events, connected, lastError, gatewayUnavailable };
}
