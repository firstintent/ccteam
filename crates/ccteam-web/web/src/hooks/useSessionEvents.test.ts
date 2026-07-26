// v0.8.7 W4 (DD.1) — useSessionEvents wiring tests.
//
// The hook itself needs EventSource + React (DOM); to stay node-env-
// friendly (no jsdom, the FIX-2 chatDefaults pattern) we extract and test
// the pure pieces: the SSE URL, the W2-shape parser, and the ring-buffer
// append. These are what the hook is built from.

import { afterEach, describe, expect, it, vi } from "vitest";

import {
  appendSessionEvent,
  parseSessionEvent,
  sessionEventsUrl,
  startSessionEventStream,
  shouldAcceptEventSeq,
  SESSION_RING_CAP,
  type SessionEvent,
} from "./useSessionEvents";

class FakeEventSource {
  readonly listeners = new Map<string, Array<(event: Event) => void>>();
  closed = false;

  addEventListener(type: string, listener: EventListenerOrEventListenerObject): void {
    const callback =
      typeof listener === "function" ? listener : (event: Event) => listener.handleEvent(event);
    const listeners = this.listeners.get(type) ?? [];
    listeners.push(callback);
    this.listeners.set(type, listeners);
  }

  close(): void {
    this.closed = true;
  }

  emit(type: string): void {
    for (const listener of this.listeners.get(type) ?? []) listener(new Event(type));
  }
}

function streamHarness() {
  const sources: FakeEventSource[] = [];
  const documentListeners = new Map<string, EventListener>();
  const windowListeners = new Map<string, EventListener>();
  let visibilityState: DocumentVisibilityState = "hidden";
  const errors: Array<string | null> = [];
  const epochs: number[] = [];
  const stop = startSessionEventStream(
    "s1",
    { current: 0 },
    {
      onOpen: (epoch) => epochs.push(epoch),
      onEvent: () => {},
      onDisconnected: () => {},
      onError: (error) => errors.push(error),
      onGatewayUnavailable: () => {},
    },
    {
      createEventSource: () => {
        const source = new FakeEventSource();
        sources.push(source);
        return source;
      },
      document: {
        get visibilityState() {
          return visibilityState;
        },
        addEventListener: (type, listener) => {
          documentListeners.set(type, listener as EventListener);
        },
        removeEventListener: (type) => {
          documentListeners.delete(type);
        },
      },
      window: {
        addEventListener: (type, listener) => {
          windowListeners.set(type, listener as EventListener);
        },
        removeEventListener: (type) => {
          windowListeners.delete(type);
        },
      },
      setTimer: (callback, delay) => setTimeout(callback, delay),
      clearTimer: (timer) => clearTimeout(timer),
    },
  );
  return {
    sources,
    errors,
    epochs,
    stop,
    showDocument() {
      visibilityState = "visible";
      documentListeners.get("visibilitychange")?.(new Event("visibilitychange"));
    },
  };
}

afterEach(() => {
  vi.useRealTimers();
});

describe("session event stream reconnect lifecycle", () => {
  it("increments the connection epoch on the second successful open", () => {
    vi.useFakeTimers();
    const stream = streamHarness();

    stream.sources[0].emit("open");
    stream.sources[0].emit("error");
    vi.runOnlyPendingTimers();
    stream.sources[1].emit("open");

    expect(stream.epochs).toEqual([1, 2]);
    stream.stop();
  });

  it("revives a given-up stream when the document becomes visible", () => {
    vi.useFakeTimers();
    const stream = streamHarness();

    // Initial source + seven scheduled retries; the eighth error exhausts
    // the retry budget and leaves the stream permanently idle until a
    // browser-lifecycle signal revives it.
    for (let attempt = 0; attempt < 8; attempt += 1) {
      stream.sources[attempt].emit("error");
      vi.runOnlyPendingTimers();
    }
    expect(stream.sources).toHaveLength(8);
    expect(stream.errors.at(-1)).toContain("SSE max retries reached");

    stream.showDocument();

    expect(stream.sources).toHaveLength(9);
    expect(stream.errors.at(-1)).toBeNull();
    stream.stop();
  });
});

