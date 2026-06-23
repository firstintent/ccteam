// v0.8.7 W4 (DD.1) — useSessionEvents wiring tests.
//
// The hook itself needs EventSource + React (DOM); to stay node-env-
// friendly (no jsdom, the FIX-2 chatDefaults pattern) we extract and test
// the pure pieces: the SSE URL, the W2-shape parser, and the ring-buffer
// append. These are what the hook is built from.

import { describe, expect, it } from "vitest";

import {
  appendSessionEvent,
  parseSessionEvent,
  sessionEventsUrl,
  SESSION_RING_CAP,
  type SessionEvent,
} from "./useSessionEvents";

describe("sessionEventsUrl", () => {
  it("targets the W2 per-sid SSE endpoint under /api/v1", () => {
    expect(sessionEventsUrl("s1")).toBe("/api/v1/sessions/s1/events");
    // NOT the old progress.jsonl SSE (/sse/project/...).
    expect(sessionEventsUrl("s1")).not.toContain("/sse/");
  });
  it("encodes the sid", () => {
    expect(sessionEventsUrl("s/odd")).toBe("/api/v1/sessions/s%2Fodd/events");
  });
});

describe("parseSessionEvent (W2 payload shape)", () => {
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
