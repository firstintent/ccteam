import { describe, expect, it } from "vitest";
import { computeAgentsLayout, edgePath, LANE_HEIGHT, X_STEP } from "./agentsLayout";
import type { AgentEdge, AgentNode } from "./agentsApi";

function node(over: Partial<AgentNode> = {}): AgentNode {
  return {
    sid: "s1",
    slug: "demo",
    role: "worker",
    vendor: "claude",
    host: "local",
    status: "live",
    depth: 0,
    last_active: "2026-01-01T00:00:00Z",
    turn_count: 0,
    ...over,
  };
}

describe("computeAgentsLayout", () => {
  it("assigns depth-based x and lanes per host", () => {
    const nodes = [
      node({ sid: "s0", host: "local", depth: 0 }),
      node({ sid: "s1", host: "local", depth: 1, parent_sid: "s0" }),
      node({ sid: "s2", host: "test-01", depth: 0 }),
    ];
    const edges: AgentEdge[] = [{ parent: "s0", child: "s1", active: false }];
    const layout = computeAgentsLayout(nodes, edges, ["local", "test-01"]);
    const bySid = Object.fromEntries(layout.nodes.map((n) => [n.sid, n]));
    expect(bySid.s0!.lane).toBe(0);
    expect(bySid.s2!.lane).toBe(1);
    // Same lane → depth 0 sits left of depth 1.
    expect(bySid.s1!.x).toBeGreaterThan(bySid.s0!.x);
    expect(bySid.s1!.x - bySid.s0!.x).toBe(X_STEP);
    // Different lanes are vertically separated by at least one LANE_HEIGHT.
    expect(Math.abs(bySid.s2!.y - bySid.s0!.y)).toBeGreaterThanOrEqual(LANE_HEIGHT);
    expect(layout.edges).toHaveLength(1);
    expect(layout.edges[0]!.x1).toBe(bySid.s0!.x);
    expect(layout.edges[0]!.x2).toBe(bySid.s1!.x);
  });

  it("orders siblings by parent then sid within one lane (stable, not by array order)", () => {
    const nodes = [
      node({ sid: "s3", depth: 1, parent_sid: "s0" }),
      node({ sid: "s0", depth: 0 }),
      node({ sid: "s2", depth: 1, parent_sid: "s0" }),
    ];
    const layout = computeAgentsLayout(nodes, [], ["local"]);
    const order = [...layout.nodes].sort((a, b) => a.y - b.y).map((n) => n.sid);
    // Root first (lowest depth), then its two children ordered by sid.
    expect(order).toEqual(["s0", "s2", "s3"]);
  });

  it("drops an edge naming an unknown sid rather than throwing", () => {
    const nodes = [node({ sid: "s0" })];
    const edges: AgentEdge[] = [{ parent: "s0", child: "ghost", active: false }];
    const layout = computeAgentsLayout(nodes, edges, ["local"]);
    expect(layout.edges).toHaveLength(0);
  });

  it("gives a node on an unlisted host its own trailing lane instead of dropping it", () => {
    const nodes = [node({ sid: "s0", host: "mystery" })];
    const layout = computeAgentsLayout(nodes, [], ["local"]);
    expect(layout.nodes).toHaveLength(1);
    expect(layout.nodes[0]!.lane).toBe(1); // hosts.length (1) is the next free lane
  });

  it("empty graph has zero nodes/edges and a non-crashing size", () => {
    const layout = computeAgentsLayout([], [], []);
    expect(layout.nodes).toHaveLength(0);
    expect(layout.edges).toHaveLength(0);
    expect(layout.width).toBeGreaterThan(0);
  });
});

describe("edgePath", () => {
  it("produces a cubic bezier `d` string through the given points", () => {
    const d = edgePath({ x1: 0, y1: 0, x2: 100, y2: 50 });
    expect(d.startsWith("M 0 0 C")).toBe(true);
    expect(d).toContain("100 50");
  });
});
