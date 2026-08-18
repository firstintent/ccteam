// v0.9.0 W4 (F4) — the team view's GLOBAL SSE hook: `GET /api/v1/agents/events`.
// Mechanics mirror `useSessionEvents` verbatim (same server contract: named
// `progress`/`delegation` events carry the data, `onmessage` never fires;
// controlled backoff 1s→30s cap 7 retries; `reconnect_hint` → close+reopen);
// reuses its exported, sid-agnostic `shouldAcceptEventSeq` so the two hooks'
// reconnect-dedup logic can never drift apart.
//
// ONE CONNECTION, MANY CONSUMERS (2026-08-02). This endpoint is GLOBAL — every
// consumer asks for the same feed — but the hook used to open its own stream
// per call site, so `AgentsView` + `ChatConsole` mounted together held TWO
// sockets and downloaded the identical frames twice. A browser opens only ~6
// HTTP/1.1 connections per origin, so duplicate streams spend a scarce,
// permanently-held resource for nothing and bring socket exhaustion (every
// other request on the page stuck pending) that much closer. The stream now
// lives in a module-level broker that refcounts its subscribers: N consumers
// share one connection and one reconnect watermark, and any FUTURE consumer is
// covered automatically. Each subscriber still keeps its own filtered ring, so
// the hook's contract is unchanged.

import { useEffect, useRef, useState } from "react";
import { createAuthedEventSource } from "../lib/authedEventSource";
import { shouldAcceptEventSeq } from "./useSessionEvents";
import type { SessionActivity } from "./useSessionEvents";

/** One frame off the global SSE stream: every ordinary per-session event
 *  (`answer`/`progress`/`activity`, now carrying `slug`) PLUS a delegation
 *  lifecycle transition (`kind: "delegation"`). Mirrors
 *  `session_event_payload`'s extended shape (`ccteam-web/src/routes/agents.rs`). */
export interface AgentsEvent {
  /** Monotonic within one subscriber's bounded ring. Assigned on append so
   * consumers can keep a stable watermark even after older objects expire. */
  seq?: number;
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
  const sequenced = { ...event, seq: (prev[prev.length - 1]?.seq ?? 0) + 1 };
  if (prev.length >= AGENTS_RING_CAP) {
    return [...prev.slice(prev.length - AGENTS_RING_CAP + 1), sequenced];
  }
  return [...prev, sequenced];
}

/** Connection health, broadcast to every subscriber of the shared stream. */
export interface AgentsStreamStatus {
  connected: boolean;
  lastError: string | null;
  gatewayUnavailable: boolean;
}

/** What a subscriber hands the broker: one callback per frame, one for health. */
export interface AgentsStreamSubscriber {
  onFrame(event: AgentsEvent): void;
  onStatus(status: AgentsStreamStatus): void;
}

/** The bits of `AuthedEventSource` the broker needs — injectable so the
 *  sharing/refcount invariants are testable without a real network. */
export interface AgentsEventSourceLike {
  addEventListener(type: string, listener: EventListener): void;
  close(): void;
}

export interface AgentsStreamEnvironment {
  createEventSource(url: string): AgentsEventSourceLike;
  setTimer(callback: () => void, delay: number): ReturnType<typeof setTimeout>;
  clearTimer(timer: ReturnType<typeof setTimeout>): void;
}

const defaultEnv: AgentsStreamEnvironment = {
  // fetch-backed (Bearer + cookie), same as useSessionEvents — native
  // EventSource is cookie-only and 401s when localStorage Bearer is the
  // only live auth path.
  createEventSource: (url) => createAuthedEventSource(url),
  setTimer: (callback, delay) => setTimeout(callback, delay),
  clearTimer: (timer) => clearTimeout(timer),
};

/** The single shared stream. `lastSeenSeq` survives the last unsubscribe so a
 *  remount resumes from its watermark instead of replaying the whole ring. */
const broker = {
  subscribers: new Set<AgentsStreamSubscriber>(),
  source: null as AgentsEventSourceLike | null,
  retryTimer: null as ReturnType<typeof setTimeout> | null,
  retryCount: 0,
  lastSeenSeq: 0,
  status: {
    connected: false,
    lastError: null,
    gatewayUnavailable: false,
  } as AgentsStreamStatus,
  env: defaultEnv,
};

function publishStatus(patch: Partial<AgentsStreamStatus>): void {
  broker.status = { ...broker.status, ...patch };
  for (const sub of broker.subscribers) sub.onStatus(broker.status);
}

