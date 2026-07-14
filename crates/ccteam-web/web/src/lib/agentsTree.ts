// 团队 roster — pure delegation-tree flattening (unit-tested without React).
//
// The roster table is the PRIMARY team view: one row per session, DFS order
// (each child directly under its parent), so the degenerate no-delegation
// case reads as a clean flat list and a delegation fan-out reads as an
// indented tree — the process-tree shape operators already know.

import type { AgentNode } from "./agentsApi";

/** One roster row: the node plus its indent level (tree depth as rendered —
 *  an orphan whose parent is invisible renders as a root, indent 0). */
export interface RosterRow {
  node: AgentNode;
  indent: number;
}

/** Flatten nodes into DFS roster order. Roots = `parent_sid` null/unknown
 *  (an orphan of an invisible parent is a root — never dropped). Children
 *  sort by sid for a stable order; a cycle in `parent_sid` (corrupt meta)
 *  cannot loop — every node is visited at most once. */
export function flattenDelegationTree(nodes: AgentNode[]): RosterRow[] {
  const bySid = new Map(nodes.map((n) => [n.sid, n]));
  const children = new Map<string, AgentNode[]>();
  const roots: AgentNode[] = [];
  for (const n of nodes) {
    const parent = n.parent_sid && bySid.has(n.parent_sid) ? n.parent_sid : null;
    if (parent && parent !== n.sid) {
      const list = children.get(parent) ?? [];
      list.push(n);
      children.set(parent, list);
    } else {
      roots.push(n);
    }
  }
  const bySidAsc = (a: AgentNode, b: AgentNode) => a.sid.localeCompare(b.sid, "en", { numeric: true });
  roots.sort(bySidAsc);
  for (const list of children.values()) list.sort(bySidAsc);

  const rows: RosterRow[] = [];
  const visited = new Set<string>();
  const walk = (n: AgentNode, indent: number) => {
    if (visited.has(n.sid)) return; // cycle guard (corrupt parent chain)
    visited.add(n.sid);
    rows.push({ node: n, indent });
    for (const c of children.get(n.sid) ?? []) walk(c, indent + 1);
  };
  for (const r of roots) walk(r, 0);
  // A pure-cycle island (no root at all) still renders — append leftovers.
  for (const n of nodes) if (!visited.has(n.sid)) walk(n, 0);
  return rows;
}
