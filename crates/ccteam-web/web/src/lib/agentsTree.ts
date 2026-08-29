// 团队 topology — pure delegation-tree grouping (unit-tested without React).
//
// The topology tree is the team view's single canvas: per project, one row
// per session in DFS order (each child directly under its parent), so the
// degenerate no-delegation case reads as a clean flat list and a delegation
// fan-out reads as an indented tree — the process-tree shape operators
// already know.

import type { AgentNode } from "./agentsApi";

/** Topology-tree row: the node plus its rendered indent (an orphan whose
 *  parent is invisible renders as a root, indent 0). `hasChildren` keeps
 *  collapse rendering pure: React only owns the set of collapsed sids. */
export interface DelegationTreeRow {
  node: AgentNode;
  indent: number;
  hasChildren: boolean;
}

/** One project's independently rendered delegation forest. */
export interface ProjectDelegationTree {
  slug: string;
  liveCount: number;
  totalCount: number;
  rows: DelegationTreeRow[];
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
 *  exactly once. A project whose slug is in `collapsedProjects` keeps its
 *  header data (slug + counts) but renders zero session rows — the header
 *  itself stays visible so the project can be re-expanded. */
export function groupDelegationTrees(
  nodes: AgentNode[],
  collapsed: ReadonlySet<string> = new Set<string>(),
  collapsedProjects: ReadonlySet<string> = new Set<string>(),
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
        liveCount: projectNodes.filter((node) => node.residency === "resident").length,
        totalCount: projectNodes.length,
        rows: collapsedProjects.has(slug) ? [] : filterCollapsedTreeRows(rows, collapsed),
      };
    });
}
