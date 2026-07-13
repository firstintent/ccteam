// v0.9.0 W4 (F4) — pure reducers driving the team graph's LIVE state from the
// global SSE stream (`useAgentsEvents`). No React, no DOM — unit-testable in
// node env, mirroring `lib/agentsLayout.ts`'s discipline.

import type { AgentEdge } from "./agentsApi";
import type { AgentsEvent } from "../hooks/useAgentsEvents";

/** Fold one delegation frame into the edge list: `dispatched` marks (or
 *  creates) the parent→child edge active; `completed`/`notified`/`stopped`/
 *  `denied` mark it quiet again. `spawned`/`collected` don't change activity
 *  (a fresh child isn't "in flight" until dispatched). Pure — returns the
 *  SAME array reference when nothing changes (cheap no-op renders). */
export function applyDelegationEvent(edges: AgentEdge[], ev: AgentsEvent): AgentEdge[] {
  if (ev.kind !== "delegation" || !ev.parent_sid || !ev.child_sid) return edges;
  const parent = ev.parent_sid;
  const child = ev.child_sid;
  if (ev.relation === "dispatched") {
    const idx = edges.findIndex((e) => e.parent === parent && e.child === child);
    if (idx === -1) {
      return [...edges, { parent, child, title: ev.title, active: true }];
    }
    if (edges[idx]!.active && (ev.title ?? edges[idx]!.title) === edges[idx]!.title) return edges;
    return edges.map((e, i) => (i === idx ? { ...e, active: true, title: ev.title ?? e.title } : e));
  }
  if (
    ev.relation === "completed" ||
    ev.relation === "notified" ||
    ev.relation === "stopped" ||
    ev.relation === "denied"
  ) {
    let changed = false;
    const next = edges.map((e) => {
      if (e.parent === parent && e.child === child && e.active) {
        changed = true;
        return { ...e, active: false };
      }
      return e;
    });
    return changed ? next : edges;
  }
  return edges;
}

/** Fold a whole batch of events (the hook's ring) into one edge list —
 *  convenience for a render pass that only has the raw event log. */
export function reduceDelegationEvents(edges: AgentEdge[], events: AgentsEvent[]): AgentEdge[] {
  return events.reduce(applyDelegationEvent, edges);
}

/** A user-facing toast for a `denied` delegation event, or `null` for
 *  anything else. */
export function delegationToast(ev: AgentsEvent): string | null {
  if (ev.kind !== "delegation" || ev.relation !== "denied") return null;
  const who = ev.child_sid ? `${ev.parent_sid} → ${ev.child_sid}` : `${ev.parent_sid}`;
  return ev.reason ? `delegation denied (${ev.reason}): ${who}` : `delegation denied: ${who}`;
}

/** An {@link AgentsEvent} stamped with its client-side receipt time (SSE
 *  frames carry no wire timestamp) — what `AgentsView` accumulates so the
 *  "in-turn" pulse can be computed from real elapsed time rather than event
 *  order alone. */
export interface TimestampedAgentsEvent extends AgentsEvent {
  receivedAt: number;
}

/** Which sids currently look "in-turn" (pulse the node ring): the LAST
 *  `progress`/`activity` frame for that sid landed within `windowMs` (default
 *  15s, per tech-design) of `nowMs`, and no finalizing `progress{done:true}`
 *  or `answer` has landed for it since. Processes events in order so a later
 *  terminal frame always wins over an earlier non-terminal one. Pure +
 *  DOM-free; `nowMs` is injectable for deterministic tests. */
export function sidsActiveWithin(
  events: TimestampedAgentsEvent[],
  windowMs: number = 15_000,
  nowMs: number = Date.now(),
): Set<string> {
  const lastNonTerminalAt = new Map<string, number>();
  for (const ev of events) {
    if (!ev.sid) continue;
    if (ev.kind === "progress" && ev.done) {
      lastNonTerminalAt.delete(ev.sid); // a finalizing progress ends the pulse
      continue;
    }
    if (ev.kind === "answer") {
      lastNonTerminalAt.delete(ev.sid);
      continue;
    }
    if (ev.kind === "progress" || ev.kind === "activity") {
      lastNonTerminalAt.set(ev.sid, ev.receivedAt);
    }
  }
  const out = new Set<string>();
  for (const [sid, at] of lastNonTerminalAt) {
    if (nowMs - at < windowMs) out.add(sid);
  }
  return out;
}
