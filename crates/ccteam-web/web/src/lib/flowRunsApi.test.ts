// 团队 → 编排 tab — flow-runs client + pure derivations.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  fetchFlowRuns,
  fetchProjectsFlowRuns,
  flowRunLeaves,
  flowRunsUrl,
  runDurationLabel,
  runStatusBadgeClass,
} from "./flowRunsApi";
import type { AgentNode } from "./agentsApi";

const realFetch = globalThis.fetch;

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

function fixtureNode(over: Partial<AgentNode> = {}): AgentNode {
  return {
    sid: "s0",
    slug: "demo",
    role: "worker",
    vendor: "claude",
    host: "local",
    status: "idle",
    residency: "resident",
    depth: 0,
    last_active: "2026-09-01T10:05:00Z",
    turn_count: 1,
    ...over,
  };
}

describe("flowRunsApi fetch surface", () => {
  beforeEach(() => {
    globalThis.fetch = vi.fn();
  });
  afterEach(() => {
    globalThis.fetch = realFetch;
    vi.restoreAllMocks();
  });

  it("flowRunsUrl targets the project-scoped route (slug encoded)", () => {
    expect(flowRunsUrl("demo")).toBe("/api/v1/projects/demo/flow-runs");
    expect(flowRunsUrl("a/b")).toBe("/api/v1/projects/a%2Fb/flow-runs");
  });

  it("fetchFlowRuns GETs the path with same-origin credentials", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(jsonResponse(200, { runs: [] }));
    const got = await fetchFlowRuns("demo");
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/v1/projects/demo/flow-runs",
      expect.objectContaining({ credentials: "same-origin" }),
    );
    expect(got.runs).toEqual([]);
  });

  it("fetchProjectsFlowRuns is per-project fail-soft: one 403 never blanks the rest", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockImplementation((url) => {
      if (String(url).includes("/alpha/")) {
        return Promise.resolve(jsonResponse(403, { error: "forbidden" }));
      }
      return Promise.resolve(
        jsonResponse(200, {
          runs: [
            {
              run_id: "r1",
              name: "audit-routes",
              description: "d",
              parent_sid: "s1",
              status: "ok",
              agents: 2,
              cost_usd: 0.5,
              started_at: "2026-09-01T10:00:00Z",
              finished_at: "2026-09-01T10:04:00Z",
            },
          ],
        }),
      );
    });
    const got = await fetchProjectsFlowRuns(["alpha", "beta"]);
    expect(got).toEqual([
      // A failed fetch is MARKED, never silently identical to zero runs.
      { slug: "alpha", runs: [], error: true },
      { slug: "beta", runs: [expect.objectContaining({ run_id: "r1" })], truncated: false },
    ]);
  });
});

describe("runStatusBadgeClass", () => {
  it("maps the four verdicts; unknown degrades to the neutral badge", () => {
    expect(runStatusBadgeClass("ok")).toBe("badge ok");
    expect(runStatusBadgeClass("error")).toBe("badge warn");
    expect(runStatusBadgeClass("brake")).toBe("badge warn");
    expect(runStatusBadgeClass("running")).toBe("badge brand");
    expect(runStatusBadgeClass("someday-status")).toBe("badge");
  });
});

describe("flowRunLeaves (delegation-graph derivation)", () => {
  const run = {
    parent_sid: "s1",
    started_at: "2026-09-01T10:00:00Z",
    finished_at: "2026-09-01T10:10:00Z",
  };

  it("collects descendants of the trigger sid, transitively, sid-ordered", () => {
    const nodes = [
      fixtureNode({ sid: "s1", parent_sid: null }),
      fixtureNode({ sid: "s10", parent_sid: "s1" }),
      fixtureNode({ sid: "s2", parent_sid: "s1" }),
      // A hire that itself delegated: grandchild counts too.
      fixtureNode({ sid: "s11", parent_sid: "s2" }),
      // Unrelated root in the same project.
      fixtureNode({ sid: "s3", parent_sid: null }),
    ];
    expect(flowRunLeaves(nodes, run).map((n) => n.sid)).toEqual(["s2", "s10", "s11"]);
  });

  it("bounds membership by the run window (with grace past finished_at)", () => {
    const nodes = [
      // Before the run started: the trigger session's earlier hire.
      fixtureNode({ sid: "s5", parent_sid: "s1", last_active: "2026-09-01T09:00:00Z" }),
      // Inside the window.
      fixtureNode({ sid: "s6", parent_sid: "s1", last_active: "2026-09-01T10:05:00Z" }),
      // Within the 60s grace after finished_at (final bookkeeping turn).
      fixtureNode({ sid: "s7", parent_sid: "s1", last_active: "2026-09-01T10:10:30Z" }),
      // Way after: a later reuse of the trigger session.
      fixtureNode({ sid: "s8", parent_sid: "s1", last_active: "2026-09-01T12:00:00Z" }),
      // Subtree of an out-of-window child is skipped with it.
      fixtureNode({ sid: "s9", parent_sid: "s5", last_active: "2026-09-01T10:05:00Z" }),
    ];
    expect(flowRunLeaves(nodes, run).map((n) => n.sid)).toEqual(["s6", "s7"]);
  });

  it("a running run (no finished_at) keeps the window open-ended", () => {
    const nodes = [
      fixtureNode({ sid: "s6", parent_sid: "s1", last_active: "2026-09-01T23:00:00Z" }),
    ];
    expect(
      flowRunLeaves(nodes, { ...run, finished_at: null }).map((n) => n.sid),
    ).toEqual(["s6"]);
  });

  it("returns [] for a CLI-driven run with no trigger session", () => {
    expect(flowRunLeaves([fixtureNode()], { ...run, parent_sid: null })).toEqual([]);
  });

  it("unparseable timestamps degrade to show, never hide", () => {
    const nodes = [fixtureNode({ sid: "s6", parent_sid: "s1", last_active: "not-a-date" })];
    expect(flowRunLeaves(nodes, run).map((n) => n.sid)).toEqual(["s6"]);
  });
});

describe("runDurationLabel", () => {
  const start = "2026-09-01T10:00:00Z";
  const now = Date.parse("2026-09-01T10:07:30Z");

  it("compact tokens across magnitudes", () => {
    expect(runDurationLabel(start, "2026-09-01T10:00:42Z", now)).toBe("42s");
    expect(runDurationLabel(start, "2026-09-01T10:04:12Z", now)).toBe("4m12s");
    expect(runDurationLabel(start, "2026-09-01T10:04:00Z", now)).toBe("4m");
    expect(runDurationLabel(start, "2026-09-01T11:12:00Z", now)).toBe("1h12m");
    expect(runDurationLabel(start, "2026-09-03T13:00:00Z", now)).toBe("2d3h");
  });

  it("a running run measures against now; garbage renders an em-dash", () => {
    expect(runDurationLabel(start, null, now)).toBe("7m30s");
    expect(runDurationLabel("nope", null, now)).toBe("—");
  });
});