function connectBroker(): void {
  if (broker.subscribers.size === 0) return;
  const es = broker.env.createEventSource(agentsEventsUrl(broker.lastSeenSeq));
  broker.source = es;
  // Ignore a stream we have already replaced or closed (a late frame from an
  // aborted fetch must not advance the shared watermark).
  const current = () => broker.source === es;

  const onFrame = (ev: Event) => {
    if (!current()) return;
    const msgEvent = ev as MessageEvent;
    const decision = shouldAcceptEventSeq(msgEvent.lastEventId, broker.lastSeenSeq);
    broker.lastSeenSeq = decision.nextHighest;
    if (!decision.accept) return;
    const parsed = parseAgentsEvent(msgEvent.data);
    if (!parsed) return;
    for (const sub of broker.subscribers) sub.onFrame(parsed);
  };

  es.addEventListener("open", () => {
    if (!current()) return;
    broker.retryCount = 0;
    publishStatus({ connected: true, lastError: null });
  });

  // The named event types the server emits (see
  // `crate::routes::agents::agents_event`) — all carry the SAME JSON shape,
  // just a different SSE `event:` name.
  es.addEventListener("progress", onFrame);
  es.addEventListener("delegation", onFrame);
  es.addEventListener("session_lifecycle", onFrame);

  es.addEventListener("gateway_unavailable", () => {
    if (!current()) return;
    publishStatus({ gatewayUnavailable: true });
  });

  es.addEventListener("reconnect_hint", () => {
    if (!current()) return;
    es.close();
    broker.source = null;
    scheduleBrokerReconnect("server requested reconnect");
  });

  es.addEventListener("error", () => {
    if (!current()) return;
    es.close();
    broker.source = null;
    scheduleBrokerReconnect("connection lost");
  });
}

function scheduleBrokerReconnect(reason: string): void {
  publishStatus({ connected: false });
  if (broker.subscribers.size === 0) return;
  if (broker.retryCount >= MAX_RETRIES) {
    publishStatus({ lastError: `SSE max retries reached (${reason})` });
    return;
  }
  broker.retryCount += 1;
  const delay = retryDelayMs(broker.retryCount);
  broker.retryTimer = broker.env.setTimer(() => {
    broker.retryTimer = null;
    connectBroker();
  }, delay);
}

function teardownBroker(): void {
  if (broker.retryTimer !== null) {
    broker.env.clearTimer(broker.retryTimer);
    broker.retryTimer = null;
  }
  broker.source?.close();
  broker.source = null;
  broker.retryCount = 0;
  broker.status = { connected: false, lastError: null, gatewayUnavailable: false };
}

/** Join the shared global stream; returns the unsubscribe. The connection opens
 *  on the FIRST subscriber and closes after the LAST one leaves. */
export function subscribeAgentsStream(
  subscriber: AgentsStreamSubscriber,
  env?: AgentsStreamEnvironment,
): () => void {
  if (env) broker.env = env;
  const first = broker.subscribers.size === 0;
  broker.subscribers.add(subscriber);
  if (first) {
    connectBroker();
  } else {
    // Late joiner: hand it the current health immediately rather than leave it
    // showing "disconnected" until the next transition.
    subscriber.onStatus(broker.status);
  }
  return () => {
    broker.subscribers.delete(subscriber);
    if (broker.subscribers.size === 0) teardownBroker();
  };
}

/** Test-only: drop every subscriber and close the shared stream. */
export function resetAgentsStreamForTests(): void {
  broker.subscribers.clear();
  teardownBroker();
  broker.lastSeenSeq = 0;
  broker.env = defaultEnv;
}

/** Test-only: how many consumers share the stream, and whether it is open. */
export function agentsStreamDebugState(): { subscribers: number; open: boolean } {
  return { subscribers: broker.subscribers.size, open: broker.source !== null };
}

export function useAgentsEvents(
  enabled: boolean = true,
  filter: "all" | "session_lifecycle" = "all",
): UseAgentsEventsResult {
  const [events, setEvents] = useState<AgentsEvent[]>([]);
  const [connected, setConnected] = useState(false);
  const [lastError, setLastError] = useState<string | null>(null);
  const [gatewayUnavailable, setGatewayUnavailable] = useState(false);
  // Read the filter through a ref so switching it never re-subscribes (which
  // would churn the shared connection for every consumer). Refreshed in an
  // effect declared BEFORE the subscribe effect, never during render.
  const filterRef = useRef(filter);
  useEffect(() => {
    filterRef.current = filter;
  }, [filter]);

  useEffect(() => {
    if (!enabled) {
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
    return subscribeAgentsStream({
      onFrame: (event) => {
        if (filterRef.current !== "all" && event.kind !== filterRef.current) return;
        setEvents((prev) => appendAgentsEvent(prev, event));
      },
      onStatus: (status) => {
        setConnected(status.connected);
        setLastError(status.lastError);
        setGatewayUnavailable(status.gatewayUnavailable);
      },
    });
  }, [enabled]);

  return { events, connected, lastError, gatewayUnavailable };
}
