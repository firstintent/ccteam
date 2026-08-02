// v0.9.0 W4 (F4) — useAgentsEvents wiring tests, mirroring
// `useSessionEvents.test.ts`'s discipline: test the pure pieces (URL,
// parser, ring append) in node env; the hook itself needs a real
// EventSource + React effects.

import { afterEach, describe, expect, it } from "vitest";

import {
  agentsEventsUrl,
  agentsStreamDebugState,
  appendAgentsEvent,
  parseAgentsEvent,
  resetAgentsStreamForTests,
  subscribeAgentsStream,
  AGENTS_RING_CAP,
  type AgentsEvent,
  type AgentsEventSourceLike,
  type AgentsStreamEnvironment,
  type AgentsStreamStatus,
} from "./useAgentsEvents";

describe("agentsEventsUrl", () => {
  it("targets the global team-view SSE endpoint under /api/v1", () => {
    expect(agentsEventsUrl()).toBe("/api/v1/agents/events");
  });

  it("omits last_event_id when absent, zero, or negative", () => {
    expect(agentsEventsUrl(0)).toBe("/api/v1/agents/events");
    expect(agentsEventsUrl(-1)).toBe("/api/v1/agents/events");
  });

  it("appends last_event_id when it names a real watermark", () => {
    expect(agentsEventsUrl(7)).toBe("/api/v1/agents/events?last_event_id=7");
  });
});

describe("parseAgentsEvent", () => {
  it("parses an ordinary answer/progress/activity frame like parseSessionEvent, now carrying slug", () => {
    const ev = parseAgentsEvent(
      JSON.stringify({ id: "e1", sid: "s1", slug: "demo", kind: "answer", content: "hi" }),
    );
    expect(ev).toMatchObject({ id: "e1", sid: "s1", slug: "demo", kind: "answer", content: "hi" });
  });

  it("parses a delegation frame with relation/parent_sid/child_sid/title/reason", () => {
    const ev = parseAgentsEvent(
      JSON.stringify({
        slug: "demo",
        kind: "delegation",
        content: "delegation dispatched: s0 -> s1",
        relation: "dispatched",
        parent_sid: "s0",
        child_sid: "s1",
        title: "research",
      }),
    );
    expect(ev).toMatchObject({
      kind: "delegation",
      slug: "demo",
      relation: "dispatched",
      parent_sid: "s0",
      child_sid: "s1",
      title: "research",
    });
    expect(ev!.reason).toBeUndefined();
  });

  it("parses a denied delegation frame's reason", () => {
    const ev = parseAgentsEvent(
      JSON.stringify({
        kind: "delegation",
        content: "",
        relation: "denied",
        parent_sid: "s0",
        child_sid: "",
        reason: "depth",
      }),
    );
    expect(ev!.relation).toBe("denied");
    expect(ev!.reason).toBe("depth");
  });

  it("parses a capacity eviction lifecycle frame", () => {
    const ev = parseAgentsEvent(
      JSON.stringify({
        kind: "session_lifecycle",
        sid: "s4",
        slug: "demo",
        content: "session evicted: s4",
        state: "evicted",
        reason: "capacity",
      }),
    );
    expect(ev).toMatchObject({ kind: "session_lifecycle", sid: "s4", slug: "demo" });
  });

  it("defaults an unrecognized kind to answer and missing content to ''", () => {
    const ev = parseAgentsEvent(JSON.stringify({ foo: "bar" }));
    expect(ev).toMatchObject({ kind: "answer", content: "" });
  });

  it("returns null for garbage / non-object payloads", () => {
    expect(parseAgentsEvent("not-json")).toBeNull();
    expect(parseAgentsEvent("42")).toBeNull();
    expect(parseAgentsEvent("null")).toBeNull();
  });
});

describe("appendAgentsEvent ring buffer", () => {
  const ev = (i: number): AgentsEvent => ({ kind: "answer", content: String(i) });

  it("appends, newest last, returning a new array", () => {
    const a: AgentsEvent[] = [];
    const b = appendAgentsEvent(a, ev(1));
    expect(b).not.toBe(a);
    expect(b[b.length - 1]!.content).toBe("1");
  });

  it("caps at AGENTS_RING_CAP (oldest drop)", () => {
    let events: AgentsEvent[] = [];
    for (let i = 0; i < AGENTS_RING_CAP + 10; i++) events = appendAgentsEvent(events, ev(i));
    expect(events).toHaveLength(AGENTS_RING_CAP);
    expect(events[events.length - 1]!.content).toBe(String(AGENTS_RING_CAP + 9));
    expect(events[0]!.content).toBe(String(10));
  });
});

// 2026-08-02 — ONE connection for N consumers.
//
// `/api/v1/agents/events` is a GLOBAL feed, but the hook used to open its own
// stream per call site: `AgentsView` + `ChatConsole` mounted together held two
// sockets and downloaded identical frames twice. A browser allows only ~6
// HTTP/1.1 connections per origin and an SSE stream holds its socket for the
// whole session, so every duplicate stream brings the whole page closer to
// socket exhaustion (the 2026-08-02 "everything pending" outage). These tests
// pin the sharing so a future consumer cannot silently re-introduce a second
// stream.

