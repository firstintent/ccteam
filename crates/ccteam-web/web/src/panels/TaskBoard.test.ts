// V0.5.0 F96 — TaskBoard pure-helper tests.
//
// The component itself doesn't render under vitest (no testing-library
// available — see crates/ccteam-web/web/src/pages/WorkflowView.test.ts
// for the same rationale). We exercise the bucketing + color-map
// helpers, which is where any future regression would land.

import { describe, expect, it } from "vitest";
import {
  bucketTasks,
  buildMemberColorMap,
} from "./TaskBoard";
import type { TeamConfig, TeamMember, TeamTask } from "../lib/teamsApi";

function task(id: string, status: string, extras: Partial<TeamTask> = {}): TeamTask {
  return {
    id,
    title: `Task ${id}`,
    status,
    assignee: null,
    description: null,
    dependencies: [],
    created_at: null,
    completed_at: null,
    ...extras,
  };
}

function member(name: string, color: string | null): TeamMember {
  return {
    agent_id: `${name}@team`,
    name,
    agent_type: "general-purpose",
    model: "sonnet",
    color,
    joined_at: null,
    cwd: null,
    prompt: "x",
    subscriptions: [],
    tmux_pane_id: null,
    backend_type: "in-process",
    plan_mode_required: null,
    definition_backed: false,
  };
}

describe("bucketTasks", () => {
  it("groups tasks by canonical status", () => {
    const out = bucketTasks([
      task("1", "pending"),
      task("2", "in_progress"),
      task("3", "completed"),
    ]);
    expect(out.pending.map((t) => t.id)).toEqual(["1"]);
    expect(out.in_progress.map((t) => t.id)).toEqual(["2"]);
    expect(out.completed.map((t) => t.id)).toEqual(["3"]);
  });
  it("folds unknown statuses into Pending", () => {
    const out = bucketTasks([
      task("1", "vapor"),
      task("2", "blocked"),
    ]);
    expect(out.pending.map((t) => t.id)).toEqual(["1", "2"]);
    expect(out.in_progress).toEqual([]);
    expect(out.completed).toEqual([]);
  });
  it("returns three buckets even when input is empty", () => {
    const out = bucketTasks([]);
    expect(Object.keys(out).sort()).toEqual(["completed", "in_progress", "pending"]);
    expect(out.pending).toEqual([]);
  });
});

describe("buildMemberColorMap", () => {
  it("maps member name to color, preserving null", () => {
    const cfg: TeamConfig = {
      name: "t",
      description: null,
      created_at: null,
      lead_agent_id: null,
      lead_session_id: null,
      members: [member("a", "blue"), member("b", null), member("c", "green")],
    };
    const map = buildMemberColorMap(cfg);
    expect(map).toEqual({ a: "blue", b: null, c: "green" });
  });
  it("returns empty object for null config", () => {
    expect(buildMemberColorMap(null)).toEqual({});
  });
});

describe("bucketTasks ordering preserves input order within buckets", () => {
  it("does not re-sort within a status", () => {
    const out = bucketTasks([
      task("z", "pending"),
      task("a", "pending"),
      task("m", "pending"),
    ]);
    expect(out.pending.map((t) => t.id)).toEqual(["z", "a", "m"]);
  });
});

describe("buildMemberColorMap handles teammates with same name across runs", () => {
  it("last-write-wins for duplicate names (defensive — schema should not allow)", () => {
    const cfg: TeamConfig = {
      name: "t",
      description: null,
      created_at: null,
      lead_agent_id: null,
      lead_session_id: null,
      members: [member("a", "blue"), member("a", "red")],
    };
    expect(buildMemberColorMap(cfg).a).toBe("red");
  });
});
