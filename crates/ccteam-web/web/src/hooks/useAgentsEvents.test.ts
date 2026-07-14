// v0.9.0 W4 (F4) — useAgentsEvents wiring tests, mirroring
// `useSessionEvents.test.ts`'s discipline: test the pure pieces (URL,
// parser, ring append) in node env; the hook itself needs a real
// EventSource + React effects.

import { describe, expect, it } from "vitest";

import {
  agentsEventsUrl,
  appendAgentsEvent,
  parseAgentsEvent,
  AGENTS_RING_CAP,
  type AgentsEvent,
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