describe("sessionEventsUrl", () => {
  it("targets the W2 per-sid SSE endpoint under /api/v1", () => {
    expect(sessionEventsUrl("s1")).toBe("/api/v1/sessions/s1/events");
    // NOT the old progress.jsonl SSE (/sse/project/...).
    expect(sessionEventsUrl("s1")).not.toContain("/sse/");
  });
  it("encodes the sid", () => {
    expect(sessionEventsUrl("s/odd")).toBe("/api/v1/sessions/s%2Fodd/events");
  });

  // v0.8.22 P1 (review §3.1-3) — the reconnect watermark query fallback.
  it("omits last_event_id when absent, zero, or negative (a fresh connect)", () => {
    expect(sessionEventsUrl("s1")).toBe("/api/v1/sessions/s1/events");
    expect(sessionEventsUrl("s1", 0)).toBe("/api/v1/sessions/s1/events");
    expect(sessionEventsUrl("s1", -1)).toBe("/api/v1/sessions/s1/events");
  });
  it("appends last_event_id when it names a real watermark", () => {
    expect(sessionEventsUrl("s1", 42)).toBe("/api/v1/sessions/s1/events?last_event_id=42");
  });
});

describe("shouldAcceptEventSeq (review §3.1-3 reconnect dedup)", () => {
  it("accepts and advances on a strictly-increasing seq", () => {
    const first = shouldAcceptEventSeq("1", 0);
    expect(first).toEqual({ accept: true, nextHighest: 1 });
    const second = shouldAcceptEventSeq("2", first.nextHighest);
    expect(second).toEqual({ accept: true, nextHighest: 2 });
  });

  it("rejects a replayed/reseeded frame at or below the watermark", () => {
    expect(shouldAcceptEventSeq("2", 5)).toEqual({ accept: false, nextHighest: 5 });
    expect(shouldAcceptEventSeq("5", 5)).toEqual({ accept: false, nextHighest: 5 });
  });

  it("passes through a frame with no seq at all (never dedups it)", () => {
    expect(shouldAcceptEventSeq(undefined, 5)).toEqual({ accept: true, nextHighest: 5 });
    expect(shouldAcceptEventSeq("", 5)).toEqual({ accept: true, nextHighest: 5 });
  });

  it("passes through a non-numeric id defensively (never throws)", () => {
    expect(shouldAcceptEventSeq("not-a-number", 5)).toEqual({ accept: true, nextHighest: 5 });
  });
});

