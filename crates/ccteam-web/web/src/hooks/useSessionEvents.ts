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
// Auth: native EventSource cannot set an Authorization header. The SPA
// keeps the token in localStorage (Bearer on REST) and the server sets an
// HttpOnly `ccteam_token` cookie on `?token=` login — those two can desync.
// We open the stream via `createAuthedEventSource` (fetch + Bearer +
// credentials) so a missing cookie no longer 401s the live reply stream
// while REST still works.

import { useEffect, useRef, useState } from "react";
import { createAuthedEventSource } from "../lib/authedEventSource";

/** One selectable option on an approval ChoicePrompt (the SSE frame's
 *  `options[]`). `label` is the button text; `id` is the stable decision
 *  value (e.g. `"allow"` / `"deny"`) the web resolve path sends back as
 *  `selection`. v0.8.7 review-fix (R-H1). */
export interface SessionEventOption {
  label: string;
  id: string;
}

/** One structured per-step activity (v0.8.19) — a tool call, reasoning,
 *  command, file change, or web search the session is doing. `kind` is the
 *  category (`tool_call` / `thinking` / `command_exec` / `file_change` /
 *  `web_search` / `tool_result`); `name` is the tool/category name (empty
 *  for thinking); `summary` is the one-line preview; `status` is the
 *  lifecycle phase (`started` / `completed` / `update`); `item_id` lets the
 *  UI dedup/merge a start↔complete pair. Mirrors the Rust `SessionActivity`
 *  serde shape. */
export interface SessionActivity {
  kind: string;
  name: string;
  summary: string;
  status: string;
  item_id: string;
}

/** One event line off the per-session SSE stream (the W2 payload shape).
 *  `kind` is "answer" (assistant reply / approval prompt), "progress"
 *  (status edit, `done` on the finalizing one), or "activity" (a structured
 *  per-step `activity` payload, v0.8.19). `options` present + non-empty
 *  marks an approval prompt; `token` is then the pending-resolution token
 *  the web resolve path POSTs back (R-H1). */
export interface SessionEvent {
  id?: string;
  sid?: string;
  kind: "answer" | "progress" | "activity" | "session_lifecycle" | "scheduled_changed";
  content: string;
  done?: boolean;
  options?: SessionEventOption[];
  token?: string;
  /** Present on an "activity" frame: the structured per-step payload. */
  activity?: SessionActivity;
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
  /** Successful-open generation for this sid. `1` is the initial stream;
   *  every later value marks a reconnect that consumers can use as an
   *  authoritative-reseed barrier. Resets to `0` when the sid changes or
   *  streaming is disabled. */
  connectionEpoch: number;
}

export const SESSION_RING_CAP = 500;
const MAX_RETRIES = 7;
const RETRY_BASE_MS = 1000;
const RETRY_CAP_MS = 30000;
const retryDelayMs = (attempt: number): number =>
  Math.min(RETRY_CAP_MS, RETRY_BASE_MS * 2 ** (attempt - 1));

/** Build the per-session SSE URL. Exported for unit tests + so callers
 *  share one template.
 *
 *  v0.8.22 P1 (review §3.1-3) — `lastEventId`, when given and > 0, is
 *  appended as `?last_event_id=`: the reconnect watermark. `EventSource`
 *  cannot set arbitrary request headers, and this hook deliberately opens a
 *  BRAND NEW `EventSource` on every reconnect (see `connect()` below), so
 *  the browser's native "resend `Last-Event-ID` on ITS OWN reconnect"
 *  behavior never applies — the server has to be told via the query string
 *  instead (it also accepts the standard header, for non-browser clients). */
export function sessionEventsUrl(sid: string, lastEventId?: number): string {
  const base = `/api/v1/sessions/${encodeURIComponent(sid)}/events`;
  return lastEventId && lastEventId > 0 ? `${base}?last_event_id=${lastEventId}` : base;
}

/** Decide whether an incoming SSE frame should be appended, given the
 *  highest replay-ring `seq` already applied (from the frame's SSE-level
 *  `id:`, i.e. `MessageEvent.lastEventId` — NOT the JSON payload's own
 *  `id`, which some frames intentionally REUSE across edits, e.g. a
 *  `Progress` status message edited in place; that field must never be a
 *  dedup key). A frame with no parseable seq (id missing/non-numeric, e.g.
 *  the very first events emitted before this field existed, or a
 *  non-numeric id from a future server) always passes through — only
 *  numbered frames participate in dedup. Pure + DOM-free so it is
 *  unit-testable without a real `EventSource`. Used both to filter a
 *  replayed/reseeded duplicate at reconnect AND to advance the watermark
 *  for the next reconnect. */