interface FakeSource extends AgentsEventSourceLike {
  emit(type: string, event: Event): void;
  closed: boolean;
  url: string;
}

function fakeStreamEnv(): AgentsStreamEnvironment & {
  sources: FakeSource[];
  runTimers(): void;
} {
  const sources: FakeSource[] = [];
  let queue: Array<{ id: number; cb: () => void }> = [];
  let nextId = 1;
  return {
    createEventSource(url) {
      const listeners = new Map<string, EventListener[]>();
      const source: FakeSource = {
        url,
        closed: false,
        addEventListener(type, listener) {
          const bag = listeners.get(type) ?? [];
          bag.push(listener);
          listeners.set(type, bag);
        },
        close() {
          source.closed = true;
        },
        emit(type, event) {
          for (const listener of listeners.get(type) ?? []) listener(event);
        },
      };
      sources.push(source);
      return source;
    },
    setTimer(cb) {
      const id = nextId++;
      queue.push({ id, cb });
      return id as unknown as ReturnType<typeof setTimeout>;
    },
    clearTimer(timer) {
      queue = queue.filter((entry) => entry.id !== (timer as unknown as number));
    },
    sources,
    runTimers() {
      const due = queue;
      queue = [];
      for (const entry of due) entry.cb();
    },
  };
}

const frame = (seq: number, payload: Record<string, unknown>): MessageEvent =>
  new MessageEvent("progress", { data: JSON.stringify(payload), lastEventId: String(seq) });

describe("shared agents stream broker", () => {
  afterEach(() => resetAgentsStreamForTests());

  it("opens ONE connection for many subscribers", () => {
    const env = fakeStreamEnv();
    const a: AgentsEvent[] = [];
    const b: AgentsEvent[] = [];
    const offA = subscribeAgentsStream(
      { onFrame: (e) => a.push(e), onStatus: () => {} },
      env,
    );
    const offB = subscribeAgentsStream(
      { onFrame: (e) => b.push(e), onStatus: () => {} },
      env,
    );

    expect(env.sources).toHaveLength(1);
    expect(agentsStreamDebugState()).toEqual({ subscribers: 2, open: true });

    // Both consumers see the same frame from that single connection.
    env.sources[0]!.emit("progress", frame(1, { kind: "progress", content: "hi" }));
    expect(a).toHaveLength(1);
    expect(b).toHaveLength(1);

    offA();
    offB();
  });

  it("closes the connection only after the LAST subscriber leaves", () => {
    const env = fakeStreamEnv();
    const offA = subscribeAgentsStream({ onFrame: () => {}, onStatus: () => {} }, env);
    const offB = subscribeAgentsStream({ onFrame: () => {}, onStatus: () => {} }, env);

    offA();
    expect(env.sources[0]!.closed).toBe(false);
    expect(agentsStreamDebugState().open).toBe(true);

    offB();
    expect(env.sources[0]!.closed).toBe(true);
    expect(agentsStreamDebugState()).toEqual({ subscribers: 0, open: false });
  });

  it("shares ONE reconnect watermark, so a resubscribe resumes instead of replaying", () => {
    const env = fakeStreamEnv();
    const off = subscribeAgentsStream({ onFrame: () => {}, onStatus: () => {} }, env);
    env.sources[0]!.emit("progress", frame(42, { kind: "progress", content: "x" }));
    off();

    subscribeAgentsStream({ onFrame: () => {}, onStatus: () => {} }, env);
    expect(env.sources[1]!.url).toBe(agentsEventsUrl(42));
  });

  it("hands a late joiner the current health immediately", () => {
    const env = fakeStreamEnv();
    subscribeAgentsStream({ onFrame: () => {}, onStatus: () => {} }, env);
    env.sources[0]!.emit("open", new Event("open"));

    const seen: AgentsStreamStatus[] = [];
    subscribeAgentsStream({ onFrame: () => {}, onStatus: (s) => seen.push(s) }, env);
    expect(seen[0]).toMatchObject({ connected: true });
  });

  it("reconnects once for the whole fleet of subscribers", () => {
    const env = fakeStreamEnv();
    subscribeAgentsStream({ onFrame: () => {}, onStatus: () => {} }, env);
    subscribeAgentsStream({ onFrame: () => {}, onStatus: () => {} }, env);

    env.sources[0]!.emit("error", new Event("error"));
    env.runTimers();
    // One replacement stream, not one per subscriber.
    expect(env.sources).toHaveLength(2);
  });

  it("ignores frames from a stream that was already replaced", () => {
    const env = fakeStreamEnv();
    const seen: AgentsEvent[] = [];
    subscribeAgentsStream({ onFrame: (e) => seen.push(e), onStatus: () => {} }, env);
    const stale = env.sources[0]!;
    stale.emit("error", new Event("error"));
    env.runTimers();

    stale.emit("progress", frame(9, { kind: "progress", content: "stale" }));
    expect(seen).toHaveLength(0);
    env.sources[1]!.emit("progress", frame(9, { kind: "progress", content: "fresh" }));
    expect(seen).toHaveLength(1);
    expect(seen[0]!.content).toBe("fresh");
  });
});
