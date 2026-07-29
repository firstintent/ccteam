// Browser EventSource cannot set Authorization headers. The SPA keeps the
// auth token in localStorage and injects `Bearer` on every `fetch` (see
// `fetchInterceptor.ts`); native EventSource only sends the HttpOnly
// `ccteam_token` cookie. When those two paths desync (cookie missing /
// expired / blocked while localStorage Bearer still works), REST succeeds
// but `GET /api/v1/sessions/{sid}/events` 401s forever — chat appears
// silent until a full page reload re-reads history via REST.
//
// This module is a minimal EventSource-shaped stream over `fetch` that
// carries the same Bearer the REST path uses (and still includes cookies
// for cookie-only sessions). Drop-in for `new EventSource(url)`.

import { getToken } from "./token";

export interface AuthedEventSource {
  addEventListener(type: string, listener: EventListener): void;
  close(): void;
  readonly url: string;
}

/** Same-origin check mirroring `fetchInterceptor` — never put the Bearer
 *  on a cross-origin URL (defense in depth if a future caller passes an
 *  absolute off-site URL). */
export function isSameOriginUrl(url: string): boolean {
  if (url.startsWith("/")) return true;
  try {
    if (typeof window === "undefined" || !window.location) return false;
    return new URL(url, window.location.origin).origin === window.location.origin;
  } catch {
    return false;
  }
}

/** Open an SSE stream authenticated like the rest of the SPA.
 *
 *  - Always `credentials: "same-origin"` so a valid `ccteam_token` cookie
 *    still authenticates cookie-only sessions.
 *  - When localStorage has a token AND the URL is same-origin, also send
 *    `Authorization: Bearer …` so a missing cookie no longer bricks the
 *    live stream.
 */
export function createAuthedEventSource(url: string): AuthedEventSource {
  const listeners = new Map<string, EventListener[]>();
  const abort = new AbortController();
  let closed = false;

  const addEventListener = (type: string, listener: EventListener): void => {
    const bag = listeners.get(type) ?? [];
    bag.push(listener);
    listeners.set(type, bag);
  };

  const emit = (type: string, event: Event): void => {
    for (const listener of listeners.get(type) ?? []) {
      try {
        // EventListener is `(evt) => void` in our usage; call as a plain
        // function (native EventSource does not bind `this` either).
        listener(event);
      } catch {
        // Listener errors must not tear the stream down.
      }
    }
  };

  const close = (): void => {
    if (closed) return;
    closed = true;
    abort.abort();
  };

  const headers: Record<string, string> = {
    Accept: "text/event-stream",
  };
  const token = getToken();
  if (token && isSameOriginUrl(url)) {
    headers.Authorization = `Bearer ${token}`;
  }

  // Fire-and-forget; lifecycle is owned by `close()` / abort.
  void (async () => {
    try {
      const res = await fetch(url, {
        method: "GET",
        headers,
        credentials: "same-origin",
        signal: abort.signal,
        // Disable HTTP cache so a reconnect always hits the live stream.
        cache: "no-store",
      });
      if (closed) return;
      if (!res.ok || !res.body) {
        emit("error", new Event("error"));
        return;
      }
      emit("open", new Event("open"));
      await readSseStream(res.body, (eventName, data, id) => {
        if (closed) return;
        const event = new MessageEvent(eventName, {
          data,
          lastEventId: id ?? "",
        });
        emit(eventName, event);
      });
      // Stream ended cleanly (server closed) — surface as error so the
      // caller's reconnect scheduler runs (same as EventSource on close).
      if (!closed) emit("error", new Event("error"));
    } catch (err) {
      if (closed) return;
      // AbortError from close() is not an error the caller should retry on
      // via this path — closed gate above already returned. Other failures
      // (network, 401 after interceptor, etc.) → error event.
      if (err instanceof DOMException && err.name === "AbortError") return;
      emit("error", new Event("error"));
    }
  })();

  return { addEventListener, close, url };
}

/** Parse a WHATWG SSE byte stream into named frames.
 *
 *  Spec subset we need: `event:`, `data:`, `id:`, blank-line dispatch,
 *  multi-line data joined with `\n`. Comments (`:`) ignored.
 *
 *  Note on `id:`: the WHATWG EventSource spec keeps lastEventId across
 *  frames until a later frame sets a new id. We reset after each dispatch
 *  (simpler). Safe for ccteam because the server stamps `.id(seq)` on
 *  every progress frame (`sessions_api` / `agents`); if a future frame
 *  omits `id:`, `MessageEvent.lastEventId` will be empty for that frame
 *  only — reconnect watermark still advances from frames that carry id.
 */
async function readSseStream(
  body: ReadableStream<Uint8Array>,
  onFrame: (eventName: string, data: string, id: string | undefined) => void,
): Promise<void> {
  const reader = body.getReader();
  const decoder = new TextDecoder("utf-8");
  let buffer = "";
  let eventName = "message";
  let dataLines: string[] = [];
  let id: string | undefined;

  const dispatch = (): void => {
    if (dataLines.length === 0) {
      eventName = "message";
      id = undefined;
      return;
    }
    const data = dataLines.join("\n");
    onFrame(eventName, data, id);
    eventName = "message";
    dataLines = [];
    // See module note: reset per frame (server always re-stamps id).
    id = undefined;
  };

  const handleLine = (line: string): void => {
    if (line === "") {
      dispatch();
      return;
    }
    if (line.startsWith(":")) return; // comment / keep-alive
    const colon = line.indexOf(":");
    const field = colon === -1 ? line : line.slice(0, colon);
    let value = colon === -1 ? "" : line.slice(colon + 1);
    if (value.startsWith(" ")) value = value.slice(1);
    switch (field) {
      case "event":
        eventName = value || "message";
        break;
      case "data":
        dataLines.push(value);
        break;
      case "id":
        // Empty id resets per spec; we treat it as "no id".
        id = value || undefined;
        break;
      default:
        break;
    }
  };

  while (true) {
    const { done, value } = await reader.read();
    if (done) {
      buffer += decoder.decode();
      if (buffer.length > 0) {
        // Flush a trailing line without a terminating newline.
        handleLine(buffer);
        buffer = "";
        dispatch();
      }
      break;
    }
    buffer += decoder.decode(value, { stream: true });
    // SSE frames are newline-delimited; keep the incomplete tail in buffer.
    let nl: number;
    while ((nl = buffer.indexOf("\n")) !== -1) {
      let line = buffer.slice(0, nl);
      buffer = buffer.slice(nl + 1);
      if (line.endsWith("\r")) line = line.slice(0, -1);
      handleLine(line);
    }
  }
}
