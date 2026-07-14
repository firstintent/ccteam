import { describe, expect, it } from "vitest";
import {
  filterCollapsedTreeRows,
  flattenDelegationTree,
  groupDelegationTrees,
  type DelegationTreeRow,
} from "./agentsTree";
import type { AgentNode } from "./agentsApi";

function node(sid: string, parent: string | null): AgentNode {
  return {
    sid,
    slug: "demo",
    role: "",
    vendor: "claude",
    host: "local",
    status: "live",
    parent_sid: parent,
    depth: 0,
    last_active: "2026-07-13T00:00:00Z",
    turn_count: 0,
  };
}

describe("flattenDelegationTree", () => {
  it("orders DFS: each child directly under its parent, siblings by sid", () => {
    const rows = flattenDelegationTree([
      node("s10", "s1"),
      node("s1", null),
      node("s2", null),
      node("s3", "s1"),
    ]);
    expect(rows.map((r) => r.node.sid)).toEqual(["s1", "s3", "s10", "s2"]);
    expect(rows.map((r) => r.indent)).toEqual([0, 1, 1, 0]);
  });

  it("no delegation degenerates to a clean flat list", () => {
    const rows = flattenDelegationTree([node("s2", null), node("s1", null)]);
    expect(rows.map((r) => r.node.sid)).toEqual(["s1", "s2"]);
    expect(rows.every((r) => r.indent === 0)).toBe(true);
  });

  it("an orphan of an invisible parent renders as a root (never dropped)", () => {
    const rows = flattenDelegationTree([node("s5", "s99")]);
    expect(rows).toHaveLength(1);
    expect(rows[0]!.indent).toBe(0);
  });

  it("a corrupt parent cycle cannot loop or drop nodes", () => {
    const rows = flattenDelegationTree([node("sA", "sB"), node("sB", "sA")]);
    expect(rows).toHaveLength(2);
  });
});

describe("groupDelegationTrees", () => {
  it("groups by slug and orders roots by most recent activity", () => {
    const oldRoot = node("s1", null);
    oldRoot.last_active = "2026-07-13T00:00:00Z";
    const newRoot = node("s2", null);
    newRoot.last_active = "2026-07-13T01:00:00Z";
    const child = node("s3", "s2");
    const otherProject = { ...node("s4", null), slug: "alpha", status: "idle" };

    const groups = groupDelegationTrees([oldRoot, child, otherProject, newRoot]);
    expect(groups.map((group) => group.slug)).toEqual(["alpha", "demo"]);
    expect(groups[0]).toMatchObject({ liveCount: 0, totalCount: 1 });
    expect(groups[1]!.rows.map((row) => row.node.sid)).toEqual(["s2", "s3", "s1"]);
    expect(groups[1]!.rows.map((row) => row.indent)).toEqual([0, 1, 0]);
    expect(groups[1]!.rows[0]!.hasChildren).toBe(true);
  });

  it("hides a collapsed node's full subtree but keeps later roots", () => {
    const groups = groupDelegationTrees(
      [node("s1", null), node("s2", "s1"), node("s3", "s2"), node("s4", null)],
      new Set(["s1"]),
    );
    expect(groups[0]!.rows.map((row) => row.node.sid)).toEqual(["s1", "s4"]);
  });
});

describe("filterCollapsedTreeRows", () => {
  it("does not hide siblings after a collapsed branch", () => {
    const rows: DelegationTreeRow[] = [
      { node: node("s1", null), indent: 0, hasChildren: true },
      { node: node("s2", "s1"), indent: 1, hasChildren: true },
      { node: node("s3", "s2"), indent: 2, hasChildren: false },
      { node: node("s4", "s1"), indent: 1, hasChildren: false },
    ];
    expect(filterCollapsedTreeRows(rows, new Set(["s2"])).map((row) => row.node.sid)).toEqual([
      "s1",
      "s2",
      "s4",
    ]);
  });
});
