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

/** Topology-tree row metadata. `hasChildren` keeps collapse rendering pure:
 *  React only owns the set of collapsed sids. */
export interface DelegationTreeRow extends RosterRow {
  hasChildren: boolean;
}

/** One project's independently rendered delegation forest. */
export interface ProjectDelegationTree {
  slug: string;
  liveCount: number;
  totalCount: number;
  rows: DelegationTreeRow[];
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

/** Remove descendants of collapsed rows from an already-DFS-ordered forest.
 *  Collapsed rows themselves remain visible. */
export function filterCollapsedTreeRows(
  rows: DelegationTreeRow[],
  collapsed: ReadonlySet<string>,
): DelegationTreeRow[] {
  const visible: DelegationTreeRow[] = [];
  let hiddenBelowIndent: number | null = null;
  for (const row of rows) {
    if (hiddenBelowIndent !== null) {
      if (row.indent > hiddenBelowIndent) continue;
      hiddenBelowIndent = null;
    }
    visible.push(row);
    if (row.hasChildren && collapsed.has(row.node.sid)) hiddenBelowIndent = row.indent;
  }
  return visible;
}

/** Build the topology view's project-grouped delegation forests. Projects
 *  sort by slug; roots sort by most-recent activity, while children retain a
 *  stable natural-sid order. Missing parents and corrupt cycles still render
 *  exactly once. */
export function groupDelegationTrees(
  nodes: AgentNode[],
  collapsed: ReadonlySet<string> = new Set<string>(),
): ProjectDelegationTree[] {
  const byProject = new Map<string, AgentNode[]>();
  for (const node of nodes) {
    const project = byProject.get(node.slug) ?? [];
    project.push(node);
    byProject.set(node.slug, project);
  }

  const sidAsc = (a: AgentNode, b: AgentNode) =>
    a.sid.localeCompare(b.sid, "en", { numeric: true });
  const activeDesc = (a: AgentNode, b: AgentNode) => {
    const delta = (Date.parse(b.last_active) || 0) - (Date.parse(a.last_active) || 0);
    return delta || sidAsc(a, b);
  };

  return [...byProject.entries()]
    .sort(([a], [b]) => a.localeCompare(b, "en", { numeric: true }))
    .map(([slug, projectNodes]) => {
      const bySid = new Map(projectNodes.map((node) => [node.sid, node]));
      const children = new Map<string, AgentNode[]>();
      const roots: AgentNode[] = [];
      for (const node of projectNodes) {
        const parent = node.parent_sid && bySid.has(node.parent_sid) ? node.parent_sid : null;
        if (parent && parent !== node.sid) {
          const siblings = children.get(parent) ?? [];
          siblings.push(node);
          children.set(parent, siblings);
        } else {
          roots.push(node);
        }
      }
      roots.sort(activeDesc);
      for (const siblings of children.values()) siblings.sort(sidAsc);

      const rows: DelegationTreeRow[] = [];
      const visited = new Set<string>();
      const walk = (node: AgentNode, indent: number) => {
        if (visited.has(node.sid)) return;
        visited.add(node.sid);
        rows.push({ node, indent, hasChildren: (children.get(node.sid)?.length ?? 0) > 0 });
        for (const child of children.get(node.sid) ?? []) walk(child, indent + 1);
      };
      for (const root of roots) walk(root, 0);
      for (const node of [...projectNodes].sort(activeDesc)) {
        if (!visited.has(node.sid)) walk(node, 0);
      }

      return {
        slug,
        liveCount: projectNodes.filter((node) => node.status === "live").length,
        totalCount: projectNodes.length,
        rows: filterCollapsedTreeRows(rows, collapsed),
      };
    });
}
