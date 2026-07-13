import { describe, expect, it } from "vitest";
import {
  applyDelegationEvent,
  delegationToast,
  sidsActiveWithin,
  type TimestampedAgentsEvent,
} from "./agentsReducer";
import type { AgentEdge } from "./agentsApi";
import type { AgentsEvent } from "../hooks/useAgentsEvents";

function delegationEvent(over: Partial<AgentsEvent> = {}): AgentsEvent {
  return { kind: "delegation", content: "", parent_sid: "s0", child_sid: "s1", ...over };
}

describe("applyDelegationEvent", () => {
  it("dispatched creates or activates the parent→child edge", () => {
    const created = applyDelegationEvent([], delegationEvent({ relation: "dispatched", title: "t" }));
    expect(created).toEqual([{ parent: "s0", child: "s1", title: "t", active: true }]);

    const existing: AgentEdge[] = [{ parent: "s0", child: "s1", active: false }];
    const activated = applyDelegationEvent(existing, delegationEvent({ relation: "dispatched" }));
    expect(activated[0]!.active).toBe(true);
  });

  it("completed/notified/stopped/denied deactivate the edge", () => {
    const base: AgentEdge[] = [{ parent: "s0", child: "s1", active: true }];
    for (const relation of ["completed", "notified", "stopped", "denied"]) {
      const next = applyDelegationEvent(base, delegationEvent({ relation }));
      expect(next[0]!.active).toBe(false);
    }
  });

  it("is a no-op (same reference) for an event naming no matching edge", () => {
    const base: AgentEdge[] = [{ parent: "sX", child: "sY", active: false }];
    const next = applyDelegationEvent(base, delegationEvent({ relation: "completed" }));
    expect(next).toBe(base);
  });

  it("ignores non-delegation events and delegation events missing sids", () => {
    const base: AgentEdge[] = [];
    expect(applyDelegationEvent(base, { kind: "answer", content: "" })).toBe(base);
    expect(applyDelegationEvent(base, { kind: "delegation", content: "" })).toBe(base);
  });
});

describe("delegationToast", () => {
  it("builds a message for a denied relation, null otherwise", () => {
    expect(delegationToast(delegationEvent({ relation: "denied", reason: "depth" }))).toBe(
      "delegation denied (depth): s0 → s1",
    );
    expect(delegationToast(delegationEvent({ relation: "dispatched" }))).toBeNull();
    expect(delegationToast({ kind: "answer", content: "" })).toBeNull();
  });
});

describe("sidsActiveWithin", () => {
  function ts(over: Partial<TimestampedAgentsEvent>): TimestampedAgentsEvent {
    return { kind: "activity", content: "", receivedAt: 0, ...over };
  }

  it("a recent activity/progress frame marks its sid pulsing", () => {
    const now = 100_000;
    const events = [ts({ sid: "s1", kind: "activity", receivedAt: now - 1000 })];
    expect(sidsActiveWithin(events, 15_000, now)).toEqual(new Set(["s1"]));
  });

  it("a frame older than the window does not pulse", () => {
    const now = 100_000;
    const events = [ts({ sid: "s1", kind: "activity", receivedAt: now - 20_000 })];
    expect(sidsActiveWithin(events, 15_000, now).has("s1")).toBe(false);
  });

  it("a finalizing progress (done:true) ends the pulse even if recent", () => {
    const now = 100_000;
    const events = [
      ts({ sid: "s1", kind: "activity", receivedAt: now - 500 }),
      ts({ sid: "s1", kind: "progress", done: true, receivedAt: now - 200 }),
    ];
    expect(sidsActiveWithin(events, 15_000, now).has("s1")).toBe(false);
  });

  it("an answer ends the pulse", () => {
    const now = 100_000;
    const events = [
      ts({ sid: "s1", kind: "activity", receivedAt: now - 500 }),
      ts({ sid: "s1", kind: "answer", receivedAt: now - 200 }),
    ];
    expect(sidsActiveWithin(events, 15_000, now).has("s1")).toBe(false);
  });
});