describe("parseSessionEvent (W2 payload shape)", () => {
  it("preserves scheduled queue invalidations for a list re-fetch", () => {
    const ev = parseSessionEvent(
      JSON.stringify({ id: "scheduled-changed-s1-d1", sid: "s1", kind: "scheduled_changed", content: "" }),
    );
    expect(ev).toMatchObject({ sid: "s1", kind: "scheduled_changed", content: "" });
  });

  it("preserves session lifecycle frames instead of degrading them to answers", () => {
    const ev = parseSessionEvent(
      JSON.stringify({ kind: "session_lifecycle", content: "session evicted: s4" }),
    );
    expect(ev).toMatchObject({ kind: "session_lifecycle", content: "session evicted: s4" });
  });

  it("parses an answer payload", () => {
    const ev = parseSessionEvent(
      JSON.stringify({ id: "e1", sid: "s1", kind: "answer", content: "hello" }),
    );
    expect(ev).toMatchObject({ id: "e1", sid: "s1", kind: "answer", content: "hello" });
    expect(ev!.done).toBeUndefined();
    expect(ev!.options).toBeUndefined();
  });

  it("parses a finalizing progress payload (done:true)", () => {
    const ev = parseSessionEvent(JSON.stringify({ kind: "progress", content: "x", done: true }));
    expect(ev!.kind).toBe("progress");
    expect(ev!.done).toBe(true);
  });

  it("carries approval options ({label,id}) + token (R-H1)", () => {
    const ev = parseSessionEvent(
      JSON.stringify({
        sid: "s2",
        kind: "answer",
        content: "session s2 wants to run rm -rf /",
        options: [
          { label: "✅ Approve", id: "allow" },
          { label: "⛔ Deny", id: "deny" },
        ],
        token: "pdeadbeef",
      }),
    );
    expect(ev!.options).toEqual([
      { label: "✅ Approve", id: "allow" },
      { label: "⛔ Deny", id: "deny" },
    ]);
    expect(ev!.token).toBe("pdeadbeef");
    expect(ev!.sid).toBe("s2");
  });

  it("drops malformed option entries but keeps well-formed ones (R-H1)", () => {
    const ev = parseSessionEvent(
      JSON.stringify({
        kind: "answer",
        content: "x",
        // a non-object and an object missing label are both dropped; the id
        // defaults to "" when absent.
        options: ["bare-string", { label: "Approve" }, { foo: "bar" }],
        token: "ptok",
      }),
    );
    expect(ev!.options).toEqual([{ label: "Approve", id: "" }]);
    expect(ev!.token).toBe("ptok");
  });

  it("parses an activity frame with the nested structured payload (v0.8.19)", () => {
    const ev = parseSessionEvent(
      JSON.stringify({
        id: "a1",
        sid: "s3",
        kind: "activity",
        content: "Bash(ls -la)",
        activity: {
          kind: "tool_call",
          name: "Bash",
          summary: "Bash(ls -la)",
          status: "started",
          item_id: "t1",
        },
      }),
    );
    expect(ev!.kind).toBe("activity");
    expect(ev!.content).toBe("Bash(ls -la)");
    expect(ev!.activity).toEqual({
      kind: "tool_call",
      name: "Bash",
      summary: "Bash(ls -la)",
      status: "started",
      item_id: "t1",
    });
  });

  it("tolerates a malformed activity object (drops it, keeps the frame)", () => {
    // A non-object activity is ignored; the bare "activity" frame still parses
    // (its `content` line renders).
    const ev = parseSessionEvent(
      JSON.stringify({ kind: "activity", content: "thinking…", activity: "garbage" }),
    );
    expect(ev!.kind).toBe("activity");
    expect(ev!.content).toBe("thinking…");
    expect(ev!.activity).toBeUndefined();
    // A partial activity object fills missing string fields with "".
    const partial = parseSessionEvent(
      JSON.stringify({ kind: "activity", content: "x", activity: { kind: "thinking" } }),
    );
    expect(partial!.activity).toEqual({
      kind: "thinking",
      name: "",
      summary: "",
      status: "",
      item_id: "",
    });
  });

  it("defaults an unknown kind to answer and missing content to ''", () => {
    const ev = parseSessionEvent(JSON.stringify({ foo: "bar" }));
    expect(ev).toMatchObject({ kind: "answer", content: "" });
  });

  it("returns null for garbage / non-object payloads", () => {
    expect(parseSessionEvent("not-json")).toBeNull();
    expect(parseSessionEvent("42")).toBeNull();
    expect(parseSessionEvent("null")).toBeNull();
  });
});

describe("appendSessionEvent ring buffer", () => {
  const ev = (i: number): SessionEvent => ({ kind: "answer", content: String(i) });

  it("appends, newest last, returning a new array", () => {
    const a: SessionEvent[] = [];
    const b = appendSessionEvent(a, ev(1));
    expect(b).not.toBe(a);
    expect(b[b.length - 1].content).toBe("1");
  });

  it("caps at SESSION_RING_CAP (oldest drop)", () => {
    let events: SessionEvent[] = [];
    for (let i = 0; i < SESSION_RING_CAP + 10; i++) events = appendSessionEvent(events, ev(i));
    expect(events).toHaveLength(SESSION_RING_CAP);
    expect(events[events.length - 1].content).toBe(String(SESSION_RING_CAP + 9));
    expect(events[0].content).toBe(String(10));
  });
});
