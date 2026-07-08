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
  kind: "answer" | "progress" | "activity";
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
  // Three-way kind; anything unrecognized degrades to "answer" (the parser
  // never throws on a future/garbage kind — the next frame lands clean).
  const kind =
    obj.kind === "progress" ? "progress" : obj.kind === "activity" ? "activity" : "answer";
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
  // v0.8.22 P1 (review §3.1-3) — the highest replay-ring seq applied so far
  // for the CURRENT sid. Threaded into the reconnect URL as `last_event_id`
  // (the query fallback — `EventSource` can't set the standard header) and
  // used by `shouldAcceptEventSeq` to dedupe a replayed/reseeded frame this
  // hook already rendered. Survives across reconnects WITHIN one sid (reset
  // only on an actual sid change, below) — that persistence is exactly what
  // makes the reconnect watermark work.
  const lastSeenSeqRef = useRef(0);
  if (sid !== streamedSid) {
    setStreamedSid(sid);
    setEvents([]);
    // A new sid starts its own seq watermark; carrying over the PREVIOUS
    // sid's seq would either wrongly drop the new sid's early frames (if it
    // happened to be numerically lower) or just be meaningless (ring seqs
    // are per-sid, not global).
    lastSeenSeqRef.current = 0;
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

    let cancelled = false;
    // Switching sid: drop the previous sid's events so streams never mix.
    queueMicrotask(() => {
      if (cancelled) return;
      setEvents([]);
      setGatewayUnavailable(false);
    });

    const connect = () => {
      if (cancelled) return;
      // v0.8.22 P1 (review §3.1-3) — build the URL AT CONNECT TIME (not once
      // per effect run): a reconnect must carry the watermark accumulated so
      // far, and this closure runs again on every `scheduleReconnect` retry
      // — a genuinely NEW `EventSource`, not the browser's own auto-retry
      // (which would resend `Last-Event-ID` on its own but never fires here
      // since we always `.close()` first).
      const es = new EventSource(sessionEventsUrl(sid, lastSeenSeqRef.current));
      esRef.current = es;

      es.addEventListener("open", () => {
        if (cancelled) return;
        retryCountRef.current = 0;
        setConnected(true);
        setLastError(null);
      });

      es.addEventListener("progress", (ev) => {
        if (cancelled) return;
        const msgEvent = ev as MessageEvent;
        // v0.8.22 P1 — dedupe a replayed/reseeded frame this hook already
        // rendered (the ring-replay/reseed catchup batch can legitimately
        // repeat a frame at a reconnect boundary — see `ring.rs`'s module
        // doc); a frame with no parseable seq always passes through.
        const decision = shouldAcceptEventSeq(msgEvent.lastEventId, lastSeenSeqRef.current);
        lastSeenSeqRef.current = decision.nextHighest;
        if (!decision.accept) return;
        const parsed = parseSessionEvent(msgEvent.data);
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