export function shouldAcceptEventSeq(
  lastEventId: string | undefined,
  highestSeenSeq: number,
): { accept: boolean; nextHighest: number } {
  if (!lastEventId) return { accept: true, nextHighest: highestSeenSeq };
  const seq = Number(lastEventId);
  if (!Number.isFinite(seq)) return { accept: true, nextHighest: highestSeenSeq };
  if (seq <= highestSeenSeq) return { accept: false, nextHighest: highestSeenSeq };
  return { accept: true, nextHighest: seq };
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
  // Known event kinds; anything unrecognized degrades to "answer" (the parser
  // never throws on a future/garbage kind — the next frame lands clean).
  const kind =
    obj.kind === "progress"
      ? "progress"
      : obj.kind === "activity"
        ? "activity"
        : obj.kind === "session_lifecycle"
          ? "session_lifecycle"
          : obj.kind === "scheduled_changed"
            ? "scheduled_changed"
          : "answer";
  const event: SessionEvent = {
    kind,
    content: typeof obj.content === "string" ? obj.content : "",
  };
  if (typeof obj.id === "string") event.id = obj.id;
  if (typeof obj.sid === "string") event.sid = obj.sid;
  if (obj.done === true) event.done = true;
  // v0.8.19 — the structured per-step activity object (DOM-free, defensive:
  // a malformed `activity` is simply dropped, leaving a bare "activity" event
  // whose `content` line still renders).
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

interface SessionEventSource {
  addEventListener(type: string, listener: EventListener): void;
  close(): void;
}

/** Browser dependencies for the reconnect controller. Exported so the real
 *  lifecycle (including retry exhaustion + visibility revival) is testable
 *  in Vitest's dependency-free node environment. */
export interface SessionEventStreamEnvironment {
  createEventSource(url: string): SessionEventSource;
  document: {
    readonly visibilityState: DocumentVisibilityState;
    addEventListener(type: string, listener: EventListener): void;
    removeEventListener(type: string, listener: EventListener): void;
  };
  window: {
    addEventListener(type: string, listener: EventListener): void;
    removeEventListener(type: string, listener: EventListener): void;
  };
  setTimer(callback: () => void, delay: number): ReturnType<typeof setTimeout>;
  clearTimer(timer: ReturnType<typeof setTimeout>): void;
}

export interface SessionEventStreamCallbacks {
  onOpen(connectionEpoch: number): void;
  onEvent(event: SessionEvent): void;
  onDisconnected(): void;
  onError(error: string | null): void;
  onGatewayUnavailable(): void;
}

/** Own one sid's EventSource + bounded retry lifecycle. The hook below is a
 *  thin React-state adapter around this controller; keeping the browser
 *  lifecycle here prevents the retry and visibility paths from drifting. */
export function startSessionEventStream(
  sid: string,
  lastSeenSeqRef: { current: number },
  callbacks: SessionEventStreamCallbacks,
  environment?: SessionEventStreamEnvironment,
): () => void {
  const env: SessionEventStreamEnvironment = environment ?? {
    // fetch-backed stream carries localStorage Bearer (and cookies). Native
    // EventSource is cookie-only and 401s when the cookie is missing while
    // the SPA still has a valid Bearer — the "send works, reply needs
    // refresh" failure mode.
    createEventSource: (url) => createAuthedEventSource(url),
    document,
    window,
    setTimer: (callback, delay) => setTimeout(callback, delay),
    clearTimer: (timer) => clearTimeout(timer),
  };
  let source: SessionEventSource | null = null;
  let retryTimer: ReturnType<typeof setTimeout> | null = null;
  let retryCount = 0;
  let connectionEpoch = 0;
  let connected = false;
  let cancelled = false;

  function scheduleReconnect(reason: string): void {
    connected = false;
    callbacks.onDisconnected();
    if (retryCount >= MAX_RETRIES) {
      callbacks.onError(`SSE max retries reached (${reason})`);
      return;
    }
    retryCount += 1;
    retryTimer = env.setTimer(() => {
      retryTimer = null;
      connect();
    }, retryDelayMs(retryCount));
  }

  function connect(): void {
    if (cancelled) return;
    const es = env.createEventSource(sessionEventsUrl(sid, lastSeenSeqRef.current));
    source = es;
    const current = () => !cancelled && source === es;

    es.addEventListener("open", () => {
      if (!current()) return;
      retryCount = 0;
      connected = true;
      connectionEpoch += 1;
      callbacks.onOpen(connectionEpoch);
    });
    es.addEventListener("progress", (event) => {
      if (!current()) return;
      const message = event as MessageEvent;
      const decision = shouldAcceptEventSeq(message.lastEventId, lastSeenSeqRef.current);
      lastSeenSeqRef.current = decision.nextHighest;
      if (!decision.accept) return;
      const parsed = parseSessionEvent(message.data);
      if (parsed) callbacks.onEvent(parsed);
    });
    es.addEventListener("gateway_unavailable", () => {
      if (current()) callbacks.onGatewayUnavailable();
    });
    es.addEventListener("reconnect_hint", () => {
      if (!current()) return;
      es.close();
      source = null;
      scheduleReconnect("server requested reconnect");
    });
    es.addEventListener("error", () => {
      if (!current()) return;
      es.close();
      source = null;
      scheduleReconnect("connection lost");
    });
  }

  function reconnectNow(): void {
    if (cancelled || connected) return;
    if (retryTimer) {
      env.clearTimer(retryTimer);
      retryTimer = null;
    }
    source?.close();
    source = null;
    retryCount = 0;
    callbacks.onError(null);
    connect();
  }

  const onVisibilityChange: EventListener = () => {
    if (env.document.visibilityState === "visible") reconnectNow();
  };
  const onOnline: EventListener = () => reconnectNow();
  env.document.addEventListener("visibilitychange", onVisibilityChange);
  env.window.addEventListener("online", onOnline);
  connect();

  return () => {
    cancelled = true;
    if (retryTimer) env.clearTimer(retryTimer);
    retryTimer = null;
    source?.close();
    source = null;
    env.document.removeEventListener("visibilitychange", onVisibilityChange);
    env.window.removeEventListener("online", onOnline);
  };
}

export function useSessionEvents(
  sid: string | null,
  enabled: boolean = true,
): UseSessionEventsResult {
  const [events, setEvents] = useState<SessionEvent[]>([]);
  const [connected, setConnected] = useState(false);
  const [lastError, setLastError] = useState<string | null>(null);
  const [gatewayUnavailable, setGatewayUnavailable] = useState(false);
  const [connectionEpoch, setConnectionEpoch] = useState(0);

  // Reset the event buffer SYNCHRONOUSLY when the sid changes (React's
  // "adjust state during render" pattern). The subscribe effect below also
  // `setEvents([])`, but that lags one render: on the first render after a
  // sid flip, `events` would still expose the PREVIOUS sid's stream. A
  // consumer that folds `events` into a per-sid transcript (ChatConsole)
  // then grafts the old session's messages onto the freshly-opened one. By
  // clearing here, a brand-new sid is observed with an empty buffer from its
  // very first render — streams never mix across a switch.
  const [streamedSid, setStreamedSid] = useState<string | null>(sid);
  // v0.8.22 P1 (review §3.1-3) — the highest replay-ring seq applied so far
  // for the CURRENT sid. Threaded into the reconnect URL as `last_event_id`
  // (the query fallback — `EventSource` can't set the standard header) and
  // used by `shouldAcceptEventSeq` to dedupe a replayed/reseeded frame this
  // hook already rendered. Survives across reconnects WITHIN one sid (reset
  // only on an actual sid change, below) — that persistence is exactly what
  // makes the reconnect watermark work.
  const lastSeenSeqRef = useRef(0);
  const watermarkSidRef = useRef<string | null>(sid);
  if (sid !== streamedSid) {
    setStreamedSid(sid);
    setEvents([]);
    setConnectionEpoch(0);
  }

  useEffect(() => {
    // A new sid starts its own seq watermark; carrying over the PREVIOUS
    // sid's seq would either wrongly drop the new sid's early frames (if it
    // happened to be numerically lower) or just be meaningless (ring seqs
    // are per-sid, not global). Refs are synchronized in the effect rather
    // than during render so React's render phase stays pure.
    if (watermarkSidRef.current !== sid) {
      watermarkSidRef.current = sid;
      lastSeenSeqRef.current = 0;
    }

    // No sid (or disabled) — tear the stream down and reset state so the
    // pane renders empty rather than stale events from the previous sid.
    if (!sid || !enabled) {
      let cancelled = false;
      queueMicrotask(() => {
        if (cancelled) return;
        setConnected(false);
        setLastError(null);
        setGatewayUnavailable(false);
        setConnectionEpoch(0);
      });
      return () => {
        cancelled = true;
      };
    }

    let cancelled = false;
    // Switching sid: drop the previous sid's events so streams never mix.
    queueMicrotask(() => {
      if (cancelled) return;
      setEvents([]);
      setGatewayUnavailable(false);
      setConnectionEpoch(0);
    });

    const stop = startSessionEventStream(sid, lastSeenSeqRef, {
      onOpen: (epoch) => {
        if (cancelled) return;
        setConnected(true);
        setLastError(null);
        setConnectionEpoch(epoch);
      },
      onEvent: (event) => {
        if (!cancelled) setEvents((previous) => appendSessionEvent(previous, event));
      },
      onDisconnected: () => {
        if (!cancelled) setConnected(false);
      },
      onError: (error) => {
        if (!cancelled) setLastError(error);
      },
      onGatewayUnavailable: () => {
        if (!cancelled) setGatewayUnavailable(true);
      },
    });

    return () => {
      cancelled = true;
      stop();
    };
    // `sid` keys the subscription; `enabled` toggles the whole effect.
  }, [sid, enabled]);

  return { events, connected, lastError, gatewayUnavailable, connectionEpoch };
}
