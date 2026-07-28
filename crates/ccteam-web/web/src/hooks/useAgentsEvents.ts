// v0.9.0 W4 (F4) — the team view's GLOBAL SSE hook: `GET /api/v1/agents/events`.
// Mechanics mirror `useSessionEvents` verbatim (same server contract: named
// `progress`/`delegation` events carry the data, `onmessage` never fires;
// controlled backoff 1s→30s cap 7 retries; `reconnect_hint` → close+reopen);
// reuses its exported, sid-agnostic `shouldAcceptEventSeq` so the two hooks'
// reconnect-dedup logic can never drift apart.

import { useEffect, useRef, useState } from "react";
import { createAuthedEventSource } from "../lib/authedEventSource";
import { shouldAcceptEventSeq } from "./useSessionEvents";
import type { SessionActivity } from "./useSessionEvents";

/** One frame off the global SSE stream: every ordinary per-session event
 *  (`answer`/`progress`/`activity`, now carrying `slug`) PLUS a delegation
 *  lifecycle transition (`kind: "delegation"`). Mirrors
 *  `session_event_payload`'s extended shape (`ccteam-web/src/routes/agents.rs`). */
export interface AgentsEvent {
  id?: string;
  sid?: string;
  slug?: string;
  kind: "answer" | "progress" | "activity" | "delegation" | "session_lifecycle";
  content: string;
  done?: boolean;
  activity?: SessionActivity;
  /** Delegation-only: one of spawned|dispatched|completed|notified|
   *  collected|stopped|denied. */
  relation?: string;
  parent_sid?: string;
  child_sid?: string;
  title?: string;
  reason?: string;
}

export interface UseAgentsEventsResult {
  events: AgentsEvent[];
  connected: boolean;
  lastError: string | null;
  gatewayUnavailable: boolean;
}

export const AGENTS_RING_CAP = 500;
const MAX_RETRIES = 7;
const RETRY_BASE_MS = 1000;
const RETRY_CAP_MS = 30000;
const retryDelayMs = (attempt: number): number =>
  Math.min(RETRY_CAP_MS, RETRY_BASE_MS * 2 ** (attempt - 1));

/** Build the global SSE URL. Exported for unit tests. */
export function agentsEventsUrl(lastEventId?: number): string {
  const base = "/api/v1/agents/events";
  return lastEventId && lastEventId > 0 ? `${base}?last_event_id=${lastEventId}` : base;
}

/** Parse one raw `progress`/`delegation` payload into an {@link AgentsEvent},
 *  or `null` for garbage / a non-object. Pure + DOM-free. */
export function parseAgentsEvent(raw: string): AgentsEvent | null {
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return null;
  }
  if (typeof parsed !== "object" || parsed === null) return null;
  const obj = parsed as Record<string, unknown>;
  const kind =
    obj.kind === "progress"
      ? "progress"
      : obj.kind === "activity"
        ? "activity"
        : obj.kind === "delegation"
          ? "delegation"
          : obj.kind === "session_lifecycle"
            ? "session_lifecycle"
          : "answer";
  const event: AgentsEvent = {
    kind,
    content: typeof obj.content === "string" ? obj.content : "",
  };
  if (typeof obj.id === "string") event.id = obj.id;
  if (typeof obj.sid === "string") event.sid = obj.sid;
  if (typeof obj.slug === "string") event.slug = obj.slug;
  if (obj.done === true) event.done = true;
  if (typeof obj.activity === "object" && obj.activity !== null) {
    const a = obj.activity as Record<string, unknown>;
    event.activity = {
      kind: typeof a.kind === "string" ? a.kind : "",
      name: typeof a.name === "string" ? a.name : "",
      summary: typeof a.summary === "string" ? a.summary : "",
      status: typeof a.status === "string" ? a.status : "",
      item_id: typeof a.item_id === "string" ? a.item_id : "",
    };
  }
  if (typeof obj.relation === "string") event.relation = obj.relation;
  if (typeof obj.parent_sid === "string") event.parent_sid = obj.parent_sid;
  if (typeof obj.child_sid === "string") event.child_sid = obj.child_sid;
  if (typeof obj.title === "string") event.title = obj.title;
  if (typeof obj.reason === "string") event.reason = obj.reason;
  return event;
}

/** Append `event` to a capped ring buffer, returning a NEW array. */
export function appendAgentsEvent(prev: AgentsEvent[], event: AgentsEvent): AgentsEvent[] {
  if (prev.length >= AGENTS_RING_CAP) {
    return [...prev.slice(prev.length - AGENTS_RING_CAP + 1), event];
  }
  return [...prev, event];
}

export function useAgentsEvents(
  enabled: boolean = true,
  filter: "all" | "session_lifecycle" = "all",
): UseAgentsEventsResult {
  const [events, setEvents] = useState<AgentsEvent[]>([]);
  const [connected, setConnected] = useState(false);
  const [lastError, setLastError] = useState<string | null>(null);
  const [gatewayUnavailable, setGatewayUnavailable] = useState(false);
  const lastSeenSeqRef = useRef(0);
  // fetch-backed (Bearer + cookie), same as useSessionEvents — native
  // EventSource is cookie-only and 401s when localStorage Bearer is the
  // only live auth path.
  const esRef = useRef<ReturnType<typeof createAuthedEventSource> | null>(null);
  const retryTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const retryCountRef = useRef(0);

  useEffect(() => {
    if (!enabled) {
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

    let cancelled = false;

    const onFrame = (ev: Event) => {
      if (cancelled) return;
      const msgEvent = ev as MessageEvent;
      const decision = shouldAcceptEventSeq(msgEvent.lastEventId, lastSeenSeqRef.current);
      lastSeenSeqRef.current = decision.nextHighest;
      if (!decision.accept) return;
      const parsed = parseAgentsEvent(msgEvent.data);
      if (parsed && (filter === "all" || parsed.kind === "session_lifecycle")) {
        setEvents((prev) => appendAgentsEvent(prev, parsed));
      }
    };

    const connect = () => {
      if (cancelled) return;
      const es = createAuthedEventSource(agentsEventsUrl(lastSeenSeqRef.current));
      esRef.current = es;

      es.addEventListener("open", () => {
        if (cancelled) return;
        retryCountRef.current = 0;
        setConnected(true);
        setLastError(null);
      });

      // The two named event types the server emits (see
      // `crate::routes::agents::agents_event`) — both carry the SAME JSON
      // shape, just a different SSE `event:` name.
      es.addEventListener("progress", onFrame);
      es.addEventListener("delegation", onFrame);
      es.addEventListener("session_lifecycle", onFrame);

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
  }, [enabled, filter]);

  return { events, connected, lastError, gatewayUnavailable };
}
