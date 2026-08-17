import { describe, expect, it } from "vitest";
import {
  filterCollapsedTreeRows,
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

  it("a collapsed project keeps its header counts but renders zero rows", () => {
    const demoNodes = [node("s1", null), node("s2", "s1")];
    const otherProject = { ...node("s3", null), slug: "alpha" };
    const groups = groupDelegationTrees(
      [...demoNodes, otherProject],
      new Set(),
      new Set(["demo"]),
    );
    expect(groups.map((group) => group.slug)).toEqual(["alpha", "demo"]);
    // The collapsed project's rows are gone; slug + counts stay for the header.
    expect(groups[1]).toMatchObject({ slug: "demo", liveCount: 2, totalCount: 2, rows: [] });
    // Other projects are untouched.
    expect(groups[0]!.rows.map((row) => row.node.sid)).toEqual(["s3"]);
  });

  it("project collapse composes with per-sid collapse on the surviving projects", () => {
    const groups = groupDelegationTrees(
      [node("s1", null), node("s2", "s1"), { ...node("s3", null), slug: "alpha" }],
      new Set(["s1"]),
      new Set(["alpha"]),
    );
    expect(groups[0]!.rows).toEqual([]); // alpha: collapsed project
    expect(groups[1]!.rows.map((row) => row.node.sid)).toEqual(["s1"]); // demo: s2 hidden by sid collapse
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
