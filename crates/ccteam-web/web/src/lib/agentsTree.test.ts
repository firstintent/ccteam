import { describe, expect, it } from "vitest";
import { flattenDelegationTree } from "./agentsTree";
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
