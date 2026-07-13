// v0.9.0 W4 (F4) — pure, DOM-free layout for the team graph: one swim-lane
// per host, depth-0 nodes at the left of their lane, children laid out to
// the right grouped near their parent. Simple layered layout (≤50 nodes
// doesn't need a force-directed graph, per the tech-design) — no React, no
// window, so it's unit-testable in node env and shared by both the SVG
// renderer and any future export.

import type { AgentEdge, AgentNode } from "./agentsApi";

/** Horizontal spacing per delegation-depth level (px). */
export const X_STEP = 220;
/** Vertical spacing between rows within one lane (px). */
export const Y_STEP = 88;
/** Vertical spacing between lanes (px) — generous enough that a lane with a
 *  handful of rows never visually collides with the next host's band. */
export const LANE_HEIGHT = 320;
/** Left margin so a depth-0 node isn't flush against the canvas edge. */
export const X_MARGIN = 40;
/** Top margin within a lane. */
export const Y_MARGIN = 50;

export interface LayoutNode extends AgentNode {
  x: number;
  y: number;
  lane: number;
}

export interface LayoutEdge extends AgentEdge {
  x1: number;
  y1: number;
  x2: number;
  y2: number;
}

export interface AgentsLayout {
  nodes: LayoutNode[];
  edges: LayoutEdge[];
  hosts: string[];
  /** Total canvas height (the last lane's bottom edge). */
  height: number;
  /** Total canvas width (the deepest node's right edge). */
  width: number;
}

/** Stable sort key within a lane: depth first (roots left), then grouped by
 *  parent (siblings stay adjacent), then sid for determinism. */
function rowSortKey(n: AgentNode): string {
  const depth = String(n.depth).padStart(6, "0");
  const parent = n.parent_sid ?? "";
  return `${depth} ${parent} ${n.sid}`;
}

/** Compute node/edge screen positions from a graph snapshot. Pure — the
 *  caller (AgentsView) owns the SVG rendering; this only produces numbers.
 *  `hosts` should be the graph's own `hosts[]` (server already sorts
 *  `"local"` first) so lane order is stable and matches the legend. */
export function computeAgentsLayout(
  nodes: AgentNode[],
  edges: AgentEdge[],
  hosts: string[],
): AgentsLayout {
  const laneIndex = new Map<string, number>();
  hosts.forEach((h, i) => laneIndex.set(h, i));
  // A node naming a host absent from `hosts[]` (shouldn't happen — the graph
  // endpoint derives `hosts` FROM the nodes — but stay defensive) gets its
  // own trailing lane rather than being silently dropped.
  let nextLane = hosts.length;
  const laneOf = (host: string): number => {
    const known = laneIndex.get(host);
    if (known !== undefined) return known;
    const assigned = nextLane;
    laneIndex.set(host, assigned);
    nextLane += 1;
    return assigned;
  };

  const byLane = new Map<number, AgentNode[]>();
  for (const n of nodes) {
    const lane = laneOf(n.host);
    const list = byLane.get(lane);
    if (list) list.push(n);
    else byLane.set(lane, [n]);
  }

  const positioned = new Map<string, LayoutNode>();
  for (const [lane, laneNodes] of byLane) {
    const sorted = [...laneNodes].sort((a, b) => (rowSortKey(a) < rowSortKey(b) ? -1 : 1));
    sorted.forEach((n, row) => {
      positioned.set(n.sid, {
        ...n,
        lane,
        x: X_MARGIN + n.depth * X_STEP,
        y: lane * LANE_HEIGHT + Y_MARGIN + row * Y_STEP,
      });
    });
  }

  const laidOutNodes = nodes.map((n) => positioned.get(n.sid)!).filter(Boolean);
  const laidOutEdges: LayoutEdge[] = [];
  for (const e of edges) {
    const parent = positioned.get(e.parent);
    const child = positioned.get(e.child);
    if (!parent || !child) continue; // defensive: an edge naming an unknown sid
    laidOutEdges.push({ ...e, x1: parent.x, y1: parent.y, x2: child.x, y2: child.y });
  }

  const laneCount = Math.max(hosts.length, nextLane, 1);
  const width =
    laidOutNodes.length === 0
      ? X_MARGIN * 2
      : Math.max(...laidOutNodes.map((n) => n.x)) + X_STEP;
  return {
    nodes: laidOutNodes,
    edges: laidOutEdges,
    hosts,
    height: laneCount * LANE_HEIGHT,
    width,
  };
}

/** A cubic bezier `d` attribute for a parent→child edge — a gentle S-curve
 *  when nodes are in different lanes, a straight-ish curve within one lane. */
export function edgePath(e: Pick<LayoutEdge, "x1" | "y1" | "x2" | "y2">): string {
  const midX = (e.x1 + e.x2) / 2;
  return `M ${e.x1} ${e.y1} C ${midX} ${e.y1}, ${midX} ${e.y2}, ${e.x2} ${e.y2}`;
}
